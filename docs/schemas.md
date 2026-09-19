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
| `arc-status/21` | `arc status` — the actionable state of one change, including dependencies, claim timing, blockers, `next_action`, the current review subject, the review map, advisories, the forge block, captured plan provenance, and the fork a promotion came from | commitment |
| `arc-check/3` | `arc check --json` — every blocker with its exit code, plus never-blocking advisories | commitment |
| `arc-inbox/10` | `arc inbox --json` — the lead-facing queue buckets across open changes, unowned branches and worktrees, and the journal backlog | commitment |
| `arc-catchup/8` | `arc catchup --json` — ledger buckets, journal lanes and shared inventory rows, memories, forks with what they hold, unowned refs and checkouts, worktree cost, and a plan's settled promotion state | commitment |
| `arc-journal-catchup/8` | `arc journal catchup --json` — artifact storage/resolution facts, converged checkpoint tips, promotion state, and journal activity | commitment |
| `arc-resume/5` | `arc resume --json` — one change's brief, live state, and journal context, including captured plan provenance and the source fork's artifacts | commitment |
| `arc-brief/1` | `arc brief --json` — one selected brief and its immutable plan-source snapshot | commitment |
| `arc-journal-artifact/2` | `arc journal show --json` — raw artifact bytes and planner metadata | commitment |
| `arc-journal-inventory/4` | `arc journal inventory [FILE] [--archived] --json` — one read-only artifact projection with storage, claims, converged checkpoint tips, questions, promotions, promotion state, and ledger observation | commitment |
| `arc-rescue/3` | `arc rescue --json` — ledger state joined with worktree divergence, a foreign claim's standing, and the transcript reader's answer with its bound and cause | commitment |
| `arc-review/3` | `arc review --json` — verdict history, findings, causes, current review options, and the next action | commitment |
| `arc-findings/2` | `arc findings --format json` — the open finding set, or the audit set with `--audit` | commitment |
| `arc-blocker-status/1` | `arc blocker-status --json` — dependency detail for one change | commitment |
| `arc-metadata/1` | `arc metadata --json` — the derived tags, dependencies, and priority | commitment |
| `arc-chain/4` | `arc chain --json` — a tagged series in dependency order, with plan bindings and review coverage | commitment |
| `arc-stats/1` | `arc stats --json` — durations, counts, rework rounds, and suggested stage budgets | commitment |
| `arc-stats-by-model/1` | `arc stats --by-model --json` — one row per delegated identity, a different shape rather than a wider one | commitment |
| `arc-changelog/1` | `arc changelog --json` — the projected release copy for integrated changes | commitment |
| `arc-forks/2` | `arc fork list --json` — every fork from markers and branches together, with its age, head, worktree dirty counts, and the changes promoted from it | commitment |
| `arc-doctor/3` | `arc doctor --json` — the ledger health report, problems apart from advice | commitment |
| `arc-workspace/1` | `arc workspace list --json` and `arc workspace inbox --json` — rows aggregated across registered projects | commitment |
| `arc-workspace-backlog/18` | `arc workspace backlog --json` — what is blocked on a decision per project, with its scope stated, its collection manifest and failures, its separate blocking, availability, and coverage facts, the ordering basis used, every open change with its predicate buckets and round deferrals, and each plan's settled promotion state | commitment |
| `arc-workspace-inventory/1` | `arc workspace inventory --json` — every artifact across the selected stores with its storage, recorded resolution, transition successor, promotions, and reconciliation explanation | commitment |
| `arc-writability/1` | `arc config --check-writable --json` — the probe an executor runs before it starts | commitment |
| `arc-sandbox-clone/1` | `arc sandbox clone --json` — the roots the copy was given | commitment |
| `arc-sandbox-diff/1` | `arc sandbox diff --json` — what the copy's events and refs differ by, in both directions | commitment |

## Journal views

| Schema | Surface | Stability |
| --- | --- | --- |
| `arc-journal-questions/3` | `arc journal questions --json` — every open question with its options, settle-by, delivery state, the capability marker, and the suggestions standing on it | commitment |
| `journal-discussion/4` | `arc journal discussion --json` — the derived view of one debate: tally, participants, rounds, open questions, and their suggestions | commitment |
| `journal-source/1` | `arc journal source --json` — what one recorded session produced here | commitment |
| `arc-journal-latest/1` | `arc journal latest --json` — the newest artifact under one topic, with the resolved identity beside its body | commitment |
| `arc-journal-scaffolds/1` | `arc journal scaffolds --json` — the scaffolds a write can prepend, and one scaffold's body | commitment |

`arc-review/3` includes the optional `review_options` field. The schema is
`arc-review/3` even when no options apply and the field is omitted.

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
