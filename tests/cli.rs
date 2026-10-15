//! Integration tests for `rp`.
//!
//! Hermetic by construction: every external command rp shells out to is stubbed by
//! a shell script in a temp dir prepended to PATH, and every path rp touches comes
//! from an env override. No network, no real graphicle store, no real clones.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn stub(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

struct Env {
    tmp: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Env {
        let uniq = format!(
            "rp-it-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        );
        let tmp = std::env::temp_dir().join(&uniq);
        let _ = fs::remove_dir_all(&tmp);
        let bin = tmp.join("bin");
        let repo = tmp.join("repo");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join("README.md"), "# stub repo\n").unwrap();

        // git: answers the four shapes rp uses, never touches the network.
        stub(
            &bin,
            "git",
            r#"
dir=""
if [ "$1" = "-C" ]; then dir="$2"; shift 2; fi
last=""
for a in "$@"; do last="$a"; done
case "$1" in
  rev-parse) echo "$dir" ;;
  status)    [ -n "$FAKE_DIRTY" ] && echo " M local-change.txt" ;;
  clone)     mkdir -p "$last/.git" && echo "fake cloned $last" ;;
  sparse-checkout) echo "fake sparse set" ;;
  *) exit 0 ;;
esac
"#,
        );

        // graphicle: registration is a plain text file of registered paths.
        stub(
            &bin,
            "graphicle",
            r#"
reg="$GRAPHICLE_STUB_REGISTERED"
target=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--project" ]; then target="$a"; fi
  prev="$a"
done
case "$1" in
  --version) echo "graphicle 0.0.0-stub"; exit 0 ;;
  context)
    [ -f "$reg" ] && grep -qxF "$target" "$reg" && exit 0
    echo "No project is registered for $target" >&2
    exit 1 ;;
  sync)
    echo "$2" >> "$reg"
    [ -n "$FAKE_SYNC_FAIL" ] && { echo "sync exploded" >&2; exit 1; }
    echo "fake synced $2"
    exit 0 ;;
  *) exit 1 ;;
esac
"#,
        );

        stub(
            &bin,
            "gh",
            r#"
case "$1 $2" in
  "--version "*) echo "gh version stub"; exit 0 ;;
  "auth status") [ -n "$FAKE_NO_AUTH" ] && { echo "not logged in" >&2; exit 1; }
                echo "github.com"; exit 0 ;;
  *) exit 0 ;;
esac
"#,
        );

        // node: the context-mode CLI stand-in.
        stub(
            &bin,
            "node",
            "[ -n \"$FAKE_INDEX_FAIL\" ] && exit 1\nexit 0",
        );

        Env { tmp, bin, repo }
    }

    fn cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rp"))
            .args(args)
            .current_dir(&self.repo)
            // HOME is redirected so agent_targets() can never reach the real box.
            .env("HOME", &self.tmp)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/local/bin", self.bin.display()),
            )
            .env("RP_LEDGER", self.tmp.join("ledger.tsv"))
            .env("RP_CACHE", self.tmp.join("cache"))
            .env("RP_SKILLS", self.tmp.join("skills"))
            .env("RP_AGENT_MD", self.tmp.join("AGENTS.md"))
            .env("RP_SRC", self.tmp.join("src"))
            .env("RP_BIN", self.tmp.join("bin-rp"))
            .env("GRAPHICLE_DATA_DIR", self.tmp.join("gstore"))
            .env("GRAPHICLE_STUB_REGISTERED", self.tmp.join("registered.txt"))
            .env("CTX_MODE_CLI", self.tmp.join("ctx.bundle.mjs"))
            .env("RP_CACHE_GB", "150")
            .env("RP_FREE_FLOOR_GB", "0")
            .env("RP_BUDGET_MB", "4096")
            .output()
            .expect("rp failed to spawn")
    }

    fn out(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.cmd(args).stdout).into_owned()
    }

    fn register(&self, path: &str) {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.tmp.join("registered.txt"))
            .unwrap();
        writeln!(f, "{path}").unwrap();
    }

    fn touch_repo(&self) -> String {
        self.repo.canonicalize().unwrap().display().to_string()
    }

    fn seed_skill_src(&self) {
        let d = self.tmp.join("src/skill");
        fs::create_dir_all(&d).unwrap();
        fs::copy(manifest_dir().join("skill/SKILL.md"), d.join("SKILL.md")).unwrap();
        fs::copy(manifest_dir().join("skill/COSTS.md"), d.join("COSTS.md")).unwrap();
    }

    fn seed_agent_files(&self) {
        for f in [
            ".pi/agent/AGENTS.md",
            ".claude/CLAUDE.md",
            ".codex/AGENTS.md",
            ".gemini/GEMINI.md",
            ".copilot/instructions.md",
            ".config/opencode/AGENTS.md",
            ".hermes/AGENTS.md",
        ] {
            let p = self.tmp.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("keep {f}\n")).unwrap();
        }
        // A Cursor rule we did not write: uninstall must leave it alone.
        let c = self.tmp.join(".cursor/rules/repo-tiers.mdc");
        fs::create_dir_all(c.parent().unwrap()).unwrap();
        fs::write(&c, "foreign cursor rule\n").unwrap();
    }

    fn agent_files(&self) -> Vec<PathBuf> {
        [
            ".pi/agent/AGENTS.md",
            ".claude/CLAUDE.md",
            ".codex/AGENTS.md",
            ".gemini/GEMINI.md",
            ".copilot/instructions.md",
            ".config/opencode/AGENTS.md",
            ".hermes/AGENTS.md",
        ]
        .iter()
        .map(|f| self.tmp.join(f))
        .collect()
    }

    fn agent_skill_links(&self) -> Vec<PathBuf> {
        [
            ".pi/agent/skills/repo-tiers",
            ".claude/skills/repo-tiers",
            ".codex/skills/repo-tiers",
            ".gemini/skills/repo-tiers",
            ".copilot/skills/repo-tiers",
            ".cursor/skills/repo-tiers",
            ".config/opencode/skills/repo-tiers",
            ".hermes/skills/repo-tiers",
        ]
        .iter()
        .map(|f| self.tmp.join(f))
        .collect()
    }

    fn ledger_lines(&self) -> usize {
        fs::read_to_string(self.tmp.join("ledger.tsv"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
}

fn stdout_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn first_visit_says_do_not_index() {
    let e = Env::new("cold1");
    let o = e.cmd(&["state"]);
    assert!(o.status.success());
    assert!(stdout_of(&o).contains("DO NOT INDEX"), "{}", stdout_of(&o));
    assert!(stdout_of(&o).contains("visits=1"));
}

#[test]
fn second_visit_escalates_to_repeat_visitor() {
    let e = Env::new("cold2");
    e.cmd(&["state"]);
    let o = e.cmd(&["state"]);
    let s = stdout_of(&o);
    assert!(s.contains("repeat visitor, indexing now pays"), "{s}");
    assert!(s.contains("visits=2"), "{s}");
    assert!(!s.contains("DO NOT INDEX"));
}

#[test]
fn docs_index_promotes_to_t1() {
    let e = Env::new("t1");
    let o = e.cmd(&["docs", "."]);
    assert!(o.status.success(), "{}", stdout_of(&o));
    let s = e.out(&["state"]);
    assert!(s.contains("T1 text index ready"), "{s}");
}

#[test]
fn registered_graph_wins_over_text_mark() {
    let e = Env::new("t2");
    let root = e.touch_repo();
    e.cmd(&["docs", "."]);
    e.register(&root);
    let s = e.out(&["state"]);
    assert!(s.contains("T2 graph ready"), "{s}");
}

#[test]
fn json_state_parses_and_carries_tier() {
    let e = Env::new("jsonstate");
    let s = e.out(&["--json", "state"]);
    let v: serde_like::Json = serde_like::parse(&s);
    assert_eq!(v.get("tier"), "T0");
    assert_eq!(v.get("visits"), "1");
    assert!(v.get("tier_label").contains("DO NOT INDEX"));
    assert!(v.get("next").contains("grep"));
}

#[test]
fn json_doctor_parses_and_counts() {
    let e = Env::new("jsondoctor");
    let s = e.out(&["--json", "doctor"]);
    let v = serde_like::parse(&s);
    assert!(v.get("checks").contains("\"name\""));
    assert_ne!(v.get("ok"), "0");
}

#[test]
fn get_stays_remote_and_never_creates_a_clone() {
    let e = Env::new("remote");
    let o = e.cmd(&["get", "pallets/click"]);
    assert!(o.status.success());
    let s = stdout_of(&o);
    assert!(s.contains("R0 remote"), "{s}");
    assert!(s.contains("gh search code"));
    assert!(
        !e.tmp.join("cache/pallets").exists(),
        "get must not create a clone"
    );
    assert!(
        !e.tmp.join("cache/click").exists(),
        "get must not create a clone"
    );
}

#[test]
fn get_routes_to_state_once_cloned() {
    let e = Env::new("getcloned");
    e.cmd(&["clone", "pallets/click"]);
    let s = e.out(&["get", "pallets/click"]);
    assert!(
        !s.contains("R0 remote"),
        "cloned repo should not report R0: {s}"
    );
    assert!(s.contains("tier:"), "{s}");
}

#[test]
fn clone_lands_in_cache_and_records_the_visit() {
    let e = Env::new("clone");
    let o = e.cmd(&["clone", "pallets/click"]);
    assert!(o.status.success(), "{}", stdout_of(&o));
    assert!(e.tmp.join("cache/click/.git").exists());
    assert!(e.ledger_lines() >= 1);
}

#[test]
fn bad_slug_is_rejected_before_any_write() {
    let e = Env::new("badslug");
    for bad in ["nonsense", "a/b/c", "", "../evil", "owner//repo"] {
        let o = e.cmd(&["clone", bad]);
        assert_eq!(o.status.code(), Some(2), "expected exit 2 for {bad:?}");
    }
    assert!(
        !e.tmp.join("cache").exists() || fs::read_dir(e.tmp.join("cache")).unwrap().count() == 0
    );
}

#[test]
fn traversal_subpath_is_rejected() {
    let e = Env::new("traversal");
    for bad in ["/etc/passwd", "../../secret", "a/../../b"] {
        let o = e.cmd(&["clone", "pallets/click", bad]);
        assert_eq!(o.status.code(), Some(2), "expected exit 2 for {bad:?}");
    }
}

#[test]
fn missing_path_is_a_usage_error() {
    let e = Env::new("missing");
    let o = e.cmd(&["state", "/definitely/not/here"]);
    assert_eq!(o.status.code(), Some(2));
    let o = e.cmd(&["warm", "/definitely/not/here"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn eviction_keeps_a_dirty_clone_and_removes_a_clean_one() {
    let e = Env::new("evict");
    let cache = e.tmp.join("cache");
    fs::create_dir_all(cache.join("clean/.git")).unwrap();
    fs::create_dir_all(cache.join("dirty/.git")).unwrap();
    fs::write(cache.join("clean/blob"), vec![0u8; 2 * 1024 * 1024]).unwrap();
    fs::write(cache.join("dirty/blob"), vec![0u8; 2 * 1024 * 1024]).unwrap();

    // Budget 0 GB forces the cache over its limit.
    let o = Command::new(env!("CARGO_BIN_EXE_rp"))
        .args(["evict"])
        .current_dir(&e.repo)
        .env("PATH", format!("{}:/usr/bin:/bin", e.bin.display()))
        .env("RP_LEDGER", e.tmp.join("ledger.tsv"))
        .env("RP_CACHE", &cache)
        .env("RP_CACHE_GB", "0")
        .env("RP_FREE_FLOOR_GB", "0")
        .env("FAKE_DIRTY", "1") // the stub reports every clone as dirty
        .output()
        .unwrap();
    let s = stdout_of(&o);
    assert!(s.contains("keep (uncommitted work)"), "{s}");
    assert!(cache.join("dirty").exists(), "dirty clone must survive");

    // Now with a clean tree the clean clone is removable.
    Command::new(env!("CARGO_BIN_EXE_rp"))
        .args(["evict"])
        .current_dir(&e.repo)
        .env("PATH", format!("{}:/usr/bin:/bin", e.bin.display()))
        .env("RP_LEDGER", e.tmp.join("ledger.tsv"))
        .env("RP_CACHE", &cache)
        .env("RP_CACHE_GB", "0")
        .env("RP_FREE_FLOOR_GB", "0")
        .output()
        .unwrap();
    assert!(
        !cache.join("clean").exists(),
        "clean clone should be evicted"
    );
}

#[test]
fn install_is_idempotent() {
    let e = Env::new("install");
    fs::create_dir_all(e.tmp.join("src/skill")).unwrap();
    fs::copy(
        manifest_dir().join("skill/SKILL.md"),
        e.tmp.join("src/skill/SKILL.md"),
    )
    .unwrap();
    fs::copy(
        manifest_dir().join("skill/COSTS.md"),
        e.tmp.join("src/skill/COSTS.md"),
    )
    .unwrap();

    let first = e.out(&["install"]);
    assert!(first.contains("installed"), "{first}");
    let second = e.out(&["install"]);
    for line in second.lines() {
        assert!(line.contains("unchanged"), "not idempotent: {second}");
    }
    let md = fs::read_to_string(e.tmp.join("AGENTS.md")).unwrap();
    assert!(md.contains("repo-tiers:start") && md.contains("repo-tiers:end"));
}

#[test]
fn uninstall_removes_only_its_own_block_and_skill() {
    let e = Env::new("uninstall");
    fs::create_dir_all(e.tmp.join("src/skill")).unwrap();
    fs::copy(
        manifest_dir().join("skill/SKILL.md"),
        e.tmp.join("src/skill/SKILL.md"),
    )
    .unwrap();
    fs::write(
        e.tmp.join("AGENTS.md"),
        "# mine\n\nkeep this\n\n## Shell\n\ntable stuff\n",
    )
    .unwrap();
    e.cmd(&["install"]);
    let mid = fs::read_to_string(e.tmp.join("AGENTS.md")).unwrap();
    assert!(mid.contains("keep this") && mid.contains("repo-tiers:start"));

    e.cmd(&["uninstall"]);
    let after = fs::read_to_string(e.tmp.join("AGENTS.md")).unwrap();
    assert!(
        !after.contains("repo-tiers"),
        "block should be gone:\n{after}"
    );
    assert!(
        after.contains("keep this"),
        "user prose must survive:\n{after}"
    );
    assert!(after.contains("## Shell"));
    assert!(
        !e.tmp.join("skills").exists(),
        "our skill dir should be removed"
    );
}

#[test]
fn uninstall_refuses_a_foreign_skill_dir() {
    let e = Env::new("foreign");
    fs::create_dir_all(e.tmp.join("skills")).unwrap();
    fs::write(
        e.tmp.join("skills/SKILL.md"),
        "---\nname: something-else\n---\nnot ours\n",
    )
    .unwrap();
    let s = e.out(&["uninstall"]);
    assert!(s.contains("skipped"), "{s}");
    assert!(e.tmp.join("skills").exists(), "foreign skill must survive");
}

#[test]
fn version_and_unknown_command() {
    let e = Env::new("ver");
    let v = e.out(&["--version"]);
    assert!(v.starts_with("rp "), "{v}");
    assert!(v.trim_end()[3..].chars().next().unwrap().is_ascii_digit());
    let o = e.cmd(&["frobnicate"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn quiet_suppresses_progress_but_not_failure() {
    let e = Env::new("quiet");
    let o = e.cmd(&["--quiet", "docs", "."]);
    assert!(o.status.success());
    assert!(stdout_of(&o).is_empty(), "quiet should print nothing");

    // With the index stub failing, --quiet must still fail loudly.
    let o = Command::new(env!("CARGO_BIN_EXE_rp"))
        .args(["--quiet", "docs", "."])
        .current_dir(&e.repo)
        .env("PATH", format!("{}:/usr/bin:/bin", e.bin.display()))
        .env("RP_LEDGER", e.tmp.join("ledger.tsv"))
        .env("RP_CACHE", e.tmp.join("cache"))
        .env("RP_SKILLS", e.tmp.join("skills"))
        .env("RP_AGENT_MD", e.tmp.join("AGENTS.md"))
        .env("RP_FREE_FLOOR_GB", "0")
        .env("FAKE_INDEX_FAIL", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("index failed"));
}

#[test]
fn ledger_grows_one_line_per_visit() {
    let e = Env::new("ledger");
    let before = e.ledger_lines();
    e.cmd(&["state"]);
    e.cmd(&["state"]);
    assert_eq!(e.ledger_lines(), before + 2);
}

#[test]
fn top_json_lists_visits() {
    let e = Env::new("topjson");
    e.cmd(&["state"]);
    let s = e.out(&["--json", "top", "5"]);
    let v = serde_like::parse(&s);
    assert!(v.get("repos").contains("visits"));
}

#[test]
fn install_agents_all_reaches_every_memory_file() {
    let e = Env::new("agentsall");
    e.seed_skill_src();
    e.seed_agent_files();
    let o = e.cmd(&["install", "--agents", "all"]);
    let s = stdout_of(&o);
    assert!(o.status.success(), "{s}");
    for p in e.agent_files() {
        let t = fs::read_to_string(&p).unwrap();
        assert!(
            t.contains("repo-tiers:start"),
            "{} lost the pointer",
            p.display()
        );
        assert!(t.contains("keep "), "{} lost user prose", p.display());
    }
    // The foreign Cursor rule must be left exactly as it was.
    let c = fs::read_to_string(e.tmp.join(".cursor/rules/repo-tiers.mdc")).unwrap();
    assert_eq!(c, "foreign cursor rule\n");
    assert!(s.contains("skipped"), "{s}");
    // Every agent gets the shared skill symlinked into its own skills dir, so the
    // agent's skill loader discovers repo-tiers, not just its memory file.
    for l in e.agent_skill_links() {
        let target =
            fs::read_link(&l).unwrap_or_else(|e| panic!("{} is not a symlink: {e}", l.display()));
        assert_eq!(target, e.tmp.join("skills"), "{} wrong target", l.display());
    }
}

#[test]
fn a_foreign_skill_dir_is_never_clobbered() {
    let e = Env::new("foreignskill");
    e.seed_skill_src();
    let mine = e.tmp.join(".claude/skills/repo-tiers");
    fs::create_dir_all(&mine).unwrap();
    fs::write(mine.join("SKILL.md"), "someone else's skill\n").unwrap();
    let o = e.cmd(&["install", "--agents", "claude"]);
    let s = stdout_of(&o);
    assert!(o.status.success(), "{s}");
    assert!(s.contains("foreign skill dir"), "{s}");
    assert_eq!(
        fs::read_to_string(mine.join("SKILL.md")).unwrap(),
        "someone else's skill\n"
    );
}

#[test]
fn a_link_pointing_elsewhere_is_left_alone() {
    let e = Env::new("elsewhere");
    e.seed_skill_src();
    let other = e.tmp.join("other-skill");
    fs::create_dir_all(&other).unwrap();
    let link = e.tmp.join(".codex/skills/repo-tiers");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&other, &link).unwrap();
    let o = e.cmd(&["install", "--agents", "codex"]);
    let s = stdout_of(&o);
    assert!(o.status.success(), "{s}");
    assert!(s.contains("link points elsewhere"), "{s}");
    assert_eq!(fs::read_link(&link).unwrap(), other);
}

#[test]
fn install_creates_a_cursor_rule_when_none_exists() {
    let e = Env::new("cursorrule");
    e.seed_skill_src();
    let o = e.cmd(&["install", "--agents", "cursor"]);
    assert!(o.status.success(), "{}", stdout_of(&o));
    let c = fs::read_to_string(e.tmp.join(".cursor/rules/repo-tiers.mdc")).unwrap();
    assert!(
        c.contains("alwaysApply: true") && c.contains("Repo tiers"),
        "{c}"
    );
}

#[test]
fn uninstall_sweeps_every_agent_it_wrote() {
    let e = Env::new("sweep");
    e.seed_skill_src();
    e.seed_agent_files();
    e.cmd(&["install", "--agents", "all"]);
    let o = e.cmd(&["uninstall"]);
    assert!(o.status.success(), "{}", stdout_of(&o));
    for p in e.agent_files() {
        let t = fs::read_to_string(&p).unwrap();
        assert!(
            !t.contains("repo-tiers"),
            "{} still carries a pointer:\n{t}",
            p.display()
        );
        assert!(t.contains("keep "), "{} lost user prose", p.display());
    }
    let c = fs::read_to_string(e.tmp.join(".cursor/rules/repo-tiers.mdc")).unwrap();
    assert_eq!(c, "foreign cursor rule\n", "foreign rule must survive");
    for l in e.agent_skill_links() {
        assert!(
            l.symlink_metadata().is_err(),
            "{} should be gone after uninstall",
            l.display()
        );
    }
}

#[test]
fn unknown_agent_is_rejected() {
    let e = Env::new("badagent");
    let o = e.cmd(&["install", "--agents", "bogus"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown agent"));
}

#[test]
fn agents_json_lists_every_target() {
    let e = Env::new("agentsjson");
    let s = e.out(&["--json", "agents"]);
    let v = serde_like::parse(&s);
    for a in [
        "pi", "claude", "codex", "gemini", "copilot", "cursor", "opencode", "hermes",
    ] {
        assert!(v.get("agents").contains(a), "missing {a} in {s}");
    }
}

#[test]
fn doctor_reports_which_tiers_are_live() {
    let e = Env::new("tiers");
    let s = e.out(&["--json", "doctor"]);
    let v = serde_like::parse(&s);
    assert!(v.get("tiers").contains("T0"), "{s}");
    assert!(v.get("tiers").contains("R1"), "{s}");
    assert!(v.get("tiers_missing").contains("needs"), "{s}");
}

#[test]
fn cache_json_reports_budget() {
    let e = Env::new("cachejson");
    let s = e.out(&["--json", "cache"]);
    let v = serde_like::parse(&s);
    assert_eq!(v.get("budget_mb"), "153600");
}

/// A tiny JSON reader, just enough to assert on flat keys, nested objects and
/// arrays. No dev-dependency: rp stays a zero-crate project end to end.
mod serde_like {
    pub struct Json(String);
    impl Json {
        /// Value for `key`: unquoted string contents, or the full literal span for
        /// numbers, booleans, objects and arrays (depth-matched, quote-aware).
        pub fn get(&self, key: &str) -> String {
            let pat = format!("\"{key}\":");
            let Some(i) = self.0.find(&pat) else {
                return String::new();
            };
            let r = self.0[i + pat.len()..].trim_start();
            let first = r.chars().next().unwrap_or(' ');
            if first == '"' {
                let mut esc = false;
                for (idx, ch) in r.char_indices().skip(1) {
                    if esc {
                        esc = false;
                        continue;
                    }
                    match ch {
                        '\\' => esc = true,
                        '"' => return r[1..idx].to_string(),
                        _ => {}
                    }
                }
                return String::new();
            }
            if first == '[' || first == '{' {
                let close = if first == '[' { ']' } else { '}' };
                let mut depth = 0usize;
                let mut in_str = false;
                let mut esc = false;
                for (idx, ch) in r.char_indices() {
                    if esc {
                        esc = false;
                        continue;
                    }
                    if in_str && ch == '\\' {
                        esc = true;
                        continue;
                    }
                    if ch == '"' {
                        in_str = !in_str;
                        continue;
                    }
                    if in_str {
                        continue;
                    }
                    if ch == first {
                        depth += 1;
                    } else if ch == close {
                        depth -= 1;
                        if depth == 0 {
                            return r[..=idx].to_string();
                        }
                    }
                }
                return r.to_string();
            }
            let end = r.find([',', '}']).unwrap_or(r.len());
            r[..end].trim().to_string()
        }
    }
    pub fn parse(s: &str) -> Json {
        assert!(
            s.trim_start().starts_with('{') && s.trim_end().ends_with('}'),
            "not a JSON object: {s}"
        );
        Json(s.to_string())
    }
}

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}
