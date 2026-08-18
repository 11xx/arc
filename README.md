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

New to arc? [docs/QUICKSTART.md](docs/QUICKSTART.md) walks a foreign repo
from install to a first integrated change.

## Installation

arc builds to a single binary and is Unix-only (see Storage and data-safety
guarantees for why):

```sh
cargo install --path .          # from a checkout of this repository
```

Optional shell completions and man page:

```sh
arc completions <bash|zsh|fish> > <completion-path>   # e.g. ~/.zfunc/_arc
arc mangen <dir>                                      # writes <dir>/arc.1
```

## The model

- A **change** is the unit of work (Gerrit's sense): a stable ID and
  slug that survive across revisions, tied to one branch and one
  integration target.
- A **patchset** is an immutable base/head snapshot of that branch,
  recorded with `arc snapshot`. It binds to the latest brief unless
  `--brief-version <n>` selects another recorded contract; changing that
  contract records a new patchset even at an unchanged Git head. Reviews and
  approvals bind to patchsets, never to moving branch names.
- A **brief** is a change-scoped implementation contract stored in the ledger;
  goal-scoped analysis briefs stay in the project journal, and briefs never
  gate checking or integration.
- A **changelog entry** is change-scoped release copy stored in the ledger.
  Integrated entries project into the generated `[Unreleased]` block.
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
- Replies target a finding ID or a comment/finding event ID. Finding replies
  appear beside their dispositions in findings JSON and beneath the finding in
  `arc show`; comment replies render beneath their parent comment.
- **Approval staleness is structural:** a verdict is valid only while
  the branch head equals the approved patchset head. Any new commit
  makes it stale. Which patchset a verdict binds to is therefore a fact worth
  stating: `arc review --patchset` takes the patchset id or the revision the
  reviewer actually read, and without it the verdict claims the newest patchset
  — which is not always the one that was reviewed.
- **Gates** are declared in `.arc/gates.toml` (committed). `arc verify`
  runs a gate and records command, exact revision, result, exit code, duration,
  and hostname — local evidence with provenance, the local analogue of required
  CI checks. Attested verification records no exit code or duration because arc
  did not run the command.
- **Snapshots record Git author and committer identity.** When a claim is live,
  the snapshot also records its generation and actor at snapshot time. Projects
  using per-actor Git identities report a mismatch as provenance evidence,
  never as an integration blocker; projects using one shared committing
  identity can explicitly disable that inapplicable comparison.
- **`arc integrate`** performs the merge only when, atomically checked:
  the head equals the approved patchset head, no blocking finding is
  open, every required gate is green at that exact head, and no hold is
  active. Holds are independent (`arc-status/8` turned the singular `hold` into
  `holds`): `arc hold` prints the event that identifies
  the hold it set, `arc release-hold <change> <hold>` lifts exactly that one
  (a unique prefix is enough; an empty or ambiguous one is refused), and
  every other hold stays in force. A release naming a hold that is no longer
  active replays as a no-op rather than a failure — two collaborators may
  reach the same conclusion, and a ledger nobody can reduce is a ledger
  nobody can repair. `arc doctor` reports it as `hold-release-names-no-hold`. It merges the approved SHA (not the branch name) with
  `--no-ff`, then verifies the merge commit's parents. `arc integrate
  --tag '#series'` applies that same guarded path to every matching change
  in dependency order, stopping at the first refusal. Refusals carry typed
  exit codes.

## Orientation

`arc` with no arguments prints the workflow guide: what the ledger owns, the
command lifecycle in order, how to pick a profile, and the invariants that
change what a session should do. It is the whole briefing an agent needs
before its first command, so no separate workflow document has to be loaded
first; `arc --help` remains the per-command reference.

`arc catchup` answers the other half — not how arc works, but what is waiting
right now. It renders the ledger's actionable buckets, live journal lanes,
shared project memories, and the full actionable journal queue in one call. A
session starting cold runs it first.

The two stores answer different questions and an empty one does not imply an
empty queue: the ledger holds changes already open, while the journal holds
everything not yet opened as a change. `arc inbox` therefore reports the
journal's tier counts and newest primary items beside its own buckets, and
`arc begin <slug> --from-journal <file>` turns a queued artifact into a change,
consuming it.

## Quick tour

```sh
arc                                    # workflow guide
arc catchup                            # what is waiting: ledger + journal

arc begin radio-refill-fix --title "Keep radio refill from restarting playback" \
  --tag '#radio' --blocked-by radio-foundation
# → branch arc/radio-refill-fix + worktree ~/.worktrees/<repo>-radio-refill-fix

arc is-blocked radio-refill-fix        # 0 ready, 1 blocked, 2 lookup/ledger error
arc blocker-status radio-refill-fix   # structured dependency detail

cd ~/.worktrees/<repo>-radio-refill-fix
eval "$(arc env)"                         # detects this harness session explicitly
arc brief radio-refill-fix --body-file executor-spec.md
arc brief radio-refill-fix --body-file executor-spec.md \
  --plan-ref 20260726T120000Z-radio-reliability-plan.md \
  --plan-slice refill-worker
arc changelog radio-refill-fix --category fixed --body-file changelog-entry.md
arc show                                  # includes the latest contract
# ... read the executor spec from arc show ...
arc stage implementing --claim            # default claim + stage
# ... implement, commit ...

arc done                                 # verifying → snapshot → verify --all → check
# Or select gates while snapshotting:
# arc snapshot --verify --gate build --gate test

# reviewer (any harness, any session) — one atomic call:
arc diff radio-refill-fix --findings         # native patchset diff + anchor drift
arc diff radio-refill-fix --stat -- src/ops.py
arc diff radio-refill-fix --since-approved   # re-review only the new patchset delta
arc findings radio-refill-fix --format sarif # export open findings to tooling
arc review radio-refill-fix                  # verdict history, findings, and next action
arc review radio-refill-fix --json           # the versioned arc-review/1 view
arc review radio-refill-fix --snapshot --verdict changes-requested \
  --cause executor \
  --body "The concurrency path still permits a stale commit." --findings-json - <<'EOF'
[{"blocking": true, "severity": "major", "summary": "stale batch can commit",
  "anchor": {"path": "src/ops.py", "line_start": 214}}]
EOF

# ... fix, then:
arc resolve radio-refill-fix f01ABC... --status resolved --commit HEAD
arc review radio-refill-fix --snapshot --verdict approved # fresh ps-02 + verdict

arc check radio-refill-fix             # exit 0 = ready
arc integrate radio-refill-fix --cleanup

# Integrate every matching series member in dependency order. --cleanup is
# allowed here; --into and --message are intentionally per-change only.
arc integrate --tag '#radio-series' --cleanup
```

Every `changes-requested` round classifies its root cause with one or more
`--cause` values. `brief` means the patchset faithfully exposed a missing,
false, or ambiguous premise; `executor` means it violated a correct applicable
brief; `integration-staleness` means later target work invalidated a brief and
implementation that were correct at their base. Other verdicts do not accept
causes. `arc review --json` and `arc stats --json` expose these classifications
without inferring them from verdict prose.

## Context awareness

Inside an open change's recorded worktree, `show`, `status`, `check`,
`snapshot`, `stage`, `claim`, `release-claim`, `verify`, `brief`, `comment`,
`finding`, `reply`, `resolve`, `review`, `done`, `blocker-status`, and `is-blocked`
infer an omitted change from the current branch, then the recorded worktree
path. An explicit change always wins. Commands that integrate, close, hold,
export, or otherwise act destructively or across changes still require an
explicit change. Ambiguous or absent context fails with the candidate list and
asks for `CHANGE`.

`arc changelog <change>` reads the latest entry for one change; adding
`--category <category> --body-file <file>` records replacement release copy as
a new append-only event. Categories are non-empty single-line strings; their
case and spelling are preserved. `arc changelog` projects entries from
integrated changes newer than the latest reachable tag. The built-in renderer
groups conventional categories in Keep a Changelog order, case-insensitively,
then emits other categories verbatim in bytewise order. `--since <revision>`
overrides the boundary, `--json` emits the versioned derived projection, and
`--write` replaces only the generated `[Unreleased]` block in `CHANGELOG.md`.
Projects can commit `.arc/changelog.toml` to select another repository-relative
target while retaining the built-in renderer:

```toml
target = "NEWS.md"
renderer = "keep-a-changelog"
```

Both keys default to the values above with `CHANGELOG.md` as the target. If the
configured file does not have a Keep a Changelog release shape, `--write`
leaves it untouched and prints the generated block to stdout.

`arc env` is the explicit identity bootstrap. It prints eval-able
`ARC_HARNESS` and `ARC_SESSION` exports from the first available variable in
this order: `CLAUDE_SESSION_ID`, `CODEX_THREAD_ID`, `OPENCODE_SESSION`,
`PI_SESSION_ID`. When the harness's own session store yields the model, it
appends an `ARC_MODEL` export (`model-slug[#effort]`): Claude reads the newest
assistant model from its project transcript; Codex honors `CODEX_HOME` and
reads the latest turn's model and effort; OpenCode reads the selected model and
variant from its SQLite session row when `sqlite3` is available; and Pi reads
model and thinking-level changes from its JSONL session. Detection failure is
a silent omission, never an error. It does not inject identity into other
commands; without a detected session it prints commented placeholders (naming
`ARC_MODEL` too) and exits 1.

`arc resume [CHANGE]` renders the latest brief, claim and stage, open findings,
head gate state, next action, live journal lanes, and matching open journal
items in one view. `--json` emits the versioned `arc-resume/1` schema with the
existing status payload and a journal block. `arc prompt [CHANGE]` prints the
stable one-line change summary used by statuslines, and exits successfully
with no output outside a change worktree.

Use `arc resume` to continue your own session's change. Use
`arc rescue [CHANGE]` when another session stopped and left work behind:
it joins the ledger state with worktree divergence and assesses a stale or
expired foreign claim as abandoned. Rescue is read-only unless `--take` is
given; takeover follows the same stale-claim rules as `arc claim --takeover`,
records the displaced owner, and narrates the handover to journal auto-log.
`--json` emits the versioned `arc-rescue/1` object.
`--transcript [--tail N]` includes the claimed session's latest operator turns;
arc prints what the transcript contains and performs no redaction, so the
option is opt-in and its output should be treated as sensitive.

Observe a change without scraping status views, or wait for one condition for
shell orchestration:

```sh
arc events --change radio-refill-fix --type patchset-added
arc events --since 01J00000000000000000000000
arc events --follow                     # replay, then stream raw NDJSON events
arc events --follow --exec './handle-event'
arc events --follow --tag arrears         # one stream for a tagged program
arc watch radio-refill-fix --until snapshot --timeout 30
arc watch radio-refill-fix --until ready,stalled --exec './notify'
arc watch radio-refill-fix --until snapshot --json
arc watch --tag arrears --all --until integrated --json
```

`arc events` emits one compact raw ledger event per line. Replay output and
each observed follow batch are sorted by `event_id`; strict total ordering
across concurrent cross-change appends is not promised. `arc watch` emits a
single diagnostic and exits 0 when its condition is reached, or exits 2 on a
timeout. `snapshot` waits for a patchset, `ready` matches `arc check` success,
`stalled` reaches only when a live claim is stale, `integrated` requires that
closure outcome, and `closed` accepts integrated, abandoned, or superseded
changes.

`events --tag` follows a whole tagged program as one stream rather than one
follower per member, which is what keeps the interleaving readable. Membership
is what it is while the stream is being read: a change that acquires the tag
joins it. `--change` and `--tag` select different scopes and cannot be combined.

`watch --json` emits one object instead of prose, naming the change, the
condition, and the event that satisfied it — `event_id` is present only when an
event did, because `ready` and `stalled` are derived from policy and elapsed
time rather than from something somebody recorded. A timeout emits
`watch-timeout` and still exits 2, so a script branches on the object rather
than on the text. Under `--tag --all` the object carries one entry per member
and no top-level placeholders.

`arc doctor` reports a `dangling-revision` for any revision the ledger records
that Git can no longer resolve — patchset heads and bases, brief bases, merge
bases, verification and verification-run revisions, audit revisions,
disposition commits, the integration commit, and the forge's recorded heads. A
history rewrite leaves the ledger intact and its evidence unreachable, which is
advice rather than a problem: the ledger is not malformed and the rewrite was
deliberate, but it is the difference between evidence and a claim.

`--since <event-id>` treats a valid ULID as an exclusive lower bound and
composes with replay, follow, change, and type filters. `events --exec <cmd>`
runs `sh -c <cmd>` once per emitted NDJSON line; handler failures are warnings
and do not stop the stream. `watch --exec <cmd>` runs once only when a watch
condition is reached, with a JSON diagnostic containing the winning
`condition` on stdin. Both hooks receive `ARC_EVENT_ID`, `ARC_EVENT_TYPE`, and
`ARC_CHANGE_ID`; watch hooks use `watch-reached` as the event type. Comma-separated
watch conditions are checked in their supplied order and the first reached
condition wins.

`arc status <change>` prints the versioned `arc-status/8` JSON report —
the contract orchestrating agents program against. It includes dependency
state, inverse `blocks` links, tags, claim owner/activity/stage timing, snapshot
provenance, a blocker summary, a machine-readable `next_action`, an additive
`forge` projection block (see below), and ready
alternative open changes while the requested change is blocked. Actively
claimed non-stale changes and held changes are never suggested; stale and
expired claims reappear. `--json` is accepted for compatibility although
status is always JSON. `arc show <change>` renders the same actionable state as
Markdown, visibly marking stale, expired, and `blocked-on` progress.

For scripts that only need part of a report, `arc status` and `arc resume`
accept `--get <dotted.path>` (including numeric array indices) and
`--fields <top-level,keys>`. `--get` prints scalars without JSON quotes and
objects or arrays as compact JSON; `--fields` prints a compact object subset.

Four derived views strengthen audit and orchestration without new events —
each is pure replay over the existing ledger:

```sh
arc log radio-refill-fix                 # one line per event, oldest first
arc log radio-refill-fix --reverse       # newest first
arc show radio-refill-fix --at <event>   # state as the actor saw it then
arc status radio-refill-fix --at <event> # same replay, JSON report
arc check radio-refill-fix --explain     # full readiness checklist
arc check radio-refill-fix --json        # every blocker + exit code as JSON
arc integrate radio-refill-fix --dry-run # simulate the merge, write nothing
```

`arc log` prints `<ts>  <actor>@<harness>  <event-type>  <summary>` per event
in ledger order; take event IDs from `arc events`. `--at <event-id>` replays a
change to that point and answers "what did the actor see?": the derived
latest-patchset head stands in for the live branch head, so an approval that a
later snapshot invalidated still shows valid as of the review. Unknown event
IDs are rejected. `check --explain` evaluates every gate condition — passing
and failing — and prints the exit code the first blocker sets; the plain
`check` exit code is unchanged, so existing automation keeps working.
`integrate --dry-run` runs the same readiness preflight and merge simulation
and reports the would-be merge parents and result without appending an event,
moving a ref, or touching a worktree.

`arc stats [--change <c> | --tag <t> | --all] [--json]` (default `--all`)
projects the durations and counts the ledger already holds: per change the
open→integrated wall time, seconds in each typed stage, snapshot→first-verdict
review latency, observed gate wall times, findings by severity, patchset count,
review causes, typed executor blocks, and structurally completed rework rounds.
Legacy blocked-on events without a referent are counted as `unclassified`. A
rework round needs a new patchset after `changes-requested` and a later
approval of that patchset; reversing the verdict on the same patchset is not
rework. A round is a revision cycle rather than a verdict event, so several
`changes-requested` verdicts on one patchset count once — one revision answers
them all. First-pass approval
means the first patchset was approved before any requested-rework verdict. The
aggregate block adds median and p90 per stage and per gate, reworked-change and
first-pass totals, completed rework rounds, plus
`suggested_stage_budgets` — each stage's p90 rounded up to a clean duration,
offered for `--stage-budget` tuning and never applied automatically. Attested
gate evidence carries no observed duration and is excluded from gate timing.
JSON is versioned `arc-stats/1`.

`arc stats --by-model` answers a different question from the same events: one
row per delegated identity rather than per change, with changes touched,
patchsets contributed, rework rounds those patchsets caused, and verdicts
issued as reviewer — before integration and after it, since an audit is a
review that happened. Rows are keyed on the `--on-behalf-of` subject, never the
actor — a lead runs the ceremony on an executor's behalf, so attributing by
actor would credit the lead for every line the executor wrote. A round is
charged to the patchset that was sent back rather than to the revision that
answered it, so the number is what an identity's work cost and not what it
cleaned up. Work with no recorded subject is counted in its own
`(unattributed)` row rather than distributed silently; model identity is a
convention leads write into `--on-behalf-of`, and nothing enforces its shape.
This view is versioned `arc-stats-by-model/1`, separately from `arc-stats/1`,
because it is a different shape rather than a wider one.

Claims are advisory rather than merge locks:

```sh
arc claim radio-refill-fix \
  --stage-budget launch=60s --stage-budget implementing=30m
arc claim radio-refill-fix --takeover
arc stage radio-refill-fix blocked-on \
  --blocker external --note "waiting for test fixture"
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
requires both a structured `--blocker` (`brief:vN`, `finding:ID`, `change:ID`,
or `external`) and a note naming the precise missing symbol, file, command, or
premise. It is distress rather than stale;
claim TTL still applies. `snapshotted` comes only from a real `arc snapshot`
event and cannot be supplied to `arc stage`. An identified caller may release
any live claim so a lead can recover stale foreign work. `claim --takeover`
explicitly replaces an active stale claim, records the displaced owner and
stage, and refuses active claims that have not exceeded their stage budget.
Every claim, release,
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
Implementers may not run `review`, `audit`, `audit-debt`, `resolve`, `hold`,
`release-hold`, `close`, or `integrate`; reviewers may not run `audit-debt`,
`close`, or `integrate`; leads retain full access. Declaring audit debt is a
lead decision because an open declaration changes whether a self-approval can
gate. Role refusals happen before the command takes a lock or writes an event.
An unset or empty role also retains full access for backward compatibility,
exactly like `lead`.

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
arc metadata radio-refill-fix --priority 20
```

Read the current derived metadata without appending an event. The default is a
concise text projection; `--json` emits the versioned `arc-metadata/1` shape:

```sh
arc metadata radio-refill-fix
arc metadata radio-refill-fix --json
```

Fleet executors can atomically select and claim work instead of racing a
separate query and claim:

```sh
arc take --tag '#radio' --ttl 2h
```

Inspect the whole tagged program without collapsing it into queue buckets:

```sh
arc chain '#radio'
arc chain '#radio' --json
arc chain '#radio' --review
```

`arc chain` includes open and closed members exactly once in dependency order,
reports their current brief plan bindings and the referenced plan history, and
names the same next ready change that `arc take --tag` would select. The JSON
form uses the `arc-chain/2` schema. The view is entirely derived and does not
infer aggregate completion, pauses, unopened slices, duplicate slices, or a
progress percentage.

With `--review`, each member also gets a compact lifetime summary with its
final-patchset verdict count alongside it. The JSON view includes the recorded
subject and `at_final` and `lifetime` windows, each containing distinct verdict
identities, verdict and finding counts, and ad hoc verification count.
`non_self_verdict` reports whether any lifetime verdict identity differs from
the subject.

`arc take` considers open, unheld changes whose blockers are integrated and
whose claims are absent, expired, or stale. It selects higher priorities first,
then the oldest change, and exits 2 when nothing is ready. Repeated `--tag`
filters are conjunctive. `--json` returns the selected change's full status.
Selection and claim publication share the repository graph lock, so concurrent
`take` calls serialize and cannot receive the same change.

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
nothing and exit 1. Every event must carry a complete envelope that this
build can decode; an unrecognized payload tag is preserved verbatim and
excluded from typed replay. Missing Git commits are warnings rather than
data loss: available patchset heads are restored under
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

[gates.integration]
command = "cargo test --test integration"
timeout = "10m"                              # optional; s, m, or h
```

By default `arc verify` runs the gate and observes the result itself. When
the gate ran elsewhere — inside a sandbox, or on another host — record that
external evidence with `--attest --result pass|fail` (optionally `--note`):

```sh
arc verify <change> --gate test --attest --result pass \
  --tested-revision <commit> --execution-host ci.example \
  --runner build/test-42 --note "ran in CI sandbox"
```

Attested evidence is recorded without executing the command and still counts
toward gate green-ness, but `arc show` and `arc status` mark it `(attested)`
and name the external runner and host so a lead can apply stricter judgment. It
has no exit code or duration because arc did not run a process. Attestation
requires the result, tested revision, execution host, and runner together;
those context flags are invalid for locally executed verification.

Every multi-gate invocation records its intended gate manifest before execution
and correlates each observed result to that run. `--skip-green` records a reuse
edge to the earlier passing evidence instead of silently omitting work. A run
is complete when every declared gate has an observed or reused terminal edge;
an interrupted run remains visibly incomplete without a mutable completion
record.

Every locally executed gate captures and attempts to retain the worktree tree
before it runs. If the tree cannot be retained, local provenance remains
unknown and a passing result does not count as green. Sequential runs also
record whether the retained tree differed from the tested revision and whether
the worktree moved during execution. For `verify --all --parallel`, the shared
worktree cannot prove that no gate made a transient change and restored it, so
cleanliness remains unknown and passing results do not count as green. A
boundary change is still recorded as `tree_moved` and explains the stronger
failure.

Evidence that passed but cannot be reused reads `pass` in its raw result, so
every human-facing surface names why it does not count: `arc resume`, `arc
rescue`, and `arc check` render the reason beside the gate, and `arc show`
renders it beside the verification it belongs to. Because a rerun
against the same unusable tree records the same unusable evidence, the
`next_action` is `clean_worktree:<gate>` while the worktree is actually
dirty — for any gate that is not green, including one that failed, because
while the tree is dirty no local run produces evidence that counts
(attested evidence carries its own execution context instead). Cleaning is
not a fix for a failure; it is the precondition for a local run whose
result is usable. Where the live tree is unknown, because the change has no
worktree, the advice is the rerun. Once the tree is clean the advice
becomes `run_gate:<gate>`, since
evidence already recorded cannot be repaired by cleaning; only a fresh run
replaces it. `clean_worktree:<gate>` is new in `arc-status/7`.

Executed gates capture combined stdout and stderr, retaining only the final
4096 bytes. Failed-gate tails appear in `arc show` and `arc status`; successful
gates stay compact. A declared timeout terminates and reaps the gate's entire
process group and records the failure as timed out. Without `timeout`, gate
execution remains unbounded.

## Git integration

Git hooks are strictly opt-in: arc never installs them silently. `arc hooks
install [--force]` writes two scripts into the resolved hooks directory (it
honors `core.hooksPath`), each a two-liner delegating to `arc hook-run`. It
refuses to overwrite a foreign hook unless `--force` is given, which first
saves the original as `<hook>.pre-arc`. `arc hooks uninstall` removes only
arc-authored scripts (a marker comment identifies them), and `arc hooks
status` reports whether each hook is absent, arc-managed, or foreign.

Both hooks are advisory and always exit 0, so they can never block a commit.
The `post-commit` hook, on a change branch, prints a notice when the new
commit has staled an approval bound to the previously approved snapshot, or
warns when the branch's change is already closed. The `prepare-commit-msg`
hook appends an `Arc-Change: <change-id>` trailer on a change branch when one
is not already present, giving commits durable linkage back to their change.

Independent of hooks, `arc query --commit <revision>` reports the changes
whose patchset heads or integration/closure commit match a revision (a unique
prefix is accepted). It searches the ledger only; it does not scan commit
trailers.

## Workspace, brief scaffolds, and restack advice

Three read-only conveniences for a lead working across repos and handoffs.

`arc workspace list|inbox|backlog [--json]` aggregates across every project.
Discovery has two modes and the configured one wins: with a `data_root` the
stores sit side by side and enumerate directly; without one they live inside
each repository's Git common dir, where the journal registry — one directory
per project, keyed by its anchor — is what knows they exist. A journal records
its anchor in `bindings.jsonl`; one written before that is reconstructed from
its directory name and confirmed against the filesystem, and a name that
resolves to no single existing path stays unresolved rather than guessed at.
`arc begin` registers the project, so opening a change is enough to make a
repository discoverable even if nothing is ever written to its journal. A
`[journals] dirs` scope registers its project too, which is how a directory
that is not a Git repository takes part.

A cold archive is identified structurally rather than by its name: `<x>-archive`
is skipped only when journal `<x>` is also present, so a project genuinely
called that is not lost.

`list` prints per-repo open-change rows; `inbox` concatenates each repo's inbox
rollup, tagged with the repo. The scan opens each store read-only, never
creates one, and skips unreadable entries with a warning. JSON is versioned
`arc-workspace/1`. The workspace inbox is ledger-derived — it consults no
per-repo working tree, so rebase and gate buckets are not evaluated there.

`backlog` answers what no single repository can: where work is blocked on a
decision rather than on effort. Per project it reports changes awaiting a
verdict, changes carrying audit debt, the journal's three tiers, and the
primary tier's oldest entry — a one-item queue never looks like a backlog from
inside its own project. Projects are ranked by what is blocked; items are never
ranked against each other across projects, because arc records no priority that
spans repositories. A project whose journal holds work but whose anchor no
longer resolves is reported under `unreachable` with the `journal rebind` that
adopts it; `list` and `inbox` cannot report it, so they name what they skipped
on stderr rather than dropping it silently, and say so plainly when no
project is registered at all rather than printing nothing: it is exactly the backlog no per-project command can reach, since
standing in the project is how every other view starts. `--since <stamp>` turns
the report into a delta, where the journal counts mean arrivals rather than
outstanding work; blocked work is still reported in full. arc stores no
previous-run marker — the boundary is supplied by the caller, so the command
stays derived. JSON is versioned `arc-workspace-backlog/1`.

`arc brief <change> --scaffold <name>` prepends a template to the brief being
recorded (`--scaffold` alone records the template). A repo-local
`.arc/templates/<name>.md` wins; otherwise a built-in applies (`sol-low`,
`sol-high`, `reviewer`). The built-ins encode the delegation fences — scope
ceiling, no tests beyond those listed, stop on a missing target, release the
claim on stop, never review or integrate — and the sandbox facts an
arc-driving executor needs (`.git` must be writable; stage and report
"staged, no SHA" when signing is unavailable so the lead commits then
snapshots then reviews; keep claim/stage heartbeats current).

`--plan-ref <artifact> --plan-slice <slug>` records which opaque slice of an
existing journal plan the brief implements. The flags are required together;
the plan may be in the hot journal or its cold archive. Later brief versions
may repoint the link, and multiple briefs may name the same plan and slice.
Every newly written brief also records the full current `HEAD` as its immutable
base revision. `--base <revision>` selects another revision and resolves it at
write time; legacy briefs alone may have no base revision.

Every brief after the first records why it exists. `--caused-by
finding:<id>`, `verdict:<event>` or `blocked-on:<event>` cites a prior event
in the same change; `--cause-note <summary>` states an external reason in
prose. Both may be repeated and combined, both require `--body-file` or
`--scaffold`, and v1 refuses them — a first contract has no prior version to
justify. A `verdict:` reference must name a `changes-requested` verdict,
because the other verdicts do not ask for a revision. References resolve by
unique prefix and are stored canonically, so an ambiguous prefix refuses
rather than picking a candidate: the resolved identifier is permanent.

`--probes-json <file>` binds named acceptance commands to one brief version.
Probe names are unique kebab-case slugs. Run a declaration explicitly with
`arc verify <change> --probe <name> --probe-phase baseline|final`; the phase
defaults to `final`, and `--brief-version <n>` selects a historical contract.
A baseline run is valid only at that brief's base revision and treats command
failure as its expected result. Final evidence treats command success as
expected. Both phases retain the canonical brief and probe reference; `done`
does not execute arbitrary acceptance probes. Once declared, every probe blocks
readiness until evidence bound to the patchset brief fails at the brief base and
passes at the patchset head. That pair proves behavioral discrimination, not
semantic relevance: reviewers still inspect the baseline output to confirm the
intended failure.

`arc restack <change> --advise` prints, for each open dependent of a change,
the exact `git rebase --onto <target> <base>` command to run in that
dependent's worktree once the change integrates. It only prints — arc never
rewrites a branch — and writes no events; with no dependents it says so and
exits 0.

## Policy

Repository integration policy is declared in `.arc/policy.toml`. Policies are
disabled when the file or setting is absent. Set
`[policy] forbid_self_approval = true` to reject an approval when its effective
author matches the one recorded by `arc snapshot` for that patchset, or when
arc assumed either identity rather than someone declaring it — two invented
names that happen to differ do not show that two people acted. Actor identity
remains advisory; this comparison does not redesign or verify identity.
Rejected self-approval follows the existing no-valid-approval path and exits 3.

Every event records `actor_source`: `flag`, `env`, or `git-fallback`. The last
means nobody declared an identity and arc took `git config user.name`, which
names whoever configured the checkout rather than whoever acted. The
substitution is announced on stderr the first time a command would record it,
because the ledger is append-only and that is the last moment to correct it.
Events written before arc recorded provenance carry no source; that is
*unknown* rather than assumed, and is compared by name as it always was.

Set `[policy] require_declared_actor = true` to refuse an event whose effective
author nobody claimed. `begin`, `verify`, and `integrate` check before they
create a branch, run a command, or merge, so a refusal never lands after the
work; every other append is guarded at the store. Reading is unaffected, as is
a bundle import, whose events are another repository's history being
transferred rather than this session's claim about who acted. A lead acting for
a declared `--on-behalf-of` subject satisfies the policy.

After integration the rule narrows: `arc audit` refuses an approving audit from
an identity arc assumed, which the auditor can fix by declaring itself, but not
one whose *authoring* identity was assumed — that is already on the ledger and
cannot be corrected, and refusing would leave the debt undischargeable rather
than making anyone independent. Such an audit warns that it shows a review
happened and not that it was independent.

Optional reviewer reminders live in the same file under `[review]`, for
example `checklist = ["exercise the failure path"]`. `arc show` renders these
advisory items only for reviewer and lead roles; they never block integration.

## Review coverage and post-integration audits

Whether somebody reviewed a change and whether somebody reviewed *what is about
to ship* are different claims, and only the second one matters at integration.
A review panel can run correctly for many rounds and still let the final round
of corrections ship unseen — an identity comparison reads that as clean.

So arc derives a **review map**: for every identity that filed a verdict or
finding, the newest patchset it saw, and whether that is the final one. `arc
status --json` carries it as `review_map`, `arc show` renders it, and `arc
check` prints warnings like `Reviewer last saw ps-07; integrating ps-11`. These
never block — plenty of changes legitimately ship with a single reviewer. A
reviewer that cannot be told apart from the patchset author (neither side
recorded `--on-behalf-of`) is reported as unknown attribution rather than
counted as independent or as self-review.

When `forbid_self_approval` is set and no second actor is reachable, a change
would otherwise be unshippable. Declaring an **audit debt** resolves that
without waiving anything:

```sh
arc integrate <change> --audit-debt "no independent reviewer reachable"

# later, when a reviewer is available
arc inbox                                    # audit-owed bucket
arc catchup                                  # the same, with reasons
arc query --audit-debt                       # IDs alone, for scripting
arc diff <change>                            # what to review
arc audit <change> --verdict approved --body-file -
arc findings <change> --audit                # what the audit raised
```

The obligation is a ledger fact that survives closure, so the owed review is
findable instead of living in prose. `arc inbox` carries it in the one bucket
that includes integrated changes, `arc doctor` reports it as
`audit-debt-outstanding`, and `arc chain` shows it beside reviewer coverage.

Three rules keep the escape hatch from becoming a hole:

- **The waiver binds to a patchset.** A debt declared for `ps-01` stops
  excusing self-approval the moment `ps-02` is snapshotted, exactly as an
  approval goes stale. Re-declaring is deliberate. A debt declared after
  integration carries no patchset and waives nothing.
- **An approving audit must come from another identity.** Otherwise the change
  ships on a self-approval and then clears its own record. Auditing into
  `changes-requested` is open to anyone — raising problems needs no
  independence.
- **An audit is a distinct event**, anchored to the integrated revision and
  refused while the change is still open. Attaching one can never rewrite the
  answer to "what shipped with what review", and audit findings stay out of the
  shipped set — `arc findings --audit` reads them. `arc reply` addresses either
  finding kind, while `arc resolve` records an integrated-only audit
  disposition for an audit finding. The ordinary disposition event remains
  open-only, so later audit work cannot rewrite shipped finding state.

## Identity

Every event records an actor, and optionally a harness and native
session ID: `--actor/--harness/--session` or `ARC_ACTOR`, `ARC_HARNESS`,
`ARC_SESSION`. Actor defaults to `git config user.name`. `claim`,
`release-claim`, and `stage` require nonempty harness and session values;
identity is the actor + harness + session tuple.

Explicit identity always wins. Set `[identity] detect = true` in the config
file to fill omitted harness, session, and model values from the running
harness's own session store. Detection is off by default and does not mix a
detected session into a different explicitly selected harness.

Journal events additionally record the acting model via `--model` or
`ARC_MODEL`, a `model-slug[#effort]` string (e.g. `kimi-k3#high`,
`gpt-5.6-sol#low`) matching the `Assisted-by: Harness:Model#Effort` grammar.
It is optional everywhere: an empty value is treated as unset, and an absent
model is serialized as absent — never stamped "unknown".

When a lead runs ceremony for a sandboxed executor — committing its staged
work, then claiming, snapshotting, or reviewing — `--on-behalf-of <subject>`
(or `ARC_ON_BEHALF_OF`) records who the action is *for* while `actor` stays the
invoker who ran it. The **effective author** of an event is
`on_behalf_of.unwrap_or(actor)`. `forbid_self_approval` compares effective
authors, so a lead snapshotting on behalf of an executor and then approving as
itself is not self-approval, whereas approving `--on-behalf-of` that same
executor is. A declared subject is always somebody's claim, so it satisfies
`require_declared_actor` however the invoker was identified. Claim ownership is unaffected: it still matches on the invoker's
actor + harness + session tuple, and `on_behalf_of` is recorded and rendered
but never changes who owns or may release a claim. The field is additive and
serialized only when set, so existing events and bundles round-trip unchanged.

## Configuration

arc treats `~/.local/ai/` as the AI data home (relocate it with
`AI_HOME`) and reads `~/.local/ai/arc/config.toml`:

```toml
worktrees_dir = "~/.worktrees"   # where change worktrees are created
data_root = "~/.local/ai/arc-data"  # optional: ledgers at <data_root>/<repo-path-slug>/

[provenance]
git_identity = "per-actor"         # default; use "shared" for one Git identity
```

Environment variables override the file: `ARC_WORKTREES_DIR`,
`ARC_DATA_ROOT`, and `ARC_DATA_DIR` (an exact ledger directory for
exactly one repository — highest precedence). `data_root` keys each
repository by its slugged main path (the project-journal convention:
`/home/x/code/y` → `-home-x-code-y`), so one root safely serves many
repositories — useful for sandboxing: point the paths somewhere
isolated and arc never writes outside them (worktrees, ledger) beyond
ordinary Git operations in the repository itself. `arc config` prints
the resolved paths as JSON.

The committed `.arc/policy.toml` may set the same `[provenance]` table for a
repository. `per-actor` compares the claim actor with the snapshot author and
committer; `shared` omits `provenance_mismatch` because that comparison does
not apply. A delegated snapshot can instead declare its subject with
`--on-behalf-of`.

Before starting an executor in a sandbox, run `arc config --check-writable`.
It probes the ledger root, lock, event-path, and Git-ref writes without adding
an event; `--json` emits `arc-writability/1` for automation and stops at the
first blocked path. It also probes committing, in a throwaway repository so the
target gains no commit, because a sandbox that cannot reach the signing agent
otherwise discovers it only once a slice is ready to land. The probe follows
the repository's resolved `commit.gpgsign` and carries its signing key, so it
exercises the credential the real commit will use and ignores a global signing
policy the repository overrides.

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
- Derived views (`list`, `inbox`, `status`, `query`) replay the ledger on
  every invocation and hold no persistent cache. This stays fast in practice
  (measured ~8 ms for `list` and `inbox` over 300 changes). Any future
  acceleration must live under a `derived/` cache that is deletable,
  rebuildable, and never authoritative — the ledger remains the only source
  of truth.
- **Unix-only, deliberately.** arc relies on POSIX semantics for its safety
  guarantees: `0700` private directories, atomic hard-link event publication,
  and process-group kill for gate timeouts. There is no Windows port and one
  is out of scope; run it under WSL there.

## Non-goals

Not a forge or forge clone, no hosted-PR parity claim, no daemon or web
UI, and no automatic multi-machine synchronization. Export/import moves
the ledger as one file; Git objects still travel separately. A forge-PR
projection is planned, while shared Git-ref sync remains deferred until
a real concurrent multi-machine need exists.

## Project journal mechanics

> **The ledger is authoritative and gating; the journal is advisory and
> contextual.**

`arc journal` encodes the drift-prone mechanics of the cross-harness project
journal while artifacts stay plain Markdown any tool-less agent can read and
write. The canonical agent-written event log is the append-only
`events.jsonl`, whose versioned `journal-events/1` events can be streamed as
NDJSON with `journal events [--limit N]`. `dir` prints the resolved journal
directory (`ARC_JOURNAL_DIR`, then the longest matching absolute path prefix
in `[journals.dirs]`, then Git identity and
`<ai_home>/journals/<repo-slug>`). `dir --explain` prints the selected source,
stable anchor, and directory; resolution refuses to guess outside Git when no
configured prefix matches. `note`
writes a timestamped `<ts>-<topic>-<kind>.md` artifact and its journal event;
`log` appends a log-only event; `journal position` adds a position to an
existing artifact; `catchup` lists newest-first.
`list [--kind <k>] [--json]` enumerates every live artifact in the hot
directory newest-first —
including the non-actionable kinds `open` and `memories` do not show —
marking each consumed item with its outcome. Read-side `--kind` filters on
`list` and `open` accept retired kinds so historical artifacts remain
discoverable, while `journal note --kind` accepts only active kinds.
`show <filename>` prints one
artifact's raw Markdown body, resolving the hot directory first and then the
cold archive. Dated inline headings (positions, conclusions, review stamps)
take the house timestamp from `journal stamp` — RFC 3339 seconds in UTC, the
same spelling as the event log's `ts`, so prose and log cross-grep — never
an agent-authored date.
Work waiting for a future session uses the primary actionable kinds — `todo`,
`handoff`, `plan`, `discussion` — plus lower-priority `later` and
`feature-request`. A feature request describes a wanted capability without
assigning execution priority. A `decision` records a settled question as a
terminal, non-actionable artifact. `journal open` lists the primary queue first,
then separate later and feature-request sections, annotating each item with how
long it has waited (for discussions, since the latest typed position; otherwise
since creation), until an explicit
`journal consume <filename> [--outcome done|superseded|discarded]
[--decision <decision-file>] [--note <text>]` retires an item with a typed
consumed event. A decision link is valid only with outcome `done` and may name
a decision from a different topic. The journal is
append-only; consumption never edits or deletes the artifact.

A `discussion` is the answer-owed actionable kind: an open debate rides the
same queue until someone resolves it, and `arc begin --from-journal` promotes
one straight into a change. `journal position <filename> [--ref <target>]
--body-file <src>` adds one position: it writes a
`### Position pos-<ulid> (<model[#effort]> via <harness>, <utc-ts>)` block —
the heading tool-computed so its stable reply target and timestamp are never
hand-authored — below the file's existing content, and emits a typed `position`
journal event carrying the same ID and identity plus the optional `--ref` (a
position ID, legacy timestamp, or item slug it answers). Consumed artifacts
reject late appends; re-litigation starts a successor discussion. The Markdown
is for people and block-scoped stance parsing; the event supplies activity,
identity, resolver-participation, and reply edges. The file stays hand-writable,
and the append is advisory and fail-open like every journal write.
`journal note --kind discussion` seeds the conventions at birth by default
(the `Position: for|against|amend` stance line, reply-to quoting, the
resolution vocabulary, and the norm that a contested discussion is resolved by
a non-author of the winning position or by the user). `--scaffold discussion`
requests that template explicitly, while `--no-scaffold` records body content
alone. A repo-local `.arc/templates/discussion.md` overrides the built-in, and
`--scaffold` works on any `journal note`: the template is prepended to
`--body-file` content, or recorded alone. Existing hand-written discussions
remain readable; `journal discussion` marks position blocks without a first
stance line as `unstated` rather than silently counting an incomplete tally.
`journal discussion <filename> [--json]` renders the derived view of one
debate: its age, the stance tally (`for`/`against`/`amend` parsed once per
actual position block, so hand-written positions count too), the distinct
participants and reply-refs from the typed `position` events, rounds grouped
by reply depth, and the position IDs still unanswered. Positions in the same
round could not have read one another, so rounds express reply structure rather
than turn-taking. Once resolved, the view also includes the outcome, optional
decision artifact, and a resolver-participation flag that surfaces a resolver
who also argued a side under the same harness-native session identity.

The journal and the ledger bridge in three advisory ways, none of which can
gate the authoritative ledger. `arc begin --from-journal <artifact>` opens a
change from an open actionable item and records the artifact filename as the
change's `journal_ref`. A plan reference leaves the plan open and seeds no
brief, so the same plan can father multiple changes until an explicit
`journal consume`. Every other actionable kind appends a `consumed` event with
outcome `superseded` and note `change <id>` (leaving the artifact file
untouched), then seeds an initial brief threaded from the source body so the
change starts with the resolution. A missing, non-actionable, or already
consumed source is refused before anything is written.
With `[journal] auto_log = true` in the config file, `begin`, `integrate`, and
`close` append a narrating `log` event (`opened change <id>`,
`integrated <id> at <sha>`, `closed change <id>`); a failure to write the
journal is a warning, never a command failure. Finally, `journal open`
annotates any item whose topic matches an open change slug, or that a change's
`journal_ref` points at, with `[change <id>: <stage|state>]`, using the
in-repo ledger; outside a repository the annotation is skipped silently.

Memory artifacts are shared, always-surfaced project facts, one per file with
a heading that describes the fact. Retire them with `journal consume`; list
live memories with `journal memories`, and `catchup` leads with them after
lanes.

Advisory **lanes** announce which topics a session is actively working so
parallel sessions sharing one archive see occupancy instead of guessing:
`journal lane open <topic> [--scope t1,t2] [--ttl 2h] [--status <text>]`,
`renew`, `close [--outcome done|handoff|abandoned|expired]`, and `list`.
Lanes are typed journal events, never locks. Malformed JSONL lines are skipped
so readers fail open. Liveness needs no heartbeats: any journal event by the
owning session restarts the ttl window; idle lanes
decay to stale, and anyone may close a stale lane `[expired]` — the
explicit takeover path. Opening a lane implicitly closes the session's
previous one (a session has at most one). `journal open` annotates items
covered by a live lane, distinguishing this-session from external
occupancy, and `catchup` leads with the lanes block so a newly opened
session sees who else is here first.

Long free text can come from a file (or stdin with `-`) instead of shell
quoting: `journal lane open|renew --status-file`, `stage --note-file`, and
`hold --reason-file` are mutually exclusive with their inline counterparts.

The artifact kinds `done`, `inbox`, and `spec` are permanently retired:
`journal note --kind` refuses them, while historical artifacts keep parsing and
listing. `journal doctor [--json]` performs a read-only health check. It reports
retired kinds as non-failing `retired-artifact-kind` advice; malformed journal
lines, artifact names, unknown kinds, and dangling references remain problems
with a failing exit status, while stale lanes and archive housekeeping remain
non-failing advice. It never creates the journal directory.

`arc doctor [--json]` performs the same read-only split for the authoritative
change ledger. Malformed events, store configuration, IDs, and missing open
events are problems; orphaned temporary files and retention refs, missing open
branches, long-expired claims, dependency cycles, and future event types are
non-failing advice. Human output names each affected path or ref; JSON uses the
versioned `arc-doctor/1` report. The exit status is 0 for a clean or advice-only
ledger and 1 when problems are present.

## Roadmap

Shipped: local core +
policy engine (M1–M2), agent-facing orientation surfaces (M3), deterministic export/import
bundles (M4), local orchestration foundations (dependencies, tags,
actionable status, query, and batch views), claims/stages and execution
roles, messaging and the inbox rollup, project-journal mechanics, and the
ledger side of forge projection (declare/link/checks/pr-state with
fail-closed validation). Remaining evidence-driven possibilities live in the
journal's open items (`arc journal open`).

## License

[Unlicense](UNLICENSE) — public domain.
