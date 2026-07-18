# arc

Change, review, and integration state over plain Git for agentic coding
arcs. Git owns content, branches, and history; `arc` owns the
collaboration objects Git deliberately lacks — changes, patchsets,
review findings, verdicts, verification evidence, holds, and a guarded
merge — as an append-only local ledger that every worktree and every AI
harness of one repository shares. No forge, no daemon, no database, no
web UI.

It exists so that the mechanical invariants of a multi-agent workflow
(approval bound to an exact patchset, blocking findings replayed
correctly, holds enforced across sessions, merges guarded against
unreviewed commits) live in code instead of in prompt discipline.
The `/arc` skill layer decides *what* to do; this CLI guarantees *what
happened and what is allowed*.

## The model

- A **change** is the unit of work (Gerrit's sense): a stable ID and
  slug that survive across revisions, tied to one branch and one
  integration target.
- A **patchset** is an immutable base/head snapshot of that branch,
  recorded with `arc snapshot`. Reviews and approvals bind to patchsets,
  never to moving branch names.
- A **brief** is a change-scoped implementation contract stored in the ledger;
  goal-scoped analysis briefs stay in the thread archive, and briefs never
  gate checking or integration.
- A **chain** is a tagged blocked-by series: independent siblings can run in
  parallel when ready, while dependent members wait mechanically until their
  prerequisites integrate.
- **Executor claims and typed stages** are advisory liveness signals. A claim
  has a stable generation ID plus actor, harness, and session, carries a TTL
  and resolved stage budgets, and is refreshed by stage activity. Stale and
  expired are distinct: stale means a live executor exceeded its current stage
  budget; expired means its lease activity exceeded the TTL.
- **Findings, verdicts, comments, replies, dispositions, claims, stages,
  verifications, holds, and closures** are append-only JSON events under
  `<git-common-dir>/arc/`, one file per event, created exclusively so
  concurrent writers never clobber each other. The ledger is
  authoritative; everything else is a view.
- **Approval staleness is structural:** a verdict is valid only while
  the branch head equals the approved patchset head. Any new commit
  makes it stale.
- **Gates** are declared in `.arc/gates.toml` (committed). `arc verify`
  runs a gate and records command, exact revision, result, exit code, duration,
  and hostname — local evidence with provenance, the local analogue of required
  CI checks. Attested verification records no exit code or duration because arc
  did not run the command.
- **Snapshots record Git author and committer identity.** When a claim is live,
  the snapshot also records its generation and actor at snapshot time and
  reports an author mismatch as provenance evidence, never as an integration
  blocker.
- **`arc integrate`** performs the merge only when, atomically checked:
  the head equals the approved patchset head, no blocking finding is
  open, every required gate is green at that exact head, and no hold is
  active. It merges the approved SHA (not the branch name) with
  `--no-ff`, then verifies the merge commit's parents. `arc integrate
  --tag '#series'` applies that same guarded path to every matching change
  in dependency order, stopping at the first refusal. Refusals carry typed
  exit codes.

## Quick tour

```sh
arc begin radio-refill-fix --title "Keep radio refill from restarting playback" \
  --tag '#radio' --blocked-by radio-foundation
# → branch arc/radio-refill-fix + worktree ~/.worktrees/<repo>-radio-refill-fix

arc is-blocked radio-refill-fix        # 0 ready, 1 blocked, 2 lookup/ledger error
arc blocker-status radio-refill-fix   # structured dependency detail

cd ~/.worktrees/<repo>-radio-refill-fix
export ARC_HARNESS=codex ARC_SESSION="$CODEX_THREAD_ID"
arc brief radio-refill-fix --body-file executor-spec.md
arc show radio-refill-fix               # includes the latest contract
arc claim radio-refill-fix --ttl 2h
arc stage radio-refill-fix started
# ... read the executor spec from arc show ...
arc stage radio-refill-fix spec-read
arc stage radio-refill-fix implementing
# ... implement, commit ...

arc stage radio-refill-fix verifying
arc snapshot radio-refill-fix          # record patchset ps-01
arc verify radio-refill-fix --gate test

# reviewer (any harness, any session) — one atomic call:
arc review radio-refill-fix --verdict changes-requested --findings-json - <<'EOF'
[{"blocking": true, "severity": "major", "summary": "stale batch can commit",
  "anchor": {"path": "src/ops.py", "line_start": 214}}]
EOF

# ... fix, then:
arc resolve radio-refill-fix f01ABC... --status resolved --commit HEAD
arc snapshot radio-refill-fix          # ps-02; old verdict is now stale
arc review radio-refill-fix --verdict approved

arc check radio-refill-fix             # exit 0 = ready
arc integrate radio-refill-fix --cleanup

# Integrate every matching series member in dependency order. --cleanup is
# allowed here; --into and --message are intentionally per-change only.
arc integrate --tag '#radio-series' --cleanup
```

Observe a change without scraping status views, or wait for one condition for
shell orchestration:

```sh
arc events --change radio-refill-fix --type patchset-added
arc events --follow                     # replay, then stream raw NDJSON events
arc watch radio-refill-fix --until snapshot --timeout 30
arc watch radio-refill-fix --until stalled
arc watch radio-refill-fix --until ready
```

`arc events` emits one compact raw ledger event per line. Replay output and
each observed follow batch are sorted by `event_id`; strict total ordering
across concurrent cross-change appends is not promised. `arc watch` emits a
single diagnostic and exits 0 when its condition is reached, or exits 2 on a
timeout. `snapshot` waits for a patchset, `ready` matches `arc check` success,
`stalled` reaches only when a live claim is stale, `integrated` requires that
closure outcome, and `closed` accepts integrated, abandoned, or superseded
changes.

`arc status <change>` prints the versioned `arc-status/5` JSON report —
the contract orchestrating agents program against. It includes dependency
state, inverse `blocks` links, tags, claim owner/activity/stage timing, snapshot
provenance, a blocker summary, a machine-readable `next_action`, an additive
`forge` projection block (see below), and ready
alternative open changes while the requested change is blocked. Actively
claimed non-stale changes and held changes are never suggested; stale and
expired claims reappear. `--json` is accepted for compatibility although
status is always JSON. `arc show <change>` renders the same actionable state as
Markdown, visibly marking stale, expired, and `blocked-on` progress.

Claims are advisory rather than merge locks:

```sh
arc claim radio-refill-fix \
  --stage-budget launch=60s --stage-budget implementing=30m
arc stage radio-refill-fix blocked-on --note "waiting for test fixture"
arc release-claim radio-refill-fix
```

`arc inbox` places active, non-stale claims in `in-progress`, showing their
owner, stage, and age in text output and as additive JSON fields. Stale active
claims remain exclusively in `stalled`; released and expired claims appear in
neither claim bucket.

Durations are positive integers ending in `s`, `m`, or `h`; the default claim
TTL is `2h`. The resolved defaults are `launch=60s`, `started=5m`,
`spec-read=2m`, `implementing=30m`, and `verifying=15m`. Repeating the current
stage is a heartbeat: it refreshes claim activity and age-in-stage. `blocked-on`
requires a note and is distress rather than stale;
claim TTL still applies. `snapshotted` comes only from a real `arc snapshot`
event and cannot be supplied to `arc stage`. An identified caller may release
any live claim so a lead can recover stale foreign work. Every claim, release,
and stage event carries its generation (snapshots carry the one observed at
snapshot time), so imported stale events cannot clear, advance, or claim
provenance for a replacement lease; a claim event without a generation is
rejected as malformed rather than replayed through inference.
Integration warns on an active foreign claim, including a stale one, but
proceeds when the normal integration gates pass.

## Forge projection

For `forge`-profile changes that project onto a hosted pull request, arc
records and validates the forge facts an agent observed — it makes no
network call, invokes no `gh`, and autodetects nothing. Declare the
explicit tuple and policy up front, then record the observed link, checks,
and lifecycle:

```sh
arc forge declare tidal-fix --host github.com \
  --base-repo 11xx/streamrip --base-ref dev \
  --head-repo 11xx/streamrip --head-ref arc/tidal-fix \
  --policy same-repository-only        # or allowed-base-repo=<owner/name>
arc forge link tidal-fix --pr 1 --url https://github.com/11xx/streamrip/pull/1 \
  --base-repo 11xx/streamrip --base-ref dev \
  --head-repo 11xx/streamrip --head-ref arc/tidal-fix --head-sha <sha>
arc forge checks tidal-fix --pr-head <sha> --state not-configured
arc forge pr-state tidal-fix --state open   # merged requires --merge-sha
```

`arc forge link` fails closed with exit 10, appending no event, when the
observed tuple differs from the declaration on any axis or violates the
declared policy (`same-repository-only` requires base repo == head repo;
`allowed-base-repo=X` requires base repo == X). The `forge` status block
reports `projection` (undeclared/declared/linked), the observed link,
`head_match` against the current approved patchset head (the exact-head
rule), the recorded checks state (`stale` for an older head, `unknown`
when none exists at the linked head), `pr_state`, and `forge_ready` —
true only when linked, head-matched, checks in {passed, not-configured},
and the PR is open, with `not-configured` surfaced as an explicit caveat.
A held, linked change renders an `awaiting_user` fact carrying the PR URL.
These facts are advisory rendering plus the fail-closed link validation;
they never change local `integrate` semantics. Close an externally merged
PR through `arc close --integrated <merge-sha>`.

## Execution roles

Delegated sessions can bind an execution boundary with
`ARC_ROLE=implementer|reviewer|lead` or the equivalent global `--role` flag.
Implementers may not run `review`, `resolve`, `hold`, `release-hold`, `close`,
or `integrate`; reviewers may not run `close` or `integrate`; leads retain full
access. Role refusals happen before the command takes a lock or writes an
event. An unset or empty role also retains full access for backward
compatibility, exactly like `lead`.

Dependencies are live ledger relationships: a blocker is satisfied when it
closes as integrated, or when a superseding successor eventually closes as
integrated (including through a transitive supersession chain). Abandoned
prerequisites and superseded chains that cannot resolve to an integrated change
are reported as `wedged`; clear or retarget them with `arc metadata`.
Missing or still-open changes remain blocked; `arc check` and `arc integrate`
enforce this boundary.
`arc blocker-status` exposes the versioned `arc-blocker-status/1` dependency
payload. `arc is-blocked` has its own polling contract: exit 0 means ready, 1
means blocked, and 2 means the lookup or ledger read failed, so automation must
stop rather than keep waiting. Add or remove dependencies and tags append-only
after creation:

```sh
arc metadata radio-refill-fix --blocked-by radio-storage --tag '#radio'
arc metadata radio-refill-fix --remove-blocked-by radio-storage --remove-tag '#radio'
```

Sequential bundle import can expose a dependency cycle assembled across
stores. Arc reports each member as blocked but does not mutate imported
history automatically. Break the cycle explicitly by removing one edge with
`arc metadata <change> --remove-blocked-by <blocker>`; exact blocker IDs also
work when the blocker's own bundle is absent.

State-derived writes use persistent OS-backed advisory locks with bounded
acquisition. Dependency metadata takes a repository graph lock before its
per-change lock; integration takes a target-branch lock before its per-change
lock. This prevents concurrent cycles and target-worktree races without
waiting forever on re-entry. State-append locks wait briefly; the target lock
waits integration-scale because its holder is legitimately running a merge.
Verification runs its external gate before taking the short state-append lock.

Query and batch views avoid ad-hoc JSON filtering:

```sh
arc query --status open --target master --tag '#radio'
arc list --format wide
arc show --tag '#radio' --json
arc check --tag '#radio'
```

See [DELEGATION.md](DELEGATION.md) before generating cross-harness executor
prompts. In particular, a Codex executor works locally and must never be told
to invoke `codex exec` on itself.

## Export / import

Move one change's complete ledger as a deterministic `arc-bundle/1`
JSON file:

```sh
arc export radio-refill-fix --output change.json
arc import change.json --dry-run
arc import change.json
```

Use `-` instead of a path for stdout or stdin. Re-exporting unchanged
events is byte-identical, and importing the same bundle again skips
identical events. A conflicting event makes the whole import write
nothing and exit 1. Missing Git commits are warnings rather than data
loss: available patchset heads are restored under
`refs/arc/keep/<change>/<patchset>`, while unavailable objects are
reported for separate transfer.

## Exit codes (`check`, and `integrate` refusals)

| Code | Meaning |
| ---- | ------- |
| 0 | ready / success |
| 1 | usage or internal error |
| 2 | open blocking findings |
| 3 | no valid approval for the current head (stale or missing) |
| 4 | hold active |
| 5 | required gates not green at head |
| 6 | closed change, missing branch, or malformed state |
| 7 | unresolved prerequisite changes |
| 8 | claim or stage ownership/liveness conflict |
| 9 | execution role refused the command |
| 10 | forge link refused: observed tuple or policy mismatch |

## Build and gate declaration

The repository provides the same convenient local commands used by its
implementation arcs:

```sh
make build
make test
make lint
```

Declared integration gates remain in `.arc/gates.toml`:

```toml
# .arc/gates.toml
[gates.build]
command = "cargo build"

[gates.test]
command = "cargo test"
profiles = ["local", "forge", "release"]   # omit = required for every profile
```

By default `arc verify` runs the gate and observes the result itself. When
the gate ran elsewhere — inside a sandbox, or on another host — record that
external evidence with `--attest --result pass|fail` (optionally `--note`):

```sh
arc verify <change> --gate test --attest --result pass --note "ran in CI sandbox"
```

Attested evidence is recorded without executing the command and still counts
toward gate green-ness, but `arc show` and `arc status` mark it `(attested)`
so a lead can apply stricter judgment. It has no exit code or duration because
arc did not run a process. `--attest` requires `--result`, and `--result`
without `--attest` is a usage error (arc observes its own result).

## Identity

Every event records an actor, and optionally a harness and native
session ID: `--actor/--harness/--session` or `ARC_ACTOR`, `ARC_HARNESS`,
`ARC_SESSION`. Actor defaults to `git config user.name`. `claim`,
`release-claim`, and `stage` require nonempty harness and session values;
identity is the actor + harness + session tuple.

## Configuration

arc treats `~/.local/ai/` as the AI data home (relocate it with
`AI_HOME`) and reads `~/.local/ai/arc/config.toml`:

```toml
worktrees_dir = "~/.worktrees"   # where change worktrees are created
data_root = "~/.local/ai/arc-data"  # optional: ledgers at <data_root>/<repo-path-slug>/
```

Environment variables override the file: `ARC_WORKTREES_DIR`,
`ARC_DATA_ROOT`, and `ARC_DATA_DIR` (an exact ledger directory for
exactly one repository — highest precedence). `data_root` keys each
repository by its slugged main path (the /thread convention:
`/home/x/code/y` → `-home-x-code-y`), so one root safely serves many
repositories — useful for sandboxing: point the paths somewhere
isolated and arc never writes outside them (worktrees, ledger) beyond
ordinary Git operations in the repository itself. `arc config` prints
the resolved paths as JSON.

Change derivation: `begin` targets the branch checked out in the
**primary worktree** (the main checkout — normally master/main), not
whatever branch the invoking worktree happens to be on. Deriving from an
open change's branch (stacking) requires an explicit `--target`.

`arc query` filters by lifecycle status, target, tags, latest verdict, opening
actor, and opening harness. `arc list --format compact|wide|json` supplies
pipe-friendly IDs, a scannable orchestration table, or structured rows.

## Storage and data-safety guarantees

- Ledger location: `<git-common-dir>/arc/` by default (shared by all
  worktrees); see Configuration for relocation.
- The ledger is append-only: arc never deletes or rewrites event files.
- Every reviewed head is pinned by its own `refs/arc/keep/<change>/<patchset>`
  ref, so Git GC cannot collect it — including earlier patchsets after a
  branch rewind. Pins are released only for heads proven reachable from
  the integration commit; abandoned or externally rewritten work stays
  pinned (release by hand with `git update-ref -d`).
- arc never passes `--force` to git: worktree removal refuses when
  dirty/untracked content is present, branch deletion refuses unmerged
  branches, and a failed merge is aborted back to the pre-checked clean
  state.
- A detected integration race (target moved) is reported, never
  "repaired" by rewriting refs.
- Git object IDs are stored as variable-length strings (SHA-256 safe).
- Anchors record path, side, blob OID, and line range; blob identity is
  what survives when line numbers drift.
- `arc` never rewrites source branches and never merges on its own —
  `integrate` is always an explicit invocation.
- Gates execute repo-committed commands (`.arc/gates.toml`): the trust
  level is the same as running `make` in that repository.

## Non-goals

Not a forge or forge clone, no hosted-PR parity claim, no daemon or web
UI, and no automatic multi-machine synchronization. Export/import moves
the ledger as one file; Git objects still travel separately. A forge-PR
projection is planned, while shared Git-ref sync remains deferred until
a real concurrent multi-machine need exists.

## Thread archive mechanics

`arc thread` encodes the drift-prone mechanics of the cross-harness /thread
archive while artifacts stay plain Markdown any tool-less agent can read and
write. The canonical agent-written journal is the append-only `journal.jsonl`,
whose versioned `thread-journal/1` events can be streamed as NDJSON with
`thread events [--limit N]`; legacy `journal.md` archives remain readable
forever. `dir` prints the resolved archive directory (`ARC_THREAD_DIR`,
then the `[threads.dirs]` map in the config file keyed by repository root,
then `<ai_home>/threads/<repo-slug>`); `note` writes a timestamped
`<ts>-<topic>-<kind>.md` artifact and its journal event; `journal` appends a
journal-only event; `catchup` lists newest-first. Work waiting for a future
session uses the primary actionable kinds — `todo`, `handoff`, `inbox`,
`plan` — plus lower-priority `later`. `thread open` lists the primary queue
first and then a separate later section until an explicit `thread consume
<filename> [--outcome done|superseded|discarded] [--note <text>]` retires an
item with a typed consumed event. The journal is append-only; consumption never edits or deletes
the artifact.

Memory artifacts are shared, always-surfaced project facts, one per file with
a heading that describes the fact. Retire them with `thread consume`; list
live memories with `thread memories`, and `catchup` leads with them after lanes.

Advisory **lanes** announce which topics a session is actively working so
parallel sessions sharing one archive see occupancy instead of guessing:
`thread lane open <topic> [--scope t1,t2] [--ttl 2h] [--status <text>]`,
`renew`, `close [--outcome done|handoff|abandoned|expired]`, and `list`.
Lanes are typed journal events, never locks. Malformed JSONL lines are skipped
so readers fail open. Liveness needs no heartbeats: any journal event by the
owning session restarts the ttl window; idle lanes
decay to stale, and anyone may close a stale lane `[expired]` — the
explicit takeover path. Opening a lane implicitly closes the session's
previous one (a session has at most one). `thread open` annotates items
covered by a live lane, distinguishing this-session from external
occupancy, and `catchup` leads with the lanes block so a newly opened
session sees who else is here first.

`thread doctor [--json]` performs a read-only archive health check. It reports
malformed journal lines, artifact names and kinds, and dangling references as
problems with a failing exit status, while stale lanes and archive housekeeping
remain non-failing advice; it never creates the archive directory.

## Roadmap

Shipped: local core +
policy engine (M1–M2), `/arc` skill wiring (M3), deterministic export/import
bundles (M4), local orchestration foundations (dependencies, tags,
actionable status, query, and batch views), claims/stages and execution
roles, messaging and the inbox rollup, thread archive mechanics, and the
ledger side of forge projection (declare/link/checks/pr-state with
fail-closed validation). Remaining evidence-driven possibilities live in the
thread archive's open items (`arc thread open`).

## License

[Unlicense](UNLICENSE) — public domain.
