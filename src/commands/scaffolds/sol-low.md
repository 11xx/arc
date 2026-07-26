# Brief: <one-line outcome>

## Goal
<what done looks like, in one or two sentences>

## Pre-resolved design
<the decisions are already made; state them so no judgment is needed>

## Deliverables
<the exact files/commands to produce — nothing beyond this list>

## Tests
<the exact tests to add — none beyond these>

## Acceptance probes
<name each probe as a runnable command whose outcome distinguishes the intended
behavior from a plausible wrong implementation; a brief whose material risk
has no such command is not ready to delegate>

- Run each probe at the final commit and record it with
  `arc verify --command '<the probe>'` before `arc done`. If the sandbox
  prevents running a probe, record it with
  `arc verify --attest --result pass` instead.
- Probe evidence is recorded beside gate results but never gates integration;
  reviewers enforce it.

## Execution contract (clear-spec delegated slice)

- Work only in this change's worktree. `git rebase master` first — earlier
  chain members land ahead of you, so the base is stale by design.
- Run as `ARC_ROLE=implementer` with `ARC_HARNESS`/`ARC_SESSION` set, and a
  distinct `ARC_ACTOR`. Loop: `arc stage <change> implementing --claim` →
  implement in scoped commits → `arc done`, then STOP for lead review.
- Scope ceiling: only the deliverables above. No refactors, renames,
  dependency additions, or tests beyond those listed. If a named file,
  symbol, or assumption is missing or wrong, run
  `arc stage <change> blocked-on --note "<what>"`, `arc release-claim`, and
  stop — do not work around it.
- If a probe cannot be run, or fails for what looks like a defect in the probe
  rather than the implementation, stop and request a new brief —
  never edit a probe to make it pass.
- Never run `review`, `integrate`, or `close`; those are the lead's.

## Sandbox facts (arc-driving executors)

- Arc needs `.git` writable. A workspace-write sandbox blocks it; run with
  `-s danger-full-access` or an equivalent that permits `.git`.
- If commit signing is unavailable, stage the work and report
  "staged, no SHA"; the lead commits, then snapshots, then reviews, in order.
- Keep `claim`/`stage` heartbeats current so the orchestrator watches the
  ledger, not transcript logs.
