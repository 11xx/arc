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

Declare them on the brief with `arc brief <change> --probes-json` (a JSON
array inline, a path, or `-` for stdin), so a probe is a contract rather than
a sentence. Naming a command is not sufficient: a declared probe blocks
readiness until evidence bound to the patchset's brief **fails at that brief's
base revision and passes at the patchset head**. Record both phases:

- `arc verify <change> --probe <name> --probe-phase baseline` at the base
- `arc verify <change> --probe <name>` at the final commit

Record the baseline while HEAD *is* the brief's base revision — arc refuses it
otherwise, attested or not. That base is the brief's, not the patchset's: if
you rebase onto a moved target after recording it, the baseline describes a
revision the work is no longer built on, and a probe can pass for something
the target brought rather than for the change. Rebase first, then record the
brief, then record the baseline; if the target moves again, ask for a new
brief rather than reusing a baseline measured somewhere else. Evidence binds to the exact brief the patchset
carries, the probe name, the phase, and those exact revisions; recording
against a different brief version, or at any other revision, leaves evidence
arc does not count.

A probe that passes at both ends proves nothing about the change. If the
sandbox prevents running one, attest it — and an attested baseline attests the
failure, because that is what the phase asserts:

    arc verify <change> --probe <name> --probe-phase baseline --attest \
      --result fail --tested-revision <base-sha> \
      --execution-host <where> --runner <who>

    arc verify <change> --probe <name> --attest \
      --result pass --tested-revision <head-sha> \
      --execution-host <where> --runner <who>

Three limits attestation and differential evidence do not lift:

- Attested evidence is a claim arc did not check. Readiness looks at results
  and bindings, never at whether arc ran anything, and an attested run
  captures no output — so the reviewer has nothing to inspect. Attest only
  what a sandbox genuinely prevented, and say which.
- Where arc ran the probe, the reviewer still inspects the captured baseline
  output and confirms it failed for the expected reason. A probe that fails at
  the base for an unrelated reason — a missing fixture, a compile error — is
  not discriminating, and arc cannot tell the difference. An attested baseline
  captures no output, so that confirmation is unavailable: the pair shows the
  results differ and nothing more, which is a weaker claim and should be
  treated as one.
- Differential evidence does not replace independent review. It shows the
  change moved one named behavior; it says nothing about what else moved.

The absence of declared probes means "no probe contract was recorded", never
"all semantic risks are covered".

## Execution contract (clear-spec delegated slice)

- Work only in this change's worktree. `git rebase master` first — earlier
  chain members land ahead of you, so the base is stale by design. Rebase
  before the brief is recorded where you can; a baseline is only as good as
  the base it was measured at. Rebase
  before the brief is recorded where you can; a baseline is only as good as
  the base it was measured at.
- Run as `ARC_ROLE=implementer` with `ARC_HARNESS`/`ARC_SESSION` set, and a
  distinct `ARC_ACTOR`. Loop: `arc stage <change> implementing --claim` →
  implement in scoped commits → `arc done`, then STOP for lead review.
- Scope ceiling: only the deliverables above. No refactors, renames,
  dependency additions, or tests beyond those listed. If a named file,
  symbol, or assumption is missing or wrong, run
  `arc stage <change> blocked-on --note "<what>"`, `arc release-claim`, and
  stop — do not work around it.
- If a probe fails for what looks like a defect in the probe rather than the
  implementation, stop and request a new brief — never edit a probe to make it
  pass. If it cannot be run at all, attest it as above and say why; if you
  cannot honestly attest it either, stop.
- When writing a probe, remember that a command substitution runs in a
  subshell: `exit` inside one exits only that subshell, and
  `[ "$(cmd || exit 1)" = x ]` reports on the *output*, not on whether `cmd`
  succeeded — it passes when a failing `cmd` still prints `x`. Bind the
  output and check the status explicitly:

      out=$(cmd) || exit 1
      [ "$out" = x ] || exit 1
- Never run `review`, `integrate`, or `close`; those are the lead's.

## Sandbox facts (arc-driving executors)

- Arc needs `.git` writable. A workspace-write sandbox blocks it; run with
  `-s danger-full-access` or an equivalent that permits `.git`.
- If commit signing is unavailable, stage the work and report
  "staged, no SHA"; the lead commits, then snapshots, then reviews, in order.
- Keep `claim`/`stage` heartbeats current so the orchestrator watches the
  ledger, not transcript logs.
