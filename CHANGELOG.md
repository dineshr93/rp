# Changelog

All notable changes to `rp` are documented here.

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
