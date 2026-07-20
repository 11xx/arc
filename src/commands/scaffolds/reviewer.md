# Review brief: <change under review>

## What to verify
<the specific behaviors, invariants, and edge cases this change must get
right — the reviewer confirms these, not just that tests pass>

## Review contract

- Run as `ARC_ROLE=reviewer` with a distinct `ARC_ACTOR` from the
  implementer: a verdict from the authoring identity is self-approval and the
  policy rejects it. When the lead ran ceremony for an executor, review
  `--on-behalf-of` no one — approve as yourself.
- A verdict binds to the exact approved patchset head. Any new commit
  invalidates it; re-review the delta (`arc diff <change> --since-approved`).
- Record findings atomically with the verdict. Use
  `--verdict changes-requested` with findings JSON for blockers;
  `--verdict approved` only when the deliverables and tests are met.
- Do not implement fixes under the reviewer role; request changes and let the
  implementer act.

## Sandbox facts

- Gate evidence may be attested (run on another host or by a sandboxed
  executor) rather than observed by arc. Apply stricter judgment to attested
  gates; they still count toward green-ness.
- Verify the ledger, not transcript logs: claim/stage heartbeats and events
  are the source of truth for what happened.
