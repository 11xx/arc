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

- Attested verification events omit unobserved exit codes and durations
  (`attested-evidence-honesty`).
- Free-text `thread journal` messages are never promoted into typed
  events, and doctor counts only artifact-shaped file fields as
  references (`journal-log-fix`).

### Changed

- The thread surface is renamed to the project journal: `arc journal` with
  nested `log` replaces `arc thread` with nested `journal`, both old
  spellings remain as aliases, and storage-tier names (`ARC_THREAD_DIR`,
  `[threads]`, the `threads/` path, `thread-journal/1`) keep the legacy
  spelling as compatibility contracts (`journal-rename`).
- The thread journal is now a typed `thread-journal/1` JSONL event log while
  legacy Markdown journals remain readable (`journal-jsonl`).

### Added

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
