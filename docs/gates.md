# Gates, evidence, and exit codes

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

A gate runs in the checkout that holds the change. `verify`, `snapshot
--verify`, `done`, and `rebase --verify` execute the command in the change's
recorded worktree and record the evidence at that worktree's head, whichever
checkout of the repository the command was typed in, and the run names the tree
it used when that is not the invoking one. The declarations come from the
invoking checkout instead, the same place `arc status` reads them, so what a
run discharges and what status still owes cannot disagree. arc refuses a gate
run only when the change's recorded worktree is missing or its HEAD is not the
branch head. A run started from another checkout of the repository is
redirected to the recorded worktree and says so. A change whose
recorded worktree is gone is refused with the path and the `git worktree add`
that restores it, because a gate run anywhere else would describe another
tree, and a worktree standing off the branch head is refused with the checkout
that puts it back, because evidence recorded there is evidence status will
never count.

By default `arc verify` runs the gate and observes the result itself. When
the gate ran elsewhere — inside a sandbox, or on another host — record that
external evidence with `--attest --result pass|fail` (optionally `--note`):

```sh
arc verify <change> --gate test --attest --result pass \
  --tested-revision <commit> --execution-host ci.example \
  --runner build/test-42 --note "ran in CI sandbox"
```

Attested evidence is recorded without executing the command and still counts
toward gate green-ness, but `arc show` and `arc status` mark it `(attested)`
and name the external runner and host so a lead can apply stricter judgment. It
has no exit code or duration because arc did not run a process. Attestation
requires the result, tested revision, execution host, and runner together;
those context flags are invalid for locally executed verification.

Every multi-gate invocation records its intended gate manifest before execution
and correlates each observed result to that run. `--skip-green` records a reuse
edge to the earlier passing evidence instead of silently omitting work. A run
is complete when every declared gate has an observed or reused terminal edge;
an interrupted run remains visibly incomplete without a mutable completion
record.

Every locally executed gate captures and attempts to retain the worktree tree
before it runs. If the tree cannot be retained, local provenance remains
unknown and a passing result does not count as green. Sequential runs also
record whether the retained tree differed from the tested revision and whether
the worktree moved during execution. For `verify --all --parallel`, the shared
worktree cannot prove that no gate made a transient change and restored it, so
cleanliness remains unknown and passing results do not count as green. A
boundary change is still recorded as `tree_moved` and explains the stronger
failure.

Evidence that passed but cannot be reused reads `pass` in its raw result, so
every human-facing surface names why it does not count: `arc resume`, `arc
rescue`, and `arc check` render the reason beside the gate, and `arc show`
renders it beside the verification it belongs to. Because a rerun
against the same unusable tree records the same unusable evidence, the
`next_action` is `clean_worktree:<gate>` while the worktree is actually
dirty — for any gate that is not green, including one that failed, because
while the tree is dirty no local run produces evidence that counts
(attested evidence carries its own execution context instead). Cleaning is
not a fix for a failure; it is the precondition for a local run whose
result is usable. Where the live tree is unknown, because the change has no
worktree, the advice is the rerun. Once the tree is clean the advice
becomes `run_gate:<gate>`, since
evidence already recorded cannot be repaired by cleaning; only a fresh run
replaces it.

A gate that passed says nothing about whether it could have failed. A check
watched to fail for a stated reason, then to pass once that reason was removed,
and a check first run after the code it checks leave the same record. `arc
verify --falsified-by <event-id> --predicted <reason>` separates them: the
passing evidence names the failing run it answers and the reason stated before
that run. arc validates the reference on the way in — a failing verification of
the same gate or command on the same change — and reads the revision from the
referenced event, so the two halves cannot disagree. The flags are valid only
together, and not with `--all`, which runs several checks at once.

Gate rows carry the result as `discrimination`: `discriminating` when any
passing evidence for that gate at the counted revision names a falsification,
`undiscriminated` when none does. It is asked of the gate at a revision rather
than of the newest run, because a check watched to fail and then pass against a
tree is not unwatched by running it again; otherwise `verify --all`, and so
every `arc done`, would silently retract the finding. `evidence_event_id` names
the evidence that counts and `discrimination_event_id` the evidence that
carried the reference, whenever they differ. Human-facing gate lists append
`(discriminating: failed at <rev>: <reason>)` or `(undiscriminated)` to a
passing line. Nothing is inferred: a check arc never recorded failing is
undiscriminated, which is unknown rather than a claim that it cannot detect
anything. The whole record is advisory — readiness, gate results, and exit
codes ignore it.

Executed gates capture combined stdout and stderr, retaining only the final
4096 bytes. Failed-gate tails appear in `arc show` and `arc status`; successful
gates stay compact. A declared timeout terminates and reaps the gate's entire
process group and records the failure as timed out. Without `timeout`, gate
execution remains unbounded.

## Exit codes

`arc check` is the integration preflight and its code names the blocker.
`integrate` and `rebase` refuse in the same vocabulary, and a queue exits with
the code of the member that stopped it, so one table reads the same way
whether one change was integrated or twenty.

- `arc check` exits 0 when the change is ready to integrate.
- `arc check` exits 2 while a blocking finding is open.
- `arc check` exits 3 when no valid approval covers the current head.
- `arc check` exits 4 while a hold is active.
- `arc check` exits 5 when a required gate is not green at the head.
- `arc check` exits 6 for a closed change, a missing branch, or malformed
  state.
- `arc check` exits 7 while a prerequisite change is unresolved.
- `arc check` exits 11 when the target moved with conflicting changes and the
  branch needs rebasing.
- `arc check` exits 12 when a declared acceptance probe is not discriminating.
- `arc check` exits 13 while the change declares it is iterating.
- `arc check` exits 14 when the tree a merge would ship has no gate evidence.

Codes 1 and 2 are also reachable without a blocker at all: `arc` exits 1 on
an internal error and 2 on a usage error, which argument parsing decides
before any command runs. A caller that must tell a usage mistake from an open
finding reads the diagnostic rather than the code alone.

Code 10 belongs to the forge projection rather than to readiness. `arc forge
link` exits 10 when the observed tuple or the declared policy does not match,
appending no event.

Two codes belong to commands rather than to readiness. `arc claim`, `arc
stage`, and `arc release-claim` exit 8 on a claim or stage ownership conflict.
arc exits 9 when the execution role refused the command, before it takes a
lock or writes an event.

The other commands a script branches on carry their own contracts:

- `arc is-blocked` exits 0 when the change is ready, 1 when it is blocked, and
  2 when the lookup or ledger read failed.
- `arc watch` exits 0 when its condition is reached and 2 on a timeout.
- `arc take` exits 2 when no change is ready.
- `arc env` exits 1 and prints the export template when no harness is
  detected.
- `arc import` exits 1 and writes nothing when an event conflicts.
- `arc history resolve` exits 2 when nothing moved the revision.
- `arc restack --advise` exits 0 when the change has no dependents.
- `arc doctor` exits 1 when problems are present and 0 for a clean or
  advice-only ledger.
- A rejected self-approval follows the no-valid-approval path and exits 3.
- An arc-managed Git hook always exits 0, so it can never block a commit.
- A repeat of a recorded source item writes nothing, prints the existing
  entry, and exits 0.
- A spooled write prints `spooled: <path>` and exits 0.

