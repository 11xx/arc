# arc-model

An independent, pure Haskell model of arc's authorization semantics: what a
recorded history permits, what it refuses, and which facts a decision rests
on. It is a characterization of behaviour, not a translation of the Rust
implementation, and it is not part of the Rust build, its gates, or its
releases.

The model characterizes arc at `df47db0b559853af567362901e3027231b2f9d1d`.
A revision is pinned so a reader compares against a fixed implementation
rather than a moving branch; the constant is exported as `comparisonRevision`.

## What it models

- **Ledger replay.** Patchsets, verdicts, findings and dispositions, gate
  verifications, debt declarations, post-integration audits, claim episodes,
  holds, and integration records fold into an immutable state. Replay decides
  nothing.
- **The integration decision.** The observed head against the recorded
  patchset head, the exact evaluated tree (the merge target when the change is
  behind it), contributor identity and reviewer independence, a recorded
  approval or a waiver bound to the exact patchset, open blocking findings,
  holds, dependencies, and a gate declaration with its evidence.
- **Four separate gate readings.** Pass/fail, coverage (which declaration and
  tree an observation answers), availability (whether a record exists and
  could be read), and demonstrated falsification are distinct fields. An
  omitted observation is `Omitted`: never false, never successful.
- **Structured refusals and a decision basis.** A permission names the exact
  events, patchset, tree, target, and policy it relied on. A refusal names the
  facts that stood in the way.
- **Permission is not effect.** `execute` re-checks the basis against the
  observations and produces a plan; only `recordIntegration` puts the effect
  in the ledger.
- **Coverage obligations.** A debt declares a missing read; a waiver binds to
  exactly one patchset; a refusing verdict is not waivable. An independent
  negative audit can fulfil the read and leave its findings open: fulfilled is
  not approved. A later audit never rewrites what an integration rested on.

## What it deliberately does not model

Deferred, with the deferral recorded here rather than implied:

- the proposed candidate/evaluation/selection protocol — the second model
  slice;
- a differential test adapter against the Rust implementation;
- Git effects: how a merged tree is synthesized, that a merge commit's tree is
  the evaluated one, and the reset that follows a mismatch are outside a pure
  model, which takes the evaluated tree and the target as observations;
- operating-system durability, locking, race, and crash-recovery behaviour;
- acceptance probes, forks, worktrees, retention, bundles, and the journal.

The full list, including the fields the model collapses, is in
[REPORT.md](REPORT.md#known-unsupported-semantics).

## Build and test

```sh
cd spec/arc-model
cabal v2-build --enable-tests
cabal v2-test --test-show-details=direct
```

The suite exits non-zero on any failed fixture, property, surviving mutant, or
unreached generator feature. Its library dependencies are `base` and
`containers`; the test suite adds `QuickCheck`. The package is not a
dependency of the Rust crate and is not named by any `Makefile` gate target,
so no arc change needs a Haskell toolchain to build.

## Deterministic seeds and replay

The run is deterministic from one seed. Defaults: seed `20260907`, 300 cases
per property. Property `n` derives seed `seed + n * 13`; mutant checks use
`1000 + index * 31` and `2000 + index * 31`; the coverage sampler uses
`seed + index`.

```sh
cabal v2-test --test-show-details=direct --test-options="--seed 20260907 --tests 300"
```

A failure prints the shrunk counterexample and the replay line. Re-running the
same command with the same seed and test count reproduces every case; a
different seed is a different sample, not a different suite. Shrinking is
structural: a candidate that would leave a debt reference dangling is filtered
out, so a counterexample always retains the references its events need.

## Fixture format

A fixture is a Haskell `Scenario` interpreted by `build` into a ledger plus
observations. Fields (`Generators.Scenario`):

| field | meaning |
| --- | --- |
| `scnPatchsets` | number of patchsets, `rev1..revN` / `tree1..treeN` |
| `scnVerdictOnFirst` | bind the verdict to `ps-01` rather than the latest |
| `scnReviewer` | `Nothing`, or `ActorIndependent`, `ActorContributor`, `ActorAssumed` |
| `scnVerdict` | `Approved`, `ChangesRequested`, `CommentOnly` |
| `scnProvisional` | record the approval as provisional |
| `scnExtraContributor` | add a third contributor to every patchset |
| `scnDebt` | `(patchset index, declared kind)` or `Nothing` |
| `scnGateMode` | `GateCovered`, `GateOtherTree`, `GateShapeMoved`, `GateUnreadable`, `GateOmitted` |
| `scnBlockingFinding` / `scnResolveFinding` | record an open or resolved blocking finding |
| `scnHeadMoved` | the observed head is not the patchset head |
| `scnPolicy` | `dangerPolicy`, `openPolicy`, or `requireDeclaredPolicy` |
| `scnTargetAfter` / `scnPolicyAfter` | the execution observation moves the target or policy |
| `scnAudit` | `(verdict, independent)` recorded after integration |
| `scnEpisodeExpired` | record a claim and expire it |

`mutations scenario` returns the one-invalidating-transition set: gate
omitted/elsewhere/shape-moved/unreadable, head moved, target moved, policy
moved, finding opened, verdict refused, review bound to a stale patchset,
reviewer made a contributor, and waiver expired. `featureOf` classifies a
built scenario so the coverage sampler can prove each required class was
reached.

## Layout

```
src/Arc/Model/Identifiers.hs     distinct identifier types
src/Arc/Model/Observation.hs     Observed, gate declarations/evidence/readings, policy, observations
src/Arc/Model/History.hs         ledger events, replay, and derived queries
src/Arc/Model/Basis.hs           decision bases, structured refusals
src/Arc/Model/Decision.hs        decide, execute, recordIntegration
src/Arc/Model/Debt.hs            debt kinds, review obligation, coverage projection
src/Arc/Model/Audit.hs           post-integration audit gating and discharges
test/Generators.hs               scenarios, generators, shrinking, mutations, features
test/Fixtures.hs                 unit fixtures
test/Mutants.hs                  the nine deliberate faults
test/Main.hs                     driver: fixtures, properties, mutants, coverage
```

[REPORT.md](REPORT.md) separates what the types enforce, what the runtime
checks, and what the model leaves unsettled.
