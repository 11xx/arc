# arc

A local context and workflow-state manager for agentic coding, over plain
Git. Git owns content and history; arc owns changes, patchsets, briefs,
findings, verdicts, gate evidence, holds, the journal, and guarded
integration.

## Non-goals

No forge or forge clone, no hosted-PR parity, no daemon, no web UI, no
database, no automatic multi-machine synchronization. arc makes no network
call, and `git` is the only program it runs on its own behalf; a declared
gate command is the project's, not arc's. Shared Git-ref sync waits for a
real concurrent multi-machine need.

## The reference is the binary

`arc` with no arguments prints the guide (`src/guide.rs`), and
`arc <verb> --help` is each command's contract (clap doc comments). There is
no other documentation tree, so a rule stated anywhere else is one no session
reads.

- A behaviour change updates the guide and every affected `--help` in the
  same change.
- A shape change takes the next schema version, and updates that surface's
  line in the guide's SCHEMAS section; `tests/cli/schemas.rs` holds the
  schema constants to it. `journal-events/1` is a stored input format and is
  versioned from the reader's side, as SCHEMAS says.
- `src/commands/contribution-trailers.md` is product content: `arc
  instructions git` prints it verbatim.

## Working on arc

- Drive non-trivial work through arc: begin, brief, snapshot, verify, review,
  integrate. Only a one-line, self-evident fix is exempt.
- Gates are `.arc/gates.toml`: build, test, and lint (clippy with
  `-D warnings`, then `cargo fmt --check`). `make build|test|lint` runs the
  same commands.
- `.arc/policy.toml` declares the danger paths and closes `src/`: a new file
  under `src/` is classified in `paths` or `acknowledged_safe` in the same
  change, or `arc doctor` fails.
- After integrating a CLI change, reinstall (`cargo install --path .
  --locked`); the binary on `PATH` is not the tree.
- `CHANGELOG.md` is generated. Record a behaviour change's entry on its
  change with `arc changelog <change> --category <c> --body-file <f>`; never
  edit the file by hand.
- [`arc-model`](https://github.com/11xx/arc-model) is an independent
  semantic model of integration authorization, derived from the guide and
  `--help` rather than from this source. Nothing here builds or gates on it.
  A change to what integration permits or refuses is one it must be re-pinned
  to.

## Releasing

- A version is its publication date, `YYYY.M.D` with no leading zeros and no
  fourth field, prerelease, or build metadata. One release per date. `arc
  --version`, the manifest, and the changelog's top release heading agree.
- The package is `arc-ledger` (the bare name is another crate's); the
  binary, repository, and command are `arc`.
- `agent-tapes-core` comes from [tapes](https://github.com/11xx/tapes) by
  `git` and `rev`. The `version` beside the rev is the release that rev
  carries; move both together.
- `publish = false` stays until the tapes versions it requires are on the
  registry. `cargo publish` is the operator's act, never a session's.

Before tagging a release:

1. Every commit reachable from the release head is signed by one key
   (`git log --format=%G? <head>` is `G` throughout).
2. `arc catchup` shows no outstanding review debt, or each remaining one is
   waived with a recorded reason.
3. The guide, `--help`, and SCHEMAS agree with the behaviour being released.
4. `[Unreleased]` is cut into a section headed with the version and date.
5. The release head is tagged; the changelog projection measures from it.
6. The manifest names the repository URL.
7. A crates.io release first removes `publish = false`, and
   `cargo publish --dry-run` is clean.
