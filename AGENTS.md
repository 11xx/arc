## The reference is the binary

`arc` with no arguments prints a bounded capability map. `arc guide` indexes
workflow topics; `arc guide <topic>` and `arc <verb> --help` hold the full
contracts. The binary is the reference; no other documentation tree defines
operating rules.

- Each unique rule has one canonical home: the map, a topic, or command help.
  Other surfaces cross-reference it.
- A behaviour change updates every affected contract in its canonical topic or
  command help in the same change. Update the map when a required bootstrap
  rule, entry route or capability family changes. Keep the fixed ceilings:
  map 80 lines / 6,144 UTF-8 bytes, topic 16,384 bytes, root help 12,288 bytes.
- A shape change follows the SCHEMAS rule in `arc guide schemas`: one new
  version per release, counted from the last release.

## Working on arc

- After integrating a CLI change, reinstall; the binary on `PATH` is not the tree.
- `CHANGELOG.md` is generated. Record a behaviour change's entry on its change
  with `arc changelog`; never edit the file by hand.
- [`arc-model`](https://github.com/11xx/arc-model) is an independent
  semantic model of integration authorization, derived from the guide and
  `--help` rather than from this source. Nothing here builds or gates on it.
  A change to what integration permits or refuses is one it must be re-pinned
  to.

## Gate boundary

Integration gates protect named invariants, not the test inventory. The exact
witness list, invariant and caller-visible claim live in `.arc/invariants.tsv`.
A missing, ignored or unexecuted name fails the invariant gate.

Adding or replacing a witness requires naming its invariant and bad state in its
doc comment and list entry, demonstrating failure for that state first, then
passing at the head. A replacement proves the same boundary before removing its
predecessor. Test existence and historical catches are not selection criteria.

Area suites run on demand with `python3 .arc/test-binary.py area cli <module>`
(or `area unit <module>`), using the exact module path. They belong to no
integration gate; publication runs them with the rest of the tree.
The invariant gate is the only integration gate. It needs the crate built to
run its witnesses, so it is also the build check. The build is declared as
preparation in `.arc/worktrees.toml`: arc runs it before the gate and times it
apart, and the gate's 10 s budget judges the witnesses. A check joins it only as a witness of something
that must never pass; a budget overrun is answered by the order of work in
`arc guide gate-rounds`, with a larger budget last.
Publication adds clippy, formatting, five release-artifact checks against
the release-profile executable, and the whole remaining suite with
`cargo test --locked`. A failure there is fixed, or the test removed, before
the release. Run `cargo clippy --all-targets -- -D warnings` and `cargo fmt`
while working: nothing checks them before publication.
Runner details and commands are in `tests/README.md`.

## Releasing
- CalVer. A version is its release date, `YYYY.M.D` with no leading zeros,
  prerelease, or build metadata. Cargo rejects a fourth field, so a second
  release waits for the next date. `arc --version`, the manifest, and the
  changelog's top release heading agree.
- The package is `arc-ledger`; the binary, repository, and command are `arc`.
- `agent-tapes-core` comes from [tapes](https://github.com/11xx/tapes) by
  `git` and `rev`. The `version` beside the rev is the release that rev
  carries; move both together.
- `publish = false` stays until the tapes versions it requires are on the
  registry. Publish only when the operator asks.

Before tagging a release:

1. The release head and its tag are signed by the operator's key. Work done on
   a machine without that key stays unsigned and enters only through a merge
   signed on the machine that holds it.
2. `arc catchup` shows no outstanding review debt, or each remaining one is
   waived with a recorded reason.
3. `arc publication --target master` passes at the release head before tagging.
4. The guide, `--help`, and SCHEMAS agree with the behaviour being released.
5. `[Unreleased]` is cut into a section headed with the version and date.
6. The release head is tagged; the changelog projection measures from it.
7. The manifest names the repository URL.
8. A crates.io release first removes `publish = false`, and
   `cargo publish --dry-run` is clean.
