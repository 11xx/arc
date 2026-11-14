# Tests

Integration runs the exact witnesses in `.arc/invariants.tsv`. Each row names its
invariant and caller-visible claim. The runner validates every full name against
libtest's inventory before dispatching it with `--exact`; missing, ignored and
unexecuted witnesses fail. It selects the spawned executable from Cargo's artifact
receipts and hands it to the harness as `ARC_TEST_BIN` with `ARC_TEST_BIN_RECEIPT`,
which names the build the selection was made for, the selected executable by
resolved path and identity, and whether it carries debug assertions. Only the
runner selects: a harness given `ARC_TEST_BIN` without a receipt for that file of
its own build refuses to start, so a plain `cargo test` never runs an exported,
stale or substituted executable. Development artifacts share one profile: the crate is unoptimized, so
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
It guards against a test run reaching, by accident, what it did not create: an
inherited environment, a forgotten helper, a wrong path, or a test or the `arc`
under test naming the real repository. The kernel confines each harness, and
every process it starts, to writing beneath the build's target directory and the
system temporary directory, through Landlock, with every file system right the
running kernel offers handled and device ioctls granted only on the terminal. A
root that is, or contains, the checkout, the home directory or the directory
holding the checkout's shared Git directory, or that lies inside that Git
directory, is refused rather than granted. Descriptors inherited above standard
error are closed and standard input is made read-only, since a descriptor opened
before confinement keeps its access. A kernel without the Landlock rights this
needs, or a refused root, leaves a plain `cargo test` unconfined with a notice;
the runner sets `ARC_TEST_CONFINEMENT=required`, and the harness then refuses to
run. A machine prepared in advance to defeat the guard, with a hard link or bind
mount planted inside the build directory, is outside it, and Landlock does not
restrict metadata-only changes such as mode, ownership, timestamps and extended
attributes; `arc doctor` and `arc catchup` still report a contaminated repository
configuration.

Started with `GIT_DIR` or `GIT_WORK_TREE` set, as Git sets them for hooks and
`rebase --exec` steps, the harness and the runner refuse to start and name the
variable. The runner removes every other inherited `GIT_` variable before it
starts Cargo, and `build.rs` runs Git with a Git environment of its own. Every other inherited `GIT_` variable is removed, and the harness sets
the few it needs: Git discovery never climbs into the checkout or the system
temporary directory from below, so Git in a directory a test forgot to
initialize finds no repository and a test that means this checkout names it;
`git init` copies no template; global configuration is a file in a directory the
harness process created, removed when it exits; system configuration is not
read. A helper's own settings for its command still apply. A child-mode switch,
a variable named `ARC_TEST_CHILD_*`, reaches a harness only from the harness
that started it, carrying that parent's token; the runner passes none.

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
