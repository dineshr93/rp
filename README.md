# rp — repo tier router

Reach a git repo the cheapest way possible: remote query → shallow/sparse clone → local grep → text index → code graph. Local disk stays a cache with a byte budget, never a library.

```sh
cargo build --release && ./target/release/rp install && rp doctor
```

`rp install` puts the binary in `~/.local/bin/rp`, the agent skill in `~/.agents/skills/repo-tiers/`, and a marked pointer block in `~/.pi/agent/AGENTS.md`. Idempotent; it only touches its own block.

Zero crates. Everything is a subprocess (`git`, `gh`, `graphicle`, `du`, `df`) plus arithmetic.

- Usage, tiers, completion criteria: [`skill/SKILL.md`](skill/SKILL.md)
- Measured costs, traps, env overrides: [`skill/COSTS.md`](skill/COSTS.md)

```sh
rp                      # tier of the repo I am standing in
rp get owner/repo       # answer remotely, never clone
rp clone owner/repo src/sub
rp doctor               # toolchain, budgets, install health
cargo test
```
