# Changes, patchsets, and briefs

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
  goal-scoped analysis briefs stay in the project journal. A brief's prose
  never gates checking or integration, but the acceptance probes declared on
  one do — see [acceptance probes](workspace.md#acceptance-probes).
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
  did not run the command. Passing evidence may also name the failure it
  answers, which is what distinguishes a gate shown able to fail from one that
  has only ever passed. Evidence binds to the tree the command ran against,
  not to the commit that happened to carry it: two commits with one tree are
  one evaluation, and a merge produces a tree neither side committed. Evidence
  written before arc recorded the tree resolves it from the revision it names.
- **A change behind its target must evaluate the merge, not its own head.**
  A change that is textually clean against a target that has moved still ships
  content nothing has run against — the two sides are each correct and their
  merge is not. `check` refuses with `merged-tree-unevaluated` and
  names `arc verify --against <branch>`, which synthesizes that merge, pins it
  so the evidence cannot outlive what it cites, runs every required gate in a
  scratch checkout it removes whatever the gates do, and records the result at
  the merged tree beside the target head it was computed from. Adding
  `--skip-green` reuses a gate already green at that merged tree instead of
  running it again, keyed exactly as readiness keys it. That evidence is
  spent as soon as the target moves again, exactly as a verdict is spent by a
  new commit. A head already on the target tip merges to the tree it already
  has and needs nothing new: a rebase that moves the base without changing the
  diff lands on the evaluated tree, and the gate reads `inherited from
  <revision>` rather than running again. `needs-rebase` keeps its narrower
  meaning: the
  text conflicts, so there is no single merged tree to evaluate at all.
- **`arc rebase [<change>] [--verify]`** is the recovery `needs-rebase` names.
  It replays the change's branch onto its target in the worktree that holds it,
  refusing first when that worktree is dirty or already mid-rebase, and saying
  which. A replay that succeeds records the new head as a patchset through the
  same path `arc snapshot` uses and names the gates the head still owes;
  `--verify` runs them the way `arc done` does. A branch already sitting on its
  target says so and records nothing. A conflict stops the rebase and leaves it
  in progress — the partial resolution belongs to whoever is doing it — prints
  the conflicting files and the commands that finish the replay, and exits
  `11`, the same code the blocker carries, because the change still needs
  rebasing. arc never runs `git rebase --abort`.
- **Snapshots record Git author and committer identity.** When a claim is live,
  the snapshot also records its generation and actor at snapshot time. Projects
  using per-actor Git identities report a mismatch as provenance evidence,
  never as an integration blocker; projects using one shared committing
  identity can explicitly disable that inapplicable comparison.
- **`arc integrate`** performs the merge only when, atomically checked:
  the head equals the approved patchset head, no blocking finding is
  open, every required gate is green at that exact head, and no hold is
  active. Holds are independent: `arc hold` prints the event that identifies the hold it set, `arc
  release-hold <change> <hold>` lifts exactly that one (a unique prefix is
  enough; an empty or ambiguous one is refused), and every other hold stays in
  force. A release naming a hold that is no longer active replays as a no-op
  rather than a failure — two collaborators may reach the same conclusion, and
  a ledger nobody can reduce is a ledger nobody can repair. `arc doctor`
  reports it as `hold-release-names-no-hold`.

  It merges the approved SHA (not the branch name) with `--no-ff`, then
  verifies the merge commit's parents and that its tree is the one the gates
  were evaluated against, undoing the merge otherwise: parents say which
  commits were merged, and only the tree says what ships. It never runs a
  gate. It records a `change-integrated` event
  carrying the patchset and head that were merged, the branch merged into, and
  where that branch stood first. A merge arc did not perform is `arc close
  --assert-integrated <rev> [--patchset <ps>] [--into <branch>]`, which writes
  `integration-asserted` — the same facts, deliberately without authorization,
  because arc did not guard that merge and cannot claim it was authorized. The
  assertion is still checked against Git: the target branch must exist, the
  revision must contain the patchset head, and it must be on that branch — and
  the change must have a recorded patchset, because otherwise nothing says what
  was integrated. Where the target stood before is read from a merge commit's
  first parent; a fast-forward has no such parent — its first parent is the
  change's own previous commit — so `--target-before <rev>` names it, and
  without one the event records no base rather than a wrong one. On a merge
  the flag is refused: Git is the better witness, and overriding it would
  record a range the merge did not integrate. A bundle carries the store format
  it was written with. arc refuses a bundle written by a newer arc rather than
  skipping lifecycle events it does not know. Skipping them would read a closed
  change as open. The same check runs
  on every path that opens a store, not only the one that creates it, and a
  ledger created by an older arc is stamped the moment something writes an
  event that older arc would skip — not on every open, so a build that merely
  read a ledger never locks its owner out of it. `arc
  status` reports which happened as `closure.integration`: `guarded`,
  `asserted`, or `legacy-unclassified` for closures written before the two
  could be told apart. `change-closed` carries only `abandoned` and
  `superseded`. A guarded merge also
  records the **authorization basis** it was taken on: the approving verdict
  event, one passing verification event per resolved required gate, each
  prerequisite's closure, the blocking-finding and hold vectors that had to be
  empty for the event to be written, and the normalized gate and policy values
  actually consumed — including uncommitted ones, which Git cannot recover for
  an auditor afterwards. This is not a config store. arc records no
  configuration history, only the inputs to one irreversible decision.
  It also records the debt declaration when that waiver is what let the
  approval stand, because then it is an authorization input like the verdict
  itself — and not when one was declared beside an approval that needed no
  waiver, where it authorized nothing. A
  gate is green for the declaration it ran — command and timeout both, since a
  run under a laxer timeout is not evidence for a stricter one. Editing a
  declaration after its evidence was recorded means the declared check has not
  run, so the gate reports `declaration_changed` rather than counting, and `verify --skip-green` reruns
  it instead of reusing a run of something else. Before merging, readiness is
  recomputed and the basis rebuilt; if the two differ, nothing is written.
  `arc integrate --dry-run` prints the basis it would record, when the merge
  would happen at all. `arc integrate <a> <b> <c>` and `arc integrate --tag '#series'` apply that same
  guarded path to a queue, in dependency order, stopping at the first member that needs a person.
  Refusals carry typed exit codes.

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
`arc inbox --json` uses `arc-inbox/8`; a change opened with `--iterating` is
classified in the separate `iterating` bucket rather than in review or ready
queues. Both `arc inbox` and `arc catchup` also carry a `deferred` section:
the findings delegated rounds left open, each with the subject and round that
deferred it, the reason, and its age. A deferral binds to a run's subject
rather than to a change, so it is the one queue entry that can name a fork or
a commit range.

## Opening a change

`--profile` selects the change's workflow (`direct`, `local`, `forge`, or
`release`). Add `--iterating` when integration is not yet the goal; `arc check`
then reports the typed `iterating` blocker instead of requesting a review, and
`arc iterating <change> --off` clears the declaration when the change is ready
to return to the integration path.

For a one-checkout change, `arc begin <slug> --no-worktree` checks the new
branch out in the invoking checkout when that checkout is clean and already on
the target branch, and records it as the change worktree. A dirty checkout or
one on another branch is left unchanged; the opening still succeeds and prints
the next Git command.

## Landing a queue

A queue repairs what needs no judgement and defers what does. Per member it
replays a branch whose target moved under it, runs the required gates that
have no answer at the tree the merge would ship, and merges. It stops at the
first member that needs a person — a conflict left in progress with the
commands that finish it, a gate that failed, a patchset nothing approves —
exits with that member's own blocker code, and prints a summary naming what
landed with its merge revision, what stopped the run and why, and what was
never attempted. `--dry-run` reports the same plan without replaying, running,
or merging anything. `--debt` stays per-change: the reason it records is a
judgment about one patchset — what review it owes and why that review could
not run — and one string spread over every member would bind to nothing in
particular. A queue is for changes that are already green or already carry
their verdict.

## Context awareness

Inside an open change's recorded worktree, `show`, `status`, `check`,
`snapshot`, `stage`, `claim`, `release-claim`, `verify`, `brief`, `comment`,
`finding`, `reply`, `resolve`, `review`, `done`, `blocker-status`, and `is-blocked`
infer an omitted change from the current branch, then the recorded worktree
path. An explicit change always wins. Commands that integrate, close, hold,
export, or otherwise act destructively or across changes still require an
explicit change. Ambiguous or absent context fails with the candidate list and
asks for `CHANGE`.

When `--no-worktree` opens a change from a clean checkout already on its target,
that checkout is recorded and the new branch supplies the first inference path
immediately. If the checkout is dirty or stands on another branch, no worktree
is recorded and the checkout remains unchanged.

`arc changelog <change>` reads the latest entry for one change; adding
`--category <category> --body-file <file>` records replacement release copy as
a new append-only event that names the entry it replaces, and says so when it
does. One entry per change still projects — the edge makes the replacement
inspectable rather than different. Categories are non-empty single-line
strings; their case and spelling are preserved. `arc changelog` projects entries from
integrated changes newer than the latest reachable tag. The built-in renderer
groups conventional categories in Keep a Changelog order, case-insensitively,
then emits other categories verbatim in bytewise order. `--since <revision>`
overrides the boundary, `--json` emits the versioned derived projection, and
`--write` replaces only the generated `[Unreleased]` block in `CHANGELOG.md`.
The human-readable renderer turns bare entries into list items, wraps every
entry at 75 columns including its marker, indents continuation lines under the
text the marker introduces, preserves paragraph breaks, and separates entries
with blank lines. A body that already leads with a list marker keeps the
markers and nesting its author chose — only the bullet arc would otherwise
have added is withheld — and whitespace-free tokens longer than the width
overflow rather than split. Wrapping is a rendering decision alone: the
recorded entry keeps exactly the text it was written with.
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
this order: `CLAUDE_SESSION_ID`, `CLAUDE_CODE_SESSION_ID` (the name Claude
Code itself exports), `CODEX_THREAD_ID`, `OPENCODE_SESSION`, `PI_SESSION_ID`.
When the harness's own session store yields the model, it appends an
`ARC_MODEL` export (`model-slug[#effort]`): Claude reads the newest
assistant model from its project transcript; Codex honors `CODEX_HOME` and
reads the latest turn's model and effort; OpenCode reads the selected model and
variant from its SQLite session row when `sqlite3` is available; and Pi reads
model and thinking-level changes from its JSONL session. Failure while reading
a session store is a silent omission, never an error. OpenCode v2 is recognized
without a session when `OPENCODE_TERMINAL` or process ancestry identifies it;
it prints `ARC_HARNESS` plus a commented `ARC_SESSION` and succeeds. `arc env`
exits 1 and prints the export template when no harness is detected. That is
the normal path for setting identity by hand rather than a failure. It does not
inject identity into other commands.

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
`--json` emits the versioned `arc-rescue/2` object.
`--transcript [--tail N]` includes the claimed session's latest operator turns;
it tries `tapes` when that CLI is installed and falls back to arc's own readers
otherwise. `tapes` is what covers OpenCode sessions, while arc continues to
work without `tapes` for its native readers. Arc names the reader that supplied
the turns, prints what the transcript contains, and performs no redaction, so
the option is opt-in and its output should be treated as sensitive.

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
single diagnostic naming the condition that was reached. `arc watch` exits 0
when its condition is reached and 2 on a timeout. `snapshot` waits for a patchset, `reviewed` waits for any verdict on
the latest patchset, and `approved` waits for the latest verdict on that
patchset to approve, including a provisional approval. `gates-green` waits for
every required gate to be green at head, `ready` matches `arc check` success,
`stalled` reaches only when a live claim is stale, `blocked` waits for a
`blocked-on` claim stage, and `brief-recorded` waits for a recorded brief.
`integrated` requires that closure outcome, and `closed` accepts integrated,
abandoned, or superseded changes.

`events --tag` follows a whole tagged program as one stream rather than one
follower per member, which is what keeps the interleaving readable. Membership
is what it is while the stream is being read: a change that acquires the tag
joins it. `--change` and `--tag` select different scopes and cannot be combined.

`watch --json` emits one object instead of prose, naming the change, the
condition, and the event that satisfied it — `event_id` is present only when an
event did, because `ready`, `gates-green`, and `stalled` are derived from
policy, head evidence, and elapsed time rather than from one originating event.
Approving verdicts also carry `provisional` when the approval records a reason
it is owed corroboration. A timeout emits `watch-timeout` in that object, so a
script branches on the object rather than on the text. Under `--tag --all`
the object carries one entry per member and no top-level placeholders.

`arc doctor` reports a `dangling-revision` for any revision the ledger records
that Git can no longer resolve — patchset heads and bases, brief bases, merge
bases, verification and verification-run revisions, the target a merged tree
was evaluated against, falsification anchors, audit revisions, the dirty-tree
waiver, disposition commits, the integration commit, its source head and
target-before, prerequisite closures, and the forge's recorded heads. A history
rewrite leaves the ledger intact and its evidence unreachable, which is advice
rather than a problem: the ledger is not malformed and the rewrite was
deliberate, but it is the difference between evidence and a claim. Where a
recorded rewrite claims to have moved a revision to a commit that is also
missing, following it forward answers nothing, and that is a problem —
`unresolved-revision`.

`--since <event-id>` treats a valid ULID as an exclusive lower bound and
composes with replay, follow, change, and type filters. `events --exec <cmd>`
runs `sh -c <cmd>` once per emitted NDJSON line; handler failures are warnings
and do not stop the stream. `watch --exec <cmd>` runs once only when a watch
condition is reached, with a JSON diagnostic containing the winning
`condition` on stdin. Both hooks receive `ARC_EVENT_ID`, `ARC_EVENT_TYPE`, and
`ARC_CHANGE_ID`; watch hooks use `watch-reached` as the event type. Comma-separated
watch conditions are checked in their supplied order and the first reached
condition wins.

`arc status <change>` prints the versioned `arc-status/16` JSON report —
the contract orchestrating agents program against. It includes dependency
state, inverse `blocks` links, tags, claim owner/activity/stage timing, snapshot
provenance, a blocker summary, a machine-readable `next_action`, an additive
`forge` projection block ([forge projection](forge.md)), and ready
alternative open changes while the requested change is blocked. Actively
claimed non-stale changes and held changes are never suggested; stale and
expired claims reappear. `--json` is accepted and changes nothing: status is
always JSON. `arc show <change>` renders the same actionable state as
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
`check` exit code is the first blocker's, which `--explain` never changes.
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
arc claim radio-refill-fix --takeover --because harness-status-absent
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
`--takeover --because <reason>` displaces one of those too. arc records the
displaced owner and the reason a claim was taken over. It prints them wherever
the displacement is rendered and verifies none of it, since the evidence that a
holder is gone —
`harness-status-absent`, `delegate-exit:<handle>` — is observed outside the
ledger. Any text is accepted, and `--because` without `--takeover` is refused.
Every claim, release,
and stage event carries its generation (snapshots carry the one observed at
snapshot time), so imported stale events cannot clear, advance, or claim
provenance for a replacement lease; a claim event without a generation is
rejected as malformed rather than replayed through inference.
Integration warns on an active foreign claim, including a stale one, but
proceeds when the normal integration gates pass.

