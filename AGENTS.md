# arc

Local, append-only change/review/integration ledger over plain Git for
multi-agent coding. Git owns content, branches, and history; arc owns the
collaboration objects Git lacks — changes, patchsets, findings, verdicts,
verification evidence, claims, holds, guarded merges — as one-file-per-event
JSON under `<git-common-dir>/arc/`. Non-goals: no forge or forge clone, no
daemon, no database, no network, no multi-machine ref sync.

## Invariants

- The ledger is authoritative and append-only. Events are created
  exclusively, never rewritten; every status, inbox, or list output is a
  derived view. Output and bundle schemas are versioned (`arc-status/5`,
  `arc-inbox/1`); a breaking shape change bumps the version.
- Approval staleness is structural: a verdict binds to the exact approved
  patchset head, and any new commit invalidates it. Never weaken this.
- The ledger is authoritative and gating; the journal is advisory and
  contextual. Claims, stages, and journal lanes are advisory liveness
  signals, never locks. Journal artifacts stay hand-writable Markdown; the
  event log is typed JSONL (`thread-journal/1`), fails open on malformed
  lines, and is versioned like every other output schema.
- The project journal lives outside the repo (`arc journal dir`) so
  worktrees stay clean; cross-session context goes there, not into
  tracked files. `arc thread` and the storage-tier thread spellings
  (`ARC_THREAD_DIR`, `[threads]`, the `threads/` path, `thread-journal/1`)
  are compatibility contracts for the journal surface.
- DELEGATION.md is the canon for mechanism-routing prose and is referenced
  by global agent config — edit deliberately, keep it in sync with the
  `agent-routing` manifests.

## Working here

- The repo dogfoods itself: run non-trivial changes through arc
  (`begin` → implement → `snapshot` → `review` → `verify --all` →
  `integrate`) with `ARC_HARNESS`/`ARC_SESSION` set. Gates live in
  `.arc/gates.toml`: build, test, and lint (clippy `-D warnings` +
  `fmt --check`) must all pass.
- Integration tests live in `tests/cli/<area>.rs` (assert_cmd against the
  real binary); parser/derivation logic gets in-module unit tests. Keep
  that split.
- After integrating a CLI change, refresh the installed binary:
  `cargo install --path . --locked`.
- Documentation ships with the change, not as a follow-up: update the
  affected README section, module doc comment, and the `[Unreleased]`
  CHANGELOG.md line in the same change that alters behavior.
- Docs and comments must be timeless and self-contained: never reference
  the authoring session, conversation, specs, plans, journal artifacts, or
  moment-of-writing context ("per the spec", "the drift fix", "new in
  this change") — state the invariant, not the story behind it.
