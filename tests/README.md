# Tests

Run the full suite from the checkout root:

```sh
python3 .arc/test-binary.py cargo test --locked
```

The runner builds the `cli-test` profile and selects its Cargo-reported executable
through `ARC_TEST_BIN`. Every CLI fixture launch uses that executable, including
shell commands and harness wrappers. Debug assertions and integer overflow checks
remain enabled. Test code and unit tests use Cargo's ordinary test profile.
`cargo test --locked` runs the same assertions against the ordinary debug CLI.

The runner uses four test workers per available logical CPU, capped at 64, to overlap
subprocess waits. An explicit `RUST_TEST_THREADS` selects the worker count.

Silent-write recovery probes select a short debug-only period with
`ARC_TEST_FEED_RECHECK_MS`. A separate policy check fixes the default at thirty
seconds. Idle-read, lock-wait and timeout tests retain their ordinary timing.

The integration selector runs every test for an unclassified change. Its narrow
selections and Markdown exclusions are defined in `.arc/test-selection.py`.
