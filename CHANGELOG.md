# Changelog

Notable user-facing changes, one line per integrated arc change, newest
first within each section. The append-only ledger remains the source of
truth for full detail (`arc list`, `arc show <change>`); this file is the
human-readable projection. Add a line under `[Unreleased]` as part of each
integrated change.

The format loosely follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added

- `arc keep --kind verified|rejected|constraint|hypothesis` records a fact the
  work discovered, and `arc resume` hands it back under `## Kept Context`, so a
  compacted or cold session does not re-derive it or re-try an approach already
  rejected. Carried on `arc status --json` as `kept` (`intentional-context`).

- Each journal artifact kind has its own subcommand (`arc journal todo`, `arc journal handoff`, `arc journal decision`, …), making `arc journal --help` the kind registry; `note --kind` stays canonical underneath and now defaults to `note`.
- `arc journal questions [--json]` lists every question waiting on a person, with its options and argued branches; `arc catchup` reports them first. `arc journal answer --other "<answer>"` settles a question outside the options it offered.
- `arc review --verdict approved --provisional "<why>"` records a verdict that gates but is owed corroboration. Independence and approval staleness are unchanged; the change carries a recorded obligation until somebody else supplies a second judgment — an independent approval of the same patchset, or an audit — and neither the reviewer being corroborated nor the change's author can supply it. `arc query --provisional` lists the outstanding ones, `arc status` reports `provisional_approval_outstanding`, `arc check` names the reason, and the merge's authorization basis records that it rested on a provisional verdict.
- `arc journal latest <topic> [--kind <kind>] [--json]` resolves the newest artifact filed under one topic, hot storage before the cold archive.
- `arc fr <topic>` files a feature request in the project journal — the top-level alias for `arc journal note --kind feature-request`. Alongside it, `arc log` accepts `--oneline` and points at `git log` instead of failing argument parsing, and a change that cannot be inferred now names the open changes and their worktrees.
- `journal question`, `journal answer`, and `position --question/--option`: a discussion can hold a question only a person settles, answered `opening` before anyone argues or `closing` once the argument is in, with positions argued under each option so a closing choice is made between explored branches rather than labels.
- Bare `arc` now teaches the discussion path — `journal note --kind discussion`, `journal position`, `journal discussion`, `journal consume --decision`, and `begin --from-journal` — beside the change lifecycle it already taught.
- `arc history rewrite` records a Git history rewrite as a repository-scoped fact carrying the operator's commit map, and `arc history resolve` follows a recorded revision forward. `doctor` reports moved revisions as `revision-rewritten` rather than dangling, and `diff` renders the surviving commit. Events are never migrated.
- A guarded merge records the authorization basis it was taken on: the approving verdict, per-gate evidence, prerequisite closures, the empty blocking-finding and hold vectors, and the normalized gate and policy values actually consumed. `arc integrate --dry-run` prints it without writing.
- `arc diff <change> --integrated` renders the exact range an integration recorded — where the target stood before, to the commit that landed — which is the range an audit reviews. Closures with no recorded range take an explicit `--base`.
- `arc workspace` no longer requires a configured `data_root`. Without one, projects are enumerated from the journal root — each journal records the project it belongs to — so `list` and `inbox` work under the default per-repository layout, and `arc begin` registers a project so opening a change is enough to make it discoverable.

  `arc workspace backlog` reports ledger and journal backlog across every known project: changes awaiting a verdict, audit debt outstanding, the journal's three tiers, and the oldest open item. Projects rank by what is blocked; items are never ranked across projects. A journal whose project can no longer be found is reported with the `journal rebind` that adopts it, rather than skipped. `--since` turns it into a delta of arrivals, with blocked work still reported in full.
- Locally executed verifications record the worktree tree they ran against and whether it was dirty; dirty or mid-run-changed evidence is displayed but never counts as green; and a patchset re-snapshotted at the same head is a rebind rather than a rework round.
- `arc doctor` reports recorded revisions Git can no longer resolve, `arc watch --json` names the change, condition and satisfying event (and reports a timeout as JSON), and `arc events --tag` follows a tagged program as one stream.
- Events, patchsets and verdicts record `actor_source` (`flag`, `env`, or `git-fallback`); a fallback identity is announced on stderr before it is recorded; `forbid_self_approval` fails closed when either side's identity was never declared; and an opt-in `[policy] require_declared_actor` refuses to append an event whose author nobody claimed.
- `arc stats --by-model` reports one row per delegated identity — changes touched, patchsets contributed, rework rounds opened, verdicts issued — keyed on the `--on-behalf-of` subject, with unattributed work in its own row.
- A journal records the project it belongs to in `bindings.jsonl`; `arc journal doctor` reports an orphaned or split journal; and `arc journal rebind <dir>` adopts an orphan, recording the move and refusing a target that already holds content.
- `arc brief` warns when an acceptance probe runs a gate the change must pass; `arc check` and `arc status` name a probe that cannot discharge because its brief has no usable base; `--probes-json` accepts inline JSON; `arc verify --gate` points at a declared probe of the same name; and `--change` works wherever the change positional is optional.
- Finding bodies and anchors now reach every read surface: `arc findings` prints them, `arc status --json` carries body, anchor, patchset and reporter, SARIF carries the body as its message, `arc log` renders findings filed inside a review batch, and blocking findings name the patchset they predate.
- `arc journal note --kind discussion` prepends the discussion scaffold by default (`--no-scaffold` opts out), and `arc journal discussion` reports position blocks that state no stance as `unstated` instead of silently undercounting the tally.
- The audit-debt waiver now binds to the patchset it was declared for, so a single declaration no longer excuses self-approval for the rest of a change's life. Outstanding obligations surface in `arc inbox` (new `audit-owed` bucket, `arc-inbox/3`), `arc catchup`, `arc doctor` (`audit-debt-outstanding`), and `arc chain` (`arc-chain/2`, with per-reviewer coverage of the final patchset). `arc findings --audit` lists post-integration findings. An approving audit is refused from the identity that authored the work.
- Review coverage (`review_map`) reports which patchset each reviewer identity actually saw, with advisory warnings in `arc check` and `arc show` when nobody independent covers the final patchset. New `arc audit` records a post-integration review against the integrated revision as a distinct event, and `arc integrate --audit-debt`/`arc audit-debt` record a review a change owes so it can ship without an independent verdict; `arc query --audit-debt` lists outstanding obligations. `arc review` now reports an approval that cannot gate instead of silently recording it. Status schema is `arc-status/6`.
- `arc catchup` orients a session with ledger buckets, journal backlog, lanes, and memories in one call. `arc inbox` now names the journal backlog beside its ledger buckets (`arc-inbox/2`). Bare `arc` prints a workflow guide covering the lifecycle, profile selection, and the invariants that change what a session does. `arc journal open` explains its three tiers.
- `arc watch --tag <tag>` watches a whole series, returning on the first member to reach a condition (`--any`, which names it) or once every member has (`--all`) (`watch-tag-scope`).
- `arc config --check-writable` also probes commit capability in a throwaway repository, signing the probe only when the repository resolves `commit.gpgsign` to true and ignoring a global policy the repository overrides; failure details carry the underlying cause (`preflight-commit`).
- A brief version records why it exists: `--caused-by finding:<id>|verdict:<event>|blocked-on:<event>` and `--cause-note`, required from v2 and validated against the referenced object's kind (`brief-causes`).
- Require blocked-on stages to identify a canonical brief, finding, change, or external referent.
- Derive completed rework rounds and first-pass approvals from patchset and verdict history.
- Require typed root causes for requested-rework verdicts and tally them in review statistics.
- Declare brief-bound acceptance probes and record typed baseline and final evidence.
- Correlate multi-gate verification with manifests, terminal results, and explicit evidence reuse.
- Bind snapshots to the exact brief version they implement.
- Anchor each brief version to the full revision it was checked against.
- Configure the built-in changelog target with contained writes and safe stdout fallback.
- Preserve complete changelog recording provenance in JSON and optional human projections.
- Resolve journals from explicit path-prefix anchors and explain the selected source.
- Report registered worktrees that belong to closed changes without removing them.
- Add `arc chain <tag>` text and JSON views that show dependency-ordered members, plan history, and the next ready change.
- Link briefs to optional plan artifacts and slices, preserving the association across status output and bundle round-trips.
- Render discussion positions grouped into rounds by reply depth, name the participants of each round, and flag positions nothing has answered. Derived entirely from existing `position_id`/`ref` data — no new event, field, or schema version.
- Add terminal `decision` journal artifacts and let resolved discussions link to them with `journal consume --outcome done --decision <filename>`.
- Add read-only `arc metadata <change>` text and `arc-metadata/1` JSON projections, including for closed changes.
- Add a read-only `arc review <change>` view with verdict history, review findings, approval freshness, next action, and versioned JSON output.
- Add opt-in `arc rescue --transcript [--tail N]` output for bounded recovery of the claimed session's latest operator turns.
- Add `arc rescue` to report abandoned foreign-session work and safely take over stale claims.
- `arc changelog` records change-scoped release copy in the ledger and projects
  integrated entries into the generated `[Unreleased]` block, eliminating
  cross-change changelog conflicts (`changelog-projection`).

### Changed

- `arc verify` refuses to run a gate or a final acceptance probe away from the change's head, where the evidence would never be counted, and names the step that fixes it — a worktree path, `git checkout`, or attestation. Baseline probes keep their own base-revision rule; attested evidence warns instead of refusing.
- `arc journal consume` refuses while a typed question on the artifact is unanswered (`--drop-questions` overrides); `arc begin --from-journal` warns and names the open question ids.
- Integration is recorded as `change-integrated` (arc guarded the merge) or `integration-asserted` (somebody else performed it), both naming the patchset, head, target branch and prior target state. `arc close --integrated` becomes `--assert-integrated`; `change-closed` now carries only abandoned and superseded outcomes, and historical integrated closures read as `legacy-unclassified`.
- `arc check` reports typed advisories (`arc-check/2`) in place of untyped coverage warnings, including `brief-author-only-review` and `audit-debt-outstanding`. Advisories never change readiness or the exit code.
- The `sol-low` brief scaffold teaches the differential probe rule — declare probes, observe the baseline fail and the head pass, and treat the absence of probes as no contract recorded — replacing the stale claim that probe evidence never gates integration.
- `arc chain --review` reports `brief_author` and `reviewed_only_by_brief_author` in place of `non_self_verdict`, which measured identity inequality and reported it as independence (`arc-chain/3`).
- Holds are independent: `arc hold` prints the event identifying the hold it set, `arc release-hold <change> <hold>` releases exactly that one, and other holds stay in force. Status reports every active hold (`arc-status/7`: `hold` becomes `holds`; `blocker_summary.hold.reason` becomes `reasons`).
- `arc workspace list` and `inbox` say when they have nothing to show, distinguishing an empty registry from one whose projects have no ledger, instead of printing nothing.
- `arc review --patchset` accepts the revision a reviewer read, not only a patchset id, and refuses an ambiguous or unknown one rather than falling back to the latest. A verdict recorded without it claims the newest patchset, which is not always what was reviewed.
- Closed-change append refusals now come from the permission table rather than twelve hand-placed guards, so the refusal names which closure state applies and why (`change <id> is <outcome>; event is open-only`) instead of a bare "is closed" (`append-policy-single-authority`).
- Require declared acceptance probes to fail at their brief base and pass at the patchset head.
- Accept free-form changelog categories while rendering conventional and custom headings deterministically.
- Permit immutable observations, discussion, and liveness cleanup after change closure while freezing work state.
- Rename `arc journal append` to `arc journal position` as a clean break while preserving the typed `position` event and artifact format.
- Retire the done, inbox, and spec journal artifact kinds for new notes while preserving historical parsing and reporting them as doctor advice.
- Prompt sol-low delegated briefs to define runnable acceptance probes, record final-head evidence, and stop rather than edit a defective probe.
- Allow one plan artifact to reference multiple changes without automatic consumption or brief seeding.
- Allow review verdicts to carry optional reasoning from inline text or a file, and expose it in show, log, and status output.

### Fixed

- `arc journal discussion` no longer skips every position after one whose body leaves a Markdown fence unclosed, which under-reported both the stance tally and the per-branch argued counts.
- A status report derived from the ledger alone now resolves the danger scope instead of assuming it: `git diff` needs the objects the recorded base and head name, not a working tree, so `arc status --at` no longer over-reports that an independent verdict is required. `arc doctor` also reports a declared danger path naming a directory — it exists, so the previous check passed it, and it can still never match, since declared paths are compared against changed files.
- Audit debt is discharged by any independent verdict on the revision that shipped, recorded after the debt was declared, rather than only by `arc audit`. Reviewing before merging no longer forces a choice between a standing debt for a review that happened and a post-integration audit that did not. Reviewer independence is judged against the patchset the reviewer read rather than the newest one, so a later snapshot by another author no longer relabels an earlier self-review as independent.
- Question settlement now enforces discussion-only targets, argued closing branches, immutable answered branches, serialized concurrent transitions, and semantic validation of hand-written events. Audit-debt waivers bind to the exact branch head, and waiver-only integration records advance and repair the store-format barrier. The workflow guide prints complete, mutually exclusive resolution and promotion commands.
- `journal discussion` now reports `unplaced`: position blocks the reply graph cannot see, because they carry no `position_id`. The tally reads the file and the graph reads the event log, so a hand-written block — or one recorded before ids existed — was counted and never reportable as answered.
- PR lifecycle facts bind to the link and head they were observed at. After a relink, `pr_state` reads unknown and `forge_ready` false until the new PR is observed, instead of inheriting the superseded PR's state. `arc forge pr-state` requires a recorded link and takes the head from it; `--link` names it explicitly.
- A gate that is not green at head now says why on every human surface (`resume`, `rescue`, `check`), instead of rendering the raw `pass` result that contradicted readiness. Dirty or moved-tree evidence advises `clean_worktree:<gate>` rather than a rerun that would record the same unusable evidence.
- `arc journal doctor`'s split-journal advice reads as one sentence, so the rebind command inside it can be copied as printed.
- Local gate evidence now requires a retained tested tree and known-clean provenance before it can be green or reused; shared parallel runs remain provenance-unknown and non-green, and failed tree pinning stays visible as unknown. Discussion docs and scaffolds teach the current position command and stable IDs.
- Reject imported future-tag events with incomplete envelopes before they can leave typed ledger reads unusable.
- Harden audit debt selection and role boundaries, preserve attributed review coverage, replay audit state through bundles, filter owed-review inbox rows consistently, and let audit findings be discussed and dispositioned without mutating shipped findings.
- Render every projected changelog entry as a list item, leaving authored markers and wrapped continuations intact (`changelog-render-normalise`).
- Require and retain the exact external execution context for attested verification.
- Allow changelog entries to be recorded after a change has been integrated.
- Group repeated doctor advice and omit historical expired claims from closed changes.
- Preserve displaced claim identity whenever stale or expired work is replaced.
- Reject malformed changelog events during bundle import before writing ledger events or retention references.
- Preserve additive findings JSON and attach replies deterministically by finding identity regardless of ledger event ordering.
- Restore retired journal kinds as read-side list and open filters, keep discussion derivation consistent with recognized events, version discussion summary JSON, and preserve capped depth for deep reply chains.
- Show replies beneath their findings and include structured reply threads in findings JSON.
- Allow projects with a shared Git identity to omit inapplicable delegated provenance mismatch warnings.
- Recompute each new patchset base from its head and target branch so rebased changes render only their own diff.

## [0.1.0] - 2026-07-20

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

- Role refusals name the role that can perform the command, and top-level
  nested leaf verbs suggest their complete command path
  (`cold-start-ergonomics`).
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

- `arc completions <shell>` and `arc mangen <dir>` generate shell completions
  and a man page; `docs/QUICKSTART.md` and a README Installation section cover
  foreign-repo setup; arc is documented as Unix-only (`release-polish`).
- `arc workspace list|inbox` aggregates open changes and inbox rollups across a
  configured `data_root`; `arc brief --scaffold <sol-low|sol-high|reviewer>`
  (or a repo `.arc/templates/<name>.md`) prepends a delegation-fenced template;
  and `arc restack <change> --advise` prints the exact rebase commands for open
  dependents without executing them (`workspace-scaffolds`).
- `arc verify --all --skip-green` skips gates already green at the exact
  current head (observed or attested), printing `skipped (green at head)` per
  gate, to avoid re-running expensive gates. A measurement (300 changes:
  `list` ~8.5 ms, `inbox` ~8.0 ms — well under budget) showed derived views
  need no on-disk cache yet, so the closure/summary caches are deferred
  (`view-cache`).
- Opt-in Git hook pack: `arc hooks install|uninstall|status` manages
  `post-commit` (stale-approval and closed-branch notices) and
  `prepare-commit-msg` (`Arc-Change:` trailer) scripts that always exit 0, plus
  `arc query --commit <rev>` to find changes by patchset or integration commit
  (`git-hooks-changeid`).
- Global `--on-behalf-of <subject>` (`ARC_ON_BEHALF_OF`) records the subject a
  lead runs delegated ceremony for while `actor` stays the invoker; the
  effective author (`on_behalf_of.unwrap_or(actor)`) drives `forbid_self_approval`
  and is rendered in show/log/status. Claim ownership still matches the invoker
  tuple. The event field is additive (`ceremony-provenance`).
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
