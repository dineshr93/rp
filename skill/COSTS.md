# Measured costs and traps

Reference for [SKILL.md](SKILL.md). Every number below was measured on the box this tool was written on (AMD, warm filesystem, 450 GB free of 3.6 TB, 88% used). Re-measure before trusting them on other hardware; the ordering holds even when the absolutes move.

## The ladder

| operation | latency | disk | notes |
|---|---|---|---|
| `gh search code` scoped to one repo | 0.4-0.8s | 0 MB | 30 requests/min cap |
| `gh api .../contents/PATH` | 0.38s | 0 MB | one file, no clone |
| `grep -rn` over 12k files | 0.02s | 0 MB | warm filesystem |
| `rtk read FILE --max-lines 40` | <0.1s | 0 MB | ~300-450 tokens to orient |
| `rp clone owner/repo src/sub` (sparse) | ~14s | 2-49 MB | 27-63 files |
| `rp clone owner/repo` (whole tree) | 20.2s | 184 MB | 7,801 files (vllm) |
| `rp docs` (context-mode, 746 files) | 9.6s | +153 MB | ~205 KB per file |
| `rp warm` 362 files | 0.8s | +9 MB | 279 nodes, 289 edges |
| `rp warm` 12k files | 18.7s | +746 MB | 42,751 nodes, 107,948 edges |

## Forge routing (measured 2026-10, live calls)

Every number below is the command `rp get` actually emitted, run against the real
service. Nothing here is inferred from documentation.

| forge | call | latency | disk | token |
|---|---|---|---|---|
| gitea/forgejo (codeberg) | `git ls-remote --symref <url> HEAD` | 0.75s | 0 MB | none |
| gitlab.com | `git ls-remote --symref <url> HEAD` | 0.67s | 0 MB | none |
| gitlab.com | `curl …/api/v4/projects/<id>/repository/tree` | 0.36s | 0 MB | none (public) |
| gitlab.com | `curl …/repository/files/README.md/raw?ref=HEAD` | 0.28s | 0 MB | none (public) |
| gitlab.com | `curl …/search?scope=blobs` | 0.24s | — | **401 without one** |
| gitea (codeberg) | `curl …/api/v1/repos/<slug>/git/trees/HEAD` | 0.22s | 0 MB | none (public) |
| gitea (codeberg) | `curl …/raw/HEAD/README.md` | 0.17s | 0 MB | none (public) |
| gitea (codeberg) | `curl …/api/v1/search/code?q=repo:…` | 0.10s | — | 404 — indexer off site-wide |
| github.com | `gh search code` / `gh api …/contents/PATH` | 0.4-0.8s | 0 MB | `gh auth` |
| github enterprise | `GH_HOST=<h> gh …` | same | 0 MB | one `gh auth login --hostname` per host |

Traps, each one measured before it was believed:

- **An empty token header is worse than no header.** `curl -H "PRIVATE-TOKEN: "`
  against a public GitLab project returns a 302 to `users/sign_in`; the same URL
  with the header omitted returns the JSON. So the emitted command carries
  `${GITLAB_TOKEN:+-H "PRIVATE-TOKEN: $GITLAB_TOKEN"}`, which expands to nothing
  when the variable is unset. A literal header with an empty variable silently turns
  every public repo into a login wall.
- **`git archive --remote` is absent on GitHub.** Present on GitLab and Gitea, so
  the unknown-forge floor offers it and github must use `gh api …/contents`.
- **`ls-remote` on a repo with a huge ref history stalls.** `gitlab-org/gitlab`
  did not answer inside a 20s timeout; `gitlab-org/cli` answered in 1.03s. Always
  pass `HEAD` explicitly — a bare `ls-remote` there is a multi-second stall on a
  repo with hundreds of thousands of refs.
- **Gitea code search depends on the server.** Codeberg returns 404 for
  `/api/v1/search/code` because the repo indexer is not enabled. A 404 is not "no
  results", it is "this forge cannot search" — drop to the git floor.
- **Enterprise GitHub needs no new code.** `gh` resolves `/api/v3` and finds the
  right keyring entry from `GH_HOST` on its own; rp only prepends the variable.
- **Cache collisions are real.** `github.com/a/cli` and `gitlab.com/a/cli` want the
  same bare directory. `rp` keeps the bare name when its recorded origin matches
  and falls back to `host-owner-repo` when it does not, so pre-existing clones are
  still found and two forges never fight over one checkout.

## Clone facts

- **`--filter=blob:none` buys nothing on a working-tree checkout.** vllm blobless 185 MB against shallow 184 MB. Blob filters trim history, and `--depth 1` already dropped the history. Only `--sparse` trims working-tree bytes.
- `--sparse` alone leaves 17 files present. Pass the subpath to `rp clone`, or widen later with `git -C DIR sparse-checkout add DIR`.
- Full clone of vllm: 39.7s / 457 MB. Shallow: 20.2s / 184 MB. A full clone is for work that needs history; reading code is not that.
- linux at `--depth 1 --filter=blob:none`: 2.1 GB, 95,978 files. One kernel checkout is a third of a day's clone budget at 1000 repos/day.

## Registration truth

- **Bare `graphicle status` reports the wrong repo.** With no path it returns the last-synced project, not the working directory's. Run it in `~/repos/AMD` and it answers with `wezterm`. At many repos per day that is a wrong-repo answer, which is worse than no answer.
- `rp` asks with `graphicle context probe --project <path>` and reads the exit code: 0 registered, 1 not. Exact, and it cannot drift to another repo.
- `graphicle mcp --project .` refuses to start on an unregistered repo (`No project is registered ... sync it first`), so a cold repo simply has no graphicle tools. Work the lower tiers, or `rp warm` and restart the agent.

## context-mode facts

- `ctx_index` defaults to `maxFiles: 200`, which silently under-indexes any real repo. `rp docs` raises the cap to the file count, ceiling 3000.
- Search is **per-project**: an index built in repo A stays invisible to a session running in repo B. Index and search in the same repo. Verified by indexing one repo and searching from another: no results.
- The index costs roughly the content bytes it stores (~205 KB per file measured).

## graphicle store growth

- No per-project delete exists. `delete_repository`, `forget_repository`, `purge_project`, `retention_apply` all return `Unsupported native project operation`. Retention runs internally at sync time and is not exposed.
- So the store only grows: ~25-62 KB per file. Prevention is the only lever, which is why `rp` warms on repeat visits.
- Reclaim path: `graphicle backup && mv ~/.local/share/graphicle ~/.local/share/graphicle.old`, then `rp warm` the repos still wanted.

## 1000 repos per day

| if you | disk per day | verdict |
|---|---|---|
| clone every repo whole-tree (~180 MB) | ~180 GB | dead in 2.5 days |
| clone sparse to subpaths (~5 MB) | ~5 GB | fine |
| answer remote, clone on repeat work | near zero | the plan |

Latency never was the constraint: 1000 shallow clones run in about 5.5 hours sequential, minutes in parallel. Bytes are the constraint.

## Source, install, portability

The crate owns the docs it describes — the skill lives inside the project, so one directory carries the whole routing system:

```
rp/                      Rust, zero crates (subprocess + arithmetic)
├── Cargo.toml
├── src/main.rs          the router
└── skill/
    ├── SKILL.md         steps + tier→command map (source of truth)
    └── COSTS.md         this file
```

```
cargo build --release && ./target/release/rp install   # binary + skill + AGENTS.md pointer
cargo test                                            # unit + hermetic CLI: parsing, routing, budgets
rp doctor                                             # health of toolchain, forges, budgets, install
```

`rp install` is idempotent: it reports `installed` / `unchanged` per file, replaces only its own `<!-- repo-tiers:start -->` … `<!-- repo-tiers:end -->` block, and leaves every other line of `AGENTS.md` alone. Delete that block to opt out. Run from a build tree it also copies the binary into `~/.local/bin/rp`; run from an installed path it skips that step, so `cargo install --path .` followed by `rp install` works too.

Fresh box, from a clone of this directory:

```
cargo build --release && ./target/release/rp install && rp doctor
```

Everything is env-overridable, so nothing is hardcoded to this machine:

| variable | default | meaning |
|---|---|---|
| `RP_LEDGER` | `~/.pi/rp/ledger.tsv` | visit log — the escalation signal |
| `RP_CACHE` | `~/cache/repos` | clone cache root |
| `RP_CACHE_GB` | 150 | cache cap |
| `RP_FREE_FLOOR_GB` | 100 | never clone below this free space |
| `RP_BUDGET_MB` | 4096 | graphicle store cap |
| `RP_SRC` | `~/tools/rp` | project dir, read by `rp install` |
| `RP_SKILLS` | `~/.agents/skills/repo-tiers` | skill install target |
| `RP_AGENT_MD` | `~/.pi/agent/AGENTS.md` | pointer target |
| `RP_BIN` | `~/.local/bin/rp` | binary install target |
| `GRAPHICLE_DATA_DIR` | `~/.local/share/graphicle` | graphicle store |
| `CTX_MODE_CLI` | `~/.pi/agent/npm/node_modules/context-mode/cli.bundle.mjs` | context-mode CLI |
| `GH_HOST` | `github.com` | default host for a bare `owner/repo`; where `gh` looks for credentials |
| `GITLAB_TOKEN` | unset | GitLab PAT, scope `read_api` — unlocks blob search |
| `GITEA_TOKEN` / `FORGEJO_TOKEN` | unset | Gitea/Forgejo access token — unlocks code search |

## Dependencies per tier

| tier | needs | unlock |
|---|---|---|
| T0 | `git` | always available |
| R0 (floor) | `git` | `ls-remote` answers **every** forge, token-free |
| R0 (github + GHE search) | `gh` + `gh auth login` (`repo` scope); `gh auth login --hostname <h>` per enterprise host | code search on GitHub-family hosts |
| R0 (gitlab search) | `curl` + `$GITLAB_TOKEN` (scope `read_api`) | blob search; tree/raw work without it |
| R0 (gitea/forgejo search) | `curl` + `$GITEA_TOKEN`, server indexer on | code search; tree/raw work without it |
| R1 | `git` | clone / sparse |
| T1 | node ≥ 18 + `context-mode` | text index |
| T2 | `graphicle` | code graph |

`rp doctor` prints `tiers live: ...` and what is missing for the rest; `rp` will not
route into a tier it cannot serve. Full install commands are in the project README.

## Installing the rule for other agents

The skill lives once in the shared library `~/.agents/skills/repo-tiers`. `rp
install --agents all` symlinks that copy into each agent's own skills dir (pi,
Claude Code, Codex CLI, Gemini CLI, GitHub Copilot CLI, Cursor, OpenCode, Hermes)
and writes the pointer into that agent's global memory file. `rp agents` (or
`--json`) is the live list of paths and their state.

Never clobbered: your own prose survives install and uninstall; a Cursor rule rp
did not write is `skipped (foreign rule file)`; a link pointing elsewhere is
`link points elsewhere`; a real directory at the link path is
`foreign skill dir`. All left as found.
