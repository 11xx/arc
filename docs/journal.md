# The project journal

## Project journal mechanics

> **The ledger is authoritative and gating; the journal is advisory and
> contextual.**

`arc journal` encodes the drift-prone mechanics of the cross-harness project
journal while artifacts stay plain Markdown any tool-less agent can read and
write. The canonical agent-written event log is the append-only
`events.jsonl`, whose versioned `journal-events/1` events can be streamed as
NDJSON with `journal events [--limit N]`. `dir` prints the resolved journal
directory for the project you are standing in (`ARC_JOURNAL_DIR`, then the
longest matching absolute path prefix in `[journals.dirs]`, then Git identity
and `<ai_home>/journals/<repo-slug>`, then a recorded anchor in the default
journal root, then that root's directory for this path's own slug) — the
authoritative location, not merely where arc defaults to creating one. `arc
config` reports the same resolution as `journal_dir_for_cwd`, so a caller
collecting paths asks one command. `dir
--explain` prints the selected source, stable anchor, and directory;
resolution refuses to guess when no source matches, and refuses ambiguous
recorded bindings. Read-only journal commands also run from inside the
journal directory itself: a directory whose binding records a project
resolves to that project's checkout and answers exactly as it would there,
while a write refuses and names the checkout, because a write records that
checkout's Git state. A journal directory that records no project says so
instead of listing the resolution sources it tried. `note`
writes a timestamped `<ts>-<topic>-<kind>.md` artifact and its journal event;
`log` appends a log-only event; `journal position` adds a position to an
existing artifact; `journal verified <filename> [--note <text>]` records that
an open artifact was checked against the source at the project's anchor head;
`catchup` lists newest-first.
`list [--kind <k>] [--json]` enumerates every live artifact in the hot
directory newest-first —
including the non-actionable kinds `open` and `memories` do not show —
marking each consumed item with its outcome. Read-side `--kind` filters on
`list` and `open` accept retired kinds so historical artifacts remain
discoverable, while `journal note --kind` accepts only active kinds.
`show <filename>` prints one
artifact's raw Markdown body, resolving the hot directory first and then the
cold archive. `latest <topic> [--kind <k>]` resolves one topic's newest
artifact, hot before cold, breaking a tie inside one timestamp second by the
transition relation and then by the order the event log recorded the
artifacts; a numeric filename suffix is never an ordering of its own. Dated
inline headings (positions, conclusions, review stamps)
take the house timestamp from `journal stamp` — RFC 3339 seconds in UTC, the
same spelling as the event log's `ts`, so prose and log cross-grep — never
an agent-authored date. Artifact filenames carry a `YYYYMMDDTHHMMSSZ` UTC
stamp, read by one parser that also accepts the legacy form without the `Z`;
a filename whose stamp parses as neither keeps its row visible with a null
time rather than being dropped or dated from its digits.
Plan artifacts may carry repeatable `planned-by: <JSON object>` lines directly
after their first title. `--planned-by` supplies those lines explicitly and
`--no-planner` records that authorship is intentionally unknown; an ordinary
new plan captures its declared invoker coordinates. `journal show --json`
reports the planner list and whether body and event metadata agree. A brief
selected with `--plan-ref` and `--plan-slice` stores the complete plan digest,
source location, and planner snapshot, so later plan edits do not rewrite an
existing brief.
Planner headers travel with ordinary plan creation, explicit copies, and
transitions. A plan transition accepts `--planned-by` for additional authors
and `--no-planner` for intentional unknown authorship. Represented humans
receive only their declared coordinates. `journal show --json` exposes header
and event sources independently; conflicts and malformed headers provide no
credit. `journal correct FILE --target artifact --field planners --value
'[{"actor":"author"}]' --note REASON` supplies an append-only correction.
Malformed metadata warns when selecting a brief; the brief still records its
digest and metadata status. Text briefs display the captured digest, planners,
and deduplicated `Assisted-by` suggestions when harness and model are known.
`archive --unresolved --note <reason>` shelves a discussion without consuming
it, and `unarchive <filename>` restores cold storage without reopening claims
or changing terminal resolution. Shelved discussions stay out of active work
until restored; terminal artifacts remain terminal in either store.
Storage moves record an operation ID, source, destination, and body digest.
The destination is created exclusively before the source link is removed;
retry validates the recorded paths and bytes and completes the same operation.
Pending operations block competing writes and appear in `journal doctor`.
Unclassified cold discussions require explicit `archive --unresolved --note`
adoption before accepting positions. Duplicate bodies are reported as conflicts.
Selected artifacts move to a paired replica as a versioned journal bundle:
`journal export <file>... --output <file>` writes it and `journal import
<file> [--dry-run]` files it into the receiving journal. Bodies and their
events travel together, an artifact the destination already holds at a
different digest refuses the whole bundle, and the first import records a
receipt naming the source replica and bundle digest. `docs/replicas.md` holds
the replica contract.
`journal catchup --archived --json` uses `arc-journal-catchup/8` and marks each row's
storage, resolution, resolution basis, latest position, and whether archived
positions exist; unknown legacy facts remain explicit rather than becoming
empty values.
`journal inventory [FILE] [--archived] [--kind KIND] --json` emits
`arc-journal-inventory/5`. It selects one hot or cold store from one observed
set of events, keeps availability separate from storage and resolution, and
reports claims, questions, exact change/brief promotions, source references,
and whether the ledger was absent, readable, or unreadable. A
`patchset_citations` map names, per selected filename, the patchsets that
recorded it among the links that framed them, with the digest recorded at
link time. An exact file is
inspected even when terminal; a missing exact file is an error.
Inventory, `journal open`, journal catchup, and workspace item rows share the
artifact projection. Rows include tier, checkpoint tips, current and historical
blocker references, unanswered questions and complete question history, and
exact promotions with the selected plan snapshot and change closure/stage. A
plan whose every promotion has closed carries `promotion_state: closed` and
stays in the queue: the work it drove is over, while consuming the plan
remains a decision. `integrate` and `close` name that command when they close
a `journal_ref` plan's last open promotion.
Unreadable ledger coverage produces `promotions: null` with an explanation;
an absent ledger produces an empty array. Body read failures and conflicting
stores are errors. These observations write no ledger or journal state.
Work waiting for a future session uses the primary actionable kinds — `todo`,
`handoff`, `plan`, `discussion` — plus lower-priority `later` and
`feature-request`. A feature request describes a wanted capability without
assigning execution priority. A `decision` records a settled question as a
terminal, non-actionable artifact. An `incident` records something that went
wrong in the running of work rather than in the code — a quota exhaustion, a
deadlocked tool, an executor blocked on a false premise. It is a record rather
than work, so it is deliberately not an actionable tier: it exists to be
counted and cited, and a decision whose revisit trigger reads "when X happens"
can fire on evidence instead of never. An incident that implies work produces a
separate artifact of the kind that work actually is.
`journal handoff <topic> --derive` prepends a `## State (derived at <ts>)`
section above the body, read from the repository at the moment of writing: the
repository and worktree paths, the current branch and short head, the change's
target branch with commits ahead and behind it, whether the tree is clean, the
inferred change with its stage and claim standing, how many actionable items
the queue carries, and the installed build beside the repository head. A fact
that cannot be read renders as `unknown` rather than being dropped, since an
absent line cannot be told apart from a fact nobody recorded. The section is
Markdown to be scanned, one fact per line, and nothing in it is meant to be
parsed back. `--derive` is refused outside a Git repository, and refuses before
the body is read so `--body-file -` never waits on stdin for a write that
cannot happen. It never satisfies the emptiness check on its own: the
mechanical half is the cheap half, and what a successor cannot derive — what
was learned, what is open, the next action — is what the author is there to
write.
Every artifact opens on a heading, because a queue row reads its description
from the first one and a row that names nothing is a row nobody picks up.
`--title <t>` writes it; a body that already opens on a heading keeps its own;
a body that opens on prose is headed from its topic slug, announced on the
diagnostic stream so stdout stays the artifact path. `journal correct --field
title` amends a heading afterwards.

`journal open` lists the primary queue first,
then separate later and feature-request sections, annotating each item with how
long it has waited (for discussions, since the latest typed position; otherwise
since creation). An item stays on the queue until an explicit
`journal consume` retires it.

Consuming a discussion reports the two facts `journal discussion` renders that
say a decision was never tested: every position came from one participant, and
nobody answered the last position. They are warnings — a one-participant
discussion is a legitimate way to settle something — and `discarded` earns them
as much as `done`. Both are derived once and shared, so the warning and the
view cannot disagree.

`journal consume <filename> [--outcome done|superseded|discarded]
[--decision <decision-file>] [--note <text>]` retires an item with a typed
consumed event. A `verified` event records the checked anchor revision in
`verified_revision` when the anchor has a Git head; an unborn or headless
anchor still records the check without a revision. Open rows and
`workspace backlog --items` name the checked revision, its age, and who
checked it, and compare it with the current anchor. A repository with no Arc
ledger uses an empty rewrite map, so matching and moved anchors remain
distinguishable without creating ledger data; an unreadable ledger or rewrite
map leaves the movement comparison unknown. `--json` carries the whole stamp,
including the session. A decision link is valid only with outcome `done`.
`<artifact>.md` names a local decision; `<project>::<artifact>.md` names one in
another registered project's journal, matched against the registry by slug or
label, and refused rather than guessed when the prefix is ambiguous. A
conclusion resolves any non-discussion actionable artifact, since a conclusion
is what finished work leaves behind; a discussion still requires a decision.
The consumed event records the referenced artifact's project, kind, and a
digest of its bytes, so a later reader can tell a resolution that still says
what it said from one rewritten underneath. The journal is append-only;
consumption never edits or deletes the artifact.

### Items distilled from a recorded session

An item read out of a recorded session carries where it came from. Every write
verb and `journal log` accept `--source '<spec>'` and `--item-key <key>`, where
the spec is space-separated `key=value` pairs: `harness`, `session`, and an
RFC 3339 `ts` are required, and `turn` (a whole-number ordinal), `schema`, and
`coverage` are recorded only when an emitter supplies them — a turn reference
is never synthesized from a timestamp that happens to be present. Any other
field, a duplicate one, an empty value, or an unparseable `ts` is refused
before anything is written. The event envelope carries the reference as
`source` with `item_key` beside it; both are optional, so every event written
without them stays valid `journal-events/1` input, while a key with no source
to index into or a coordinate that resolves to nothing is reported by
`journal doctor`. No transcript text, credential, or content digest enters the
journal. arc never opens the recording to infer a project or a body. A
recorded session directory is evidence, never authority to write there.

The reference is what makes a repeated scan of the same endings cheap.
Identity is the recording and the caller's key together — `(harness, session,
item_key)` — so one recording carries as many items as the caller gives it
keys, and the same item read out of a different session is a different item.
A repeat of a recorded source item writes nothing, prints the existing entry,
and exits 0. The entry prints as `existing: <artifact or event stamp>
[<disposition>]`. The
check runs before the body is read, so a repeat neither blocks on a stdin body
nor needs one. Nothing deduplicates by title or prose similarity.

A disposition is one of `open`, `no-action`, or `consumed:<outcome>`, where the
outcome is whichever one consumption recorded — `done`, `superseded`, or
`discarded`. Every terminal artifact state spells the same way, so a scan
separates work still waiting from work already disposed of by the prefix alone,
and reads the outcome only when it cares which disposal it was.
`journal log <topic> "<message>"
--source .. --item-key .. --outcome no-action` records a judgment that an item
needs no work at all; `no-action` is the only outcome a log line may carry,
because every other disposition belongs to an artifact, and recording it as a
log event answers the next scan without putting anything into a queue.

`journal source --harness <h> --session <id> [--item-key <k>] [--json]` lists
what one recording produced here: every item key, its disposition, the artifact
when one exists, and the events behind it. It is derived from the event log
rather than kept in a store of its own, which could disagree with it; JSON is
versioned `journal-source/1`. `journal source-attach <filename> --source
'<spec>' [--item-key <k>]` records a further recording as evidence behind an
existing artifact as an event with no body, since one item can be evidenced by
several sessions and an artifact that has been consumed still accepts one —
where a record came from stays worth knowing after the work it described is
discharged. `journal show` prints an artifact's sources on the diagnostic
stream so stdout remains the body verbatim, and `journal open --json` carries
them on the queue row as `sources`.

A `discussion` is the answer-owed actionable kind: an open debate rides the
same queue until someone resolves it, and `arc begin --from-journal` promotes
one straight into a change. `journal position <filename> [--ref <target>]
[--stance <for|against|amend>] --body-file <src>` adds one position: it writes a
`### Position pos-<ulid> (<model[#effort]> via <harness>, <utc-ts>)` block —
the heading tool-computed so its stable reply target and timestamp are never
hand-authored — below the file's existing content, and emits a typed `position`
journal event carrying the same ID and identity plus the optional `--ref` (a
position ID, legacy timestamp, or item slug it answers) and, when supplied,
the explicit stance.
When `--stance` is supplied, arc writes `Position: <stance>` above the body
and records it on the event; without the flag, a hand-written first body line
can provide the stance. Consumed artifacts reject late positions, questions,
and answers; re-litigation starts a successor discussion, while `correct` and
`retract` stay open on them. The Markdown is for people and block-scoped
stance parsing; the event supplies activity, identity,
resolver-participation, and reply edges. The file stays hand-writable, and the
append is advisory and fail-open like every journal write.
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

`journal question <filename> --placement opening|closing --option <a>
--option <b> --body-file <src>` poses a question the session should not settle
alone, and `--settle-by person|anyone|delegate --delegate <name>` records who
may settle it. `journal questions [--json]` is the queue of those still open.
Who may settle a question is not whether anybody was asked, so a question also
carries a delivery record: `journal delivered <filename> --question <id> --to
person|anyone|delegate:<name> [--handle <opaque>]` records a delivery the
caller already made. Arc keeps the fact and sends nothing — it holds no
messaging credentials and infers no delivery from prose — and `--handle`
stores whatever opaque identifier the delivery system returned, verbatim and
unresolved, so the delivery can be found again outside arc. arc refuses a
delivery naming an audience that cannot settle the question. It is refused
when the question is unknown, when it is already answered, and when the
question waits on one named delegate and the delivery names anyone else, since
that would record prompting done against an audience that cannot settle it.
Who may answer is changed by correcting or superseding the question.
Deliveries accumulate rather than replace, because a question asked twice and
still unanswered is more urgent than one asked once.

`journal suggest <filename> --question <id> --option <opt> --body-file <src>`
records advice for a decider without deciding: a typed `suggestion` event
carrying the suggesting identity and the reason. Suggestions show beside
their question in `journal questions` and `journal discussion` as
`suggested: <option> by <identity>`, and an `answer` that names a different
option warns that it departed from them. A suggestion never settles a
question and never counts toward the stance tally, and one naming an option
the question did not offer, or a question already answered, is refused. The
event kind is additive input under `journal-events/1`: a reader that does not
know it drops the advice and still reads the question, its answers, and its
tally unchanged.

Every question view reports the resulting state. `journal questions --json`
(schema `arc-journal-questions/3`) and the questions in `journal discussion
--json` carry `delivery` — `unasked` while prompting work remains, `delivered`
once somebody was asked and the reply is merely pending, `answered` once it is
settled, and `unknown` for a question older than the record itself — plus
`deliveries`, every recorded attempt with its recipient, handle, identity, and
timestamp. The text listing leads each question with the same state. arc never
claims a recipient read anything. It records that a caller reported sending
it.

The boundary that makes `unknown` meaningful is a typed `capability` event
naming a facility and the anchor head it began at, appended once per name by
the first write that can record the facility — posing a question as much as
delivering one. Marking it at the first delivery alone would leave a project
that has never delivered anything unable to say that nobody has been asked,
which is the state such a project is most often in. So a question this build
poses reads `unasked` immediately, and only a question predating the marker
reads `unknown`. The boundary is the marker's position in the append-only log
rather than its timestamp: stamps are second-granular and the marker is
written in the same second as the entry that needed it. `questions --json`
carries the marker, because the state cannot be interpreted without knowing
which questions predate the record.

An entry filed wrong is amended rather than rewritten. `journal correct
<filename> --target <target> --field <field> --value <value> [--note <text>]`
replaces one field of one entry, and `journal retract <filename> --target
<target> --body-file <src>` withdraws one with a required reason. `--target`
is `artifact`, a position ID, a question ID, or `answer:<question id>`; the
fields a target carries are closed — `title` on the artifact, `stance`,
`option`, `ref`, `actor`, and `model` on a position, `option`, `actor`, and
`model` on an answer, `actor` and `model` on a question — so a correction
naming a field its target lacks is refused rather than recorded where nothing
will read it. Retraction takes a position or an answer; an artifact is
withdrawn by consuming it and a question by answering it. Both append a
`### Correction cor-<ulid>` or `### Retraction ret-<ulid>` block naming the
target and the change, never rewriting an existing one, and both are accepted
on a consumed artifact, which is closed to new work but not to being wrong.

Provenance recorded wrong at creation is not an amendment. `journal
reattribute <filename> --set-actor <a> --set-harness <h> --set-session <s>
--set-model <m>` rewrites the authorship fields of the artifact's one
creation (`note`) event in place, each absent flag leaving the recorded field
alone and nothing inferred for a field nobody named. The operation is
deliberate maintenance rather than ordinary mutation: it validates the whole
log before writing, holds the journal transition and event-write locks across
the read and the replace, keeps the previous bytes as `events.jsonl.bak`, and
publishes the replacement by fsync and rename, so an interruption leaves either the old log
or the fully written new one. Every event append shares the event-write lock;
a writer either appends to the published log or fails with a busy-lock error.
Tools that ignore these locks cannot be serialized by Arc. `--dry-run` names
the record and every other record carrying the replaced identity and writes
nothing; those other records
are deliberately left for a separate decision rather than blanket-rewritten.
Every other event kind is refused by name, because position, answer, claim,
and checkpoint events carry contributions and continuations that provenance
repair must not rewrite, and so is a creation event recording an
`on-behalf-of` subject.

The effect lives in the derived views, and the latest correction of a given
target and field wins in event order. `journal discussion` counts the
corrected stance and branch, carries the corrected actor on each round
position and answer, labels participants by the corrected model, and follows
the corrected reply reference; `journal open` and `catchup` show a corrected
title while the artifact keeps the heading it was filed with. Prose headings
are unchanged throughout: a correction says who argued, never that somebody
else wrote the block. A
retracted position leaves the stance tally and the branches argued under a
question, and the summary lists it under `retracted:` with its reason; a
retracted answer leaves its question open, so `journal questions` lists it
again and one further settlement is accepted. `journal doctor` reports an
amendment naming an entry the artifact never recorded — it resolves to
nothing and every view ignores it — and advises on two corrections of one
field of one entry sharing a timestamp. Rows in the open queue carry
`[amended ×N]` in text and `amendments: N` in JSON for the positions standing
on a filed claim, so a claim amended three times reads differently from one
filed once; a discussion is argued by positions rather than amended by them
and carries no such count.

The journal and the ledger bridge in three advisory ways, none of which can
gate the authoritative ledger. `arc begin --from-journal <artifact>` opens a
change from an open actionable item and records the artifact filename as the
change's `journal_ref`. A plan reference leaves the plan open and seeds no
brief, so the same plan can father multiple changes until an explicit
`journal consume`. Every other actionable kind appends a `consumed` event with
outcome `superseded` and note `change <id>` (leaving the artifact file
untouched), then seeds an initial brief threaded from the source body so the
change starts with the resolution. A missing, non-actionable, or already
consumed source is refused before anything is written: arc refuses a
`--from-journal` source that is missing, non-actionable, or already consumed.
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
so readers fail open. A lane's owner is the declared harness and session
together: harnesses mint session strings independently, so the same string
under two harnesses is two owners, and lane writes require both halves.
Liveness needs no heartbeats: any journal event by the owning
harness and session restarts the ttl window; idle lanes
decay to stale, and anyone may close a stale lane `[expired]` — the
explicit takeover path. Opening a lane implicitly closes that owner's
previous one (an owner has at most one). `journal open` annotates items
covered by a live lane, distinguishing this-session from external
occupancy, and `catchup` leads with the lanes block so a newly opened
session sees who else is here first.

A **claim on a journal artifact** is the same object the ledger uses for a
change, with the artifact filename as its subject: `claim <file>.md [--ttl 2h]
[--takeover]`, `stage <file>.md <stage>`, and `release-claim <file>.md
[--outcome paused|abandoned|expired]`. Any of those commands takes an artifact
filename where it takes a change reference — a name ending in `.md` that exists
in the journal directory is an artifact, and a change ID cannot contain a dot.
Both subjects reduce to the same claim state and are timed by the same
function, so `active` and `expired` mean what they mean on a change. A change's
stages carry budgets and a stage over budget reads `stale`; an artifact has no
stages to budget, so its lease is the whole of what expires and `expired` is
when it becomes reclaimable. A live claim held by someone else is refused unless
`--takeover --because <reason>` states why its holder is gone; an expired one
is displaced with `--takeover` alone. Either way the displaced claim is
recorded, with the reason when one was given. A lane is occupancy of a topic and a claim is occupancy of a file: both
render on a row, and neither is ever rewritten into the other.

`journal checkpoint <file>.md --body-file - [--next <action>] [--gate <cmd>]
[--blocker <ref>] [--supersedes <checkpoint-id>]` appends a `### Work
checkpoint` block to the artifact and records a typed event carrying the
structured fields and a digest of exactly the bytes appended, so a reader can
tell a checkpoint that still says what it said from one rewritten underneath.
It requires a live claim held by this identity and is refused once the artifact
is consumed. Later checkpoints in one claim are ordinary progress and replace
that claim's earlier ones; `--supersedes` names a checkpoint to replace across
claims and repeats, so a takeover converges several continuations in one
checkpoint. Views report the tips that remain: more than one across claims is
a contest unless a `--supersedes` edge converges them, and `journal doctor`
reports exactly that.

`journal consume` and `journal transition` refuse while any claim on the
artifact is open, naming each, and proceed only when `--acknowledge-claim <id>`
names every one — a lease that has run out included, since the claim being dead
is a fact about the clock rather than its holder's consent. Those claims end
with `ended_by` naming the lifecycle event and no owner outcome, because how
somebody's work stopped is theirs to say. `begin --from-journal` closes the
invoker's own claim as `promoted` citing the change it opened, and ends every
other claim on the artifact with the change opening.

`journal open` rows append `[claimed by <actor> via <harness>: <state>]`, and
`--json` carries `availability` (`available`, `occupied`, `reclaimable`, or
`terminal`) beside the claim objects. Where nothing holds an artifact, the row
says whether that is knowable: one filed before the `artifact-claims`
capability marker reads `unknown`, and one filed after it with no claim
`never-started`. `catchup` lists open artifact claims beside the lanes with
each claim's next action; `rescue <file>.md [--take]` reports where the work
stopped and takes over a lease another identity left run out; and `watch
<file>.md --until stalled` waits for a lease to run out, the only condition an
artifact answers.

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

### Spooling a write the journal cannot take

The journal lives outside the repository, so a process sandboxed to the
repository it is editing cannot write one — and the executors running bounded
rounds are precisely the ones asked to record what they deferred. Every kind
verb and `journal log` therefore accept `--spool`, and a write also spools on
its own when the journal directory cannot be created or written. Either way
the write lands in `<repo toplevel>/.arc/outbox/<ts>-<kind>-<topic>.json`. A
spooled write prints `spooled: <path>` and exits 0. Arc creates the outbox on
first
use with a `.gitignore` of `*`: a spooled write is in-flight state, not
project content.

A spool file is `{"schema": "arc-journal-spool/1", "filename": <the artifact
this write would create, absent for a log>, "event": <the journal event as it
would have been appended>, "body": <the artifact body, null for a log>}`.

```sh
arc config --check-writable          # says whether writes go to the journal or the outbox
arc journal todo deferred-work --body-file - --spool
arc journal spool                    # what is waiting
arc journal spool --promote          # file it, oldest first
```

`--promote` replays each write as a real one: the artifact is written, the
event is appended with the spooled identity verbatim — actor, harness,
session, model, and represented subject — plus `promoted_by` naming the
promoting caller, and the spool file is removed only after the event is
appended. Promotion adds a carrier, never a new author. A spool file that
cannot be parsed is reported and left in place, because it is the only copy of
that write. `promoted_by` is an optional `journal-events/1` field, so an
events file written before it stays valid.

The outbox belongs to the checkout that holds it, and a change's worktree is
its own checkout, so a write an executor spools there disappears when the
worktree is removed. `arc snapshot` and `arc integrate` therefore promote the
spool in the change's recorded worktree before the merge that removes it,
printing what they filed; a spool neither can file is named, left intact, and
blocks neither the snapshot nor the merge. `arc catchup` lists every spool
still waiting in an open change's worktree with its count.

`arc doctor [--json]` performs the same read-only split for the authoritative
change ledger. Malformed events, store configuration, IDs, and missing open
events are problems; orphaned temporary files and retention refs, missing open
branches, long-expired claims, dependency cycles, and future event types are
non-failing advice. Human output names each affected path or ref; JSON uses the
versioned `arc-doctor/4` report. Both open with the roots the invocation reads
and writes and whether a sandbox is in force, because every finding is a
statement about state at those paths. `arc doctor` exits 1 when problems are
present and 0 for a clean or advice-only ledger.
