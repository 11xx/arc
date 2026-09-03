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
stays derived. `--items` names every actionable artifact under each project in
the same open, later, and feature-request tier order used by `journal open`.
Each item can include its `verification` stamp, and the text rows use the same
renderer as `journal open`. JSON is versioned `arc-workspace-backlog/11` and
states whether its scope is global or beneath one canonical path. Missing
anchors are filtered by their recorded path, so an unreachable project inside
a requested workspace remains visible without unrelated orphans leaking in.
Its top-level `summary` totals the project rows, ledger queues, journal tiers,
and unreachable journals, and the text view prints those totals before project
detail. Temporary and scratch anchors are collapsed in the default text view;
`--unreachable` expands every maintenance row, while JSON always retains the
complete structured list.

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
who implemented it; an obligation declared before the kind was recorded carries
no `missing` and reads as `unversioned`, which is independent-review debt that
cannot be filtered by what it owes. The debt count is split by kind alongside
the total, because one number over every obligation says how many exist and
nothing about what any of them owes.

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

`shared_surfaces` names each path more than one outstanding obligation
changed, with the changes that changed it. Debt is recorded per change, so a
file several obligations carry is invisible from any one of them: reviewing
the change that finally touches it reads only the newest of the readings
nobody has done. The text view names the most-carried paths and counts the
rest; `--json` carries them all. A debt whose recorded range cannot be read
carries `surfaces: null` and is excluded from exact-path correlation without
being presented as known to touch nothing.

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

Move one change's complete ledger as a deterministic `arc-bundle/2`
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

