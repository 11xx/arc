# Brief: <one-line outcome>

## Goal
<the problem and what a correct solution achieves>

## Context and constraints
<what the executor may exercise judgment over, and the hard constraints it
must not cross — name them explicitly; inferred intent drifts>

## Deliverables
<the surfaces to change; the executor chooses the implementation>

Before sending: grep-verify every path, symbol, and line number named below,
and say here what each was verified against.

## Tests
<the behaviors that must be covered; the executor may add more where the
design demands it>

## Execution contract (judgment-bearing delegated arc)

- Work only in this change's worktree. `git rebase master` first — the base
  is stale by design as earlier members land.
- Run as `ARC_ROLE=implementer` with `ARC_HARNESS`/`ARC_SESSION` set and a
  distinct `ARC_ACTOR`. Loop: `arc stage <change> implementing --claim` →
  scoped commits → `arc done`, then STOP for lead review.
- Overbuild fence: do not turn a small change into a rewrite; add no tests,
  refactors, or dependencies beyond what the goal needs, and say so if the
  goal itself is underspecified rather than guessing. Updating literals the
  goal forces (a new field or variant breaks every construction site, usually
  test fixtures) is inside the ceiling, not overbuild.

- Over-determination fence: if a target, symbol, or precondition is missing,
  `arc stage <change> blocked-on --note "<what>"`, release the claim, and
  stop — never work around a missing target to satisfy the literal goal.
- Never run `review`, `integrate`, or `close`; the lead gates those.

## Sandbox facts (arc-driving executors)

State this run's environment, not the general lesson: what this executor can
write, whether it can sign commits, and what it can reach. The generic facts
below stay only as defaults you have checked apply.

- Arc needs `.git` writable. A workspace-write sandbox blocks it; run with
  `-s danger-full-access` or an equivalent that permits `.git`.
- If commit signing is unavailable, stage the work and report
  "staged, no SHA"; the lead commits, then snapshots, then reviews, in order.
- Keep `claim`/`stage` heartbeats current so the orchestrator watches the
  ledger, not transcript logs.
