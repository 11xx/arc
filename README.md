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
  recorded with `arc snapshot`. Reviews and approvals bind to patchsets,
  never to moving branch names.
- A **brief** is a change-scoped implementation contract stored in the ledger;
  goal-scoped analysis briefs stay in the project journal, and briefs never
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
eval "$(arc env)"                         # detects this harness session explicitly
arc brief radio-refill-fix --body-file executor-spec.md
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
arc review radio-refill-fix --snapshot --verdict changes-requested --findings-json - <<'EOF'
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

## Context awareness

Inside an open change's recorded worktree, `show`, `status`, `check`,
`snapshot`, `stage`, `claim`, `release-claim`, `verify`, `brief`, `comment`,
`finding`, `reply`, `resolve`, `review`, `done`, `blocker-status`, and `is-blocked`
infer an omitted change from the current branch, then the recorded worktree
path. An explicit change always wins. Commands that integrate, close, hold,
export, or otherwise act destructively or across changes still require an
explicit change. Ambiguous or absent context fails with the candidate list and
asks for `CHANGE`.

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

Observe a change without scraping status views, or wait for one condition for
shell orchestration:

```sh
arc events --change radio-refill-fix --type patchset-added
arc events --since 01J00000000000000000000000
arc events --follow                     # replay, then stream raw NDJSON events
arc events --follow --exec './handle-event'
arc watch radio-refill-fix --until snapshot --timeout 30
arc watch radio-refill-fix --until ready,stalled --exec './notify'
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

`--since <event-id>` treats a valid ULID as an exclusive lower bound and
composes with replay, follow, change, and type filters. `events --exec <cmd>`
runs `sh -c <cmd>` once per emitted NDJSON line; handler failures are warnings
and do not stop the stream. `watch --exec <cmd>` runs once only when a watch
condition is reached, with a JSON diagnostic containing the winning
`condition` on stdin. Both hooks receive `ARC_EVENT_ID`, `ARC_EVENT_TYPE`, and
`ARC_CHANGE_ID`; watch hooks use `watch-reached` as the event type. Comma-separated
watch conditions are checked in their supplied order and the first reached
condition wins.

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
review latency, observed gate wall times, findings by severity, and patchset
count. The aggregate block adds median and p90 per stage and per gate, plus
`suggested_stage_budgets` — each stage's p90 rounded up to a clean duration,
offered for `--stage-budget` tuning and never applied automatically. Attested
gate evidence carries no observed duration and is excluded from gate timing.
JSON is versioned `arc-stats/1`.

Claims are advisory rather than merge locks:

```sh
arc claim radio-refill-fix \
  --stage-budget launch=60s --stage-budget implementing=30m
arc claim radio-refill-fix --takeover
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
arc metadata radio-refill-fix --priority 20
```

Fleet executors can atomically select and claim work instead of racing a
separate query and claim:

```sh
arc take --tag '#radio' --ttl 2h
```

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

[gates.integration]
command = "cargo test --test integration"
timeout = "10m"                              # optional; s, m, or h
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

`arc workspace list|inbox [--json]` aggregates every arc store under a
configured `data_root` (it errors otherwise, since per-repo git-common-dir
ledgers are not enumerable). `list` prints per-repo open-change rows; `inbox`
concatenates each repo's inbox rollup, tagged with the repo. The scan opens
each store read-only, never creates one, and skips unreadable entries with a
warning. JSON is versioned `arc-workspace/1`. The workspace inbox is
ledger-derived — it consults no per-repo working tree, so rebase and gate
buckets are not evaluated there.

`arc brief <change> --scaffold <name>` prepends a template to the brief being
recorded (`--scaffold` alone records the template). A repo-local
`.arc/templates/<name>.md` wins; otherwise a built-in applies (`sol-low`,
`sol-high`, `reviewer`). The built-ins encode the delegation fences — scope
ceiling, no tests beyond those listed, stop on a missing target, release the
claim on stop, never review or integrate — and the sandbox facts an
arc-driving executor needs (`.git` must be writable; stage and report
"staged, no SHA" when signing is unavailable so the lead commits then
snapshots then reviews; keep claim/stage heartbeats current).

`arc restack <change> --advise` prints, for each open dependent of a change,
the exact `git rebase --onto <target> <base>` command to run in that
dependent's worktree once the change integrates. It only prints — arc never
rewrites a branch — and writes no events; with no dependents it says so and
exits 0.

## Policy

Repository integration policy is declared in `.arc/policy.toml`. Policies are
disabled when the file or setting is absent. Set
`[policy] forbid_self_approval = true` to reject an approval when its actor
string exactly matches the actor recorded by `arc snapshot` for that patchset.
Actor identity remains advisory; this comparison does not redesign or verify
identity. Rejected self-approval follows the existing no-valid-approval path
and exits 3.

Optional reviewer reminders live in the same file under `[review]`, for
example `checklist = ["exercise the failure path"]`. `arc show` renders these
advisory items only for reviewer and lead roles; they never block integration.

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
executor is. Claim ownership is unaffected: it still matches on the invoker's
actor + harness + session tuple, and `on_behalf_of` is recorded and rendered
but never changes who owns or may release a claim. The field is additive and
serialized only when set, so existing events and bundles round-trip unchanged.

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
repository by its slugged main path (the project-journal convention:
`/home/x/code/y` → `-home-x-code-y`), so one root safely serves many
repositories — useful for sandboxing: point the paths somewhere
isolated and arc never writes outside them (worktrees, ledger) beyond
ordinary Git operations in the repository itself. `arc config` prints
the resolved paths as JSON.

Before starting an executor in a sandbox, run `arc config --check-writable`.
It probes the ledger root, lock, event-path, and Git-ref writes without adding
an event; `--json` emits `arc-writability/1` for automation and stops at the
first blocked path.

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
directory (`ARC_JOURNAL_DIR`, then the `[journals.dirs]` map in the config
file keyed by repository root, then `<ai_home>/journals/<repo-slug>`); `note`
writes a timestamped `<ts>-<topic>-<kind>.md` artifact and its journal event;
`log` appends a log-only event; `append` adds a position to an existing
artifact; `catchup` lists newest-first.
`list [--kind <k>] [--json]` enumerates every live artifact in the hot
directory newest-first —
including the non-actionable kinds `open` and `memories` do not show —
marking each consumed item with its outcome; `show <filename>` prints one
artifact's raw Markdown body, resolving the hot directory first and then the
cold archive. Dated inline headings (positions, conclusions, review stamps)
take the house timestamp from `journal stamp` — RFC 3339 seconds in UTC, the
same spelling as the event log's `ts`, so prose and log cross-grep — never
an agent-authored date.
Work waiting for a future session uses the primary actionable kinds — `todo`,
`handoff`, `inbox`, `plan`, `discussion` — plus lower-priority `later` and
`feature-request`. A feature request describes a wanted capability without
assigning execution priority. `journal open` lists the primary queue first,
then separate later and feature-request sections, annotating each item with how
long it has waited (for discussions, since the latest typed position; otherwise
since creation), until an explicit
`journal consume <filename> [--outcome done|superseded|discarded]
[--note <text>]` retires an item with a typed consumed event. The journal is
append-only; consumption never edits or deletes the artifact.

A `discussion` is the answer-owed actionable kind: an open debate rides the
same queue until someone resolves it, and `arc begin --from-journal` promotes
one straight into a change. `journal append <filename> [--ref <target>]
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
`journal note --kind discussion --scaffold discussion` seeds the
conventions at birth (the `Position: for|against|amend` stance line, reply-to
quoting, the resolution vocabulary, and the norm that a contested discussion is
resolved by a non-author of the winning position or by the user). A repo-local
`.arc/templates/discussion.md` overrides the built-in, and `--scaffold` works
on any `journal note`: the template is prepended to `--body-file` content, or
recorded alone. `journal discussion <filename> [--json]` renders the derived
view of one debate: its age, the stance tally (`for`/`against`/`amend` parsed
once per actual position block, so hand-written positions count too), the
distinct participants and reply-refs from the typed `position` events, and —
once resolved — the outcome with a resolver-participation flag that surfaces a
resolver who also argued a side under the same harness-native session identity.

The journal and the ledger bridge in three advisory ways, none of which can
gate the authoritative ledger. `arc begin --from-journal <artifact>` opens a
change from an open actionable item: it records the artifact filename as the
change's `journal_ref`, appends a `consumed` event with outcome `superseded`
and note `change <id>` (leaving the artifact file untouched), seeds an initial
brief threaded from the source body so the change starts with the resolution,
and refuses a missing, non-actionable, or already-consumed source before writing
anything.
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

`journal doctor [--json]` performs a read-only health check. It reports
malformed journal lines, artifact names and kinds, and dangling references as
problems with a failing exit status, while stale lanes and archive housekeeping
remain non-failing advice; it never creates the journal directory.

`arc doctor [--json]` performs the same read-only split for the authoritative
change ledger. Malformed events, store configuration, IDs, and missing open
events are problems; orphaned temporary files and retention refs, missing open
branches, long-expired claims, dependency cycles, and future event types are
non-failing advice. Human output names each affected path or ref; JSON uses the
versioned `arc-doctor/1` report. The exit status is 0 for a clean or advice-only
ledger and 1 when problems are present.

## Roadmap

Shipped: local core +
policy engine (M1–M2), `/arc` skill wiring (M3), deterministic export/import
bundles (M4), local orchestration foundations (dependencies, tags,
actionable status, query, and batch views), claims/stages and execution
roles, messaging and the inbox rollup, project-journal mechanics, and the
ledger side of forge projection (declare/link/checks/pr-state with
fail-closed validation). Remaining evidence-driven possibilities live in the
journal's open items (`arc journal open`).

## License

[Unlicense](UNLICENSE) — public domain.
