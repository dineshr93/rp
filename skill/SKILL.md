---
name: repo-tiers
description: Repo tiers - the cheapest way to reach a git repo: remote query, shallow or sparse clone, local grep, text index, or code graph. Use when a repo is not on disk, before cloning, before indexing with graphicle or context-mode, when entering an unfamiliar repo, or when disk or context is tight.
---

# Repo tiers

Local disk is a **cache with a budget**, not a library. `rp` (`~/.local/bin/rp`) picks the tier, keeps the visit ledger, and holds the cache inside its budget. Measured costs and the traps sit in [COSTS.md](COSTS.md).

## Step 1 - ask before you reach

```
rp                       # in a local repo: prints the tier and the next command
rp get owner/repo        # repo not on disk: prints the remote commands, clones nothing
```

Done when the tier is named. Each call appends to `~/.pi/rp/ledger.tsv`, so a repo's second visit reads differently from its first. That count is the escalation signal — `rp` decides it, not your gut.

## Step 2 - act on the tier

| tier | the move |
|---|---|
| **R0** remote | `gh search code 'PATTERN' --repo owner/repo --limit 20` · `gh api repos/OWNER/REPO/contents/PATH --jq .content \| base64 -d` |
| **R1** clone | `rp clone owner/repo [subpath]` — the subpath is the lazy one: 2 MB against 184 MB |
| **T0** grep | `grep -rn 'PATTERN' .` · `rtk read FILE --max-lines 40` |
| **T1** text | `rp docs .` then `ctx_search(queries: [...])` |
| **T2** graph | `rp warm .` then `recall_project` / `query_graph` |

Climb one tier at a time, and only once the current tier has stopped answering. Every tier up buys seconds and hundreds of megabytes that a question the lower tier could settle never repays.

## Which question belongs to which tier

| question | tier |
|---|---|
| does this symbol exist, and where | R0 search, or T0 grep |
| one file's contents, repo still remote | R0 `gh api ... /contents/PATH` |
| who calls this function, what it calls | T2 graph |
| what breaks if I change this | T2 graph (`affected`) |
| why was it built this way | T2 graph, plus its captured sessions |
| prose and docs across one repo | T1 text |
| a repo you will touch twice | warm it: T1 or T2 |

## Step 3 - keep the cache inside its budget

```
rp cache     # per clone: size, last touch, dirty flag
rp evict     # LRU by last touch; a clone holding uncommitted work stays put
rp top 10    # most-visited repos = where warming pays
rp gc        # graphicle store report and the reclaim path
rp doctor    # toolchain, budgets and install health; exit 1 on a hard miss
```

Budgets read from the environment: `RP_CACHE_GB` (150), `RP_FREE_FLOOR_GB` (100), `RP_BUDGET_MB` (4096). `rp clone` checks free space and evicts before it downloads anything.

Done when `rp cache` reports the cache under budget, `rp doctor` reports no hard miss, and the question was answered at the lowest tier that could answer it.
