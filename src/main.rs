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

#![deny(unsafe_code)]
#![deny(clippy::all)]

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{exit, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Failed I/O that must not pass as success. Every `warn_io` bumps this; `main`
/// exits 1 if it is non-zero, so a caller can tell "worked" from "printed anyway".
static ERRORS: AtomicUsize = AtomicUsize::new(0);
static JSON: AtomicBool = AtomicBool::new(false);
static QUIET: AtomicBool = AtomicBool::new(false);

/// Progress chatter: silenced by --quiet. Errors go to stderr and always print.
fn say(s: String) {
    if !QUIET.load(Ordering::Relaxed) {
        println!("{s}");
    }
}

fn json_on() -> bool {
    JSON.load(Ordering::Relaxed)
}

fn warn_io(what: &str, e: &std::io::Error) {
    ERRORS.fetch_add(1, Ordering::Relaxed);
    eprintln!("rp: {what}: {e}");
}

/// Minimal JSON string escaping — no serde, no crate.
fn jesc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn jstr(s: &str) -> String {
    format!("\"{}\"", jesc(s))
}

/// Array of raw strings (each quoted here).
fn jarr(items: &[String]) -> String {
    format!(
        "[{}]",
        items.iter().map(|s| jstr(s)).collect::<Vec<_>>().join(",")
    )
}

/// Array of already-built JSON values — joined verbatim, never re-escaped.
fn jarr_raw(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "usage: rp [flags] [command] [args]\n\
\n\
commands:\n\
  state    (default) tier of a local repo; records the visit\n\
  get      [host/]owner/repo: answer remotely, never clone (github, GHE, GitLab, Gitea)\n\
  clone    [host/]owner/repo [subpath]: budget-checked shallow / sparse clone\n\
  warm     graphicle code graph (callers, impact, why)\n\
  docs     context-mode text index\n\
  top      most-visited repos = warm candidates [N]\n\
  cache    clone cache: size, last touch, dirty flag\n\
  evict    LRU eviction; never removes a clone with uncommitted work\n\
  gc       graphicle store report and the reclaim path\n\
  install  bootstrap this box: binary + skill + agent memory pointers (idempotent)\n\
    --agents <a,b|all>   also write the pointer into each agent's global memory file\n\
    --file <path>        extra memory file to write (repeatable)\n\
  agents   list the supported agents, their memory files and install state\n\
  uninstall  undo install: pointer blocks + skill dir (--all also removes the binary)\n\
  doctor   toolchain, tier capability, budget and install health\n\
\n\
flags:\n\
  -j, --json   machine-readable output (state, get, top, cache, doctor, agents)\n\"
  -q, --quiet  suppress progress chatter; errors still print\n\
  -V, --version  print version\n\
  -h, --help   this text\n\
\n\
exit codes: 0 ok · 1 failed or degraded (see stderr) · 2 bad usage or bad input";

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
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
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
            if let Err(e) = fs::create_dir_all(p) {
                warn_io("cannot create ledger dir", &e);
            }
        }
        if let Err(e) = fs::create_dir_all(&c.markdir) {
            warn_io("cannot create index-mark dir", &e);
        }
        if let Err(e) = fs::create_dir_all(&c.cache) {
            warn_io("cannot create clone cache", &e);
        }
        c
    }
}

/// Run a program, return (exit code, stdout+stderr). Never panics on spawn failure.
fn run(prog: &str, args: &[&str]) -> (i32, String) {
    match Command::new(prog).args(args).stdin(Stdio::null()).output() {
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
    match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&c.ledger)
    {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{}\t{path}", now()) {
                warn_io("ledger write", &e);
            }
        }
        Err(e) => warn_io("ledger open", &e),
    }
}

fn visits(l: &Ledger, p: &str) -> usize {
    l.iter().filter(|(_, q)| q == p).count()
}

fn last_touch(l: &Ledger, p: &str) -> i64 {
    l.iter()
        .filter(|(_, q)| q == p)
        .map(|(t, _)| *t)
        .max()
        .unwrap_or(0)
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

/// Which forge serves a host. Decides which R0 commands rp can hand you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Forge {
    Github,
    GithubEnterprise,
    Gitlab,
    Gitea,
    Unknown,
}

impl Forge {
    fn label(self) -> &'static str {
        match self {
            Forge::Github => "github",
            Forge::GithubEnterprise => "github-enterprise",
            Forge::Gitlab => "gitlab",
            Forge::Gitea => "gitea",
            Forge::Unknown => "unknown",
        }
    }
}

/// A remote repo reference, host-qualified. A bare `owner/repo` means the default
/// host (`GH_HOST`, else github.com); anything carrying a host keeps it, so GitLab,
/// Gitea/Forgejo and GitHub Enterprise route the same way as github.com instead of
/// silently becoming a github.com clone.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Repo {
    host: String,
    owner: String,
    name: String,
    forge: Forge,
    url: String,
}

impl Repo {
    fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
    /// Ledger and display key. Host-qualified so `gitlab.com/foo/bar` and
    /// `github.com/foo/bar` never share a visit count or a cache directory.
    fn key(&self) -> String {
        format!("{}/{}", self.host, self.slug())
    }
    /// `owner%2Frepo` — GitLab takes the whole path as its `:id` URL segment,
    /// and with a nested subgroup the owner carries slashes of its own.
    fn id_enc(&self) -> String {
        self.slug().replace('/', "%2F")
    }
}

/// Hosts `gh` is authenticated against, straight out of `gh auth status`.
/// The authoritative way to tell a GitHub Enterprise host from an unknown one:
/// `gh` already knows, so rp does not have to guess from the hostname.
fn gh_auth_hosts() -> Vec<String> {
    let (rc, o) = run("gh", &["auth", "status"]);
    if rc != 0 {
        return Vec::new();
    }
    let mut hosts = Vec::new();
    const MARK: &str = "Logged in to ";
    for line in o.lines() {
        if let Some(i) = line.find(MARK) {
            let h: String = line[i + MARK.len()..]
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            if !h.is_empty() && !hosts.iter().any(|x| x == &h) {
                hosts.push(h);
            }
        }
    }
    hosts
}

fn forge_for(host: &str, gh_hosts: &[String]) -> Forge {
    let h = host.to_ascii_lowercase();
    if h == "github.com" {
        return Forge::Github;
    }
    // Authenticated against it with gh => it is a GitHub, whatever it is called.
    if gh_hosts.iter().any(|x| x.eq_ignore_ascii_case(&h)) {
        return Forge::GithubEnterprise;
    }
    if h.contains("gitlab") {
        return Forge::Gitlab;
    }
    if h.contains("gitea") || h.contains("forgejo") || h == "codeberg.org" {
        return Forge::Gitea;
    }
    if h.contains("github") {
        return Forge::GithubEnterprise;
    }
    Forge::Unknown
}

/// Host of a bare `owner/repo`. `GH_HOST` is the gh-native switch: set it and a
/// bare slug means your enterprise GitHub, exactly as it does for `gh` itself.
fn default_host() -> String {
    let h = env::var("GH_HOST").unwrap_or_default();
    let h = h.trim();
    if h.is_empty() {
        "github.com".to_string()
    } else {
        h.to_string()
    }
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && !h.starts_with('-')
        && !h.ends_with('-')
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Split a reference into (host, "owner/repo"). `None` = no host present, so the
/// caller supplies the default. Covers `https://`, `http://`, `ssh://`, `git://`,
/// scp-like `git@host:o/r`, and bare `host/owner/repo` — a dot in the first
/// segment is what marks a segment as a host rather than an owner.
fn split_host(s: &str) -> Option<(String, String)> {
    for scheme in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            let rest = match rest.rsplit_once('@') {
                Some((_, after)) => after,
                None => rest,
            };
            let (h, r) = rest.split_once('/')?;
            let h = h.split(':').next().unwrap_or(h); // drop :port, https clone needs none
            if h.is_empty() || r.is_empty() {
                return None;
            }
            return Some((h.to_string(), r.to_string()));
        }
    }
    if let Some(at) = s.find('@') {
        let after = &s[at + 1..];
        let (h, r) = after.split_once(':')?;
        if h.contains('.') && !r.is_empty() && !r.starts_with('/') {
            return Some((h.to_string(), r.to_string()));
        }
        return None;
    }
    let segs: Vec<&str> = s.split('/').collect();
    if segs.len() >= 3 && segs[0].contains('.') {
        return Some((segs[0].to_string(), segs[1..].join("/")));
    }
    None
}

/// Parse any of: `owner/repo`, `host/owner/repo`, `https://host/o/r[.git]`,
/// `ssh://git@host/o/r.git`, `git@host:o/r.git`. Every segment is checked with
/// `valid_name`, so traversal, empty segments and forge-invalid names are rejected
/// before `rp clone` can turn a typo into a write outside the cache.
fn parse_repo(raw: &str, gh_hosts: &[String]) -> Result<Repo, String> {
    let s = raw.trim().trim_matches('/');
    if s.is_empty() {
        return Err(format!("empty repo reference: {raw:?} (want owner/repo)"));
    }
    // `.git` is transport noise: strip it for parsing, keep it in the clone URL.
    let no_git = s.strip_suffix(".git").unwrap_or(s);
    // A dotted first segment with nothing after the repo is `host/repo`, which is
    // not a shape any forge has. Better a clear error than github.com/gitlab.com/x.
    let bare = !no_git.contains("://") && !no_git.contains('@');
    let shape: Vec<&str> = no_git.split('/').collect();
    if bare && shape.len() == 2 && shape[0].contains('.') {
        return Err(format!("want host/owner/repo, got {raw:?}"));
    }
    let (host, rest) = match split_host(no_git) {
        Some(hr) => hr,
        None => (default_host(), no_git.to_string()),
    };
    // "transport" means the reference named its own protocol: a scheme, or the
    // scp-style `user@host:`. A bare `host/owner/repo` does not — it gets https.
    let transport = no_git.contains("://") || no_git.contains('@');
    if !valid_host(&host) {
        return Err(format!("invalid host in {raw:?}: {host}"));
    }
    let segs: Vec<&str> = rest.split('/').collect();
    if segs.len() < 2 {
        return Err(format!("want owner/repo or host/owner/repo, got {raw:?}"));
    }
    if !segs.iter().all(|p| valid_name(p)) {
        return Err(format!("invalid repo slug in {raw:?}"));
    }
    let forge = forge_for(&host, gh_hosts);
    // GitHub has no nested groups: anything but exactly owner/repo is a typo there.
    // GitLab subgroups and Gitea org trees may nest, so only GitHub is strict.
    if matches!(forge, Forge::Github | Forge::GithubEnterprise) && segs.len() != 2 {
        return Err(format!("{host} repos are owner/repo, got {raw:?}"));
    }
    // GitLab nested subgroups: everything above the repo name is the owner.
    let name = segs[segs.len() - 1].to_string();
    let owner = segs[..segs.len() - 1].join("/");
    // A reference that arrived with its own transport keeps it verbatim: an
    // scp-style URL clones over ssh with the user's keys, an https one over https.
    let url = if transport {
        s.to_string()
    } else {
        format!("https://{host}/{owner}/{name}")
    };
    let forge = forge_for(&host, gh_hosts);
    Ok(Repo {
        host,
        owner,
        name,
        forge,
        url,
    })
}

/// The origin URL recorded in a clone, or None if it is not a git dir.
fn origin_of(dir: &Path) -> Option<String> {
    let (rc, o) = run(
        "git",
        &[
            "-C",
            &dir.to_string_lossy(),
            "config",
            "--get",
            "remote.origin.url",
        ],
    );
    if rc == 0 && !o.trim().is_empty() {
        Some(o.trim().to_string())
    } else {
        None
    }
}

/// `https://host/o/r`, `https://host/o/r.git` and `git@host:o/r.git` are one
/// origin. Normalised so a clone made over ssh matches a lookup made over https.
fn norm_origin(u: &str) -> String {
    let mut s = u.trim().trim_end_matches('/').to_string();
    for sc in ["https://", "http://", "ssh://", "git://"] {
        if let Some(r) = s.strip_prefix(sc) {
            s = r.to_string();
        }
    }
    if let Some((_, after)) = s.split_once('@') {
        s = after.to_string();
    }
    if let Some(i) = s.find(':') {
        let (h, r) = (&s[..i], &s[i + 1..]);
        if !h.contains('/') && r.contains('/') {
            s = format!("{h}/{r}");
        }
    }
    if let Some(r) = s.strip_suffix(".git") {
        s = r.to_string();
    }
    s.to_ascii_lowercase()
}

/// Cache directory for a repo. Prefers the bare repo name so clones made before rp
/// grew host-aware are still found; falls back to `host-owner-repo` when the bare
/// name is already taken by a *different* remote. Without this, `github.com/a/cli`
/// and `gitlab.com/a/cli` would fight over one directory.
fn cache_dir_for(c: &Cfg, repo: &Repo) -> PathBuf {
    let bare = c.cache.join(&repo.name);
    if !bare.join(".git").exists() {
        return bare;
    }
    match origin_of(&bare) {
        Some(o) if norm_origin(&o) == norm_origin(&repo.url) => bare,
        _ => c
            .cache
            .join(format!("{}-{}-{}", repo.host, repo.owner, repo.name)),
    }
}

/// Clone-free commands for this forge, cheapest first.
fn remote_cmds(repo: &Repo) -> Vec<(&'static str, String)> {
    let slug = repo.slug();
    let host = repo.host.as_str();
    match repo.forge {
        Forge::Github => vec![
            (
                "search",
                format!("gh search code 'PATTERN' --repo {slug} --limit 20"),
            ),
            (
                "tree",
                format!("gh api repos/{slug}/git/trees/HEAD?recursive=1 --jq '.tree[].path'"),
            ),
            (
                "file",
                format!("gh api repos/{slug}/contents/PATH --jq .content | base64 -d"),
            ),
        ],
        // Same CLI, different host. `gh` resolves /api/v3 and the keyring itself
        // from GH_HOST, so enterprise costs rp nothing but a prefix.
        Forge::GithubEnterprise => {
            let e = format!("GH_HOST={host} ");
            vec![
                (
                    "search",
                    format!("{e}gh search code 'PATTERN' --repo {slug} --limit 20"),
                ),
                (
                    "tree",
                    format!(
                        "{e}gh api repos/{slug}/git/trees/HEAD?recursive=1 --jq '.tree[].path'"
                    ),
                ),
                (
                    "file",
                    format!("{e}gh api repos/{slug}/contents/PATH --jq .content | base64 -d"),
                ),
            ]
        }
        Forge::Gitlab => {
            let id = repo.id_enc();
            // The header must vanish when the token is unset: GitLab answers an
            // empty `PRIVATE-TOKEN:` with a 302 to sign_in, but the same call
            // with no header at all returns the public repo.
            let tok = "${GITLAB_TOKEN:+-H \"PRIVATE-TOKEN: $GITLAB_TOKEN\"}";
            vec![
                (
                    "search",
                    format!(
                        "curl -s {tok} \"https://{host}/api/v4/projects/{id}/search?scope=blobs&search=PATTERN\""
                    ),
                ),
                (
                    "tree",
                    format!(
                        "curl -s {tok} \"https://{host}/api/v4/projects/{id}/repository/tree?recursive=true&per_page=1000\""
                    ),
                ),
                (
                    "file",
                    format!(
                        "curl -s {tok} \"https://{host}/api/v4/projects/{id}/repository/files/PATH_URL_ENCODED/raw?ref=HEAD\""
                    ),
                ),
            ]
        }
        Forge::Gitea => {
            let tok = "${GITEA_TOKEN:+-H \"Authorization: token $GITEA_TOKEN\"}";
            vec![
                (
                    "search",
                    format!(
                        "curl -s {tok} \"https://{host}/api/v1/search/code?q=repo:{slug}+PATTERN\""
                    ),
                ),
                (
                    "tree",
                    format!(
                        "curl -s {tok} \"https://{host}/api/v1/repos/{slug}/git/trees/HEAD?recursive=true\""
                    ),
                ),
                (
                    "file",
                    format!("curl -s {tok} \"https://{host}/api/v1/repos/{slug}/raw/HEAD/PATH\""),
                ),
            ]
        }
        // No forge API: the git-native floor is all there is, and it is enough.
        Forge::Unknown => {
            let tmp = format!("/tmp/rp-{}-{}", repo.owner, repo.name);
            vec![
                ("refs", format!("git ls-remote --symref {} HEAD", repo.url)),
                (
                    "tree",
                    format!(
                        "git clone --depth 1 --filter=blob:none --no-checkout {} {tmp} && git -C {tmp} ls-tree -r --name-only HEAD",
                        repo.url
                    ),
                ),
                (
                    "file",
                    format!("git archive --remote={} HEAD PATH | tar -x", repo.url),
                ),
            ]
        }
    }
}

/// The no-token, zero-byte floor. Works on every forge, so R0 never dies.
fn git_floor(repo: &Repo) -> String {
    format!("git ls-remote --symref {} HEAD", repo.url)
}

fn remote_notes(repo: &Repo) -> Vec<String> {
    match repo.forge {
        Forge::Github => {
            vec!["search cap is 30/min — batch queries, never loop per file".to_string()]
        }
        Forge::GithubEnterprise => vec![
            format!(
                "needs `gh auth login --hostname {}` first; gh speaks /api/v3 there so you never touch it",
                repo.host
            ),
            "site admins can disable code search — if `gh search` 404s, drop to the git floor"
                .to_string(),
        ],
        Forge::Gitlab => vec![
            "tree and raw files are open on public repos with no token at all".to_string(),
            "code search needs $GITLAB_TOKEN with scope read_api — anonymous blob search is 401"
                .to_string(),
            "never send the header empty: GitLab 302s it to sign_in, which is why the commands above use ${VAR:+…}"
                .to_string(),
        ],
        Forge::Gitea => vec![
            "tree and raw files are open on public repos with no token at all".to_string(),
            "code search needs $GITEA_TOKEN (or $FORGEJO_TOKEN) *and* a server with the repo indexer on — a 404 means this forge cannot search, use the git floor"
                .to_string(),
        ],
        Forge::Unknown => vec![
            format!("no forge API known for {} — git-native floor only", repo.host),
            "`git archive --remote` works on GitLab/Gitea, NOT on GitHub".to_string(),
            "ls-remote on a repo with a huge ref history can take 20s+ — always name HEAD, never list all refs"
                .to_string(),
        ],
    }
}

fn valid_name(p: &str) -> bool {
    !p.is_empty()
        && p != "."
        && p != ".."
        && !p.starts_with(".git")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Repo-relative sparse path: no absolute paths, no `..`, no NUL.
fn valid_subpath(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('/') && !s.split('/').any(|p| p == "..") && !s.contains('\0')
}

fn die_input(msg: &str) -> ! {
    eprintln!("rp: {msg}");
    exit(2)
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
    let (rc, o) = run("find", &[root, "-type", "f", "-not", "-path", "*/.git/*"]);
    if rc != 0 {
        warn_io(
            "file count (find)",
            &std::io::Error::other(format!("find exited {rc}")),
        );
        return 0;
    }
    o.lines().filter(|l| !l.is_empty()).count()
}

fn abs(p: &str) -> String {
    fs::canonicalize(p)
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string())
}

// ---------------------------------------------------------------- commands ---

fn cmd_state(c: &Cfg, path: &str) {
    if !Path::new(path).exists() {
        die_input(&format!("no such path: {path}"));
    }
    let root = repo_root(&abs(path));
    let name = base(&root);
    record(c, &root);
    let l = read_ledger(c);
    let v = visits(&l, &root);

    let (tier, label, note, next): (&str, &str, &str, Vec<String>) = if registered(&root) {
        (
            "T2",
            "graph ready",
            "",
            vec!["recall_project / query_graph (MCP) — callers, impact, why".into()],
        )
    } else if mark_path(c, &root).exists() {
        (
            "T1",
            "text index ready",
            "",
            vec!["ctx_search(queries:[...]) — prose/log/code text, no graph".into()],
        )
    } else if v >= 2 {
        (
            "T0",
            "cold — repeat visitor, indexing now pays",
            "",
            vec![
                "grep -rn 'PATTERN' .            (0.02s, free)".into(),
                "rp warm .                       (graphicle graph, background)".into(),
                "rp docs .                       (context-mode text index)".into(),
            ],
        )
    } else {
        (
            "T0",
            "cold — DO NOT INDEX",
            "indexing costs ~1-10s and 9-380 MB; a one-touch repo never earns it back",
            vec!["grep -rn 'PATTERN' .   |   rtk read FILE --max-lines 40".into()],
        )
    };

    let mb = du_mb(&c.gstore);
    let over = mb > c.budget_mb;

    if json_on() {
        println!(
            "{{\"repo\":{},\"path\":{},\"visits\":{},\"tier\":{},\"tier_label\":{},\"note\":{},\"next\":{},\"store_mb\":{},\"store_budget_mb\":{},\"over_budget\":{}}}",
            jstr(&name),
            jstr(&root),
            v,
            jstr(tier),
            jstr(label),
            jstr(note),
            jarr(&next),
            mb,
            c.budget_mb,
            over
        );
        return;
    }

    println!("repo: {name}  visits={v}  tier: {tier} {label}");
    for n in &next {
        println!("  {n}");
    }
    if !note.is_empty() {
        println!("  note: {note}");
    }
    let overtxt = if over { " — OVER: run rp gc" } else { "" };
    println!("  store: {mb} MB / {} MB budget{overtxt}", c.budget_mb);
}

fn cmd_warm(c: &Cfg, path: &str) {
    if !Path::new(path).exists() {
        die_input(&format!("no such path: {path}"));
    }
    let root = repo_root(&abs(path));
    record(c, &root);
    say(format!("rp: graphicle sync {root} (background)"));
    let (rc, o) = run("graphicle", &["sync", &root, "--background"]);
    say(tail(&o, 3));
    if rc != 0 {
        eprintln!("rp: sync failed");
        exit(1);
    }
    say(format!("rp: store now {} MB", du_mb(&c.gstore)));
}

fn cmd_docs(c: &Cfg, path: &str) {
    if !Path::new(path).exists() {
        die_input(&format!("no such path: {path}"));
    }
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
    let cap = found.clamp(1, 3000).to_string();
    let src = format!("repo:{}", base(&root));
    say(format!(
        "rp: context-mode index {root} ({cap} files, ~{} MB)",
        cap.parse::<u64>().unwrap_or(0) * 205 / 1024
    ));
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
    say(tail(&o2, 3));
    if rc != 0 {
        eprintln!("rp: index failed");
        exit(1);
    }
    if let Err(e) = fs::File::create(mark_path(c, &root)) {
        warn_io("index mark write", &e);
    }
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
    let l = read_ledger(c);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, p) in &l {
        *counts.entry(p.clone()).or_insert(0) += 1;
    }
    let mut rows: Vec<(usize, String)> = counts.into_iter().map(|(p, cnt)| (cnt, p)).collect();
    rows.sort_by_key(|(cnt, p)| (Reverse(*cnt), p.clone()));
    rows.truncate(k);
    if json_on() {
        let items: Vec<String> = rows
            .iter()
            .map(|(cnt, p)| {
                format!(
                    "{{\"path\":{},\"visits\":{},\"label\":{}}}",
                    jstr(p),
                    cnt,
                    jstr(&label_for(c, &l, p))
                )
            })
            .collect();
        println!("{{\"repos\":{}}}", jarr_raw(&items));
        return;
    }
    println!("most-visited repos (warm candidates):");
    for (cnt, p) in rows {
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
        .filter_map(|l| {
            l.split_once('\t')
                .map(|(a, b)| (a.to_string(), b.to_string()))
        })
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
        println!(
            "registered folders (store order, oldest first): {}/{} shown",
            folders.len().min(40),
            folders.len()
        );
        for (u, p) in folders.iter().take(40) {
            println!("  {u}  {p}");
        }
    }
    println!("clone cache: {} MB", du_mb(&c.cache));
    println!(
        "no per-project delete exists in the graphicle CLI (verified: delete_* / forget_* all"
    );
    println!("return \"Unsupported native project operation\") — so the store only grows.");
    println!(
        "To reclaim: graphicle backup && mv ~/.local/share/graphicle ~/.local/share/graphicle.old,"
    );
    println!(
        "then rp warm only what you need. Prevention beats cleanup: rp warms on repeat visits."
    );
}

fn cmd_get(c: &Cfg, raw: &str) {
    let repo = match parse_repo(raw, &gh_auth_hosts()) {
        Ok(r) => r,
        Err(e) => die_input(&e),
    };
    let slug = repo.key();
    let dest = cache_dir_for(c, &repo);
    if dest.join(".git").exists() {
        return cmd_state(c, &dest.to_string_lossy());
    }
    record(c, &format!("remote:{slug}"));
    let cmds = remote_cmds(&repo);
    let floor = git_floor(&repo);
    let clone_hint = format!("rp clone {slug} [subpath]");
    let mut notes = remote_notes(&repo);
    notes.push(format!(
        "clone only when you will grep more than ~5 times: {clone_hint}"
    ));
    if json_on() {
        let cjson: Vec<String> = cmds
            .iter()
            .map(|(k, v)| format!("{}:{}", jstr(k), jstr(v)))
            .collect();
        println!(
            "{{\"repo\":{},\"host\":{},\"forge\":{},\"url\":{},\"tier\":\"R0\",\"cloned\":false,\"commands\":{{{}}},\"floor\":{},\"notes\":{}}}",
            jstr(&slug),
            jstr(&repo.host),
            jstr(repo.forge.label()),
            jstr(&repo.url),
            cjson.join(","),
            jstr(&floor),
            jarr(&notes)
        );
        return;
    }
    println!(
        "repo: {slug}  forge: {}  tier: R0 remote — NOT cloned (a clone costs 50-460 MB and 10-40s)",
        repo.forge.label()
    );
    for (k, v) in &cmds {
        println!("  {k}: {v}");
    }
    println!("  floor (no token, 0 MB): {floor}");
    for n in &notes {
        println!("  note: {n}");
    }
}

fn cmd_clone(c: &Cfg, raw: &str, sub: Option<&str>) {
    let repo = match parse_repo(raw, &gh_auth_hosts()) {
        Ok(r) => r,
        Err(e) => die_input(&e),
    };
    let slug = repo.key();
    if let Some(sp) = sub {
        if !valid_subpath(sp) {
            die_input(&format!(
                "invalid subpath: {sp} (repo-relative; no absolute path, no '..')"
            ));
        }
    }
    let dest = cache_dir_for(c, &repo);
    let ds = dest.to_string_lossy().into_owned();
    if dest.join(".git").exists() {
        say(format!("rp: already cloned at {ds}"));
        return cmd_state(c, &ds);
    }
    let free = free_gb();
    if free < c.floor_gb {
        say(format!(
            "rp: only {free} GB free (< {} GB floor) — evicting LRU clones first",
            c.floor_gb
        ));
        evict(c);
    }
    let url = repo.url.clone();
    match sub {
        Some(sp) => {
            say(format!(
                "rp: sparse clone {slug} -> only '{sp}' (cheapest by far on monorepos)"
            ));
            // blob:none keeps the fetch trees-only: the cone's blobs arrive at checkout,
            // everything else stays remote and lazy-fetches if a history read needs it.
            let (rc, o) = run(
                "git",
                &[
                    "clone",
                    "--depth",
                    "1",
                    "--filter=blob:none",
                    "--sparse",
                    &url,
                    &ds,
                ],
            );
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
            say(format!("rp: shallow clone {slug} (whole tree)"));
            let (rc, o) = run("git", &["clone", "--depth", "1", &url, &ds]);
            if rc != 0 {
                eprintln!("rp: clone failed\n{}", tail(&o, 3));
                exit(1);
            }
        }
    }
    record(c, &ds);
    say(format!(
        "rp: {ds} = {} MB, {} files",
        du_mb(Path::new(&ds)),
        count_files(&ds)
    ));
    say(format!(
        "rp: free now {} GB; expand: git -C {ds} sparse-checkout add <dir>",
        free_gb()
    ));
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
    let total = du_mb(&c.cache);
    let mut rows = clones(c);
    rows.sort_by_key(|(t, _, _, _)| *t);
    if json_on() {
        let items: Vec<String> = rows
            .iter()
            .map(|(t, mb, dirty, p)| {
                format!(
                    "{{\"path\":{},\"mb\":{},\"last_ts\":{},\"last\":{},\"dirty\":{}}}",
                    jstr(p),
                    mb,
                    t,
                    jstr(&fmt_date(*t)),
                    dirty
                )
            })
            .collect();
        println!(
            "{{\"cache_dir\":{},\"total_mb\":{},\"budget_mb\":{},\"clones\":{}}}",
            jstr(&c.cache.display().to_string()),
            total,
            c.cache_mb,
            jarr(&items)
        );
        return;
    }
    println!(
        "clone cache: {} ({total} MB / {} GB budget)",
        c.cache.display(),
        c.cache_mb / 1024
    );
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

const POINTER_START: &str =
    "<!-- repo-tiers:start (written by `rp install`; delete the block to opt out) -->";
const POINTER_END: &str = "<!-- repo-tiers:end -->";

/// The pointer text. Everything else is behind it.
fn pointer_body(skills: &str) -> String {
    "## Repo tiers — `rp` before you clone or index\n\
\n\
`rp` picks the cheapest tier for reaching a git repo: remote query, shallow or sparse clone, local grep, text index, code graph. Run `rp` in a repo, or `rp get owner/repo` for one not on disk, before cloning or indexing anything. Commands, measured costs and traps live in the `repo-tiers` skill at `SKILLS`. Bootstrap a fresh box with `cargo build --release && ./target/release/rp install`.\n\
\n\
Carry this one always: **local disk is a cache with a budget, not a library**, and cloning costs more than indexing."
        .replace("SKILLS", skills)
}

fn pointer_block(skills: &str) -> String {
    format!("{POINTER_START}\n{}\n{POINTER_END}", pointer_body(skills))
}

/// Global memory file each supported agent reads. Verified against this machine:
/// every path below exists here except where noted, and each is the file that
/// agent loads for all projects.
struct AgentTarget {
    name: &'static str,
    path: PathBuf,
    /// Where this agent looks for skills. The skill itself lives once in the shared
    /// ~/.agents/skills library and is symlinked in here, the way every other
    /// skill on this box is wired.
    skills_dir: PathBuf,
    /// Cursor loads `.mdc` rule files with frontmatter instead of a plain memory file.
    cursor_rule: bool,
}

fn agent_targets() -> Vec<AgentTarget> {
    let home = svar("HOME", ".");
    let t = |name: &'static str, rel: String, skills_rel: String, cursor_rule: bool| AgentTarget {
        name,
        path: PathBuf::from(rel),
        skills_dir: PathBuf::from(skills_rel),
        cursor_rule,
    };
    vec![
        t(
            "pi",
            format!("{home}/.pi/agent/AGENTS.md"),
            format!("{home}/.pi/agent/skills"),
            false,
        ),
        t(
            "claude",
            format!("{home}/.claude/CLAUDE.md"),
            format!("{home}/.claude/skills"),
            false,
        ),
        t(
            "codex",
            format!("{home}/.codex/AGENTS.md"),
            format!("{home}/.codex/skills"),
            false,
        ),
        t(
            "gemini",
            format!("{home}/.gemini/GEMINI.md"),
            format!("{home}/.gemini/skills"),
            false,
        ),
        t(
            "copilot",
            format!("{home}/.copilot/instructions.md"),
            format!("{home}/.copilot/skills"),
            false,
        ),
        t(
            "cursor",
            format!("{home}/.cursor/rules/repo-tiers.mdc"),
            format!("{home}/.cursor/skills"),
            true,
        ),
        t(
            "opencode",
            format!("{home}/.config/opencode/AGENTS.md"),
            format!("{home}/.config/opencode/skills"),
            false,
        ),
        t(
            "hermes",
            format!("{home}/.hermes/AGENTS.md"),
            format!("{home}/.hermes/skills"),
            false,
        ),
    ]
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn make_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "skill symlinks need a unix filesystem",
    ))
}

/// Symlink the shared skill library entry into one agent's skills dir, so that
/// agent's skill loader discovers `repo-tiers`. Never clobbers: a link pointing
/// somewhere else, or a real directory we did not create, is reported and left be.
fn link_skill(skills_dir: &Path, skill_root: &Path) -> &'static str {
    let link = skills_dir.join("repo-tiers");
    if let Ok(target) = fs::read_link(&link) {
        return if target == skill_root {
            "unchanged"
        } else {
            "skipped (link points elsewhere)"
        };
    }
    if link.exists() {
        return "skipped (foreign skill dir)";
    }
    if let Err(e) = fs::create_dir_all(skills_dir) {
        warn_io("cannot create agent skills dir", &e);
        return "FAILED";
    }
    match make_symlink(skill_root, &link) {
        Ok(()) => "linked",
        Err(e) => {
            warn_io("skill symlink", &e);
            "FAILED"
        }
    }
}

/// Remove only a link we recognise. A real directory at that path is never touched.
fn unlink_skill(skills_dir: &Path, skill_root: &Path) -> bool {
    let link = skills_dir.join("repo-tiers");
    match fs::read_link(&link) {
        Ok(target) if target == skill_root => match fs::remove_file(&link) {
            Ok(()) => true,
            Err(e) => {
                warn_io("skill symlink removal", &e);
                false
            }
        },
        _ => false,
    }
}

fn cursor_rule_text(skills: &str) -> String {
    format!(
        "---\ndescription: Repo tiers - the cheapest way to reach a git repo\nalwaysApply: true\n---\n{}\n",
        pointer_body(skills)
    )
}

/// Write the pointer into one agent's memory file. Marker-guarded for plain files;
/// a Cursor rule we own is rewritten whole, a foreign one is left alone.
fn install_target(path: &Path, cursor_rule: bool, skills: &str) -> &'static str {
    if let Some(p) = path.parent() {
        if let Err(e) = fs::create_dir_all(p) {
            warn_io("cannot create agent dir", &e);
            return "FAILED";
        }
    }
    let cur = fs::read_to_string(path).unwrap_or_default();
    if cursor_rule {
        if cur.contains("Repo tiers") {
            let want = cursor_rule_text(skills);
            if cur == want {
                return "unchanged";
            }
            return match fs::write(path, want) {
                Ok(()) => "updated",
                Err(e) => {
                    warn_io("cursor rule write", &e);
                    "FAILED"
                }
            };
        }
        if !cur.trim().is_empty() {
            return "skipped (foreign rule file)";
        }
        return match fs::write(path, cursor_rule_text(skills)) {
            Ok(()) => "created",
            Err(e) => {
                warn_io("cursor rule write", &e);
                "FAILED"
            }
        };
    }
    upsert_pointer(path, &pointer_block(skills))
}

fn target_ours(path: &Path, cursor_rule: bool) -> bool {
    let cur = fs::read_to_string(path).unwrap_or_default();
    if cursor_rule {
        cur.contains("Repo tiers")
    } else {
        cur.contains(POINTER_START)
    }
}

fn resolve_agents(spec: &str) -> Result<Vec<String>, String> {
    let all: Vec<String> = agent_targets().iter().map(|t| t.name.to_string()).collect();
    if spec.eq_ignore_ascii_case("all") {
        return Ok(all);
    }
    let mut out = Vec::new();
    for raw in spec.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if !all.iter().any(|n| n.eq_ignore_ascii_case(raw)) {
            return Err(format!("unknown agent '{raw}' (known: {})", all.join(", ")));
        }
        out.push(raw.to_lowercase());
    }
    Ok(out)
}

fn cmd_agents(c: &Cfg) {
    let skills = c.skills.display().to_string();
    if json_on() {
        let skills_root = c.skills.clone();
        let items: Vec<String> = agent_targets()
            .iter()
            .map(|t| {
                let link = t.skills_dir.join("repo-tiers");
                let state = match fs::read_link(&link) {
                    Ok(target) if target == skills_root => "linked",
                    Ok(_) => "elsewhere",
                    Err(_) => {
                        if link.exists() {
                            "foreign"
                        } else {
                            "none"
                        }
                    }
                };
                format!(
                    "{{\"agent\":{},\"memory_file\":{},\"exists\":{},\"rp_installed\":{},\"skill_link\":{},\"skill_link_path\":{}}}",
                    jstr(t.name),
                    jstr(&t.path.display().to_string()),
                    t.path.exists(),
                    target_ours(&t.path, t.cursor_rule),
                    jstr(state),
                    jstr(&link.display().to_string())
                )
            })
            .collect();
        println!(
            "{{\"agents\":{},\"skill_dir\":{}}}",
            jarr_raw(&items),
            jstr(&skills)
        );
        return;
    }
    let skills_root = c.skills.clone();
    println!("shared skill library: {}\n", skills_root.display());
    for t in agent_targets() {
        let here = if t.path.exists() { "present" } else { "absent" };
        let ours = if target_ours(&t.path, t.cursor_rule) {
            "yes"
        } else {
            "no"
        };
        let link = t.skills_dir.join("repo-tiers");
        let linked = match fs::read_link(&link) {
            Ok(target) if target == skills_root => "linked here",
            Ok(_) => "link points elsewhere",
            Err(_) => {
                if link.exists() {
                    "foreign dir in the way"
                } else {
                    "not linked"
                }
            }
        };
        println!("  {:<10} {}", t.name, t.path.display());
        println!("  {:<10} memory {here} · rp pointer {ours}", "");
        println!("  {:<10} {}", "", link.display());
        println!("  {:<10} skill {linked}", "");
    }
    println!("\nwrite them with: rp install --agents all   (or a comma list)");
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
    let new = new.replace(&format!("{POINTER_END}\n#"), &format!("{POINTER_END}\n\n#"));
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

fn cmd_install(c: &Cfg, agents: &[String], files: &[String]) {
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
    let skills = c.skills.display().to_string();
    if agents.is_empty() && files.is_empty() {
        println!(
            "  {:<10} {}",
            upsert_pointer(&c.agent_md, &pointer_block(&skills)),
            c.agent_md.display()
        );
    } else {
        for want in agents {
            match agent_targets().into_iter().find(|t| t.name == *want) {
                Some(t) => {
                    println!(
                        "  {:<10} {}  [{} memory]",
                        install_target(&t.path, t.cursor_rule, &skills),
                        t.path.display(),
                        t.name
                    );
                    println!(
                        "  {:<10} {}  [{} skills]",
                        link_skill(&t.skills_dir, &c.skills),
                        t.skills_dir.join("repo-tiers").display(),
                        t.name
                    );
                }
                None => {
                    eprintln!("rp: unknown agent {want}");
                    exit(2);
                }
            }
        }
        for f in files {
            let p = PathBuf::from(f);
            println!(
                "  {:<10} {}",
                upsert_pointer(&p, &pointer_block(&skills)),
                p.display()
            );
        }
    }
    // Self-install only when launched from a build tree, so the installed binary
    // re-run never tries to overwrite the file it is running from.
    if let Ok(exe) = env::current_exe() {
        if exe.to_string_lossy().contains("/target/") {
            let dest = PathBuf::from(svar(
                "RP_BIN",
                &format!("{}/.local/bin/rp", svar("HOME", ".")),
            ));
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

/// Cut the marked pointer block out of AGENTS.md and collapse the gap it leaves.
/// Prose outside the markers is never touched.
fn remove_pointer(md: &Path) -> &'static str {
    let Ok(cur) = fs::read_to_string(md) else {
        return "absent";
    };
    let (Some(s), Some(e)) = (cur.find(POINTER_START), cur.find(POINTER_END)) else {
        return "absent";
    };
    if e < s {
        return "absent";
    }
    let mut new = format!("{}{}", &cur[..s], &cur[e + POINTER_END.len()..]);
    while new.contains("\n\n\n") {
        new = new.replace("\n\n\n", "\n\n");
    }
    if s == 0 {
        new = new.trim_start_matches('\n').to_string();
    }
    if fs::write(md, new).is_err() {
        return "FAILED";
    }
    "removed"
}

/// Undo `install`. Sweeps every agent memory file, but removes only what rp wrote:
/// its own marker block in plain files, its own Cursor rule, its own skill dir.
fn cmd_uninstall(c: &Cfg, all: bool) {
    println!(
        "  {:<10} {}",
        remove_pointer(&c.agent_md),
        c.agent_md.display()
    );
    for t in agent_targets() {
        if unlink_skill(&t.skills_dir, &c.skills) {
            println!(
                "  {:<10} {}  [{} skills]",
                "removed",
                t.skills_dir.join("repo-tiers").display(),
                t.name
            );
        }
        if t.path == c.agent_md || !target_ours(&t.path, t.cursor_rule) {
            continue;
        }
        if t.cursor_rule {
            if let Err(e) = fs::remove_file(&t.path) {
                warn_io("cursor rule removal", &e);
            } else {
                println!("  {:<10} {}  [{}]", "removed", t.path.display(), t.name);
            }
        } else {
            println!(
                "  {:<10} {}  [{}]",
                remove_pointer(&t.path),
                t.path.display(),
                t.name
            );
        }
    }
    let sk = c.skills.join("SKILL.md");
    if sk.exists() {
        let ours = fs::read_to_string(&sk)
            .map(|t| t.contains("name: repo-tiers"))
            .unwrap_or(false);
        if !ours {
            println!(
                "  {:<10} {} (not the repo-tiers skill; left alone)",
                "skipped",
                c.skills.display()
            );
        } else if let Err(e) = fs::remove_dir_all(&c.skills) {
            warn_io("skill dir removal", &e);
        } else {
            println!("  {:<10} {}", "removed", c.skills.display());
        }
    } else {
        println!("  {:<10} {}", "absent", c.skills.display());
    }
    if all {
        let bin = PathBuf::from(svar(
            "RP_BIN",
            &format!("{}/.local/bin/rp", svar("HOME", ".")),
        ));
        if bin.exists() {
            if let Err(e) = fs::remove_file(&bin) {
                warn_io("binary removal", &e);
            } else {
                println!("  {:<10} {}", "removed", bin.display());
            }
        } else {
            println!("  {:<10} {}", "absent", bin.display());
        }
    } else {
        println!("  {:<10} binary (pass --all to remove it)", "kept");
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

fn status_of(rows: &[(&'static str, &'static str, String, &'static str)], name: &str) -> bool {
    rows.iter().any(|r| r.0 == name && r.1 == "ok")
}

/// Which tiers this box can actually serve, given what is installed.
fn live_tiers(rows: &[(&'static str, &'static str, String, &'static str)]) -> Vec<&'static str> {
    let mut v = vec!["T0"];
    // R0 never dies: `git ls-remote` is a zero-byte remote query against every
    // forge, so git alone serves it. The forge APIs only buy code *search* on top.
    if status_of(rows, "git") {
        v.push("R0");
    }
    if status_of(rows, "git") {
        v.push("R1");
    }
    if status_of(rows, "node") && status_of(rows, "context-mode cli") {
        v.push("T1");
    }
    if status_of(rows, "graphicle") {
        v.push("T2");
    }
    v.sort_unstable();
    v
}

fn missing_tiers(
    rows: &[(&'static str, &'static str, String, &'static str)],
) -> Vec<(&'static str, &'static str)> {
    let mut m = Vec::new();
    if !status_of(rows, "git") {
        m.push((
            "R0",
            "install git — without it there is no remote floor at all",
        ));
        m.push(("R1", "install git"));
    }
    if !(status_of(rows, "node") && status_of(rows, "context-mode cli")) {
        m.push(("T1", "install node, then: npm install -g context-mode"));
    }
    if !status_of(rows, "graphicle") {
        m.push((
            "T2",
            "install graphicle (see README: Dependencies) or set GRAPHICLE_DATA_DIR",
        ));
    }
    m
}

/// Per-forge: can rp actually search this box's remotes, right now. The git floor
/// is always live; what varies is the code-search API on top of it.
fn forge_search_report(rows: &[(&'static str, &'static str, String, &'static str)]) -> String {
    let gh = status_of(rows, "gh cli") && status_of(rows, "gh auth");
    let curl = status_of(rows, "curl");
    let gl_tok = !env::var("GITLAB_TOKEN")
        .unwrap_or_default()
        .trim()
        .is_empty();
    let gt_tok = !env::var("GITEA_TOKEN")
        .or_else(|_| env::var("FORGEJO_TOKEN"))
        .unwrap_or_default()
        .trim()
        .is_empty();
    let ent = gh_auth_hosts().iter().any(|h| h != "github.com");
    format!(
        "github {} · enterprise {} · gitlab {} · gitea {} · unknown hosts: git floor only",
        if gh { "ok" } else { "NO (gh not authed)" },
        if ent {
            "ok"
        } else {
            "NO (gh auth login --hostname <host>)"
        },
        if !curl {
            "NO (curl missing)"
        } else if gl_tok {
            "ok"
        } else {
            "public only (no $GITLAB_TOKEN)"
        },
        if !curl {
            "NO (curl missing)"
        } else if gt_tok {
            "ok"
        } else {
            "tree/raw only (no $GITEA_TOKEN)"
        }
    )
}

fn cmd_doctor(c: &Cfg) {
    let mut rows: Vec<(&'static str, &'static str, String, &'static str)> = Vec::new();

    let (rc, o) = run("git", &["--version"]);
    push_row(
        &mut rows,
        "git",
        rc,
        &o,
        false,
        "install git: the clone tier depends on it",
    );
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
        "gh auth login  (without it GitHub / GitHub Enterprise R0 search is dead; the git floor still works)",
    );
    let (rc, o) = run("curl", &["--version"]);
    push_row(
        &mut rows,
        "curl",
        rc,
        &o,
        true,
        "install curl: GitLab and Gitea/Forgejo R0 are plain REST",
    );
    let (rc, o) = run("glab", &["--version"]);
    push_row(
        &mut rows,
        "glab cli",
        rc,
        &o,
        true,
        "optional: rp's GitLab R0 uses curl + $GITLAB_TOKEN either way",
    );
    let gl_tok = env::var("GITLAB_TOKEN").unwrap_or_default();
    rows.push((
        "$GITLAB_TOKEN",
        if gl_tok.trim().is_empty() {
            "warn"
        } else {
            "ok"
        },
        if gl_tok.trim().is_empty() {
            "unset — private GitLab search is off".to_string()
        } else {
            "set — private GitLab search is on".to_string()
        },
        "export GITLAB_TOKEN=<pat with scope read_api>",
    ));
    let gt_tok = env::var("GITEA_TOKEN")
        .or_else(|_| env::var("FORGEJO_TOKEN"))
        .unwrap_or_default();
    rows.push((
        "$GITEA_TOKEN",
        if gt_tok.trim().is_empty() {
            "warn"
        } else {
            "ok"
        },
        if gt_tok.trim().is_empty() {
            "unset — Gitea/Forgejo code search is off".to_string()
        } else {
            "set — Gitea/Forgejo code search is on".to_string()
        },
        "export GITEA_TOKEN=<access token> (or FORGEJO_TOKEN)",
    ));
    let hosts = gh_auth_hosts();
    rows.push((
        "gh hosts",
        if hosts.is_empty() { "warn" } else { "ok" },
        if hosts.is_empty() {
            "none".to_string()
        } else {
            hosts.join(", ")
        },
        "gh auth login --hostname github.corp.example  (one gh login per enterprise host)",
    ));
    rows.push(("R0 forge search", "warn", forge_search_report(&rows), ""));
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
    push_row(
        &mut rows,
        "node",
        rc,
        &o,
        true,
        "context-mode's CLI needs node",
    );
    let ctx_ok = Path::new(&c.ctxbin).exists();
    rows.push((
        "context-mode cli",
        if ctx_ok { "ok" } else { "warn" },
        c.ctxbin.clone(),
        "npm i -g context-mode, or export CTX_MODE_CLI=<path>",
    ));
    let (rc, o) = run("cargo", &["--version"]);
    push_row(
        &mut rows,
        "cargo",
        rc,
        &o,
        true,
        "only needed to rebuild rp",
    );

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

    let live = live_tiers(&rows);
    let missing = missing_tiers(&rows);

    let mut miss = 0;
    let mut warn = 0;
    if json_on() {
        let items: Vec<String> = rows
            .iter()
            .map(|(n, s, d, f)| {
                format!(
                    "{{\"name\":{},\"status\":{},\"detail\":{},\"fix\":{}}}",
                    jstr(n),
                    jstr(s),
                    jstr(d),
                    jstr(f)
                )
            })
            .collect();
        let gaps: Vec<String> = missing
            .iter()
            .map(|(t, why)| format!("{{\"tier\":{},\"needs\":{}}}", jstr(t), jstr(why)))
            .collect();
        let jmiss = rows.iter().filter(|r| r.1 == "miss").count();
        let jwarn = rows.iter().filter(|r| r.1 == "warn").count();
        println!(
            "{{\"checks\":{},\"ok\":{},\"warn\":{},\"miss\":{},\"tiers\":{},\"tiers_missing\":{}}}",
            jarr_raw(&items),
            rows.len() - jmiss - jwarn,
            jwarn,
            jmiss,
            jarr(&live.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
            jarr_raw(&gaps)
        );
        if jmiss > 0 {
            exit(1);
        }
        return;
    }
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
    println!("\ntiers live: {}", live.join(" "));
    if missing.is_empty() {
        println!("  every tier rp knows is available on this box");
    } else {
        for (t, why) in &missing {
            println!("  {t} unavailable → {why}");
        }
    }
    println!(
        "\n{} ok · {warn} warn · {miss} miss",
        rows.len() - warn - miss
    );
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

    let raw: Vec<String> = env::args().skip(1).collect();
    let mut args: Vec<String> = Vec::new();
    let mut all = false;
    let mut agents: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < raw.len() {
        match raw[i].as_str() {
            "-j" | "--json" => JSON.store(true, Ordering::Relaxed),
            "-q" | "--quiet" => QUIET.store(true, Ordering::Relaxed),
            "-V" | "--version" => {
                println!("rp {VERSION}");
                return;
            }
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return;
            }
            "--all" => all = true,
            "--agents" => {
                let v = raw.get(i + 1).cloned().unwrap_or_default();
                if v.is_empty() {
                    die_input("--agents needs a comma list (pi,claude,...) or 'all'");
                }
                agents = resolve_agents(&v).unwrap_or_else(|e| die_input(&e));
                i += 1;
            }
            "--file" => {
                let v = raw.get(i + 1).cloned().unwrap_or_default();
                if v.is_empty() {
                    die_input("--file needs a path to an agent memory file");
                }
                files.push(v);
                i += 1;
            }
            other => args.push(other.to_string()),
        }
        i += 1;
    }
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
        "install" => cmd_install(&c, &agents, &files),
        "agents" => cmd_agents(&c),
        "uninstall" => cmd_uninstall(&c, all),
        "doctor" => cmd_doctor(&c),
        "-h" | "--help" | "help" => println!("{USAGE}"),
        other => {
            eprintln!("rp: unknown command: {other}\n{USAGE}");
            exit(2);
        }
    }

    // A swallowed I/O failure must not look like success.
    if ERRORS.load(Ordering::Relaxed) > 0 {
        exit(1);
    }
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
    fn bare_slug_is_github() {
        use super::{parse_repo, Forge};
        let r = parse_repo("pallets/click", &[]).unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.slug(), "pallets/click");
        assert_eq!(r.forge, Forge::Github);
        assert_eq!(r.url, "https://github.com/pallets/click");
    }

    #[test]
    fn transport_forms_land_on_one_repo() {
        use super::parse_repo;
        let want = parse_repo("pallets/click", &[]).unwrap();
        for form in [
            "https://github.com/pallets/click.git",
            "git@github.com:pallets/click",
            "github.com/pallets/click",
            "  https://github.com/pallets/click/  ",
        ] {
            let got = parse_repo(form, &[]).unwrap();
            assert_eq!(got.host, want.host, "{form}");
            assert_eq!(got.slug(), want.slug(), "{form}");
        }
    }

    #[test]
    fn enterprise_github_is_known_from_gh_not_guessed() {
        use super::{parse_repo, Forge};
        let hosts = vec!["github.com".to_string(), "github.acme.io".to_string()];
        let r = parse_repo("github.acme.io/acme/widget", &hosts).unwrap();
        assert_eq!(r.forge, Forge::GithubEnterprise);
        assert_eq!(r.key(), "github.acme.io/acme/widget");
    }

    #[test]
    fn other_forges_route_to_their_own_api() {
        use super::{parse_repo, Forge};
        assert_eq!(
            parse_repo("gitlab.com/foo/bar", &[]).unwrap().forge,
            Forge::Gitlab
        );
        assert_eq!(
            parse_repo("codeberg.org/foo/bar", &[]).unwrap().forge,
            Forge::Gitea
        );
        assert_eq!(
            parse_repo("git.self-hosted.io/foo/bar", &[]).unwrap().forge,
            Forge::Unknown
        );
    }

    #[test]
    fn ssh_urls_keep_their_transport_for_the_clone() {
        use super::parse_repo;
        let r = parse_repo("ssh://git@gitlab.com/foo/bar.git", &[]).unwrap();
        assert_eq!(r.host, "gitlab.com");
        assert_eq!(r.url, "ssh://git@gitlab.com/foo/bar.git");
    }

    #[test]
    fn two_forges_same_owner_repo_never_share_a_key() {
        use super::parse_repo;
        let g = parse_repo("github.com/a/cli", &[]).unwrap();
        let l = parse_repo("gitlab.com/a/cli", &[]).unwrap();
        assert_eq!(g.name, l.name);
        assert_ne!(g.key(), l.key());
    }

    #[test]
    fn bad_references_are_rejected() {
        use super::parse_repo;
        assert!(parse_repo("", &[]).is_err());
        assert!(parse_repo("only-one-segment", &[]).is_err());
        assert!(parse_repo("../evil", &[]).is_err());
        assert!(parse_repo("owner/../repo", &[]).is_err());
        // host/repo is not a shape any forge has; do not silently make it github's.
        assert!(parse_repo("gitlab.com/onlyone", &[]).is_err());
    }

    #[test]
    fn gitlab_nested_subgroups_stay_one_repo() {
        use super::parse_repo;
        let r = parse_repo("gitlab.com/grp/sub/proj", &[]).unwrap();
        assert_eq!(r.host, "gitlab.com");
        assert_eq!(r.owner, "grp/sub");
        assert_eq!(r.name, "proj");
        assert_eq!(r.id_enc(), "grp%2Fsub%2Fproj");
    }

    #[test]
    fn origin_normalisation_unifies_ssh_and_https() {
        use super::norm_origin;
        assert_eq!(
            norm_origin("git@github.com:a/b.git"),
            norm_origin("https://github.com/a/b")
        );
        assert_eq!(
            norm_origin("https://github.com/a/b/"),
            norm_origin("https://github.com/a/b.git")
        );
        assert_ne!(
            norm_origin("https://github.com/a/b"),
            norm_origin("https://gitlab.com/a/b")
        );
    }
}
