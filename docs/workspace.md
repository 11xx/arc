# Workspace, scaffolds, and cross-repo transfer

Three read-only conveniences for a lead working across repos and handoffs,
and the bundle that moves one change's ledger between repositories.

## Workspace

`arc workspace list|inbox|backlog [--json]` aggregates across registered
projects. `workspace backlog --here` restricts the report to canonical project
anchors beneath the current directory; `--under <path>` names another
directory, and `--global` states the default all-project scope explicitly.
The path boundary, rather than a project basename, keeps same-named projects
independently addressable. From a non-Git workspace root, start with
`workspace backlog --here`, enter one reported anchor, then use project-local
`catchup`.
Discovery has two modes and the configured one wins: with a `data_root` the
stores sit side by side and enumerate directly; without one they live inside
each repository's Git common dir, where the journal registry — one directory
per project, keyed by its anchor — is what knows they exist. A journal records
its anchor in `bindings.jsonl`; one written before that is reconstructed from
its directory name and confirmed against the filesystem, and a name that
resolves to no single existing path stays unresolved rather than guessed at.
When Git and configured path-prefix discovery do not apply, a journal in the
default root is reopened by its recorded anchor when one exactly matches the
canonical current directory — two such bindings are refused as ambiguous — and
otherwise by slugging that directory, which names the journal arc would itself
create there. A journal already carrying a binding for somebody else is never
taken by the slug: it has answered who owns it.
`arc begin` registers the project, so opening a change is enough to make a
repository discoverable even if nothing is ever written to its journal. A
`[journals] dirs` scope registers its project too, which is how a directory
that is not a Git repository takes part. Journal timestamps are read by one
parser over the canonical `YYYYMMDDTHHMMSSZ` stamp and the legacy form
without the `Z`; both mean UTC and both filter identically under `--since`.
RFC 3339 cutoffs may include fractional seconds, which are retained by the
normalized value in `selection.since` and by the detail command.
A filename whose stamp parses as neither stays visible with `filed_at: null`
and `timestamp_status: "invalid"`: it rides inside the tier it was filed
into, and under an active cutoff it is additionally counted as
`unknown_time_items` — per project and in the summary — because a delta that
silently dropped what it could not date would under-report. The `selection`
object states what the journal counts mean (`arrivals` under a cutoff,
`outstanding` without), the normalized cutoff in `since`, and whether a
cutoff is active at all.

A cold archive is identified structurally rather than by its name: `<x>-archive`
is skipped only when journal `<x>` is also present, so a project genuinely
called that is not lost.

`list` prints per-repo open-change rows; `inbox` concatenates each project's
inbox rollup, tagged with the project. `inbox` accepts the same `--under`,
`--here`, and `--global` scope as `backlog`, so the two projections select the
same set; `list` takes no scope. Both rollups open each store read-only, never
create one, and skip projects whose stores or changes cannot be read with a
warning. Healthy projects remain in the output. JSON is versioned
`arc-workspace/1`. The workspace inbox observes each project through a context standing in its own
anchor, so gate policy and live heads are the project's own and its buckets
answer what a per-project tour would.

`inventory [--storage hot|archived|all]` reconciles the workspace's stores.
Every artifact is a row keyed by project and filename — two files sharing a
topic are two rows — carrying the store it sits in, the resolution the events
record (absent when none does), the successor a transition named, its
promotions, and an explanation: `present`, `terminal`, `archived`, or
`superseded`. Nothing is classified from a missing row alone, so a completed
item and a shelved one are distinguished without assuming either, and a legacy
artifact whose resolution was never recorded stays unknown. It accepts the
same `--under`/`--here`/`--global` scope, reports the same collection manifest,
exits 16 on a partial collection, and is versioned
`arc-workspace-inventory/1`.

`backlog` answers what no single repository can: where work is blocked on a
decision rather than on effort. Per project it reports changes awaiting a
verdict, changes carrying audit debt, the journal's three tiers, and the
primary tier's oldest entry — a one-item queue never looks like a backlog from
inside its own project. Each project row also carries every open change with
the predicate buckets it satisfies, the per-kind debt split, and the
outstanding round deferrals, observed from that project's own checkout, so a
held-only change or an uncollected deferral keeps the project visible. Projects are ranked by the fact `--rank-by` names —
`blocking` (verdicts owed plus decisions waiting on a person) by default, or
`availability` (primary work ready to pick up) or `coverage` (review
obligations owed on shipped work) — and the report's `ordering` object states
the basis it used. Coverage debt is its own field, so a completed project
holding routine debt does not rank as waiting on a decision. Items are never
ranked against each other across projects, because arc records no priority that
spans repositories. The human report ends with a `detail:` footer naming the
command that re-runs the same report as itemized JSON — the resolved scope, the
normalized and shell-quoted `--since`, `--rank-by` when it is not the default,
and `--unreachable` exactly as the report used them, with values quoted for the
shell. The footer appears over an
empty scope too, and
never on `--json`, whose whole stdout must stay one parseable value. A project whose journal holds work but whose anchor no
longer resolves is reported under `unreachable` with the `journal rebind` that
adopts it; `list` and `inbox` cannot report it, so they name what they skipped
on stderr rather than dropping it silently, and say so plainly when no
project is registered at all rather than printing nothing: it is exactly the backlog no per-project command can reach, since
standing in the project is how every other view starts. `--since <stamp>` turns
the report into a delta, where the journal counts mean arrivals rather than
outstanding work; blocked work is still reported in full. arc stores no
previous-run marker — the boundary is supplied by the caller, so the command
stays derived. `--items` names every actionable artifact under each project in
the same open, later, and feature-request tier order used by `journal open`.
Each item can include its `verification` stamp, and the text rows use the same
renderer as `journal open`. Item rows share the journal inventory projection,
including exact promotions, question history, checkpoint tips, and explicit
ledger read coverage. JSON is versioned `arc-workspace-backlog/18` and
states whether its scope is global or beneath one canonical path. Missing
anchors are filtered by their recorded path, so an unreachable project inside
a requested workspace remains visible without unrelated orphans leaking in.
Its top-level `summary` totals the project rows, ledger queues, journal tiers,
and unreachable journals, and the text view prints those totals before project
detail. Temporary and scratch anchors are collapsed in the default text view;
`--unreachable` expands every maintenance row, while JSON always retains the
complete structured list. The `collection` manifest states the collection's
boundaries — discovered, selected, skipped, observed-empty, observed-with-facts,
and failed — with one entry per failed component, so an orphaned anchor or an
unreadable ledger or journal is a named failure rather than an empty project.
A journal at a vanished path that holds nothing and was never bound is counted
as empty.
Discovered equals selected plus skipped, and selected equals empty plus non-empty
plus failed. `arc workspace backlog` exits 16 when any selected project's
observation failed; the rows that were read still print.

Review rows carry the actor, model, harness, and session that recorded the
latest patchset, plus `on_behalf_of` when that actor represented another
subject; debt rows carry the same identity from the obligation event. `arc show
--json` retains opening identity as `opened_by`, `opened_on_behalf_of`,
`opened_model`, `opened_harness`, and `opened_session`, and patchset and debt
objects keep their own event identity. Missing values remain `null`: Arc never
guesses a native session that the harness did not declare.

The two ledger buckets carry the facts that decide what to do about them, so a
reader never re-derives them per change. A review entry names how many
patchsets exist, how many days the newest has waited, and the verdict a newer
patchset superseded — absent when the change has never been reviewed at all. A
debt entry names when it was declared, its age in days, who declared it, what
it says is missing, the coverage the shipped work did have, and who planned and
who implemented it. Every row also carries its effective kind and the basis it
came from: an obligation declared before the kind was recorded still reads as
`independent-review` debt — the meaning every reader gives the legacy shape —
with `missing_basis: "legacy-default"`, while a typed row reads `"recorded"`.
The debt count is split by effective kind alongside the total, so grouping
rows by `effective_missing` reproduces the summary split, and the summary
names the legacy subset as `legacy_debt_owed`, because one number over every
obligation says how many exist and nothing about what any of them owes.
Discharge behavior and recorded event bytes are unchanged: the projection
distinguishes what the report counts, not what the history says.

Only a change carrying a patchset can be answered by a verdict. An open change
with none is reported under `no_patchset`, and does not count as blocked: its
next step is work, not a person.

A review entry names two independent target-movement facts. `behind_target`
is the number of commits the target has taken since the latest patchset's
base, an integration-staleness measure. `target_path_overlap` is the sorted
set of paths changed by both that patchset and the target movement, a direct
file-overlap signal. A semantic conflict can cross different files and is
established only by evaluating the combined tree. Either report value is
`null` when its Git range cannot be read; the text view says `unknown` rather
than presenting a failed probe as zero or an empty set.

Per project the report also inventories active forks with the same read-only
projection `arc fork list` uses — slug, branch, ahead count, base — and sums
them in the summary. Forks are orientation, never obligation: they add nothing
to the blocked or decision score, a fork-only project still appears, an
unreadable ahead count stays null rather than zero, and retired forks remain
history. This visibility is independent of the journal-arrival cutoff, so an
active fork remains visible when its marker is older than `--since`. If the
fork inventory itself cannot be read, the report fails with the affected
project rather than treating the repository as having no forks. Every project
row names its `journal_dir`, the directory its questions and items were read
from.

`decision_questions`, `opening_question_count`, and `closing_question_count`
are all calculated over unanswered questions whose artifact is still open.
Questions on consumed, archived, or missing artifacts remain in
`open_questions` as unresolved records but do not inflate the active decision
breakdown.

The report is an observation, not a snapshot protocol: `observation` carries
when the pass started and finished, and `consistency: "sequential"` states how
it was built — projects read one after another in one pass. Arithmetic
agreement between project rows and the summary is checked over the emitted
rows; it cannot establish that the underlying state did not move mid-read, and
the report does not claim it did. A report writes nothing: no ledger event, no
journal file, no tracked tree change.

`shared_surfaces` names each path more than one outstanding obligation
changed, with the changes that changed it. Debt is recorded per change, so a
file several obligations carry is invisible from any one of them: reviewing
the change that finally touches it reads only the newest of the readings
nobody has done. The text view names the most-carried paths and counts the
rest; `--json` carries them all. A debt whose recorded range cannot be read
carries `surfaces: null` and is excluded from exact-path correlation without
being presented as known to touch nothing.

### Workspace report

`arc workspace report` uses the same project selection as `workspace
backlog --items`, including projects whose backlog is empty, reads each
project's ledger for how long its open changes have been open and which
closed changes left a worktree, and
classifies them by named rules, so a reader of any kind —
a person, a renderer, a model — gets the same answer from the same ledger. Its
JSON is versioned `arc-workspace-report/2`; the text view prints the tallies
and the attention list. The report writes nothing.

Every journal artifact takes one `status`, decided in this order:

- `delivered` — every promotion is closed and at least one closed
  integrated: the work landed and the artifact was never consumed.
- `abandoned-promotion` — every promotion is closed and none integrated.
- `claimed` — an active claim occupies it; `claimed_by` names the holder.
- otherwise the tier's own status: `unresolved` for the primary tier,
  `proposal` for feature requests, `parked` for later.

`unresolved` states that the journal records no resolution. It is not a claim
that work remains: shipped work whose artifact nobody consumed reads
`unresolved` until something records otherwise, unless its promotions say
`delivered`.

A row's `title` is the artifact's heading without its `#` marks, or its topic
with hyphens as spaces when the heading is absent or belongs to a scaffold
(`How to append a position`, `Positions`, `How it resolves`, `Questions only a
person settles`) or to a position block.

`sections` groups the rows: `needs_person` holds open questions whose
`settle_by` is `person`; `needs_agent` holds questions anyone or a named
delegate may settle, with that authority carried in `settle_by`. `in_flight`
holds one row per change, with all its `buckets`, distinct `next_actors`, and
days since it opened. A change with no patchset also carries `no-patchset`
in its buckets. Counts measure changes, not bucket memberships.
`review_owed` holds each debt with its kind and age;
`deferred` round deferrals; `work`, `proposals`, and `parked` the journal
tiers, ordered by project, then kind (handoff, plan, todo, discussion, other),
then filing time.

`attention` lists named facts that deserve a look, each `{rule, project,
subject, evidence}`:

- `delivered-unconsumed` — an artifact whose status is `delivered`.
- `stale-question` — a decision question open longer than 7 days.
- `stale-handoff` — a handoff unresolved longer than 14 days.
- `stale-claim` — a claim that lapsed and can be reclaimed.
- `stale-no-patchset` — a change open longer than 7 days with no patchset.
- `worktree-outlives-change` — a closed change whose separate worktree is
  still on disk; a change begun without a worktree records the main checkout
  and is never flagged for it.
- `debt-grew` — a project whose review owed rose against `--previous`.
- `collection-failed` — a component of a project that could not be read.
- `unreachable-anchor` — a registered project whose anchor is gone; anchors
  under the temporary directory, `/var/tmp`, or a `scratchpad` fold into one
  `unreachable-scratch` entry, since their disappearance is housekeeping.

With `--previous <file>`, an earlier report of the same schema and scope,
each tally carries its earlier value when both collections are complete.
A row says whether it is `new_since_previous` only when its project was
read successfully in both observations; otherwise the field is null.
Debt growth likewise requires two successful project observations.
`departed_since_previous` lists every artifact the earlier report held
that is no longer in the backlog, with the reason its journal records:
`consumed` with its outcome, `archived` (shelving records the outcome
`unresolved`), `superseded` with the successor, `unobserved` when its project
failed collection, and `unknown` when the journal cannot establish a reason.
An artifact still recorded as present has no established departure reason.
A departure is not a resolution. A file of another schema or scope is refused
as a baseline. Every failed component read, including the report's ledger
read, enters the collection manifest and makes the command exit 16. The
observation ends after the ledger and departure reads finish.

## Brief scaffolds

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

## Acceptance probes

`arc brief --probes-json` binds named acceptance commands to one brief
version, taking a JSON array inline, a path, or `-` for stdin. Probe names are
unique kebab-case slugs. Declaring a probe is `brief`'s job; `arc verify` only
records evidence for one. Run it explicitly with
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

## Audit ranges

`arc diff <change> --integrated` renders the exact range an integration
recorded — from where the target stood before to the commit that landed —
which is what an audit reviews; a patchset range describes the work instead.
It conflicts with `--patchset`, `--between`, `--since-approved`, and
`--findings`, whose anchors are a patchset question. A closure written before
arc recorded the range knows what landed but not what it landed onto, so it
requires `--base <rev>` rather than having one guessed; passing `--base` where
a range *was* recorded is refused, because the recorded range is the fact. A
change that was abandoned or superseded has no integration range and says so.

## Restack advice

`arc restack <change> --advise` prints, for each open dependent of a change,
the exact `git rebase --onto <target> <base>` command to run in that
dependent's worktree once the change integrates. It only prints and writes no
events: rewriting a dependent's branch is the operator's call, made in that
worktree. `arc restack --advise` exits 0 when the
change has no dependents. It says so on the way out.

## Export / import

Move one change's complete ledger as a deterministic `arc-bundle/4`
JSON file:

```sh
arc export radio-refill-fix --output change.json
arc import change.json --dry-run
arc import change.json
```

Use `-` instead of a path for stdout or stdin. Re-exporting unchanged
events is byte-identical, and importing the same bundle again skips
identical events. `arc import` exits 1 and writes nothing when an event
conflicts. Every event must carry a complete envelope that this
build can decode; an unrecognized payload tag is preserved verbatim and
excluded from typed replay. Missing Git commits are warnings rather than
data loss: available patchset heads are restored under
`refs/arc/keep/<change>/<patchset>`, while unavailable objects are
reported for separate transfer.

A long-lived exchange can carry only what the other side is missing.
`arc export <change> --since <checksum>` writes a bundle whose events are
the suffix after the prefix that checksum names, where the checksum is the
one an earlier export printed. The receiving store must already hold that
prefix, or the import refuses and writes nothing. Both halves of the claim
are checked before anything is written: the prefix's own checksum against
the events the store holds, and the checksum the bundle carries over that
prefix and the new events together. There is no kind-filtered partial
bundle, so a bundle is either a complete ledger or a contiguous suffix of
one whose prefix the receiver already holds.
