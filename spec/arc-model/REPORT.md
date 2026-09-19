# arc-model report

What the types enforce, what the runtime checks, and what the model leaves
unsettled. The suite that produces the evidence below is run with:

```sh
cd spec/arc-model
cabal v2-test --test-show-details=direct --test-options="--seed 20260907 --tests 300"
```

## Structural type invariants

These hold by construction; no test can observe them violated, and no
counterexample can be generated for them.

- **Distinct identifiers.** `ChangeId`, `PatchsetId`, `Revision`, `TreeId`,
  `ActorId`, `GateName`, `DeclarationId`, `EventId`, `FindingId`, `DebtId`,
  `ClaimId`, `HoldId`, and `FailureLabel` are separate types. A revision
  cannot be compared with a tree, and a patchset cannot be passed where an
  event is wanted.
- **Observation is total.** `Observed a = Omitted | Observed a`. There is no
  default, no `Bool`, and no exception path from a missing observation to a
  result: a gate reading can be `Omitted` in every one of its four fields.
- **Four gate readings are four fields.** `GateReading` carries the result,
  the coverage (`Covered` / `NeverEvaluated` / `EvaluatedOtherTree` /
  `DeclarationMoved`), the availability (`NotProduced` / `Recorded
  execution` / `EvidenceUnreadable`), and the demonstrated falsification
  separately. `gateGreen` reads coverage first, so a pass recorded elsewhere
  cannot stand in for the declaration and tree in force.
- **A refusal is structured.** `Refusal` is a sum whose payloads are the
  facts that stood in the way; `Refusal` has no free-text constructor, and
  `refusalText` is a rendering, not the carrier.
- **A permission carries its basis.** `Decision = Permitted DecisionBasis |
  Refused Refusal`. `DecisionBasis` names the patchset, head, tree, target,
  policy, authorization, one covered evaluation per required gate, and the
  finding and hold vectors that had to be empty.
- **Permission and effect are different values.** `execute :: Observations ->
  ChangeState -> Decision -> Either Refusal ExecutionPlan` re-checks the basis;
  `recordIntegration` is the only function that appends an
  `IntegrationRecord`. A `Decision` alone cannot record anything.
- **Authorization is a three-way sum.** `AuthorizedByVerdict`,
  `AuthorizedByWaiver`, and `AuthorizedByVerdictUnderWaiver` are distinct, so
  "a waiver authorized this" cannot be confused with "a waiver was recorded".
- **Obligation, outcome, and history are separate types.**
  `CoverageAfterIntegration` carries `coverageRead`, `coverageVerdict`,
  `coverageApproved`, `coverageOpenFindings`, and `coverageAuthorization`
  independently. `historicalAuthorization` is a projection of the newest
  `IntegrationRecord`; no function writes over a recorded basis.
- **Reference integrity under shrinking.** A scenario's references are
  positional; `shrinkScenario` filters out any candidate whose debt reference
  would exceed its patchset count, so every counterexample carries the
  references its events need.
- **The waiver query is single-slot.** All debt declarations are retained as
  ledger facts, while `newestWaiver` reads exactly one — the newest bound to
  the latest patchset — which is the semantics of the production reducer.

## Runtime validation

### Unit fixtures

`test/Fixtures.hs` pins the exact answer for each required case:

| fixture | what it anchors |
| --- | --- |
| `waiver-expiry` | a debt waives `ps-01` and not `ps-02` |
| `debt-beside-refusal` | a changes-requested verdict with an open finding is not permitted; the debt is in no basis; the refusal stands without the finding |
| `under-debt` | integration under a waiver, then a negative audit: the read is fulfilled, the findings stay open, the basis is untouched, fulfilled is not approved |
| `repair-review` | an approved first patchset plus an unread repair is a stale approval and an `OwedReview RepairUnread` |
| `unknown` / `elsewhere` / `changed` / `unreadable` | omitted, other-tree, shape-moved, and unreadable gate evidence each produce their own reading and refusal |
| `equal-tree` | equal trees with different contributor and obligation scopes decide differently; a waiver rescues the contributor; an unused debt is not named |
| `episode` | an expired claim ends liveness, not the retained debt and evidence; the waiver still applies |
| `debt-unused` | a debt recorded beside an approval that stood anyway authorized nothing |
| `stale` / `target-moved` / `policy-moved` | a moved head refuses; a target or policy that moves between decision and execution stands the action down |
| `permission-not-effect` | permission alone records no integration; recording lands the basis |
| `audit` | an open change refuses an audit; an approving audit needs a declared independent identity; a negative audit is open to anyone |
| `provisional` | a provisional approval gates like any other |
| `demonstration` | the model refuses a contributor reviewer; the deliberate fault permits |

### Properties

Ten properties run over generated scenarios; seeds and case counts are
recorded in the README.

| property | statement |
| --- | --- |
| integratable permits | every history the generator marks integratable permits |
| mutation flips | each one-invalid-transition mutation of an integratable history either refuses or stands down on a moved basis |
| basis grounded | every fact in a permitted basis is present in the ledger and observations; the consumed finding and hold vectors were empty |
| unknown never permits | omitted, unreadable, other-tree, and shape-moved evidence never permit |
| moved basis stands down | a target or policy moved between decision and execution produces `RefusedBasisMoved`, never a reused basis |
| waiver exact | a named waiver is bound to the basis patchset and is the newest for it |
| refusal stands | a changes-requested or comment-only verdict on the current patchset is never permitted |
| read needs a reader | a fulfilled read implies a declared, non-contributor reader recorded on the shipped revision |
| debt unused | a debt bound to an approved patchset is reported as recorded debt, not as an authorization input |
| negative audit does not approve | a negative audit that fulfils the read leaves the approval flag false unless an independent approving answer exists |

### Mutants

Nine deliberate faults; each must be killed, and each divergence must be of
the predicted class. Distinguishing a different refusal from a permission is
deliberate: dropping one ground of a refusal is a real fault even when another
ground answers.

| fault | channel | predicted divergence | killed (seed 20260907) |
| --- | --- | --- | --- |
| contributor identity ignored | decision | permits, different refusal, different basis | 13 tests, 4 shrinks |
| gate matched by name | decision | permits, different refusal | 11 tests, 3 shrinks |
| unknown treated as success | decision | permits, different refusal | 18 tests, 4 shrinks |
| authorization reused after its basis moved | execution | permits, different refusal | 102 tests, 4 shrinks |
| fulfilled implies approved | coverage | different value | 13 tests, 1 shrink |
| latest debt applied to every patchset | decision | permits, different refusal | 1 test, 5 shrinks |
| debt clears a refusing verdict | decision | permits, different refusal | 33 tests, 2 shrinks |
| later audit rewrites the integration basis | historical | different value | 6 tests, 5 shrinks |
| unreadable evidence counts as review | decision | permits, different refusal | 23 tests, 6 shrinks |

A surviving mutant is reported as a failure; the suite exits non-zero. The
reason a mutant may legitimately exhibit more than one class is the fault
itself: a ground dropped from a refusal surfaces as a permission when nothing
else stands in the way, and as a different refusal when something does.

### Demonstrated counterexample

The smallest demonstration is the unit fixture, not a shrunk random case. The
history is one patchset authored by `author`, with `author` also recording an
approving verdict:

- model: `Refused (RefusedSelfApproval (EventId 2) (ActorId "author")
  (fromList [ActorId "author"]))`;
- `contributor-identity-ignored` mutant: `Permitted`, because it drops the
  contributor comparison from the independence rule.

The mutant's property also finds its own smallest random counterexample (13
tests, 4 shrinks): a one-patchset history whose verdict is recorded by its
contributor under a policy that requires independence. Both are reported by
the suite; neither is committed as a golden string, because the fixture pins
the behaviour and the shrunk case pins the shrinker.

### Generator coverage

The coverage sampler generates 4000 scenarios and fails if any required class
is never reached. At seed `20260907` the counts are: permitted 201, waived 58,
self-approval refused 166, gates refused 682, verdict stands 818, head moved
829, target moved 39, policy moved 41, debt unused 69, unknown observation
1563, audit fulfilled read 83, audit negative not approved 9, episode expired
2011, equal-tree contributor variation 2026. A class with a count of zero is
reported by name, so the suite cannot pass on trivial histories.

## Unsettled design

Points where the model fixes a reading that production or a future slice may
still choose differently. Each is a deliberate choice, not an oversight.

- **Refusal priority.** The model checks findings before authorization, and
  authorization before gates, and holds last. Production renders the first
  blocker by its own ordering; the model's ordering is stated rather than
  derived, and a reader comparing refusals should compare the facts, not only
  the first tag.
- **Debt kind derivation.** `nothing-read`, `contributor-only`, and
  `independent-review` are derived; `merge-resolution-unread` and
  `repair-unread` are accepted only when declared, because only the caller can
  say which of the two a ledger sees identically. Whether the ledger itself
  should distinguish them is open.
- **Coverage after a repair.** A negative audit fulfils the read and leaves
  its findings open. A repair after that audit starts a fresh obligation in
  the model. Whether the fulfilled read should survive a repair is a policy
  question the model does not answer.
- **Approval beside a later negative audit.** `coverageApproved` reports
  whether an independent approving answer exists. An independent approval on
  the shipped patchset makes it true even after a negative audit; the audit's
  verdict remains its own field. Whether a later negative audit should
  withdraw the approval flag is open.
- **Policy motion.** A policy that changes between decision and execution
  produces `RefusedBasisMoved` rather than a re-decision. Re-deciding under
  the new policy would be a different action, with a different basis.
- **Contested verdicts.** The model treats more than one active tip as
  contested and refuses. It does not model the repair of a contested chain
  beyond the arrival of a single superseding verdict.
- **Claim episodes.** A claim's expiry is modelled as a fact about liveness
  only. Stage budgets, staleness, and claim generation are not modelled.

## Deferred out of this slice

Both are recorded here so a later reader does not read the package's scope as
a claim about them:

- **The candidate protocol.** The proposed challenge/evaluation/selection
  semantics is a second model slice. This package models existing arc
  authorization only, and the candidate rules must not be added to it by
  accident.
- **The differential adapter.** A test adapter that runs the same histories
  through the Rust implementation and the model is not built here. A quiet
  differential run would be supporting evidence, not proof of equivalence.

## Known unsupported semantics

- **Git effects.** Synthesizing a merged tree, verifying that a merge commit's
  tree is the evaluated one, and resetting after a mismatch are outside the
  model. The evaluated tree and the target before are observations.
- **Durability, locking, races, crash recovery.** A pure model cannot
  establish them; they stay independent OS-level tests.
- **Acceptance probes.** The `AcceptanceProbesNotGreen` blocker is not
  modelled.
- **Provenance categories.** `actor_source` values (`flag`, `env`, `derived`,
  `git-fallback`) collapse to a declared/assumed boolean. `require_declared_actor`
  is modelled for the invoker only.
- **Forks, worktrees, retention, bundles, journal, changelog projection.**
  None are modelled.
- **Review-map advisories.** `reviewer-behind-final-patchset`,
  `no-independent-reviewer`, and the other advisories are not modelled; they
  never block, and the model's job is the blocking decision.
- **Dependency status.** `obsBlockedBy` names blockers; how a chain's
  readiness is computed is not modelled.
