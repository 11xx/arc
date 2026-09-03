# Changing arc

## Changing arc

Rules for working on arc itself, as distinct from working with it.

**Drive non-trivial work through arc.** The tool is its own first
consumer, and a lifecycle nobody runs is a lifecycle nobody tests. Open a
change, record a brief, snapshot, verify, review, and integrate through
the guarded path. Only a one-line, self-evident fix is worth exempting.

**Reinstall after integrating a CLI change.** The binary on `PATH` is not
the tree; until it is replaced, arc reports the ledger with the code the
change just replaced:

```sh
cargo install --path . --locked
```

**A shape change bumps its schema version.** Every projection, report,
and bundle is versioned so a consumer can program against it, and any
change to what one emits takes the next version — adding a field as much
as removing, renaming, or redefining one. A reader that pinned a version
can then tell from the version alone whether the shape it parsed is the
shape it is holding. Mark the new field with the version that introduced
it, the way the surrounding fields are marked, and update every place the
version is quoted in prose — a version quoted in three documents goes
stale in three documents.

A stored input format is the exception, and `journal-events/1` is the one
that exists. Its version marks what a reader must accept, not what a writer
emits, so a new optional field that leaves every older file valid keeps the
version. Removing a field, or making one required, does not.

**Record a behaviour change as a changelog entry on the change that made
it.** The `[Unreleased]` block is projected from those entries at release
time; a behaviour change with no entry is a behaviour change the release
notes cannot mention. Integration says so as advice when a change closes
with no entry recorded, naming the command that records one; which changes
deserve a release line is the author's call, not arc's.

The projection owns only the lines it can produce. `arc changelog --write`
declines to replace a block holding a line no recorded entry produced, and
names each one, because that prose lives in the file and nowhere else.
Either record it on the change that made it, or pass `--keep-unrecorded`,
which writes the projected entries below the lines it kept under an
`<!-- unrecorded -->` marker.

**Keep the instruction surfaces accurate.** arc teaches its own use, so the
guide `arc` prints with no arguments, each command's `--help`, and the pages
under `docs/` are load-bearing rather than decorative. A change that alters
behaviour updates them in the same change. The CLI is the surface an agent
reads, so a rule stated only in `docs/` is a rule the session acting on it
never sees; `tests/cli/docs.rs` holds the two surfaces together by asserting
that every exit code and every refusal or guarantee sentence in `docs/`
appears in the guide or in a command's help.

## Releasing

**A version is the calendar date of its publication.** Releases are named
`YYYY.M.D`, the date written as the three numeric fields Cargo's semver
parser accepts and nothing more: no leading zero on the month or the day,
and no fourth field, prerelease, or build metadata, all of which Cargo
either rejects or orders in a way a date does not. One release is cut per
date; a second waits for the next date rather than qualifying a version.
`arc --version`, the manifest, and the changelog's top released heading all
carry the same string.

The checklist a release passes, in order:

1. Every commit reachable from the release head is signed by one key
   (`git log --format=%G? <head>` is `G` throughout).
2. `arc catchup` reports no outstanding review debt, or each remaining one
   waived with a reason recorded on the ledger.
3. `docs/schemas.md` names which schema versions are commitments, and the
   guide and `--help` text agree with the behaviour being released.
4. The `[Unreleased]` block is cut into a section headed with the release
   version and its date, leaving `[Unreleased]` empty above it.
5. The release head is tagged. Until a tag names it, the changelog
   projection has no boundary to measure from and offers the whole history
   as unreleased.
6. `cargo publish --dry-run` is clean, and the manifest names a repository
   URL so the packaged crate points somewhere.

Publication itself is the operator's act: `cargo publish` is never run from
a session.

## Non-goals

Not a forge or forge clone, no hosted-PR parity claim, no daemon, no web
UI, no database, and no automatic multi-machine synchronization. arc never
makes a network call. Every command reads and writes the local repository and
nothing else, and `git` is the only program it invokes on its own behalf — a
declared gate command is the project's, not arc's.
Export/import moves the ledger as one file; Git objects still travel
separately. A forge-PR projection is planned, while shared Git-ref
sync remains deferred until a real concurrent multi-machine need exists.

## Roadmap

Shipped: local core +
policy engine (M1–M2), agent-facing orientation surfaces (M3), deterministic export/import
bundles (M4), local orchestration foundations (dependencies, tags,
actionable status, query, and batch views), claims/stages and execution
roles, messaging and the inbox rollup, project-journal mechanics, and the
ledger side of forge projection (declare/link/checks/pr-state with
fail-closed validation). Remaining evidence-driven possibilities live in the
journal's open items (`arc journal open`).

