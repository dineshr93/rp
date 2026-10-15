# Changelog

All notable changes to `rp` are documented here.

## [1.3.0] - 2026-10-15

### Added

- **Every forge, not just github.com.** `rp get` and `rp clone` now route on the
  host in the reference: `owner/repo` (github.com, or `GH_HOST`'s enterprise
  GitHub), `host/owner/repo`, `https://`, `ssh://`, `git://` and scp-style
  `git@host:o/r`. GitLab nested subgroups (`gitlab.com/grp/sub/proj`) parse into
  one repo with a multi-segment owner and are `%2F`-encoded for the `:id` param.
  Enterprise GitHub costs no new API code — `gh` resolves `/api/v3` and the
  keyring itself from `GH_HOST`, so it is a prefix on the same command.
- **A token-free floor under R0.** `git ls-remote --symref <url> HEAD` answers any
  git host for 0 MB with no API, so R0 no longer dies when `gh` is missing and an
  unknown host is still reachable. `rp get` prints it as the `floor:` line on every
  forge. Measured: 0.75s Forgejo, 0.67s GitLab.
- **Per-forge R0 commands.** GitHub (`gh`), GitHub Enterprise (`GH_HOST=… gh`),
  GitLab REST v4 (`repository/tree`, `repository/files/…/raw`, `search?scope=blobs`)
  and Gitea/Forgejo REST v1 (`/repos/…/git/trees`, `/raw/HEAD/PATH`,
  `/search/code`). Verified against the live services: tree and raw files return
  anonymously on both GitLab and Codeberg; blob search is 401 without a token.
- **GitLab's empty-header trap is closed.** `curl -H "PRIVATE-TOKEN: "` against a
  public project returns a 302 to `users/sign_in`; the same URL with the header
  omitted returns JSON. Emitted commands use
  `${GITLAB_TOKEN:+-H "PRIVATE-TOKEN: $GITLAB_TOKEN"}` so the header disappears
  when the variable is unset.
- **Host-qualified ledger keys and cache dirs.** `gitlab.com/a/cli` and
  `github.com/a/cli` no longer share a visit count or a checkout. `cache_dir_for`
  keeps the bare repo name when its recorded `remote.origin.url` matches, so clones
  made before this change are still found, and falls back to `host-owner-repo`
  otherwise.
- `rp doctor` rows: `curl`, `glab cli`, `$GITLAB_TOKEN`, `$GITEA_TOKEN`,
  `gh hosts` (every GitHub host `gh` is authed against, enterprise included) and
  `R0 forge search`, a per-forge live/dead summary.

### Changed

- `rp doctor` no longer calls R0 dead without `gh`. The git floor means R0 is live
  whenever `git` is; `gh`, `$GITLAB_TOKEN` and `$GITEA_TOKEN` only buy code
  *search* on top, and that is what the new rows report.
- A bare `host/repo` (two segments, dotted first segment) is now a hard error
  instead of silently becoming `github.com/host/repo`.
- GitHub-family hosts reject anything but exactly `owner/repo`; GitLab and Gitea
  may nest. `rp clone a/b/c` is refused again rather than pointed at a github.com
  path that cannot exist.
- `rp clone` uses the URL it was given instead of rebuilding
  `https://github.com/<slug>`. An `ssh://` or `git@host:` reference now clones
  over ssh with the user's keys; a GitLab reference no longer clones github.com.

### Changed

- `rp clone owner/repo <subpath>` now passes `--filter=blob:none`. The sparse path
  used to download every blob in the tip commit and then check out a fraction of it,
  so 7 MB of the gitdir was bytes the cone never touches. Measured on
  `morluto/rea` cone `src`: 33 MB -> 25 MB (gitdir 13 MB -> 6 MB), 4.2s.
  Committing back is unaffected: the cone's base blobs arrive at checkout, so
  `git diff` and `git commit` still work with the remote unreachable, and
  `sparse-checkout add <dir>` lazy-fetches only that dir. Cost is history reads
  outside the cone (`log -p`, `blame`, `diff <old-sha>`) now need the network.
  The whole-tree branch is unchanged — a full checkout needs every blob anyway.

## [1.2.1] - 2026-10-15

### Fixed

- The bundled skill's `description` frontmatter is quoted. An unquoted `": "` in
  the value made YAML read it as a nested mapping, so agent harnesses refused to
  load `repo-tiers` at all — pi showed "Nested mappings are not allowed in compact
  mappings" and the skill stayed invisible until the file was patched by hand.
- CI parses `skill/SKILL.md` frontmatter in the lint job, so the next unquoted
  colon on that line fails the build instead of the TUI.

## [1.2.0] - 2026-10-15

### Added

- Skill install is now a library + symlink model. The skill lives once in the shared
  library at `RP_SKILLS` (`~/.agents/skills/repo-tiers`), and `rp install --agents`
  symlinks that copy into each agent's own skills dir — `~/.claude/skills/repo-tiers`,
  `~/.cursor/skills/repo-tiers`, `~/.hermes/skills/repo-tiers` and the rest — the
  same wiring every other skill on a shared-skills box already uses.
- `rp agents` reports the skill link state per agent; `--json` adds `skill_link`
  (`linked` / `elsewhere` / `foreign` / `none`) and `skill_link_path`.

### Fixed

- `rp install --agents` only wrote memory-file pointers, so no agent's skill loader
  could actually discover `repo-tiers` — the rule was readable but never fired as a
  skill. Each agent now gets both: the link for discovery, the pointer for always-on
  routing.
- Links are never clobbered. An existing link pointing at another directory reports
  `link points elsewhere`; a real directory sitting at the link path reports
  `foreign skill dir`; both are left byte for byte, pinned by
  `a_link_pointing_elsewhere_is_left_alone` and `a_foreign_skill_dir_is_never_clobbered`.

## [1.1.0] - 2026-10-15

### Added

- `rp install --agents <a,b|all>` writes the tier pointer into each supported
  agent's global memory file, so the rule is in force whichever agent opens the
  next repo: pi, Claude Code, Codex CLI, Gemini CLI, GitHub Copilot CLI, Cursor
  (written as an `alwaysApply` `.mdc` rule), OpenCode, Hermes.
- `rp install --file <path>` for any other memory file; repeatable.
- `rp agents` lists every target path with whether the file exists and whether rp's
  pointer is in it; `rp agents --json` returns `{agent, memory_file, exists,
  rp_installed}` per agent for scripts.
- `rp doctor` reports `tiers live: R0 R1 T0 T1 T2` and, per missing tier, what
  would unlock it. `--json` exposes `tiers` / `tiers_missing`.
- README: **Dependencies** (per-tier install and verification commands) and
  **Agent integrations** (the verified path map). Skill `COSTS.md` carries the
  short version.

### Fixed

- `uninstall` resolved agent memory files from the real `$HOME` even when the rest
  of the configuration was overridden, so the test suite swept the author's live
  `~/.pi/agent/AGENTS.md` and removed its pointer block. Tests now redirect `HOME`
  into the sandbox, and the new `uninstall_sweeps_every_agent_it_wrote` test pins
  the behaviour: rp's blocks go, foreign prose and foreign Cursor rules stay.

## [1.0.0] - 2026-10-15

### Added

- Rust rewrite of the whole tool: zero crate dependencies, `#![deny(unsafe_code)]`,
  `#![deny(clippy::all)]`, single 529 KB static binary.
- Tier routing for reaching a git repo at the cheapest available cost:
  `rp` (detect), `rp warm` (index), `rp docs` (graph), `rp top [N]`, `rp gc`,
  `rp get owner/repo`, `rp clone owner/repo [subpath]`, `rp cache`, `rp evict`.
- `rp install` / `rp uninstall` — idempotent bootstrap of binary, skill and the
  marked `AGENTS.md` pointer block; uninstall refuses to remove a foreign skill dir.
- `rp doctor` — toolchain, budget and install health with per-line fixes.
- `--json` on `state`, `get`, `top`, `cache`, `doctor`; `--quiet` for scripts;
  `--version`.
- Input validation: repo slugs reject traversal and empty segments, subpaths reject
  absolute paths, `..` and NUL bytes.
- Error visibility: swallowed I/O failures now exit non-zero instead of reporting
  success.
- 26 tests (unit + hermetic integration with stubbed `git`/`gh`/`graphicle`/`node`),
  CI for fmt, clippy, build and test on Linux and macOS, install/uninstall smoke
  test, and release artifacts on version tags.
- MIT license, this changelog, rewritten README.

### Measured

Costs behind the routing rules are recorded in `skill/COSTS.md`, measured on a
2025 Ryzen AI 9 HX 370 with an NVMe drive: grep 0.02 s / 0 MB, context-mode index
9.6 s / +153 MB per 746 files, graphicle sync 0.8 s / +9 MB at 362 files rising
to 18.7 s / +746 MB at 12,191 files, and a full clone of a 12k-file repo at
181 MB against 1.5 MB for the same repo sparse-checked-out to one subpath.
