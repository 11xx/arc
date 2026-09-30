## The reference is the binary

`arc` with no arguments prints the guide and
`arc <verb> --help` is each command's contract (clap doc comments). There is
no other documentation tree, so a rule stated anywhere else is one no session
reads.

- A behaviour change updates the guide and every affected `--help` in the
  same change.
- A shape change takes the next schema version.

## Working on arc

- After integrating a CLI change, reinstall; the binary on `PATH` is not the tree.
- `CHANGELOG.md` is generated. Record a behaviour change's entry on its change
  with `arc changelog`; never edit the file by hand.
- [`arc-model`](https://github.com/11xx/arc-model) is an independent
  semantic model of integration authorization, derived from the guide and
  `--help` rather than from this source. Nothing here builds or gates on it.
  A change to what integration permits or refuses is one it must be re-pinned
  to.

## Releasing
- PEP 440. A version is its publication date, `YYYY.M.D` with no leading zeros
  and no fourth field, prerelease, or build metadata. One release per
  date. `arc --version`, the manifest, and the changelog's top release heading
  agree.
- The package is `arc-ledger`; the binary, repository, and command are `arc`.
- `agent-tapes-core` comes from [tapes](https://github.com/11xx/tapes) by
  `git` and `rev`. The `version` beside the rev is the release that rev
  carries; move both together.
- `publish = false` stays until the tapes versions it requires are on the
  registry. `cargo publish` is the operator's act, never a session's.

Before tagging a release:

1. The release head and its tag are signed by the operator's key. Work done on
   a machine without that key stays unsigned and enters only through a merge
   signed on the machine that holds it.
2. `arc catchup` shows no outstanding review debt, or each remaining one is
   waived with a recorded reason.
3. The guide, `--help`, and SCHEMAS agree with the behaviour being released.
4. `[Unreleased]` is cut into a section headed with the version and date.
5. The release head is tagged; the changelog projection measures from it.
6. The manifest names the repository URL.
7. A crates.io release first removes `publish = false`, and
   `cargo publish --dry-run` is clean.
