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
  derived view. Output and bundle schemas are versioned (`arc-status/6`,
  `arc-inbox/3`); a breaking shape change bumps the version.
- Approval staleness is structural: a verdict binds to the exact approved
  patchset head, and any new commit invalidates it. Never weaken this.
- The ledger is authoritative and gating; the journal is advisory and
  contextual. Claims, stages, and journal lanes are advisory liveness
  signals, never locks. Journal artifacts stay hand-writable Markdown; the
  event log is typed JSONL (`journal-events/1` in `events.jsonl`), fails
  open on malformed lines, and is versioned like every other output schema.
- The project journal lives outside the repo (`arc journal dir`) so
  worktrees stay clean; cross-session context goes there, not into
  tracked files.
- arc holds no routing opinion. It records whatever `--model`, `--actor` and
  `--harness` it is given and never selects among them, so any model in any
  harness can drive it. Delegation doctrine belongs to whoever is delegating,
  not to the ledger.

## Working here

- The repo dogfoods itself: run non-trivial changes through the ledger,
  `arc begin` to `arc integrate`. Run `arc catchup` first — the actionable
  backlog lives in the journal, not in the ledger.
- The CLI is the only workflow surface an agent needs: bare `arc` teaches the
  lifecycle and profiles, and `--help` carries each command's contract. Keep
  them accurate when behavior changes; there is no external skill to fall
  back on.
- After integrating a CLI change, refresh the installed binary:
  `cargo install --path . --locked`.
- Record behavior changes on their arc change via `arc changelog`; the
  `[Unreleased]` block in `CHANGELOG.md` is generated at release time.
