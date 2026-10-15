# Changelog

All notable changes to `rp` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] — 2026-10-16

First production release. The tier model, the budget, and the install contract are
considered stable.

### Added
- **Tier routing** — `rp` reports the cheapest tier for reaching a repo and records
  the visit: `R0` remote query, `R1` shallow/sparse clone, `T0` local grep,
  `T1` context-mode text index, `T2` graphicle code graph.
- **Visit ledger as the escalation signal** — a first-touch repo is told *do not
  index*; the second visit flips it. The count decides, not the caller's judgement.
- **Budget enforcement** — `RP_CACHE_GB` cap and `RP_FREE_FLOOR_GB` free-space
  floor; `rp clone` checks free space and evicts before it downloads.
- **LRU eviction with a dirty guard** — a clone holding uncommitted work is never
  removed.
- **`rp install` / `rp uninstall`** — idempotent bootstrap of binary, agent skill
  and a marked `AGENTS.md` pointer block. Uninstall removes only its own block and
  refuses a foreign skill directory.
- **`rp doctor`** — 13 checks over toolchain, budgets and install state, each with
  the fix on the next line. Exits 1 on a hard miss.
- **`--json`** — machine-readable output for `state`, `get`, `top`, `cache`,
  `doctor`, so an agent can branch on `tier` instead of parsing prose.
- **`--quiet`** — suppresses progress chatter; errors still reach stderr.
- **Input validation** — `owner/repo` slugs and sparse subpaths are validated, so
  a malformed argument cannot write outside the clone cache.
- **Error visibility** — swallowed I/O failures now count against the process and
  exit non-zero; a printed line is no longer proof of success.
- **`--version`**, documented exit codes (0 / 1 / 2), MIT license.
- **Integration suite** — 21 tests that stub `git`, `gh`, `graphicle` and `node`
  in a temp `PATH` and redirect every path rp touches, so the suite needs no
  credentials and no network. 26 tests total with the unit tests.
- **CI** — fmt, clippy `-D warnings`, build and test on Linux and macOS, an
  install/uninstall smoke test, and a release binary artifact on tags.

### Fixed
- Date arithmetic in the ledger formatter used `era * 365` instead of `era * 400`,
  printing 1851 for 2026 timestamps.
- `gc` listed only ledger-known repos and hid projects synced before `rp` existed;
  it now reads the graphicle store directly.
- `top` labelled deleted clones as `cold`; they now read `gone`.
- `rp | head` aborted on a closed stdout pipe; handled without a libc dependency.
- JSON arrays double-escaped pre-built objects.

### Known limits
- No per-project delete exists in the graphicle CLI, so the graph store only grows.
  `rp gc` documents the reclaim path; prevention (warm on repeat visits) is the
  real lever.
- `R0` assumes GitHub. Another forge needs its own remote tier.

[1.0.0]: https://github.com/dineshr93/rp/releases/tag/v1.0.0
