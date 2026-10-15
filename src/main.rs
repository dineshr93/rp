//! rp — repo tier router.
//!
//! R0 remote (0 MB)  ->  R1 shallow/sparse clone  ->  T0 local grep
//!                   ->  T1 context-mode text      ->  T2 graphicle code graph
//!
//! Local disk is a cache with a byte budget, not a library. Measured on the box
//! this was written on: a whole-tree clone of a 7.8k-file repo costs 184 MB and
//! 20s; the same repo sparse-cloned to one subpath costs 49 MB. 1000 repos/day
//! at 180 MB each is 180 GB/day, so the default answer is: do not clone.
//!
//! No crates on purpose. Everything here is a subprocess plus arithmetic.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{exit, Command, Stdio};

const USAGE: &str = "usage: rp [state|warm|docs|top [N]|gc|get|clone|cache|evict|install|doctor] [path]\n\
  state  (default) report the tier of a local repo and record the visit\n\
  get    owner/repo: answer remotely, never clone\n\
  clone  owner/repo [subpath]: budget-checked shallow / sparse clone\n\
  warm   graphicle code graph (callers, impact, why)\n\
  docs   context-mode text index\n\
  top    most-visited repos = warm candidates\n\
  cache  clone cache: size, last touch, dirty flag\n\
  evict  LRU eviction; never removes a clone with uncommitted work\n\
  gc     graphicle store report and the reclaim path\n\
  install  bootstrap this box: skill files + AGENTS.md pointer + binary (idempotent)\n\
  doctor   check toolchain, budgets and install; exits 1 on a hard miss";

struct Cfg {
    ledger: PathBuf,
    markdir: PathBuf,
    cache: PathBuf,
    gstore: PathBuf,
    ctxbin: String,
    src: PathBuf,
    skills: PathBuf,
    agent_md: PathBuf,
    budget_mb: u64,
    cache_mb: u64,
    floor_gb: u64,
}

fn svar(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn uvar(key: &str, default: u64) -> u64 {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Cfg {
    fn new() -> Cfg {
        let home = svar("HOME", ".");
        let c = Cfg {
            ledger: PathBuf::from(svar("RP_LEDGER", &format!("{home}/.pi/rp/ledger.tsv"))),
            markdir: PathBuf::from(format!("{home}/.pi/rp/indexed")),
            cache: PathBuf::from(svar("RP_CACHE", &format!("{home}/cache/repos"))),
            gstore: PathBuf::from(svar(
                "GRAPHICLE_DATA_DIR",
                &format!("{home}/.local/share/graphicle"),
            )),
            ctxbin: svar(
                "CTX_MODE_CLI",
                &format!("{home}/.pi/agent/npm/node_modules/context-mode/cli.bundle.mjs"),
            ),
            src: PathBuf::from(svar("RP_SRC", &format!("{home}/tools/rp"))),
            skills: PathBuf::from(svar(
                "RP_SKILLS",
                &format!("{home}/.agents/skills/repo-tiers"),
            )),
            agent_md: PathBuf::from(svar("RP_AGENT_MD", &format!("{home}/.pi/agent/AGENTS.md"))),
            budget_mb: uvar("RP_BUDGET_MB", 4096),
            cache_mb: uvar("RP_CACHE_GB", 150) * 1024,
            floor_gb: uvar("RP_FREE_FLOOR_GB", 100),
        };
        if let Some(p) = c.ledger.parent() {
            let _ = fs::create_dir_all(p);
        }
        let _ = fs::create_dir_all(&c.markdir);
        let _ = fs::create_dir_all(&c.cache);
        c
    }
}

/// Run a program, return (exit code, stdout+stderr). Never panics on spawn failure.
fn run(prog: &str, args: &[&str]) -> (i32, String) {
    match Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code().unwrap_or(1), s)
        }
        Err(e) => (127, format!("rp: {prog}: {e}")),
    }
}

fn tail(s: &str, k: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(k);
    lines[start..].join("\n")
}

fn du_mb(p: &Path) -> u64 {
    let (_, o) = run("du", &["-sm", &p.to_string_lossy()]);
    o.split_whitespace()
        .next()
        .and_then(|x| x.parse().ok())
        .unwrap_or(0)
}

fn free_gb() -> u64 {
    let (rc, o) = run("df", &["-BG", "--output=avail", "/"]);
    if rc != 0 {
        return u64::MAX; // unknown: never evict on a guess
    }
    o.lines()
        .rev()
        .find_map(|l| l.trim().trim_end_matches('G').trim().parse::<u64>().ok())
        .unwrap_or(u64::MAX)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// epoch -> YYYY-MM-DD, no chrono.
fn fmt_date(ts: i64) -> String {
    if ts <= 0 {
        return "never".into();
    }
    let z = ts.div_euclid(86400) + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    format!("{}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

type Ledger = Vec<(i64, String)>;

fn read_ledger(c: &Cfg) -> Ledger {
    let mut v = Ledger::new();
    if let Ok(f) = fs::File::open(&c.ledger) {
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            if let Some((ts, path)) = line.split_once('\t') {
                if let Ok(t) = ts.parse::<i64>() {
                    v.push((t, path.to_string()));
                }
            }
        }
    }
    v
}

fn record(c: &Cfg, path: &str) {
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&c.ledger)
    {
        let _ = writeln!(f, "{}\t{path}", now());
    }
}

fn visits(l: &Ledger, p: &str) -> usize {
    l.iter().filter(|(_, q)| q == p).count()
}

fn last_touch(l: &Ledger, p: &str) -> i64 {
    l.iter().filter(|(_, q)| q == p).map(|(t, _)| *t).max().unwrap_or(0)
}

/// Is this path a registered graphicle project?
///
/// `graphicle context --project <path>` exits 0 only when registered. Bare
/// `graphicle status` must never be used for this: with no path it silently
/// returns the LAST-SYNCED project, not the cwd's (verified: run in
/// ~/repos/AMD it returned wezterm).
fn registered(path: &str) -> bool {
    run("graphicle", &["context", "probe", "--project", path]).0 == 0
}

fn repo_root(p: &str) -> String {
    let (rc, o) = run("git", &["-C", p, "rev-parse", "--show-toplevel"]);
    if rc == 0 && !o.trim().is_empty() {
        o.trim().to_string()
    } else {
        p.to_string()
    }
}

fn base(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

fn norm_slug(raw: &str) -> String {
    let mut r = raw.trim();
    for pre in ["https://github.com/", "git@github.com:"] {
        if let Some(x) = r.strip_prefix(pre) {
            r = x;
        }
    }
    if let Some(x) = r.strip_suffix(".git") {
        r = x;
    }
    r.trim_matches('/').to_string()
}

/// Stable short id for a repo root: FNV-1a, no hashing crate.
fn mark_path(c: &Cfg, root: &str) -> PathBuf {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in root.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    c.markdir.join(format!("{h:016x}"))
}

fn count_files(root: &str) -> usize {
    let (_, o) = run(
        "find",
        &[root, "-type", "f", "-not", "-path", "*/.git/*"],
    );
    o.lines().filter(|l| !l.is_empty()).count()
}

fn abs(p: &str) -> String {
    fs::canonicalize(p)
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string())
}

// ---------------------------------------------------------------- commands ---

fn cmd_state(c: &Cfg, path: &str) {
    let root = repo_root(&abs(path));
    let name = base(&root);
    record(c, &root);
    let l = read_ledger(c);
    let v = visits(&l, &root);

    if registered(&root) {
        println!("repo: {name}  visits={v}  tier: T2 graph ready");
        println!("  use: recall_project / query_graph (MCP) — callers, impact, why");
    } else if mark_path(c, &root).exists() {
        println!("repo: {name}  visits={v}  tier: T1 text index ready");
        println!("  use: ctx_search(queries:[...]) — prose/log/code text, no graph");
    } else if v >= 2 {
        println!("repo: {name}  visits={v}  tier: T0 cold — repeat visitor, indexing now pays");
        println!("  now: grep -rn 'PATTERN' .            (0.02s, free)");
        println!("  then: rp warm .                     (graphicle graph, background)");
        println!("  docs only: rp docs .                (context-mode text index)");
    } else {
        println!("repo: {name}  visits={v}  tier: T0 cold — DO NOT INDEX");
        println!("  indexing costs ~1-10s and 9-380 MB; a one-touch repo never earns it back");
        println!("  use: grep -rn 'PATTERN' .   |   rtk read FILE --max-lines 40");
    }

    let mb = du_mb(&c.gstore);
    let over = if mb > c.budget_mb {
        " — OVER: run rp gc"
    } else {
        ""
    };
    println!("  store: {mb} MB / {} MB budget{over}", c.budget_mb);
}

fn cmd_warm(c: &Cfg, path: &str) {
    let root = repo_root(&abs(path));
    record(c, &root);
    println!("rp: graphicle sync {root} (background)");
    let (rc, o) = run("graphicle", &["sync", &root, "--background"]);
    println!("{}", tail(&o, 3));
    if rc != 0 {
        eprintln!("rp: sync failed");
        exit(1);
    }
    println!("rp: store now {} MB", du_mb(&c.gstore));
}

fn cmd_docs(c: &Cfg, path: &str) {
    let root = repo_root(&abs(path));
    record(c, &root);
    let (_, o) = run(
        "find",
        &[
            &root,
            "-type",
            "f",
            "(",
            "-name",
            "*.md",
            "-o",
            "-name",
            "*.mdx",
            "-o",
            "-name",
            "*.txt",
            "-o",
            "-name",
            "*.py",
            "-o",
            "-name",
            "*.ts",
            "-o",
            "-name",
            "*.tsx",
            "-o",
            "-name",
            "*.js",
            "-o",
            "-name",
            "*.go",
            "-o",
            "-name",
            "*.rs",
            ")",
            "-not",
            "-path",
            "*/node_modules/*",
            "-not",
            "-path",
            "*/.git/*",
            "-not",
            "-path",
            "*/dist/*",
            "-not",
            "-path",
            "*/build/*",
        ],
    );
    let found = o.lines().filter(|l| !l.is_empty()).count();
    // context-mode defaults to --max-files 200 and silently under-indexes any real repo.
    let cap = found.min(3000).max(1).to_string();
    let src = format!("repo:{}", base(&root));
    println!(
        "rp: context-mode index {root} ({cap} files, ~{} MB)",
        cap.parse::<u64>().unwrap_or(0) * 205 / 1024
    );
    let (rc, o2) = run(
        "node",
        &[
            &c.ctxbin,
            "index",
            &root,
            "--project",
            &root,
            "--source",
            &src,
            "--max-files",
            &cap,
            "--max-depth",
            "8",
        ],
    );
    println!("{}", tail(&o2, 3));
    if rc != 0 {
        eprintln!("rp: index failed");
        exit(1);
    }
    let _ = fs::File::create(mark_path(c, &root));
}

fn label_for(c: &Cfg, l: &Ledger, p: &str) -> String {
    if p.starts_with("remote:") {
        return "remote".into();
    }
    if !Path::new(p).exists() {
        return "gone".into();
    }
    let mut label = "cold".to_string();
    if registered(p) {
        label = "graph".into();
    }
    if mark_path(c, p).exists() {
        label = format!("{label}+text");
    }
    let _ = l;
    label
}

fn cmd_top(c: &Cfg, k: usize) {
    println!("most-visited repos (warm candidates):");
    let l = read_ledger(c);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, p) in &l {
        *counts.entry(p.clone()).or_insert(0) += 1;
    }
    let mut rows: Vec<(usize, String)> = counts.into_iter().map(|(p, cnt)| (cnt, p)).collect();
    rows.sort_by_key(|(cnt, p)| (Reverse(*cnt), p.clone()));
    for (cnt, p) in rows.into_iter().take(k) {
        println!("  {cnt:4}  {p:<42} {}", label_for(c, &l, &p));
    }
}

/// Authoritative list of folders registered in the graphicle store.
/// Read-only sqlite via python3 stdlib — rp stays dependency-free.
fn store_folders(c: &Cfg) -> Vec<(String, String)> {
    let db = c.gstore.join("reviews.sqlite3");
    if !db.exists() {
        return Vec::new();
    }
    let script = "import sqlite3,sys\ntry:\n c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True)\n for p,u in c.execute('select f.path,f.updated_at from repo_folders f order by f.updated_at'):\n  print(str(u)[:16]+'\\t'+str(p))\nexcept Exception:\n pass";
    let (rc, o) = run("python3", &["-c", script, &db.to_string_lossy()]);
    if rc != 0 {
        return Vec::new();
    }
    o.lines()
        .filter_map(|l| l.split_once('\t').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect()
}

fn cmd_gc(c: &Cfg) {
    let mb = du_mb(&c.gstore);
    println!("graphicle store: {mb} MB (budget {} MB)", c.budget_mb);
    let folders = store_folders(c);
    if folders.is_empty() {
        println!("  (store unread or empty; falling back to the visit ledger)");
        let l = read_ledger(c);
        let mut last: BTreeMap<String, i64> = BTreeMap::new();
        for (t, p) in &l {
            if p.starts_with("remote:") {
                continue;
            }
            let e = last.entry(p.clone()).or_insert(0);
            if *t > *e {
                *e = *t;
            }
        }
        let mut rows: Vec<(i64, String)> = last.into_iter().map(|(p, t)| (t, p)).collect();
        rows.sort_by_key(|(t, _)| *t);
        for (t, p) in rows.iter().filter(|(_, p)| registered(p)) {
            println!("  {}  {p}", fmt_date(*t));
        }
    } else {
        println!("registered folders (store order, oldest first): {}/{} shown", folders.len().min(40), folders.len());
        for (u, p) in folders.iter().take(40) {
            println!("  {u}  {p}");
        }
    }
    println!("clone cache: {} MB", du_mb(&c.cache));
    println!("no per-project delete exists in the graphicle CLI (verified: delete_* / forget_* all");
    println!("return \"Unsupported native project operation\") — so the store only grows.");
    println!("To reclaim: graphicle backup && mv ~/.local/share/graphicle ~/.local/share/graphicle.old,");
    println!("then rp warm only what you need. Prevention beats cleanup: rp warms on repeat visits.");
}

fn cmd_get(c: &Cfg, raw: &str) {
    let slug = norm_slug(raw);
    let dest = c.cache.join(base(&slug));
    if dest.join(".git").exists() {
        return cmd_state(c, &dest.to_string_lossy());
    }
    record(c, &format!("remote:{slug}"));
    println!("repo: {slug}  tier: R0 remote — NOT cloned (a clone costs 50-460 MB and 10-40s)");
    println!("  search: gh search code 'PATTERN' --repo {slug} --limit 20        (~0.8s, 0 MB)");
    println!(
        "  tree:   gh api repos/{slug}/git/trees/HEAD?recursive=1 --jq '.tree[].path' | head -60"
    );
    println!("  file:   gh api repos/{slug}/contents/PATH --jq .content | base64 -d | head -80");
    println!("  search cap is 30/min — batch queries, never loop per file");
    println!("  clone only when you will grep more than ~5 times: rp clone {slug} [subpath]");
}

fn cmd_clone(c: &Cfg, raw: &str, sub: Option<&str>) {
    let slug = norm_slug(raw);
    let dest = c.cache.join(base(&slug));
    let ds = dest.to_string_lossy().into_owned();
    if dest.join(".git").exists() {
        println!("rp: already cloned at {ds}");
        return cmd_state(c, &ds);
    }
    let free = free_gb();
    if free < c.floor_gb {
        println!(
            "rp: only {free} GB free (< {} GB floor) — evicting LRU clones first",
            c.floor_gb
        );
        evict(c);
    }
    let url = format!("https://github.com/{slug}");
    match sub {
        Some(sp) => {
            println!("rp: sparse clone {slug} -> only '{sp}' (cheapest by far on monorepos)");
            let (rc, o) = run("git", &["clone", "--depth", "1", "--sparse", &url, &ds]);
            if rc != 0 {
                eprintln!("rp: clone failed\n{}", tail(&o, 3));
                exit(1);
            }
            let (rc2, o2) = run("git", &["-C", &ds, "sparse-checkout", "set", sp]);
            if rc2 != 0 {
                eprintln!("rp: sparse-checkout set failed\n{}", tail(&o2, 3));
                exit(1);
            }
        }
        None => {
            println!("rp: shallow clone {slug} (whole tree)");
            let (rc, o) = run("git", &["clone", "--depth", "1", &url, &ds]);
            if rc != 0 {
                eprintln!("rp: clone failed\n{}", tail(&o, 3));
                exit(1);
            }
        }
    }
    record(c, &ds);
    println!(
        "rp: {ds} = {} MB, {} files",
        du_mb(Path::new(&ds)),
        count_files(&ds)
    );
    println!("rp: free now {} GB; expand: git -C {ds} sparse-checkout add <dir>", free_gb());
}

fn clones(c: &Cfg) -> Vec<(i64, u64, usize, String)> {
    let l = read_ledger(c);
    let mut rows = Vec::new();
    if let Ok(rd) = fs::read_dir(&c.cache) {
        for e in rd.flatten() {
            let d = e.path();
            if !d.join(".git").exists() {
                continue;
            }
            let p = d.to_string_lossy().into_owned();
            let (_, o) = run("git", &["-C", &p, "status", "--porcelain"]);
            let dirty = o.lines().filter(|x| !x.is_empty()).count();
            rows.push((last_touch(&l, &p), du_mb(&d), dirty, p));
        }
    }
    rows
}

fn cmd_cache(c: &Cfg) {
    println!(
        "clone cache: {} ({} MB / {} GB budget)",
        c.cache.display(),
        du_mb(&c.cache),
        c.cache_mb / 1024
    );
    let mut rows = clones(c);
    rows.sort_by_key(|(t, _, _, _)| *t);
    for (t, mb, dirty, p) in rows {
        println!("  {mb:6} MB  last={}  dirty={dirty}  {p}", fmt_date(t));
    }
}

/// LRU by last touch. A clone with uncommitted work is never removed.
fn evict(c: &Cfg) {
    let total = du_mb(&c.cache);
    let over = total as i64 - c.cache_mb as i64;
    if total == 0 || over <= 0 {
        println!(
            "rp: cache {total} MB under {} GB budget, nothing to evict",
            c.cache_mb / 1024
        );
        return;
    }
    println!("rp: cache {total} MB is {over} MB over budget — evicting LRU");
    let mut rows = clones(c);
    rows.sort_by_key(|(t, _, _, _)| *t);
    let mut over = over;
    for (_, mb, dirty, p) in rows {
        if over <= 0 {
            break;
        }
        if dirty > 0 {
            println!("  keep (uncommitted work): {p}");
            continue;
        }
        match fs::remove_dir_all(&p) {
            Ok(()) => {
                println!("  evicted {mb} MB: {p}");
                over -= mb as i64;
            }
            Err(e) => eprintln!("  rp: could not remove {p}: {e}"),
        }
    }
}

fn arg(args: &[String], i: usize) -> String {
    args.get(i).cloned().unwrap_or_else(|| ".".into())
}

#[cfg(test)]
mod tests {
    use super::fmt_date;

    #[test]
    fn unix_origin_formats() {
        assert_eq!(fmt_date(1), "1970-01-01");
    }

    #[test]
    fn known_timestamps() {
        assert_eq!(fmt_date(1_735_689_600), "2025-01-01");
        assert_eq!(fmt_date(1_704_067_199), "2023-12-31");
        assert_eq!(fmt_date(1_792_089_573), "2026-10-15");
    }

    #[test]
    fn leap_day() {
        // 2024-02-29 00:00:00 UTC
        assert_eq!(fmt_date(1_709_164_800), "2024-02-29");
    }

    #[test]
    fn no_touch_formats_as_never() {
        assert_eq!(fmt_date(0), "never");
        assert_eq!(fmt_date(-1), "never");
    }

    #[test]
    fn slug_normalisation_strips_transport() {
        use super::norm_slug;
        assert_eq!(norm_slug("https://github.com/pallets/click.git"), "pallets/click");
        assert_eq!(norm_slug("git@github.com:pallets/click"), "pallets/click");
        assert_eq!(norm_slug("  pallets/click/  "), "pallets/click");
    }
}

const POINTER_START: &str =
    "<!-- repo-tiers:start (written by `rp install`; delete the block to opt out) -->";
const POINTER_END: &str = "<!-- repo-tiers:end -->";

/// The always-on pointer that lives in AGENTS.md. Everything else is behind it.
fn pointer_block(skills: &str) -> String {
    format!(
        "{POINTER_START}\n\
## Repo tiers — `rp` before you clone or index\n\
\n\
`rp` picks the cheapest tier for reaching a git repo: remote query, shallow or sparse clone, local grep, text index, code graph. Run `rp` in a repo, or `rp get owner/repo` for one not on disk, before cloning or indexing anything. Commands, measured costs and traps live in the `repo-tiers` skill at `{skills}`. Bootstrap a fresh box with `cargo build --release && ./target/release/rp install`.\n\
\n\
Carry this one always: **local disk is a cache with a budget, not a library**, and cloning costs more than indexing.\n\
{POINTER_END}"
    )
}

/// Replace the marked block, insert it before `## Shell`, or append. Never touches
/// prose outside the markers, so a hand-edited AGENTS.md survives a re-install.
fn upsert_pointer(md: &Path, block: &str) -> &'static str {
    let cur = fs::read_to_string(md).unwrap_or_default();
    let new = match (cur.find(POINTER_START), cur.find(POINTER_END)) {
        (Some(s), Some(e)) if e > s => {
            let after = e + POINTER_END.len();
            format!("{}{}{}", &cur[..s], block, &cur[after..])
        }
        _ if cur.trim().is_empty() => format!("{block}\n"),
        _ => match cur.find("\n## Shell") {
            Some(i) => format!("{}\n\n{}\n\n{}", &cur[..i], block, &cur[i + 1..]),
            None => format!("{}\n\n{block}\n", cur.trim_end()),
        },
    };
    // Keep exactly one blank line after the end marker, whichever branch ran.
    // Written as a replace so a re-install never stacks a second blank line.
    let new = new.replace(
        &format!("{POINTER_END}\n#"),
        &format!("{POINTER_END}\n\n#"),
    );
    if new == cur {
        return "unchanged";
    }
    if fs::write(md, new).is_err() {
        return "FAILED";
    }
    if cur.contains(POINTER_START) {
        "updated"
    } else if cur.trim().is_empty() {
        "created"
    } else {
        "inserted"
    }
}

fn cmd_install(c: &Cfg) {
    let skill_src = c.src.join("skill");
    if !skill_src.join("SKILL.md").exists() {
        eprintln!(
            "rp: skill source missing at {}\n     point RP_SRC at the rp project directory",
            skill_src.display()
        );
        exit(2);
    }
    if fs::create_dir_all(&c.skills).is_err() {
        eprintln!("rp: cannot create {}", c.skills.display());
        exit(1);
    }
    for name in ["SKILL.md", "COSTS.md"] {
        let from = skill_src.join(name);
        if !from.exists() {
            continue;
        }
        let to = c.skills.join(name);
        let same = fs::read_to_string(&from).ok() == fs::read_to_string(&to).ok();
        if !same && fs::copy(&from, &to).is_err() {
            println!("  {:<10} {}", "FAILED", to.display());
            continue;
        }
        println!(
            "  {:<10} {}",
            if same { "unchanged" } else { "installed" },
            to.display()
        );
    }
    println!(
        "  {:<10} {}",
        upsert_pointer(&c.agent_md, &pointer_block(&c.skills.display().to_string())),
        c.agent_md.display()
    );
    // Self-install only when launched from a build tree, so the installed binary
    // re-run never tries to overwrite the file it is running from.
    if let Ok(exe) = env::current_exe() {
        if exe.to_string_lossy().contains("/target/") {
            let dest = PathBuf::from(svar("RP_BIN", &format!("{}/.local/bin/rp", svar("HOME", "."))));
            if dest != exe.canonicalize().unwrap_or_else(|_| exe.clone()) {
                if let Some(p) = dest.parent() {
                    let _ = fs::create_dir_all(p);
                }
                let same = fs::read(&exe).ok() == fs::read(&dest).ok();
                if !same {
                    let _ = fs::copy(&exe, &dest);
                }
                println!(
                    "  {:<10} {}",
                    if same { "unchanged" } else { "installed" },
                    dest.display()
                );
            }
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn push_row(
    rows: &mut Vec<(&'static str, &'static str, String, &'static str)>,
    name: &'static str,
    rc: i32,
    out: &str,
    warn: bool,
    fix: &'static str,
) {
    let st = if rc == 0 {
        "ok"
    } else if warn {
        "warn"
    } else {
        "miss"
    };
    rows.push((name, st, first_line(out), fix));
}

fn cmd_doctor(c: &Cfg) {
    let mut rows: Vec<(&'static str, &'static str, String, &'static str)> = Vec::new();

    let (rc, o) = run("git", &["--version"]);
    push_row(&mut rows, "git", rc, &o, false, "install git: the clone tier depends on it");
    let (rc, o) = run("gh", &["--version"]);
    push_row(
        &mut rows,
        "gh cli",
        rc,
        &o,
        false,
        "install GitHub CLI: the R0 remote tier depends on it",
    );
    let (rc, o) = run("gh", &["auth", "status"]);
    push_row(
        &mut rows,
        "gh auth",
        rc,
        &o,
        false,
        "gh auth login  (without it R0 remote queries are dead)",
    );
    let (rc, o) = run("graphicle", &["--version"]);
    push_row(
        &mut rows,
        "graphicle",
        rc,
        &o,
        true,
        "install graphicle, or skip the T2 tier",
    );
    let (rc, o) = run("node", &["--version"]);
    push_row(&mut rows, "node", rc, &o, true, "context-mode's CLI needs node");
    let ctx_ok = Path::new(&c.ctxbin).exists();
    rows.push((
        "context-mode cli",
        if ctx_ok { "ok" } else { "warn" },
        c.ctxbin.clone(),
        "npm i -g context-mode, or export CTX_MODE_CLI=<path>",
    ));
    let (rc, o) = run("cargo", &["--version"]);
    push_row(&mut rows, "cargo", rc, &o, true, "only needed to rebuild rp");

    let free = free_gb();
    rows.push((
        "free disk",
        if free >= c.floor_gb { "ok" } else { "miss" },
        format!("{free} GB free, floor {} GB", c.floor_gb),
        "free space before cloning: rp evict, then clear elsewhere",
    ));
    let store = du_mb(&c.gstore);
    rows.push((
        "graphicle store",
        if store <= c.budget_mb { "ok" } else { "warn" },
        format!("{store} MB of {} MB", c.budget_mb),
        "no per-project delete exists: graphicle backup && mv the store, then rp warm what you need",
    ));
    let cache = du_mb(&c.cache);
    rows.push((
        "clone cache",
        if cache <= c.cache_mb { "ok" } else { "warn" },
        format!("{} MB of {} MB", cache, c.cache_mb),
        "rp evict",
    ));
    let skill_ok = c.skills.join("SKILL.md").exists();
    rows.push((
        "skill installed",
        if skill_ok { "ok" } else { "miss" },
        c.skills.display().to_string(),
        "rp install",
    ));
    let md = fs::read_to_string(&c.agent_md).unwrap_or_default();
    rows.push((
        "AGENTS.md pointer",
        if md.contains(POINTER_START) {
            "ok"
        } else {
            "miss"
        },
        c.agent_md.display().to_string(),
        "rp install",
    ));
    let cwd = env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".into());
    let cwd_root = repo_root(&cwd);
    let (rc, _) = run("graphicle", &["context", "probe", "--project", &cwd_root]);
    rows.push((
        "cwd graph (info)",
        "warn",
        if rc == 0 {
            format!("registered: {cwd_root}")
        } else {
            format!("not synced: {cwd_root}")
        },
        "",
    ));

    let mut miss = 0;
    let mut warn = 0;
    for (name, st, detail, fix) in &rows {
        let tag = match *st {
            "ok" => "[ ok ]",
            "warn" => "[warn]",
            _ => "[MISS]",
        };
        if *st == "miss" {
            miss += 1;
        }
        if *st == "warn" {
            warn += 1;
        }
        println!("{tag} {name:<19} {detail}");
        if *st != "ok" && !fix.is_empty() {
            println!("       → {fix}");
        }
    }
    println!("\n{} ok · {warn} warn · {miss} miss", rows.len() - warn - miss);
    if miss > 0 {
        exit(1);
    }
}

fn main() {
    // `rp | head` closes the pipe early; that is normal use, not a crash.
    // Handled here instead of pulling in libc just to reset SIGPIPE.
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{info}");
        if msg.contains("Broken pipe") || msg.contains("failed printing to stdout") {
            exit(0);
        }
        eprintln!("rp: {msg}");
        exit(1);
    }));

    let args: Vec<String> = env::args().skip(1).collect();
    let c = Cfg::new();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("state");
    match cmd {
        "state" => cmd_state(&c, &arg(&args, 1)),
        "warm" => cmd_warm(&c, &arg(&args, 1)),
        "docs" => cmd_docs(&c, &arg(&args, 1)),
        "top" => {
            let k = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(15);
            cmd_top(&c, k);
        }
        "gc" => cmd_gc(&c),
        "get" => match args.get(1) {
            Some(r) => cmd_get(&c, r),
            None => {
                eprintln!("rp get needs owner/repo\n{USAGE}");
                exit(2);
            }
        },
        "clone" => match args.get(1) {
            Some(r) => cmd_clone(&c, r, args.get(2).map(|s| s.as_str())),
            None => {
                eprintln!("rp clone needs owner/repo\n{USAGE}");
                exit(2);
            }
        },
        "cache" => cmd_cache(&c),
        "evict" => evict(&c),
        "install" => cmd_install(&c),
        "doctor" => cmd_doctor(&c),
        "-h" | "--help" | "help" => println!("{USAGE}"),
        other => {
            eprintln!("rp: unknown command: {other}\n{USAGE}");
            exit(2);
        }
    }
}
