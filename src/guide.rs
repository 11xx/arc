//! Stateless capability map and semantically owned workflow topics.
//! Operation-specific contracts live in command help.

pub const GUIDE: &str = r#"arc — persistent context and guarded workflow state over plain Git for agentic coding arcs.

arc is a local context and workflow-state manager for agentic coding. It
keeps a shared account of what is being worked on, what has been learned or
decided, what remains to be done, and what is actually safe to integrate.
A cold session reconstructs project and workflow context from the journal
and change ledger shared across worktrees and harnesses.

Git owns content, branches, and history. arc owns changes, patchsets, briefs,
handoffs, findings, verdicts, verification evidence, holds, and guarded
integration. Review verdicts bind to exact patchsets; integration checks
findings, holds, and required verification gates before merging.
Run `arc catchup` for live project state and `arc journal open` for work
waiting for a session.
Run `arc guide` for indexed topics, `arc guide <topic>` for depth, or
`arc guide --search <text>` to find a topic or command-help route.

SAY WHO YOU ARE (before the first write)
  eval "$(arc env)"                    Detect harness, session, model, and session link.
  export ARC_ACTOR=<name> ARC_HARNESS=<claude|codex|opencode|pi> \
         ARC_SESSION=<id> ARC_SESSION_LINK=<url>

  `arc env` prints a build comment naming the compiled commit and whether
  tracked files differed at build time (clean or dirty). Builds outside Git
  report unknown. `arc --version` prints the release version alone on its
  first line and `build: <commit> (clean|dirty)`, or `build: unknown`, on its
  second; `-V` prints the release line alone.

  `arc env` leaves ARC_MODEL unset. Each write resolves the acting session's
  model. Declare one with `--model` or export ARC_MODEL; the declaration is
  recorded with any observed disagreement. `arc show --json` and `arc log`
  show the model evidence.

  A Codex model selection outside the transcript tail is sought through at
  most sixteen 1 MiB metadata pages per invocation, in addition to the normal
  store and transcript reads. Private checkpoints under
  `<ai-home>/arc/cache/model-lookups` retain the selection or backward cursor
  bound to the recording's session, source and stat revision. An unchanged
  recording resumes; changed recordings restart. Rerun `arc env` after a
  bounded lookup to continue. Missing selections, skipped records and source
  changes remain explicit rather than attributing an older cached model.

  Omit a value nobody established; never declare a placeholder. A write
  refuses a harness, session, actor or on-behalf-of subject that is one
  (unknown, unset, none, null, n/a, na, tbd, undefined, ?, -, <synthetic>,
  synthetic), an actor of the form harness:model carrying one, and a model
  that is one or is a session id. Reads and Git hooks never refuse.
  Agent harness, model and supplied effort segments must be nonempty.
  An actor written as harness:session requires a nonempty session.

  Text views validate recorded producer models. A missing model, a placeholder
  or a session id prints as `<harness> (model not recorded)`, or
  `model not recorded` when no harness is established. Valid model and effort
  spellings retain their coordinates. JSON production identities, journal
  artifact and entry writers, filed and recent sessions, suggestions,
  verification stamps, review coverage, and audit and verdict details retain
  the raw model value with `model_recorded: false` when it names no model;
  valid models omit that flag.
  Workspace report debt rows retain the raw implementer value with the same
  optional flag. Review coverage retains the originating session and validates
  the model against it; coverage a debt declaration captured before it carried
  a session reads that session from the verdict it was captured from. Audit
  and verdict details validate models against their recorded sessions in text
  and JSON.

  Local change, repository and replica IDs are totally ordered by issuance
  across processes sharing one ledger. Ordinary issuance and publication use
  one ledger-wide lock and a durable newest-issued-ID marker. Appends read constant-size
  bookkeeping; missing or lagging bookkeeping is recovered by scanning the
  event namespaces once. Observation timestamps retain the recording clock.
  Issuance refuses an exhausted or noncanonical ULID floor rather than writing
  an event that sorts below held history.
  Non-event identifiers and scratch names use an independent random ULID generator.

  A provisional intent durably reserves its exact event ID before binding Git
  refs. Later local IDs sort after the reservation and may publish before it.
  ID cursors wait below the oldest reservation that is neither published nor
  abandoned by recovery. Recovery releases an unapplied reservation when its
  change is abandoned and the saved refs still prove no transition occurred;
  its intent remains evidence. The ledger ID lock never spans Git, hooks or network.

  Imported change and replica events keep their origin IDs. They may arrive
  below IDs already read. State, rewrite, relation, candidate, pass, log and replica
  views replay the held history, so an import can revise a previous projection;
  ID order across origins does not promise arrival or causal order. Historical
  `explain --at` views likewise describe the history held when read.

  `events --since ID` applies an ID boundary to its initial replay. Replay and
  follow withhold events at or above an unresolved reservation without marking
  them seen. A supplied cursor at or above that reservation refuses and names
  the boundary; resume below it or replay without --since. Follow
  emits every subsequently discovered matching event, including imports below
  that cursor, sorted within each read. A restart with --since excludes older
  imports already present; recover them by replaying without --since and
  deduplicating by event ID. A delta export's --since checksum binds an exact
  history prefix. Change exports stop below unresolved reservations, so a
  prefix cannot cross an event awaiting publication. An import that changes
  that prefix requires a full export.
  `messages --since` filters recording timestamps, which imports preserve;
  recover older imported messages by reading without that filter. Other
  timestamp views describe recorded clocks rather than local arrival order.

  Every event records who wrote it. Most writes accept an undeclared
  identity. In that case arc records an actor nobody claimed:
  `<harness>:<session>` when both are known, else `git config user.name`.
  Either is assumed rather than declared, so it cannot be the independent
  party to an approval. When neither exists no actor is established: a write
  refuses, naming `--actor` and `ARC_ACTOR`, and a read proceeds. A harness
  or session nobody established is left off a journal event, never recorded
  as a word standing in for it.
  The ledger's append guard reads `require_declared_actor` from the invoking
  checkout when opening a store for a command that can record, including for
  repository-wide events with no change target. Queue integration opens it
  afresh before each member's preparation and landing. An unparseable append
  policy refuses the command. These commands record nothing, in any form or flag:
{READ_ONLY_COMMANDS}
  They read the ledger through a handle that never parses that policy,
  refuses any append, and leaves ledger files byte-identical. Entries name complete leaf paths. Output files,
  generated manuals, hook commands and writability probes retain their
  declared effects. Policy inspection parses the policy it reports.
  `config` parses its style inputs, including repository policy.
  A derived handoff reads its change facts the same way. The debt advisory
  reads the same file for its thresholds, and an unreadable one sets none. Integration also checks the change target's
  policy before merging.

  Claude Code's Remote Control supplies `CLAUDE_CODE_BRIDGE_SESSION_ID` to
  connected tool shells. For a resolved Claude harness, `arc env` derives
  `ARC_SESSION_LINK` from that id; otherwise it unsets the link. The link is
  private event provenance in the ledger and journal. Arc never projects it
  into Git commits, trailers, changelogs, or forge text. Journal export
  bundles between the operator's replicas carry event provenance, including
  a recorded link.

  `arc env` detects a harness by the session variable it exports; not every
  harness exports one. A harness exports its session id into the processes it
  starts, so when several harnesses' variables are present it is the nearest
  ancestor that exported one that owns this process: a pi run inside a Claude
  Code tool shell reports pi, not the shell's claude. Where the ancestry names
  no single owner the ambiguity is reported and no harness, session, or model
  is set, rather than choosing by variable order; an ancestry that cannot be
  read, or that shows no exporter, says why beside the ambiguity. It reads that harness's own
  session store for the model, honouring the store's own override —
  `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_SESSION_DIR`,
  `PI_CODING_AGENT_DIR` — before the default under `$HOME`, and reports
  whether the store corroborates the exact canonical session id. An id the
  store does not hold is uncorroborated. An ambiguous or unreadable lookup,
  one that resolves to a recording under another id, a recording file named
  for one session that records another, and an unread subagent lineage are
  unresolved and name their reason, both ids included: a failed lookup is
  never reported as absence, nor as the absence of subagents. An unreadable
  Claude subagent directory or child ending leaves the model unresolved and
  reports the read failure in `arc env` and events such as `arc begin`.
  Recordings and child metadata must be regular files; a FIFO is refused
  without waiting for a writer. A plugin child's ending must name the
  parent's native session; a foreign or absent identity leaves lineage
  unresolved and reports the identity failure.
  Every event naming a harness and session records the store's verdict,
  with its reason unless corroborated, whether detection supplied that
  identity or the caller declared it. An unreadable arc configuration leaves ambient detection off
  and says why on stderr. It exports the
  harness, session, and session link it establishes and explicitly unsets
  each it does not; the model is a comment and ARC_MODEL is always unset. Pi re-sets its
  live recording file, model, and reasoning level for each tool call, so those
  answer while `PI_SESSION_ID` is the acting session, and a Claude subagent
  shares its parent's session id. An unfinished child recording leaves the
  invoking shell's owner ambiguous even if the parent's recording is newer,
  so no model is named, and no other model stands in for it; the reason names
  each unfinished child by the agent id and role its store records, three at
  most and then a count. Claude Code exposes no evidence that distinguishes a
  subagent's tool shell from its parent's. Completed children permit the
  parent's recorded model. For a corroborated Claude recording without a
  model selection, `arc env` and write-time model resolution re-read every
  50 ms for up to 1 second to allow the model-bearing record to reach disk.
  Other store answers return immediately. A missing recording and a
  recording still without a model after the wait each receive an explicit
  diagnostic. A recorded model that is a placeholder or a session id names no
  model: the diagnostic names the value and no write records it, and a live
  Pi value of that kind leaves the recording to answer. A live Pi variable
  that is set but not valid Unicode is unreadable, not absent: no model is
  named, since the recording may predate the turn in flight, and the
  diagnostic names the variable. Lookup reasons omit tapes' debug-quoted
  native-query failure prefix before the one-line, 240-character bound,
  preserving space for the failure cause.
  Launch configuration does not establish an observed acting model.
  OpenCode v2 (`opencode2`) is recognized without one — by
  `OPENCODE_TERMINAL` or its process ancestry — and prints the harness
  export, unsets the session and model, and leaves the session as a comment to
  set by hand. With nothing to
  detect at all it exits non-zero and prints the export template, which is
  the normal path for setting identity manually, not a failure.
  `[policy] require_declared_actor` makes an undeclared identity a refusal
  instead of a record.

FROM A WORKSPACE ROOT (outside a project)
  arc workspace backlog --here
                       Discover work beneath this directory without crawling it.
                       One report ends with the detail command that re-runs it
                       as itemized JSON for that exact scope; --items selects
                       the rows and --json selects the rendering.
  arc workspace inbox --here
                       Each project's count of open actionable filings untouched
                       for more than 14 days, including projects with no ledger.
                       `arc journal filed --stale 14` lists their fates.
                       Each registered project's predicate buckets, observed
                       from that project's own checkout; the same --under and
                       --here scope selects the same set as the backlog.
  arc workspace inventory [--storage hot|archived|all]
                       Reconcile every registered project's stores: where each
                       artifact sits, what resolved it, and its recorded
                       successor. It accepts the same
                       `--under`/`--here`/`--global` scope, reports the same
                       collection manifest, exits 16 on a partial collection,
                       and is versioned `arc-workspace-inventory/2`.
  arc workspace pins [--here|--under PATH] [--json]
                       Each project's pinned journal artifacts, grouped by
                       project, and the operator journal's when unscoped
                       (arc guide journal-references). Exits 16 when a
                       journal cannot be read. `arc-workspace-pins/1`.
  arc workspace report [--previous FILE] [--library] --json
                       The backlog classified by named rules: each artifact's
                       status (unresolved, delivered, claimed, ...), the
                       sections a reader acts from, and attention facts, so
                       any renderer draws the same page from the same ledger.
                       --previous compares the same schema and scope. Deltas
                       require observed facts; departures keep their recorded
                       reasons and do not imply resolution. Each change is
                       counted once with all its inbox predicates; questions
                       retain who may settle them. Two or more open changes
                       whose in-force briefs share a plan and slice, or a body
                       digest, are one `shared-plan-slice` fact. Review-owed
                       rows carry the effective `kind`, declared `reason`,
                       nullable `patchset_id`, `declared_at`, and `declared_by`.
                       `--help` lists every attention rule. Versioned
                       `arc-workspace-report/3`. With --library, each observed
                       project's `library` lists all hot and archived artifacts
                       and recorded relocations, newest filename first. Rows
                       carry `project`, `file`, absolute `path`, corrected
                       `title`, `kind`, `topic`, filename `timestamp`, nullable
                       RFC 3339 `filed_at` and `changed_at`, and `storage`
                       (hot, archived, relocated). `changed_at` is the latest
                       understood event naming the file or superseding it;
                       without an event it is null. `status` is present,
                       unresolved, consumed, archived, superseded or relocated;
                       consumption takes precedence over archiving. Optional
                       `resolution` and `resolution_basis` retain the inventory
                       vocabulary; `superseded_by` names a successor and
                       `superseded_by_relocated: true` marks its relocation.
                       `pinned: true` marks an in-force pin in the project's
                       journal; otherwise the key is absent. Operator journal
                       pins belong to that journal, not to project libraries.
                       Titles use the actionable rows' heading and fallback
                       rules; without a correction the heading lookup reads
                       the artifact body. No inventory claims, promotions or
                       content rendering are computed for library rows.
                       Without --library the field is absent. Failed library
                       reads enter the collection manifest as `library`.
  <ai-home>/arc/feed/events.jsonl
                       The workspace feed: after each committed ledger or
                       journal write, one `arc-workspace-feed/1` line. A
                       ledger notice (`source` `ledger`) names the published
                       `path` and, for a change event, its `change`; a journal
                       notice (`source` `journal`) names the journal `dir`,
                       with `binding` true when the write changed what the
                       registry says of it. Each carries `at`. Reads append
                       nothing. A notice
                       follows its write: failing to append fails nothing,
                       and a follower that missed notices reads again. Past
                       1 MiB the file moves to `events.jsonl.1`, replacing it.
  arc workspace watch [--previous FILE] [--library]
                       The report as one JSON line, then again each time a
                       notice changes it: only the projects the feed names are
                       read again. Requests on stdin compare with a previous
                       report or refresh every project; it ends when stdin
                       closes. --library retains each project's library beside
                       its row and reads it again only when that project is
                       dirty; a library-only change, including a pin or unpin,
                       emits too. `--help` holds the contract.
  cd <anchor>          Enter one project named by the report, then orient there.
  arc workspace retire <journal-dir|slug|anchor> --reason TEXT [--dry-run]
                       Take a registration whose project is gone (a probe,
                       deleted worktree, or scratch directory) out of what the
                       workspace views observe, so its missing anchor stops
                       failing the collection. It shows what the project still
                       holds, refuses while the anchor exists, a `data_root`
                       ledger holds open changes or owed debts, a transition
                       is unfinished, or a claim is active, and refuses
                       unresolved artifacts unless --archive-unresolved
                       shelves each in the cold archive with the reason. The
                       `retired` binding with its reason lands in the
                       journal's own bindings.jsonl; nothing is deleted.
                       Views count it as `collection.retired` and list it
                       under --retired. Read its history with
                       `ARC_JOURNAL_DIR=<journal-dir> arc journal show <file>`.
                       A moved project is not retired: `arc journal rebind`
                       adopts it from the new checkout.
  arc workspace restore <journal-dir|slug|anchor> --reason TEXT
                       End a retirement. A journal write from a checkout at
                       the anchor ends it too, re-registering the project.
                       Archived artifacts stay archived until
                       `arc journal unarchive`.

ORIENT INSIDE A PROJECT (start here, in this order)
  arc catchup --sessions 5
                         Recent project sessions across harnesses, ordered by
                         last recorded activity, with model, filed item count,
                         and naming handoffs/conclusions. Activity within seven
                         days with neither filings nor an ending artifact reads
                         unfiled.
                         Recordings span the primary checkout and its worktrees;
                         ledger and journal writers remain visible without a
                         recording. Each backend is queried once with an unrestricted
                         candidate window; readable rows rank by recorded activity.
                         A backend scan bound carries a one-line warning that hidden
                         sessions may rank inside --sessions. Failures are reported.
                         Pending transitions carry diagnostics and supply neither
                         filings nor naming handoffs/conclusions.
  arc journal filed [--session ID] [--stale 14] [--json]
                         Filed artifacts grouped by writing session, retaining
                         distillation sources and superseded origins. Direct writes
                         and published transition successors count, including imported
                         and spool-promoted artifacts with their original writers.
                         A transition requires its supersedes link and a complete
                         staged body match or its source's recorded retirement,
                         in hot or archived storage. Unverified pending intents
                         are omitted; journal doctor reports pending transitions.
                         --session matches the writing or source session id.
                         Fates are open, taken (with change ids), done, superseded,
                         discarded, relocated, or stale. Staleness
                         measures recorded inactivity on open actionable items;
                         active claims and evidence suppress it. Plain reads
                         record no activity.
                         JSON includes fate counts for mining and renderers,
                         and each item's count of evidence blocks.
                         Catchup and workspace inbox surface the same stale count.
  arc catchup            Text warns when the build commit differs from HEAD
                         in an arc-ledger checkout: older for an ancestor,
                         differs otherwise, and dirty if built with tracked
                         edits. Unknown revisions and other projects have no
                         build warning.
                         Live state: ledger queue, deferrals delegated rounds
                         left open, journal backlog, forks, unowned branches
                         and worktrees, and what the change and fork
                         worktrees occupy — apparent size, with the mount's
                         free space, measured at the configured worktree root
                         or, when the configuration cannot be loaded, at the
                         current checkout, which a stderr warning says. A path
                         several changes record names them all, and a change
                         not checked out there carries the command that would
                         let it gate.
  arc fork adopt <slug> [--branch B]
                         Record a branch the operator made by hand as a fork;
                         any local branch, keeping the name it carries.
  arc fork <slug>        Fork this repository: a worktree on fork/<slug>,
                         outside the change lifecycle — unintegrated by
                         intent; the operator decides what to merge, rebase,
                         or discard. A fork's work is unintegrable: a change
                         whose branch is a fork's is refused by `integrate`
                         and `check` from every directory, and `begin`
                         refuses to open one. `arc fork retire <slug>
                         <outcome>` records the
                         disposition and removes the worktree. `arc fork
                         thread <slug>` names the harness, session, and model
                         that opened it, and how to resume that session.
  arc fork list --json   `arc-forks/3`; base_branch is null when no recorded
                         or safely discovered integration branch is available.
                         Text renders that missing base as unknown.
  arc journal open       The actionable backlog — work waiting for a session.
                         Incomplete successors appear under `pending`, with
                         availability `pending-transition`, outside actionable
                         counts. Retry the source transition before promotion;
                         transition and claim refuse a pending successor too.
                         Inventory and workspace backlog retain that status.
                         A row occupied by somebody reads `[claimed by <actor>
                         via <harness>: active|expired]`; `--json` carries the
                         same as `availability` and the claims themselves.
  arc journal inventory --json
                         Shared artifact facts, claims, questions, promotions,
                         and explicit ledger coverage. --archived selects cold
                         storage; a filename inspects one exact artifact. A
                         plan whose every promotion has closed reads
                         `promotions: closed` while staying listed, and
                         `integrate`/`close` name the `journal consume` that
                         makes closing the loop a decision.

  Plans carry portable planned-by metadata: `--planned-by` takes a JSON
  object of `actor`, `harness`, `session`, and `model`, with effort riding in
  the model as `<model>#<effort>`. A brief selecting --plan-ref and
  --plan-slice captures the body digest and declared planners; reading the
  brief prints available credit suggestions. Conflicting or malformed
  metadata grants no credit, and corrections leave existing briefs unchanged.
  Headers, event metadata, corrections and captured brief coordinates must
  satisfy the coordinate rules before granting credit. Forbidden nonempty
  coordinates read as malformed, including placeholders and session-id models.
  A planner correction that fails validation when read, as one imported from
  another build can, is not dropped: `arc journal show --json` reads
  `planner_status: malformed` with its amendment ID in
  `planner_correction_event`, and no planner is credited until a later
  correction replaces it. `planner_correction_event` also names the correction
  in force when the status is `corrected`, and is null when none was recorded.

  Shelve a discussion with journal archive FILE --unresolved --note REASON.
  Use journal position FILE --archived to argue there, and journal unarchive
  FILE to restore storage. Terminal resolution remains terminal. Pending
  storage moves appear in journal doctor and must be retried before other
  writes; the retry validates the operation's paths and body digest.
  Archiving keeps every byte readable. To take content out of reach:
    arc journal relocate FILE... --to DIR --reason TEXT [--dry-run]
                         Move whole artifacts to a directory outside arc. The
                         body leaves the hot directory, the cold archive, and
                         any prepared transition copy; a `relocated`
                         tombstone keeps the file, destination, reason, and
                         digest, and readers report that fate. Open claims,
                         unfinished transitions or storage moves, and
                         unanswered questions refuse until acknowledged.
                         Exchange neither carries nor restores the artifact.
    arc journal redact FILE (--replace OLD --with NEW)... | --body-file F
        --reason TEXT [--dry-run]
                         Rewrite passages of a hot or archived body in place,
                         keeping its entries. The `redacted` event records
                         digests and the reason, never the removed text, and
                         exchange keeps the redacted revision on both sides.
  The event log keeps what events recorded, such as topics and titles; both
  commands change bodies, not the log.
  arc journal dir        The authoritative journal location for the project
                         you are standing in, honoring ARC_JOURNAL_DIR and
                         [journals.dirs]; the default path is only a default.
                         Read-only journal commands also resolve from the
                         journal directory itself; a write refuses there and
                         names the checkout it needs.
  Find a journal artifact from the project checkout, never by searching the
  filesystem: a filename belongs to its owning project's journal, and
  `--global` is a separate operator scope.
    known filename   `arc journal show <file.md>` reads the body, hot storage
                     first and the cold archive after it;
                     `arc journal discussion <file.md>` reads its stances and
                     open questions. Discovery rows print the filename, so a
                     row is copied, never rebuilt from its fields.
    known entry      `arc journal show <entry-id>` prints that one entry, its
                     artifact on stderr; the ID alone is its address.
                     `<file.md>#<entry-id>` also reads it, checked against
                     the artifact that records it.
    another project  `<project>::<entry-id>` or `<project>::<file.md>` reads
                     it by registered slug or label; `<journal-dir>::` names
                     the journal directory and is the form a recorded
                     reference keeps.
    known topic      `arc journal latest <topic> [--kind <kind>]`, matched
                     exactly; `arc journal show <topic>` shows the newest
                     live one and names it on stderr.
    no filename      `arc journal list [--kind <kind>]` browses live
                     artifacts; `arc journal open` lists the actionable ones.
    a path           `arc journal dir` (`--explain` names the source that
                     resolved it) only when a filesystem path is required.
  arc claim <file.md>    Claim a journal artifact, the way a change is
                         claimed: a lease with a TTL, renewed by working it,
                         released with `arc release-claim <file.md> --outcome
                         paused|abandoned|expired`. `arc stage <file.md>
                         <stage>` records typed progress against it.
                         For claim, stage, release-claim, watch, and rescue, a
                         bare .md filename held only by another known journal
                         is refused with its owning journal and project.
                         Artifact claims require a declared actor, harness,
                         and session. Renewal with --ttl replaces the lease
                         duration, measured from the renewal timestamp;
                         omission retains its current duration.
  arc journal checkpoint <file.md> --body-file -
                         Where the work stands and what a successor should
                         read, appended to the artifact and recorded with a
                         digest of the block. `--next`, `--gate`, and
                         `--blocker` carry the structured half; `--supersedes
                         <checkpoint-id>` corrects an earlier one without
                         hiding it.
  arc journal evidence <file.md> --body-file -
                         Add a finding, an option, or a revised section to a
                         live item of any kind instead of filing a successor:
                         an append-only block under a heading naming its id,
                         writer, and time, recorded as an `evidence` event.
                         `arc journal show` prints it, `arc journal filed`
                         counts it per item, and it is recorded activity, so
                         it keeps an open item from going stale. It states no
                         stance and never enters a stance tally; argue a
                         discussion with `journal position`, and replace a
                         recorded field with `journal correct`. A consumed
                         item is closed: file a successor.
                         Structural position headings in evidence refuse;
                         fenced and indented examples are prose. Evidence and
                         position check target availability before reading the
                         body and again under the transition lock. Question,
                         answer, suggestion, retraction, and checkpoint writes
                         use the same two-stage target preflight.
                         Evidence cannot be appended in cold storage; use
                         `journal unarchive` before appending. `--title`
                         names the block after the heading's writer and
                         time, and `journal append` is the same command
                         (arc guide journal-references).
  arc journal verified <file> [--note <text>]
                         Record a source check at the head of the checkout it
                         was made in: the project anchor, or a fork's own
                         head recorded with the fork as its scope.
  arc resume <change>    One change's brief, live state, and journal context.
  arc explain <change> [--at <event>]
                         What one change knew and what it was accepted on:
                         contract, supplied context, declared facts,
                         observed reads, rejected alternatives, evaluation,
                         coverage at acceptance, and later knowledge.
                         Observed reads are the change's read records, each
                         with its inferred blob and capture state. A
                         promoted patchset names its selection and promotion
                         in the contract, and the promoted candidate's
                         siblings are rejected alternatives. Every
                         row carries a standing: `recorded` (an event records
                         it), `declared` (somebody stated it and arc did not
                         check it), `inferred` (arc derived it; no event
                         states it), `absent` (nothing records it), or
                         `unavailable` (its source cannot be read now), with
                         a reason for all but `recorded`. The opening
                         artifact is `recorded` when its digest was captured
                         at open, `declared` when not; a pass's inferred
                         falsification reads `inferred`, beside any declared
                         one. Contract,
                         evaluation, and coverage stay on the integration's
                         basis; `--at` hides every other record made after
                         the event, or replays a change not integrated. It
                         writes nothing.
  arc inbox              Lead-facing queue across open changes, unowned
                         branches and worktrees, and the journal backlog.
                         The branches work integrates into are never unowned:
                         the default branch, every open change's target, and
                         a closed change's target that is not itself a work
                         branch.
  arc workspace backlog  The same question asked of every registered project.
    --here | --under <path>  Restrict it to one workspace directory tree.
    --rank-by blocking|availability|coverage
                         Order rows by one recorded fact; the report states
                         the basis it used.
  arc stats --provenance How often each provenance record was written where it
                         could have been — falsifications, journal links,
                         rejected alternatives, plan-linked briefs, cited
                         facts, changes with read records — each count beside
                         what it counts in.
  arc stats --round-trip Each integrated change's final landing, from its
                         patchset's recording (or the start of the rebase that
                         recorded it) to the integration event, split into
                         rebase, gates, waiting and integration, with reuses
                         counted; the median, the worst and the five slowest.

  Work waiting for this project lives in two places. The ledger holds changes
  already open; the journal holds everything not yet opened as one. An empty
  inbox does not mean an empty queue — check both, which is what `catchup` does.

  Every inbox bucket is an independent predicate, so a change in a state none
  of them anticipates would land nowhere and the queue would read as empty
  while work waited. The `unclassified` bucket catches exactly that and names
  the reason. A row there is a gap in the derivation rather than a resting
  place: it means arc failed to classify open work, and it is worth reporting.

  Aggregate change observations retain readable changes and name each
  unreadable change with its error on stderr, including with JSON output.
  A read aimed at one change refuses if that change cannot be read. Status
  keeps unreadable prerequisites unresolved, so they cannot authorize
  integration. Operations that require the complete dependency graph refuse
  an incomplete read. A changelog file write requires every change to be
  readable so a failed reduction cannot silently remove an entry. `arc doctor`
  reports each change whose events fail to reduce as `unreplayable-change`,
  with the reduction error, and exits 1.

  Every command in the project orientation answers for the project you are
  standing in. Some
  questions are comparisons — which project to open next, what has waited
  longest, where a verdict is the only thing missing — and those are answerable
  only across projects, which is what `workspace backlog` is for.

  A source check is worth keeping only with the revision it checked: `journal
  verified <file>` records that fact, and the open queue says so on the row.
  Nothing more is said while the stamp holds — that is what verified means;
  the row speaks up once the anchor head has moved past it.

SANDBOX (rehearse something destructive)
  arc doctor             Reports the build commit and tracked-tree state
                         (clean, dirty, or unknown) in text and JSON.
                         Opens with the roots this invocation reads and
                         writes, and whether a sandbox is in force.
  arc sandbox clone <prefix>
                         Copy this project — repository, ledger, journal,
                         configuration — into the prefix, so the copy answers
                         the way the source does.
  ARC_SANDBOX=<prefix> arc <command>   (or --sandbox <prefix>, on any command)
                         Every root arc writes by default lives under the
                         prefix: journal, registry, configuration, worktrees,
                         temp, and the ledger wherever a data root places it.
  arc sandbox diff <prefix>      What the copy's ledger events, journal
                         events, and refs differ by, in both directions.
  arc sandbox discard <prefix>   Remove it — only where the marker arc wrote
                         and the directory agree about what is there.

  The prefix stands in for the home directory, so one value moves every root
  at once and a configured `~/…` path follows it. A variable naming one exact
  directory — ARC_AI_HOME, AI_HOME, ARC_WORKTREES_DIR, ARC_DATA_ROOT, ARC_DATA_DIR,
  ARC_JOURNAL_DIR — still means the directory it names: the prefix replaces
  defaults, not statements.

  The repository arc was pointed at is still read and written as a Git
  repository, which is why a rehearsal runs in a clone rather than beside the
  original. A clone carries committed state: uncommitted work is not copied,
  and an open change's recorded checkout is the source's until one is made in
  the sandbox. The prefix bounds recorded paths too: a checkout outside it is
  refused by name rather than run in, so gates and `rebase` say to make one
  inside with `git worktree add`.

SETTLE A QUESTION (before it is work)
  arc journal note <topic> --kind discussion --body-file -
  arc journal position <file> --body-file - [--stance <for|against|amend>]
  arc journal question <file> --placement opening|closing --option <a> --option <b> --body-file -
  arc journal questions                Every question waiting on a person.
  arc journal delivered <file> --question <id> --to person|anyone|delegate:<name>
    --handle <opaque>                  Record that you asked somebody.
  arc journal scaffolds [--show <n>]   What a write adds, before it does.
  arc journal answer <file> --question <id> --option <choice> --body-file -
    --other "<answer>"                 Settle it outside the options offered.
  arc journal discussion <file>        Read stances, branches, and open questions.
                                       The file is read hot or cold from the project
                                       journal; ORIENT shows how to find one.
  arc journal suggest <file> --question <id> --option <opt> --body-file -
                         Suggest an option without settling the question;
                         every question view shows it beside the options.
  arc journal latest <topic> [--kind <k>]
                         One topic's newest artifact, hot before cold; inside
                         one second the successor relation and recording order
                         decide, never the filename suffix.
  arc journal correct <file> --target <t> --field <f> --value <v> [--note]
                         Model corrections are checked against the target's
                         recorded session and the invoking session. An answer
                         target names its latest settlement. Evidence IDs
                         accept actor and model corrections; retraction marks
                         their evidence withdrawn in `journal show --json`.
  arc journal show <file> --json
                         Positions and evidence list IDs, headings, bodies,
                         recorded identities, corrected fields, and withdrawal
                         status. Recorded identity models retain raw values with
                         `model_recorded: false` when missing, placeholders or
                         session ids; valid models omit the flag.
                         Unrecorded blocks have null recorded identity;
                         detached events have null heading and body. Corrections
                         and retractions list typed events in log order.
                         Headings and fence markers permit up to three columns
                         of indentation; indented code is prose. Unrecorded
                         fenced entry, question, and answer examples are prose;
                         recorded position, evidence, question, and answer
                         markers terminate an open fence, and a heading that
                         repeats an entry ID already met is a quotation.
                         Entry bodies keep their indentation. Question readers,
                         `show --json`, discussion tallies, and doctor share
                         quotation and stance rules.
  arc journal doctor
                         Matches position and evidence blocks with typed events
                         for every artifact kind in hot and cold storage. Either
                         missing half is a problem.
  arc journal retract <file> --target <t> --body-file -
  arc journal reattribute <file> --set-actor <a> --set-harness <h>
    --set-session <s> --set-model <m> [--dry-run]
                         Repair one artifact's recorded creation authorship
                         in place; other event kinds and delegated records
                         refuse by name. Repair and every event append share
                         an event-write lock, so an append cannot be lost when
                         the repaired log is published. Model replacements are
                         flag declarations compared with the retained observation;
                         changing harness or session clears that observation.
                         The effective model must not equal the effective
                         recorded session, including a supplied --set-session.
                         Unnamed model fields otherwise keep their historical values.
                         Legacy placeholder harnesses and sessions read as absent
                         and do not identify sibling references during repair.
  arc journal decision <topic> --body-file -        Write the verdict first.
  arc journal consume <file> --outcome done --decision <decision>
  arc journal transition <file> --to discussion [--dry-run]
    Change a live artifact's kind as one guarded operation: a typed successor
    with a `supersedes` link, the source retired. Promotion to a code change
    stays with `begin --from-journal`, never a kind conversion.
    Complete Markdown is staged durably under `.transitions/` before the
    relation event, then published exclusively and verified before retirement.
    Reusing a matching preparation syncs its file and directory before the
    relation is recorded. Retry uses the prepared bytes, including a supplied body and title. A
    legacy pending successor is verified against the original body and title;
    a mismatch preserves the artifact and refuses retirement. A readable
    supersedes prefix alone supplies no publication evidence.
  arc journal <kind> <topic> --body-file -   One verb per kind; `arc journal
                                       --help` is the registry. `arc fr` is
                                       the top-level alias for one of them.
  arc journal <kind> <topic> --source 'harness=<h> session=<id> ts=<rfc3339>'
    --item-key <k>                     Where a distilled item came from; a
                                       repeat reports the existing item.
  arc journal <kind> <topic> --source 'intake=<name> item=<id>' --item-key <k>
                                       The same for an intake item.
  arc journal source --harness <h> --session <id> [--item-key <k>] [--json]
                                       What one recording already produced.
  arc journal source --intake <name> --item <id> [--item-key <k>] [--json]
  arc journal source-attach <file> --source '<spec>' [--item-key <k>]
  arc begin <slug> --from-journal <file>

  The ledger records what happened and what is allowed; the journal records
  what people think. A question with more than one defensible answer belongs in
  a discussion before it belongs in a change — positions carry the model and
  harness that argued them, so a decision can be read later by who reached it
  and on what grounds, rather than surviving only in one session's transcript.
  Every journal event records the `--on-behalf-of` subject beside the identity
  that ran the command, so ceremony a lead performs for an executor is legible
  as both rather than as the lead alone.

  A proposal with one defensible answer is not a discussion, it is a feature
  request: `arc fr` files it, `arc journal open` lists it back, and `begin
  --from-journal` turns it into work when its turn comes.

  Every kind is a verb under `arc journal`, so the closed set is legible from
  `--help` rather than from a flag's values; `note --kind <k>` remains the
  same write underneath. `discussion` has no write verb on purpose — one is
  argued and read far more often than created, so `position`, `question`,
  `answer` and the `discussion` summary are its surface, and `note --kind
  discussion` opens one.

  A discussion opens on its conventions, then the opening body under
  `## The question`, then `## Positions`. `position` appends at the end of
  the file, so what follows `## Positions` is the argument and nothing else.
  Any scaffold places the body at its `{{body}}` line, and a template without
  one is followed by the body.

  Every artifact opens on a heading, because a queue row reads its description
  from there and a row that names nothing is a row nobody picks up. `--title`
  sets it; a body that already opens on a heading keeps its own; a body that
  opens on prose is headed from its topic slug and the write says so, leaving
  a name worth correcting rather than a blank one to discover later.

  An item distilled out of a recorded session carries where it came from.
  Every write verb and `journal log` take `--source 'harness=<h>
  session=<id> ts=<rfc3339>'`, with optional `turn=`, `schema=`, and
  `coverage=` recorded only when an emitter supplies them, plus
  `--item-key <k>` naming one item inside that recording. The coordinate is
  all that is kept: no transcript text, credential, or digest enters the
  journal, and arc never opens the recording to infer a project or a body.

  That reference is what makes rescanning the same endings cheap. A write
  whose recording and key were already recorded writes nothing and prints
  the existing entry with its disposition — `open`, `no-action`, or
  `consumed:<outcome>`. `journal log <topic> "<message>"
  --source .. --item-key .. --outcome no-action` records the judgment that
  an item needs no work at all, so a later scan is answered without an
  artifact entering any queue. `journal source --harness <h> --session
  <id>` lists everything one recording produced here; `journal
  source-attach <file> --source ..` records a further recording as
  evidence behind an artifact, since one item can be evidenced by several
  sessions. Which project the item lands in stays the caller's choice
  through ordinary anchor resolution: a recorded session directory is
  evidence, never authority to write there.

  An item a person left in an intake outside arc, such as `tray`, is a
  source too: `--source 'intake=<name> item=<id>'`, exactly those two
  fields, the name matching `[a-z][a-z0-9-]{0,31}` and the ID one token of
  at most 128 bytes. Its identity is the intake and the item alone, so a
  keyed repeat by any session, harness or model writes nothing and prints the
  existing entry; the session that writes is still recorded as the writer.
  `journal source --intake <name> --item <id>` lists what the item produced,
  `source-attach` accepts the same spec, and views keyed by session never
  read an intake item as one. `arc catchup` counts open intake items: `arc
  catchup --help`.

  An entry filed wrong is amended, never rewritten. `correct` replaces one
  field of one entry — the artifact's `title`, a position's `stance`,
  `option`, `ref`, `actor` or `model`, an answer's `option` — and `retract`
  withdraws a position or an answer with a reason. Both append a block naming
  what changed and leave the original where it was filed, so the artifact
  still reads as the argument that happened while the tally, the branches,
  the question queue, and the open queue read the value in force. A retracted
  position leaves the tally; a retracted answer reopens its question, and a
  question answered again is settled by its latest answer: an amendment of
  `answer:<question id>` names the answer it was made against
  (`settlement`), and one recorded without it binds to the answer standing at
  its place in the log. Both are
  open on a consumed artifact, which is closed to new work but not to being
  wrong. A row for a filed claim carries `[amended ×N]` for the positions
  standing on it, so a claim amended three times is not read as one filed
  once.

  Consuming a discussion names what it shows: every position from one
  participant, or a last position nobody answered. Warnings, never refusals —
  one voice is a legitimate way to settle something, and the point is that
  whoever resolves it is told which they are resolving.

  Resolve a discussion as done, or promote the still-open discussion to work —
  never both. `consume --outcome done` cites a terminal decision; `begin
  --from-journal` consumes the open discussion as superseded by the change.

  A question outlives the work that raised it. `consume` refuses while a typed
  question is unanswered, because disposal would drop it into a file no queue
  lists; `begin --from-journal` warns instead, since supersession claims
  nothing was settled and the body travels onto the change. Both name the ids,
  and a question worth keeping past either is worth its own artifact.

  A question says what this session should not settle alone. arc records that
  one is waiting, and who may settle it: a person by default, `anyone` with
  `--settle-by anyone`, or a named delegate with `--settle-by delegate
  --delegate <name>` — an operator who handed the call to a stronger model
  records exactly that, instead of a question holding open against a person
  who was never the only possible answerer. Raising it is still the agent's
  job, through whatever prompt its harness offers — `arc journal questions
  --json` carries the settle-by, the options, and the branches already
  argued, which is everything a prompt needs — and `answer` is where the
  reply comes back. Once you have raised it, `delivered <file> --question
  <id> --to person|anyone|delegate:<name>` records that you did; arc keeps
  the fact and sends nothing. Every question view then reports `delivery`:
  `unasked` means prompting work remains, `delivered` means the reply is
  merely pending, `answered` ends it, and `unknown` marks a question older
  than the record itself — posing marks the facility, so only questions
  predating that reach it. Asking again records
  again — nothing is erased, because a question asked twice and still
  unanswered is the more urgent one — and a question waiting on one named
  delegate accepts only that delegate. When the answer is none of
  the options, `--other "<answer>"` records it in the answerer's own words
  and marks that the menu was stepped outside, because a menu somebody had to
  leave was framed wrong and the next one should know. Placement decides when:
  `--placement opening` before anyone argues, so every participant starts from
  the same premise, or `closing` once the argument is in — never mid-argument,
  which would make you watch a run you delegated. Argue a closing question on
  both sides first (`position <file> --body-file - --question <id> --option <opt>`)
  and the answer picks between explored branches instead of labels. Advice, not
  a condition: an answer over a branch nobody argued records and says which
  branch was skipped. Refusing it would measure the typed binding rather than
  the arguing, and is satisfied most cheaply by a thin position under each
  losing branch — the labels this exists to avoid.

RUN A CHANGE
  arc begin <slug> --profile <p>     Open a change: branch + checkout + record.
    --planned-by [<value>]           Assert planning credit (repeatable).
    --orchestrated-by [<value>]      Assert orchestration credit (repeatable).
                                     With no value, use the invoking identity
                                     as `arc roles` does; unresolved
                                     coordinates refuse.
    --from-journal <file>            Open it from a journal item, consuming it.
    --blocked-by <change> --tag <t>  Declare a chain up front, at planning time.
    --worktree [<path>]              Force a worktree, at the path or under worktrees_dir.
    --no-worktree                    Switch this checkout, carrying local work.
    Checkout follows [change] checkout (built-in in-place), independent of profile.
    Without --branch, append the slug to [change] branch_prefix (built-in arc/).
    --base <rev>                     Start from <rev>, not the target head.
                                     Without it, a target behind its upstream
                                     (as the last fetch left it) is a warning
                                     naming both revisions and the fix.
    If commits landed on the target by mistake: keep a branch at the work with
    `git branch <work-branch>`, then `arc begin <slug> --adopt <work-branch>
    --base <intended-base> --target <target>`. Repair the target deliberately;
    do not reset it.
  arc brief --body-file <f>          Record the next contract version.
    --planned-by [<value>]           Assert planning credit on the change;
                                     with no value, as on `arc begin`.
    --caused-by <kind:ref>           Why it changed. v1 refuses a cause; every
    --cause-note <text>              later version requires one of these.
  arc claim / stage / release-claim  Advisory liveness while implementing.
    --takeover                       Displace a claim that may be taken over:
                                     a stale one on a change, an expired one
                                     on an artifact.
    --because <reason>               Displace one that may not be, stating why
                                     its holder is gone. `agent-status-absent`
                                     and `delegate-exit:<handle>` are the
                                     expected evidence; any text is accepted.
  arc accept INPUT --branch LOCAL-BRANCH --publisher PRINCIPAL
      --received-sha256 SHA256 [--worktree PATH] [--key KEY]
      [--contributors ACTOR,... | --solo] [--custody-dir DIR] [--dry-run]
      [--json]
                                     Prepare a fetched delivery on its paired
                                     authority, as lead; return readiness and
                                     the local judgment still owed.
  arc snapshot                       Record the current head as a patchset.
                                     A rewrite that invalidated approval
                                     requires a fresh patchset even when the
                                     mapped head already matches the branch.
                                     A snapshot grants no approval. A
                                     clean `arc rebase` keeps the replayed
                                     patchset's recorded contributor
                                     coordinates.
                                     Default executor attribution:
                                     `arc guide debt-review`.
    --verify [--gate <name>...]      Run required gates, or the named ones,
                                     reusing the newest green evidence at the
                                     same tree; --rerun runs them anyway.
                                     Names must be unique and declared;
                                     invalid names refuse before any write.
    --contributors <a,b> | --solo    Declare who wrote it; required while
                                     another actor holds a live claim.
    --journal-ref <file>             Name a journal artifact that framed the
                                     work; recorded with the digest read now.
    --thread <scheme:id>             Name the external thread it belongs to.
                                     Identifiers only, never transcript text.
  arc verify --gate <name>           Run a declared gate; record the evidence.
  arc verify --command <cmd>         Same, for an ad hoc probe.
  arc verify --against <branch>      Run every required gate on the merge with
                                     that branch, not on this head. Gates
                                     are declared by that destination.
                                     A textual conflict refuses before any
                                     gate runs or evidence is recorded;
                                     rebase first.
    --rerun                          Run declared gates even where a pass
                                     would be reused. Without it, --all,
                                     --gate and --against reuse passing
                                     evidence at the head's tree, or at the
                                     merged tree, whichever commit it ran
                                     on, under the same declaration and
                                     environment. Only the newest evidence
                                     readiness counts there is a candidate:
                                     a gate reruns when it failed, was not
                                     admitted, or carries neither tree nor
                                     tested_tree, and when its probe yields
                                     no identity. The reuse records its
                                     tree. Replays of reuse events without
                                     a tree require matching revisions.
                                     `--skip-green` is accepted and ignored.
    --attest --environment <id>      Record a run arc did not perform, naming
                                     the environment it applies to.
    --attest --after-integration     Retain a check run on an integrated
                                     change's shipped tree as a receipt,
                                     never gate evidence.
    --falsified-by <id> --predicted <why>
                                     Name the failure this pass answers,
                                     storing its original event revision.
  arc done                           Snapshot, verify every gate with green-tree
                                     reuse, print check state.
    --contributors <a,b> | --solo    As on `arc snapshot`.
  arc rebase [--verify]              Replay the branch onto its target, snapshot
                                     the new head, name the gates it owes.
    --contributors <a,b> | --solo    As on `arc snapshot`; while another actor
                                     holds a live claim, refused without one
                                     before the branch moves.
  arc policy show                    Show effective project and operator rules,
                                     with the file that declared each rule.
  arc policy path                    Print the operator policy path for this repo.
  arc policy write --body-file FILE  Replace that local policy from TOML.
  arc review --verdict <v>           Record a verdict (+ --findings-json -).
                                     An implicit verdict is refused when a
                                     rewrite invalidated the latest patchset's
                                     review head; snapshot that head first.
                                     Explicit --patchset judgments may record
                                     historical work and stay stale.
    --snapshot                       Snapshot the clean worktree first;
      --contributors <a,b> | --solo  attribute that patchset, as on
                                     `arc snapshot`.
                                     After a rewrite that invalidated approval,
                                     the verdict names a fresh patchset.
    --cause <c>                      Required with changes-requested, refused
                                     otherwise: brief, executor, or
                                     integration-staleness.
    --findings-json <file|->         A JSON array; each finding needs a
                                     severity (critical, major, minor, note)
                                     and a summary. `--help` has the rest.
    --provisional <why>              It gates, and owes a second judgment.
    --relation corroborates          Support the standing verdict, not replace it.
  arc resolve                        Dispose of a finding; after integration,
                                     one left open when the change shipped.
                                     `arc findings` lists findings, and no
                                     command reads a single one.
    --evidence-event <id>            Cite the verification run that justifies it.
  Nothing submits or marks a change ready: `arc snapshot` records a patchset
  and `arc check` computes readiness. `arc close <change> --abandoned` abandons.
  arc check                          Integration preflight; exit code names the blocker.
  arc integrate <c>...               Guarded integration once the gates are green.
    --shape ff-only|no-ff            Resolved style; built-in ff-only.
    <c> <c> <c>, or --tag <t>        Land a queue, in dependency order.
  arc export <change> --output <file|-> [--since <sha256>]
                                     Export a versioned event bundle and its
                                     Git objects, with a count by role.
    --objects-json <file>            Write an arc-bundle-objects/1 transport file.
    --json                          Print arc-export/1 summary JSON, including
                                     custody counts; requires --output <file>.
    --custody-dir <dir>              Also write an arc-custody-bundle/1 sidecar
                                     of the retained forge content.
  arc import <file|-> [--dry-run]     Require every listed object locally before
                                     writing any events or refs. Missing OIDs and
                                     roles are reported; transport the objects first.
    --provenance-only                Import events despite missing objects,
                                     reporting the unavailable evidence.
    --custody-dir <dir>              Install a sidecar the bundle binds.
    --json                          Print arc-import/1 summary JSON, including
                                     diagnostics and structured custody counts.
                                     Custody phase is planned, installed or
                                     not-supplied; installed counts in a planned
                                     summary describe the prospective effect.
                                     arc accept --json carries the same custody
                                     summary from its latest import step.

  Change history checksums hash compact JSON with recursively sorted object
  keys and a newline after each event. The checksum names the same prefix
  regardless of stored event field order.

  Availability checks never fetch from a promisor remote. A dry run writes
  no files, Git objects, events, or refs. The --objects-json file is an
  envelope with `schema: "arc-bundle-objects/1"` and an `objects` array.
  The bundle's `objects` array contains `{oid, role}` pairs, deduplicated by
  pair and sorted by OID then role. It covers exactly the exported change
  events (only the suffix under --since) and bundled repository events.
  Roles are `patchset-base`, `patchset-head`, `evaluated-tree`,
  `retention-commit` (objects held by refs/arc/tree for exported evidence,
  including direct tree pins, and the commits a rewrite retained under
  refs/arc/retained for the exported change or in repository scope),
  `synthesized-merge` (verify --against),
  `disposition-commit`, `integration-commit`, `target-commit`, `content-tree`,
  `cited-blob`, `cited-commit`, `rewrite-source`, `rewrite-successor`, and
  `observed-commit` (a commit a forge fact records as the provider's own,
  listed only where the exporter holds it, so a commit never fetched here
  obliges no receiver). Unknown event payloads remain opaque. A transport
  publishes the listed objects and their Git reachability closure through scoped refs or a Git
  bundle before announcing the result. Bundles without the objects field
  retain advisory patchset-commit checks; /6 accepts that absent field.
  A bundle's rewrite events carry the retention refs the exporter created; an
  import holds each one it names for the imported change or in repository
  scope, where a checkpoint's are, under `refs/arc/retained/` when the commit
  is here, and refuses a name that is no retention ref. Bundles whose rewrite
  events name none import unchanged. An import holds each `observed-commit`
  of the imported change that is here the same way, under the event that
  recorded it. Once its events are published, an import also holds every
  commit the ledger cites that the bundle's object list delivered, that is
  here, and that no ref reaches, under the name rewrite retention gives it
  (`arc guide rewrites-evidence`): once per commit, leaving every name that
  already stands, and printing `retained: <ref> <commit>` for each. A
  rebase's retention refs, which no event names, are held again this way. A
  dry run creates none, and a bundle without an object list delivers
  nothing to hold.

  Retained forge content, and the receipt file each read ingested from one
  cites by digest and length, travel beside the bundle as an optional sidecar,
  never inside it. `arc export --custody-dir <dir>` writes
  `arc-custody-bundle/1` into a new or empty directory: one `sha256/<digest>`
  file per available digest, copied once however many references share it, and
  `manifest.json` listing every digest the exported events reference as
  `available` (with its path) or `unavailable` (with the reason `absent` or
  `corrupt`). `complete` is true only when nothing is unavailable. Content
  that fails verification is never shipped. The bundle's `custody` member
  holds the manifest's SHA-256, and the export summary prints the entry
  counts and `complete` or `incomplete`. `arc import --custody-dir <dir>`
  refuses, before any event, ref or file is written, a sidecar when the bundle
  binds none or another manifest; when the manifest names another change,
  lists other content than the bundled events reference, claims completeness
  over an unavailable entry or names a path other than `sha256/<digest>`; or
  when its content is a symlink, is not a regular file or differs in length
  or SHA-256 from its entry, or when the ledger's `custody/` or
  `custody/sha256` directory is a symlink. The events are published first;
  verified files are then copied into ledger custody unless already held,
  each verified again as it is copied, and the summary counts installed, held
  and unavailable content. Custody is reported installed only after that, so
  a failure installing it says the events are published, and repeating the
  import finishes the installation without recording events, as does
  importing with a repaired sidecar. Unavailable entries stay unavailable and
  the import says incomplete. Importing a bundle that binds a sidecar without
  one says how many references stay unavailable, which manifest digest the
  bundle binds and that --custody-dir installs it; `arc accept` says the
  same. An
  export refuses, before writing anything, `--output`, `--objects-json` and
  `--custody-dir` destinations that name one location or lie inside one
  another, and a file destination that is a directory. It finishes every
  artifact beside its destination, publishes the sidecar first and then the
  object list and the bundle, and on any failure removes what it published
  and restores what it replaced: a previous file keeps its bytes and an empty
  directory that was to hold the sidecar is empty again, so no bundle names a
  missing sidecar. Neither export nor import initializes a replica.
  Full and delta histories use active, uncontradicted rewrites admitted with
  rewrite authority to prove a falsification's failure identity. A publisher
  lacking rewrite authority is refused; legacy transport without an
  authorization receipt supplies no such proof. Absent, withdrawn, unrelated
  maps and revisions that merely converge on one successor prove nothing. A
  reference only a withdrawn map carried imports set aside, as the replica
  that withdrew the map reads it; the others refuse the import.
  Claim-contest replay uses the same admitted proof, or strict identity when
  there is none. Doctor uses the store's admitted proof while leaving recorded
  revisions unresolved for dangling-object diagnostics.

  arc replica id --json              Read this store's repository ID for pairing.
  arc replica init <name>             Start a logical project and hold authority.
  arc replica pair <name> --repository-id <id>
                                     Record a peer for explicit file exchange.
  arc replica export --output <file>  Export known replica events.
  arc replica import <file> [--dry-run]
                                     Import pairing, offer, or authority events.
  arc replica authority offer --to <name>
                                     Relinquish authority to a paired recipient.
  arc replica authority reclaim --because <reason>
                                     Request return without acquiring authority.
  arc replica authority confirm-return
                                     Relinquish authority to a requesting origin.
  arc journal export <file>... --output <file>
                                     Export selected journal artifacts with
                                     the events recorded about them.
  arc journal import <file> [--dry-run]
                                     Admit understood artifacts; report and
                                     retry artifacts with unsupported events.
  arc audit <change> --verdict <v>   Review an already-integrated revision.
  arc close                          Terminal outcome arc did not merge itself.
  arc changelog [<c>] --category <k> --body-file <f>
                                     Record the change's release copy; <c> is
                                     inferred from the branch or worktree.
  arc changelog [<c>] --none --reason <why>
                                     Record that the change needs no entry.
  arc changelog [--json | --write]   Render unreleased copy, or write the target.

  A gate runs where its change lives. `verify`, `snapshot --verify`, `done`,
  and `rebase --verify` execute the command in the change's recorded worktree
  and record the evidence at that worktree's head, whichever checkout of the
  repository the command was typed in; the run says which tree it used when
  that is not the one you are standing in. Which gates are required is read
  from the change's target branch, wherever you stand, plus any gate the change
  itself adds, so `status`, `verify`, and `integrate` agree on what is owed; a
  change cannot delete or weaken a gate its target declares, and `status` lists
  where the declarations in play differ. A change whose target branch cannot be
  resolved is blocked (`target-unreadable`), since nothing says what it owes.
  A recorded brief's gate-shaped probe warning and `arc show`'s review
  checklist use the change's target declarations wherever they are run.
  A change whose worktree is gone has nowhere to gate: give it one with `git
  worktree add`, or record evidence arc did not run with `verify --attest`.

  Executed verification receipts separate `wait_ms` in slot or declared exclusive
  admission (`arc guide gates`), `prepare_ms` configuring build storage, and `run_ms`
  from the command's spawn through its leader's exit and output drain.
  `duration_ms` spans admission, in-run preparation, execution and
  bookkeeping. `prepare_ms` also includes preparation before environment
  probes; a batch charges that shared preparation to its first executed
  command. Preparation before execution need not be inside `duration_ms`.
  A wrapper's own admission wait belongs to its command's `run_ms`; arc
  cannot separate work inside a command. `show --json` and `status` carry
  these observations.
  `load_average` holds the host's 1, 5 and 15 minute load averages and
  `core_count` its online logical CPUs, sampled at command start.

  `cpu_user_ms` and `cpu_system_ms` sum the command's and its descendants'
  usage, excluding arc's holder. The holder is a child subreaper and uses
  wait4 for the command and adopted children, including usage their waited
  descendants contribute. CPU is absent unless every descendant was reaped;
  a timeout or group cleanup that interrupts accounting, or unavailable
  accounting, supplies no CPU figure. Work delegated to an existing external service
  is outside that accounting. Unavailable host observations are absent.
  Attestations, bypasses and older events supply no missing measurements.
  `arc stats` uses `run_ms`, falling back to `duration_ms` only for events
  without command timing. These observations impose no cost limit.

  A pass already recorded for the content answers again instead of running
  again: `verify`, `snapshot --verify`, `done`, `rebase --verify` and an
  integration queue record `verification-reused` for each declared gate whose
  newest evidence there passed under the same declaration and environment,
  and run the rest; `--rerun` on `verify` and `snapshot` runs them anyway.
  Acceptance runs each local gate itself, and `arc publication` reuses only
  with its own `--skip-green`. What a pass answers for, and the `binds =
  "commit"` declaration that narrows it to one commit, is in `arc guide
  rules-evidence`.

  A gate or environment probe runs in its own process group, which a
  terminal's interrupt does not reach. When SIGINT, SIGTERM or SIGHUP reaches
  arc while one runs, arc kills that group, records no result from the run,
  says on stderr that it was interrupted, and then ends with the same signal,
  so a calling shell sees an interruption rather than a failure. A signal
  arc inherited as ignored stays ignored.

  An exclusive admission expiry (`arc guide gates`) or a command that exits
  75 (`EX_TEMPFAIL`), as a wrapper does when a host
  admission queue gives up before any check starts, records `not-admitted`:
  evidence of nothing, neither a pass nor a failure. A timeout outranks it, so
  a command that exits 75 after overrunning its timeout timed out. The gate is
  not green; `status`, `check` and `show` say it did not run and should be
  rerun, `verify` reruns it rather than reuse an older pass, acceptance
  refuses it as unobserved, and a publication guard that ran no failing gate
  records `not-admitted` and exits 1. A not-admitted attempt cannot answer
  an earlier failure.

  A gate whose declaration names `bypass` may be skipped on a judgement:
  `arc verify <change> --gate <name> --bypass <reason>` records
  `verification-bypassed` with the actor, the change's head and tree, the
  reason and the declared qualification, and no command, exit code or
  duration. Any other gate refuses, as do an empty or placeholder reason and
  an unresolved failure recorded at that head. Only a later run green at
  that head answers the failure; a pass on an unwaived dirty tree or a tree
  that moved during the run cannot answer it. A change's own declaration
  cannot add `bypass` to a gate its target declares, and one that omits it
  withdraws it.
  The bypass meets that gate's integration requirement at its immutable
  recorded head alone, while its declaration still names `bypass` and the
  budget applicability in `arc guide gates` permits it; every new
  head, including a recorded rewrite holding the same tree, or a merged tree,
  needs a run or a new bypass. `status`, `check`, `show`, `explain` and their
  JSON name it `bypassed` with its reason, never `pass`. `verify` never reuses it, no
  publication guard reads it, it discharges no review debt, and importing one
  takes the waiver capability.

  `done` moves its caller's live claim to `verifying` before it snapshots,
  and only after every snapshot input is accepted — the attribution,
  `--thread`, and each `--journal-ref` — so an input it refuses leaves the
  claim's stage and its clock as they were.

  A profile with no declared gate runs none: `done` still snapshots and
  prints the check state, saying plainly that no gate is declared rather
  than reporting a pass, and `verify --all` and `verify --against` still
  refuse because there is nothing to run.

  While a change declares `iterating`, `check` reports `iterating` and
  suppresses `no-valid-approval`; every other blocker still applies.
  `arc iterating <change> --off` restores the approval check.

  A findings batch (`--findings-json` on `review`, `audit`, and `external
  verdict`) is read for `blocking`, `severity`, `summary`, `body`, and
  `anchor`, and an anchor for `path`, `side`, `line_start`, `line_end`, and
  `context`. A field that looks like a misspelling of one its finding or
  anchor omits, such as `blocker` or `line`, refuses the batch: the omitted
  field would be recorded at its default, and a non-blocking finding lets an
  approval through. Any other unknown field is ignored with a warning naming
  it; a finding's `id` is ignored silently, since arc assigns finding IDs.

  `review --snapshot` validates the batch and the verdict before it records
  the patchset, and keeps the change locked from that patchset until the
  verdict on it is recorded: a refused review records no patchset, and a
  command acting on the change in between reports the lock busy. Whether an
  approval gates is said once the verdict is recorded; a failure to evaluate
  it is a warning naming the recorded verdict, and the review still succeeds.

  Each acceptance probe declared on the patchset's brief blocks readiness
  until baseline evidence fails at that brief's base and final evidence passes
  at the patchset's head. Both runs must name that brief and probe; the newest
  run for each phase at its required revision decides. A brief with no base,
  or with a base equal to the patchset head, cannot discharge a probe. The pair
  proves discrimination, not relevance.

  Executed baseline probes run at the brief's recorded base. If the invoking
  checkout's HEAD differs, arc creates a detached scratch checkout under
  the configured worktrees directory (including ARC_WORKTREES_DIR). Relative
  directories resolve against the invoking checkout's root. Arc reports
  free space and the absolute path, runs the probe there, and force-removes
  it after the run. Evidence is recorded through the invoking checkout. At
  the base, the probe runs in place. Attested baseline evidence requires
  --tested-revision equal to the base and creates no checkout. A creation
  refusal prints the reason and commands to create, verify in, and remove
  a manual checkout. Failed creation removes the allocation owned by that
  attempt and only administrative entries whose gitdir names that path.
  Cleanup confirms that Git's worktree inventory contains no registration
  for the attempt's path before offering the manual recreation commands.
  Unrelated worktree registrations are preserved. Failed cleanup reports the
  retained path and its removal command instead of a command to recreate it.
  --waive-dirty refuses before recording when a baseline requires a scratch
  checkout: that checkout is clean, and the waiver would name a revision the
  probe does not test. At the base, a waiver binds to that checkout's HEAD.
  A removal failure warns with the path and cleanup
  command without changing the recorded result or exit code.

  A brief version keeps the preceding version's base unless `--base` names
  another revision; the first version defaults to HEAD. Recording prints the
  selected base, also carried as `base_revision` in JSON. Probes carry forward
  unless `--probes-json` supplies a replacement array. Repeatable
  `--probe NAME=COMMAND` adds or updates named probes; `--withdraw-probe NAME`
  removes named probes and keeps the others. Unknown withdrawals, duplicate
  declarations, and declaring and withdrawing the same name are refused.
  These flags are mutually exclusive with `--probes-json`.
  Probe edits without `--body-file` or `--scaffold` require a preceding brief
  and preserve its body, title, plan reference, slice, captured plan provenance,
  and mandatory reads. Explicit title, plan, or read inputs replace those
  fields. A body or scaffold records a replacement contract.

  An undischargeable probe is recovered by a later brief version with
  `--base` at the revision the work started from and a cause; `arc snapshot`
  then binds the patchset to that version, declaring
  the patchset's contributors, which a live claim held by another actor
  requires. `check` prints both with the change, probes, and contributors
  filled in, and only the snapshot once a later version the probe can
  discharge at exists. Each contributor is its own `--contributors=`
  argument; one the flag cannot spell exactly, because it holds a comma or
  edge whitespace, is left as a placeholder the line names.

  `arc rebase` is what `check` names when the selected integration shape
  requires replay onto the target. Where Git can, arc performs the replay
  itself with `git replay`, which runs no hooks and leaves the worktree alone.
  The replay runs in a temporary bare repository that shares the project's
  object store and inherits none of its configuration, so a merge driver or
  filter the project configures has no definition to run. arc reads the result
  from the ref Git itself updated there, moves the branch by a compare-and-swap
  against the old head, and brings the worktree to it without discarding a
  local change. The commits it creates carry no signature, whatever the
  repository configures for commits it makes; `arc rewrite sign` signs a
  replayed range. Such a replay carries the matching patchset's author
  coordinates, or completed bound-run attribution established at the original
  head before replay when no matching patchset exists, under the evidence rule
  in `arc guide debt-review`. It retains every contributor's coordinate,
  model included; the replaying session is not its implementer, and the
  patchset names exactly the tip that replay wrote. A `post-rewrite` hook, a reflog entry, a merge
  driver, or an inherited `GIT_REFLOG_ACTION` cannot make ordinary work look
  replayed.
  `--contributors` and `--solo` decide the replayed patchset's contributor
  set; a declared member the replay carries over keeps its recorded
  coordinate, and one it does not carry is credited as declared. Where Git
  cannot replay — a Git without the command, a refusal, or a conflict — the
  replay falls back to `git rebase`, whose result is recorded as ordinary
  work. A conflict stops it and leaves the rebase in progress — the partial
  resolution is yours, and aborting would throw it away — with the conflicting
  files and the commands that finish the replay. Resolve,
  `git rebase --continue`, then `arc snapshot --verify`, which records the
  resolved head under the attribution rules in `arc guide debt-review`.
  Over another actor's live claim
  an unattributed rebase refuses before the branch moves, and the change stays
  locked from that check until the replayed head is recorded. The gates the
  replayed head owes are named after it is recorded; a failure to evaluate
  them is a warning, and the rebase still succeeds.

  `integrate --shape ff-only|no-ff` selects the local integration shape.
  Without a flag, operator defaults, target project defaults, user config,
  and built-in ff-only apply in that order. Resolution and history validation
  run under the target and change locks against the policy consumed by
  authorization, retaining the original flags. Each authorization reading
  validates its shape constraints, history and declared actor requirement;
  configuration disagreement refuses before merging. Destination declarations
  are pinned to the transition's observed revision; their actor requirement
  and agreement with the authorization readings are required for integration.
  Under ff-only, the observed target must be an ancestor of the
  approved head, or already contain it. A fast-forward updates the target
  ref only if it still equals the observed revision, then updates the index
  and checkout with Git read-tree. Inherited merge settings cannot alter the
  operation. A checkout update failure attempts a guarded ref rollback that
  refuses to overwrite a competing move. Failed transition checks report the
  ref, HEAD, index and checkout state for recovery, without a reset. This
  includes target checkout takeover and every failure after Git mutation.
  Fast-forwards changing initialized submodules refuse before takeover or
  ref advancement regardless of Git submodule ignore settings; use
  --shape no-ff or a manual integration with submodule updates. Recovery
  checkout restoration includes --recurse-submodules.
  Closure publication failure reports whether the exact event is published,
  absent, or unreadable. A published event requires durability recovery and
  inspection, not another integration; an unreadable ledger must be inspected
  before retrying. An absent event can be retried after ledger repair and
  checkout reconciliation.
  Divergence observed under the target and change locks exits 11 before debt
  is declared and names `arc rebase <change> --verify`. External Git does not
  honor arc locks: if it diverges after that observation but before the fresh
  decision, the refusal retains the declared debt, and records no integration.
  The rewritten head requires fresh review or declared debt. Integration
  performs no automatic rebase under ff-only. With --into selecting another
  destination, arc rebase cannot select that destination: run git rebase
  <destination> in the change checkout, then arc snapshot <change> --verify
  and obtain fresh review or declared debt. `--shape no-ff` creates a merge
  commit with the observed target and approved head as its parents; branch
  merge options, merge.ff and squash defaults cannot change its shape.
  The selected destination supplies gate, actor, contribution and authorization
  policy declarations throughout preflight and execution. Destination names
  resolve through local branch refs; same-named tags do not select policy or
  history revisions.
  `--message` names that merge commit and requires a resolved no-ff shape.

  Several changes, or `--tag`, make `integrate` a queue in dependency order.
  Dry-run prerequisites follow supersession successors through projected
  closures with the same missing-change and cycle safeguards as execution.
  Under ff-only it lands the stacked prefix and stops at the first member
  whose target is not an ancestor of its approved head. It never falls back
  to a merge commit. Under no-ff it can replay a branch whose target moved
  with conflicts. Both shapes can run gates that lack evidence for the tree
  to ship and stop at a red gate or missing verdict. The exit code is the
  stopped member's blocker; style or history refusals exit 1. The summary
  names landed revisions, the stopped member and reason, and members never
  attempted, including when style or history constraints refuse. `--dry-run` reports the
  plan without replaying, running gates, or integrating. With a resolved
  destination, known holds, review blockers and iteration stop projection
  before advancing the destination.
  Prerequisites planned earlier are treated as conditionally closed. Each
  member resolves style, history, approval, danger and provenance against its
  projected destination.
  A divergent merge or replay makes later destination ancestry, merged policy,
  style, history and authorization conditional; stale revisions cannot justify
  a refusal. Exact-patchset review vetoes stop projection even when the
  destination is unresolved. Review refusals name the next decision required
  from the local or external reviewer. Queue execution opens the landing store
  afresh for each member, so its append policy includes earlier landings.
  Invoking-store and destination actor requirements, known history, style,
  divergence and lifecycle constraints are checked before replay or gate
  preparation, and authorization is revalidated under the
  target and change locks. Future gates, replay and
  checkout guards remain conditional.
  `--into`, `--message`, and `--debt` are single-change options: each member
  lands into its recorded target, and a debt reason binds to one patchset.

  A merge runs where the target is checked out, and that checkout blocks it
  only where the merge could change or lose bytes there. Its tracked
  modifications refuse, staged or unstaged. Untracked or ignored paths the
  merge does not write are left in place and named in the report. A path the
  merge would write that already exists there without being tracked, ignored
  paths included, is refused by name.

  A head the target already contains, at its tip or behind it, has no merge
  to make: the guard still runs every check, closes the change at the target
  revision that already holds the head, and records that no merge was
  created. A successor behind such a change proceeds normally.

  A guarded integration records the shipped patchset and head, the target
  branch and its preceding revision, its shape (`fast-forward`, `merge`, or
  `already-contained`), and its authorization basis: the
  approving verdict and its provisional reason when present, an external
  approval when consumed, one passing verification event per required gate,
  each prerequisite's satisfying closure, the empty blocking-finding and hold
  vectors, the normalized gate and policy values consumed, and the danger
  determination. A debt declaration is recorded in the basis only when its
  waiver supplied the approval or let it stand. `integrate --dry-run` prints
  the basis it would record. Before merging, readiness is recomputed and the
  basis rebuilt; if readiness fails or the basis differs, nothing is written.

  `close --assert-integrated <rev> --target-before <base>` records the
  caller's base as asserted when it is an ancestor of the revision, regardless
  of the revision's parent count. A non-ancestor is refused without recording
  events. Without the flag, a merge head supplies its first parent; other
  heads record no base. A merge's first parent describes the prior target
  when that merge performed the integration. For a fast-forward onto a
  merge head, name the prior target with `--target-before`.

  A closed change is judged at the head its closure recorded, or at its
  newest patchset's when the closure recorded none. Head drift compares with
  the closure's source patchset when recorded, otherwise with the newest
  snapshot. Status names that patchset in `comparison_patchset` and reports
  `head_matches_comparison_patchset`; `head_matches_latest_patchset` remains
  a literal comparison with the newest snapshot. Show names the shipped
  patchset when the closure records one. `status`, `show`, and
  `check` read neither its branch nor its target, so deleting the branch or
  moving the target after closure leaves the head its verdict and gate
  evidence are read against where it was. A replay with `--at` to the
  closure or any later event judges the same head. Its gate and policy
  declarations are the target's at the revision the closure recorded as
  where the target stood, plus the gates its judged head adds, so a gate
  the target declares later is not owed and declarations the target breaks
  later do not fail its report. A closure that recorded no target revision
  (an abandoned or superseded change, a legacy closure, or an asserted
  fast-forward given no `--target-before`) reads the target's current
  declarations instead. The operator's own gate and policy files are read as
  they stand now.

  Gate evidence is open-only, so a check that finishes after its change
  integrated is no gate evidence. `verify <c> --attest --after-integration`
  with `--gate` or `--command`, `--result`, `--tested-revision`,
  `--execution-host`, and `--runner` retains it as a receipt instead. The
  tested revision's tree must equal the integrated commit's; a receipt for
  other content is refused, so none reads as verification of what shipped.
  It records the tested revision and tree, the integrated commit, the command
  and the gate's declared timeout, the result, host, runner, producer, and
  when it was recorded. `show` lists it under `Verification after
  integration`; `show --json` and `status --json` carry it in
  `post_integration_verifications` with `after_integration: true`; `explain`
  lists it as later knowledge, `declared` because arc ran nothing. It names
  the integrated commit as the integration recorded it, and views follow that
  through rewrite maps, so a withdrawal strands none. An earlier release's
  receipt recorded after a rewrite names the successor instead; if that map
  is withdrawn, the receipt is set aside until the map is recorded again. It
  changes no authorization basis, discharges no debt, and satisfies no gate.
  On an open change the form is refused, naming the ordinary attestation; plain
  `--attest` on an integrated change is refused, naming this form.

  The decision is one function of facts observed under the target and change
  locks: a plan naming the target revision, the approved head, the tree the
  merge must ship, and the basis to record, or a refusal. The merge runs from
  the plan and is confirmed against it. `integrate --dry-run --json` prints
  that plan, decided the same way, as `arc-integration-plan/1`. Handing it
  back with `integrate --expect-basis <file>` never changes the decision; it
  names what moved since — the approved head, the target revision, the gate
  or policy declarations, the approval, or provisional routing and prerequisites
  — in one line beside a refusal, or
  as a warning when the integration still proceeds. A gate's declaration in
  the basis carries `binds: "commit"` when it binds to a commit and omits the
  key when it binds to a tree, so a binding changed since the plan is a move.

  When no checkout holds the target, `integrate` takes over the change's own
  checkout if it still holds the change branch, checks the target out there,
  merges, and leaves the checkout on the target. The dry run says it would.
  Any other shape keeps the refusal naming the missing target checkout. Slot
  use and release follow `arc guide slots`.

  Some changes must reach their target together or not at all. `arc metadata
  <a> --lands-with <b>` records that two open changes with one target land
  together; `--remove-lands-with <b> --reason <why>` withdraws it. Each is one
  repository event, written under the graph lock, so both ends see it at once.
  The relation is symmetric, and a component is every change it joins
  transitively. `status`, `show` and `check` list a change's component with
  each member's state.

  No integration path takes a member alone. `integrate`, a queue of `integrate
  a b` (its members are gated and moved one at a time), provisional
  integration, and `close --assert-integrated` refuse while another member is
  open, with blocker `lands-with` (exit 19) and the remedy `arc land` naming
  every open member. An integrated member satisfies its part. A member
  abandoned or superseded wedges the rest with the same blocker until the
  relation is withdrawn, and the remedy names that withdrawal.

  `arc land <change>... [--target T] [--debt <why>] [--dry-run [--json]]`
  installs several changes in one guarded transition. Members land in
  dependency order: each after every member it is `blocked_by`, ties in the
  order given; a cycle refuses. An earlier member satisfies a prerequisite,
  which the basis records as `landing <attempt>`. A member whose recorded
  base is the revision before it (the target for the first, the previous
  member's landed head after) lands as it is. When every member does, the
  landing is a stack and nothing is replayed. Otherwise it is composed:
  every other member is replayed onto that revision with `git replay`, run
  as `arc rebase` runs it, after the intent is written. The composed head is
  pinned by `refs/arc/landing/<attempt>` until the target holds it or the
  attempt ends. Each member's original commits and the commits they landed
  as are recorded in the intent alone. No `history-rewritten` event is
  written, so no revision resolution ever follows a member to a trial
  commit. A replay that conflicts stops the attempt and closes nothing,
  naming the member and `arc rebase <member>` (exit 11).

  Every open member of each component is named. Each member keeps its own
  approval, judged against the destination's policy at the revision the
  landing starts from. A replayed member whose approval read its exact
  patchset on its recorded base refuses (exit 3) unless `--debt`. With it,
  the member carries `replayed-unread`, naming that approval, its base and
  the parent it landed on (`arc guide debt-obligations`). With `--debt`, a
  member no approval covers carries the debt `integrate --debt` would
  declare. Each debt is carried inside the completion, including an
  ineligible read's event, findings reason, qualifiers and assessment basis.
  A target that integrates with no-ff or receives contributions refuses, and
  `integration.history = squash` refuses more than one commit. `--dry-run`
  prints the plan, as `arc-landing-plan/1` with `--json`: the shape, the
  members and their pins (patchset, base, head, whether it is replayed and
  the approval a replay outruns, brief digest, approval or debt basis,
  holds, findings, component, prerequisites, gates and budget constraints,
  policy, danger scope), the combined total, the components, and the gate
  union, each gate with its binding and the
  members requiring it. A composed plan names no head: the replay that
  makes it runs only once the attempt is under way.

  Both planning and execution refuse an empty required-gate union (exit 5),
  for a stack or a composed landing, before any attempt is prepared.

  The intent is written to `landing-intents/<attempt>.json` in the ledger
  directory, and a `landing-prepared` event names the attempt. Until it
  finishes, its members show `pending landing <attempt>` (blocker
  `landing-pending`, exit 20) and refuse every other terminal transition,
  naming `arc land --recover <attempt>`. The gates every member requires
  run once, with no ledger or change lock held, and their evidence carries
  `landing: <attempt>`. A stack runs the planned gate union at its top head,
  using the selected destination even when its recorded target differs;
  a gate two members declare differently, `binds` included, or one the top
  head does not declare the same way, refuses. A composed landing runs every member's
  gates at the composed head, recorded on its last member. They run in a
  checkout the attempt owns: a slot it leases when the repository declares
  `[slots]`, else a worktree of its own under the worktrees directory. The
  slot is released, or the worktree removed, once the gates have run; a
  lease the attempt still holds names it, and recovering or abandoning the
  attempt frees it, its checkout left detached. Reuse
  follows each gate's binding: a tree-bound pass by tree, a commit-bound
  pass only at the gated head's own commit. A failed gate abandons the
  attempt, closes nothing and drops the pin.

  Both landing forms fold the minimum gate budgets and total across every
  member's effective destination, head and operator obligations. Budget-only
  differences do not conflict in execution identity. The combined suite must
  satisfy the strictest total, even when each member's gate rows are green.
  Every gate in that suite counts once regardless of its requiring profile.
  Gates run sequentially, so this total bounds their summed command budgets
  within one attempt; preparation, admission, replay and additional split
  attempts are outside that bound. Before authorization, every shared receipt
  is judged against every requiring member's budget. Missing run_ms or an
  over-budget pass refuses with the member, gate, run time and budget, leaves
  the target unmoved and does not trigger splitting or an implicit retry.
  Effective budgets, totals and sources are pinned, confirmed and retained in
  each member's authorization.

  Commit takes replica authority, the target, the graph, each member's
  transition lock in change-ID order, then repository events, and reads
  everything pinned again; any difference refuses naming the field and
  abandons the attempt. The exact completion and its basis are saved
  (`authorized`), then one ref transaction moves the target from its old
  revision and creates `refs/arc/landing-applied/<attempt>`; the target
  checkout follows (`applied`); one `landing-completed` event is appended
  (`published`); the applied ref goes once cleanup is done. Each member's
  closure derives from that event: integrated via the landing, with
  `target_before` the revision the landing started from, which every
  declaration and policy reader keeps reading, and `range_base` and
  `integrated_commit` its own landed range in the installed history (for a
  replayed member, the commits it landed as), which `diff --integrated`,
  audits, `stats` and cost attribution read. The shared gate run is counted
  once, on the top member. Every reader of a closure sees a member closed
  by its landing: `doctor`, import admission, `accept` preflight, receipts
  after integration, and a cost `--footprint` of the lander. `--at` a point
  before the completion replays without it, so the member reads open. A
  member's export carries the completion that closed it; an attempt still
  pending is not exported.

  `--max-attempts N` (default 1, the first attempt included) bounds a
  landing's attempts. When a gate fails and more remain, arc splits it.
  Each must-land-together component contracts to one node over the
  members' `blocked_by`; a cycle among the nodes makes the landing
  unsplittable, and arc says so. The nodes' dependency order is cut into a
  prefix child and a suffix child. Each is a new attempt whose intent and
  `landing-prepared` event name its parent and its exact members, planned
  and gated afresh on the target as it then stands. The suffix runs only
  once every member of the prefix landed; otherwise it is reported
  blocked. A child whose gate fails splits the same way while the budget
  lasts. A failed attempt closes nothing, only the members of children
  that landed close, and arc reports every attempt with its parent,
  members and outcome. Finding every failing one of n components takes
  at most 2n-1 attempts. With a budget of 1, a failed landing names the
  option and that bound.

  `arc land --recover <attempt>` finishes an interrupted attempt. It
  publishes a saved `authorized` completion whose applied ref exists, after
  bringing a target checkout still at the old tree to the new one; it
  abandons an attempt whose applied ref is absent and whose target never
  moved; anything else it reports as movement arc did not authorize, and
  closes nothing. Run again, it changes nothing. `arc land --abandon
  <attempt> --reason <why>` abandons an attempt whose target arc did not
  move. Once the attempt is finished, either frees each slot leased to it,
  leaving the checkout detached where it stands, and touches no other
  lease; a slot still in use or left dirty keeps its lease, and arc prints
  the `arc slots release` to run once it is resolved.

  A member exported alone carries every finished landing's events: a
  completion and the gate evidence it names reproduce the member's closure
  via the landing wherever it is imported, with its own range. Admitting a
  completion needs the capabilities `arc guide replicas` lists. Import
  refuses a completion naming a member at a patchset, or a source head, that
  the member's ledger there does not hold, naming the member and the
  patchset, and writes nothing: the change the bundle carries is checked
  against every completion naming it, and each other member already held
  against a completion the bundle brings. Both follow the same combined
  repository history, including rewrites arriving with that completion; a
  signature-only successor needs no newer partner patchset. A member
  exported at a later patchset is imported before a partner whose bundle carries the
  completion. Gate evidence
  recorded at a composed head is pinned by
  `refs/arc/tree/<change>/landing-<commit>` until the member closes, so a
  failed attempt's evidence still names content both stores can hold; no
  rewrite map ever names a trial commit, on either side.

  A change whose commits reached its target as other commits that arc did
  not produce (a cherry-pick, a rebase elsewhere) closes with `arc close
  <change> --assert-integrated <rev> --target-before <before>
  --correspondence <file>`. The file (`arc-correspondence/1`, shape in `arc
  close --help`) stands in for the asserted revision containing the source
  head, and for nothing else. It names the change, the closed patchset and
  its head, `<before>` and `<rev>`, each of which must match, and maps every
  commit of the patchset, oldest first, to one commit in `<before>..<rev>`:
  one to one, each landed commit descending from the previous one, with
  equal stable Git patch ids. A merge or an empty commit on either side
  has no ordinary patch identity and refuses. Each refusal names the commit
  and the rule it broke. The closure stays asserted, never via a landing,
  carries no gate evidence, and records the pairs, which `status`, `show`,
  `explain` and an export carry.

  `integration.cleanup` governs branch and worktree cleanup after integration
  (built-in false). A slot checkout is never removed; its release detaches it
  and deletes the merged branch (arc guide slots). `integrate --cleanup` enables it; `--no-cleanup` overrides
  a configured true. `integration.discard` selects `"artifacts"` (built-in) or
  `"ignored"`. `integrate --discard <artifacts|ignored>` overrides that scope,
  implies cleanup, and conflicts with `--no-cleanup` for single changes and
  queues. The ignored scope discards every Git-ignored file and directory
  outside `.arc/`, as classified by the parent checkout's Git ignore rules.
  A worktree containing another repository is never removed, under either
  scope. Cleanup walks its whole filesystem tree independently of Git status.
  A directory with a `.git` entry or a bare repository layout (`HEAD`,
  `objects/` and `refs/`) keeps the checkout and is named. The checkout's
  own `.git` file at its root is the only exclusion. The walk never follows
  symlinks, and any read error keeps the checkout. A recorded worktree path
  that is itself a symlink keeps the checkout uninspected. Tracked edits, untracked
  files and index entries marked skip-worktree or assume-unchanged
  keep the checkout. Ignored `.arc/` content keeps it under either scope,
  including recognized artifacts; only owned, emptied outbox metadata is
  exempt. Ownership checks apply under either scope. Cleanup uses the
  cleanup, discard and disposable style values selected before the merge;
  a change's policy edits do not govern its own cleanup. Cleanup deletes
  merged branches with Git’s merged-branch check and removes only clean
  worktrees inside the sandbox. Arc removes only
  the checkout instance it created, identified by an owner record in that
  checkout’s Git administrative directory. `begin` allocates the worktree
  without checkout, writes its owner record, and then populates its files.
  Creation authority requires the registered path to retain that allocated
  administrative directory after checkout hooks run. A changed identity opens
  the change without creation authority and keeps all worktrees. `fork begin`
  also verifies the allocated directory after checkout and refuses its marker
  on a changed identity, keeping all worktrees.
  Worktree creation runs post-checkout with a null old revision, HEAD as the
  new revision, and flag 1, matching Git's creation hook arguments. Population
  or hook failure keeps the branch and worktree and records no opening or fork
  marker. The error prints adoption and discard commands. The begin adoption
  command preserves the opening options and resolved target and base. Every
  option value uses --flag=<shell-quoted value> to preserve leading hyphens.
  The retained branch determines the checkout. Fork provenance during
  adoption requires an explicit base. `begin` removes its
  unrecorded owner token when its identity matches; cleanup failures are reported.
  The registered checkout must still hold the change branch. An ignored
  directory outside `.arc/` identified as a build artifact by a valid
  CACHEDIR.TAG, a built-in recognizer, or a configured
  `integration.disposable` rule's path and marker is eligible for removal
  only if a repository scan succeeds and finds no repository. With the
  artifacts scope, other ignored content keeps it. Tracked edits and untracked files keep it under
  either scope. Recognition grants no checkout removal authority. An
  outbox containing only the spool's regular `.gitignore` with `*` and a
  trailing newline is disposable metadata only when its Git administrative
  ownership record identifies that file and records successful promotion.
  Foreign metadata and any other outbox content keep the checkout.
  The integration checkout is kept. A kept worktree retains its checked-out
  branch; each retained path and branch carries its reason. Cleanup failures
  warn after closure and do not fail integration or stop a queue.

  Integration prints the integrated revision and closure event before
  releasing retention refs. A retention cleanup failure, including a ref lock
  or a hook rejecting deletion, warns with the ref prefixes that may still
  hold pins; the recorded integration succeeds,
  including as a queue member. `close` prints the closed change and closure
  event before retention cleanup; a cleanup failure warns with the remaining
  ref prefixes and the recorded closure succeeds, including an asserted
  integration after an external fast-forward merge.

  In-place checkout creates and switches to the change branch in the invoking
  checkout, recording it as the change's worktree so subsequent commands infer
  the change. Tracked edits, staged changes, untracked files, and ignored files
  carry across when Git can preserve them. An overwrite conflict refuses with
  a non-zero exit naming the conflict, without creating a branch or opening a
  change.
  An existing branch requires `--adopt`; a branch checked out elsewhere cannot
  be switched to in place. `--adopt` records an existing worktree and creates
  none; only explicit `--no-worktree` switches to the adopted branch.

  Release copy is recorded per change and rendered at release; arc imposes
  no file convention. While an open change has no changelog record, `arc
  status` and `arc check` advise one under the code `no-changelog-entry`,
  never as a blocker, and `arc integrate` prints that advice once it has
  merged such a change. Recording `--none --reason` that the change needs
  no entry answers the advice and projects nothing; the latest record wins.
  A repository that keeps no changelog declares `none = "<reason>"` in
  `.arc/changelog.toml`. The reason must be non-empty and not a placeholder;
  `none` conflicts with every explicit `target`, `renderer`,
  `renderer_command`, or `renderer_timeout` key. The declaration silences
  `no-changelog-entry`, including integration advice. Like debt advisory
  thresholds, it is read from the invoking checkout, not the change's target
  branch. An unreadable declaration warns on stderr and declares nothing for
  `arc status`, `arc check` and integration advice; readiness is unchanged.
  `arc check` prints the declaration and its reason; `--json` includes
  the optional `changelog_none` reason in `arc-check/4`. `arc changelog`
  refuses entry recording and `--write` under the declaration. Per-change
  `--none --reason` remains valid and is unnecessary; reads remain available.
  Without the file, the target is `CHANGELOG.md`.
  Text --provenance omits the session field and masks its id as [session]
  wherever it occurs in metadata values, including derived actors. The
  provenance prefix and field names retain their literal text.
  --json retains full session provenance. --provenance refuses with --write.
  The built-in renderer replaces the `## [Unreleased]` block of the target
  `.arc/changelog.toml` names, `CHANGELOG.md` by default. Prose is refilled;
  fenced and indented code retain their lines and container-relative columns.
  Empty list items retain their markers.
  A project whose file follows another convention selects `renderer =
  "command"` with a `renderer_command` argv. That command runs from the
  repository root without a shell, with the authority of whoever runs `arc
  changelog`, reads an `arc-changelog-render-request/1` document on stdin,
  and answers on stdout: the rendering for a read, and for `--write` the
  complete replacement file, written atomically after it exits 0. It may
  answer without reading the whole request. Its process group is killed when
  it ends.
  The request's projection is the text-side view: each entry's recorded
  session is null and its id is masked as [session] in the other recorded
  values, with or without --provenance.

PREPARE A DELIVERED CHANGE
  Fetch the delivery's Git objects and recorded branch through your transport,
  then run `arc accept INPUT --branch LOCAL-BRANCH --publisher PRINCIPAL
  --received-sha256 SHA256`. The branch must exist under the recorded name at
  the delivered latest patchset head, or its recorded signature-only successor.
  Acceptance never fetches, creates a branch alias, or overwrites another branch.
  The receiver must be paired, hold integration authority, and act as lead.

  Acceptance reads and digests the bundle once and uses import admission against
  held history. It checks authority, objects, attribution, checkout identity,
  and the delivered patchset's bound brief and resolved base before importing.
  Admission revalidates under publication locks. If the authoritative target
  moved beyond that base, acceptance stops with explicit rebase, snapshot, and
  fresh review guidance; content replay requires the lead's own decision.
  Provisional ancestry requires canonical prerequisites and explicit replay.

  A delivery whose bundle binds an arc-custody-bundle/1 sidecar installs it
  with --custody-dir DIR, judged exactly as `arc import --custody-dir` judges
  it, in the import preview and before any effect; a refusal stops acceptance
  at import-preview. Without the flag, acceptance still imports, and the
  import-preview and import steps and next_steps say custody is unavailable,
  name the manifest digest the bundle binds and the flag that installs it.

  Acceptance reuses a clean registered checkout of the branch or allocates an
  owned checkout under the configured worktree root. --worktree selects an
  existing matching checkout or an absent path, never unrelated content.
  A ChangeCheckoutBound fact binds the checkout to its recording replica.
  Only locally allocated ownership grants cleanup rights; relayed paths and
  tokens grant none. Local facts receive IDs after admitted change and repository
  events, independent of the sender's timestamps. Binding, signing, snapshot,
  and verification records retain their own observation times. Operational
  projections choose a local binding and clear foreign paths and ownership.

  In order, acceptance imports, binds the checkout, signs exclusive change
  commits using Git's configured key (or --key), snapshots, runs required local
  head gates, evaluates a distinct target tree when needed, and runs each bound
  probe at baseline and final in declaration order. Baselines use scratch
  checkouts at the bound brief's base. The snapshot preserves the delivered
  patchset's resolved base, brief binding, and explicit contributors, including
  when a foreign actor holds the claim. Insufficient legacy attribution requires
  --contributors or --solo. Imported passing evidence never suppresses the local
  head-gate run. A profile without gates evaluates nothing.

  Preparation records evidence and returns readiness; it records no judgment.
  Signature-only rewrites carry eligible existing approval through the verified
  rewrite map. They emit no verdict. Genuine eligible verdict forks retain all
  tip IDs and origins and refuse readiness; the local reviewer resolves them
  through the explicit superseding verdict path after reading the signed head.
  A configured foreign-review exception remains visible in verdict assessments.
  Preparation commonly completes with exit 3 and local review still owed.
  Gate failure exits 5, probe discrimination exits 12, and execution or
  observation failure exits 1; an unavailable runner proves no discrimination.

  --json emits one arc-accept/1 document containing delivery coordinates,
  prepared head, worktree, preparation_complete, ordered operation argv/cwd,
  outcomes, exit codes, recorded event IDs, stopping step, readiness, verdict
  tips, local_verdict_owed, and next_steps. Each step distinguishes commands from
  internal operations. Command argv and cwd reproduce the selected operation;
  checkout allocation uses --no-checkout, followed by an internal owner/binding
  record, explicit read-tree population, and the post-checkout hook.
  A local allocation record binds repository, change, branch, expected head,
  physical checkout and Git administrative directory to its reserved owner token.
  A binding refusal retains this proof and reports unpublished binding state.
  Retry revalidates registration, identity and ownership before finishing the
  binding and population. Only an empty, index-free allocation containing its
  own .git link can be populated; unrelated content or a displaced owner refuses
  recovery. The record is removed only after a local binding and population
  complete. Relayed paths and tokens supply no allocation proof.
  Steps report observed ref changes, written object IDs and kinds, checkout
  allocations/removals, written administrative owner records, unpublished local
  bindings and allocation recovery, and pending rewrite intent with its map, planned refs,
  current ref values, publication event, and continuation argv/cwd. These effects
  include commits and ref moves preceding event publication. Observation errors
  remain explicit and cannot turn incomplete accounting into successful
  preparation. Human output prints the same actual constituent operations. The first failing step stops the coordinator; earlier
  import, signing, checkout, snapshot, and evidence effects remain visible,
  including unrecorded signing effects. Restore ledger publication access before
  continuing a pending rewrite; the intent preserves the exact objects and ref
  moves to reuse.
  Retry revalidates the same delivery and reuses admission, local binding,
  unchanged patchset, and rewrite recovery. Checks may record fresh evidence.

  --dry-run performs admission and local-state preflight and prints conditional
  steps. It imports nothing, allocates no checkout, creates no Git objects,
  signs nothing, runs no checks, and records no evidence. Signing and future
  verification outcomes remain conditional.

PAIR REPLICA STORES
  A change built on a paired worker moves in four steps: `arc snapshot` then
  `arc export` on the worker; carry the bundle and its Git objects with your own
  transport; `arc accept` on the authority (`arc guide delivery`); then review
  and `arc integrate` there.

  A rebase on the worker requires a patchset before export. On the publishing
  replica, record the branch head with
  `arc snapshot <change> --contributors <the implementing actor>` when the
  publisher only rebased, keeping the implementer as the contributor. For
  multiple implementers, supply their comma-separated actors. Export again,
  then accept the new bundle on the receiver. Acceptance requires the branch
  head to match the delivered latest patchset head or its recorded
  signature-only successor.

  Import reads received bytes once and checks a supplied --received-sha256
  before parsing. Object availability and publishing authorization are checked
  before publication. Either refusal leaves events, receipts, configuration,
  format stamps and refs unchanged. --provenance-only permits missing listed
  Git objects; publishing authorization still applies. Git checks do not fetch
  from promisor remotes. A candidate's optional original evidence-request
  digest binds its received document and declarant before publication. Records
  without that intent remain readable and importable without establishing a
  registration retry.
  Pairing grants project membership, never publishing capabilities. A paired
  receiver requires --publisher PRINCIPAL and --received-sha256 SHA256 on both
  import commands. Enroll publishing principals only in operator-policy.toml:

    [replica]
    trusted_review_replicas = []
    [[replica.publishers]]
    principal = "worker-channel"
    source_replica_id = "worker-store-id"
    [[replica.publishers.origins]]
    replica_id = "worker-store-id"
    roles = ["facts", "review-input", "debt-input", "rewrite", "provisional-integration"]
    actors = []

  The receiving caller obtains the principal from authenticated transport and
  computes the exact received file's SHA256 independently of bundle contents.
  Arc checks the assertion against local enrollment and binds the exact bytes
  to the supplied digest. Arc does not authenticate a flag or actor string;
  integrity against a supplied checksum establishes no identity or independence.
  A caller able to edit policy or assert arbitrary principals controls admission.

  source_replica_id identifies the exporting store; repository_id identifies
  each event's recording origin and the change's original store. Relay export
  preserves all recorded identities. A publisher needs explicit per-origin
  capabilities to introduce relayed events. Identical held events need no grant.
  Enrollment and review trust are operator-only; project policy cannot grant
  them. Unpaired receivers without enrollment retain legacy file imports.
  Admission receipts bind each permission to its scope, recorded origin and
  full canonical event content digest. An event ID cannot transfer permission
  to another record. Conflicting event identities refuse the entire import.
  Operator policy replacement and admission share a publication lock. Admission
  revalidates enrollment after acquiring the format and publication locks;
  revocation before that boundary refuses without publishing or changing format.
  A published batch advances store format 7. An unpublished failure leaves its
  format stamp unchanged. A failure after publication reports publication state.
  Finding release, waiver, debt discharge and provisional corroboration use
  eligible graphs. Provisional obligations use the same local-first verdict
  history as approval: an eligible foreign claim creates no obligation beside
  an authoritative local judgment. Debt discharge retains its exact witness
  event for explain and every receiver projection.
  No foreign replica supplies review authority by default. A foreign verdict
  needs both admitted review-authority and current trusted_review_replicas
  membership to gate. Contributor and assumed-identity checks still apply.
  Recorded supersession history remains visible; foreign edges cannot replace
  a local refusal or settle a contested local graph. Review-input audits cannot
  discharge debt or corroborate provisional approval; debt-input cannot waive
  approval. A landing completion needs integration, and debt-input or waiver
  as well when it carries a member's debt; it never grants rewrite, so a
  replayed member's commits map to nothing without a rewrite event of their
  own. Status and check separate head coverage from gating permission.
  Historical closures remain facts; legacy events have legacy-unknown admission
  provenance, with no transport authentication invented for their claims.

  A store keeps its own repository ID as event provenance. `replica init`
  records the first replica and logical project ID. `replica pair` records an
  operator-named peer by its repository ID; that peer adopts the project only
  by importing the exported pairing event. Replica files contain identities
  and authority events, while checkout paths remain local to their store.

  The first replica holds integration authority. `authority offer` relinquishes
  it when the offer is recorded; the named replica acquires it when it imports
  the offer file. Export the recipient's replica events back to the offering
  store to report the acquisition there. Reclaim requests return from the
  recipient and leaves the origin blocked. The recipient imports the request,
  records `authority confirm-return`, and exports that confirmation. The
  origin reacquires authority only after importing it. A recipient that has
  forwarded authority cannot confirm its return.
  arc integrate CHANGE --provisional [--into STAGING-BRANCH]
      [--for-replica NAME | --for-forge REFERENCE] [--message TEXT]
      [--debt REASON [--kind KIND]] [--dry-run] [--json] [--expect-basis FILE]
  arc check CHANGE --provisional [--into STAGING-BRANCH] [--explain | --json]
  arc replica provisional resolve CHANGE --integration EVENT-ID
      [--provisional EVENT-ID ...] --because REASON

  Explicit provisional preparation is available on paired replicas without
  authority. Plain integrate refuses there with exit 17. Preparation creates
  an unsigned no-ff merge through plumbing and updates only an unchecked
  staging ref and refs/arc/provisional/<change>/<event>. The default staging
  ref is refs/heads/arc/provisional/<local-replica>/<recorded-target>. Supplied
  destinations stay in that namespace; canonical targets, source branches,
  the primary checkout branch and replica.protected_targets are protected.
  Queues, --tag, --shape ff-only and --cleanup conflict with provisional mode.
  Publication history shape constraints apply to canonical publication;
  canonical review, actor and gate rules and additional staging/head gates
  still govern preparation. A staging policy cannot weaken them.

  [replica] provisional_enabled defaults to enabled for paired workers;
  an explicit project or operator false disables it. protected_targets is
  the union of explicit project/operator declarations and protected branches.
  Recipient routing defaults to the known authority holder. An authority
  offer in flight requires --for-replica or --for-forge.
  Check reports an executable `integrate --provisional --dry-run` command with
  explicit recipient routing and the selected staging destination when authority
  is in flight.
  A forge reference is opaque: preparation performs no transport and grants no authority.
  An intent binds the exact event and guarded ref transition before visibility;
  publication failure reports the merge, observed staging and retention refs,
  event state, retained intent and exact recovery command. Execution --json
  emits `arc-provisional-transition/2`. Execution JSON includes the debt
  declared before any subsequent refusal, including a failed ledger reread,
  naming its written or reused event,
  bound patchset, reason and declaration details. Its `publication` states what
  was observed of the event: `durable` once the file and its directory entry
  are synced; `visible-unconfirmed` when the event is readable but the sync
  failed, so a retry that answers with it confirms it; `absent`; or `unknown`
  when the ledger could not be read. A declaration that fails after the
  attempt still reports the debt, with disposition `attempted`, beside
  `transition: null` and the error, and `publication_error` naming the failure.
  Text output ends a refusal after a declaration with `debt event <id>
  <written|reused> publication=<state>`; a declaration that failed to publish
  names the same event and state in its error.
  Doctor, status and workspace report expose unfinished intents separately
  from published obligations and review debt.
  Recovery of an applied transition precedes new-preparation eligibility checks,
  including changed authority or provisional_enabled declarations, and retains
  its exact event and authorization. Later preparations may advance the same
  staging branch: the exact retention pin and containment in its staging history
  prove the applied transition, whose recovery publishes its event without
  resetting the later staging head. Divergent refs preserve the intent and refuse.
  An unapplied retry rereads lifecycle and every readiness guard under its
  transition lock. A refusal retains the saved event and authorization; an
  eligible retry must still match that basis and ref transition exactly.
  Explicit destination or recipient changes refuse before any mutation and
  name the exact saved retry parameters.
  A retry finishes that transition or preserves competing refs, and a completed
  matching preparation reuses its merge and event. Its dry-run plan names
  reuse_event_id and reuse_commit and declares that it creates no merge.

  Authority verdicts retain their observed grant and holder. A verified refusal
  remains attached across authority transfer until an explicit superseding
  judgment disposes of it or an accepting integration settles the delivery.
  An approval disposes of a refusal through a recorded supersession relation
  on the delivered patchset, with its publication grant verified. Event-ID order
  and wall clock never establish that observation; a late earlier-grant approval
  without the relation leaves the refusal attached.
  Review eligibility follows the shared admission, trust and independence rules.

  Every delivery is retained across snapshots and closure. Status lists its
  recipient, recording replica, merge and disposition. Catchup and workspace
  report list unresolved deliveries in separate provisional-integration
  sections, including age and refusal/reconciliation evidence. They are not
  review debt; query --provisional selects provisional review approvals.
  Changes requested, abandonment and supersession leave publication owed.
  Local preparation may consume an applicable local provisional prerequisite
  held in the selected staging history without an authority rejection; its
  authorization records provisional_prerequisites apart from canonical closures.
  Imported preparation never satisfies a canonical dependency.

  arc begin dependent --target MAIN --base STAGING-BRANCH --blocked-by PREREQUISITE

  Publish prerequisites canonically before dependents. A dependent containing
  provisional ancestry requires replay onto accepted authority history,
  a new patchset and fresh review or explicit debt. Substituting an authority
  merge for a provisional merge is a content rewrite, not signature-only.
  Authority integration atomically records closure and accepted deliveries,
  distinguishing same-head from revised-head acceptance. Review eligibility
  is the shared admission/trust policy, including independence requirements.
  Worker debt imported without waiver authority remains an obligation input.
  Late delivery requires explicit authority-owned reconciliation naming the
  valid accepting integration and reason. Forge settlement requires asserted
  integration with matching external-reference evidence and is labeled
  resolved by assertion. Conflicting or missing references refuse.

  Imports have a dry-run preview, are idempotent, and record a receipt with the
  source replica and bundle digest. A paired replica without authority is
  refused by `integrate`.

  Journal artifacts travel between paired replicas as a versioned bundle:
  each body, the events recorded about it, its digest, the exporting replica,
  and every artifact the selection references by filename. An import is
  all-or-nothing within each artifact: bundle integrity is validated before
  anything is written. Journal bundle checksums retain event field order and
  opaque payload tokens, excluding formatting whitespace.
  An artifact with an unsupported event is held back in full, naming the event
  kind and its journal-events schema. Artifacts that
  reference a held-back artifact are held back too, retaining the reference
  closure. Other artifacts are admitted. A receipt covers only admitted
  artifacts; every import retries the held-back artifacts.
  Deduplication includes understood physical log records rejected by state
  replay.
  Storage tiers must agree for a held artifact. Appended sections are keyed
  by their recorded event identities. Each answer append has its own identity,
  including answers to reopened questions that repeat a choice. A prefix
  revision retains the longer body; concurrent appends merge as a union ordered
  by event timestamp, then
  event ID. The base and shared blocks must match byte for byte; divergence
  refuses the whole bundle, naming both digests and the first differing block.
  An absent artifact lands in the storage the bundle records. Events and
  appended blocks keep their recorded provenance. A live claim arriving on
  an artifact a live local claim holds is reported as a contest naming both
  replicas, and nothing on either claim's behalf is recorded. Importing the
  same completely admitted bundle twice changes nothing; the receipt names
  the source replica and bundle digest. An interrupted write retains a prepared transaction with
  the bodies, event log, and receipt. Every journal write, including provenance
  repair and spool promotion, refuses a pending import and names its bundle
  digest and the remedy: `arc journal import <bundle>`, or `arc journal
  --global import <bundle>` when the global journal holds it, including a
  global write or promotion run from a project checkout. Reads stay allowed.
  Import completes the transaction before comparing revisions. Recovery refuses
  to overwrite unexpected body or event-log changes after preparation.
  A dry-run requires the pending exchange to be completed first.

KEEP WHAT THE WORK DISCOVERS (mid-change, before it is lost)
  arc keep --kind rejected   --body "<why it failed>" --evidence "<what showed it>"
  arc keep --kind verified   --body "<the premise checked>"
  arc keep --kind constraint --body "<what must be respected>"
  arc keep --kind hypothesis --body "<believed, not established>"
  arc keep --kind verified   --body "<premise>" --cites <event> (repeatable)

  Compaction is lossy compression chosen by something that does not know what
  will be needed. The session doing the work does. A fact filed here is in the
  ledger, so there is nothing for it to survive: `arc resume` hands it back to
  a compacted or cold successor.

  File the moment a premise is checked or an approach is abandoned. A rejected
  approach is the highest-value kind and the least likely to be re-derived —
  a cold session will cheerfully try it again. Keep the kinds honest: a
  hypothesis recorded as verified is worse than not recording it.

  `--cites` names a verification, verdict, finding, disposition, or earlier
  kept fact on this change that the fact rests on. It is checked when the
  fact is kept: an id naming nothing on this change, or another kind of
  event, is refused. `--evidence` stays free text and a claim either way.

  Selectivity is the point. Filing everything rebuilds the transcript this
  exists to replace, at higher cost and in an append-only record.

STANDING REFERENCES (what a project keeps coming back to)
  A research note, a finished plan or an argued discussion can outlive the
  work that produced it. Pin it to keep it in view and out of the queues.
  A pin is a fact about one artifact of any kind, consumed or not, never a
  kind of its own.
    arc journal pin <file.md> [--reason TEXT]
                         Record a `pinned` event. The body must be in hot
                         storage. Pinning again with the recorded reason
                         writes nothing; another reason replaces it.
    arc journal unpin <file.md>
                         Record `unpinned`; an actionable artifact returns to
                         the queues. An artifact that is not pinned refuses.
    arc journal pins [--json]
                         Pinned artifacts, most recently active first: title,
                         reason, appended block count and the newest block,
                         and storage when the body is not hot. `--global`
                         reads the operator journal; `arc workspace pins`
                         reads every registered project and the operator
                         journal.
  `catchup` and `journal catchup` list pins first in the journal section. A
  pinned artifact never enters `journal open`, nor the inbox, backlog and
  report tiers read from it; `journal filed` never marks it stale; `journal
  archive --consumed` keeps it and names it on stderr; an explicit `journal
  archive` or `journal relocate` refuses until it is unpinned.

  A living document grows by appended sections, not by successors:
    arc journal append <file.md> --body-file - [--title TEXT]
                         `journal evidence` under the name that fits. The
                         title is one line, written after the heading's
                         writer and time (`### Evidence <id> (<who>, <ts>):
                         <title>`) and recorded on the event, so every reader
                         still keys on the ID. `journal correct --target
                         <evd-id> --field title` amends it; corrections and
                         retractions are not counted as appended blocks.

  A page is the readable copy of one artifact away from the terminal:
    arc journal render <address> [--out FILE] [--strip-private|--keep-private]
                         One standalone HTML page: the corrected title, the
                         body, each appended block under its title and
                         anchored by its ID, and a contents list (a rail on a
                         wide screen; folded on a narrow one past eight
                         entries). Corrections
                         and retractions show where they apply rather than as
                         sections. No scripts and no referrer: raw HTML shows
                         as code, links keep http(s), mailto and in-page
                         anchors, images load only over https. Takes the
                         addresses `show` takes.
                         A home-directory path, a path under a directory arc
                         knows is local (home, arc's data home, the journal,
                         the checkout), a private, shared or link-local
                         address, or a `.local`, `.lan`, `.home.arpa` or
                         `.ts.net` host refuses with each line and text;
                         `--strip-private` replaces them in the page,
                         `--keep-private` renders them for a page that stays
                         on its owner's network, and `journal redact` removes
                         them from the artifact.
    arc journal render <address>... --json [--fragment]
                         Many pages in one run, each journal's log read once:
                         `arc-journal-render/1` carries the stylesheet, then
                         per address its file, title, kind, block count, the
                         sha256 and length of the body as read, and its HTML,
                         or the error that stopped it (exit 16; the rest still
                         render). `--fragment` drops the document shell, the
                         stylesheet and the dated footer, for a host page that
                         draws its own around the markup; the stylesheet
                         honors `data-theme="light|dark"` on the root element.
    arc journal published <file.md> --url URL
                         Record where a published copy lives (`published`
                         event; the latest is in force). Arc never publishes
                         or fetches. `render`, `pins` and `catchup` name it.
  An artifact address in a page (`<project>::<file.md>`, a filename, an entry
  ID, or `<file.md>#<entry-id>`) links to the target's published copy, and an
  entry ID of a block on the same page to its anchor; anything else stays
  text. Rendering to the same `--out` path each time lets a publisher that
  maps a path to a copy update that copy.

END A SESSION (what the next one reads)
  arc journal handoff <topic> --derive        Stopped midstream.
  arc journal conclusion <topic> --body-file - Finished the thing.
  arc journal memory <topic> --body-file -     Learned a durable project fact.
  arc journal memory --global <topic> --body-file -
                                              Learned an operator-wide fact.
  arc journal promote <file> --global         Promote a project memory.
  arc journal consume <file> --outcome done    Drain what you resolved.
  arc journal archive --consumed               Move the drained to cold storage.

  ORIENT above is fed entirely by these writes: `catchup` surfaces memories,
  `journal open` lists what a handoff parked.

  Global memories belong to the operator and apply across projects. Their
  journal is `<ai-home>/arc/journals/global`; `ARC_AI_HOME`, then `AI_HOME`,
  select an explicit AI home, otherwise `--sandbox`/`ARC_SANDBOX` supplies the
  home beneath which `.local/ai` lives. `arc catchup` lists global timestamp,
  topic, title and age in days beneath project memories, without bodies.
  Read a body with `arc journal show --global <file>`. An unavailable global
  journal has one warning naming its source; JSON carries global_memory_error
  in the journal field. Project context and other readable sources remain
  in the orientation. An unavailable project journal carries
  project_journal_error while global memories remain readable. Recording
  discovery failures appear in session_discovery_errors; arc event writers
  still supply session activity.

  `arc journal --global` selects that journal from any directory for all
  kinds and their positions and corrections, including `memory`,
  `show`, `list`, `memories`, `consume`, `position`, `export` and `import`.
  Project directory mappings and `ARC_JOURNAL_DIR` do not redirect this scope.
  Promotion takes a live project memory, preserves its body in the global
  journal, links the journals in both directions and consumes the project
  record as `superseded`, naming the global file. Historical project bodies
  remain available. Existing memories stay where the operator filed them.
  Both journals must have settled exchanges before promotion writes. A durable
  intent fixes the destination body and both journals' event records in one
  recoverable transaction. Complete bodies publish without replacing a file.
  Subsequent journal writes finish interrupted promotions; retrying promotion
  verifies the body and replays both relation records before reporting success.
  Global recovery remains discoverable until source intent removal is durable.
  A remaining source intent whose exact relation records are committed in both
  journals needs only metadata cleanup after verifying both bodies in hot or
  archived storage. Recovery preserves their storage and subsequent events.
  Changed prepared bodies, event logs or a different global journal refuse
  recovery. Open claims must be released with
  `arc release-claim <file> --outcome paused|abandoned|expired`.

  A trailing event line without its newline is a torn append. Readers refuse
  it; writers complete it from the shared prepared transaction. Without a
  prepared record, the diagnostic gives a command that preserves the log and
  removes only its incomplete tail.

  Global exchange uses `arc-journal-bundle/2` with logical project identity
  `arc-global-journal` and a local replica identity held in `replica.json`.
  The identity is allocated on a global write. Global import accepts global
  bundles without Git replica pairing; the operator chooses which bundles
  to carry. Project exchange retains its project identity and pairing checks.

  Which one is decided by how the work ended, not by how much there is to say.
  Stopped midstream — a handoff; `--derive` reads branch, worktree, head,
  distance from the target, change and claim out of the repository, so the
  author writes only what is done, what is open, the next action, and the gate
  command. Finished — a conclusion. Learned something that will be true next
  month and is not in the code — a memory, which every later `catchup` shows.
  Never label unfinished work a conclusion.

  A session journals at five moments, and none of them is the end. A premise
  checked or an approach abandoned is kept as it happens (`arc keep`), while
  the reasoning that produced it is still recoverable. A filed claim found
  wrong is corrected in the same breath as learning so (`journal correct`, or
  `journal retract` where the entry should stand withdrawn), because a false
  artifact read as authoritative is worse than no artifact. More evidence for
  an item that stands is appended with `journal evidence`, not filed as a
  second item. A round that
  closes having deliberately left work records what it left (`run end
  --note`), since a deferral living only in a transcript was dropped rather
  than deferred. A session stopping midstream writes a handoff (`journal
  handoff --derive`), which is cheap enough to write at any interruption
  because only what was learned has to be written by hand. And anything worth
  doing that is not this change's work — a gap in another tool, a proposal, a
  field datum about a model — is filed in the owning project's journal when
  it is noticed (`journal todo` or `journal feature-request`, with `--source`
  naming this session and an `--item-key`), not saved up for the end, so a
  later scan of the same recording finds it already filed. The end of a
  session checks that nothing is left; it is not when filing happens.

CHECKPOINTS (a revision handed over for testing)
  arc checkpoint record <name> [--rev REV] [--tag] [--note TEXT]
  arc checkpoint show <name> [--json]
  arc checkpoint list [--json]
  arc journal handoff <topic> --checkpoint <name>

  A series that pauses so somebody can test an integrated build records a
  checkpoint at the revision handed over (`HEAD` unless `--rev` names one).
  It holds the revision, its tree and branch, every open change with its
  patchset and head, every live, unconsumed journal todo of the project,
  and the name of every todo filed by then, open or consumed. Names are
  unique in a repository. `--tag` also writes the annotated tag
  `checkpoint/<name>` with `git tag -a`, signed as Git's configuration
  signs tags; an existing tag refuses with nothing recorded and is never
  adopted. The tag is written before its event, so an interruption between
  them leaves a `checkpoint/<name>` tag no checkpoint records; delete it
  with `git tag -d checkpoint/<name>` and record again. When the event
  append fails, the tag is deleted again only if the event is known
  absent; an event published but not confirmed durable, or one whose
  publication cannot be told, keeps its tag and the error says which.
  The event is a plain fact: it grants and requires no authority.
  Recording pins nothing: the revision and every change head the checkpoint
  recorded are commits the ledger cites, so a rewrite arc performs or
  records holds each one it replaces under
  `refs/arc/retained/repository/<event>/<commit>` (`arc guide
  rewrites-evidence`), with or without `--tag`. `arc doctor` advises on one
  no ref reaches and names one this repository no longer holds.

  When findings come back, `show` sorts them against the checkpoint. A
  change open at it is integrated (whether the integrated commit is inside
  the checkpoint revision is stated), closed otherwise, or still open with
  its head then and now. Landed since lists every integrated change whose
  integrated commit its target branch reaches and the checkpoint revision
  does not, by Git ancestry, open at the checkpoint or not. Ancestry Git
  cannot answer because a commit is not in this repository is reported as
  unknown with its reason, never as not reached: a missing checkpoint
  revision leaves landed since unknown, and a missing integrated commit
  lists its change as landing unknown. Each todo open at it is consumed
  since, with outcome and time, or still open. Filed since lists the todos
  the checkpoint did not record as filed; they are not part of it. A
  checkpoint recorded without that list compares filing times instead and
  marks the answer approximate. A finding about a change landed since is fixed since, one matching a todo
  is already filed, and the rest are new. `arc journal checkpoint` is a
  different record: progress on a claimed journal artifact.

  A handoff stopping at the checkpoint names it with `--checkpoint`, which
  adds one line naming the checkpoint and its revision and refuses a name
  the repository does not record.

PROFILES (--profile, default local)
  direct   Bounded, reversible, one session and checkout. Checkout follows
           [change] checkout (built-in in-place) and --worktree/--no-worktree.
           Implement, verify, review the diff, commit. No journal topic.
  local    Spans sessions or roles, or needs host-local evidence CI cannot
           reproduce. Dedicated branch, checkout follows [change] checkout
           (built-in in-place) and --worktree/--no-worktree. Verdict or debt per
           REVIEW COVERAGE AND DEBT, resolved style (built-in fast-forward).
  forge    A hosted PR adds a remote record, inline discussion, clean runner
           evidence, or branch protection. local underneath; project only public
           integration facts into the PR. Delivery and disclosure evidence is
           local, non-authorizing context; forge closure grants no approval.
  release  Deployment, publishing, irreversible migration, security-sensitive
           work. forge plus explicit release and rollback gates.

  Promote when the work reveals more scope, risk, or concurrency; record why.
  Never keep an undersized profile just because the change started there.

FORGE
  query --forge PROVIDER:HOST/OWNER/REPO#NUMBER searches all recorded identities,
  including superseded links, returning every matching change. Arc never
  contacts a provider. forge show CHANGE [--history] [--observation OBS]
  [--body GENERATION] [--json] reconstructs held evidence. The alternate
  forge show --provider P --host H --repo OWNER/REPO --pr N returns all matches.
  arc-forge-view/1 includes observations, generations, coverage, custody
  availability, patchsets, candidates, runs, acceptance and review bindings.
  show, status, log and explain carry the same reconstruction facts, including
  local changes with delivery, disclosure or candidate/manual-origin records.
  Provider lookup traverses their explicit reference and adoption relationships.
  Related workflows retain their local closure alongside provider lifecycle;
  neither a context relationship nor public closure supplies an approval.
  Deliveries, disclosures and candidate context retain typed records. Their
  source_availability distinguishes valid, conflicting and unresolved references;
  byte_references names current receipt/outgoing custody separately from reported
  outcomes. Source read failures degrade those references. Selected Git support
  and private ledger records retain their access-basis labels and current availability.
  Exact coalesced event aliases and read IDs use the same source selector as
  review bindings. New bindings refuse unusable sources; a later conflicting
  read preserves earlier declarations and exposes their current source conflict.
  Local workflow records and current related workflows are separately named
  reads; --observation bounds provider content and lifecycle, not local acceptance.
  show --json and status keep recorded forge state in forge and current reads
  in forge_view. Exact provider lookup requires a recorded provider identity;
  a legacy link without one does not establish a provider coordinate.
  --observation names an exact event or read ID and freezes forge content and
  its acquisition-history frontier. Links, lifecycle facts and the checks
  rollup are those bound to observations in that history;
  local_resolution.checks is the last such rollup, null only when none was
  recorded there, and a review binding's later_observations counts only the
  reads inside it. A provider lookup applies it to the changes that hold the
  read, shows the others without it, and refuses only when none does. Local
  rewrite resolution and current byte availability are
  separate reads. Change and repository event frontiers and reader options
  are included. With --at, show, status and explain stop the change events,
  run references, local records and their source and byte references at that
  event; repository history, rewrite resolution and retained bytes are
  current reads.
  Freshness names the last successful read, latest attempt, age and clock
  anomalies. A failed acquisition does not erase a successful retained read;
  recording time never establishes present provider agreement.
  --body emits exact verified retained bytes to stdout with metadata on stderr.
  JSON carries base64 bytes or null with an availability diagnostic. Missing
  bytes weaken only claims that need them; they imply neither empty content,
  zero cost nor a missing judgment. forge show records nothing and creates
  no ledger directories, refs, reports or snapshots.
  The local round trip joins recorded forge generations to declared preparation
  origins or candidate parent/adoption paths, recorded selection/promotion,
  accepting patchset and local closure. `show --json` names these under
  round_trip; its candidate_paths retain origin changes and exact promotion
  patchsets/revisions. The local_integration_revision and provider lifecycle are
  independent: closed is closed-unmerged; merged names the observed merge SHA.
  A local cherry-pick, accepted candidate or public closure proves no forge
  merge or independent approval. Manual preparation declares its origin with
  `context declare --forge-origin CHANGE:OBSERVATION:GENERATION`. Only a
  change with the forge profile, recorded forge facts or such records carries
  forge_view and a Forge round trip section in its text; any other change,
  integrated or not, carries neither.

  `message CHANGE --type note --summary TEXT --delivery-file FILE` admits
  arc-forge-delivery/1: message_id, attempt_id, evidence_id, optional
  previous_evidence, run, transport_request, sources and separate delivery,
  acknowledgement and acted_on objects. Sources name change_id, observation
  event ID and generation. Each outcome has standing (observed, uncertain,
  not-observed), detail, evidence RetainedContentRefs and source generations.
  Observed outcomes require evidence. Identical attempt/evidence and message
  retries return the original event; conflicting reuse refuses. A later outcome
  names previous_evidence on the same immutable attempt; earlier uncertainty
  remains recorded. A transport request cannot name two attempts. A delivered
  message never implies acknowledgement, action, paid execution or authority.
  Messages and all imported evidence remain non-authorizing.

  `context declare --disclosure-file FILE [--custody-dir DIR]` admits
  arc-disclosure/1 privately. It records disclosure_id, optional
  previous_disclosure, selected source generations/fields with access_basis
  (accessible-git, accessible-forge, withheld-private-assertion), allowlist_version,
  validator_version, author, optional reviewer/editor, discloser (each a
  participant: a placeholder or a session id is refused, and absence is
  null), recipient_access_basis, omissions, outgoing RetainedContentRef and exact
  readback source generation. A selected source names a forge generation,
  a ledger event (change_id/event), or Git support (change_id/commit); Git
  support uses a full lowercase held commit ID, never a tag or blob, is pinned
  under refs/arc/retained/ before the declaration is recorded, and travels with
  the object closure where this repository holds the commit.
  Repository-scoped ledger sources name change_id=repository and travel with
  repository events rather than a related change.
  Outgoing and readback digests and lengths must
  agree, and verified exact bytes must be in custody before append. A missing
  or corrupt file refuses a local declaration. Import preserves the historical
  manifest when bytes are unavailable; current availability remains separate.
  Arc records evidence of actual bytes, never renders or publishes externally,
  and a validator version is an attributed assertion, not a security proof.

  An optional comment object retains comment_id, act_id, material authors,
  optional source_frontiers/source_statements and roles, renderer_version and an optional
  opaque cost reference. Each author names the original act and either a human
  name or optional harness/model/effort with act-specific model_observation.
  Absent effort has no guessed suffix. Supplied agent coordinates follow the
  shared coordinate rules: placeholders and malformed separators refuse; missing
  model/harness evidence degrades the projection without inventing a coordinate. Material synthesis names each author
  and its source statements; quoted, transported and implementation participants
  remain roles and never become authors automatically; a role's participant
  follows the same participant rule. Corrections name an
  editor and previous_disclosure, preserve original authors, and bind the
  outgoing/readback generation independently of the original judgment. A comment
  identity has one original disclosure; later annotations link its lineage.
  Local readers can project one compact line: Author: harness:model#effort ·
  Review cost: unmeasured. A recorded cost reference reads Review cost: <basis>
  as of <as_of> instead; arc invents no amount. Multiple material authors use Authors. Missing model
  evidence reads model not recorded. A cost reference carries reference, as_of,
  scope=review-exchange, basis, coverage and source_frontier; it is separately
  owned evidence, not a number or a whole-session/implementation allocation.
  Arc never infers monetary amounts or numerical zero. A later reconciliation
  is another linked annotation/disclosure, preserving its disclosed frontier.

  Export includes flat full related_changes histories for source/context and
  candidate adoption and explicit predecessor-record closure, plus their custody
  references and held Git objects.
  Related members share the root custody manifest. Import validates every
  member before publication, then admits each through normal guarded imports.
  Publication is atomic per change; an interruption can leave already-admitted
  related histories, and an identical retry completes the remaining members.
  Missing bytes degrade only dependent claims. Export never initializes a replica.

  forge observe CHANGE --file frontier.json [--json] admits an adapter's
  arc-forge-observation/1 document. Admission never changes readiness. A valid
  failed or truncated read succeeds; invalid schemas, digests, references and
  generation relationships refuse before append. Identical read_id retries
  return the original event with already-recorded; conflicting content refuses.
  Import validates introduced facts before publication in their supplying
  history, each against the state it was recorded in: a read against the reads
  before it; a projection against the read it names and, for checks and
  lifecycle, the link it was bound to, and for a link, any earlier declaration
  that admits its tuple; a delivery or disclosure source against the original
  read event it names. A link, declaration or conflict that merged history
  makes current later never refuses what was recorded without it. A local
  write is still judged against the reads usable now.
  Independent replicas can supply different events for the same read.
  Replay coalesces identical documents and retains their event IDs as aliases.
  Conflicting documents mark every member of that read_id as conflict; invalid
  documents carry the validation reason in invalid. Both remain readable and
  are excluded from source selection and generation dependencies. Projections
  naming unusable observations carry unresolved_binding and cannot establish
  head matches, applicable checks or lifecycle. Forge diagnostics in show and
  status identify observation conflicts, invalid documents and unresolved
  projections. Stored facts are retained without making replay an admission
  gate; historical bindings are read against their recorded links.
  arc-forge-receipt/1 carries command, change, event/observation/read IDs,
  generation IDs, coverage, outcome and, for each reference, the claim it
  supports and its availability (available, unavailable or corrupt).

  Observations carry adapter identity, a provider/host/repository/PR snapshot,
  nullable full observed head/base SHAs, adapter read_at, optional provider
  update/event coordinates, responses, items, generations and an optional
  previous_observation. Arc's created_at and collector identity are independent.
  Missing revisions on failed reads stay absent. Each response and item names
  its SHA-256, byte length and RetainedContentRef: provider, opaque handle,
  sha256, byte_length, media_type and reported capture (pinned or unpinned).

  A reference is a claim until ledger custody holds its bytes. Custody keeps
  content under `custody/sha256/<digest>` in the ledger directory
  (`<git-common-dir>/arc/` unless configured otherwise), writes each file
  atomically, and re-verifies the recorded length and sha256 on every read. It
  never follows a symlink: a `custody/` or `custody/sha256` directory that is
  one is refused by every read and write, so nothing is created, replaced or
  read outside the ledger, and a symlink in place of a digest's file reads as
  corrupt and is replaced rather than followed. A
  reference reads as available; unavailable, when no bytes are held; or
  corrupt, when held bytes fail verification. Absent bytes are never empty
  content, and missing content degrades only the generations and responses
  that reference it, never a retained fact or judgment. A provider's reported
  capture and arc's verification are separate facts. Arc never resolves a
  provider handle: bytes arrive as files an adapter materialized or as an
  export's custody sidecar.

  `forge observe --custody-dir DIR` copies materialized files. For each
  digest the document references, DIR/sha256/<digest> (lowercase hex) is the
  adapter's file: a regular file inside DIR, directly or through a symlink that
  stays inside it, with exactly the reference's length and sha256. Every file
  is verified before the first event or custody file is written, and each is
  verified again as it is copied. A mismatch, a non-regular file, a symlink
  leaving DIR or a DIR without a sha256/ directory refuses. Files the document
  does not reference are never read, and a reference with no file stays
  unavailable. Repeating a read with --custody-dir publishes bytes that are
  missing or damaged. Receipts name each reference's claim: a response with
  its response_id, or an item with its item_kind, node_id and generation_id.
  Provider-observed commits (observed head and base, merge commits, link,
  checks and lifecycle heads) are held by retention refs when this
  repository has them, and `arc export` lists them as `observed-commit`.
  Custody travels beside an export (see export and import above). `arc doctor`
  advises `custody-content-mismatch` for a custody file whose bytes do not hash
  to its name, which arc reads as corrupt, and `custody-temp-orphaned` for a
  hidden temporary file an interrupted write left more than an hour ago. It
  removes and rewrites nothing, and neither finding fails the command. A
  custody directory that cannot be read, or is a symlink, is the problem
  `custody-unreadable`.

  coverage has overall and collections entries. Each has state (complete,
  truncated or failed), scope, optional frontier and failure_reason (required
  on failure). Complete describes a finite read. Absence and zero checks
  require complete coverage of their collection. A passed rollup requires
  positive count and complete checks coverage. Other collections may be
  truncated or failed independently. Item facts are comment,
  pull-request (state and merge_sha), checks (state and count), or absence
  (collection). Raw bodies belong to referenced content, not typed facts.

  A PR's identity is its provider, host, repository and PR coordinates. Each
  coordinate is the native identifier (repository_id, native_id) when the read
  reports one, otherwise the name or number. A coordinate reported by only one
  of two reads makes them different PRs, so typed and legacy links never share
  an identity. URLs, base and head repositories and refs, and names or numbers
  that a native identifier supersedes are snapshots; they may change without
  changing the PR. Retargeting a native PR keeps its item generations:
  unchanged bytes reuse a generation and edits name their predecessor. An
  item's identity adds its kind and node_id. A typed link exposes its
  observation's provider_identity; checks, lifecycle facts and the legacy
  ambiguity test compare it, so a replacement PR cannot inherit another's
  facts through a matching URL, number or ref.

  Every item names a generation_id; generations carry kind, node_id, sha256
  and optional predecessor. Unchanged bytes reuse that generation in a distinct
  read. Changed bytes name a recorded predecessor on the same provider item.
  Reversion creates another generation; concurrent predecessor branches remain
  visible. Arrival order establishes no provider causality. show/status JSON
  exposes every observation, including historical generations and digests.
  export/import preserves observation event documents.

  link/checks/pr-state accept --observation EVENT-OR-READ-ID and --json.
  Values come from that fact; explicit flags must agree. --detail is valid
  only on legacy check assertions; typed reads retain prose through references.
  checks and pr-state
  accept --link and require the current PR identity and raw observed head.
  link still requires a matching declaration and policy. Legacy flag forms
  render as unretained assertions with unknown acquisition time. Checks with
  no link have unknown PR identity. Observed heads never come from rewrites.

  Evidence origins stay apart. A local gate evaluation is a result arc itself
  ran at a tree (`verify`, `candidate verify`); the gates of integration and
  selection read evaluations. Hosted checks are a provider's rollup for one PR
  identity and observed head, read through the forge observations above.
  Received execution evidence is an external run's claim about its own
  commits, recorded on a candidate (see CANDIDATES), with its provenance and
  receipts, labelled external wherever it is shown. None stands in for
  another: a green hosted check or a received pass is no local evaluation, a
  local pass says nothing of a provider's checks, and a Git or forge identity
  is never an agent role.

  Arc records provider assertions through forge declare/link/checks/pr-state;
  it never contacts a provider. Link head, checks PR head, lifecycle PR head
  and merge SHA retain their provider-observed values. Views pair each
  observed value with its resolved local rewrite successor; a dropped
  revision has a null successor. Stored event fields are raw observations.

  show and status expose observed_head_match and resolved_head_match against
  the patchset's approval anchor, which follows signature-only and
  role-addition rewrites (`arc guide rewrites-evidence`). Any other content
  or trailer rewrite keeps the open patchset's approval anchor;
  a fresh snapshot binds the successor. A local rewrite correspondence does not supply
  provider evidence at its successor. checks_record preserves the rollup's
  observed_pr_head, resolved_pr_head and the link persisted when recorded.
  Checks persist whether a binding was recorded, including an absent link.
  Import order never supplies an explicit binding. Unbound older checks and
  lifecycle facts can use an earlier link from their recording repository
  only while the complete history contains one distinct link. A foreign
  link or ambiguous history leaves the legacy binding unknown.
  Without a recorded link its PR identity is unknown. Hosted checks apply
  only to that PR identity and observed head; checks_applicable also requires
  that observed head to equal the approved head. A different PR or head
  renders the rollup stale. Repeating an identical link retains its binding.

  pr-state binds to the current link's provider-observed head; --merge-sha
  is the provider-reported merge revision. Local lifecycle correspondence,
  hosted checks and local integration authorization are separate facts.
  Forge readiness requires an observed approved head, applicable passed or
  not-configured checks, and an observed open PR. Zero checks retains its
  not-configured caveat. Integration authorization uses local gates and
  judgments independently of forge readiness.

REVIEW COVERAGE AND DEBT
  For ordinary work the review of record is the lead's own read of the diff,
  recorded as the verdict — or, where the lead contributed to the patchset or
  policy refuses its approval, a declared debt naming what was read and what
  is still owed. Neither waits on a reviewer: record it, then integrate. An
  independent reviewer is asked for only where policy requires one, or when
  someone asks for one. A deeper pass is bought later, over a batch, on the
  operator's call; the inbox and catchup hold what it covers.

  Review coverage is measured against the final patchset, not participation:
  `arc check` warns when a reviewer's last look predates what is about to ship,
  or when nobody distinguishable from the author covers it. Warnings, never
  blockers — one reviewer is a legitimate way to ship.

  `arc status --json` carries the exact identities the independence check
  compares as `review_subject`: invoker, effective author, the effective
  contributor set, and whether a recorded set or author fallback supplies it. For a closed
  integrated change, it names the patchset recorded as shipped while
  `latest_patchset` remains the later history. `arc show` and `arc check` name
  them beside an approval rejection, and `arc catchup` repeats the subject
  beneath each open change and each integrated debt item.

  An event's effective author is its `--on-behalf-of` subject when set,
  otherwise its actor. A patchset's effective contributors are its recorded
  set when nonempty, otherwise its effective author alone.
  Contributor declarations follow the actor rules in `arc guide identity-basics`.

  `arc status` and `arc review <change>` print the rule as `review_options`:
  for ordinary scope, `declare_debt` then `request_review`, with
  `next_action` `declare_debt`; a required-review or undetermined-danger
  change lists `request_review` alone. The list is what arc advises, not what
  integration permits: `arc integrate --debt` still lands a required-review
  change, on the lead's own judgement and with its review owed. Reading the
  list writes nothing — the lead runs `arc debt` or `arc integrate --debt`
  itself. A current
  `changes-requested` or `comment-only` verdict is its own next action and
  offers no debt route: a waiver records a missing review, not a way past a
  refusal.

  Repairing a patchset's attribution happens only before any verdict:
  `snapshot --amend <ps> --contributors ...` replaces the whole set, and the
  refusal after a verdict or closure names the blocker. Run evidence affects
  attribution only when a patchset is recorded; it repairs no existing patchset.

  Independence is judged against the patchset a reviewer actually read, not
  against whatever is newest. Otherwise a later snapshot by somebody else
  would retroactively turn a self-review into an independent one.

  It is also a relation between declared identities. The ledger, never commit
  trailers, says who held which role. A patchset's recorded contributor set,
  when nonempty, is its authorship; otherwise its implementer is its
  effective author: its `--on-behalf-of` subject when one was named, else its
  recorder. Its planner is the plan's recorded planners, else an asserted
  `Planned-by`, else the brief's recorder; its orchestrator is the invoker
  recorded beside an `--on-behalf-of` subject, and its integrator the actor of
  `arc integrate`, never the subject that command was invoked on behalf of.
  Planning, orchestrating and integrating somebody else's work are not
  authorship. A clean `arc rebase` that arc performs itself with `git replay`,
  run in a repository without the project's configuration, whose ref arc
  reads, carries the matching patchset's authorship, or completed bound-run
  attribution established before replay under `arc guide change-integration`,
  retaining every contributor's coordinate and model even when context or
  hunk positions moved. Only that replay's result, applied in the
  same invocation, is the replay: a snapshot of any other head, and any replay
  that fell back to `git rebase` or stopped for a person to resolve, is
  ordinary work. Reflog text, hooks, merge drivers, and inherited
  `GIT_REFLOG_ACTION` are not evidence for this and do not enter it. The
  replaying session is the replayer, never the author. A record supplies an
  actor's coordinate only when it credits that actor as an author; a session
  that merely replays or records somebody else's work supplies none. An
  explicit `--contributors` or `--solo` decides the replayed patchset's
  contributor set; members the replay carries over keep their recorded
  coordinates. Ordinary snapshots use their explicit `--contributors`, `--solo`
  or `--on-behalf-of` declaration when supplied, including an environment
  subject. With none supplied, completed bound runs can establish authorship
  under the following rule.

  Snapshot examines commits since the previous patchset's mapped head, or
  the first patchset's resolved base. That boundary must remain an
  ancestor of the head. Maps require recorded proof of complete role
  appends (`arc guide rewrites-evidence`) or signature-only changes for every
  recreated commit, readable objects, unchanged `Implemented-by` sets and a
  recorder outside covering executor bindings. Other rewrites refuse if
  their old side names the boundary or an added commit, or their new side
  names either and they were recorded at or after a covering dispatch
  began.
  For each delegated commit, a local dispatch naming this
  change must have a nonconflicting local implementation binding and a local
  end.
  Its dispatch-to-end interval must include the commit's committer timestamp
  exactly as Git records it, in whole seconds; fractional event times are not
  rounded. The binding may be recorded after the end but not before dispatch.
  The dispatch's worktree must resolve to the checkout holding the change
  branch. Its valid agent `--on-behalf-of` coordinate must name the bound
  harness, and the commit's `Implemented-by` set must equal that coordinate
  alone. Trailers corroborate the ledger's dispatch; they supply no identity
  by themselves. Several covering dispatches must agree on the executor
  session and model, and one executor session must retain one coordinate
  throughout the commit range. Dispatches starting after a commit or ending
  before it are irrelevant to that commit; a nonconflicting reviewer binding
  supplies no implementation evidence.

  Each established executor contributes its bound `harness:session`, with
  its dispatch's represented coordinate and model retained as author evidence.
  Several executors are all recorded. Production names the first executor
  in commit order as implementer; every executor remains an author for
  independence and clearance. A commit under the invoker's own binding, or
  outside every interval with no other implementer's trailer, also includes
  the invoker as a contributor. Without any established executor, the
  recorder remains the author. A temporally relevant missing bind or end,
  conflicting bindings or covering executors, mismatching worktree, coordinate
  or trailers, an uncovered implementer's trailer, or a rewrite that fails
  the boundary and dispatch-time checks retains recorder attribution for the
  whole patchset. Snapshot prints one line naming that ambiguity and the explicit attribution flags to pass. Successful
  derivation prints the executors and supporting dispatch ids. A change with
  no dispatch retains ordinary recorder attribution without that diagnostic.
  An unchanged patchset keeps its attribution, including after a recorded
  signature-only rewrite or append of complete role entries. A plain `git
  rebase` inherits no authorship; when run evidence cannot attribute its
  result, the diagnostic names `arc rebase` as the mechanical replay remedy.
  These rules also govern `review --snapshot`, `done`, squash and ordinary rebase snapshots; mechanical
  replay and explicitly attributed delivery or candidate promotion retain
  their own attribution contracts. Live-claim declaration requirements apply.
  Arc cannot detect an unrecorded rewrite that preserves the boundary,
  committer times and corroborating trailers.

  An approval is refused when its effective author matches the ledger's
  authorship, or when arc assumed the reviewing identity from git config or the
  harness session, which is nobody's claim and so cannot be the second party.
  An assumed authoring identity refuses nothing on its own: a reviewer that
  declared a different name is independent of it, before the merge and in an
  audit after it alike. Two assumed identities are refused, since neither side
  claimed anything. Authorship is also a model identity: a read under the model,
  at any effort, of the ledger's implementer is a self-review. A read is under
  the model it records and, where its event records that the reader's session
  store observed another, the observed model too; either one matching makes
  it a self-review unless the dated tie-break election below applies, and
  the rejection names the observed model. Where
  self-approval is forbidden the gate rejects the approval as it rejects a
  contributor match, and `arc audit` refuses to record it;
  `--verdict changes-requested` is open to anyone. Model-match refusals compare
  strings after dropping effort; tie-break mixed-authorship checks also resolve
  aliases from the read's dated ladder when one is available. `Planned-by` and
  `Orchestrated-by` never refuse a read; an audit records which of those roles
  its auditor held as dirty context for routing, and computes no score.

  `[policy] tiebreak_reviewers` elects dated `model#effort` coordinates for
  mixed-model work. The read's recorded coordinate must exactly match an
  entry in force at its `created_at`, and any session-store observation must
  match that complete coordinate too. The ledger's authoring models must
  include at least one model the reader does not name; different efforts of
  the reader's own model alone never qualify. This waives only the model-match
  independence test. A contributor, actor or authoring-session match, an
  assumed reviewing identity, or a contradictory observation never qualifies.
  A represented reader's session is the one its subject coordinate names;
  otherwise the recorded session applies. A missing harness does not create
  another model. Under a ladder, aliases of one model count as one model.
  Every other approval, capability, authority and coverage bar still applies.

  Elections come from the target branch's committed policy layered with the
  operator policy, never the candidate's file. New audits of closed changes
  use the target's current list. Effort and RFC 3339 `since` are required;
  `since` is inclusive and `until` an optional exclusive end. Malformed entries
  and overlapping intervals for the same model (at any effort) refuse, naming
  the entry. Different models may be in force together. A successor model gets
  a new entry and the predecessor an `until`. The list is append-only in meaning: every read keeps its election
  or its absence when recorded; later date edits, additions and removals never
  re-judge it. Reads without a recorded election have no exception. Unreadable
  election policy grants none: reads that ordinary rules allow still record
  and warn. A model-match audit remains refused where self-approval is forbidden. Each election
  binds to its recorded destination; a different `--into` cannot borrow it. `show`
  and `status` identify an elected read as `tie-break reviewer model#effort
  (policy since ...)`. A model-match refusal on mixed work names
  `tiebreak_reviewers` as the operator remedy.

  Commit trailers are a projection arc writes from the ledger. A range whose
  trailers disagree with the ledger is reported. For commits with no ledger
  record, such as imported ones, trailers are declared, unverified evidence:
  alone they neither discharge nor refuse, and the read says so. A reviewer arc
  cannot place is reported as unknown attribution rather than counted as
  independent or as self-review, whether it cast a verdict or only filed
  findings.

  Which changes need one is declared in `[danger] paths` in
  `.arc/policy.toml` or the operator policy at
  `<git-common-dir>/arc/operator-policy.toml`. Both path lists apply, so a
  path declared dangerous by either file is dangerous. Integration reads
  `.arc/policy.toml` from the change's target branch, wherever it is run. A change touching a declared
  path needs a verdict from somebody other than its author; elsewhere a
  self-recorded verdict satisfies the gate. `forbid_self_approval` refuses a
  self-approval on those paths. `arc integrate --debt` still lands such a
  change and leaves its review owed. Declare no danger paths and the
  gate stays uniform. `arc begin --dangerous` raises a single change
  whatever it turns out to touch, and nothing lowers it afterwards — a project
  decides its own gate in advance, rather than a change deciding it under
  pressure to ship. `arc check` names the rule that fired, and `arc doctor`
  reports a declared literal that can never match — one that names nothing, or
  a directory, since declared paths are matched against changed files.
  Gate and policy declaration files can change these integration rules, so
  changes touching `.arc/gates.toml` or `.arc/policy.toml` need independent
  review when those paths are declared dangerous.

  Required booleans apply when either policy enables them, danger and safe
  declarations combine with danger taking priority, debt thresholds use the
  stricter value, and `per-actor` provenance applies when either file selects
  it. Project and operator gates combine by name: profiles combine and the
  shorter timeout applies when the command and environment probe agree. The
  same name with a different command or probe is a conflict: `arc doctor`
  names both declarations, while `arc
  check` and gate execution refuse it. `arc status`, `arc show`, and `arc policy
  show` name the file that declared each rule. The operator file is outside
  the work tree, so local policy does not add tracked files to a contribution.
  `arc doctor` diagnoses the invoking checkout's declarations and tracked
  files, including uncommitted local edits. The debt priority advisory also
  uses that checkout's thresholds because it summarizes repository-wide debt,
  not a particular change's readiness.
  `arc policy show` includes each gate's command, profiles, timeout, and
  environment probe.

  Clearance requirements and the dated registry: `arc guide debt-clearance`.

  Work sent to a repository you do not own is decided by its receiver.
  `arc external verdict <change> --verdict <v> --decided-by <who>
  --reference <ref> --revision <rev>` records that decision beside, never as,
  a verdict arc witnessed: an approval gates only the revision it names and
  never alone on a dangerous path. An external approval never supersedes a
  local refusal. A change request carries findings, and a
  rejection of the head closes the change. A `[contribution] history =
  "squash"|"preserve"` declaration makes `integrate` record the change ready to
  send instead of merging it, refusing merge commits and, under squash, more
  than one commit. The event and projected state retain the effective
  destination branch, evaluated revision and authorization. Reuse requires
  matching patchset, destination branch and revision, contribution history
  and authorization. `arc squash <change> -m <message>` makes that one commit as
  a new patchset with its own gates and verdict, attributed by `--contributors`
  or `--solo` as on `arc snapshot`. Over another actor's live claim an
  unattributed squash refuses before the branch moves, and the change stays
  locked from that check until the patchset is recorded. A failed squash commit, a
  changed commit tree, or tracked edits left by a hook restore the original
  head, index, and tracked files. Untracked files created by hooks are retained
  for inspection. Paths obstructing restoration are moved under
  `<git-common-dir>/arc/squash-recovery/`; the command prints the recovery
  directory before moving them.

  A debt is declared with `arc debt <change> --reason <why>`, or at the merge
  with `arc integrate <change> --debt "<why>"`. An equivalent declaration
  reuses the obligation by syncing its exact backing publication, whether a
  loose event file or an import batch, without copying an imported event.
  Equivalence compares extents at their signature-only or role-addition
  successors; durability confirmation syncs the unchanged backing publication.
  A debt already in force routes to
  integration: status says integrate beside the flagged waiver, check drops
  the review-queue advisory, and the inbox keeps one lead row — until the
  head moves past the covered patchset, which requires a fresh snapshot. The
  debt can stand in for an absent verdict or rescue a self-approval rejected
  by repository policy. It binds to the exact patchset head declared, so new
  work needs a new
  declaration — it does not excuse the rest of the change's life. A waiver
  follows the rewrites an approval follows: signature-only and role-addition
  ones (`arc guide rewrites-evidence`). Any other message or content rewrite
  leaves the waiver stale even when the patchset's mapped head matches the
  branch; a fresh snapshot and declaration are required for that head. A refused
  `arc integrate --debt` keeps the debt it declared; declaring it again with
  the same reason, kind, and declarer against the same patchset and coverage
  records nothing and prints the debt already in force.

  `arc integrate --debt ... --dry-run` plans the integration as it reads once
  the debt is declared, and writes no event. The plan says `would declare
  debt` with its kind, reason, bound patchset and, for `ineligible-review`, the
  named read, qualifiers and assessment basis; a debt the change already
  carries reads `debt already declared` with its event. A declaration the
  execution would refuse is refused by the plan with the same message and
  exit code: an unknown qualifier, no verdict to name, an eligible assessment,
  a read flag on another kind. The plan never invents an approval, and a
  changes-requested verdict on the exact head still refuses it. `--json`
  carries the debt as `debt` in `arc-integration-plan/1`, with
  `disposition` `would-declare` or `already-declared`; a plan that would
  declare names no debt event in `authorization`. Handed to `--expect-basis`,
  the event the execution itself writes is not a move; a debt another actor
  declared meanwhile, or an existing declaration the execution reuses, is.
  `--provisional --dry-run` plans the same way.

  A debt is a record, not a count. It carries what kind of deficit it is,
  what review the work did have and at what coordinates, and who planned and
  who implemented it:

    nothing-read             no verdict on any patchset of the change
    merge-resolution-unread  approved, then a resolution nobody read
    repair-unread            approved, then authored work nobody read
    replayed-unread          approved, then replayed by `arc land` onto a
                             base the approval did not read
    contributor-only         verdicts on the shipped patchset, all its own
    ineligible-review        a read happened without supplying clearance
    independent-review       a read by somebody independent, unsupplied

  arc derives the kind from the ledger; `--kind <k>` on `arc debt` and on
  `arc integrate --debt` declares one instead, and a declared kind wins. Only
  the caller can say a resolution was what went unread, because the ledger
  sees a repair and a merge resolution the same way.

  `replayed-unread` is derived, never declared, and an import refuses a
  change ledger declaring it. `arc land --debt` records it for a member
  when all three hold: an eligible approval read its exact original
  patchset, arc replayed that patchset without conflict, and the base it
  was recorded on differs from the composed parent it landed on.
  The record names the approval, the recorded base and the parent
  (`replayed` beside the debt). The edit was read and the context it landed
  in was not, so only an audit of the integrated revision discharges it; a
  verdict on the original patchset never does. A member no approval covers
  carries the kind the ledger gives instead. The kind is the weight,
  carried as a label rather than a number: `arc query --debt` works through
  the list above in order, then by age, and every summary row splits its
  count by kind rather than reporting one total.

  An `ineligible-review` declaration names a read that does not count as
  independent. Under assessed clearance a findings approval also derives this
  kind (`arc guide debt-clearance`).

  `arc integrate --debt <why> --kind ineligible-review` (and `arc debt`)
  records beside the debt:

    read         the verdict or audit event of that read: `--read <event>`,
                 else the latest verdict on the patchset the debt covers, else
                 once integrated its latest audit. Refused when there is none
    qualifiers   why it does not count, from the closed set same-model,
                 same-family, below-floor, floor-unknown, reviewer-unseated
                 (a reviewer holding no seated review tier), below-clearance
                 (a tier below the clearance floor)
    assessment   `assessed` when `--assessment <file>` supplied them,
                 `declared` when `--qualifier <q>` flags did
    route_version  `--route-version`, or the assessment's, as a coordinate

  The assessment file is `agent-routing assess --json` output, schema
  `agent-routing-assessment/1` or `agent-routing-assessment/2`. `/2` keeps
  `independence` with its `/1` meaning and adds `findings` (`eligible` or
  `ineligible`) and `clears` (true or false); arc stores both beside
  `independence`, and a `/1` file leaves them absent. arc keeps its
  `independence`, `qualifiers` and `route_version`, never a tier or a model
  string (an audit's binding to its auditor is the one comparison made,
  below), and validates shape only: qualifier names from the closed set, a
  one-token route version, and answers that agree. In `/1` qualifiers are
  present exactly when the independence is `ineligible`; in `/2` a file that
  clears is eligible on both answers, names no qualifier and names its
  reviewer's effort, and one that does not clear names a qualifier. An
  `eligible` assessment is
  refused on this kind, because there is no debt to record. A tier is
  routing's reading at the time it is read, so every read shows the record
  as its route version and basis say, never as current. Within the kind,
  `arc query --debt`, the inbox, catchup and the workspace backlog put a
  `below-floor` read first.

  Under assessed ladder policy, a clearing agent audit discharges it without
  an assessment. Other cases retain the assessment requirements below; the
  clearance rule and all independent bars are in `arc guide debt-clearance`.

  It clears with an audit that approves or requests changes:
  `arc audit --assessment <file>` with `independence: eligible`, by an
  auditor who is also independent of the
  contributors and whom the assessment describes. This eligible, bound
  assessment is required for a person's audit too. The assessment covers the
  work: the `authors` it lists answer, through their `identities`,
  to the model of every `Implemented-by` line in the integrated range, at
  that line's effort. An author entry without an `effort` covers its model at
  any effort; a line that names no effort is covered only by such an entry. An
  assessment that omits one is recorded with binding `authors-uncovered` and
  the omitted `model[#effort]` coordinates in `uncovered_authors`, and leaves
  the debt open;
  readers list them beside the obligation. A range read and found to have no
  `Implemented-by` line asks nothing of the authors; a range whose lines
  cannot be established (no list kept with the production record, and the
  range does not read) is not an empty one, and its audit is recorded with
  binding `authors-unread` and leaves the debt open. No other kind has this
  rule. The
  assessment describes the auditor when the auditor's model is among the
  reviewer's `identities` (every model string routing accepts for it) and,
  where both name an effort, the effort equals the reviewer's `effort`. The
  auditor's model is the one its session store observed wherever the store
  has one, else the one its audit event recorded; a declared model the store
  contradicts is recorded with the observed one beside it and binds nothing,
  for a hand-recorded read as for a receipt item, and a verdict's assessment
  binds the same way. arc keeps only the outcome on the audit's assessment: `bound`,
  `model-mismatch`, `effort-mismatch`, `model-unrecorded` (an audit by a
  person records no model), `identities-unstated` (the file lists none) or
  `authors-uncovered` (the authors it lists omit a coordinate of the range,
  named beside it) or `authors-unread` (the range's authors could not be read). An
  audit without an assessment, with an `ineligible` one, or
  whose binding is not `bound` is recorded, says why, and is listed beside the
  obligation, which stays open; a verdict recorded since clears it only with
  an eligible, bound assessment of its own (`arc review --assessment`).
  `arc query --debt`, `arc catchup`, `arc inbox`, the workspace backlog, the
  debt advisory, `arc show` and `arc explain` show the qualifiers, the read's
  reviewer and model as its verdict recorded them, whether the assessment
  was assessed or declared, the route version, and the later reads that
  left it open.

  Production names who set the contract, who implemented it, and the
  orchestrator and integrator when the ledger records them. The planner is the
  plan the work was opened from, else an asserted `Planned-by`, else the
  brief's recorder. The implementer is the identity of the patchset's recorded
  contributor set when nonempty, else its effective author; the
  orchestrator is the invoker recorded beside an `--on-behalf-of` subject, and
  the integrator is the actor of `arc integrate`.

  Production also names the `Implemented-by` values of the commits in the
  patchset's range, read when the debt is declared and kept verbatim, without
  repeats, as `trailer_implementers`: a projection kept for comparison, never
  the source of a role. A range whose commits carry none records an empty list,
  which readers show as "no Implemented-by trailers"; a record kept before
  the range was read, or whose range could not be read then, has no list and
  says nothing of it: the two read alike, and an audit reads the range again.
  `arc catchup`, the
  workspace backlog, `arc explain` and `arc show` show the list wherever it
  is empty or disagrees with the ledger's implementer.

  Coverage names each verdict's reviewer, the model string it was cast under
  kept whole, the effort that string's trailing `#suffix` names, and the
  routing version `arc review --route-version` or `arc audit --route-version`
  declared. Production names who set the contract, who implemented it, and
  the orchestrator and integrator the ledger records. Arc records coordinates
  and holds no opinion: no score, no roster to join them against, and no
  ordering between two models.

  A verdict can also be owed corroboration rather than absent. `arc review
  --verdict approved --provisional "<why>"` records one that gates like any
  other — independence and staleness are unchanged, because an unproven
  reviewer is still not the author — while saying it should not be relied on
  yet. Use it when the reviewer's judgment has not been validated: a model
  nobody has measured, a pass made under time pressure, a reviewer outside
  what they know. arc never infers this; deciding which reviewers are proven
  would be a routing opinion, and arc holds none. Without the flag nothing
  changes.

  A verdict says what it does to the verdicts already standing. `supersedes`,
  the default, replaces them; `corroborates` supports one without becoming a
  second authority, which is what discharging a provisional approval is. Two
  verdicts replacing the same earlier verdict fork the chain and leave the
  change contested — no verdict is authoritative until one supersedes them
  all, and `arc check` says so rather than reporting that nobody reviewed it.
  The same shape a contested finding has, for the same reason.

  EXTENTS. A debt and an audit may name an extent: what the owed review
  must cover, and what the auditor read. `--extent whole` names the whole
  subject; `--extent-paths <glob>...`, with `--extent-exclude <glob>...`,
  names the entries the globs match. The subject is the patchset the debt
  binds to, or once the change closed what it shipped, in the domain
  `--extent-domain` names: `diff` (the default), the change from its base
  to its head, or `tree`, the candidate tree at its head. What a member
  landed by replay shipped is the range it landed as, from the composed
  parent to the integrated revision: the commits its target holds, and the
  context a `replayed-unread` debt owes a read of. A debt declared on such
  a member and every audit of it resolve against that range, whatever the
  debt's kind; a stack member's landed range is its patchset's. Globs resolve before anything is
  written, to each matching path with its blob at the head and, in a diff,
  each deletion with its blob at the base; a glob matching nothing, or
  exclusions leaving nothing, is refused, and no glob is stored. The extent
  is kept as `domain`, `revision`, and `base` (diff) or `tree` (tree), with
  `scope.entries` and `scope.exclusions` when scoped. An event carrying one
  stamps store format 7.

  A read covers an obligation only at the same domain, the same base and
  the same revision, or a signature-only or role-addition successor of it
  (`arc guide rewrites-evidence`); then:

    whole read     covers the whole subject and any scope of it
    scoped read    covers a scoped obligation when it lists every one of
                   its entries at the same blob, deletions included, and
                   excludes none; never a whole one
    no extent      covers no obligation that names one; beside one that
                   names none, the rule without extents decides, shown as
                   `extent: not established`

  A tree read never covers a diff obligation, nor the reverse. An obligation
  naming no extent is covered by a whole read of what shipped, never by a
  scoped one. Scoped reads never combine: each is judged alone. `arc guide
  debt-recovery` says how coverage enters discharge.

  Debt and a provisional verdict are the same obligation at two distances:
  debt says no review happened, provisional says one happened and is not yet
  trusted. Corroboration is a second judgment, not one particular command: an
  independent approval of the same patchset discharges it before the merge,
  an audit after. Neither the reviewer being corroborated nor the change's
  own author can supply it.

  Coming back to owed work:

    arc inbox                       debt-owed bucket, including closed changes
    arc catchup                     the same, with each reason and review subject
    arc query --debt                change IDs alone, for scripting
    arc query --provisional         approvals still owed corroboration
                                    (query filters on flags alone; the acting
                                     identity never narrows a read, so --debt
                                     lists every harness's obligations)
    arc diff <change> --integrated  the exact range that landed, for an audit
    arc audit <change> --verdict <v>          record a post-integration review
    arc findings <change> --audit             what an audit raised

  A debt stands in one of three states, and every view that shows it reads
  the same one. `declared`: the change has not integrated, so nothing is owed
  yet and no review has answered it. `owed`: the change integrated and no
  eligible review has answered it. `discharged`: one has, and the debt names
  it. `arc show` and `arc explain` print the state in words (`Declared, not
  yet integrated`, `Owed`, `Owed (discharged)`); `arc status --json` and
  `arc chain --review --json` carry it as `debt_standing`, and every JSON
  view of the debt itself as `debt.standing`. `debt_outstanding`, the
  debt queues and the waiver keep their meaning: true, listed and binding
  only for `owed`, which is why a declared debt is absent from `arc inbox`,
  `arc query --debt` and the workspace backlog.

  An independent read of the revision that shipped, recorded after the debt
  was declared, that approves it or requests changes discharges it — whether
  it came from `arc review` before the merge or `arc audit` after. The debt
  records that no such read existed, not that one command must supply it. A
  `changes-requested` read is that review; its findings carry what remains.
  A `comment-only` read judged nothing: it is kept as a read and the debt
  stays owed. Clearance follows `arc guide debt-clearance`. Without assessed
  ladder policy, an ineligible assessment leaves debt owed; a read without
  assessed policy or an assessment is judged by actor and model authorship. `arc audit` prints `debt
  discharged` only when its own read discharged the debt, and otherwise what
  the debt still owes and why this read leaves it owed.
  A debt that carries no production record still has its shipped patchset's
  recorded authorship to compare against, so a legacy obligation discharges as
  it did before production summaries existed. An integrated closure that
  recorded no source patchset recovers it from the ledger: the patchset whose
  head is its integrated commit, else the latest one recorded before the
  closure. Only a range with no patchset at all has nothing to compare: its
  read is recorded, and declared trailers alone neither discharge nor refuse
  it. Outside assessed ladder clearance, ineligible-review additionally
  requires an audit that approves or requests changes with an eligible,
  bound assessment. Persons retain that requirement
  under ladder clearance; `arc guide debt-obligations` owns the binding rule.

  Under `review_clearance = "assessed"` (`arc guide debt-clearance`) only a
  clearing read discharges a debt of any kind. Every other read since the
  declaration is a findings read: its verdict and findings stand, the debt
  stays owed, and `arc status`, `arc show` and `arc check` list it beside the
  obligation as `debt.findings_reads`, with kind `findings`, its reason and
  remedy. Ladder reasons name family, floor, explicit lower effort, binding
  or missing evidence. Without a ladder, assessment-only reasons and the
  `agent-routing assess --purpose clearance --json` remedy apply. A
  `comment-only` read remains a read that judged nothing.

  Coverage is judged last: a read that every rule above lets discharge the
  debt discharges it only when its extent covers the obligation's (`arc
  guide debt-obligations`). Otherwise it is partial coverage: the debt stays
  owed, and `arc status`, `arc show` and `arc check` list the read beside the
  obligation as `debt.partial_coverage` (`debt_partial_coverage` in `arc
  check --json`), with kind `partial`, its extent, the reason
  (`read-extent-not-established`, `obligation-extent-not-established`,
  `domain-mismatch`, `base-mismatch`, `revision-mismatch`, `scoped-read`,
  `entries-excluded`, `entries-missing`) and the obligation paths it leaves
  unread. A discharged debt says how in `debt.discharge_extent`: `covered`,
  or `not-established` where neither side named an extent. A
  `changes-requested` read within its extent discharges as any read does;
  its findings carry what remains. A verdict records no extent, so only an
  audit discharges an obligation that names one.

  An audit is a separate event from the verdict that shipped, so attaching one
  never rewrites what shipped with what review. An approving audit must come
  from an identity the ledger does not credit with authorship — otherwise the
  obligation would discharge itself — though anyone may audit into
  `changes-requested`, since raising problems needs no independence. Planning
  and orchestrating are recorded as the auditor's held roles and never refuse;
  integrating somebody else's work is not contributing to it. An auditor is
  compared with the authors, planners and orchestrators under the model its
  audit records and, where its session store observed another, the observed
  model too: either matching refuses, warns, holds the role or leaves the
  debt owed, and the message names the model that matched.

  A review finding the change shipped unresolved, whether open, still-open,
  disputed, or contested, stays that way in what shipped. `arc resolve` on
  the integrated change records its later disposition, such as `resolved
  --commit <fix>`, `obsolete`, or `accepted-risk`, beside that state, and
  readers show both: how it shipped, and how it stood after integration;
  `explain` lists the disposition as later knowledge. A finding resolved,
  accepted-risk, or obsolete in the receiver's eligible disposition graph at
  integration takes none. Replay preserves recorded dispositions and
  references regardless of receiver trust; release permission is assessed in
  the receiver's projection.
  Such a disposition is not a verdict and discharges no debt.

  `arc findings --format sarif` reports every unresolved finding, review or
  audit. A finding leaves it only when its applicable history has a single
  resolved, accepted-risk, or obsolete tip; that history is the later
  dispositions when there are any, otherwise the finding's own.

  Where `forbid_self_approval` is off, an approving verdict from the identity
  that wrote the work is recorded rather than refused, and `arc review` and
  `arc audit` both name the match: who it was recorded as, which patchset that
  identity wrote, and whether arc assumed the identity rather than anyone
  declaring it. Such a
  verdict is a review that happened, not an independent one, and it leaves an
  independent-review debt owed. A read under a model the ledger credits as
  implementer, recorded or observed, leaves the debt owed at any effort,
  regardless of `forbid_self_approval`, unless the tie-break election in
  `arc guide debt-review` applies. Where the ledger records no model for the implementer,
  the read is judged by the actor, session and harness it records; commit
  trailers are declared text and never supply the missing model.
  That policy governs approval authority, not independent coverage.

REVIEW CLEARANCE
  Without `[policy] review_clearance = "assessed"`, identity, independence
  and the existing assessment-independence checks decide; an assessment's
  `clears` answer is retained without deciding clearance.

  Under assessed policy, integration answers to the destination's committed
  policy and authorizing ladder. That source is the target head while open,
  and the target revision recorded by closure once closed. A candidate's
  own ladder never supplies the judgment that approves it. The accepted
  registry is the recorded destination branch's current head. It judges
  audits and other reads recorded after integration, ordered by event id.
  If the change's shipped or landed range touches `.arc/review-ladder.toml`,
  the authorizing registry judges every read of that change, including its
  debt's audits after integration. It stays at closure's `target_before`.
  Under assessed clearance, an integrated change without that revision
  is not established and names `target_before`. An unread range
  cannot establish that the change left the ladder untouched. Candidate and
  dirty checkout files supply neither source. A foreign closure supplies no
  local ladder authority for other changes: the receiver judges their reads
  with its accepted registry at their basis times.

  Read-time debt projections (`show`, `status`, `catchup`, `query --debt`
  and the inbox) use assessed clearance when either the invoking checkout's
  policy or the applicable target registry requires it. A target registry
  under legacy policy cannot loosen an assessed checkout: the assessment-only
  rule applies there. Integration uses the destination's committed policy.

  With no applicable ladder file, an agent clears only with its recorded
  model and effort and a bound `agent-routing-assessment/2` whose `clears`
  is true. Missing models, missing assessments, `/1` files, false answers and
  failed bindings remain findings reads. This assessment-only rule does not
  require a ladder producer or an external process.

  With an applicable ladder, arc owns capability. At the read's basis time,
  it selects the latest version whose `effective_from` is at or before that
  instant. Each verdict and audit records `basis_time`, `basis_source` and
  the recorder's `basis_version` when its applicable ladder has one. Receipt
  items may declare `judged_at` as an RFC 3339 instant within the inclusive
  `issued_at`..ingestion window; an invalid or out-of-window declaration
  refuses that item, even if a matching reply exists.

  A receipt's basis is the earliest already-existing visible assistant reply
  in the reviewer's exact session that names the item's key and its verdict,
  within that same window. Arc streams the whole recording through tapes;
  a reply before the ordinary transcript tail remains evidence. Echo `<key>: <verdict>` on its own line, or return
  a JSON item (or receipt with `items`) containing that exact `key` and
  `verdict`. Operator prompts, reasoning, tool calls, a different item or
  verdict, and replies outside the window supply no basis. Without a match,
  an active session falls back immediately to `judged_at` (`declared`), else
  to event `created_at` (`recorded`). A matched reply is `session-reply`.
  Direct reads use `created_at` (`recorded`). Coordinate admission and dated
  tie-break election use recording time, independent of this judgment basis.

  Export and import preserve the recorded fields. A receiver selects its own
  applicable ladder version at the recorded basis time; the recorder's
  version remains provenance, not authority. A read without recorded basis
  time uses `created_at`, shown as `inferred`. The judgment reports the basis
  time, source and locally selected version. A gap
  names `ladder-gap`; a missing version, reviewer model, model identity,
  effort or author coordinate gives `not-established` and names the missing
  piece. Retired rows remain usable. Each model row's efforts list is its
  complete effort set; an effort outside it is unknown.

  Findings eligibility requires the reader rank to meet every agent author's
  floor. Reading the same canonical model at a lower named effort is never
  findings-eligible, even if the numbers meet the floor. Standard efforts
  order as none, minimal, low, medium, high, xhigh, max; different unknown
  effort labels on the same model leave their ordering not established.
  Model aliases resolve through the selected version. With no recorded
  effort, a reader uses its minimum row rank and an author its maximum row
  floor. `conservative_bounds` names those missing efforts and distinguishes
  conservative evidence from a known weak coordinate.

  A findings-eligible agent clears only when its family differs from every
  agent author's, unless the dated tie-break election in `arc guide
  debt-review` applies. A tie-break replaces only this family test: its rank
  must meet every agent author's floor, and same-model effort ordering still
  applies. An author set resolving entirely to the reader's canonical model
  cannot use the family exception. Otherwise a two-lab author set needs a third-lab reader or a person;
  several whole-change reads from the authors' labs do not combine. Both
  recorded and disagreeing observed reviewer models must satisfy the bar.
  Actor/self-review independence, observed-model assessment binding, receiver
  review authority and required extent coverage remain independent bars.
  No ladder answer grants an imported read authority or covers an unread
  extent. A reader matching an author identity does not clear.

  An optional assessment still has to bind the read, including its observed
  coordinate and range authors. Its capability answers do not decide the
  ladder judgment. Where `clears` disagrees in either direction, the ladder
  decides capability and `arc show` names `assessment-disagreement`. Binding
  failures still bar the read even when the capability answer is clearing.

  Persons are exempt from model-coordinate enforcement and capability floors;
  their identities must remain independent of the complete author set. A
  person's audit of ineligible-review answers the assessment requirement in
  `arc guide debt-obligations`. A clearing ladder agent needs no assessment to discharge that
  kind. No read before the debt declaration or without required coverage
  discharges it. An approval that only finds cannot gate integration;
  `arc integrate --debt` derives `ineligible-review` naming that read.
  Changes-requested findings, failed gates and other refusals remain binding.

  `[policy] review_clearance_since = "<RFC 3339>"` sets an inclusive recording
  cutoff. At or after it, new agent reads under assessed ladder policy must
  record a model with effort. Earlier reads may record incomplete coordinates
  and are judged from their evidence. Without the cutoff, coordinates are
  required from enablement onward. An assessed layer without a cutoff cannot
  be relaxed by another layer's later cutoff; otherwise the earliest declared
  cutoff wins. Person reads are exempt. Coordinate admission does not require
  the model or effort to be known: unknown coordinates remain not established
  rather than clearing. No ladder or no assessed policy adds no admission bar.

  `review_context.judgments`, keyed by read event, reports findings eligibility,
  capability clearance, reasons, missing pieces, basis and conservative bounds.
  State, status, review and check projections share the same clearance path.
  Debt is rejudged by the receiver's registry; a recorded integration's
  authorization remains its recorded fact. Assurance counts only clearing
  reads; its freshness assessment is separate from debt.

  LADDER REGISTRY. `arc review-ladder import <file>` validates
  `arc-review-ladder/1` JSON or TOML and writes TOML at
  `.arc/review-ladder.toml` in the current open change's checkout. It never
  redirects a write into the target or another change's checkout; a linked
  `.arc` directory refuses. A refused input leaves the previous file intact.
  The table has dated versions with unique ids, RFC 3339 `effective_from`
  strictly increasing by instant, a nonempty source, and either a nonempty
  model table or `gap = true`.
  Each model row's efforts list declares its complete effort set for that
  version.
  Gaps omit the model table. Each model names its canonical id, identities,
  family, superseded flag, and a nonempty list of unique effort rows with
  positive integer rank and floor. Identities, including canonical model ids,
  resolve to one model in each version. Every floor must be met by a rank in
  another family in that version. Corrections refuse as
  `corrections-unsupported`.

  A table with two families, in either encoding under the same keys:

    schema = "arc-review-ladder/1"
    [[version]]
    id = "v2026-10-07"
    effective_from = "2026-10-07T00:00:00Z"
    source = "Capability judgment"
    gap = false
    [[version.model]]
    model = "model-a"
    identities = ["model-a"]
    family = "lab-a"
    superseded = false
    efforts = [{ effort = "high", rank = 1, floor = 1 }]
    [[version.model]]
    model = "model-b"
    identities = ["model-b"]
    family = "lab-b"
    superseded = false
    efforts = [{ effort = "high", rank = 1, floor = 1 }]

  `arc review-ladder show [--json]` prints the accepted registry from the
  target branch's current committed head. `--change` selects a change's
  target, including after closure; an integrated change uses its recorded
  integration destination. Otherwise its branch identifies the current
  change, or the repository default identifies the target. A missing
  target, ambiguous change, invalid registry, or absent file refuses with a
  diagnostic. Candidate and dirty checkout files supply no accepted table.
  State, status, review and check JSON expose `review_context` only when
  an authorizing or accepted source has a registry file. A missing target
  alone adds no context. Its `authorizing` and `accepted` readings name the
  target, revision and version ids, or an `unavailable` diagnostic. The authorization reading shares policy's
  closure basis and explicit projected destination revision; the accepted
  reading follows the recorded integration destination's current head.
  Candidate and dirty checkout files do not supply either reading.

  `review_context.authors` is keyed by patchset. Each set is the union of
  its recorded contributors, else its effective author; the subject's
  production implementer; and every `Implemented-by` coordinate in its
  range. Production is included even outside the contributor declaration.
  `landed_authors` reads a replayed member's landed range, excluding earlier
  composed members. Each set names its `base`, `revision`, identities and
  deduplicated agent `models`. Persons remain identities without adding
  model coordinates. A missing or malformed agent model or unreadable range
  gives `status: not-established` and names the missing evidence. `recorded`
  means the coordinates are present; capability and clearance still require
  their own judgment. Carried replay coordinates retain their recorded models.

  Import reports the authorizing registry read at the destination revision
  that also governs policy. Table publication and inspection confer no
  clearance; CLEARANCE defines the review requirements. Repositories that
  govern ladder changes through independent review declare
  `.arc/review-ladder.toml` in `[danger] paths`.

REVIEW ASSURANCE
  Every integrated change carries a dated assurance record: the reads it
  received, each with its model string as recorded, its effort, whether it was
  a verdict or an audit, when it was recorded, and the route version that
  selected the reviewer where one was recorded. A clean independent approval
  is the zero-balance case of the same record, not an exit from it.

  The record is derived, never written. It is a read-time projection of the
  verdict and audit events the ledger already holds, so every past
  integration has one without migration, and no tier, family, price or score
  is ever stored.

    arc query --assurance           one row per integrated change: its reads
                                    and where its debt stands
    arc query --assurance --stale   ask routing now, and list every change
                                    not known fresh, stale work first
    --json                          `arc-assurance/1`

  Staleness is a question for routing at read time, never a stored fact. A
  clearance goes stale when `agent-routing frontier` seats an eligible
  reviewer stronger than every read the change received, re-read under the
  current route version:

  - The authors are the models of the shipped work's `Implemented-by` lines,
    read from the debt's production record, else from the shipped range, else
    the recorded patchset model. A change with several authors is stale when
    it is stale for any one of them. An `Implemented-by` entry appended after
    approval is part of the shipped range and is read as written.
  - A read counts only when routing assesses it eligible. A verdict counts
    when it approves the shipped patchset, an audit when it approves. An
    approval carried across a role-addition rewrite (`arc guide
    rewrites-evidence`) still approves that patchset. Under
    `review_clearance = "assessed"` (`arc guide debt-clearance`) a read counts
    only when it also clears; every read carries `read_kind` `clearing` or
    `findings`, and a findings read its `findings_reason` and `counts: false`.
    Under any policy a read counts only when its `coverage` is `whole`, a
    whole read of the shipped subject that also covers the debt's extent
    (`arc guide debt-obligations`), or `not-established`, an audit or verdict
    naming no extent beside a debt naming none. A `partial` read is listed
    and never refreshes the change. A change whose closure records a replayed
    landing (`arc guide change-landing`) lists every verdict with coverage
    `edit-only`: the verdict read the original patchset, never the range that
    landed, so it never counts; an audit reads that range. Declaring another
    debt does not alter which range landed or make those verdicts count.
    Standing compares tiers only, so a same-family read at the frontier tier
    still does not count: a change with no eligible read is `owed`, which its
    debt view already says, and is never `stale`.
  - A change is `fresh` when an eligible read stands at or above the
    frontier, `stale` when every eligible read stands below it, `owed` with no
    eligible read, and `unknown` when an answer is missing. `no-live-lines`
    is a stale change none of whose lines remain.
  - Absent is not zero. A read with no recorded model or effort, an author
    nobody recorded, a read routing refuses, a routing that is missing,
    failing or malformed, a floor routing has not established, an
    unreadable range and an unreadable policy each leave that standing or
    component `unknown` with a fixed reason. None is ever `fresh`, and one
    failure never hides another row. A change is stale only when every
    counting read was assessed, since one routing never saw may stand at the
    frontier.
  - Routing is asked once per distinct author and set of reads, not once per
    change. Arc reads its answer as `agent-routing-frontier/1`, and a refusal
    as `agent-routing-refusal/1`; a refused reviewer coordinate costs only
    the read it names, a refused author costs the change its standing. Its
    own error text and paths never reach the view.

  Routing requests and their output pipes share a 30-second deadline.
  Responses larger than 1 MiB or invalid UTF-8 are malformed; Arc prints
  only fixed failure reasons.

  With `--stale` each row shows four components, in this order, each
  displayed and none stored. A change's diff, its commits and the range its
  authors' `Implemented-by` values are read from are its shipped patchset's
  `base..head`, except for a change closed by a landing, which reads its own
  landed range (`arc guide change-landing`): for a member landed by replay,
  the commits it landed as, which the target holds, never the commits it was
  snapshotted as.

  - frontier gap: the frontier's tier minus the best eligible read's tier, as
    routing answered it now;
  - live fraction: the share of the lines the change's diff added that
    `git blame` still attributes to the change's own commits at HEAD, with a
    file moved since followed. The answer is cached under the ledger's
    `cache/assurance` directory, one entry per change recording the HEAD it was
    read at and the paths it read. A later HEAD reuses the entry when that HEAD
    descends from the recorded one and nothing between touched those paths, a
    move included; otherwise the change is blamed again and the entry
    rewritten in place, so the cache never outgrows the changes. A change whose range is not
    reachable from HEAD has an unknown fraction, not a zero;
  - exposure: whether the change's diff touches the declared `[danger]
    paths`, and how many of them;
  - age: since the newest read.

  Stale changes order by frontier gap, then live fraction, then exposure,
  then age: calendar age only orders changes whose other components are
  equal, and date alone creates no debt. Only live code accrues: a stale
  change whose lines were all rewritten or deleted has nothing to refresh and
  is listed as `no-live-lines`, never as stale.

  Staleness never gates anything. No readiness, integration or waiver path
  reads it; it is a queue to sweep with spare quota.

REVIEW AND REPAIR
  finding and review accept repeatable --forge-source OBS:GENERATION. Exact
  references must name a usable recorded observation and one of its generations;
  canonical observation event IDs and generation IDs remain immutable. Inline
  findings accept forge_sources as an array of OBS:GENERATION strings and
  inherit the review's sources. A local verdict retains the forge frontier
  present when recorded. Later observations expose drift without superseding
  or invalidating that verdict. A verdict without a recorded frontier has
  unknown drift. A received public review creates no local
  verdict; only an explicit local judgment does. Integration authority remains
  governed by local verdicts, findings and gates.
  Before a repair path is chosen, a finding carries the trigger and the wrong
  observable behavior, the violated contract and its source, the constraints
  a correction must preserve, and discriminating acceptance evidence in the
  existing finding or brief shape. Its probe must fail against the affected
  revision for the predicted reason; a structural finding carries the
  counterexample and the check that separates compliance.

  A stable, nontrivial finding goes to an executor, who owns the edit and may
  dispute the diagnosis, refuse ambiguous correctness, or show the probe
  passing before any fix. Where plausible fixes imply different contracts,
  name them and the observation that separates them.

  A bounded local repair needs governing instructions that allow it and a
  recorded reason its handoff would cost more; name the local scope and the
  handoff avoided, and let stricter instructions control.

  Record who authored a correction and who examined the behavior it produced:
  an author cannot independently assess their own correction, while a lead
  that did not author it can. Delegating the edit discharges no review; a
  further independent pass answers to policy, a request, or the cost of a
  wrong approval — an escalation, not the default.

  A round is bounded on purpose: its brief carries the findings that matter
  now, because the implementation's shape decides what the next review can
  see and a repair raises findings no earlier pass could name. What it leaves
  is recorded before it closes — as debt or a journal deferral.

  An executor arguing back is the point: a brief says a disputed finding is
  worth arguing, not complying with. The executor holds the code, and the
  best returns are a finding shown worse than stated, or a fix shown the
  weaker of two options — neither reachable by a reader who has not written
  the diff.

RECEIPTS
  A delegated reader that cannot write to the ledger returns its reading as
  an `arc-review-receipt/1` file. `arc receipt template --verdict <change>...`
  or `--audit <change>...` pins each item's subject and frontier before
  dispatch; the reader fills in its identity, verdict, findings,
  dispositions and, on an audit item, the `extent` it read, and copies each
  `key` and `subject` unchanged. `arc review
  --receipt` records verdict items and `arc audit --receipt` audit items, each
  as the read `--verdict` records: the reader is the represented subject, the
  invoker the writer, and the event names the receipt item and the digest
  and length of its file, whose bytes the ledger keeps in custody. An export's
  custody sidecar carries them as it carries other custody bytes, and `arc
  doctor` names a cited receipt the ledger does not hold. Independence,
  clearance and assessment binding judge an item exactly as they judge a
  hand-recorded read.

  A receipt-wide error refuses everything and writes nothing. An item error
  refuses that item alone, and the exit is 1 when any item was refused. An
  item is refused when its subject no longer matches, when a verdict, finding
  or disposition was recorded after the template, or when it contradicts
  itself; a contradiction is refused, never repaired by editing the report.
  A read that clears nothing is recorded and reports its bar. Dispositions
  publish inside the read's own event, which stamps store format 7. Rerunning
  a receipt prints the event of each identical item published by the same
  represented reviewer and model and writes nothing for it, so an interrupted
  batch resumes. Both the declared model and the binding model must match:
  the session store's observed model where present, otherwise the declared
  model. A retry with changed content, reviewer identity or either model is
  refused, naming the recorded and submitted coordinates. `arc receipt --help`
  holds the shape and the refusal codes.

CONTRIBUTION TRAILERS
    arc roles <change> --planned-by [<value>] --orchestrated-by [<value>]
                       Assert roles on an open change. Both flags repeat;
                       at least one assertion or withdrawal is required.
                       --withdraw <role>=<value> withdraws an exact assertion.
                       Values use the convention's agent or person form.
                       A flag with no value stands for the invoking
                       identity: a declared Name <email> actor's own form,
                       else harness:model from the resolved harness and
                       model, under a declared actor or one derived from the
                       harness session. A role assertion is no approval, so
                       a derived actor may make one. An identity assumed
                       from git config, or an unresolved harness or model,
                       refuses: omit the role or pass a real coordinate.
                       A value, typed or resolved, that carries a
                       placeholder or a session id refuses; withdrawal takes
                       any recorded value.
                       Assertions are explicit; event ancestry grants none.
                       begin's two role flags and brief --planned-by take the
                       same forms. Put the change first when a role flag has
                       no value, or attach an explicit coordinate with
                       --planned-by=<value> or --orchestrated-by=<value>.
                       A role value resolving to a repository change refuses.
                       begin records the assertions directly
                       after opening, brief when writing a contract.
    arc trailers <change> [--json]
                       Print Planned-by and Orchestrated-by assertions and
                       Implemented-by from recorded patchset contributors,
                       or the snapshot subject when none are listed, in
                       convention order. Duplicate lines print once.
                       The subject is --on-behalf-of when set, else the actor.
                       A subject that is a fresh harness:model[#effort] or
                       Name <email> supplies itself. Retained replay or
                       bound-run author coordinates take precedence over
                       session names. The recorder's harness and model apply
                       only to the recorder's own work, on a snapshot that declared its attribution: a declared
                       actor, --solo, --contributors or --on-behalf-of. A
                       recorder inferred from its harness session or Git
                       configuration is not credited, since who ran snapshot
                       is not proof of who wrote the diff.
                       No line carries a placeholder or a session id. What
                       cannot be attributed is omitted and named in a note
                       with the value, why (placeholder, session id,
                       unresolved, undeclared) and the remedy; no coordinate
                       is guessed. A note on a session-form contributor may
                       name the one model this change's own records by that
                       actor carry, as a suggestion that is never projected.
                       An asserted role that is no fresh coordinate becomes
                       a note naming arc roles --withdraw.
                       Review snapshots and squash use the same rule;
                       rewrites retain patchset attribution.
                       Review credit stays in the ledger: Reviewed-by is
                       never projected or backfilled into commits.
                       --json emits arc-trailers/1 with trailers and notes.
    check and integrate advise when a commit in target..head lacks an
    asserted line, under missing-contribution-trailers, naming arc rewrite
    trailers --from <rev> --append <line>; only an assertion that is a fresh
    coordinate is advised. They advise when a commit's role line carries a
    placeholder or a session id, or a person without an email, under
    placeholder-contribution-trailers, naming each commit and line and arc
    rewrite trailers --from <rev> --edits <file>, whose `arc-trailer-edits/1`
    lines drop or replace exactly those lines and leave the valid lines under
    the same key. Neither advice blocks
    integration or changes ready or the exit code; rewrite before
    integration. Appending complete role entries keeps the approval or
    declared debt, and the appended text is a declaration, not a verdict;
    any other message rewrite requires fresh snapshot and approval or
    declared debt (`arc guide rewrites-evidence`). Post-integration audits
    stay in the ledger.
    arc instructions git [--check <file>]
                       Print the portable contribution-trailer convention
                       `arc-contribution-trailers/2`: `Planned-by`,
                       `Implemented-by`, `Reviewed-by`, and `Orchestrated-by`
                       name material contributions in agent
                       `harness:model[#effort]` or human `Name <email>` form.
                       A placeholder or a session id is not a coordinate;
                       its line is omitted.
                       Agent harness, model and supplied effort segments
                       must be nonempty.
                       `--check <file>` reports malformed role values,
                       placeholder role values (a placeholder or session-id
                       segment, or a person without an email), and keys
                       outside the convention. It never rewrites a message,
                       and exits 1 when a role key carries a malformed or
                       placeholder value.

HISTORY REWRITES
    arc rewrite sign [--key <id>] [--from <rev>] [--include-shared]
                     [--dry-run] [--retag]
                       Recreate every commit from --from through the branch
                       head so one key signs them all, move the branch, arc's
                       refs and the local branches and tags that point into
                       the range, and record the map. The commit named by
                       --from is itself rewritten. On a tracked change branch
                       its default is the oldest
                       commit not signed by the key in target..head; outside
                       a change it searches the whole history. Commits
                       reachable from the target or another local or
                       remote-tracking branch are
                       refused, naming the commit and ref, including in
                       --dry-run. A remote-tracking ref is the change
                       branch's own work only when its fetch source is a
                       branch with the change branch's name. The source comes
                       from the `remote.<name>.fetch` refspecs of every
                       remote (exact or `*` globs, `+` or not, several lines,
                       negative `^` refspecs excluding a source), whatever
                       the tracking ref is called: a branch fetched onto the
                       change branch's name is that branch's. A ref no
                       refspec writes, from an older configuration or made by
                       hand, has no known source and stays protected, as does
                       one written from a source outside refs/heads/. Where
                       several refspecs write one ref, each must name the
                       change branch, including for a configured upstream
                       with a fetch-refspec alias. Upstream configuration
                       grants no exemption from the all-sources check.
                       Target refs remain
                       protected, a remote-tracking ref fetched from a target
                       branch among them. --include-shared
                       grants permission without changing the default range.
                       With a target, the default always stays in target..head;
                       an inclusive shared range requires --from. A refusal
                       advises --include-shared alone for a default range and
                       retains --from only for an explicitly selected range.
                       Its planned moves use that same range; an explicit
                       range reaching target history names the protected
                       target commits and refs as well as the moves.
                       Explicit --from includes merged ancestry beyond the
                       named commit's parents, including target history.
                       Every moved ref is named with its old and new value.
                       --dry-run checks map consistency and append policy,
                       prints the map, every ref it would move, and
                       the tags it would re-point or leave alone, and stops; --retag
                       recreates the annotated tags whose targets were
                       rewritten, signed like the commits. Run again to
                       finish one that was interrupted.
    arc rewrite trailers --from <rev> [--drop <key>] [--append <line>]
                         [--edits <file>] [--include-shared]
                       Edit the trailer block of every commit message from
                       --from through the branch head, and carry the result
                       through the same map, refs and record. --drop removes
                       every trailer with that key, matched without case;
                       --append adds a `Key: value` line the block does not
                       already carry; both repeat and apply to every commit
                       in the range. --edits <file> replaces or drops one
                       exact trailer line of one named commit (below); it
                       repeats, and at least one of the three is required.
                       Edits apply first, reading each commit's block as the
                       commit carries it, then --drop, then --append.
                       A role line, or an Assisted-by line read without its
                       trailing `(role)`, whose value is no fresh
                       coordinate (malformed, carrying a placeholder or a
                       session id, or a person without an email) refuses
                       before any commit is recreated, including in
                       --dry-run.
                       A model equal to the invoking session refuses even
                       when the session id is not UUID-shaped.
                       Agent harness, model and supplied effort segments
                       must be nonempty.
                       The commit named by --from is itself edited;
                       it is rewritten when its message or ancestry changes.
                       Recreating a commit reachable from the target or another
                       local or remote-tracking branch is refused, naming that commit and ref,
                       including in --dry-run. --include-shared permits an
                       intentional shared-history edit. Commits the edit leaves
                       untouched keep their IDs even inside a shared range.
                       The same own-remote and upstream exemptions apply;
                       permission never changes the range named by --from.
                       Every moved ref is named with its old and new value.
                       --key, --no-sign, --dry-run and --retag mean what they
                       mean above.
    arc history rewrite --map <file> --reason <why>
                       Record a rewrite performed elsewhere, from its commit
                       map.
    arc history resolve <rev>     Where a recorded revision ended up.
    arc history withdraw <event-id> --reason <why>
                       Withdraw one history-rewritten repository event. Its
                       map does not participate in resolution or readiness;
                       refs are not moved back and retention refs are kept.
                       Output names recorded ref moves and retention refs,
                       or says when that information is absent.
                       Other event types are refused. A withdrawal travels
                       with the map in full and delta exports, and importing
                       replicas honour it even when they already hold the
                       map. A delta can carry repository events with an empty
                       change suffix. A correct map can be recorded for the
                       same revisions. Stored events remain intact.
                       A falsification reference storing a rewritten failure
                       revision, or a receipt after integration naming the
                       integrated commit's successor, that only the withdrawn
                       map carried is set aside: the change stays readable,
                       `show` reports it under `Set aside`, `doctor` lists it
                       as the advice `set-aside-reference`, and it counts for
                       nothing. Re-record the actual mapping with
                       `arc history rewrite --map <file> --reason <why>` to
                       restore it without editing ledger events.

  Only the signature and the commit ids change: tree, parents, author,
  committer, dates, encoding, message and any other header the commit carries
  — `mergetag`, on a merge of a signed tag — travel through untouched, so a
  commit recreated without signing has the id it started with. The rewrite
  names each commit that carried such a header. Trees are identical either
  way, which is why tree-keyed gate evidence counts the same on both sides of
  a signing rewrite.

  An edits file is `arc-trailer-edits/1`, one edit per line, the fields
  separated by tabs:

    # arc-trailer-edits/1
    <full commit sha>  replace|drop  <exact old trailer line>  <new trailer line, empty for drop>

  Blank lines and `#` comments are skipped; a comment naming another
  `arc-trailer-edits` version refuses, and a drop may leave out its empty
  last field. A field beginning with a double quote is a JSON string:
  `\t` encodes a tab, `\"` a quote and `\\` a backslash; unquoted fields keep
  literal backslashes. For example, `"Planned-by:\tcodex:unknown"` names a
  line with a tab after its colon. Quoted fields contain no carriage return
  or newline; malformed quoted fields refuse with the file and line.
  An edit applies only to the named commit, which has to be in
  the --from range, and only to a trailer line of that commit's trailer block
  whose trimmed text equals the old line exactly. That line occurs once in the
  block, is not the head of a value continuing below it, and is named by no
  other edit. `replace` puts the new line where the old one was and `drop`
  removes it. A new line on `Planned-by`, `Implemented-by`, `Reviewed-by` or
  `Orchestrated-by` is a fresh coordinate, and on `Assisted-by` so is its
  value without a trailing `(role)`. Agent harness, model and supplied effort
  segments must be nonempty. Models equal to the invoking session
  refuse even when the id is not UUID-shaped. Any violation refuses before
  a commit is recreated, `--dry-run` included, naming the file and line. `--dry-run`
  lists each edit as `<short sha> <action> <old line> -> <new line>` with the
  map. A recreated commit keeps its tree, author, committer and both dates;
  `--key` signs it, and pins, retained commits and `--retag` tags move as for
  any rewrite.

  A trailer edit recreates a commit only when it reached that commit or its
  ancestry moved: a message the edit leaves alone, under commits it left
  alone, keeps the id it had and stays out of the map. So a run that names a
  key nothing carries and a line every commit already has moves nothing. The
  trailer block is the last paragraph when every line in it is a `Key: value`
  line or a continuation indented with whitespace, and never the subject; a
  message with no such paragraph grows one, separated by a blank line.
  Messages are edited as bytes, so one that is not UTF-8 comes through
  unchanged.

  An annotated tag names its commit through a tag object of its own, so
  moving the ref would not re-point it. Left alone it keeps naming a replaced
  commit, and `git describe` and the changelog projection have no release
  boundary on the branch until it is re-pointed; the rewrite says so for each
  one. `--retag` recreates them, carrying name, message, tagger and date. A
  tag signature covers the tag object, so none carries over: every recreated
  tag is signed afresh by the key the commits are signed with (`--key`, or
  Git's configured signing key), whether or not the original was signed, and
  `--no-sign` leaves them unsigned. `--dry-run` says which tags would be
  signed.

  A commit that any recorded verdict or audit read, that a patchset recorded
  as its head, that a forge fact records as the provider's own (a full
  object ID, from an observation, link, checks or lifecycle fact), that a
  candidate's received source, tested or report evidence names, that a
  disclosure selects as Git support, or that a checkpoint names as its
  revision or an open change's head is cited, and stays reachable after any
  rewrite or rebase arc performs or records. Each such commit a
  rewrite replaces is held by
  `refs/arc/retained/<change>/<subject>/<commit>`: the subject is the
  patchset the commit was the head of, the candidate, or the event that
  recorded a verdict, forge fact or disclosure on it, and a change holds a
  commit once. A checkpoint's
  commits are held in the `repository` scope under its event, when nothing
  else cites them. A
  `refs/arc/keep/<change>/<patchset>` pin the rewrite moves is held the same way, whether or not a verdict cites
  its commit. The pin follows the successor, which is what projections and
  signature-only approvals stand on; a retention ref holds the commit that
  was read and never follows a rewrite, so it never makes a replaced commit
  look like the head of anything. The refs are created in the transaction
  that moves the others, restored with them when publication fails, finished
  with them on resume, and named by the event's `retained` map; `--dry-run`
  lists them. `arc history rewrite --map` moves no refs: it holds each cited
  commit the map replaces that this repository still has, and names each it
  cannot. Old revisions may abbreviate cited SHA-1 or SHA-256 commits from
  seven characters up to their full width; retention holds the full commit.
  On an append error, retention refs created by the attempt are
  withdrawn only when publication is confirmed absent; published or uncertain
  events keep them, and the error names that state. `arc doctor` reports a
  retention ref whose object is gone as the problem `retained-object-missing`,
  and a cited commit no ref reaches, a checkpoint's among them, as the advice
  `unretained-commit`, naming the `git update-ref` that holds it.
  If Git cannot complete the reachability walk, the report retains collected
  findings and names the problem `retention-reachability-failed` with the error.

  `arc rebase` records no map and moves no pin, and a pin holds its
  patchset's head with every commit below it. Before replaying, by `git
  replay` or by `git rebase`, a rebase holds each cited commit of the range
  it replays that none of the change's pins reaches, under the same names,
  and prints each ref it creates. A rebase stopped on a conflict has
  already held them when it is finished by hand. These refs are named by no
  event; an import holds their commits again on the receiving side (`arc
  guide change-bundles`).

  Every ref moves in one Git transaction, so a rewrite leaves the branch and
  arc's evidence refs on one history or leaves them all alone. The map and
  the ref moves are written to `repository/rewrite-intent.json` before any of
  it is applied. Map consistency, successor validity, signature-only
  classification and append policy are checked under the repository-event
  lock before any ref moves. A contradictory map records no intent and moves
  no refs. The file is removed once the map is recorded and the checkout
  index is refreshed, so an interrupted rewrite is finished by running it
  again rather than repeated —
  repeating it would sign the same commits afresh and record a second
  successor for each. A ref moved by something else in the meantime stops the
  rewrite, named.
  If publication fails before an event exists, the attempt restores its ref
  moves in one transaction and retains its intent for retry with the same
  computed commits. A concurrent ref change can prevent that rollback;
  the error names the retained intent for recovery. An event already published,
  or one whose publication cannot be determined, keeps the applied refs and
  intent and reports that state for recovery rather than claiming nothing moved.
  On resume, refs already applied by a previous attempt stay applied when
  publication fails. Diagnostics distinguish those refs from moves restored
  by the current attempt and name the command that finishes the saved intent.

  Recorded revisions keep saying what they said; every derived reading follows
  them forward. `arc doctor` inspects the recorded revisions, a checkpoint's
  revision and recorded change heads among them, and reports
  `dangling-revision` for missing objects without a recorded rewrite, or
  `unresolved-revision` when a recorded successor is missing.
  Admitted rewrite proof can recover falsification identity without
  advancing those revisions. A recorded revision resolves on an exact match, or on an
  abbreviation of seven hex digits or more that exactly one recorded revision
  answers; an ambiguous one is refused naming the candidates. Rewrite records
  that contradict each other make the map unreadable, which every projection
  refuses rather than reading as no rewrite at all — `arc doctor` reports it
  as `invalid-rewrite-mapping`. A branch with commits of its own on top of the rewritten
  range shares no commit with the branch it was cut from — the rewrite names
  each one and the `git rebase --onto` that replays it, and until then no
  comparison between the two has an answer. An approval follows a recorded
  rewrite in two cases, judged per commit over every commit the rewrite
  recreated:

  - signature-only: the successor differs in nothing but its signature —
    same tree, author, committer, message and every other header, with
    parents the map translates — which is what `arc rewrite sign` produces;
  - role-addition: the successor's message is the old one with complete
    `Planned-by`, `Implemented-by`, `Reviewed-by` or `Orchestrated-by`
    entries (a line with a value and its continuation lines) appended to its
    final trailer block, or to a new block after one blank line when it had
    none; tree and every other header byte-identical, the signature aside,
    and parents reached only through rewrites of either kind. This is what
    `arc rewrite trailers --append` with role lines produces.

  Any other successor leaves the approval stale in `arc status` and `arc
  check`: a replaced, removed or reordered role entry, `Assisted-by` or any
  other key, a body edit, a changed committer or extra header, or a commit
  beneath the head whose content or message changed. Added role text is
  declared, not reread: it is no verdict, and the ledger's contributor,
  production and debt records stay what they recorded. Admitting a pending
  signing (`arc accept`) still requires signature-only. To re-approve after
  any other trailer edit, or a map naming different content, run `arc
  snapshot` and record a verdict on the fresh patchset, or use `arc review
  --snapshot --verdict approved` with the work's contributor attribution.
  Snapshot records a new patchset whenever
  the latest patchset's approved head differs from the current branch head,
  even when its mapped head matches. Earlier verdicts remain bound to their
  own patchsets and stay stale for the rewritten head; a snapshot or rewrite
  grants no approval. While the mapped and approved heads differ, status and
  check direct the caller to snapshot, and an implicit verdict on the latest
  patchset refuses before recording anything. An explicit --patchset judgment
  may record historical work, retaining that patchset's review head and its
  stale coverage. Whether a successor is signature-only or a role addition is
  judged when the map is recorded, and again by the receiver when imported
  (the sender's judgement is dropped), never when a change is read. Incoming
  rewrite events are judged together until receiver-derived parent carry is
  stable. Their approval carry matches dependency-ordered single-event imports,
  regardless of event-ID or bundle order; only held judgments and judgments
  established against local objects supply that carry. A cycle of changed
  judgment states refuses before any import events or refs are written:
  `rewrite judgments do not stabilize; nothing was imported`.

THE HISTORY MODEL
  Git is the accepted history and the object store; arc records what the
  work behind that history knew, tried, and was accepted on. Five things
  carry it, each defined as much by what it is not.

  A registration is content with a contract: a tree, the brief version it
  answers, its producers, and the registrations it continues or adopts. It
  opens no change and creates no patchset, and two registrations of one tree
  are two candidates that share storage and nothing else. See CANDIDATES.

  A selection is validation of a named choice, never a choice. The caller
  names the candidate, the destination, the target, and the evaluations it
  relies on; arc checks each ground and records the basis the choice rested
  on, or refuses and records nothing. Permission is not effect: promotion is
  a separate, recoverable transaction whose product is an ordinary patchset,
  reviewed, waived, and integrated as any other. No review belongs to a
  candidate.

  A relation is typed by how it was established. A record is written by a
  tool or by arc as the fact happens: a read, a gate run, a verdict. A claim
  is somebody's declaration, attributed and unchecked beyond the citations it
  names; a declaration is never a read. An inference is what arc derives
  from other records, labelled with the rule and source it came from. `arc
  explain` gives every row one standing — `recorded`, `declared`,
  `inferred`, `absent` (nothing records it), or `unavailable` (its source
  cannot be read now) — and nothing reads stronger than its source: a
  declaration never renders as recorded, an inference never as declared, and
  an absence is shown, never omitted. See CONTEXT.

  A retention root is a selection or a promotion. It reaches its candidate
  and what that candidate's parents and adoptions carry. arc never collects
  on its own: `arc candidate retire` deletes a pin only when no root reaches
  it, on the operator's command, and retirement deletes a pin, never an
  event. Reusing an evaluation supplies no pin root for its registration or
  ancestry. An otherwise-unrooted supplier may retire; its registration,
  evaluation and the basis's original references remain evidence, and the
  chosen registration pins the evaluated tree.

  A debt is a record of review that did not happen, carried with the
  coverage the work did have, its coordinates, and who produced it. A later
  independent read of the shipped revision that approves it or requests
  changes discharges it; a
  discharge never rewrites the basis the integration was accepted on. See REVIEW COVERAGE AND DEBT.

  `arc explain <change>` reads a change's history through these, `arc
  candidate` and `arc context` write them, and `arc stats --provenance`
  counts how often each provenance record was written where it could have
  been.

CANDIDATES
  To choose between answers to one brief: register each, `arc candidate verify`
  each, then `arc candidate select`, which validates your choice and promotes
  it. `arc candidate judge` records why another lost.

    arc candidate register --tree <tree-or-commit> --brief <change>[@<event>]
                           --producer <actor>... [--parent <id>]...
                           [--adopts <id>]... [--episode <claim>]... [--id <id>]
                           [--evidence-file <file> [--custody-dir <dir>]]
                       Record an alternative answer to one brief version and
                       pin its tree at `refs/arc/candidate/<id>`.
    arc candidate show <id> [--json]
    arc candidate list [--brief <change>] [--json]
    arc candidate judge <id> (--rejected | --superseded-by <id>) --reason <why>
    arc candidate retire <id>
    arc candidate verify <id> [--gate <gate>]
                       Run the required gates against the candidate's tree.
    arc candidate select --chosen <id> --into <change> --target <revision>
                         [--evaluation <event>]... --rationale <text|@file>
                       Validate a named choice and promote it.
    arc candidate promote <selection>
                       Promote a recorded selection, or finish or discard an
                       interrupted promotion.

  A registration is immutable content with a contract: a tree, the brief
  version it answers with a `sha256:` digest of that brief's body, the
  producers, the parent registrations it continues, the registrations it
  adopts, and the claims it ran under on the brief's change. Registering opens
  no change and creates no patchset. Two registrations of one tree are two
  candidates that share storage and nothing else. A parent answers the same
  brief version; content carried into another contract is adopted instead,
  and the adopter's producers include every producer along the adopted
  registration's parent chain. Every refusal names its rule and writes
  nothing.

  A judgement — rejected, or superseded by another registration — is its
  declarant's claim. It alters no registration and selects nothing.

  Received evidence is what an external execution reports of a candidate,
  recorded beside the registration with `register --evidence-file`. The
  document, `arc-received-evidence/1`, carries the execution's private run
  handle; the packet or brief reference it was handed; the source, tested and
  report heads, each a full commit with its tree; provenance entries;
  receipt references (RetainedContentRef: provider, opaque handle, sha256,
  byte_length, media_type, capture); commands with reported results (pass,
  fail, error or not-run, an optional exit code, an optional receipt);
  the environment; the head the results apply to; and omissions. The three
  heads are three claims: a report commit that is the source's child does not
  move the tested head, and the tested head need not descend from either.
  Arc records original commits as reported and resolves each through recorded
  rewrites beside them, as `resolved_commit`: itself, its successor, or null
  when a rewrite dropped it. Registration, and an import for each evidence
  event the bundle adds, require each held raw head object to be a commit with
  the claimed tree. Tags are not peeled; held tags, blobs and trees refuse as
  `evidence-head-type`, and a held commit with a different tree refuses as
  `evidence-tree-mismatch`; an import's refusal names the evidence event.
  Absent commits remain external claims. Evidence already held is never
  re-judged by an import: a commit that arrives later with another tree is a
  diagnostic on `candidate show` (`diagnostics`: code, role, commit, detail)
  and the `held-evidence-mismatch` advice of `arc doctor`, and blocks no
  import. Held originals have retention refs through rewrites and
  garbage collection, including after import. One commit named twice
  carries one tree. Omissions are explicit: provenance, receipts, commands and
  environment each appear in `omissions`, with a reason, exactly when empty,
  and `omissions` itself is required.

  Each provenance entry is one agent role (producer, collector or reviewer)
  with the observations of it: the native producer and the model, each with
  its source coordinates (a kind and a reference), the forge account, the
  review sender, the Git author and the Git committer. They are separate
  facts and none implies another: one forge account behind three roles is
  three entries, and account equality establishes no equality of roles,
  models or Git identities. A coordinate or an identity is a value or is left
  out: registration, and an import for each evidence event the bundle adds,
  refuse a placeholder (unknown, none, n/a, ...) anywhere in a provenance
  entry or the environment identity, and a session id as a model, effort or
  source coordinate. A candidate's `producers`, a promoted patchset's
  contributors and `--on-behalf-of` keep their own roles and are never
  derived from, or merged with, a provenance entry. `registered_by` and the
  evidence's `recorded_by` are the declarant of the write.

  Received evidence is the sender's claim, written once and never altered, and
  never an evaluation. It records no `candidate-verified` or
  `verification-recorded` event and eligibility for selection is unchanged:
  each required gate still needs an evaluation `candidate verify` ran at the
  shipped tree, whatever the evidence reports of any tree. `candidate show`
  and `list` render it as `received_evidence` with `origin: external` and
  `tested_tree_is_registered_tree`, which says only whether the tree the
  execution reports having tested is the registered one. Each receipt reads
  from custody as available, unavailable or corrupt, and a missing receipt
  leaves every recorded claim as it was. `--custody-dir DIR` copies each
  receipt's file from `DIR/sha256/<digest>` into custody after verifying its
  length and sha256, as `forge observe --custody-dir` does; a mismatch
  refuses and records nothing. `arc export` lists the commits and trees the
  evidence names that the exporter holds as `received-commit` and
  `received-tree`, and the receipts ride the custody sidecar; an exporter that
  never held a head leaves it as an external claim, subject to the receiver's
  held-object checks before any event, ref or custody publication.

  Registration records `requested_evidence_sha256` only when --evidence-file
  requests received evidence: the SHA256 of the typed document serialized as
  JSON with sorted object keys. The request is immutable and shares the
  registration event's publication boundary. An active registration with
  explicit --id and --evidence-file permits recovery only when its original
  request digest, tree, brief, producers, parents, adoptions, episodes and
  declarant match. A retry can finish missing evidence or publish supplied
  receipt bytes. Held evidence must match the requested document and declarant
  exactly. Each fact is recorded once; conflicting retries refuse. Registration
  without evidence establishes no request: --evidence-file cannot attach one
  later (`evidence-not-requested`). Older records without this optional intent
  remain readable but grant no registration retry. Already-held evidence
  without an intent reads as `request_binding: unbound`, never producer
  consent. A newly admitted attachment requires an original request digest
  and matching declarant; an unrequested attachment refuses before any import
  publication, naming its event. Registrations without attachments remain
  importable. An evidence publication failure names the durable
  registration event. Without a custody directory, absent receipt bytes remain
  unavailable and do not block registration.

  Retirement deletes a candidate's pin and records that it did, and only when
  no root reaches the candidate: a selection or a promotion, reaching what its
  candidate's parents and adoptions carry. The registration and its
  judgements stand. arc never retires a candidate on its own.

  An evaluation runs the required gates of the brief's change, as its target
  declares them, in a scratch checkout of the candidate's tree, removed
  afterwards. It records `candidate-verified` keyed by the tree, with the
  gate's command, timeout, and environment probe as consumed, the identity the
  probe yielded there, and the result.

  Selection is validation, never choice. The caller names the registration,
  the destination (the change whose brief it answers), the target head it
  decided against, and the evaluations it relies on; arc checks every ground
  and reports every failing one by code, recording nothing unless all hold.
  `[candidates] evaluation_reuse` must be declared, and the target must not
  have moved. Each required gate needs a named, passing evaluation at the
  shipped tree, the chosen registration's, under the declaration in force and
  in the environment observed now at that tree; under `never` only the chosen
  registration's evaluations count, under `matching-coordinates` any
  registration's whose tree, declaration, and environment match, even after
  that supplier retires. Reuse roots neither the supplier nor its ancestry;
  its registration and evaluation remain evidence and the chosen pin holds
  the evaluated tree. A supplier reached independently by a selection,
  promotion, parent or adoption still cannot retire. A retired chosen
  registration remains refused. Each
  `--must-read` of the registration's brief version needs a tool read on the
  registration or one along its parent chain, by an episode one of them cites,
  at the required version and covering the required extent. A declaration,
  the contract's own plan or opening artifact, a partial read, and a read of
  unknown coverage are each refused by name; reads through an adoption never
  count. An artifact requirement is met by digest equality with the text a
  tool returned, so a tool that decorates it, like a line-numbered `Read`,
  cannot meet one until its recorder digests the content itself.

  A permitted selection records `candidate-selected` with its basis: the
  chosen registration, the destination head and target it was validated at,
  the evaluation and read meeting each requirement, the reuse policy, the
  contributors (the producers along the parent chain; an adopted
  registration's producers only as the adopter's), the selector, and the
  rationale. A repair is a child registration, so whoever repairs is a
  contributor and never only the selector. No review belongs to a candidate:
  the promoted patchset is reviewed, waived, and integrated as any other.

  Permission is not effect. The promotion holds the destination's transition
  lock and the repository-events lock, never the target's, and in order:
  refuses `basis-moved`, recording nothing, when the destination head or the
  target differs from the basis; refuses a checkout of the destination with
  tracked modifications or untracked paths in the way; commits the shipped
  tree onto the destination head with the selector as committer and keeps it
  at `refs/arc/candidate-promotion/<candidate>/<selection>`; moves the branch
  from the head it read, so a concurrent move stops it there; updates the
  checkout; records a patchset carrying the selection's contributors and a
  candidate link; and records `candidate-promoted`. A basis is never reused:
  after a move, select again, and the new selection names the stranded one as
  superseded. `arc doctor` and `candidate show` report a promotion ref no
  `candidate-promoted` event records; `candidate promote` completes it when the
  branch points at its commit and deletes it otherwise, and says which.
  Siblings stay registered and unjudged until somebody judges them; `arc
  workspace report` flags a promoted brief with unjudged siblings, and `arc
  explain` lists them under rejected alternatives.

  Candidate events, received evidence, evaluations, selections, and promotions
  among them, are repository events, so every `arc export` carries them
  and `arc import` judges them with the ledger they join before writing
  anything. Every imported candidate event stamps store format 7, including
  an evaluation, selection, or promotion without its registration.
  An imported registration whose tree this object store lacks stays
  unpinned. `arc doctor` advises on a pin with no registration and on
  undeclared `[candidates] evaluation_reuse`, and reports a registration a
  root reaches whose pin is gone.

CONTEXT
  A brief cause forge:OBS:GENERATION binds a contract revision to exact recorded
  forge content. It conveys source context, never operator or review authority.
    arc context read --subject <change|candidate> --episode <claim>
                     (--record <tool-record-id> --path <path> --digest <sha256:…>
                      [--lines <from>-<to> | --whole] | --from-tapes <file.json>)
                     [--at <revision>]
    arc context declare --subject <change|candidate>
                        (--cites | --relies-on | --considers)
                        (--path <path> [--at <revision>] | --artifact <file>)
                        [--citation <tool-record-id>]
                        [--forge-origin CHANGE:OBSERVATION:GENERATION]
                        [--disclosure-file FILE [--custody-dir DIR]]
    arc context capture --record <tool-record-id> [--subject <…>]
                        (--pinned | --unpinned)

  A relation attaches context to a subject, a change or a candidate, and says
  how it is established; nothing weaker reads as something stronger.

  A read record is a tool's record that a call succeeded and returned bytes
  for a path and range, within an episode: a claim on the subject's change,
  or on a candidate's brief change. Its version is the `sha256:` of the
  returned bytes, not a blob. Its coverage is the range the tool recorded:
  whole, a line range, or unknown, and unknown never counts as whole. A
  failed call is never a read, and a tool record id is one read per subject.
  A read is a record of what a tool returned; it is not evidence the bytes
  were a file's.

  An inference is what arc derives, labelled with its source. With `--at`,
  when the returned bytes equal the path's blob over the recorded range at
  that revision, the blob is recorded as inferred, `content-matches-revision`;
  otherwise no blob is recorded and the read stands on its digest. A read of a
  file directly in a journal, hot or cold, also names that artifact with its
  body digest at recording, compared with the read's and never assumed equal:
  by file name in this project's journal, as `<journal-dir>::<file>` in
  another project's.

  `--from-tapes` takes the reads from a `tapes-events/14` document or the
  `tapes-session/19` `.json` of a `tapes export` bundle, one per tool call
  with a `read` member, skipping each call with no stable id, path, or digest,
  each that failed, and each record already held, with one printed line.
  Legacy `tapes-events/9` and `tapes-session/14` documents are also accepted.
  arc reads only that file; it opens no harness store and runs no program.
  tapes digests the text a tool returned, which arc records unnormalized, so
  a tool that decorates what it returns, like a line-numbered `Read`, never
  infers a blob.

  A declaration is its declarant's claim that the subject cites, relies on,
  or considers a path or a journal artifact. arc checks only that a
  `--citation` names a read on the same subject. A declaration is never a
  read and never satisfies one.

  A capture report is what the provider (`tapes capture <session> --json`)
  said about retaining a read's recording, attributed to its declarant; the
  latest for a record stands. It is not the recording. `arc explain` reads a
  read `at risk` unless its latest report is `pinned`.

  A change's relations are events on its ledger; a candidate's are repository
  events, so every bundle carries them, and `arc import` judges both with the
  relations they join. `arc explain` shows a change's reads under observed
  reads and its declarations under declared facts; `arc candidate show`
  lists a candidate's.

RULES THAT CHANGE WHAT YOU DO
  - A verdict binds to the exact approved patchset head. Any new commit makes
    the approval stale until a fresh verdict on a new snapshot.
  - The ledger gates; claims, stages, and lanes are advisory signals, never locks.
  - Integration warns about an active foreign claim unless the caller
    supplies the complete matching actor, harness, and session. An absent or
    empty coordinate cannot match a recorded owner. Integration remains lead-owned.
  - A lane's owner is a harness and a session together. Harnesses mint session
    strings independently, so neither half names an owner on its own.
  - Owned journal lifecycle events require complete identity. Claim renewal,
    progress, checkpoints, and release match the claim's actor, harness, and
    session; lane renewal and closure match its harness and session. A stale
    lane can be closed by another complete identity with outcome `expired`.
    Established identity strings retain their whitespace on writes and reads.
    Lane renewal checks existence and ownership before reading status input
    and again under the transition lock. Lane opening and closure share that
    lock, so the check and append observe one lane state.
  - A lane is occupancy of a topic; a claim on a journal artifact is occupancy
    of that file. Both render on a row and neither becomes the other: work with
    no artifact still takes a lane.
  - An artifact claim expires by its lease alone. A change's stages are
    budgeted and a stage over budget reads `stale`; an artifact's claim takes
    `arc stage <file>` progress in the same vocabulary but no stage carries a
    budget, so `expired` is when it becomes reclaimable, and `--takeover` is
    what displaces it.
  - A claim that is not yet reclaimable is displaced only by `--takeover
    --because <reason>`, and the reason is recorded on the displaced claim,
    printed wherever it is rendered, and verified by nobody. The evidence a
    holder is gone — `agent-status-absent`, `delegate-exit:<handle>` — is
    observed outside the ledger, and the record is what makes a lease cut
    short auditable afterwards. Without a reason the refusal stands, because
    a live lease and a dead one read alike from inside arc.
  - `arc journal consume` and `arc journal transition` refuse while any claim
    on the artifact is open, and take `--acknowledge-claim <id>` for each.
    Those claims then end with `ended_by` naming the event and no owner
    outcome: how somebody's work stopped is theirs to say. `arc begin
    --from-journal` closes the invoker's own claim as `promoted` citing the
    change it opened.
  - Give every concurrent writer its own branch and worktree. Integration and
    shared refs belong to the lead alone. The stash is one of those refs:
    every worktree of a repository shares `refs/stash`, so a concurrent
    writer never runs `git stash` — another worktree's pop applies it there.
    It commits work in progress and amends it, or copies files aside.
  - Worktrees of a repository share one ledger, so a write can find a lock
    another worktree's command holds. Ledger, journal and operator policy
    publication locks are waited for up to 15 seconds, and a wait past one
    second prints a stderr line naming the lock. A lock still held after that
    refuses the command, naming the lock and asking for a retry. An integration
    target lock, and the replica-state lock an integration holds around it,
    wait up to 30 seconds. A verification whose gates ran but whose
    evidence could not be recorded prints each such result as `not recorded:`
    and exits non-zero; that result is not evidence until a rerun records it.
  - An executor's first act is `arc config --check-writable`; a nonzero exit
    means stop, not work around. Only writability decides that exit: the
    `commit` line says whether a commit can be made at all, while the
    `signing` line reports whether a signed one can be and never gates the
    exit, because whoever lands the work signs it. A project whose
    `commit.gpgsign` is on still needs signing working somewhere before its
    commits can land. It releases its claim whenever it stops, for any
    reason, so a live claim always means live work.
  - An executor that hangs never reaches its own release. Before leaving a
    delegated run unattended, arm `arc watch <change> --until stalled`; silence
    is unknown, not healthy. `stalled` is a stage clock, not an activity
    check: it holds once a live claim has sat in one stage longer than that
    stage's budget, counted from its last `arc stage`, or from the claim while
    it is still at `launch`. Only `arc stage`, re-reporting the current stage
    included, or a snapshot under the claim restarts the clock; output, logs,
    and claim renewals do not, so an executor that works without reporting a
    stage reads as stalled exactly like a hung one. The reached line names the
    stage, its age, and its budget. `arc rescue <change> --take` recovers it,
    and `arc rescue <change> --transcript` reads the exact claimed session's
    latest operator turns through the linked tapes library, taking the newest
    4 MiB of a recording file as its window. It names the reader and the read
    bound, or the cause and reason when lookup or reading cannot finish; the
    reason is one bounded line, as an identity lookup's is.
  - `arc watch <file.md> --until stalled` arms the wait over an artifact
    claim, whose stages carry no budget, so it stalls when its lease runs out,
    and `arc rescue <file.md> [--take]` reports where the work stopped and
    takes it over. An artifact answers only `stalled`; the rest of the
    vocabulary asks about patchsets and verdicts.
  - `arc watch <change> --until` accepts `snapshot`, `stalled`, `reviewed`,
    `approved`, `gates-green`, `ready`, `blocked`, `brief-recorded`,
    `integrated`, and `closed`. `approved` returns on the latest approving
    verdict, including a provisional approval and its recorded reason;
    `gates-green` checks every required gate at the current head; `blocked`
    and `brief-recorded` name the events that recorded those facts.
    --timeout bounds Git probes and their pipe cleanup. At the deadline,
    the probe process group is terminated and unfinished pipe workers are
    detached.
  - Gate evidence binds to a tree, not to a commit. A change sitting behind
    its target merges to content neither branch committed, and evidence at the
    head says nothing about it: `check` refuses with `merged-tree-unevaluated`
    until `arc verify --against <target>` runs the required gates on that
    merge, in a scratch checkout it removes afterwards. The result is spent
    the moment the target moves, because that is a different merge. A head
    already on the target tip merges to the tree it already has and needs
    nothing new: a rebase that moves the base without touching the diff lands
    on the tree that was evaluated, and the evidence carries over to it. The
    gate line reads `inherited from <revision>` wherever the run that answered
    was against another commit holding that tree, and `verify`, `snapshot
    --verify`, `done` and an integration queue reuse that run instead of
    repeating it, recording `verification-reused`, unless `--rerun` is
    given. Reuse claims content alone. A gate whose command reads commit
    messages or history declares `binds = "commit"`, which no other
    declaration of that gate withdraws: only a run at the
    head's own commit, or at the merge of that commit, answers for it, and a
    recorded rewrite never carries its evidence to the successor;
    `publication --skip-green` reuses its pass only at the target revision
    it ran on. An arc that predates the key ignores it and treats the gate
    as tree-bound. A rewrite that changed only messages, such as an appended
    role entry, keeps every other gate's pass and leaves that one to run
    again. A single `integrate` runs no
    gate; it checks that the merge it made carries the tree that was
    evaluated, and undoes it otherwise. A queue runs the ones with no answer
    at that tree, because it moves the target itself and every member behind
    the one it just landed is now evaluating a different merge.
    The rule names no integration shape: under `ff-only`, where no merge
    ships, a change behind its target still lists `merged-tree-unevaluated`
    beside `needs-rebase` in `check --json`, and `check` exits 11 for the
    rebase before it would exit 14.
  - At unchanged content, an explicit snapshot subject (`--on-behalf-of`,
    including `ARC_ON_BEHALF_OF`) that differs from the latest patchset's
    effective author records a fresh patchset. An equal or omitted subject
    preserves the existing patchset and its attribution; omission never
    reverts a represented subject to the invoker. The output names the
    effective subject. Earlier verdicts stay bound to their own patchsets;
    gate evidence at an unchanged tree follows its ordinary reuse rules.
  - A snapshot records the context that framed the work. Each journal link
    names an artifact by filename with the body digest read at record time,
    and a `via` saying where it came from. With no `--journal-ref`, a
    snapshot (`snapshot`, `done`, `rebase`, and the ones `squash` and `review
    --snapshot` take) links the artifact the change was opened from (`via: begin`,
    whose digest `begin --from-journal` also records on the change) and the
    plan its brief names (`via: brief`), one link per file; a file named by
    both is linked once as `begin`. A default that no longer resolves in the
    journal or its cold archive is left out with one stderr line naming the
    file and its source, and the snapshot still succeeds. Any `--journal-ref`
    replaces the defaults: only the flagged artifacts are linked, `via:
    flag`, and a name that resolves to no artifact refuses the snapshot.
    `--thread <scheme:id>` records an opaque external thread reference. A
    rerun at an unchanged head that supplies neither flag keeps the
    patchset's existing links. `arc show`, `arc log`, and `arc status
    --json` render them, bundles carry them, and `arc journal inventory`
    names the patchsets that cite an artifact. Arc records identifiers only,
    never transcript text: retaining a recording is not arc's promise, and a
    remote recording is read on the machine that holds it.
  - A journal artifact named for promotion or framing (`begin
    --from-journal`, `brief --plan-ref`, `snapshot --journal-ref`) is a
    filename in this project's journal, or `<journal-dir>::<file>` for one
    another project's journal holds, `<journal-dir>` being the absolute path
    that project's `arc journal dir` prints; the file is read hot, then cold.
    A directory that is not a journal, or a file it does not hold, is refused
    naming both. These references and a `brief --must-read` artifact refuse a
    path by naming the reference that resolves it, and a filename this
    project's journal does not hold by naming each other known journal that
    holds it as `<journal-dir>::<file>`, or that form when none does; when
    the known journals cannot be listed the refusal says why instead. The
    change records the reference as given, with digests read from the owning
    journal. A cross-project `begin --from-journal` or `brief
    --plan-ref` appends a `promoted` event to the owning journal naming this
    repository id, change, and checkout, and a non-plan source is consumed
    there as superseded by that change. The owning journal's `inventory`,
    `open`, and workspace views list the promotion with its `repository_id`,
    its status read from the promoting ledger: `unknown` when that ledger
    cannot be read, which never counts as closed. Only the owning project
    consumes, archives, unarchives, transitions, appends to, amends, or
    reattributes an artifact. A qualified reference to one is refused naming
    the owning journal, and so is a bare filename neither this project's
    journal nor its cold archive holds but another known journal's hot or
    cold storage does: the refusal names each such journal and its project,
    where the write runs with the bare filename.
  - A gate may declare an environment probe: a command whose output
    identifies the environment the gate's evidence applies to. `verify` runs
    the probe beside the gate and records its identity on the evidence. The
    identity is a digest of the probe's stdout, and only a successful run
    that prints something yields one; a failed, empty, or overrunning probe
    yields none, so two environments in which it fails do not share one.
    Readiness runs the probe the target's declarations select against the
    content the evidence is looked up at: the merged tree when the change is
    behind its target, else the head's. It runs in the invoking checkout when
    that holds the content clean, else in the change's worktree when that
    does, so a change editing a probe's script, or adding a gate with the
    script its probe runs, is judged by its own copy. With neither, a report
    that only observes, such as `status`, `check` or `integrate --dry-run`,
    runs the probe nowhere and counts no receipt: the gate reports
    `needs_evaluation`. `integrate` runs it in a checkout made for the
    content, without build preparation, and removes that before merging or
    refusing. Readiness counts
    the evidence only when the two identities agree; a receipt from another
    environment is reported inapplicable, naming both identities, and a
    receipt with no identity is reported unknown. A probe that yields no
    identity leaves every receipt for the gate not-green. Evidence that
    records no environment identity satisfies only gates that declare no
    probe, and a gate that declares none takes evidence from any
    environment. A probe is bounded by the gate's declared timeout, or
    thirty seconds when the gate declares none, and its process group is
    stopped before arc uses its answer, so nothing a probe starts outlives
    it. The newest run at the
    evaluated tree under the declared gate and applicable environment
    decides; other runs cannot hide it. An attested run happened where arc
    observes nothing, so `--attest` takes the identity with
    `--environment <IDENTITY>`.
  - A hostname arc could not read is recorded as `unknown`, a host fact
    on verification and candidate-evaluation events.
  - Executed verification and candidate gates retain combined stdout and
    stderr under the producing ledger's private `gate-output/` directory.
    Files have mode 0600 and the directory 0700. Each capture keeps its first
    16 MiB; publication retains 16 completed files, expiring the oldest
    captures, while active captures are separate. Events carry
    `output_artifact`: the producing store, relative path, retained SHA-256
    and byte length, full observed stream SHA-256 and byte length, and
    explicit truncation or capture errors. Capturing a truncated prefix does
    not prove the absence of diagnostics in the omitted bytes. A capture
    error does not change the command's observed result or prevent its tail
    from being recorded. Status carries the metadata even for a passing gate;
    ordinary output retains only a short tail. Attestations carry no capture.
    An output reader unfinished at its deadline supplies no artifact: its
    diagnostics have unknown capture coverage, never an invented empty stream.
    These files are local diagnostics: exports contain their metadata alone,
    and a path naming an expired or another store's file is unavailable. The
    verification event binds the capture to its command, revision, tree,
    environment and result; retention never changes those evidence facts.
  - A gate that passed is not evidence that it could have failed. Watch it
    fail first, then record the pass with `--falsified-by <failing-event>
    --predicted "<why it should fail>"`; the gate line then reads
    `discriminating` instead of `undiscriminated`. Advisory: it changes no
    result and no exit code, and only the declaration makes a row
    `discriminating`. Separately, a pass that follows a failure of the same
    gate (or command, when unnamed) on the change records the newest such
    failure as `falsification_inferred`, declared or not: an inference,
    labelled as one, that decides nothing. Acceptance-probe evidence neither
    records one nor serves as the failure one names.
    The reference stores the failure event's original revision; projections
    follow active recorded rewrites. A stored rewritten reference is accepted
    only when an active rewrite chain carries that same failure revision to
    the exact stored successor. Unrelated revisions, including ones sharing
    an eventual successor, are refused. Withdrawn maps supply no proof: a
    reference only a withdrawn map carried is set aside and reported, and
    re-recording the actual map with `arc history rewrite` recovers it.
    Verification replays its candidate event before appending or pinning
    its tree, refusing evidence that would make the change unreadable.
    A passing gate row is `discriminating` when any passing evidence for that
    gate at the counted tree (or revision when the tree is unresolved) names
    a falsification. A later pass without one does not retract it.
  - The journal lives outside the repo, so worktrees stay clean. Cross-session
    context goes there, never into tracked files. A sandbox that cannot reach
    it is not a reason to leave the record in a transcript: every kind verb
    and `journal log` take `--spool`, and a write spools by itself when the
    journal cannot be written. It parks in `.arc/outbox/` and prints
    `spooled: <path>`; `arc journal spool --promote` files it later with the
    identity it was spooled with. Spooling requires only checkout write access;
    optional ownership bookkeeping grants cleanup permission, and an
    unrecorded owner keeps the checkout. `arc config --check-writable` says
    which of the two will happen before you write.
  - An artifact is named `<timestamp>-<topic>-<kind>.md` at second resolution,
    so two writes of one topic and kind in one second want one name. The second
    takes a numeric suffix — `-2`, `-3` — before the extension, and every write
    prints the name it got. A name counts as taken in cold storage as well as
    the live directory, so archiving the first never frees its name for a
    second that would then collide with it. The suffix names no part of the
    artifact: its timestamp, topic and kind read exactly as written.
  - Journal events replay in their events.jsonl append order, established by
    the journal append lock. Equal or reversed observation timestamps keep
    that order. Imported and spool-promoted events take their receiving
    append positions and retain their recorded timestamps. Question state,
    corrections, claims, and storage transitions use the same replay order;
    journal doctor checks question state in that order too. Concurrent body
    sections exchanged between replicas merge by timestamp and event ID.
  - A spool made inside a change's worktree is deleted with that worktree, so
    `snapshot` and `integrate` file it from the change's recorded worktree
    first and print what they promoted; one they cannot file is named and left
    intact rather than blocking the command. `arc catchup` lists every spool
    still waiting in an open change's worktree.
  - `arc run dispatch` names exactly one subject: `--change <id>`, `--fork
    <slug>`, `--range <base>..<head>`, or `--journal <file>`. A round is its
    ordinal within that subject, so the loop of bounded rounds records on a
    fork, a bare commit range, or a journal artifact as readily as on a ledger
    change. A journal subject resolves as `begin --from-journal` resolves it
    and is recorded as given, with the digest of the body read then; after a
    kind transition its `supersedes` chain leads from either name to the
    other, and `run list` names the newest.
  - `arc run list --json` emits an `arc-run-list/1` object with `schema` and
    `runs`, an array in most-recent-dispatch order. Each row carries its
    subject, round, binding and terminal outcome; journal subjects carry
    `journal` and, when their journal resolves, `journal_lineage`. An empty
    list carries `runs: []`.
  - `arc run bind <dispatch> --executor-harness <h> --executor-session <id>
    [--handle <handle>] [--forked-from <session>]` records which backend
    session executed a dispatch, at any time after it, its ending included.
    The global `--session` names
    the caller; the binding names the executor. A dispatch has one binding:
    the same values again record nothing and print the event already held,
    and different values refuse. An empty value, a placeholder, whitespace, a
    harness carrying `:`, and a session id given as the harness refuse. A
    follow-up or resume is its own dispatch with its own binding, so two
    dispatches bound to one executor session are two spans of one
    measurement scope. `run list` shows each binding, or `unbound`. Distinct
    complete tuples arriving by import remain a binding conflict, with each
    tuple visible in text and in `binding.conflicts` in JSON. Equal imported
    tuples are harmless duplicate observations. Rebinding a conflicted
    dispatch refuses; no tuple establishes an exclusive assignment.
    Binding identities carry no `/`, `\` or `~`; a refusal names the field
    and a fixed reason without echoing a private locator. A held locator
    refuses binding retries and run listings with a fixed diagnostic.
    Cost reads qualify the affected assignment and preserve readable session
    figures. Imports and exports refuse nonportable bindings while
    preserving ledger events.
    `--forked-from` names the session in the executor's harness whose
    history it began as a copy of, and follows the validation and retry
    rules of `--executor-session`. Fork origins participate in binding tuples.
  - A lead that spawns a native child, a Claude Agent-tool subagent or a
    Codex `thread_spawn` thread, records it by hand as a run: `arc run
    dispatch --change <id> --route <route> --worktree <path>` before it
    spawns the child, `arc run bind <dispatch> --executor-harness <h>
    --executor-session <child> --parent-session <parent> [--executor-role
    reviewer]` once the child's id is known, and `arc run end` when it
    returns. A Claude subagent is named by the agent id the Agent tool's
    result names; tapes reads its recording, held under the parent's, as
    `<parent>::<agent>`. A Codex child is named by its own thread id, and its
    recording must name the parent as `parent_thread_id`. The pinned reader
    must find exactly one recording naming both, or the binding refuses with
    a fixed reason that echoes neither: the parent does not resolve, the
    parent records no such child, the child's recording is not in the store,
    or it names another parent or none. A delegated-agents mirror row
    (`agentType` `delegated-agents:run`) carries no consumption of its own:
    binding one refuses and names the agent-delegate run it mirrors, with
    the dispatch that binds that run's handle when one does. A child is
    never named `<parent>::<agent>` in `--executor-session`.
    `--executor-role` is `implementer`, the default, or `reviewer`, on any
    binding. `run list` shows `parent=` and `role=`, and JSON rows carry
    `binding.parent_session` and `binding.role`. A resumed child is a further
    dispatch bound to the same child, so its runs are its spans. Binding
    still attributes by the ledger alone, never by timestamp proximity.
  - `arc cost show <change|journal-file>... [--json]` reports what the executor
    sessions bound to the selected runs, and the participants that act on the
    selected work, consumed, and records nothing. A
    change selects itself and the artifact it was begun from; an artifact
    selects its `supersedes` lineage and the changes begun from any name in
    it. Membership compares resolved journal and artifact identity, so bare,
    qualified and equivalent directory spellings select the same work.
    References retain their original spelling as provenance. References to
    names outside a readable artifact's lineage need no resolution.
    Unavailable membership qualifies selection and withholds only the work
    totals that depend on it; readable session figures remain available.
    Unreadable ledger records and published imports are reported as coverage
    gaps beside readable records. A query refuses only when neither its
    target metadata nor any relevant dispatch or session can be read.
    A citation selects
    nothing, and transitions and `begin
    --from-journal` add no consumption. Each session is measured through
    the pinned tapes reader, or from retained facts when its source no longer
    answers, and the report says which; a fresh read that disagrees with
    facts retained at the same frontier shows the conflict. A session
    whose read failed and that no retained facts measure is unmeasured with
    `usage read failed`, or with `usage read failed transiently; a later
    read may succeed` where tapes found its store busy or unavailable
    through every attempt it makes, so reading again may measure it. Direct
    totals hold
    each undivided session whose work the item holds, and the item's share of
    each divided session with a span on it; inclusive totals add the artifact
    a change was begun from, and the union counts each session once, a
    divided one by the selected items' shares. A run's
    figure is the segments that run alone covers. A harness-written dollar
    figure is a harness-calculated estimate with an unknown pricing basis;
    token counts are resource quantities by tapes' categories. A list-price
    estimate prices a figure's disjoint categories by a `tapes-price-table/1`
    or `/2` table: `--prices <file>`, else the file `TAPES_PRICES` names, else
    `$XDG_CONFIG_HOME/tapes/prices.toml` (`~/.config/tapes/prices.toml`). It
    uses the table rows of the one model the recording says served the
    figure, or each model's rows share by share, through tapes' pricing,
    where the recording splits it by several models, where it counts events
    on branches besides the active one, each priced by the model it names,
    or where its attribution is uncertain and the split leaves part of it to
    no model, a part no row prices; a model served at several efforts is one share,
    priced over every request it served, and a figure added up from a
    divided session's segments holds each segment's models. Each share is
    priced by its own recorded cache-write TTLs and keeps its own reasons. A `/2` row's terms set by the request, a
    long-context rate above a prompt size for the billing it applies to,
    time-of-day rates and a validity window, are priced request by request
    over the requests a fresh read kept, a Pi figure's on every branch its
    recording retains, under the billing the recording establishes; a
    subscription pays no long-context surcharge. A model the
    table lacks, a figure no model is known to have served, a figure one
    model served whole where its read leaves attribution uncertain, a
    category measured incompletely, a cache write with no recorded TTL, a
    term the figure's requests do not settle, billing the recording does not
    establish, and a request no row was in effect for each leave a known
    subtotal with a fixed reason, priced at the least it can cost where a
    rate is in doubt, with the most it can cost under `bound`: `whole
    figure` when those doubts are its only reasons, else `known_bound`. A
    total adds its members' least and most alike. Retained facts, a share
    of a request and a child's figure from its family hold no request one
    by one, so a term they could pay leaves such a subtotal. The estimate
    is priced when the view is shown, from fresh or retained facts alike,
    and is never retained. With no table it is absent with the
    reason `no price table is loaded`; an invalid default table is reported
    and leaves it absent, and a named or environment table that cannot be read
    refuses.

    A harness's native counters add only within that harness: a raw input sum across
    harnesses is refused and other native sums across harnesses are
    withheld. Tapes' disjoint categories (uncached input, cache read, cache
    write by TTL, output, reasoning as a subset of output, and their total)
    add across harnesses, each with its harness's convention, within one
    accounting basis. An unbound or conflicting dispatch withholds its work
    totals.
    A missing amount leaves a total absent beside its known subtotal, never
    zero. Measurement, assignment, and identity coverage are reported apart.
    Every quantity names its accounting basis. Incompatible harness or basis
    domains withhold the combined quantity and expose separate partitions.
    Completeness is per category and is tapes' typed answer for the figure
    measured, never a count of its prose notes. A counter or disjoint
    category tapes qualifies is a known subtotal, with one fixed reason per
    tapes reason: a missing counter or price, a written zero price, a
    stopped request, an unreadable record, a read window or counter reset,
    an unestablished scope, a request not attributed to a model, a
    category the counters do not determine, or a record carrying usage that
    no reading counted. A record carrying no usage, such as a harness's
    attachment note, qualifies nothing. A counter holding no amount is
    absent. A disjoint category carries the reasons tapes gives it, and the
    total those of its four categories. Only the reader's `--latest`
    selection warnings remain prose, counted as such. A recording that
    keeps several conversation paths, such as a rewound Pi session, counts
    each consumption event once across every retained branch, with the
    events, their per-counter coverage and how they are identified reported
    beside the figure. A recorded zero price beside nonzero or unknown usage
    is unpriced, never a measured zero, and pricing coverage is reported
    with the cost.
    Token arithmetic preserves integers and refuses overflow. Currency sums
    round to ten fractional places, preserving smaller nonzero values;
    currency bounds round outwards.
    A recording beginning before its session's first dispatch has
    pre-dispatch consumption: assignment is incomplete and work totals are
    withheld. An unknown opening boundary also withholds them. A session the
    ledger gives one span is measured whole. Spans come only from the
    session's own ledger records, never from proximity: an executor's span is
    each bound run, from its dispatch to its recorded `run end`, open to the
    read frontier while it has none; a participant's span on a work item runs
    from its first to its last ledger event there, both inclusive. Any event
    its own session records counts, so a reviewer opens its span with `arc
    review <change> --start` before it reads, as a claim its session records
    also does, and the span then runs to its verdict. An event a lead records
    on its behalf carries the lead's instant, not the session's, and opens
    no span of it. A session
    with several spans is cut at every span boundary into disjoint segments,
    read in one pass through tapes' span reader, so no request is counted
    twice. A segment one work item alone covers is that item's share. A
    segment several items cover, a segment none covers, and a request whose
    time the recording leaves unrecorded belong to no item: they are the
    session's unattributed part, with the fixed reasons `overlap`, `before
    first item`, `between items`, `after last item` and `request time
    unrecorded`. Conservation holds per counter within the segments'
    accounting basis: the item shares and the unattributed part add up to the
    session's whole figure in that basis. `conservation` reads `conserved`
    only when every counter and disjoint category compares: one the parts
    and the whole do not add up in is named under `differs` (`not-conserved`),
    and one either side leaves unknown is named under `uncompared`
    (`unestablished`), never read as agreeing. A Codex session's segments
    are read in the summed-requests basis. A recorded total no span divides,
    such as a Claude cost-state or an OpenCode session row, stays whole in its
    own basis on the session row while the shares take the request basis tapes
    reads spans in, and the unattributed part then names `recorded total not
    splittable`; every figure names its basis, and totals mix bases only as
    the partition rules allow. A session the reader refuses to divide (`span
    reading unsupported`), one with no measurement (`unmeasured`), a
    participant whose ledger records are not all readable (`ledger records are
    unreadable`), and retained facts whose segments are not the ones the
    ledger gives now (`retained spans do not match the ledger`), and a
    participant with several members one of which only events recorded on
    its behalf name (`recorded on its behalf`) stay
    undivided, outside every share with that reason. An undivided session
    that also acted on work the figure does not hold has an unknown share in
    it, so every figure it would join is withheld, naming the session. A
    divided session joins every figure by its items' shares, even when the
    figure holds all of its work, so its unattributed part is never charged
    to a selection; its consumption before its first span is unattributed and
    withholds no share, and an undivided session counted whole still
    withholds as above. `--allocate even`
    divides each segment several items cover evenly among them, tokens kept
    whole with the first items in order taking one token each of the
    remainder. `--allocate following` gives what remains unattributed
    between two items, after a participant's estimated parts, to the next
    item. Every allocated figure is labelled `allocated`, with its
    `allocation` method, beside the conserving one and never replaces it,
    and allocation is never the default.
    A participant's segments no item covers are estimated from its working
    stretches, read from the same transcript pass as its spans: its
    activity cut wherever the gap between two stamps exceeds `--idle`
    (default `45m`, as `tapes stats` reads active time). Within a stretch,
    the requests up to and including one of the participant's own events
    serve that event's item, those after the stretch's last event serve the
    same item, and a stretch with no event of the participant stays
    unattributed; a segment several items cover keeps to its evidence.
    Estimates never apply to an executor, whose run spans are exact. Each
    estimated part is labelled `estimated` with its basis
    (`working-stretch`) and idle gap, and never enters the measured figure:
    `sessions[].split.estimate` holds each item's estimated share, what stays
    unattributed, and whether the measured shares, the estimated shares and
    that remainder add up to the whole. Every figure a participant's
    estimate touches carries the figure with estimates beside the measured
    one, under `work[].estimated` and `union.estimated`, and the table adds
    a `total with estimates` row. A read past the request timeline's bound
    estimates nothing and names tapes' reason, as does a reader that keeps
    no timeline (`request timeline unavailable`). An estimate answers only
    the idle gap it was read at: retained facts estimated at another gap
    than `--idle` estimate nothing (`retained facts were estimated at
    another idle gap`), and their measured split stands.
    Lineage relatives are plain session ids. A non-id reference makes
    identity unknown with a fixed reason; arc does not parse a recording path
    to infer an id. Lineage is read from the whole recording, as usage is,
    so a large recording with no fork, copy or child evidence keeps its
    identity established. Lineage notes and failed reads make identity
    unknown, using fixed categories and note counts.
    Every direct, inclusive, and union figure groups recorded parent, child,
    and fork relations transitively, including a binding's declared
    `--forked-from`.
    An origin outside the figure contributes no consumption and does not
    make the figure's identity incomplete by itself. Binding relations are
    durable alongside retained usage facts, so source rotation preserves
    these groups. Unknown lineage may overlap any other session.
    Overlapping groups make identity incomplete and withhold exact
    totals; each group's consumption is bounded by its largest member and
    the sum of its members, a list price in doubt counting its least toward
    the first and its most toward the second. Disjoint group bounds add.
    Missing or incomplete measurements qualify the bounds as covering only
    known recorded amounts.
    Per-session figures remain available. The top-level identity status
    summarizes the figures. A participant is a session no dispatch binds
    whose ledger events act on the selected work; a session a dispatch binds
    is an executor and never a participant. One rule gives every session
    its roles: those its bindings name (an executor's `--executor-role`),
    and those the kinds of its events name. A snapshot or a verdict makes
    the session a reviewer, an integration, a debt or a closure an
    integrator, and a dispatch an orchestrator; any other kind names no
    role and makes no participant, and prose and trailers name none. A
    bound session's snapshot is the work its binding gave it and names no
    role, while its own verdicts, integrations and dispatches give it those
    roles beside its binding's. An event recorded `--on-behalf-of` a
    `harness:<id>` subject that names a session is the subject's event too:
    it gives the subject the kind's role and membership while the invoker
    keeps its own roles for the event it ran. A subject
    `harness:<id>:<model>[#effort]`, followed by a parenthesized label or
    not, names the session `<id>` and declares `<model>` as that session's
    model on the event. It counts as `--model` does, including for a verdict's
    or audit's independence check, and `--model` outranks it. Without either,
    the event's model is the one observed for session `<id>`, or none: neither
    `ARC_MODEL` nor the invoker's session speaks for a `harness:<id>` subject.
    The subject names a session
    when the tapes reader of its harness locates the id as one of that
    harness's recordings, each harness shaping its own ids (an OpenCode
    `ses_…`, a Pi id as its recording names it, a Claude or Codex UUID),
    when that harness's store cannot answer, when the ledger retains facts
    for it, or when the id is shaped as that harness shapes a session id (a
    UUID, a Claude cloud session's `cse_…`, an OpenCode `ses_…`) though
    nothing on this machine locates it:
    such a session is a participant without a figure, `not located on this
    machine`, so the figures it would join say they are incomplete. A
    subject naming a person, a label, a `harness:model`, or any other id no
    reader locates and no retained facts name, names no session. A participant is measured through the same
    reader as an executor, or from retained facts when its source no longer
    answers, and divided by its span on each work item. Its membership is
    every change in this ledger with an event of any kind that names it, as
    its session or its subject, read in one pass; `--footprint` selects by
    the same rule.
    Undivided, it counts whole where the item holds every member. Divided,
    its share joins each item's total, and a union takes the selected items'
    shares. It is listed in `participants` with its whole figure, roles,
    span count, the number of members, `counted` and `share`. A recording cannot show
    whether its session is still running, so the view asks `agent-status`
    once, bounded to two seconds (`ARC_AGENT_STATUS` names another program).
    A session it lists, in any state, is live: its figure is as of its read
    frontier and incomplete. A claude, codex or pi session absent from a
    snapshot its registry owner answered has ended: a fresh read of it is
    complete as of its final frontier. Every other answer is unknown and
    incomplete with a fixed reason: no program, a failure, no answer in
    time, unreadable output, a snapshot no registry owner answered, or a
    harness agent-status does not report on. Facts retained after the
    session ended stay complete unless it is listed again; facts retained
    while it was live or unknown stay incomplete even once it has ended,
    since it may have gone on past their frontier. Each participant row
    shows the answer under `liveness`. An executor's run ends are the
    stronger evidence: one whose every bound dispatch has a recorded `run
    end` has ended, and one with a dispatch that has none is still running
    and incomplete as of its frontier, naming the open dispatch. An
    agent-status listing makes an executor live either way. When unreadable
    ledger records could hold the missing end, only a listing qualifies it.
    An executor's facts retained before its last run ended stay incomplete;
    facts retained after it, or while agent-status answered ended, are
    final. arc works fully without agent-status. A session may
    also act in other repositories' ledgers, which this ledger cannot see.
    An unreadable ledger record leaves a participant's membership incomplete,
    so it is never counted as the work's alone. Acting events with no
    harness or session, or one that is not a plain id, are counted under
    `unattributed_events` and make assignment incomplete. A participant
    that is not measured, such as one the store cannot resolve or a subject
    not located on this machine, is still a session row and a participant
    without a figure: every total it would
    join is absent or withheld beside its known subtotal, never complete,
    measurement coverage lists it, and it is named outside assignment
    coverage with its event count and reason, which makes assignment
    incomplete. A bound executor whose read fails with nothing retained
    likewise leaves its totals without an amount. An unmeasured session has
    no accounting basis, so it qualifies every partition it could belong
    to. Another session with events on the selected work but no acting
    kind is named outside assignment coverage with its event count, never
    zero. A Claude session's recorded total is
    reconciled per counter with the requests its recording family
    identifies. The residual is the signed difference, never clamped, with
    tapes' status (`reconciled`, `conflict`, `discrepancy` or `withheld`) and
    fixed reasons; a conflict, a recorded total below its requests, qualifies
    the counter it understates, a withheld counter has no residual, and a
    family that cannot be read leaves only the residual unavailable.
    A native child a binding names is an executor row measured from its own
    recording by its own requests, with `parent` naming the session that
    spawned it, roles from its bindings' `--executor-role`, and its agent
    from the model its recording says served it, else its dispatches'
    routes. A Claude session whose recording names native children with
    consumption of their own is measured by its own requests, in the
    summed-requests basis, because its recorded total covers those children
    too; a session whose only figure is held in the recorded-total basis is
    unmeasured (`recorded total covers its native children`) rather than
    counted beside its children. A spawn a binding identified is a parent
    and a child with recordings of their own, so it groups no overlap,
    except that a Claude child's recording can hold a request its parent's
    holds too: unless the parent's family reconciliation is `conserved`, a
    measured parent and its bound child are one overlap group, and a total
    holding both is withheld between its largest member and their sum,
    with identity incomplete, never a complete double count. A
    bound child whose recording is gone is unmeasured (`child recording
    missing`). Each parent row carries `family`: `bound`, its bound child
    rows; `children`, every child its recording names, `bound` when any
    dispatch binds it, to this view's work or to work it does not select,
    with the recording `present` or `missing` and, for one no binding names,
    its figure, from
    tapes' family reconciliation for a Claude subagent and a fresh read for
    a child with its own session; and `unassigned`, those children's figure,
    listed and never in a total. An unassigned child without a figure
    leaves that figure absent beside its known subtotal, naming the child. A mirror row is never listed. A Claude
    parent's `family.reconciliation` says whether its own requests and each
    child's add up to the family's identified union per counter
    (`conserved`, `not-conserved` naming each counter under `differs`, as a
    request two recordings both hold leaves it, or `unestablished`), and
    carries the residual, the recorded total less that union: the parent's,
    no child's, and in no total.
  - `arc cost show` selects the union of its positional targets, change ids
    and journal artifacts mixed, and its range selectors, and the union
    counts each session once. `--tag <t>`, repeatable with every tag
    required, selects each change carrying it as `begin --tag` and `arc metadata
    --tag` record it. `--footprint <harness:id>` selects each change and
    journal item that session was bound to or dispatched, and each change
    with an event of any kind that names it, as its session or its
    `--on-behalf-of` subject: the work its membership holds, so a session
    counted whole in a change view is counted whole beside them. It shows
    its whole figure beside them. A native child is named by its
    measured key, `claude:<parent>::<agent>`, and selects the work its
    bindings name; a child key no binding records is refused. `--since` and `--until` select each
    change with a ledger event, or a run dispatched or ended for it, in the
    window; the window selects work items only, and a session is divided by
    its spans, never by the window's edges. `--all` selects every change in this ledger. Each selected
    target keeps its own selection rules; a change whose state cannot be
    read for a tag is a selection gap, and a range that selects nothing
    refuses.
    Text output is a table, one row per session: its agent
    (`harness:model#effort`, from the bound dispatches' routes when each
    names one coordinate, else the models its own events carry and the
    subjects recorded on its behalf declare, else the one
    model its recording says served it, effort unnamed when it served
    several; the harness alone when none
    establishes a single model; a coordinate that carries the row's harness
    as a prefix, such as a route `claude:claude-opus-5-5#medium`, names it
    once; one whose prefix names another harness tapes reads establishes
    none and is shown beside the agent with its source, as in
    `claude:claude-opus-5-5 (route codex:gpt-6.1-sol#high)`, and in
    `agent_conflict`), roles (impl, review, orch, integ), the shortest
    session id prefix of at least eight characters no other row
    shares, runs and spans, the disjoint token total, the money columns, and a
    status: `complete`, `subtotal`, `split ×N` (divided among N work items,
    its share in the total), `shared ×N` (undivided and outside the total),
    `live`, `unmeasured` or `withheld`. With a price table the money columns
    are the list-price estimate, then the harness-written estimate; without
    one the harness-written estimate alone, and a footer line says how to
    install a table. Footer rows hold the total, which is the union figure
    with its status and a row per accounting partition when it spans several;
    the `total with estimates` row when a participant's estimate touches it;
    with `--allocate`, the allocated total beside it; the `unattributed
    shared` row with everything of each session outside the total, its
    unattributed part and its shares of items the view does not select
    (`unselected item`), and one line per session naming its reasons; and a line counting the qualifications
    `--detail` explains, then per parent of native children a `children`
    line naming its bound rows, the children bound to unselected work, and
    the unassigned figure outside the total,
    and a `family` line with the reconciliation status and each residual
    counter that does not reconcile. An amount not established is shown as
    its known subtotal, as `least–most` for a list price in doubt, or as `≥`
    the least an overlapping figure is known to be, with its status
    beside it. `--detail` prints the per-category report instead. `--by
    agent|role|model|harness|change|session`, repeatable or comma-separated,
    groups the counted rows with one subtotal per group; a subtotal never
    adds incompatible accounting domains and shows their partitions as the
    total does. `change` gives each selected change its subtotal: a divided
    session joins each by its share of it, never by its membership, and a
    session several selected changes hold undivided forms one group naming
    them. A model groups under the id its recording gives, namespace and
    case kept (`z-ai/glm-5.3-flash`). `--json` carries the same rows, total, allocated total,
    unattributed figure, qualification count, groups, families and price
    table under `table`, each session's segments, shares, run figures and unattributed
    parts under `sessions[].split`, each item's allocated figures under
    `work[].allocated`, the price table under `prices`, and a footprint under
    `footprint`.
  - `arc cost retain <change|journal-file>` keeps, per bound or participating
    session, the facts a fresh read gives: reader and calculation versions, source
    frontier, digest and opening boundary, counters with their basis,
    coverage and category-specific gaps, the disjoint categories with their
    convention and gaps, the consumption events with their per-counter coverage and
    identity, the Claude residual, and any harness-written dollar figure
    with its pricing coverage, plus lineage relations, read failures
    as fixed categories, the one model the recording says served the
    session when it names one, whether the recording leaves its attribution
    uncertain, each model's disjoint categories, its efforts added together
    and its cache writes by TTL, where the figure is priced share by share,
    the segments a session with several spans was read in, each with
    its bounds, covering work items and runs, request count and facts, the
    remainder and the whole in the segments' basis, and the liveness
    agent-status answered then (`live`, `ended` or `unknown`). A list-price
    estimate is never retained. Retained facts carry no recording locators or raw
    errors. Display sanitizes older facts field by field, so one unreadable
    field leaves the rest of the measurement available; bundle import and export refuse
    nonportable identities, metadata, quantities or reason categories without
    changing recorded events.
    Unavailable journal membership and unreadable ledger peers are reported
    as coverage gaps; readable bound and participating sessions are still
    retained. A session
    whose source does not answer is reported and skipped independently.
    The source bytes stay in the harness store. Identical facts record
    nothing; retained facts are observations at their frontier and are never
    added to one another. A fresh read and retained facts at one frontier
    compare their segment figures when both cut the session alike, and a
    disagreement shows as a conflict.
  - `arc cost show --commit <rev> [--json]` answers for a commit, which has
    no cost of its own. Git resolves the revision, and the commit maps to the
    change that records it: a patchset head or the integrated commit decides
    first, and otherwise a commit inside a change's current patchset range
    belongs to that change. A change closed by a landing holds the commits of
    its landed range (`arc guide change-landing`) as its own; when it landed
    by replay, the commits it was snapshotted as are still its own, placed
    with the role `replayed`. A commit that several changes record is
    refused, naming them. The view embeds the change's `arc-cost/1` document
    unchanged, adds the commit, its position ("2 of 3 commits") and its
    sibling commits, and states that the change's figures are shared by
    those commits and that none is per commit. Each role trailer on the
    commit lists the sessions recorded for that role. `Implemented-by` lists
    the sessions bound as implementer to the change's runs whose route
    model, effort and a prefix naming the executor's harness dropped, equals
    the trailer's model. `Reviewed-by` lists
    the sessions bound as reviewer on the same terms, and the sessions,
    bound or not, whose events give them the reviewer role, and `Planned-by`
    and `Orchestrated-by` those whose events give them the orchestrator
    role, whose own model equals the trailer's: the models the session's
    own events, or a subject recorded on its behalf, declared in that role,
    else the one model its recording says served it, effort dropped, never
    the invoker's model on an event recorded on its behalf. Each session
    matched by its events names its `model_source` (`events` or
    `recording`). A session in that role of the trailer's harness that no
    source gives a model is listed in `unestablished` as unmatched, `no
    model of its own is established`, rather than matched by a guess. A
    matching session's figure is its whole figure, shared exactly as the
    change view shares it, with the change's share beside it when the view
    divides the session, and its `status` is the word the change's table
    gives its row (`complete`, `subtotal`, `split ×N`, `unmeasured`, ...);
    `measurement` keeps whether the reader's counters cover its recording.
    A trailer no recorded session answers, such as a
    person's or another model's, reads `no session recorded` when no
    session is unmatched, never zero. A commit no change records answers
    `no change records this commit`, shows no figure and succeeds. The lookup
    reads every change's state and follows the aggregate read rule: each
    change whose state cannot be read is named with its error on stderr,
    and the command still succeeds. Such a change might record the commit,
    so a lookup that skipped one never answers that no-match: it answers
    `no readable change records this commit` naming the unreadable changes,
    with `lookup.status` `unresolved`. A match found beside one is still
    shown, with `lookup.status` `may-not-be-unique` and a line saying so.
    `lookup.unreadable` lists the changes either way, and a lookup that read
    every change carries no `lookup`. `--json`
    emits `arc-cost-commit/1`, wrapping the embedded `arc-cost/1`. A price
    never enters a commit message: a figure is incomplete while a session
    runs, and no session's spend has a per-commit split.
  - `arc cost note <change|commit> [--all]` writes that commit view as a Git
    note on each commit of the change, under `refs/notes/arc-cost`, so
    `git log --notes=arc-cost` shows cost beside history. A note is a cache
    of one read, never a source: the ledger and its retained facts stay the
    truth. It opens with the time it was read and the reader revision, keeps
    every figure with its quantity kind, accounting basis, status and
    reasons, labels a dollar figure a harness-calculated estimate on the
    basis shown beside it, and says the figures are shared by the change's
    commits. It carries no commit subject or change title. Its text passes
    the portability check retained cost facts pass: a line with a path
    separator or a control character is refused, so no path, session link or
    raw error text reaches a note. Rerunning replaces each note with a newer
    read, so a figure written while a session was still running completes on
    refresh. `--all` refreshes every commit that already holds an arc-cost
    note, wherever a newer read changes its text beyond the opening time,
    and leaves the others as they are; a noted commit no change records
    keeps its note. A commit target or a noted commit is located as `cost
    show --commit` locates it: a target no readable change records is
    refused naming the unreadable changes, a noted one is left as it is with
    them named, and a note written from a match found beside an unreadable
    change carries the same `may not be unique` line. The notes ref is
    local: arc never configures it for push, never adds it to a refspec and
    never pushes it. The command records no ledger event.
  - `arc cost evidence <change|journal-file>... | --all` writes the evidence
    `cost show` divides its sessions by to standard output, as a
    `tapes-attribution-evidence/1` file (UTF-8 JSON Lines) that `tapes usage
    SESSION --evidence FILE` reads, or with `--output-dir DIR` into a
    directory of such files that `tapes usage SESSION --evidence DIR` reads,
    so tapes gives arc's figure wherever arc's ledger exists. It takes `cost
    show`'s selection, read the same way.
    Export `--all` for exact figures: a participant's division rests on every
    piece of work it took part in, not only the selected one. The first line
    is the header, with producer `arc/<version>`. A session `cost show` gives
    a figure writes the evidence that decides it, every record at tier
    `bound`: an executor the ledger gives one run, one `binding` with no
    span, the whole session; an executor with several runs, one `binding` per
    run with a `span` from the dispatch to its recorded `run end`, or to the
    header's `written_at` while the run has none, and the run's role; a
    participant divided by its own events, one `event` per event with the
    role its kind gives it; a participant acting on one piece of work, one
    `binding` with no span, with its role when its events give it exactly
    one. A participant the ledger does not divide, because an event recorded
    on its behalf names an item, or because a ledger record could not be
    read, writes nothing, as does a session `cost show` names without a
    figure; standard error counts both. Each record's work is `label`, the
    change id or journal artifact as `cost show` names it; `commits`, the
    commits the change made itself, in full, left out for a change that has
    none and for an artifact; and `repository`, the `origin` remote URL
    without its user information, query or fragment, left out when there is
    none or it names a place on this machine. A change's commits are those
    `cost note` finds for it less the current-patchset commits of every change
    below it in a stack, and always include its own integration commit. A
    change is below another when it is open or integrated and its current
    patchset head, different from the other's, lies in the other's patchset
    range; a change closed without integration is below nothing, and changes
    with one head exclude nothing from each other. For a change closed by a
    landing, its current-patchset commits, head and range here are those of
    its landed range. Where a dispatch records
    both the session that made it and the executor it bound, one `lineage`
    record per distinct pair is written at the earliest such dispatch, with
    the binding's handle as `label` and the work as `subject`; a native child
    is linked by its recording and writes none. The `lineage` lines come
    first, by time, parent and child, then each session's bindings and events,
    by session, time and work. Two exports of an unchanged ledger differ only
    in `written_at`, which also ends each span of a run still open. Evidence
    tapes would stop reading before its end is refused and nothing is
    written. Standard output carries one file, of at most 10,000 records
    after the header; a longer export is refused naming `--output-dir`.
    `--output-dir DIR` takes an absent or empty directory, checked before the
    ledger is read, writes `evidence-0001.jsonl`, `evidence-0002.jsonl`, ...
    into it in name order, and leaves standard output empty: each file is the
    header, one `written_at` for all, followed by at most 10,000 records,
    which keep the export's order and are never split. Evidence that would
    need more than the 1,000 files tapes reads from a directory, or that holds
    a line longer than 256 KiB, which no file holds whole, is refused either
    way. The files are written beside DIR and moved into place together, so a
    failure leaves DIR as it was. Standard error summarizes the sessions
    written, the records by kind, the commits that still appear under several
    labels, the sessions without a figure, the sessions the ledger does not
    divide, the runs still open, and with `--output-dir` the files written.
    Every session is measured as `cost show` measures it, and the command
    records no ledger event.
  - A bounded round records what it deliberately left, so the next round
    inherits a list instead of a memory: `arc run end <dispatch> --outcome
    <o> --reviewed-head <sha> --raised-json <path|-> --deferred-json <path|->`.
    A deferral requires a `why` and gets a `def-<ulid>` when it names no id; a
    later round on the same subject discharges it with `--collects <id>`.
    `arc inbox` and `arc catchup` carry the ones still open.
  - arc holds no routing opinion. It records the --actor and --harness
    it is given and resolves an undeclared model from the acting session.
    Who to delegate to is the caller's policy, not arc's.
  - A delegated session binds its boundary with `ARC_ROLE` or `--role`. An
    implementer may not record a verdict, an external verdict, `resolve`,
    `hold`, `release-hold`, `audit`, `debt`, `close`, or `integrate`. A
    reviewer may not run `debt`, `close`, or `integrate`. Recording a brief
    is a lead's. An unset role is `lead`, with full access.

EXIT CODES
  forge observe exits 0 for admitted complete, truncated or failed reads and
  identical retries, whatever the availability of their content. Invalid
  input, conflicting read IDs or a refused --custody-dir exit 1; CLI usage
  errors exit 2. forge link policy/tuple/host mismatches exit 10.

  `arc check` is the integration preflight and its code names the blocker.
  `integrate` and `rebase` refuse in the same vocabulary, and a queue exits
  with the code of the member that stopped it, so one table reads the same
  way whether one change was integrated or twenty.

    `arc check` exits 0 when the change is ready under the integration shape
      resolved for its target. --shape ff-only|no-ff evaluates an explicit
      alternative; status uses the resolved default.
    `arc check` exits 2 while a blocking finding is open.
    `arc check` exits 3 when no valid approval covers the current head.
    `arc check` exits 4 while a hold is active.
    `arc check` exits 5 when a required gate is not green at the head or its
      effective budget declarations are invalid (`arc guide gates`).
    `arc check` exits 6 for a closed change, a missing branch, or a target
      branch that cannot be resolved.
    `arc check` exits 7 while a prerequisite change is unresolved.
    `arc check` exits 19 while a must-land-together partner is open or
      closed without integrating; the change lands only with `arc land`.
    `arc check` exits 20 while a pending landing attempt holds the change;
      `arc land --recover <attempt>` finishes it.
    `arc check` exits 11 when the target conflicts or ff-only ancestry
      requires rebasing; it shadows 14 when both are present.
    `arc check` exits 12 when a declared acceptance probe is not
      discriminating.
    `arc check` exits 13 while the change declares it is iterating.
    `arc check` exits 14 when the tree a merge would ship has no gate
      evidence; a `not-admitted` run is none.
    `arc check` exits 15 when the change's branch is a fork's: fork work is
      unintegrated by intent, and the boundary binds to the change rather
      than to the directory the command runs in.
    `arc check --tag` exits with the code of the first blocked change it
      lists, and 0 when every match is ready or closed.
    Arc exits 17 when a paired replica does not hold integration authority;
      the refusal names the holder or an offer in flight.

  Human blocker reports add exact topic routes beside their existing facts.
  A refusal lists its blockers, then `Next step:`: the command that does
  what comes next, with the change filled in and the `next_action` code in
  parentheses, as in `arc verify <change> --gate test (run_gate:test)`.
  Where no single command does it, the line says what to do and names the
  command that records it; an approval rejection prints the policy's reason
  in place of a code. `status`, `show`, `review`, `resume`, and `rescue`
  print the same line, and `--json` carries the code alone.

  Codes 1 and 2 are also reachable without a blocker at all: `arc` exits 1 on
  an internal error and 2 on a usage error, which argument parsing decides
  before any command runs. A signal that interrupts a gate or probe ends arc
  with that signal rather than an exit code; see `arc guide change-gates`.

  The other commands a script branches on:

    `arc claim`, `arc stage`, and `arc release-claim` exit 8 on a claim or
      stage ownership conflict.
    arc exits 9 when the execution role refused the command, before it takes
      a lock or writes an event.
    `arc is-blocked` exits 0 when the change is ready, 1 when it is blocked,
      and 2 when the lookup or ledger read failed.
    `arc watch` exits 0 when its condition is reached and 2 on a timeout.
    `arc take` exits 2 when no change is ready.
    `arc env` exits 1 and prints the export template when no harness is
      detected.
    Both import commands exit 18 for publishing authorization refusals,
      including missing provenance, digest mismatch, exporter/origin mismatch,
      missing capability, forbidden actor claims, and authorization movement.
      Exit 1 reports invalid bundles, enrollment, replay, or publication failures.
      An admission batch publishes change facts, repository facts and its local
      authorization receipt together. Admission batches require store format 7.
      Temporary batches are invisible. Receipts
      are private admission evidence and never exported as a trust grant.
      Retention failures after publication name the published admission.
    `arc import` exits 1 and writes nothing when an event conflicts.
    `arc changelog --write` exits 1 and writes nothing when the renderer
      cannot perform the write, naming the target and the reason on stderr.
    `arc changelog` exits 1 and leaves the target byte-identical when a
      command renderer cannot start, exits non-zero, overruns its timeout,
      prints more than 16 MiB or output that is not UTF-8, or prints nothing
      for `--write`.
    `arc import` refuses a delta bundle whose prefix this store does not
      hold, and writes nothing.
    `arc forge link` exits 10 when the observed tuple or the declared policy
      does not match, appending no event.
    `arc history resolve` exits 2 when nothing moved the revision.
    `arc candidate retire` exits 1 and writes nothing while a root reaches
      the candidate, naming the root.
    `arc restack --advise` exits 0 when the change has no dependents.
    `arc workspace backlog` exits 16 when any selected project's observation
      failed; the rows that were read still print.
    `arc journal render --json` exits 16 when any address failed to render;
      its entry carries the error and the other pages still render.
    `arc doctor` exits 1 when problems are present and 0 for a clean or
      advice-only ledger; a ledger it cannot read also exits 1, with the
      error on stderr and no report.
    `arc journal doctor` exits 1 when problems are present and 0 for a clean
      or advice-only journal; a journal it cannot read also exits 1, with the
      error on stderr and no report.
    A rejected self-approval follows the no-valid-approval path and exits 3.
    An arc-managed Git hook always exits 0, so it can never block a commit.
    A repeat of a recorded source item writes nothing, prints the existing
      entry, and exits 0.
    A spooled write prints `spooled: <path>` and exits 0.

FILES
  User config is `<ai-home>/arc/config.toml`. The AI home defaults to
  `~/.local/ai/`; ARC_AI_HOME or AI_HOME overrides it. ARC_WORKTREES_DIR and
  ARC_DATA_ROOT override their keys; ARC_DATA_DIR selects one repository's
  exact ledger above both. It holds events and `custody/sha256/<digest>` forge
  content. `arc config` prints resolved paths. Only a missing configuration
  selects built-in settings. Other read or parse errors propagate; advisory
  consumers name their fallback and the error on stderr.

    worktrees_dir = "~/.worktrees"       where change worktrees are created
    data_root = "<dir>"                  ledgers at <dir>/<repo-path-slug>/
                                         rather than <git-common-dir>/arc/
    [journals] dirs = { "<prefix>" = "<journal-dir>" }
                                         journal by path prefix; the longest
                                         matching prefix wins
    [journal] auto_log = true            begin/integrate/close log to journal;
                                         unreadable configuration disables it
                                         and reports why on stderr
    [identity] detect = true             an undeclared harness, session, and
                                         model are read from the session store
    [provenance] git_identity = "per-actor" | "shared"
    [intake.<name>] argv = ["<cmd>", ..] open-item count; arc catchup --help
    [change] checkout = "in-place" | "worktree", branch_prefix = "arc/"
    [integration] shape = "ff-only" | "no-ff", cleanup = false

  Policy is `.arc/policy.toml`, read from the change's target branch, and
  the operator policy at `<git-common-dir>/arc/operator-policy.toml`, outside
  every tree (`arc policy path|show|write`). Both apply, the stricter reading
  winning.
  `arc doctor` reports an unreadable or malformed policy as the problem
  `policy-unreadable`, naming its file and error, and exits 1.

    [policy] forbid_self_approval = true
    [policy] require_declared_actor = true
    [policy] debt_count_threshold = <n>          debt turns advisory past n
    [policy] debt_age_threshold_seconds = <n>    or once the oldest is older
    [policy] worktree_free_floor_bytes = <n>     `arc begin` warns below it
    [policy] tiebreak_reviewers = [{ model = "reader#xhigh", since = "<RFC 3339>" }]
      Dated elections: arc guide debt-review
    [policy] review_clearance = "assessed"       what clears: arc guide debt-clearance
    [danger] paths = ["<glob>"]                  `*` in a segment, `**` across
    [danger] acknowledged_safe = ["<glob>"]
    [danger] source_roots = ["<dir>/"]           every file inside is classified
    [review] checklist = ["<item>"]              printed by `arc show`
    [contribution] history = "squash" | "preserve"
    [defaults.change] checkout = "worktree", branch_prefix = "arc/"
    [defaults.integration] shape = "ff-only", cleanup = false, discard = "artifacts"
    [integration] shapes = ["ff-only", "no-ff"], history = "preserve"
    [candidates] evaluation_reuse = "matching-coordinates" | "never"
                                                 no default; `never` wins

  Style precedence is flag, operator defaults, target project defaults, user,
  built-in. Project and operator shape constraints intersect; values outside
  the intersection refuse. Style and constraint tables must be tables, and
  known keys must have valid types and values; refusals name the file and
  key. User configuration accepts style only.
  `integration.cleanup` governs post-integration branch and worktree cleanup;
  `integrate --no-cleanup` overrides it for every selected change.
  `integration.discard` resolves through the style layers and accepts
  `"artifacts"` (built-in) or `"ignored"`. `integrate --discard` selects the
  scope, implies cleanup, and conflicts with `--no-cleanup`. The ignored
  scope uses the parent checkout's Git ignore rules. A worktree containing
  another repository is never removed under either scope. Cleanup walks the
  whole filesystem tree independently of Git status, checking every directory
  for `.git` entries and bare repository layouts (`HEAD`, `objects/` and
  `refs/`). The checkout's own `.git` file at its root is the only exclusion.
  The walk never follows symlinks. A repository, a symlinked recorded path or
  any read error keeps the checkout; repository paths are named. Tracked edits, untracked files and
  index entries marked
  skip-worktree or assume-unchanged keep it. Ignored `.arc/` entries,
  including recognized artifacts, keep it under either scope; only owned,
  emptied outbox metadata is exempt. Ownership checks apply under either
  scope. Cleanup uses the values selected before the merge for cleanup,
  discard and disposable; a change's policy edits do not govern its own
  cleanup. Ignored directories outside `.arc/` identified as build artifacts
  by a valid CACHEDIR.TAG, a built-in recognizer, or an
  `integration.disposable` rule's path and marker are eligible for removal
  only if their repository scans succeed and find no repositories. Other
  ignored content keeps the checkout with the artifacts scope. Every recognizer names a file its tool writes inside the
  directory; a directory name alone identifies nothing.
  A CACHEDIR.TAG must be a regular file beginning with the exact bytes
  `Signature: 8a477f597d28d172789f06886806bc55`, which Cargo, Meson, uv,
  pytest and other tools write. A marker shows only that a tool ran in a
  directory, so a built-in recognizer exists only where the layout leaves the
  tool's own workflow no place for sources. Each marker is a regular file
  that must begin with the bytes shown:

    **/dist-newstyle   cache/plan.json     `{"cabal-version":`
                       No `src` entry, where Cabal clones dependency sources.
    **/*.egg-info      PKG-INFO            `Metadata-Version: `
                       Only files setuptools writes there: `PKG-INFO`,
                       `SOURCES.txt`, `requires.txt`, `entry_points.txt`,
                       `top_level.txt`, `dependency_links.txt`,
                       `namespace_packages.txt`, `eager_resources.txt`,
                       `zip-safe` and `not-zip-safe`.
    **/__pycache__     Only `<module>.<cpython-* or pypy*>.pyc` files, each
                       opening with a magic number whose last two bytes are a
                       carriage return and a line feed.

  CMake build directories, virtual environments without a CACHEDIR.TAG,
  Ninja logs and `node_modules` are not recognized. CMake's FetchContent
  and ExternalProject place dependency sources inside the build directory,
  a virtual environment can be created over an application or hold a
  patched package, a log can sit beside a hand-written `build.ninja`, and
  installed packages can carry edits not yet saved as a patch. A
  configured rule declares such a directory disposable.

  CACHEDIR.TAG and the built-in recognizers always apply at any depth,
  independently of configured rules and checkout ownership. A configured rule
  covers an artifact they do not:

    [integration]
    disposable = [{ path = "build", marker = "CMakeCache.txt" }]

  `integration.disposable` is an array of rules with required string `path`
  and `marker`, and optional string `signature`. Operator defaults, project
  defaults and user configuration resolve above the built-in empty array;
  the highest layer setting it replaces the entire array. There is no flag.
  `path` matches the worktree-relative directory path: `*` stays within a
  component, `**` matches zero or more whole components, and other characters
  are literal. Paths must be relative with no empty, `.` or `..` component.
  The marker must be a regular file inside a real directory, with no symlink
  traversal; `signature` requires its content to begin with those exact bytes.
  Individually reported ignored files are discarded only with the ignored
  scope. Cleanup reports each discarded entry after Git removes the worktree
  and before reporting removal. Recognized artifacts name their recognizer;
  other ignored entries name `integration.discard = ignored`. Cleanup
  deletes no artifacts from a checkout it keeps.
  Integration history folds with contribution history; squash requires at
  most one reviewed commit above the target. Integration alone stays local.

  Gate declarations in `.arc/gates.toml` and the operator policy are
  described in `arc guide gates`.

  `.arc/worktrees.toml` declares optional build preparation:

    [prepare]
    command = "<project preparation command>"
    timeout = "30s"

  Preparation runs after begin, fork, acceptance and replay checkout
  population. The default `source-bound` mode gives its working directory
  and `ARC_WORKTREE_BUILD` a checkout-specific source-tree directory. Gates
  and probes use prepared output only with a matching successful receipt;
  otherwise they receive fresh cold storage.
  `ARC_WORKTREE_PATH`, `ARC_WORKTREE_REVISION` and `ARC_WORKTREE_TREE` identify
  the source; `ARC_WORKTREE_CACHE` names a shared cache protected by a lease.
  Build outputs belong in the build directory and reusable compiler data in
  the cache. An inherited `CARGO_TARGET_DIR` is removed. A busy cache or failed
  preparation continues cold; changed source, index, HEAD or untracked source
  paths refuses the checkout and keeps it for inspection. Evaluation
  checkouts are removed on other preparation errors. The timeout covers
  output pipes and cleanup of the hook's process group. Preparation is never
  gate evidence.
  Gate commands and environment probes receive the same build/source/cache
  variables. A project selects its prepared output explicitly, such as
  `CARGO_TARGET_DIR="$ARC_WORKTREE_BUILD/target"`. An administrative receipt
  reuses preparation at unchanged source; changed source prepares again.

  A project whose compiler workspace depends on absolute paths can select
  persistent storage:

    [prepare]
    mode = "path-stable"
    directory = "../build"
    command = "<project incremental preparation command>"
    timeout = "30s"

  `directory` is a literal absolute path or a path relative to the source
  checkout. Without it, storage under the Git common directory is keyed by
  the canonical checkout path, independently of its tree or administrative
  allocation. Stable storage survives `integrate --cleanup`. `[slots]`
  declares arc's pool of fixed-path checkouts (arc guide slots). A project
  driver recreating a fixed `--worktree` path beside `../build` owns that
  allocation and drains its external writers before checkout cleanup.
  Keeping only the build path fixed cannot stabilize changing source paths.

  A stable directory is available without a per-tree preparation receipt.
  `command` is optional in this mode: project hooks or gates establish
  compiler, header, dependency and source-metadata freshness. Their ordinary
  checks remain authoritative. Arc never copies or clears the directory.
  Control files live beside it, so a compiler clean can remove its outputs
  without removing the active lease. The parent directory must remain
  writable and control files must be preserved while writers are active.
  A foreign owner, busy lease or failed preparation refuses stable use.
  Failed or interrupted hooks retain an incomplete marker until successful
  preparation retries; readiness probes do not run preparation or compile.
  A busy or incomplete directory makes its probe identity unavailable.

  Arc holds the exclusive build lease through each owned hook, probe and
  gate. `ARC_WORKTREE_BUILD_LEASE` names it and
  `ARC_WORKTREE_BUILD_LEASE_FD` identifies the inherited held descriptor.
  Cooperating project commands keep that descriptor open through their
  writers and reuse it rather than flocking the file again; external builds
  acquire the same lease. Parent death does not close the child's inherited
  descriptor. Source/index/HEAD mutation checks and gate evidence rules
  apply in both modes; build storage and preparation grant no verification.
  Index checks compare indexed content and flags, permitting harmless stat
  refreshes when a project restores timestamps for byte-identical sources.

  A timeout bounds the process and its output-pipe cleanup. A descendant
  holding a pipe after the leader exits still times out; arc kills the
  process group and returns without waiting on an escaped descendant. An
  unfinished output reader supplies no tail or environment identity.
  Whether a gate or probe finishes or overruns, arc kills its process group
  before recording the result, so a background process it left behind does
  not outlive it.

  Every patchset's head is pinned by `refs/arc/keep/<change>/<patchset>`
  so garbage collection cannot take it. Pins follow recorded rewrites onto
  their successors. The commit a pin held, and any commit the ledger cites
  that a rewrite, rebase or import would leave unreachable, is held under
  `refs/arc/retained/<change>/<subject>/<commit>` (`arc guide
  rewrites-evidence`). A pin or a retention ref is released only once
  its commit is reachable from the integration commit, where history holds it;
  a replaced commit is on no branch, so its ref stays while the ledger cites
  it, and closing a change without integrating it releases nothing. Release
  any other with `git update-ref -d`. A registered candidate's tree is pinned by
  `refs/arc/candidate/<id>` until `arc candidate retire` releases it.

  Publication receipts retain their tested commits under
  `refs/arc/publication/<receipt-id>`, named by the receipt's `retention_ref`.
  Export and import preserve the tested objects and pin identity. Integration cleanup keeps these refs while the
  publication history cites them; an operator may release one deliberately
  with `git update-ref -d`.

GATES
  `arc gates [--json]` lists every declaration on this tree beside its cost
  across changes, from per-change verification events. Declarations include
  `exclusive` and `admission_ms`, the effective admission limit when a lock is
  named or an explicitly declared limit. Both are null when omitted without a
  lock. Gate rows include budget_ms, sources, command-time budget share and the
  last measured over-budget run. The report includes the declared total and
  the unqualified integration sum; profile violations are listed separately.
  Runs, exact-tree reuses and selector skips are counted separately; attested
  evidence and not-admitted attempts are separate from runs and timings.
  History groups gates by name across declarations. Median (lower middle),
  p90 (nearest rank) and worst use command `run_ms`; only older events without
  it use `duration_ms`, including preparation and waiting. The report names
  how many such samples it uses. `ever_failed` describes observed runs;
  `last_failed_first` is the latest change whose first observed verification
  failure was this gate, with its time and event. Dedicated publication
  receipts carry no timing and are outside this per-change history.
  Each declaration includes `paths` and its effective `inert` exclusions.
  `unclaimed_paths` counts tracked paths outside inert exclusions that no
  explicit gate pattern names; a gate with no paths does not name a path.
  Unreadable changes are named, readable history is retained, and a partial
  report exits 16. JSON is `arc-gates/1`.

  `arc gates paths [--json]` lists every tracked path and its affected gates,
  including paths nobody names. It uses one `git ls-files` inventory and no
  per-path commands or ledger reads. JSON is `arc-gate-paths/1`, with `paths`
  rows (`path`, `gates`, `inert`, `unclaimed`) and `unclaimed_paths` names.
  Paths beneath inert exclusions are excluded from the unclaimed count;
  dangerous inert paths still affect every gate.

  Budget shares use at most the latest 20 command-time observations for the
  current command and declared timeout, in the newest observed environment.
  Legacy duration_ms samples remain in the named historical distribution and
  never enter budget shares. A median above 80% is labelled in the report.

  `arc gates init` measures each declaration once, writes each observed run_ms
  as its budget in .arc/gates.toml, and sets total to their sum. Operator-only
  declarations are copied into that project file; operator policy is untouched.
  A sub-millisecond measurement declares 1ms. It refuses
  existing project or operator budgets, failed or unmeasured commands, and
  declarations edited during measurement. It records no verification evidence.
  `arc gates over [CHANGE] [--gate G]` shows the newest measured run exceeding
  the applicable budget, its history sources, host observations and output
  artifact. Investigate the slow checks, make them faster, remove checks worth
  less, and consider a declared raise only after that work.

  `arc gates measure [--runs N] [--gate G]` runs declarations sequentially on
  this tree, five times by default, honoring timeouts. Each round follows the
  `exclusive` and `admission` contract below. It prints command wall and child
  CPU time with admission wait separately, and the wall min, max and spread.
  Wall far above CPU indicates waiting inside the command; a wide spread
  indicates load sensitivity. A round not admitted prints
  `not measured (not-admitted)` and contributes no timing sample. Summaries
  count measured and unmeasured rounds. It records no evidence and writes no
  ledger or output artifacts; the gate commands may write their own build
  outputs. A failed, timed-out or unmeasured round makes the command exit 1.
  Unknown gates and zero runs are refused. Every declared gate runs unless
  `--gate` selects one.

  `arc workspace gates [--json] [--under PATH|--here|--global] [--retired]`
  lists each registered project's declarations and its own ledger's costs,
  including projects without gates. Global is the default. Scope and the
  collection manifest follow `arc workspace backlog`: retired registrations
  are counted without observation, `--retired` lists them, orphaned anchors
  are failures, and empty unresolved registrations count as empty. Partial
  collection retains readable rows and exits 16. JSON is
  `arc-workspace-gates/1`. These commands record no events and their ledger
  reads do not require a parseable append policy.

  Gates are `[gates.<name>]` tables in `.arc/gates.toml` or the operator
  policy:

    command = "<shell command>"
    profiles = ["<profile>"]            omitted: required for every profile
    events = ["integration"]           omitted: integration and publication
    timeout = "10m"                     s, m, or h; omitted: no declared timeout
    budget = "2.5s"                     command wall limit for acceptance
    paths = ["src/", "tests/"]           omitted or empty: every non-inert path
    exclusive = "host"                  host-wide lock name; omitted: no lock
    admission = "60s"                   lock wait limit; omitted: 60s
    environment = "<probe command>"     evidence counts only where the probe
                                        prints the same output, run against
                                        the content being evaluated
    bypass = "<what qualifies>"         omitted: the gate cannot be bypassed;
                                        every declaration of it must name one
    binds = "commit"                    a pass answers for its commit, not
                                        its tree; omitted: "tree", as an
                                        older arc reads it

  Budget durations accept positive whole milliseconds with ms, s, m or h
  units, including decimal forms such as 2.5s and 0.001s. Overflow and precision
  below a millisecond are refused. A [budget] table may declare total = "15s".
  Every required integration gate needs a budget when that table exists.
  The total is the sum of all required gate budgets, without a concurrency
  discount. The effective suite is validated after layering for every declared
  profile and the unqualified case. A missing budget or excessive sum is a
  doctor finding and independently blocks check, accept, integrate and land, even
  when every gate row is green or the active profile requires no gate.

  The budget in force is the minimum of every applicable selected destination,
  committed change-head, operator and provisional staging declaration. Absence
  never loosens a budget or total. A change may add or tighten an obligation;
  it cannot raise one through a declaration alone. These constraints and their
  sources enter the authorization basis and are confirmed before integration.
  Budgets are outside execution identity. Reuse selects the same structurally
  eligible receipt and then judges its command time, with no search for an
  older faster receipt and no implicit rerun. An over-budget selected receipt
  is refused until explicit verify --rerun. The integration queue stops on a
  recorded budget refusal. Reduction and replay read structural facts only.

  A command outcome remains pass in the ledger even when its run_ms exceeds
  the budget in force. Equality passes; one millisecond over fails. Ordinary
  verify exits 1 for a budget refusal, including parallel preflight whose
  receipts retain unknown cleanliness. Status, check and show project current
  budget applicability beside the raw result. Show JSON adds gate_status beside
  raw verifications; status gate rows carry budget.verdict, budget_ms, run_ms,
  excess_ms and sources. budget_rules.total is milliseconds in JSON.
  Missing run_ms is unmeasured
  and cannot satisfy a budget, including legacy local, imported and attested
  evidence. duration_ms never substitutes for run_ms. A bypass cannot overrule
  an observed budget refusal. Explain labels historical authorization apart
  from applicability under the current destination and operator declarations;
  a later reduction does not rewrite closure or the recorded authorization.

  Without a declared timeout, a budgeted command has an execution limit of
  ten times its effective budget, computed in checked milliseconds. The clock
  starts with the command and includes output drain, excluding admission and
  preparation. Evidence keeps timeout_seconds exactly as declared, including
  absence, and separately records execution_limit_ms and its origin, timeout
  or budget-times-ten. A receipt's recorded budget is provenance; current
  judgement uses the budget in force when the receipt is read. Timed evidence
  needs no recorded budget judgement to count. Verify warns when the median of
  up to 20 matching command-time observations in this change exceeds 80% of
  its budget; legacy total durations are excluded.

  Refusals name command time, effective budget and sources, excess, host,
  start load averages and online cores, CPU where recorded, and excluded wait
  and preparation. Missing observations stay missing. Load above the core count
  may indicate possible host contention; it never changes the judgement.

  A ledger adopts store format 8 before gate verification or reuse under any
  budget declaration, including a total with missing individual budgets,
  before writing budget-bearing evidence or authorization, or when an
  import consumes budget declarations, evidence or authorization. Older
  executables supporting formats through 7 refuse that ledger. Reads alone do
  not adopt it; budget-free writes retain the ordinary event envelope and
  ledger format.
  Candidate selection and promotion, and arc publication do not judge
  budgets. Their acceptance contracts do not provide budget enforcement.

  A gate command exits 75 (`EX_TEMPFAIL`) to say it did not run, which arc
  records as `not-admitted`. Both are described in `arc guide change-gates`.

  An exclusive name admits one command at a time across checkouts and projects
  in the account. `host` conventionally reserves the machine; another name
  can reserve a browser or port. Locks live under
  `~/.local/ai/arc/exclusive`, independently of repository and ledger paths;
  a sandbox prefix supplies that home when sandboxing. A holder retains the
  lock until the command's last descendant exits, including detached children
  and after the launching arc exits. There is no queue order or role queue.
  A waiter prints the holding project, change, gate and elapsed hold time.
  Admission expiry starts no command and records `not-admitted`, not a gate
  failure. `wait_ms` includes exclusive admission and is separate from `run_ms`.
  Admission accepts positive durations such as `500ms`, `2.5s`, `2m` or `1h`.
  Both keys are outside evidence identity: they govern admission, not the check.
  Either declaration may require a lock; an omitted key cannot remove one.
  Matching lock names take the shortest admission limit, including the default.
  Different names conflict between project and operator policy; between target
  and change they report a divergence and the target's lock stays in force.

  `arc verify --all --event integration|publication` selects the event's
  gates. Evidence identifies its event, tree, command and environment;
  a publication pass does not satisfy an integration requirement.
  Historical verification evidence with no recorded event is integration
  evidence. An omitted event on a gate declaration applies to both events.
  `arc publication --target <branch> --profile <profile>` runs the complete
  publication tier at that branch's head. Push and release tooling can use
  `--revision <full-commit-id>` to require the exact revision queued by the
  caller and use its exit status as a local guard. Arc makes no network
  request and installs no hook implicitly. A failed publication names the integrations since the
  most recent successful publication pass.

  `[selection] inert = ["**/*.md"]` declares paths that affect no gate.
  Patterns use `policy::glob_match`, as path policy does: `*` stays within a
  segment, `**` crosses segments, `**/` includes zero directories, and a
  trailing `/` includes everything beneath that directory. Patterns are
  case-sensitive and match repository-relative paths.

  A path affects gates whose `paths` match it. Inert paths affect none. A path
  no explicit gate pattern names affects every gate. Dangerous policy paths
  affect every gate, including those beneath inert exclusions. A gate without
  paths is affected by every non-inert path. Renames check both old and new
  paths. The inventory includes gates from every profile and event.

  Integration selection needs an earlier pass in the same change, with the
  same declaration and environment. A new change runs its required gates even
  for an inert-only delta. Publication and commit-bound gates do not select
  across trees. Arc emits `arc-gate-selection/1` as selector `arc:paths` when
  paths prove the gate unaffected. It asks a declared script only when the
  gate remains affected; unnamed and dangerous paths cannot be cleared by a
  script while path selection is declared. An uncertain delta runs the gate.
  An unreadable danger declaration affects every path; inspection remains
  available when append policy cannot be parsed.

  Paths and inert exclusions participate in evidence identity. The complete
  coverage map and dangerous patterns also participate because they determine
  unnamed-path fallback. Declaring nonempty coverage settings requires a fresh
  pass; legacy evidence without identity cannot answer a scoped declaration.
  A declaration without coverage settings retains its default identity. Target
  coverage stays in force until an edited declaration lands, as with selector
  changes. Project and operator inert lists combine; differing paths for the
  same gate conflict. Scoped run receipts retain identity, and delta receipts
  retain the acknowledged request, answer and its digest for replay.

  A gate's `selector` reads `arc-gate-delta/1` JSON: `source_tree`,
  `target_tree`, `paths` and `gates`. Its `arc-gate-selection/1` answer
  includes `complete: true`, `request` equal to the complete input object,
  and `gates` naming affected checks. A successful stdin write does not prove
  consumption. Missing, failed, malformed, incomplete, unknown or mismatched
  answers run the required checks. Selection applies to integration;
  publication checks every required gate on a changed tree. Reuse receipts
  retain the passing source tree, target tree, selector and declarations;
  they grant no result to another event or environment.

SLOTS
  `[slots]` in `.arc/worktrees.toml` declares a pool of fixed-path checkouts
  kept across changes, so a change starts from the previous change's build:

    [slots]
    size = 3                    # 1..=32, fixed once the pool exists
    root = "~/.worktrees/x"     # default <worktrees_dir>/<repo>-slots
    documentation = ["*.md"]    # paths ranking ignores (the default)
    identity = "rustc -vV"      # toolchain identity; its stdout is digested
    retain = ["target"]         # ignored paths a slot carries between changes
    invalidate = "<command>"    # discards outputs on an identity change
    default = true              # begin uses a slot for worktree checkouts
    warm = "<command>"          # what `arc slots warm` builds a slot with
    warm_after_integrate = true # integrate starts one warm (default false)

  A pattern without `/` matches a file name at any depth, one with `/`
  matches from the checkout root, and `*` and `?` stay within one path
  component. A project without `[slots]` sees no change; a malformed
  `[slots]` table refuses every command that reads it, as does
  `warm_after_integrate` without `warm`. The identity command is bounded at
  120 s and must exit 0; `invalidate` is bounded at 600 s.

  The root holds `pool.json` (the repository's canonical Git common
  directory and a random pool id), `pool.lock`, and per slot `slot-NN.json`
  (its state), `slot-NN.use.lock` and the checkout `slot-NN`. A root another
  repository owns refuses, as does a nonempty directory that is not a pool.
  The pool lock is innermost: held only around reading and writing slot
  state, with nothing acquired under it. A slot is reused only when its
  checkout is registered to this repository at that path with the
  administrative directory and owner record arc wrote when creating it;
  unknown ownership refuses reuse.

  `arc begin <slug> --slot`, or any worktree checkout without --worktree,
  --no-worktree or --adopt when `default = true`, leases the best-ranked free
  slot under the pool lock and releases the lock. Ranking uses the slot's
  `built` record (revision and identity digest, set only by a successful
  preparation or warm in that slot, never by a gate) when its identity matches the
  identity command's digest in the checkout running begin: known distances
  first, by paths changed from the built revision to the target outside
  `documentation`; then built revisions that cannot be compared; then cold
  slots (no matching build, or no checkout). Ties keep slot order. A slot
  whose state cannot be read is never handed out, and a free slot still in
  use is skipped. Exhaustion refuses with the slot table.

  An absent checkout is created with `git worktree add`. A present one must
  be owned, clean, detached, hold no open change and carry no ignored path
  outside `retain`, as Git's status lists ignored paths (an emptied outbox
  is exempt). Otherwise begin refuses naming the slot and the path, and keeps
  the lease for inspection with the `arc slots recover` command to run once
  resolved. Begin then runs `git switch -c <branch> <target>` and the
  identity command in the slot. On a digest change it runs `invalidate` with
  the gate environment; without one it removes the `retain` paths and the
  arc-owned storage keyed by the checkout (source-bound builds and receipts,
  path-stable storage under the Git common directory). Path-stable storage in
  a project `directory` without `invalidate` refuses the change. Preparation
  follows. The change records the slot as its worktree, with a `slot-leased`
  event carrying the lease token. The slot state's phase (`leasing`,
  `switching`, `recorded`, `warming`, `detaching`, `releasing`) lets
  `recover` resume an interrupted operation.

  Gates, environment probes and preparation in a slot hold its use lock
  shared, waiting for an operation that changes the checkout. Begin's
  switch, warming, integration's target-checkout takeover and merge,
  integrate's and close's detach, release, recover,
  `rebase` replay and `rewrite` in a slot checkout hold it exclusively
  without waiting and refuse while it is held. One arc process holds a slot's lock once; nested
  work shares that hold. A command in a slot runs under an arc holder that
  leads its process group and inherits the lock. The holder is a child
  subreaper: it reports the command's exit and keeps the lock until `wait`
  reports no child left, so a descendant that detaches or closes its
  descriptors keeps the slot in use after the arc that launched it is
  killed. Arc's completion and timeout handling still stop the whole process
  group, holder included; an escaped descendant then holds the slot only
  while it keeps the inherited descriptor. Outside slots no holder runs.

  A slot checkout is never removed by `--cleanup` or `[integration] cleanup`.
  After recording an integration, arc detaches the clean slot at the landed
  commit, deletes the merged branch with `git branch -d`, records
  `slot-released` on the change and frees the lease. `close`, whatever its
  outcome, detaches a clean slot at its `HEAD`, keeps the branch, records
  `slot-released` and frees it. A slot in use, with uncommitted or untracked
  work, or failing ownership keeps its lease, and the command prints the
  release command to run once resolved. A provisional integration resolved
  by `arc replica provisional resolve` releases its slot the same way, at
  the accepting integration's commit when this repository holds it and at
  the slot's `HEAD` otherwise. A slot record naming a pool this repository
  does not own, as one imported from another replica does, is left alone.

  `arc slots warm [--target <rev>] [--limit <n>]` builds slots before a
  change needs them. Under the pool lock it ranks the free slots as begin
  does for the target (default: the primary checkout's branch head) and
  leases the best one that is not current: current means a `built` record
  at the target under the identity of the checkout running warm, so a cold
  slot whose `HEAD` already stands at the target is still warmed. It takes
  the slot's use lock exclusively for the whole operation, detaches the
  checkout at the target under begin's reuse rules (creating it when
  absent, invalidating on an identity change), and runs `warm` under the
  holder, which receives that exclusive descriptor and never acquires the
  lock itself. The command runs through the preparation and environment
  gates use (`ARC_WORKTREE_*`, path-stable storage), so it fills the storage
  gates consume, at `nice` 19 in the idle I/O class. Success that leaves the
  checkout detached at the target and reusable records `built` and frees the
  slot; anything else keeps the lease in phase `warming` and prints the
  `recover` command, which detaches the clean checkout where it stands and
  frees it. A running warm's slot is leased, so begin skips it. Warming
  takes one slot at a time; `--limit` (default 1) bounds how many. Without
  `warm`, it refuses naming the key.

  With `warm_after_integrate = true`, a successful `arc integrate` starts one
  detached `arc slots warm --limit 1`, its output in `warm.log` in the pool
  root, but only when the pool exists and no slot's use lock is held, so no
  gate or begin of this repository is running in a slot. It never blocks or
  fails the integration and says what it started or why it started none.

  `arc slots status [--json]` (`arc-slots/1`) shows each slot's phase, lease
  and token, the change holding the lease with its slug and state (`open`,
  its closure outcome, `absent`, or `unreadable`; for a lease a landing
  attempt took to gate in, the attempt with `landing-pending`,
  `landing-completed` or `landing-abandoned`), checkout `HEAD`, built
  revision and identity match, rank for the primary checkout's branch, and
  whether it is in use. Before the pool exists it reports the declared pool
  as not yet created (`created: false`) and succeeds. A change's slot also appears in
  `arc status` (`slot`), `arc show` and each `arc stats --round-trip`
  landing. `arc slots release
  <slot> --token <t>` frees a lease whose change is closed or absent, or
  whose landing attempt is finished; both it and `arc slots recover` refuse
  while the attempt is pending, naming `arc land --recover <attempt>`,
  which frees the slot itself.
  `arc slots recover <slot> --token <t>` resumes a phase: a begin whose
  change exists records the lease on it only when the owned, clean checkout
  still holds that change's branch at its recorded worktree path, including
  when the lease event already exists. It takes exclusive use before checking
  or recording the lease or phase. A begin that opened no change is undone,
  deleting its branch only while it still points at the target; later phases
  and warms finish the release. Both refuse an obsolete token, a busy slot,
  a dirty checkout and unknown ownership. Leases never expire; tokens appear only in
  status and recovery output. Compiler freshness after a switch stays with
  the project's build, as for path-stable storage.

SCHEMAS
  Every structured surface carries a `schema` string `<name>/<n>`. A shape
  change takes one new version per release, counted from the last release:
  adding a field as much as removing, renaming, or redefining one. A surface
  already bumped for the unreleased version keeps that number for further
  shape changes. A commitment is a shape callers outside arc read, and its
  version is a promise. An internal shape is arc's own on-disk bookkeeping;
  parsing one means tracking arc's implementation.

  An input format, stored or supplied, is versioned from the reader's side:
  its version marks what a reader must accept, so a new optional field that
  leaves every older file valid keeps the version, and removing a field or
  making one required takes the next. `journal-events/2` and
  `arc-trailer-edits/1` are two.

  Commitments, derived views:
    `arc-forge-view/1`               arc forge show --json; forge reconstruction
    `arc-state/3`                    arc show --json
    `arc-trailers/1`                 arc trailers --json
    `arc-status/27`                  arc status
    `arc-assurance/1`                arc query --assurance --json
    `arc-check/4`                    arc check --json
    `arc-inbox/11`                   arc inbox --json
    `arc-catchup/12`                 arc catchup --json
    `arc-journal-catchup/9`          arc journal catchup --json
    `arc-resume/8`                   arc resume --json
    `arc-explain/2`                  arc explain --json
    `arc-watch/1`                    arc watch --json, stdin of arc watch --exec
    `arc-config/1`                   arc config
    `arc-accept/1`                   arc accept --json
    `arc-export/1`                   arc export --json
    `arc-import/1`                   arc import --json
    `arc-integration-plan/1`         arc integrate --dry-run --json
    `arc-landing-plan/1`             arc land --dry-run --json
    `arc-gate-delta/1`               gate selector stdin
    `arc-gate-selection/1`           gate selector stdout
    `arc-provisional-transition/2`   arc integrate --provisional --json
    `arc-brief/2`                    arc brief --json
    `arc-journal-artifact/3`         arc journal show --json
    `arc-journal-entry/1`            arc journal show <entry address> --json
    `arc-journal-open/1`             arc journal open --json
    `arc-journal-pins/1`             arc journal pins --json
    `arc-journal-render/1`           arc journal render --json
    `arc-journal-inventory/6`        arc journal inventory --json
    `arc-rescue/5`                   arc rescue --json
    `arc-review/5`                   arc review --json
    `arc-receipt-result/1`           arc review|audit --receipt --json
    `arc-findings/3`                 arc findings --format json
    `arc-blocker-status/1`           arc blocker-status --json
    `arc-metadata/1`                 arc metadata --json
    `arc-chain/5`                    arc chain --json
    `arc-candidate/1`                arc candidate show|list --json
    `arc-gates/1`                    arc gates --json
    `arc-gate-paths/1`               arc gates paths --json
    `arc-workspace-gates/1`          arc workspace gates --json
    `arc-stats/2`                    arc stats --json
    `arc-stats-by-model/1`           arc stats --by-model --json
    `arc-stats-provenance/1`         arc stats --provenance --json
    `arc-stats-round-trip/1`         arc stats --round-trip --json
    `arc-cost/1`                     arc cost show --json
    `arc-cost-commit/1`              arc cost show --commit --json
    `arc-changelog/1`                arc changelog --json
    `arc-changelog-render-request/1` stdin of a command changelog renderer
    `arc-forks/3`                    arc fork list --json
    `arc-run-list/1`                 arc run list --json
    `arc-doctor/6`                   arc doctor --json
    `arc-slots/1`                    arc slots status --json
    `arc-perf/1`                     ARC_TRACE=1, one line on stderr at exit
    `arc-workspace/2`                arc workspace list|inbox --json
    `arc-workspace-backlog/19`       arc workspace backlog --json
    `arc-workspace-report/3`         arc workspace report|watch; optional projects[].library
    `arc-workspace-inventory/2`      arc workspace inventory --json
    `arc-workspace-pins/1`           arc workspace pins --json
    `arc-workspace-retirement/1`     arc workspace retire|restore --json
    `arc-writability/1`              arc config --check-writable --json
    `arc-sandbox-clone/1`            arc sandbox clone --json
    `arc-sandbox-diff/1`             arc sandbox diff --json
    `arc-replica-id/1`               arc replica id --json
    `arc-replica/2`                  arc replica status --json
    `arc-journal-questions/4`        arc journal questions --json
    `journal-discussion/5`           arc journal discussion --json
    `journal-source/2`               arc journal source --json
    `arc-journal-filed/1`            arc journal filed --json
    `arc-journal-latest/1`           arc journal latest --json
    `arc-journal-scaffolds/1`        arc journal scaffolds --json
    `arc-checkpoint/1`               arc checkpoint show --json
    `arc-checkpoint-list/1`          arc checkpoint list --json

  Read views with `review_context` carry target revisions and author
  coordinates computed at projection time. Verdict and audit entries may carry
  `tiebreak_reviewer`, the dated policy entry elected when the read was recorded,
  and `tiebreak_target`, the destination whose policy elected it. An absent
  entry or destination grants no exception. Store format 7 accepts this optional read
  metadata alongside records without it. Verdict and audit entries also carry
  optional `basis_time`, `basis_source` (`session-reply`, `declared` or
  `recorded`) and `basis_version`, preserved in bundles and replica events.
  `basis_version` names the recorder's applicable version; `review_context`
  judgments name the receiver's version at that time. An absent basis time
  is legacy evidence, inferred from `created_at`. Receipt items accept optional
  RFC 3339 `judged_at`; its bounds and reply precedence are defined by
  `arc guide debt-clearance`. These optional input fields leave older records
  valid under store format 7.

  Commitments, files:
    `arc-bundle/6`                   arc export / arc import
    `arc-bundle-objects/1`           arc export --objects-json
    `arc-custody-bundle/1`           arc export / import --custody-dir
    `arc-forge-delivery/1`           arc message --delivery-file
    `arc-disclosure/1`               arc context declare --disclosure-file
    `arc-forge-observation/1`        arc forge observe --file
    `arc-forge-receipt/1`            arc forge observe|link|checks|pr-state --json
    `arc-review-receipt/1`           arc receipt template; arc review|audit --receipt
    `arc-received-evidence/1`        arc candidate register --evidence-file
    `arc-replica-bundle/3`           arc replica export / import
    `arc-replica-event/3`            one event inside an arc-replica-bundle
    `arc-journal-bundle/2`           arc journal export / import
    `journal-events/2`               events.jsonl, streamed by arc journal events
    `arc-workspace-feed/1`           <ai-home>/arc/feed/events.jsonl (arc guide workspace)
    `arc-journal-spool/2`            .arc/outbox/<ts>-<kind>-<topic>.json
    `arc-trailer-edits/1`            arc rewrite trailers --edits
    `arc-workspace-watch-request/1`  arc workspace watch, one per stdin line
    `arc-correspondence/1`           arc close --correspondence
    `arc-review-ladder/1`            .arc/review-ladder.toml; review-ladder import|show --json

  `arc cost evidence` writes `tapes-attribution-evidence/1`, a format tapes
  owns and versions; this rule counts the shapes arc defines, so that file
  takes no arc schema string.

  Imports accept `arc-bundle/5` and `arc-replica-bundle/2` alongside
  current export formats. Journal imports accept
  `arc-journal-bundle/1`.
  Their absent model provenance stays absent.

  A journal event omits the harness or session key nobody established.
  Owned claim and lane lifecycle events require their complete identities;
  an event lacking one is unsupported and cannot change ownership state.
  `journal-events/2` covers that and the `evidence`, `relocated`,
  `redacted`, `pinned`, `unpinned`, and `published` kinds; `journal-events/1`
  events, which carry both keys, are read alongside it, and a value of
  `unknown` in either reads as absent. A journal bundle and a journal spool
  (`arc-journal-spool/1` or current) carry events of either version. A reader
  of an older version skips an event it does not know, `journal doctor`
  reports that event as `unknown-jsonl-event`, and journal import holds back
  each artifact carrying an unsupported event and its dependents while
  admitting unrelated understood artifacts.

  Private successor preparation uses complete Markdown at
  `.transitions/<successor>`, retained until source retirement. These files
  are recovery evidence, separate from versioned journal events.

  Internal:
    store format 8                   .git/arc/config.json and the change ledger
    `arc-accept-checkout/1`           local administrative allocation recovery
    `arc-landing-intent/1`            landing-intents/<attempt>.json recovery
    `arc-import-batch/1`             local atomic change import and authorization
    `arc-replica-import/3`           receipt of an imported replica bundle
    `arc-journal-exchange-import/1`  receipt of an imported journal bundle
    `arc-sandbox/2`                  .arc-sandbox.json
    `arc-slot-pool/1`, `arc-slot/1`  a slot pool's pool.json and slot-NN.json
    assurance blame cache            <ledger>/cache/assurance/<change>.json
    Codex model checkpoint format 1  <ai-home>/arc/cache/model-lookups/<key>.json
    `journal-binding/1`              bindings.jsonl

WHAT ARC WILL NOT DO
  Ledger and repository:
    arc never deletes or rewrites a ledger event file.
    arc rewrites a branch only through `arc rewrite`, which records the map
      it produced, and merges only through `arc integrate` when every
      required gate is green.
    arc never force-removes a checkout it did not create. Worktree removal
      refuses while dirty or untracked content is present, `fork retire
      --force` is the operator's decision to get past that. The
      scratch checkouts arc force-removes are those it creates to evaluate
      a merge or run a baseline acceptance probe.
    arc never runs `git rebase --abort`.
    arc never installs a Git hook silently.
    arc never makes a network call. Every transport it offers is a file the
      caller moves: change and replica bundles, journal bundles, and the
      tapes records `arc context read --from-tapes` is handed. On its own
      behalf arc runs `git`, the signing program Git is configured with,
      and local probes (`df`, `du`, `findmnt`, the installed `arc
      --version`); a declared gate, renderer, hook, or intake command is
      the project's or its caller's.
    arc never refuses to create a worktree over disk space.
    arc records no configuration history, only the inputs to one
      irreversible decision.

  Refusals worth knowing before they happen:
    arc refuses a change or replica bundle written by a newer arc rather
      than skipping lifecycle events it does not know. A journal import holds
      back each artifact carrying an unsupported event and reports its kind
      and schema, while admitting the understood reference closure.
    `arc import` reads `arc-bundle/6` and `arc-bundle/5`.
      Absent optional fields stay absent; other bundle schemas are refused.
    arc refuses a gate run only when the change's recorded worktree is
      missing or its HEAD is not the branch head. A run started from another
      checkout of the repository is redirected to the recorded worktree and
      says so.
    arc refuses a `--from-journal` source that is missing, non-actionable,
      already consumed, or an incompletely published transition successor.
    arc refuses a delivery naming an audience that cannot settle the
      question.
    arc refuses a rewrite map claiming a revision survives as a commit this
      repository does not hold.

  Judgements arc does not make:
    arc records declared identities and observed models, and holds no
      routing opinion.
    arc records the displaced owner and the reason a claim was taken over.
    arc never scores the coordinates a debt records, joins them against a
      roster, or orders two models against each other.
    arc never infers an identity from a branch or a directory name.
    arc never claims a recipient read anything.

  `arc <command> --help` prints that command's full contract.
  `arc --help` indexes root commands.
"#;

const IDENTITY_OPTIONS: &str = r#"GLOBAL OPTIONS
  --actor
    Acting identity, from ARC_ACTOR when the flag is absent. Falls back to
    <harness>:<session> when both are known, else git user.name; arc
    records either as an identity nobody declared. A write refuses when
    none of these names one, and refuses a declared placeholder (unknown,
    none, n/a, ...) or a harness:model form carrying one; a
    harness:session form is an actor. A read proceeds without one

  --harness
    Harness label, e.g. claude, codex, opencode. A write refuses a
    placeholder such as unknown; omit a harness nobody established

  --session
    Native session ID of the acting harness thread. A write refuses a
    placeholder such as unknown; omit a session nobody established

  --session-link
    Private web link for the acting session; recorded only in arc events

  --model
    Declare a model slug with optional #effort. Without this flag or
    ARC_MODEL, each invocation resolves the acting session store once when
    it writes. A corroborated Claude recording without a model selection
    is re-read every 50 ms for up to 1 second; other store answers return
    immediately. Declarations retain any observed disagreement and its
    coordinate. Model and supplied effort segments must be nonempty.
    A write refuses a placeholder model or effort (unknown,
    none, n/a, ...) and a session id as the model, whether flagged or in
    ARC_MODEL: drop the declaration and arc records the observed model

  --on-behalf-of
    Subject a lead runs delegated ceremony for; recorded beside the
    invoker. A write refuses a placeholder or a harness:model form carrying
    one. A session subject may declare harness:id:model[#effort]; an empty
    declared model or effort refuses even with --model. Recording another
    session's review, add --model with that session's model: the event's
    model is what arc judges independence from

  --role
    Execution boundary: implementer | reviewer | lead

  --change
    Change to act on, wherever the positional is optional

  --sandbox
    Absolute prefix that stands in for the home directory: every root arc
    writes by default (ledger under a data root, journal, registry,
    configuration, worktrees, temp) lives beneath it

TRACING
  ARC_TRACE=1 writes one `arc-perf/1` JSON line to stderr as arc exits,
  whatever the outcome: a status the command chose, an error, refused
  arguments, an interrupt or a panic. Stdout, the exit status and the
  ledger are unchanged, and nothing is written anywhere else. Any other
  value, or none, traces nothing.

  The record names the subcommand path (`command`, null when arguments were
  refused before one was selected), the build, `outcome` (exited, error,
  usage, interrupted, panicked), `exit_code` and `elapsed_ms`. It carries no
  path, session id or argument value.

  Counters are logical, one per call arc makes, not syscalls:
    ledger.<namespace>   for change, repository and import files: directory
                         enumerations and entries listed, opens attempted and
                         opened, bytes, files decoded, events typed, and
                         shared_hits served by an earlier read in the process
    counts               repository_materializations, changes_reduced,
                         scope_derivations, receipt_validations,
                         replica_materializations, status_reports,
                         journal_projections; session_candidates and
                         session_rows, the recordings session discovery's
                         backends scanned and the project's rows they kept
    cache.<value>        hits and misses of a value read once and reused:
                         for the process, or while it holds a lock
    children             runs by kind: the program name, and Git's subcommand;
                         `ms` is time spent waiting on a child arc ran to
                         completion, and a child it only started has a run
                         and no time
  `phases` holds the calls and exclusive milliseconds of enumerate, read,
  decode, reduce, scope, round_trip, journal, sessions, listing and
  children. `sessions` is session discovery as a whole and `listing` one
  call per backend listing its store, each on a thread of its own. A phase
  entered inside another on its thread is subtracted from the outer one;
  phases on concurrent threads are summed, so they can total more than
  elapsed_ms; time outside every phase is not attributed.

  `coverage` names what the counters see. `ledger: store` counts ledger
  files read through the store's readers, not a file a diagnostic opens on
  its own; `children: arc` counts processes arc starts, not those a library
  starts for it, such as session discovery's.
"#;

const JOURNAL_RESOLUTION: &str = r#"JOURNAL DIRECTORY RESOLUTION
  Read a known artifact with `arc journal show <file.md>`; use `arc journal
  dir` only when a filesystem path is required. `arc journal dir --explain`
  reports the source and stable anchor; `--archive` selects the cold sibling.

  Resolution takes the first source that answers:
  1. ARC_JOURNAL_DIR, an explicit directory override.
  2. The longest [journals.dirs] path-prefix matching the repository root,
     or the current directory outside a repository.
  3. <ai-home>/journals/<repo-path-slug> for a Git repository.
  4. A default-root journal whose bindings.jsonl records this directory,
     or the journal named by this directory's slug.
  A prefix covering repositories shadows their Git journals.

  `arc journal rebind <journal-dir>` adopts an existing journal and records
  the move to this project's anchor. It refuses when both journals hold
  content. Read `arc journal rebind --help` for its input contract.
"#;

pub const MAP: &str = r#"arc keeps the shared account of agentic work over plain Git: what is in progress,
what was decided, what remains, and what is safe to integrate.

Git owns content, branches and history. Arc owns changes, patchsets, the journal,
findings, verdicts, gate evidence and guarded integration.

SAY WHO YOU ARE
  eval "$(arc env)"              before writing, in every shell
  Leave out any value nobody established. Details: arc guide identity

FIND YOUR BEARINGS
  arc catchup                    inside a project; then read the memories it names
  arc workspace backlog --here   above several projects
  arc journal open               work waiting for a session
  arc resume <change>            one change's brief, state and journal context
  arc rescue <change>            where another session stopped; --take to take it over
  arc journal show <file.md>     a known artifact, live then archived; --global for
                                 operator records
  arc journal dir                only when a path itself is needed

RUN A CHANGE
  arc begin <slug>               open it
  arc brief <change>             record what it must do, then edit and commit
  arc snapshot                   record the patchset
  arc gates; arc gates measure   inspect gate costs; measure without evidence
  arc verify --all               run the gates and record the evidence
  arc check                      preflight integration
  arc integrate                  land it once review is settled
  arc land <change>...           land changes in one transaction
  arc close <change> --abandoned   stop without landing
  Readiness is computed, not declared: nothing marks a change ready.

REVIEW
  Read the diff and record your verdict with arc review. If you contributed, or
  policy refuses your approval, record arc debt instead. Ask for an independent
  reviewer only when policy or someone requires one.
  A path policy declares dangerous needs somebody else's verdict, or a debt that
  stays owed.
  An approval binds its exact patchset, so a new commit needs a new snapshot and a
  fresh verdict. Gates must cover the tree that will ship.
  What is owed: arc inbox, arc query --debt. Discharge it with arc audit.
  Findings: arc findings <change>, with --audit after integration; arc resolve
  disposes of one.
  A read-only reviewer's reading: arc receipt template, then --receipt.
  arc review-ladder              import or inspect dated capability tables.

WORK BESIDE OTHERS
  arc claim takes work. Each writer uses its own branch or worktree.
  Snapshots attribute completed bound executor work; `arc guide debt-review`
  defines the evidence. Override with --contributors, --solo or --on-behalf-of.
  arc roles records planners and orchestrators.
  ARC_ROLE bounds a delegated run. Integration belongs to the lead.

KEEP WHAT YOU LEARN
  arc keep                       a checked premise or rejected approach, as it arises
  arc journal todo, arc fr       separate work, filed when you notice it
  arc journal memory             project facts; --global for operator practices

END A SESSION
  arc journal handoff <topic> --derive   for unfinished work
  arc journal conclusion <topic>         for a completed outcome
  arc checkpoint record <name>           a revision handed over for testing
  Release your claims when you stop.

GO DEEPER
  arc guide                      every topic and command contract
  arc guide <topic>              one topic
  arc guide --search "TEXT"      literal, case-insensitive search
  arc guide --all                the full workflow depth
  arc <command> --help           the full contract; -h is its summary
  arc --help                     every root command
  arc instructions git           contribution rules for Git
  Topics: overview identity workspace orient sandbox questions change delivery
  replicas journal ending profiles forge debt assurance repair contributions
  rewrites history candidates context rules cost probes cleanup exit-codes files
  gates slots schemas boundaries
"#;

pub fn print() {
    print!("{MAP}");
}

/// Instruction routes follow already-established blockers and never affect readiness.
pub(crate) fn blocker_pointers(blockers: &[crate::blockers::Blocker]) -> String {
    use crate::blockers::Blocker;
    let mut topics = Vec::new();
    for blocker in blockers {
        let topic = match blocker {
            Blocker::IntegrationMode => "replicas",
            Blocker::NeedsRebase => "change-integration",
            Blocker::LandingPending | Blocker::LandsWith => "change-landing",
            Blocker::MergedTreeUnevaluated | Blocker::GatesNotGreen => "rules-evidence",
            Blocker::BlockingFindings => "repair",
            Blocker::NoValidApproval => "debt",
            Blocker::AcceptanceProbesNotGreen => "probes",
            Blocker::Closed
            | Blocker::BranchMissing
            | Blocker::TargetUnreadable
            | Blocker::ForkBranch
            | Blocker::Iterating
            | Blocker::BlockedByChanges
            | Blocker::HoldActive => "change",
        };
        if !topics.contains(&topic) {
            topics.push(topic);
        }
    }
    if topics.is_empty() {
        return String::new();
    }
    format!(
        "Read: {}\n",
        topics
            .iter()
            .map(|topic| format!("arc guide {topic}"))
            .collect::<Vec<_>>()
            .join("; ")
    )
}

fn read_only_commands() -> String {
    let mut lines: Vec<String> = Vec::new();
    for name in crate::commands::READ_ONLY_COMMANDS {
        match lines.last_mut() {
            Some(line) if line.len() + 2 + name.len() <= 76 => {
                line.push_str("  ");
                line.push_str(name);
            }
            _ => lines.push(format!("    {name}")),
        }
    }
    lines.join("\n")
}

const TOPIC_LIMIT: usize = 16_384;

struct Section {
    name: &'static str,
    family: &'static str,
    title: &'static str,
    start: &'static str,
}

const SECTIONS: &[Section] = &[
    Section { name: "overview", family: "overview", title: "Arc and Git ownership", start: "arc — persistent context and guarded workflow state over plain Git for agentic coding arcs." },
    Section { name: "identity-basics", family: "identity", title: "Attribution and model evidence", start: "SAY WHO YOU ARE (before the first write)" },
    Section { name: "identity-events", family: "identity", title: "Event ordering, cursors and read-only commands", start: "  Local change, repository and replica IDs are totally ordered by issuance" },
    Section { name: "identity-sessions", family: "identity", title: "Harness and session detection", start: "  Claude Code's Remote Control supplies `CLAUDE_CODE_BRIDGE_SESSION_ID` to" },
    Section { name: "workspace", family: "workspace", title: "Workspace orientation", start: "FROM A WORKSPACE ROOT (outside a project)" },
    Section { name: "orient", family: "orient", title: "Project orientation and plans", start: "ORIENT INSIDE A PROJECT (start here, in this order)" },
    Section { name: "journal-lookup", family: "journal", title: "Artifact lookup, archival and project queues", start: "  Shelve a discussion with journal archive FILE --unresolved --note REASON." },
    Section { name: "sandbox", family: "sandbox", title: "Destructive rehearsals", start: "SANDBOX (rehearse something destructive)" },
    Section { name: "questions", family: "questions", title: "Discussions, questions and decisions", start: "SETTLE A QUESTION (before it is work)" },
    Section { name: "change-commands", family: "change", title: "Change lifecycle commands", start: "RUN A CHANGE" },
    Section { name: "change-bundles", family: "change", title: "History export, objects and custody", start: "  Change history checksums hash compact JSON with recursively sorted object" },
    Section { name: "change-gates", family: "change", title: "Gate execution and atomic review input", start: "  A gate runs where its change lives. `verify`, `snapshot --verify`, `done`," },
    Section { name: "probes", family: "probes", title: "Acceptance probes and brief bases", start: "  Each acceptance probe declared on the patchset's brief blocks readiness" },
    Section { name: "change-integration", family: "change", title: "Replay, integration shapes and shipped evidence", start: "  `arc rebase` is what `check` names when the selected integration shape" },
    Section { name: "change-landing", family: "change", title: "Landings and must-land-together relations", start: "  Some changes must reach their target together or not at all. `arc metadata" },
    Section { name: "cleanup", family: "cleanup", title: "Integration cleanup and retained checkouts", start: "  `integration.cleanup` governs branch and worktree cleanup after integration" },
    Section { name: "change-finalization", family: "change", title: "Retention, checkouts and release copy", start: "  Integration prints the integrated revision and closure event before" },
    Section { name: "delivery", family: "delivery", title: "Preparing delivered changes", start: "PREPARE A DELIVERED CHANGE" },
    Section { name: "replicas", family: "replicas", title: "Replica pairing, authority and publication", start: "PAIR REPLICA STORES" },
    Section { name: "journal-records", family: "journal", title: "Keeping facts during work", start: "KEEP WHAT THE WORK DISCOVERS (mid-change, before it is lost)" },
    Section { name: "journal-references", family: "journal", title: "Pinned references, living documents and published pages", start: "STANDING REFERENCES (what a project keeps coming back to)" },
    Section { name: "ending", family: "ending", title: "Session endings and global memory", start: "END A SESSION (what the next one reads)" },
    Section { name: "profiles", family: "profiles", title: "Change profiles", start: "PROFILES (--profile, default local)" },
    Section { name: "forge-delivery", family: "forge", title: "Forge identity, delivery and disclosure", start: "FORGE" },
    Section { name: "forge-observations", family: "forge", title: "Observations, custody and evidence origins", start: "  forge observe CHANGE --file frontier.json [--json] admits an adapter's" },
    Section { name: "debt-review", family: "debt", title: "Review authority, independence and policy", start: "REVIEW COVERAGE AND DEBT" },
    Section { name: "debt-obligations", family: "debt", title: "Debt declarations, coverage and corroboration", start: "  A debt is declared with `arc debt <change> --reason <why>`, or at the merge" },
    Section { name: "debt-recovery", family: "debt", title: "Owed work, audits and findings after integration", start: "  Coming back to owed work:" },
    Section { name: "debt-clearance", family: "debt", title: "Dated ladder capability and clearance", start: "REVIEW CLEARANCE" },
    Section { name: "assurance", family: "assurance", title: "Review assurance and staleness", start: "REVIEW ASSURANCE" },
    Section { name: "repair", family: "repair", title: "Finding repair and bounded rounds", start: "REVIEW AND REPAIR" },
    Section { name: "contributions", family: "contributions", title: "Contribution roles and canonical Git trailers", start: "CONTRIBUTION TRAILERS" },
    Section { name: "rewrites-operations", family: "rewrites", title: "Signing, trailer edits and tags", start: "HISTORY REWRITES" },
    Section { name: "rewrites-evidence", family: "rewrites", title: "Recorded revisions and atomic rewrite maps", start: "  A commit that any recorded verdict or audit read, that a patchset recorded" },
    Section { name: "history", family: "history", title: "The history model", start: "THE HISTORY MODEL" },
    Section { name: "candidates", family: "candidates", title: "Registration, received evidence, selection and promotion", start: "CANDIDATES" },
    Section { name: "context", family: "context", title: "Reads, declarations and provenance", start: "CONTEXT" },
    Section { name: "rules-claims", family: "rules", title: "Concurrent claims, lanes and recovery", start: "RULES THAT CHANGE WHAT YOU DO" },
    Section { name: "rules-evidence", family: "rules", title: "Trees, attribution and acceptance evidence", start: "  - Gate evidence binds to a tree, not to a commit. A change sitting behind" },
    Section { name: "rules-journal", family: "rules", title: "Journal ownership, events and spool safety", start: "  - The journal lives outside the repo, so worktrees stay clean. Cross-session" },
    Section { name: "rules-runs", family: "rules", title: "Dispatches, executor bindings and native children", start: "  - `arc run dispatch` names exactly one subject: `--change <id>`, `--fork" },
    Section { name: "cost-measurement", family: "cost", title: "Selection, measurement and pricing", start: "  - `arc cost show <change|journal-file>... [--json]` reports what the executor" },
    Section { name: "cost-accounting", family: "cost", title: "Counters, coverage and accounting bases", start: "    A harness's native counters add only within that harness: a raw input sum across" },
    Section { name: "cost-spans", family: "cost", title: "Session spans, allocation and estimates", start: "    A recording beginning before its session's first dispatch has" },
    Section { name: "cost-identity", family: "cost", title: "Lineage, participants, liveness and child families", start: "    Lineage relatives are plain session ids. A non-id reference makes" },
    Section { name: "cost-selection", family: "cost", title: "Range and footprint selection", start: "  - `arc cost show` selects the union of its positional targets, change ids" },
    Section { name: "cost-retention", family: "cost", title: "Retained usage facts", start: "  - `arc cost retain <change|journal-file>` keeps, per bound or participating" },
    Section { name: "cost-commits", family: "cost", title: "Commit accounting and Git notes", start: "  - `arc cost show --commit <rev> [--json]` answers for a commit, which has" },
    Section { name: "cost-evidence", family: "cost", title: "Portable attribution evidence", start: "  - `arc cost evidence <change|journal-file>... | --all` writes the evidence" },
    Section { name: "rules-rounds", family: "rules", title: "Round endings and execution roles", start: "  - A bounded round records what it deliberately left, so the next round" },
    Section { name: "exit-codes", family: "exit-codes", title: "Exit codes and blocker precedence", start: "EXIT CODES" },
    Section { name: "files", family: "files", title: "Configuration, build storage and retention", start: "FILES" },
    Section { name: "gates", family: "gates", title: "Gate declarations and selection", start: "GATES" },
    Section { name: "slots", family: "slots", title: "Leased fixed-path checkouts kept across changes", start: "SLOTS" },
    Section { name: "schemas", family: "schemas", title: "Versioned surfaces", start: "SCHEMAS" },
    Section { name: "boundaries", family: "boundaries", title: "Authority and effect boundaries", start: "WHAT ARC WILL NOT DO" },
];

struct Topic<'a> {
    name: &'static str,
    family: &'static str,
    title: &'static str,
    text: &'a str,
}

fn topics(text: &str) -> Vec<Topic<'_>> {
    let offsets: Vec<_> = SECTIONS
        .iter()
        .map(|section| {
            text.lines()
                .scan(0, |offset, line| {
                    let at = *offset;
                    *offset += line.len() + 1;
                    Some((at, line))
                })
                .find(|(_, line)| *line == section.start)
                .expect("compiled semantic section exists")
                .0
        })
        .collect();
    let mut topics: Vec<_> = SECTIONS
        .iter()
        .enumerate()
        .map(|(i, section)| Topic {
            name: section.name,
            family: section.family,
            title: section.title,
            text: &text[offsets[i]..*offsets.get(i + 1).unwrap_or(&text.len())],
        })
        .collect();
    topics.push(Topic {
        name: "identity-options",
        family: "identity",
        title: "Global options",
        text: IDENTITY_OPTIONS,
    });
    topics.push(Topic {
        name: "journal-resolution",
        family: "journal",
        title: "Journal directory resolution and anchors",
        text: JOURNAL_RESOLUTION,
    });
    topics
}

fn families(topics: &[Topic<'_>]) -> Vec<&'static str> {
    let mut families = Vec::new();
    for topic in topics {
        if !families.contains(&topic.family) {
            families.push(topic.family);
        }
    }
    families
}

fn family_title(name: &str) -> &'static str {
    match name {
        "identity" => "Identity, attribution and sessions",
        "journal" => "Journal lookup, records, references and resolution",
        "change" => "Running a change, from gates to retention",
        "forge" => "Forge delivery and observations",
        "debt" => "Review authority, debt and recovery",
        "rewrites" => "History rewrites and their evidence",
        "rules" => "Claims, evidence, journal, run and round rules",
        "cost" => "Work cost measurement and accounting",
        _ => "Topics",
    }
}

fn family_text(name: &str, topics: &[Topic<'_>]) -> String {
    let mut text = format!("{}\n", family_title(name));
    for topic in topics.iter().filter(|topic| topic.family == name) {
        text.push_str(&format!("  arc guide {:<30} {}\n", topic.name, topic.title));
    }
    text
}

fn topic_routes(topics: &[Topic<'_>]) -> Vec<(String, String)> {
    let mut routes: Vec<_> = topics
        .iter()
        .map(|topic| {
            (
                format!("arc guide {}", topic.name),
                format!("{}\n\n{}", topic.title, topic.text),
            )
        })
        .collect();
    for family in families(topics) {
        if !topics.iter().any(|topic| topic.name == family) {
            routes.push((format!("arc guide {family}"), family_text(family, topics)));
        }
    }
    routes
}

fn instruction_commands(mut command: clap::Command, path: &str, out: &mut Vec<(String, String)>) {
    command = command.mut_args(|arg| arg.hide_env_values(true));
    command.build();
    for child in command.get_subcommands_mut() {
        *child = child.clone().mut_args(|arg| arg.hide_env_values(true));
    }
    out.push((
        format!("{path} --help"),
        command.clone().render_long_help().to_string(),
    ));
    for child in command
        .get_subcommands()
        .filter(|child| child.get_name() != "help")
    {
        instruction_commands(child.clone(), &format!("{path} {}", child.get_name()), out);
    }
}

/// Every command repeats the shared options, so their rows are not search results.
const SHARED_OPTION_POINTER: &str = "see `arc guide identity-options`";

fn context(text: &str, needle: &str) -> Option<String> {
    let folded = text.to_lowercase();
    let offset = folded.find(needle)?;
    let line = folded[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let compact = text
        .lines()
        .skip(line)
        .take(3)
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = compact.chars();
    let mut snippet: String = chars.by_ref().take(140).collect();
    if chars.next().is_some() {
        snippet.push('…');
    }
    Some(snippet)
}

pub fn show(
    topic: Option<&str>,
    all: bool,
    search: Option<&str>,
    command: clap::Command,
) -> anyhow::Result<()> {
    let text = GUIDE.replace("{READ_ONLY_COMMANDS}", &read_only_commands());
    let topics = topics(&text);
    if all {
        for topic in &topics {
            print!("{}", topic.text);
        }
        return Ok(());
    }
    let routes = topic_routes(&topics);
    if let Some(search) = search {
        anyhow::ensure!(
            !search.is_empty(),
            "search text must not be empty; use `arc guide` for the index"
        );
        let needle = search.to_lowercase();
        let mut routes = routes;
        instruction_commands(command, "arc", &mut routes);
        let mut found = false;
        for (route, body) in routes {
            let body: String = body
                .lines()
                .filter(|line| !line.contains(SHARED_OPTION_POINTER))
                .fold(String::new(), |text, line| text + line + "\n");
            if let Some(context) = context(&body, &needle) {
                println!("{route}: {context}");
                found = true;
            }
        }
        if !found {
            println!("No instruction matches; use `arc guide` for the index.");
        }
        return Ok(());
    }
    if let Some(name) = topic {
        if let Some((_, body)) = routes
            .iter()
            .find(|(route, _)| route == &format!("arc guide {name}"))
        {
            anyhow::ensure!(
                body.len() <= TOPIC_LIMIT,
                "compiled topic exceeds its byte budget"
            );
            print!("{body}");
            return Ok(());
        }
        anyhow::bail!("unknown guide topic {name:?}; use `arc guide` for the index");
    }
    println!("Workflow topics:");
    let width = topics
        .iter()
        .map(|topic| topic.name.len())
        .max()
        .unwrap_or(0)
        + 2;
    for family in families(&topics) {
        let standalone = topics.iter().any(|topic| topic.name == family);
        if !standalone {
            println!("  arc guide {family:<width$}  {}", family_title(family));
        }
        for topic in topics.iter().filter(|topic| topic.family == family) {
            if standalone {
                println!("  arc guide {:<width$}  {}", topic.name, topic.title);
            } else {
                println!(
                    "    arc guide {:<child$}  {}",
                    topic.name,
                    topic.title,
                    child = width - 2
                );
            }
        }
    }
    println!("\nBy task:");
    println!("  Record a patchset       arc snapshot --help");
    println!("  Preflight integration   arc check --help");
    println!("  Claim work              arc claim --help");
    println!("  Project records         arc journal --help");
    println!("  Review owed             arc debt --help, arc audit --help, arc query --help");
    println!("  Read findings           arc findings --help");
    println!("  Contribution rules      arc instructions git");
    println!("\nEverything at once: arc guide --all");
    println!("Literal search:     arc guide --search TEXT");
    println!("\nCommand contracts:");
    let mut routes = Vec::new();
    instruction_commands(command, "arc", &mut routes);
    for (route, _) in routes {
        println!("  {route}");
    }
    Ok(())
}

#[cfg(test)]
mod topic_tests {
    use super::*;

    /// Each original byte has one semantic owner; an accidental gap or overlap loses or duplicates depth.
    #[test]
    fn semantic_sections_partition_full_depth_and_obey_the_topic_budget() {
        let text = GUIDE.replace("{READ_ONLY_COMMANDS}", &read_only_commands());
        let topics = topics(&text);
        assert_eq!(
            topics[..SECTIONS.len()]
                .iter()
                .map(|topic| topic.text)
                .collect::<String>(),
            text
        );
        for (route, body) in topic_routes(&topics) {
            assert!(body.len() <= TOPIC_LIMIT, "{route}: {} bytes", body.len());
        }
        for family in families(&topics) {
            assert!(
                MAP.split_whitespace().any(|word| word == family),
                "map omits {family}"
            );
        }
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::collections::BTreeSet;

    fn instruction_stack(work: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(16 << 20)
            .spawn(work)
            .unwrap()
            .join()
            .unwrap();
    }

    fn command_routes(command: &clap::Command, path: &str, paths: &mut BTreeSet<String>) {
        paths.insert(format!("{path} --help"));
        for child in command
            .get_subcommands()
            .filter(|child| child.get_name() != "help")
        {
            command_routes(child, &format!("{path} {}", child.get_name()), paths);
        }
    }

    /// Every operation has an indexed help route; parsing help proves reachability without executing it.
    #[test]
    fn command_index_reaches_every_leaf_without_dispatch() {
        instruction_stack(|| {
            let mut command = crate::cli_command();
            command.build();
            let mut expected = BTreeSet::new();
            command_routes(&command, "arc", &mut expected);
            let mut rendered = Vec::new();
            instruction_commands(command.clone(), "arc", &mut rendered);
            let actual: BTreeSet<_> = rendered.iter().map(|(route, _)| route.clone()).collect();
            assert!(expected.len() > 200);
            assert_eq!(actual, expected);
            for (route, _) in rendered {
                let error = command
                    .clone()
                    .try_get_matches_from(route.split_whitespace())
                    .unwrap_err();
                assert_eq!(error.kind(), ErrorKind::DisplayHelp, "{route}: {error}");
            }
        });
    }

    /// A printed command or topic typo must fail in the command tree before any mutation can run.
    #[test]
    fn printed_instruction_routes_resolve_in_the_command_tree() {
        instruction_stack(|| {
            let mut command = crate::cli_command();
            command.build();
            let text = GUIDE.replace("{READ_ONLY_COMMANDS}", &read_only_commands());
            let topics = topics(&text);
            let mut surfaces = topic_routes(&topics);
            surfaces.push(("map".into(), MAP.into()));
            surfaces.push((
                "blocker pointers".into(),
                blocker_pointers(&crate::blockers::PRIORITY),
            ));
            instruction_commands(command.clone(), "arc", &mut surfaces);
            let topic_names: BTreeSet<_> = topic_routes(&topics)
                .into_iter()
                .map(|(route, _)| route.trim_start_matches("arc guide ").to_owned())
                .collect();
            fn flags(command: &clap::Command, names: &mut BTreeSet<String>) {
                for arg in command.get_arguments() {
                    if let Some(long) = arg.get_long() {
                        names.insert(long.to_string());
                    }
                    if let Some(aliases) = arg.get_visible_aliases() {
                        names.extend(aliases.iter().map(|name| name.to_string()));
                    }
                }
                for child in command
                    .get_subcommands()
                    .filter(|child| child.get_name() != "help")
                {
                    flags(child, names);
                }
            }
            let mut flag_names = BTreeSet::new();
            flags(&command, &mut flag_names);
            let mut checked = 0;
            let mut gaps = Vec::new();
            for (home, body) in &surfaces {
                let spans = body
                    .split('`')
                    .skip(1)
                    .step_by(2)
                    .filter_map(|code| code.trim().strip_prefix("arc "));
                let rows = body
                    .lines()
                    .filter_map(|line| line.trim().strip_prefix("arc "))
                    .filter(|tail| {
                        tail.split_whitespace().next().is_some_and(|word| {
                            command.find_subcommand(word).is_some() || word.starts_with(['<', '-'])
                        })
                    })
                    .map(|tail| tail.split("  ").next().unwrap_or(tail));
                for tail in spans.chain(rows) {
                    let compact = tail.split_whitespace().collect::<Vec<_>>().join(" ");
                    if let Err(reason) = validate_command(&command, &compact, &topic_names) {
                        gaps.push(format!("{home}: `arc {compact}`: {reason}"));
                    }
                    checked += 1;
                }
                for tail in body.split("--").skip(1) {
                    let flag: String = tail
                        .chars()
                        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
                        .collect();
                    if !flag.is_empty()
                        && flag
                            .chars()
                            .next()
                            .is_some_and(|ch| ch.is_ascii_alphabetic())
                        && !flag_names.contains(&flag)
                        // Git, tapes and agent-routing options, and the schematic --flag placeholder, are separate namespaces.
                        && ![
                            "flag",
                            "purpose",
                            "continue",
                            "recurse-submodules",
                            "no-checkout",
                            "onto",
                            "latest",
                            "notes",
                            "abort",
                            "ff-only",
                        ]
                        .contains(&flag.as_str())
                    {
                        gaps.push(format!("{home}: unknown flag --{flag}"));
                    }
                }
                for tail in body.split("arc guide ").skip(1) {
                    // A gap of further spaces ends a route and starts its description.
                    if tail.starts_with(' ') {
                        continue;
                    }
                    let name = tail
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(['`', '.', ',', ';', ')', ':']);
                    if name.starts_with(['<', '-', '(', '[']) || name.is_empty() {
                        continue;
                    }
                    assert!(topic_names.contains(name), "{home}: unknown topic {name}");
                }
            }
            assert!(checked > 300, "only {checked} command examples checked");
            assert!(gaps.is_empty(), "{}", gaps.join("\n"));
        });
    }

    fn validate_command(
        command: &clap::Command,
        tail: &str,
        topics: &BTreeSet<String>,
    ) -> Result<(), String> {
        let mut current = command;
        let mut words = tail.split_whitespace().peekable();
        let mut path = vec!["arc"];
        while let Some(word) = words.next() {
            let word = word.trim_matches(['[', ']', ',', '`']);
            if word == "--help" || word == "-h" {
                return Ok(());
            }
            if word == "<kind>" && current.get_name() == "journal" {
                current = current.find_subcommand("note").unwrap();
                path.push("note");
                continue;
            }
            if word.starts_with('<')
                || word.starts_with('"')
                || word.starts_with('\'')
                || word == "..."
            {
                continue;
            }
            if let Some(child) = current.find_subcommand(word) {
                current = child;
                path.push(word);
                if word == "guide" {
                    if let Some(name) = words.peek().filter(|name| !name.starts_with(['<', '-'])) {
                        if !topics.contains(*name) {
                            return Err(format!("unknown topic {name}"));
                        }
                    }
                }
                continue;
            }
            if word.starts_with('-') {
                for flag in word.split('|') {
                    let flag = flag
                        .split('=')
                        .next()
                        .unwrap_or(flag)
                        .trim_end_matches(['.', ':', ')']);
                    let found = current.get_arguments().find(|arg| {
                        flag.strip_prefix("--").is_some_and(|name| {
                            arg.get_long() == Some(name)
                                || arg
                                    .get_visible_aliases()
                                    .is_some_and(|aliases| aliases.contains(&name))
                        }) || flag.strip_prefix('-').is_some_and(|short| {
                            short.len() == 1
                                && arg.get_short().is_some_and(|ch| short.starts_with(ch))
                        })
                    });
                    let Some(arg) = found else {
                        return Err(format!("unknown flag {flag} on {}", path.join(" ")));
                    };
                    if arg.get_action().takes_values() && !word.contains('=') {
                        words.next();
                    }
                }
            } else if current.has_subcommands() && !word.contains('|') {
                return Err(format!("unknown root command {word}"));
            }
        }
        Ok(())
    }

    /// Child listings are single rows and long contracts remain available on each operation.
    #[test]
    fn help_summaries_and_inherited_environment_entries_are_concise() {
        instruction_stack(|| {
            fn walk(command: &clap::Command) {
                for arg in command
                    .get_arguments()
                    .filter(|arg| arg.get_env().is_some())
                {
                    assert!(
                        arg.is_hide_env_values_set(),
                        "{}: {}",
                        command.get_name(),
                        arg.get_id()
                    );
                }
                for child in command.get_subcommands() {
                    if let Some(about) = child.get_about() {
                        assert!(!about.to_string().contains('\n'), "{}", child.get_name());
                        if child.get_long_about().is_some_and(|long| long != about) {
                            assert!(
                                child.clone().render_help().to_string().len()
                                    < child.clone().render_long_help().to_string().len(),
                                "{}",
                                child.get_name()
                            );
                        }
                    }
                    walk(child);
                }
            }
            let mut command = crate::cli_command();
            command.build();
            walk(&command);
        });
    }
}
