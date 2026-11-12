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

The integration selector runs every test for an unclassified change. Its narrow
selections and Markdown exclusions are defined in `.arc/test-selection.py`.
