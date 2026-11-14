# Tests

Integration runs the exact witnesses in `.arc/invariants.tsv`. Each row names its
invariant and caller-visible claim. The runner validates every full name against
libtest's inventory before dispatching it with `--exact`; missing, ignored and
unexecuted witnesses fail. It selects the spawned executable from Cargo's artifact
receipts and hands it to the harness as `ARC_TEST_BIN` with `ARC_TEST_BIN_RECEIPT`,
which names the build the selection was made for and whether the selected
executable carries debug assertions. Only the runner selects: a harness given
`ARC_TEST_BIN` without a receipt for its own build refuses to start, so a plain
`cargo test` never runs an exported, stale executable. Development artifacts share one profile: the crate is unoptimized, so
an incremental rebuild after a source edit stays short, and dependencies are
optimized. Debug symbols are disabled; debug assertions and integer overflow
checks stay enabled. Preparation declared in `.arc/worktrees.toml` builds the
artifacts before a gate runs; the invariant gate's own build step then finds
them current. Build
metadata reads preserve the watched Git index.

Run the invariant gate:

```sh
python3 .arc/test-binary.py invariants
```

Run an area suite on demand by its exact module path:

```sh
python3 .arc/test-binary.py area cli gate_raises
```

Use `area unit store::tests` for a unit module and `area cli ''` for root tests. The module argument names its
direct tests, not modules sharing a substring. Area suites belong to no
integration gate.
The runner's own acceptance checks are `python3 .arc/test_binary_test.py`.
`cargo test --locked` runs the whole remaining Rust tree with the development executable.

To add or replace a witness, name the invariant and rejected state in its doc
comment. First demonstrate failure for that state on the unfixed implementation
or a faulty variant; then demonstrate success. Add its full name and claim to
the list. Replacements must establish the same boundary before a witness leaves
it. A test's existence, cost or historical failure count does not make it required.

Publication runs the invariant gate, clippy, formatting, the five exact tests in module `release`
against the release-profile executable built from the checkout, and `cargo test --locked`.
Run that artifact check with `python3 .arc/test-binary.py release`. A publication failure in
the remaining tree is fixed, or the test removed, before the release.

Both harnesses enter through `src/harness_entry.rs` before their first test.
Started with `GIT_DIR` or `GIT_WORK_TREE` set, as Git sets them for hooks and
`rebase --exec` steps, the harness and the runner refuse to start and name the
variable. Every other variable through which Git selects a repository, index,
object directory, common directory, namespace or configuration is removed from
the harness's environment, so no fixture command, a helper's or one a test builds, can
reach a repository the fixture did not create. Git discovery never climbs into
the checkout, the build's temporary directory or the system temporary directory
from below, so Git in a directory a test forgot to initialize finds no
repository; a test that means this checkout names it. Global configuration is a
file of the harness process's own and system configuration is not read. A
helper's own settings for its command still apply.

CLI fixtures copy independent Git files with empty hooks and exclude locations,
disable automatic maintenance and retain explicit collection and pruning. Child
waits wake on process exit and kill and reap after a 120-second hang bound.
`RUST_TEST_THREADS` sets invariant concurrency; its default is four workers per
available logical CPU, capped at 64. Silent-write recovery probes use a short
debug-only clock, and their bounds follow the debug assertions of the executable
under test; a separate area check pins the thirty-second production default.

Gate coverage is declared in `.arc/gates.toml`; `arc gates paths` lists every
tracked path and its gates. People-only Markdown and the license are inert.
Markdown embedded in the executable is source and remains covered. `.arc/` is
protected by the selection contract. Cross-change base passes require committed
destination adoption and an eligible baseline; declaration changes require fresh
execution. See `arc guide gates` and `arc guide base-passes`.

Fixture identity exemptions require a debug executable, the explicit
`ARC_TEST_FIXTURE_IDENTITY=1` flag, and `arc-test-fixture` in that repository's
common Git directory. They permit fixture commit addresses and suppress only the
seeded local Tester identity in health reports. Release executables enforce the
ordinary identity rules. Git environment guard tests remove the flag to exercise
product refusals.
