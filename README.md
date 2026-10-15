# rp — repo tier router

**Reach a git repo the cheapest way possible, and keep local disk a cache with a byte budget instead of a library.**

`rp` answers one question before you touch a repo: *what does it cost to get at this code?* Then it hands you the command for the cheapest tier that can answer, and records the visit so a repeat repo reads differently from a one-touch repo.

Measured on a 7,800-file repo:

| tier | how | latency | disk |
|---|---|---|---|
| **R0** remote | `gh search code` / `gh api contents` | 0.4–0.8s | **0 MB** |
| **R1** clone | `rp clone owner/repo src/sub` (sparse) | ~14s | 2–49 MB |
| **R1** clone | `rp clone owner/repo` (whole tree) | 20s | 184 MB |
| **T0** grep | `grep -rn` over 12k files | 0.02s | 0 MB |
| **T1** text | `rp docs` → context-mode FTS5 | 9.6s | +153 MB |
| **T2** graph | `rp warm` → graphicle code graph | 0.8–18.7s | +9 MB → +746 MB |

At 1000 repos a day, whole-tree clones are 180 GB/day — dead in two and a half days on a 450 GB disk. Answer remotely first, clone on repeat work, let `rp` evict.

## Every forge, not just github.com

`rp` routes on the **host in the reference**. A bare `owner/repo` is github.com —
or your enterprise GitHub when `GH_HOST` is set, exactly as `gh` reads it. Anything
that arrives with a host keeps that host, so GitLab (including nested subgroups),
Gitea/Forgejo and GitHub Enterprise route through the same ladder instead of being
silently rewritten into a github.com clone.

| reference | forge | R0 code search | R0 with no token at all |
|---|---|---|---|
| `pallets/click` | github | `gh search code` | `git ls-remote` |
| `GH_HOST=github.acme.io` + `acme/widget` | github enterprise | `GH_HOST=github.acme.io gh search code` | `git ls-remote` |
| `gitlab.com/grp/sub/proj` | gitlab | REST v4 `scope=blobs` (needs `$GITLAB_TOKEN`) | tree + raw file, anonymous |
| `codeberg.org/forgejo/forgejo` | gitea / forgejo | REST v1 (needs `$GITEA_TOKEN` **and** a server indexer) | tree + raw file, anonymous |
| `git.self-hosted.io/a/b` | unknown | none | `ls-remote`, blobless tree, `git archive` |

Measured 2026-10 against the real services, using the exact commands `rp get` emits:

| call | latency | disk |
|---|---|---|
| `git ls-remote --symref <forgejo url> HEAD` | 0.75s | 0 MB |
| `git ls-remote --symref <gitlab url> HEAD` | 0.67s | 0 MB |
| `curl …/api/v4/projects/<id>/repository/tree` (anon) | 0.36s | 0 MB |
| `curl …/api/v1/repos/<slug>/raw/HEAD/README.md` (anon) | 0.17s | 0 MB |
| `curl …/api/v4/projects/<id>/search?scope=blobs` (anon) | 0.24s | 401 — token required |

**The git floor is why R0 never dies.** `git ls-remote` answers any git host for
zero bytes with no API and no credential beyond what git already has, so a forge rp
has never heard of is still reachable. What a forge API buys on top is *code
search* — nothing more.

Enterprise GitHub costs rp no new code: `gh` resolves `/api/v3` and the keyring
itself from `GH_HOST`, so the whole thing is a prefix on the same command.

[![ci](https://github.com/dineshr93/rp/actions/workflows/ci.yml/badge.svg)](https://github.com/dineshr93/rp/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![zero dependencies](https://img.shields.io/badge/dependencies-zero-brightgreen)](Cargo.toml)

## Install

```sh
git clone https://github.com/dineshr93/rp && cd rp
cargo build --release && ./target/release/rp install && rp doctor
```

`rp install` puts the binary in `~/.local/bin/rp`, the skill in the shared library
`~/.agents/skills/repo-tiers/`, and a marked pointer block in
`~/.pi/agent/AGENTS.md`. It is idempotent and only ever touches its own block, so
hand-edited notes survive. `rp uninstall` reverses it.

## Dependencies

`rp` itself needs a shell, `git` and coreutils. Everything else buys a tier:

| tier | needs | install | check |
|---|---|---|---|
| **T0** grep, ledger, budget | `git` | any package manager | `rp state` |
| **R0** remote, git floor | `git` | as above — `ls-remote` answers every forge | `rp get <repo>` → `floor:` line |
| **R0** github + GHE search | `gh`, authenticated | `apt install gh` / `brew install gh`, then `gh auth login` (`repo` scope); one login per enterprise host: `gh auth login --hostname github.acme.io` | `rp doctor` → `gh hosts` |
| **R0** gitlab search | `curl` + `$GITLAB_TOKEN` | PAT with scope `read_api` | `rp doctor` → `$GITLAB_TOKEN` |
| **R0** gitea/forgejo search | `curl` + `$GITEA_TOKEN` | access token; the server must have its repo indexer on | `rp doctor` → `$GITEA_TOKEN` |
| **R1** clone / sparse | `git` | as above | `rp clone owner/repo` |
| **T1** text index | Node ≥ 18 + context-mode | `npm install -g context-mode` (pi: `pi install npm:context-mode`) | `rp warm <repo>` |
| **T2** code graph | graphicle | `curl -fsSL https://github.com/graphicle-ai/graphicle/releases/latest/download/install.sh \| sh`, then `graphicle doctor --fix` | `rp doctor` → `graphicle ok` |
| build only | stable Rust, 2024 edition | `curl https://sh.rustup.rs -sh` | `cargo build --release` |

A missing dependency is never fatal: `rp doctor` prints which tiers are live and
what would unlock the rest, and `rp` will not route into a tier it cannot serve.
`rp doctor --json` carries the same as `tiers` / `tiers_missing`.

`python3` is optional and only used to read the graphicle store for size reporting;
without it `rp cache` still works, the store line just reads `?`.

Environment overrides: `RP_LEDGER`, `RP_CACHE`, `RP_CACHE_GB`, `RP_FREE_FLOOR_GB`,
`RP_BUDGET_MB`, `RP_SRC`, `RP_SKILLS`, `RP_AGENT_MD`, `RP_BIN`,
`GRAPHICLE_DATA_DIR` (graphicle store), `CTX_MODE_CLI` (context-mode binary).

Forge routing reads the same variables the forges' own CLIs read: `GH_HOST` (the
default host for a bare `owner/repo`, and where `gh` looks for credentials),
`GITLAB_TOKEN`, and `GITEA_TOKEN` / `FORGEJO_TOKEN`. `rp doctor` reports which of
them are live.

## Agent integrations

The skill lives **once**, in the shared library `~/.agents/skills/repo-tiers`.
`rp install --agents` symlinks that one copy into each agent's own skills dir —
the same wiring every other skill on this box already uses — so that agent's skill
loader discovers it, and writes the pointer into that agent's global memory file so
the tier rule stays on without anyone invoking it. One source, no copies to drift.

| agent | memory file | skill symlinked into |
|---|---|---|
| pi | `~/.pi/agent/AGENTS.md` | `~/.pi/agent/skills/repo-tiers` |
| Claude Code | `~/.claude/CLAUDE.md` | `~/.claude/skills/repo-tiers` |
| Codex CLI | `~/.codex/AGENTS.md` | `~/.codex/skills/repo-tiers` |
| Gemini CLI | `~/.gemini/GEMINI.md` | `~/.gemini/skills/repo-tiers` |
| GitHub Copilot CLI | `~/.copilot/instructions.md` | `~/.copilot/skills/repo-tiers` |
| Cursor | `~/.cursor/rules/repo-tiers.mdc` (an `alwaysApply` rule) | `~/.cursor/skills/repo-tiers` |
| OpenCode | `~/.config/opencode/AGENTS.md` | `~/.config/opencode/skills/repo-tiers` |
| Hermes | `~/.hermes/AGENTS.md` | `~/.hermes/skills/repo-tiers` |

```bash
rp agents                               # what exists, and which files already carry the pointer
rp install --agents all                 # write every one of them
rp install --agents pi,claude,hermes    # or a subset
rp install --file ~/team/MEMORY.md      # plus any other memory file (repeatable)
rp uninstall                            # sweeps every file it wrote
```

Nothing you did not ask for gets touched. Plain memory files get the block between
`<!-- repo-tiers:start -->` and `<!-- repo-tiers:end -->`; your own prose survives
install and uninstall either way. A Cursor rule rp did not write is reported
`skipped (foreign rule file)` and left byte for byte. A skill link pointing at some
other directory reports `link points elsewhere`, and a real directory sitting at
that path reports `foreign skill dir` — neither is ever overwritten. `rp agents
--json` returns `{agent, memory_file, exists, rp_installed, skill_link,
skill_link_path}` per agent for scripts.

Agents with no skill loader of their own (Copilot CLI, Gemini CLI) still get the
pointer, and it names the shared library by path — they read `SKILL.md` and
`COSTS.md` from there the same way pi does.

## Use

```sh
rp                      # tier of the repo I am standing in, plus the next command
rp get owner/repo       # repo not on disk: remote commands, never a clone
rp get gitlab.com/grp/sub/proj     # any forge: the host routes it
rp get codeberg.org/forgejo/forgejo
rp clone owner/repo src/sub
rp warm .               # graphicle code graph: callers, impact, why
rp docs .               # context-mode text index
rp top 10               # most-visited repos = where warming pays
rp cache                # clone cache: size, last touch, dirty flag
rp evict                # LRU; a clone with uncommitted work is never removed
rp gc                   # graphicle store report and the reclaim path
rp doctor               # toolchain, budget and install health
```

```
$ rp
repo: aosp2sbom  visits=8  tier: T2 graph ready
  recall_project / query_graph (MCP) — callers, impact, why
  store: 932 MB / 4096 MB budget

$ rp            # a repo touched for the first time
repo: newthing  visits=1  tier: T0 cold — DO NOT INDEX
  grep -rn 'PATTERN' .   |   rtk read FILE --max-lines 40
  note: indexing costs ~1-10s and 9-380 MB; a one-touch repo never earns it back
```

The visit count is the escalation signal. One touch says *do not index*; the
second visit flips it to *indexing now pays*. That is a ledger lookup, not a
judgement call, which is what keeps 1000 repos/day off the disk.

## Flags

| flag | effect |
|---|---|
| `-j`, `--json` | machine-readable output for `state`, `get`, `top`, `cache`, `doctor` |
| `-q`, `--quiet` | suppress progress chatter; errors still print to stderr |
| `-V`, `--version` | print version |
| `-h`, `--help` | usage |
| `--all` | with `uninstall`, also remove the binary |

```json
$ rp --json state
{"repo":"aosp2sbom","path":"/home/dinesh/repos/aosp2sbom","visits":8,"tier":"T2",
 "tier_label":"graph ready","note":"","next":["recall_project / query_graph (MCP) — callers, impact, why"],
 "store_mb":932,"store_budget_mb":4096,"over_budget":false}
```

## Exit codes

| code | meaning |
|---|---|
| 0 | success |
| 1 | failed or degraded — a step reported an I/O or tool failure, read stderr |
| 2 | bad usage or bad input (unknown command, invalid slug, missing path) |

A swallowed I/O failure exits 1 rather than printing its way to success.

## Configuration

Everything is an environment variable; nothing is hardcoded to a machine.

| variable | default | meaning |
|---|---|---|
| `RP_LEDGER` | `~/.pi/rp/ledger.tsv` | visit log — the escalation signal |
| `RP_CACHE` | `~/cache/repos` | clone cache root |
| `RP_CACHE_GB` | `150` | clone cache cap |
| `RP_FREE_FLOOR_GB` | `100` | never clone below this much free space |
| `RP_BUDGET_MB` | `4096` | graphicle store cap |
| `RP_SRC` | `~/tools/rp` | project directory, read by `rp install` |
| `RP_SKILLS` | `~/.agents/skills/repo-tiers` | skill install target |
| `RP_AGENT_MD` | `~/.pi/agent/AGENTS.md` | pointer target |
| `RP_BIN` | `~/.local/bin/rp` | binary install target |
| `GRAPHICLE_DATA_DIR` | `~/.local/share/graphicle` | graphicle store |
| `CTX_MODE_CLI` | `~/.pi/agent/npm/node_modules/context-mode/cli.bundle.mjs` | context-mode CLI |

## Design notes

- **Zero crates.** Every capability is a subprocess (`git`, `gh`, `graphicle`,
  `du`, `df`, `node`, `python3`) plus arithmetic. The date formatter, the JSON
  writer, the FNV-1a path hash and the LRU policy are all hand-rolled for exactly
  this reason. `#![deny(unsafe_code)]`.
- **Registration is checked by exit code**, never by `graphicle status`, which with
  no path returns the last-synced project rather than the current one — a wrong-repo
  answer, worse than none.
- **Sparse before whole.** `--filter=blob:none` saves nothing on a working-tree
  checkout; only `--sparse` cuts bytes. Measured, so nobody re-derives it.
- **The skill lives in the crate** (`skill/`), so one directory carries the tool,
  its agent instructions and its measured evidence.

## Test

```sh
cargo test            # 26 tests: unit + hermetic integration
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

The integration suite stubs `git`, `gh`, `graphicle` and `node` in a temp `PATH`
and redirects every path rp touches, so it needs no credentials, no network and
no real indexes.

## Layout

```
src/main.rs      the router
skill/SKILL.md   agent-facing steps and the tier→command map
skill/COSTS.md   measured costs, traps, env reference
tests/cli.rs     hermetic CLI integration tests
```

## License

MIT — see [LICENSE](LICENSE).
