---
name: repo-tiers
description: "Repo tiers - the cheapest way to reach a git repo: remote query, shallow or sparse clone, local grep, text index, or code graph. Use when a repo is not on disk, before cloning, before indexing with graphicle or context-mode, when entering an unfamiliar repo, or when disk or context is tight."
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
| **R0** remote | `rp get <host/>owner/repo` prints the right commands for that forge. Universal floor: `git ls-remote --symref <url> HEAD` |
| **R1** clone | `rp clone owner/repo [subpath]` — the subpath is the lazy one: 2 MB against 184 MB |
| **T0** grep | `grep -rn 'PATTERN' .` · `rtk read FILE --max-lines 40` |
| **T1** text | `rp docs .` then `ctx_search(queries: [...])` |
| **T2** graph | `rp warm .` then `recall_project` / `query_graph` |

Climb one tier at a time, and only once the current tier has stopped answering. Every tier up buys seconds and hundreds of megabytes that a question the lower tier could settle never repays.

## R0 is per-forge

`rp get` prints the commands; nothing here needs memorising. The host in the
reference picks them — a bare `owner/repo` is github.com, or your enterprise
GitHub when `GH_HOST` is set, exactly as `gh` reads it.

| reference | code search | no-token floor |
|---|---|---|
| `owner/repo` | `gh search code --repo o/r` | `git ls-remote --symref <url> HEAD` |
| `GH_HOST=github.acme.io` + `acme/widget` | `GH_HOST=github.acme.io gh search code` | same |
| `gitlab.com/grp/sub/proj` | REST v4 `scope=blobs`, needs `$GITLAB_TOKEN` | tree + raw file, anonymous |
| `codeberg.org/o/r` (gitea/forgejo) | REST v1, needs `$GITEA_TOKEN` **and** a server-side indexer | tree + raw file, anonymous |
| any other host | none | `ls-remote`, blobless `--no-checkout` tree, `git archive --remote` |

Two traps that only bite off github.com:

- **Never send a forge token empty.** An empty `PRIVATE-TOKEN:` header makes GitLab
  302 to `sign_in`; the identical call with no header returns the public repo. rp
  emits `${GITLAB_TOKEN:+-H "PRIVATE-TOKEN: $GITLAB_TOKEN"}` for exactly this
  reason — copy that shape, do not hand-write the header.
- **`git archive --remote` does not exist on GitHub** (GitLab and Gitea have it),
  and `ls-remote` on a repo with a huge ref history can take 20s+. Always name
  `HEAD`; never list all refs.

The floor is why R0 never dies: `ls-remote` answers any git host for zero bytes
and no API. What a forge API buys on top is *code search*, nothing else.

## Which question belongs to which tier

| question | tier |
|---|---|
| does this symbol exist, and where | R0 search (github/GHE, or a tokened forge), else T0 grep |
| one file's contents, repo still remote | R0: `gh api .../contents/PATH`, GitLab `repository/files/.../raw`, Gitea `raw/HEAD/PATH` |
| what branches/HEAD does a repo I cannot API have | R0 floor: `git ls-remote --symref <url> HEAD` |
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

`rp doctor` also names which forges this box can actually search — `gh hosts` lists
every GitHub host `gh` is authed against (github.com and each enterprise host
separately), and `$GITLAB_TOKEN` / `$GITEA_TOKEN` say whether the REST forges are
live. A forge with no token is not a dead end: the git floor still answers there.

Done when `rp cache` reports the cache under budget, `rp doctor` reports no hard miss, and the question was answered at the lowest tier that could answer it.
