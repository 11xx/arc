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

Declare them on the brief with `--probes-json`, so the probe is a contract
rather than a sentence. Naming a command is not sufficient: a declared probe is
structurally ready only once arc has observed it **fail at the brief's base
revision and pass at the head**. Record both phases:

- `arc verify <change> --probe <name> --probe-phase baseline` at the base
- `arc verify <change> --probe <name>` at the final commit

A probe that passes at both ends proves nothing about the change. If the
sandbox prevents running one, record it with `arc verify --attest --result
pass` and say so — attested evidence carries an external execution context
instead of local provenance.

Two limits that differential evidence does not lift:

- The reviewer still inspects the baseline output and confirms it failed for
  the expected reason. A probe that fails at the base for an unrelated reason —
  a missing fixture, a compile error — is not discriminating, and arc cannot
  tell the difference.
- Differential evidence does not replace independent review. It shows the
  change moved one named behavior; it says nothing about what else moved.

The absence of declared probes means "no probe contract was recorded", never
"all semantic risks are covered".

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
- When writing a probe, remember that `exit` inside a command substitution
  exits only the subshell: `[ "$(cmd || exit 1)" = x ]` never fails the probe.
  Bind the result to a variable and test it, or use `set -e` outside any
  substitution.
- Never run `review`, `integrate`, or `close`; those are the lead's.

## Sandbox facts (arc-driving executors)

- Arc needs `.git` writable. A workspace-write sandbox blocks it; run with
  `-s danger-full-access` or an equivalent that permits `.git`.
- If commit signing is unavailable, stage the work and report
  "staged, no SHA"; the lead commits, then snapshots, then reviews, in order.
- Keep `claim`/`stage` heartbeats current so the orchestrator watches the
  ledger, not transcript logs.
