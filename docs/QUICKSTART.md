# arc quickstart

Get arc running in your own repository — install, declare gates, wire
identity, and run one change end to end.

## 1. Install

```sh
cargo install --path .        # from a checkout of this repository
```

This puts `arc` on your `$PATH` (typically `~/.cargo/bin` or
`~/.local/share/cargo/bin`). Shell completions and a man page are optional:

```sh
arc completions zsh  > ~/.zfunc/_arc          # or: bash | fish
arc mangen ~/.local/share/man/man1            # writes arc.1
```

arc is Unix-only by design (see the README portability note).

Then run `arc` with no arguments once. It prints the workflow guide — the
lifecycle, profile selection, and the invariants that change how a session
works — and `arc catchup` shows what is already waiting in this repository's
ledger and journal.

## 2. Declare gates

Gates are repo-committed commands arc runs as verification evidence. Create
`.arc/gates.toml` at the repository root:

```toml
# Every gate runs for every profile unless it lists `profiles`.
[gates.build]
command = "cargo build --all-targets"

[gates.test]
command = "cargo test"

[gates.lint]
command = "cargo clippy --all-targets -- -D warnings && cargo fmt --check"
# timeout = 600   # optional: seconds before the gate is killed and failed
```

Commit it: gate trust equals the trust of running `make` in the repository.

## 3. Choose a profile

A change's profile selects which gates are required and how it integrates:

- `local` — the default; merge into a local branch, no forge.
- `direct` — minimal ceremony for host-local work.
- `forge` — track a hosted pull request alongside the ledger.
- `release` — release-flow changes.

## 4. Wire identity

Every event records who acted. Export these per session (or pass
`--actor/--harness/--session`):

```sh
export ARC_ACTOR="Ada Lovelace"     # defaults to git config user.name
export ARC_HARNESS=claude           # claude | codex | opencode | ...
export ARC_SESSION="$(uuidgen)"     # your harness's native session id
```

`claim`, `stage`, and `release-claim` require non-empty harness and session.

## 5. Map the project journal (optional)

The cross-harness journal lives outside the repo so worktrees stay clean:

```sh
arc journal dir                     # prints the resolved directory
export ARC_JOURNAL_DIR=/path/to/journal   # or set [journals] in the config
```

Resolution checks `ARC_JOURNAL_DIR`, the longest `[journals.dirs]` path
prefix, Git discovery, a default-root journal whose `bindings.jsonl` records
the canonical current directory, and finally the default-root journal this
directory's own slug names. `arc journal dir --explain` shows which source
won. A path prefix covering directories with repositories beneath it shadows
their Git discovery, and duplicate recorded anchors are refused.

## 6. Run one change

```sh
arc begin fix-parser                # branch arc/fix-parser + a worktree
cd "$(arc show fix-parser --json | jq -r .worktree)"
# ... implement, commit ...
arc snapshot fix-parser             # record a patchset from the clean worktree
arc verify fix-parser --all         # run every declared gate as evidence
# reviewer (a distinct actor):
arc review fix-parser --verdict approved
arc check fix-parser                # exit 0 means ready
arc integrate fix-parser --cleanup  # guarded --no-ff merge into the target
```

`arc done` composes snapshot + verify + check in one call. `arc resume`
prints a change's brief, live state, and journal context to pick work back up.

## 7. Sandboxing across repos

Point ledgers at an isolated tree instead of each repo's Git dir so a
sandboxed executor never needs write access to `.git`:

```sh
export ARC_DATA_ROOT=~/.local/state/arc      # per-repo slug subdirectories
arc config --check-writable                  # fail fast if paths are read-only
```

With a `data_root` configured, `arc workspace list|inbox` aggregates every
repo's changes in one view.
