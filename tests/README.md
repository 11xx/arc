# Tests

Integration runs the exact witnesses in `.arc/invariants.tsv`. Each row names its
invariant and caller-visible claim. The runner validates every full name against
libtest's inventory before dispatching it with `--exact`; missing, ignored and
unexecuted witnesses fail. It selects the spawned executable from Cargo's artifact
receipts. Development artifacts share optimization level 1 with debug symbols
disabled. Debug assertions and integer overflow checks stay enabled. The build
gate prepares the artifacts the invariant gate consumes. Build metadata reads
preserve the watched Git index.

Run the invariant gate:

```sh
python3 .arc/test-binary.py invariants
```

Run an area suite on demand by its exact module path:

```sh
python3 .arc/test-binary.py area cli gate_raises
```

Use `area unit store::tests` for a unit module and `area cli ''` for root tests. The module argument names its
direct tests, not modules sharing a substring. Area suites belong to no gate.
The runner's own acceptance checks are `python3 .arc/test_binary_test.py`.
`cargo test --locked` runs the whole remaining Rust tree with the development executable.

To add or replace a witness, name the invariant and rejected state in its doc
comment. First demonstrate failure for that state on the unfixed implementation
or a faulty variant; then demonstrate success. Add its full name and claim to
the list. Replacements must establish the same boundary before a witness leaves
it. A test's existence, cost or historical failure count does not make it required.

Publication runs build, lint, the invariant gate and the five exact tests in module `release`
against the release-profile executable built from the checkout. Run that artifact
check with `python3 .arc/test-binary.py release`. Area inventories do not participate.

CLI fixtures copy independent Git files with empty hooks and exclude locations,
disable automatic maintenance and retain explicit collection and pruning. Child
waits wake on process exit and kill and reap after a 120-second hang bound.
`RUST_TEST_THREADS` sets invariant concurrency; its default is four workers per
available logical CPU, capped at 64. Silent-write recovery probes use a short
debug-only clock; a separate area check pins the thirty-second production default.

Gate coverage is declared in `.arc/gates.toml`; `arc gates paths` lists every
tracked path and its gates. People-only Markdown and the license are inert.
Markdown embedded in the executable is source and remains covered. `.arc/` is
protected by the selection contract. Cross-change base passes require committed
destination adoption and an eligible baseline; declaration changes require fresh
execution. See `arc guide gates` and `arc guide base-passes`.
