# Changelog

Notable user-facing changes, one line per integrated arc change, newest
first within each section. The append-only ledger remains the source of
truth for full detail (`arc list`, `arc show <change>`); this file is the
human-readable projection. Add a line under `[Unreleased]` as part of each
integrated change.

The format loosely follows [Keep a Changelog](https://keepachangelog.com/);
no version has been tagged yet.

## [Unreleased]

### Fixed

- Piped output dies silently when the reader goes away instead of panicking
  with a broken-pipe error (e.g. `arc list --format compact | head`)
  (`sigpipe-exit`).
- Event publication fsyncs the containing directory after linking the event
  file, preserving acknowledged entries across power loss (`store-dir-fsync`).
- Attested verification events omit unobserved exit codes and durations
  (`attested-evidence-honesty`).
- Free-text `thread journal` messages are never promoted into typed
  events, and doctor counts only artifact-shaped file fields as
  references (`journal-log-fix`).

### Changed

- The journal drops every thread spelling: no `arc thread` or nested
  `journal` aliases, and the storage tier is `ARC_JOURNAL_DIR`,
  `[journals]`, `<ai_home>/journals/`, and `journal-events/1` in
  `events.jsonl`; the legacy `journal.md` merge-read is removed, so
  existing archives need a one-time migration (`journal-clean-break`).
- The thread surface is renamed to the project journal: `arc journal` with
  nested `log` replaces `arc thread` with nested `journal`
  (`journal-rename`).
- The thread journal is now a typed `thread-journal/1` JSONL event log while
  legacy Markdown journals remain readable (`journal-jsonl`).

### Added

- `arc begin --from-journal <artifact>` opens a change from an open actionable
  journal item, stamping `journal_ref` and consuming the item as superseded;
  opt-in `[journal] auto_log` narrates begin/integrate/close into the journal
  (advisory, warns on failure); and `journal open` annotates items taken up by
  an open change (`journal-bridge`).
- `arc stats [--change | --tag | --all] [--json]` projects ledger durations
  and counts — per-change wall time, stage and gate timing, review latency,
  findings, and patchset count, plus aggregate median/p90 and suggested stage
  budgets (`arc-stats/1`) (`ledger-stats`).
- `arc log [CHANGE] [--reverse]` prints one line per ledger event; `--at
  <event-id>` on `arc show`/`arc status` replays a change to that point;
  `arc check --explain` lists every gate condition (and `--json` every
  blocker), and `arc integrate --dry-run` simulates a merge without writing
  (`timeline-views`).
- `arc diff --between ps-A ps-B` and `--since-approved` make re-review deltas
  explicit; `arc findings --format json|sarif` exports current dispositions and
  open findings for external review tooling (`interdiff-sarif`).
- `arc diff [CHANGE] [--patchset ps-NN] [--stat] [--findings] [-- <path>...]`
  renders native patchset diffs with unresolved finding-anchor drift markers
  (`arc-diff`).
- `arc config --check-writable [--json]` probes the local ledger and Git-ref
  paths before a sandboxed executor starts work (`ledger-writability-probe`).
- Status/resume field projection, discussion-event ID prefixes, and file-backed
  lane, stage, and hold text input (`status-field-projection`).
- Resumable event cursors, non-fatal event and watch command hooks, and
  first-winner multi-condition watches (`event-hooks-cursor`).
- Atomic `arc take` scheduling claims the highest-priority ready change, with
  additive priority metadata and priority-aware queue ordering (`take-next`).
- Composed transitions cover claim-and-stage, snapshot-and-verify, snapshot-and-review, implementation completion without integration, and deterministic parallel gate execution (`composed-transitions`).
- Change-worktree inference, explicit harness environment bootstrap, resumable context, and a statusline prompt (`cwd-context`).
- Opt-in repository policy rejects self-approval using snapshot and verdict actor strings (`self-approval-policy`).
- Verification captures final output tails and enforces optional per-gate process-group timeouts (`gate-evidence-output`).
- Read-only `arc doctor [--json]` ledger integrity and housekeeping checks
  (`ledger-doctor`).
- Read-only `thread doctor [--json]` archive health and housekeeping checks
  (`thread-doctor`).
- Change-scoped implementation briefs stored in the ledger and carried by
  export/import bundles (`change-briefs`).
- Shared project memory artifacts listed by `thread memories` and always
  surfaced by hot `catchup` (`thread-memory`).
- Advisory session work lanes in the thread archive: `thread lane
  open|renew|close|list`, heartbeat-free liveness from owner journal
  activity, stale takeover, and lane occupancy surfaced in `thread open`
  and `catchup` (`thread-lanes`).
- Dependency-ordered tagged series integration via `integrate --tag`
  (`series-integration`).
- Lower-priority `later` tier in the thread open queue
  (`thread-later-tier`).
- Active executor claims shown as an `in-progress` inbox bucket
  (`inbox-in-progress`).
- `verify --all` runs every gate declared for the change profile
  (`verify-all`).
- `check` reports needs-rebase from merge simulation (`check-needs-rebase`).
- Verification attestation (`verify --attest`) and the deferred
  review-note sweep (`deferrals-sweep`).
- Cold sibling archive for thread directories (`thread-archive`) and
  open-item tracking with `thread open`/`consume` (`thread-open-items`).
- Thread archive mechanics absorbed as `arc thread` (`thread-mechanics`).
- Message events and the lead-facing orchestrator inbox
  (`messaging-inbox`); messages and assignment in status JSON
  (`status-messages`).
- Forge projection: observed hosted-PR facts with fail-closed repository
  tuple validation (`forge-projection`).
- Executor claim leases and the typed stage protocol
  (`claim-stage-protocol`).
- Implementer/reviewer/lead role enforcement (`role-enforcement`).
- Event replay and follow plus watch probes (`arc-watch-events`).
- Deterministic export/import bundles, M4 (`m4-export-import`).

### Changed

- Blocker release and wedged prerequisite semantics
  (`blocker-release-semantics`).
- Delegation and blocker UX improvements (`delegation-blocker-ux`).
- Orchestration status and recovery UX polish
  (`orchestration-status-polish`).
- Destructive-action hardening and path configuration
  (`safety-and-path-config`).

### Internal

- Monolithic `tests/cli.rs` and `commands.rs` split by area (`test-split`).
