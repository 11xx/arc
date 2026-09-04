# Schemas

Every structured surface carries a `schema` string of the form `<name>/<n>`.
The version is what a consumer programs against: any change to what a surface
emits takes the next version — adding a field as much as removing, renaming, or
redefining one — so a reader that pinned a version can tell from the version
alone whether the shape it parsed is the shape it is holding.

**Stability** says who the shape is for. A **commitment** is a shape callers
outside arc read: a `--json` view, an interchange file, or a stored input
format. Its version is a promise, and a consumer that pins one is entitled to
it. **Internal** marks arc's own on-disk bookkeeping — read and written by arc
alone, versioned for the same reason but carrying no promise to anybody else.
Parsing an internal shape means tracking arc's implementation.

## Derived views

| Schema | Surface | Stability |
| --- | --- | --- |
| `arc-status/17` | `arc status` — the actionable state of one change, including dependencies, claim timing, blockers, `next_action`, the review map, advisories, and the forge block | commitment |
| `arc-check/3` | `arc check --json` — every blocker with its exit code, plus never-blocking advisories | commitment |
| `arc-inbox/8` | `arc inbox --json` — the lead-facing queue buckets across open changes | commitment |
| `arc-catchup/4` | `arc catchup --json` — ledger buckets, journal lanes and queue, memories, forks, and worktree cost in one object | commitment |
| `arc-resume/1` | `arc resume --json` — one change's brief, live state, and journal context | commitment |
| `arc-rescue/2` | `arc rescue --json` — ledger state joined with worktree divergence and a foreign claim's standing | commitment |
| `arc-review/2` | `arc review --json` — verdict history, findings, causes, and the next action | commitment |
| `arc-findings/2` | `arc findings --format json` — the open finding set, or the audit set with `--audit` | commitment |
| `arc-blocker-status/1` | `arc blocker-status --json` — dependency detail for one change | commitment |
| `arc-metadata/1` | `arc metadata --json` — the derived tags, dependencies, and priority | commitment |
| `arc-chain/4` | `arc chain --json` — a tagged series in dependency order, with plan bindings and review coverage | commitment |
| `arc-stats/1` | `arc stats --json` — durations, counts, rework rounds, and suggested stage budgets | commitment |
| `arc-stats-by-model/1` | `arc stats --by-model --json` — one row per delegated identity, a different shape rather than a wider one | commitment |
| `arc-changelog/1` | `arc changelog --json` — the projected release copy for integrated changes | commitment |
| `arc-forks/1` | `arc fork list --json` — every fork from markers and branches together | commitment |
| `arc-doctor/3` | `arc doctor --json` — the ledger health report, problems apart from advice | commitment |
| `arc-workspace/1` | `arc workspace list --json` and `arc workspace inbox --json` — rows aggregated across registered projects | commitment |
| `arc-workspace-backlog/11` | `arc workspace backlog --json` — what is blocked on a decision per project, with its scope stated | commitment |
| `arc-writability/1` | `arc config --check-writable --json` — the probe an executor runs before it starts | commitment |
| `arc-sandbox-clone/1` | `arc sandbox clone --json` — the roots the copy was given | commitment |
| `arc-sandbox-diff/1` | `arc sandbox diff --json` — what the copy's events and refs differ by, in both directions | commitment |

## Journal views

| Schema | Surface | Stability |
| --- | --- | --- |
| `arc-journal-questions/2` | `arc journal questions --json` — every open question with its options, settle-by, delivery state, and the capability marker | commitment |
| `journal-discussion/2` | `arc journal discussion --json` — the derived view of one debate: tally, participants, rounds, open questions | commitment |
| `journal-source/1` | `arc journal source --json` — what one recorded session produced here | commitment |
| `arc-journal-latest/1` | `arc journal latest --json` — the newest artifact under one topic, with the resolved identity beside its body | commitment |
| `arc-journal-scaffolds/1` | `arc journal scaffolds --json` — the scaffolds a write can prepend, and one scaffold's body | commitment |

## Files

| Schema | Surface | Stability |
| --- | --- | --- |
| `arc-bundle/2` | `arc export` / `arc import` — one change's complete ledger as a deterministic JSON file | commitment |
| `journal-events/1` | `events.jsonl`, streamed by `arc journal events` — the canonical agent-written event log | commitment |
| `arc-journal-spool/1` | `.arc/outbox/<ts>-<kind>-<topic>.json` — a journal write parked for later promotion | commitment |
| `arc-sandbox/2` | `.arc-sandbox.json` — the marker naming a prefix as arc's to remove | internal |
| `journal-binding/1` | `bindings.jsonl` — which anchor a journal directory belongs to | internal |

## Versioning a stored input format

A derived view is versioned from the writer's side: arc emits it, and the
version says what arc emits. A stored input format is versioned from the
reader's side, and `journal-events/1` is the one that exists. Its version marks
what a reader must accept, so a new optional field that leaves every older file
valid keeps the version. Removing a field, or making one required, takes the
next one.

`arc-bundle/N` sits in between and is checked in both directions: a bundle
carries the store format it was written with, and arc refuses a bundle written
by a newer arc rather than skipping lifecycle events it does not know.
