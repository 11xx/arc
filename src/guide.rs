//! The bare-`arc` guide: what an agent needs to drive a change end to end
//! without reading anything else first.
//!
//! `--help` is the reference — every command, every flag. This is the
//! orientation: what arc owns, which commands matter in what order, and the
//! handful of rules that change what a session should do. Anything that
//! belongs to one command's contract stays in that command's `--help`.

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

  Omit a value nobody established; never declare a placeholder. A write
  refuses a harness, session, actor or on-behalf-of subject that is one
  (unknown, unset, none, null, n/a, na, tbd, undefined, ?, -, <synthetic>,
  synthetic), an actor of the form harness:model carrying one, and a model
  that is one or is a session id. Reads and Git hooks never refuse.
  Agent harness, model and supplied effort segments must be nonempty.

  Text views validate recorded producer models. A missing model, a placeholder
  or a session id prints as `<harness> (model not recorded)`, or
  `model not recorded` when no harness is established. Valid model and effort
  spellings retain their coordinates. JSON production identities, journal
  artifact and entry writers, filed and recent sessions, suggestions,
  verification stamps, and review coverage retain the raw model value and carry
  `model_recorded: false` when it names no model; valid models omit that flag.
  Workspace report debt rows retain the raw implementer value with the same
  optional flag. Review coverage retains the originating session and validates
  the model against it. Audit and verdict detail text validates models against
  their recorded sessions.

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
  that cursor, sorted within each poll. A restart with --since excludes older
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
  arc workspace report [--previous FILE] --json
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
                       `arc-workspace-report/3`.
  cd <anchor>          Enter one project named by the report, then orient there.

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
                         discarded, or stale. Staleness
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
                         counts. Retry the source transition before promotion.
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
                     open questions.
    known topic      `arc journal latest <topic> [--kind <kind>]`, matched
                     exactly.
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
                         `journal unarchive` before appending.
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
                         markers terminate an open fence. Question readers,
                         discussion tallies, and doctor share quotation rules.
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
  arc journal source --harness <h> --session <id> [--item-key <k>] [--json]
                                       What one recording already produced.
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

  An entry filed wrong is amended, never rewritten. `correct` replaces one
  field of one entry — the artifact's `title`, a position's `stance`,
  `option`, `ref`, `actor` or `model`, an answer's `option` — and `retract`
  withdraws a position or an answer with a reason. Both append a block naming
  what changed and leave the original where it was filed, so the artifact
  still reads as the argument that happened while the tally, the branches,
  the question queue, and the open queue read the value in force. A retracted
  position leaves the tally; a retracted answer reopens its question. Both are
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
                                     A snapshot grants no approval.
    --verify [--gate <name>...]      Run required gates, or selected gates.
                                     Selected names must be unique and declared;
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
    --skip-green                    Reuse passing evidence with --all at the
                                     head's tree, or --against at the merged
                                     tree, whichever commit it ran on.
                                     Only the newest evidence readiness
                                     counts there is a candidate: a gate
                                     reruns when it failed or carries
                                     neither tree nor tested_tree. The
                                     reuse records its tree. Replays of
                                     reuse events without a tree require
                                     matching revisions.
    --attest --environment <id>      Record a run arc did not perform, naming
                                     the environment it applies to.
    --falsified-by <id> --predicted <why>
                                     Name the failure this pass answers,
                                     storing its original event revision.
  arc done                           Snapshot, run every gate, print check state.
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
    --evidence-event <id>            Cite the verification run that justifies it.
  arc check                          Integration preflight; exit code names the blocker.
  arc integrate <c>...               Guarded integration once the gates are green.
    --shape ff-only|no-ff            Resolved style; built-in ff-only.
    <c> <c> <c>, or --tag <t>        Land a queue, in dependency order.
  arc export <change> --output <file|-> [--since <sha256>]
                                     Export a versioned event bundle and its
                                     Git objects, with a count by role.
    --objects-json <file>            Write an arc-bundle-objects/1 transport file.
    --custody-dir <dir>              Also write an arc-custody-bundle/1 sidecar
                                     of the retained forge content.
  arc import <file|-> [--dry-run]     Require every listed object locally before
                                     writing any events or refs. Missing OIDs and
                                     roles are reported; transport the objects first.
    --provenance-only                Import events despite missing objects,
                                     reporting the unavailable evidence.
    --custody-dir <dir>              Install a sidecar the bundle binds.

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
  including direct tree pins, and the commits refs/arc/retained holds for the
  exported change), `synthesized-merge` (verify --against),
  `disposition-commit`, `integration-commit`, `target-commit`, `content-tree`,
  `cited-blob`, `cited-commit`, `rewrite-source`, `rewrite-successor`, and
  `observed-commit` (a commit a forge fact records as the provider's own,
  listed only where the exporter holds it, so a commit never fetched here
  obliges no receiver). Unknown event payloads remain opaque. A transport
  publishes the listed objects and their Git reachability closure through scoped refs or a Git
  bundle before announcing the result. Bundles without the objects field
  retain advisory patchset-commit checks; /6 accepts that absent field.
  A bundle's rewrite events carry the retention refs the exporter created; an
  import holds each one it names for the imported change under
  `refs/arc/retained/` when the commit is here, and refuses a name that is no
  retention ref. Bundles whose rewrite events name none import unchanged. An
  import holds each `observed-commit` of the imported change that is here the
  same way, under the event that recorded it.

  Retained forge content travels beside the bundle as an optional sidecar,
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
  or SHA-256 from its entry. Verified files are copied into ledger custody
  unless already held, before the events that cite them are published, and
  the summary counts installed, held and unavailable content. Unavailable
  entries stay unavailable and the import says incomplete. Importing a bundle
  that binds a sidecar without one says how many references stay unavailable,
  which manifest digest the bundle binds and that --custody-dir installs it;
  `arc accept` says the same. Custody is reported installed only after the
  events are published. A failure installing it after publication says the
  events are published, and repeating the import finishes the installation
  without recording events, as does importing with a repaired sidecar. The
  export writes the sidecar beside its directory and moves it into place only
  once the bundle is written, so a failed export leaves neither a partial
  sidecar nor a bundle naming one. Neither export nor import initializes a
  replica.
  Full and delta histories use active, uncontradicted rewrites admitted with
  rewrite authority to prove a falsification's failure identity. A publisher
  lacking rewrite authority is refused; legacy transport without an
  authorization receipt supplies no such proof. Absent, withdrawn, unrelated
  maps and revisions that merely converge on one successor prove nothing.
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
  requires replay onto the target. A conflict stops it and leaves the rebase in progress — the partial
  resolution is yours, and aborting would throw it away — with the conflicting
  files and the commands that finish the replay. Resolve, `git rebase
  --continue`, then `arc snapshot --verify`. Over another actor's live claim
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

  The decision is one function of facts observed under the target and change
  locks: a plan naming the target revision, the approved head, the tree the
  merge must ship, and the basis to record, or a refusal. The merge runs from
  the plan and is confirmed against it. `integrate --dry-run --json` prints
  that plan, decided the same way, as `arc-integration-plan/1`. Handing it
  back with `integrate --expect-basis <file>` never changes the decision; it
  names what moved since — the approved head, the target revision, the gate
  or policy declarations, the approval, or provisional routing and prerequisites
  — in one line beside a refusal, or
  as a warning when the integration still proceeds.

  When no checkout holds the target, `integrate` takes over the change's own
  checkout if it still holds the change branch, checks the target out there,
  merges, and leaves the checkout on the target. The dry run says it would.
  Any other shape keeps the refusal naming the missing target checkout.

  `integration.cleanup` governs branch and worktree cleanup after integration
  (built-in false). `integrate --cleanup` enables it; `--no-cleanup` overrides
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
  branch. `arc check` prints the declaration and its reason; `--json` includes
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
  complete replacement file, written atomically after it exits 0.

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
  approval. Status and check separate head coverage from gating permission.
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
           integration facts into the PR.
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
  show, status, log and explain carry the same reconstruction facts.
  show --json and status keep recorded forge state in forge and current reads
  in forge_view. Exact provider lookup requires a recorded provider identity;
  a legacy link without one does not establish a provider coordinate.
  --observation names an exact event or read ID and freezes forge content and
  its acquisition-history frontier. Local rewrite resolution and current byte
  availability are separate reads. Change and repository event frontiers and
  reader options are included; explain --at bounds change knowledge, while
  repository history, rewrite resolution and retained bytes are current reads.
  Freshness names the last successful read, latest attempt, age and clock
  anomalies. A failed acquisition does not erase a successful retained read;
  recording time never establishes present provider agreement.
  --body emits exact verified retained bytes to stdout with metadata on stderr.
  JSON carries base64 bytes or null with an availability diagnostic. Missing
  bytes weaken only claims that need them; they imply neither empty content,
  zero cost nor a missing judgment. forge show records nothing and creates
  no ledger directories, refs, reports or snapshots.
  forge observe CHANGE --file frontier.json [--json] admits an adapter's
  arc-forge-observation/1 document. Admission never changes readiness. A valid
  failed or truncated read succeeds; invalid schemas, digests, references and
  generation relationships refuse before append. Identical read_id retries
  return the original event with already-recorded; conflicting content refuses.
  Import validates introduced facts before publication in their supplying
  history. Independent replicas can supply different events for the same read.
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
  atomically, and re-verifies the recorded length and sha256 on every read. A
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
  custody directory that cannot be read is the problem `custody-unreadable`.

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
  the patchset's approval anchor, which follows signature-only rewrites.
  A content or trailer rewrite keeps the open patchset's approval anchor;
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
  contributor set, and whether it was declared or synthesized. For a closed
  integrated change, it names the patchset recorded as shipped while
  `latest_patchset` remains the later history. `arc show` and `arc check` name
  them beside an approval rejection, and `arc catchup` repeats the subject
  beneath each open change and each integrated debt item.

  An event's effective author is its `--on-behalf-of` subject when set,
  otherwise its actor. A patchset's effective contributors are its recorded
  set when nonempty, otherwise its effective author alone.

  `arc status` and `arc review <change>` print the rule as `review_options`:
  for ordinary scope, `declare_debt` then `request_review`, with
  `next_action` `declare_debt`; a required-review or undetermined-danger
  change lists `request_review` alone. Reading the list writes nothing — the
  lead runs `arc debt` or `arc integrate --debt` itself. A current
  `changes-requested` or `comment-only` verdict is its own next action and
  offers no debt route: a waiver records a missing review, not a way past a
  refusal.

  Repairing a patchset's attribution happens only before any verdict:
  `snapshot --amend <ps> --contributors ...` replaces the whole set, and the
  refusal after a verdict or closure names the blocker. A run dispatch is
  activity, not authorship — it repairs no patchset.

  Independence is judged against the patchset a reviewer actually read, not
  against whatever is newest. Otherwise a later snapshot by somebody else
  would retroactively turn a self-review into an independent one.

  It is also a relation between declared identities. An approval is refused
  when its effective author matches a contributor on the patchset it approves,
  or when arc assumed the reviewing identity from git config or the harness
  session, which is nobody's claim and so cannot be the second party. An assumed authoring identity
  refuses nothing on its own: a reviewer that declared a different name is
  independent of it, before the merge and in an audit after it alike. Two
  assumed identities are refused, since neither side claimed anything. A
  reviewer arc cannot place that way is reported as unknown attribution rather
  than counted as independent or as self-review, whether it cast a verdict or
  only filed findings.

  Which changes need one is declared in `[danger] paths` in
  `.arc/policy.toml` or the operator policy at
  `<git-common-dir>/arc/operator-policy.toml`. Both path lists apply, so a
  path declared dangerous by either file is dangerous. Integration reads
  `.arc/policy.toml` from the change's target branch, wherever it is run. A change touching a declared
  path needs a verdict from somebody other than its author; elsewhere a
  self-recorded verdict satisfies the gate. Declare no danger paths and the
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
  A debt already in force routes to
  integration: status says integrate beside the flagged waiver, check drops
  the review-queue advisory, and the inbox keeps one lead row — until the
  head moves past the covered patchset, which requires a fresh snapshot. The
  debt can stand in for an absent verdict or rescue a self-approval rejected
  by repository policy. It binds to the exact patchset head declared, so new
  work needs a new
  declaration — it does not excuse the rest of the change's life. A waiver
  follows only signature-only rewrites. A message or content rewrite leaves
  the waiver stale even when the patchset's mapped head matches the branch;
  a fresh snapshot and declaration are required for that head. A refused
  `arc integrate --debt` keeps the debt it declared; declaring it again with
  the same reason, kind, and declarer against the same patchset and coverage
  records nothing and prints the debt already in force.

  A debt is a record, not a count. It carries what kind of deficit it is,
  what review the work did have and at what coordinates, and who planned and
  who implemented it:

    nothing-read             no verdict on any patchset of the change
    merge-resolution-unread  approved, then a resolution nobody read
    repair-unread            approved, then authored work nobody read
    contributor-only         verdicts on the shipped patchset, all its own
    independent-review       a read by somebody independent, unsupplied

  arc derives the kind from the ledger; `--kind <k>` on `arc debt` and on
  `arc integrate --debt` declares one instead, and a declared kind wins. Only
  the caller can say a resolution was what went unread, because the ledger
  sees a repair and a merge resolution the same way. The kind is the weight,
  carried as a label rather than a number: `arc query --debt` works through
  the list above in order, then by age, and every summary row splits its
  count by kind rather than reporting one total.

  Coverage names each verdict's reviewer, the model string it was cast under
  kept whole, the effort that string's trailing `#suffix` names, and the
  routing version `arc review --route-version` or `arc audit --route-version`
  declared. Production names who recorded the brief version the shipped work
  answered and who recorded the patchset. Arc records coordinates and holds
  no opinion: no score, no roster to join them against, and no ordering
  between two models.

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

  Any independent verdict on the revision that shipped, recorded after the
  debt was declared, discharges it — whether it came from `arc review` before
  the merge or `arc audit` after. The debt records that no verdict existed,
  not that one command must supply it. What the verdict concluded lives in
  the verdict and its findings; discharging the debt does not mean approval.

  An audit is a separate event from the verdict that shipped, so attaching one
  never rewrites what shipped with what review. An approving audit must come
  from an identity other than the author — otherwise the obligation would
  discharge itself — though anyone may audit into `changes-requested`, since
  raising problems needs no independence.

  A review finding the change shipped unresolved, whether open, still-open,
  disputed, or contested, stays that way in what shipped. `arc resolve` on
  the integrated change records its later disposition, such as `resolved
  --commit <fix>`, `obsolete`, or `accepted-risk`, beside that state, and
  readers show both: how it shipped, and how it stood after integration. A
  finding resolved, accepted-risk, or obsolete in the receiver's eligible
  disposition graph at integration takes none. Replay preserves recorded
  dispositions and references regardless of receiver trust; release permission
  is assessed in the receiver's projection.
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
  independent-review debt owed.

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
                       Implemented-by from declared patchset contributors,
                       or the snapshot subject when none are listed, in
                       convention order. Duplicate lines print once.
                       The subject is --on-behalf-of when set, else the actor.
                       A subject that is a fresh harness:model[#effort] or
                       Name <email> supplies itself. The recorder's harness
                       and model apply only to the recorder's own work, on a
                       snapshot that declared its attribution: a declared
                       actor, --solo, --contributors or --on-behalf-of. A
                       derived or git-config recorder is not credited, since
                       who ran snapshot is not proof of who wrote the diff.
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
    integration. A message rewrite requires fresh snapshot and approval or
    declared debt; post-integration audits stay in the ledger.
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
                       A role line whose value is no fresh
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
                       revision requires an active map proving that successor.
                       After withdrawal, re-record the actual mapping with
                       `arc history rewrite --map <file> --reason <why>` to
                       restore that proof without editing ledger events.

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
  as its head, or that a forge fact records as the provider's own (a full
  object ID, from an observation, link, checks or lifecycle fact) stays
  reachable after any rewrite arc performs or records. Each such commit a
  rewrite replaces is held by
  `refs/arc/retained/<change>/<subject>/<commit>`: the subject is the
  patchset the commit was the head of, or the event that recorded a verdict
  or forge fact on it, and a change holds a commit once. A
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
  and a cited commit no ref reaches as the advice `unretained-commit`, naming the `git update-ref` that holds it.
  If Git cannot complete the reachability walk, the report retains collected
  findings and names the problem `retention-reachability-failed` with the error.

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
  them forward. `arc doctor` inspects the recorded revisions and reports
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
  rewrite only when the successor differs from the approved head in nothing but
  its signature — same tree, author, committer and message — which is what
  `arc rewrite sign` produces. Any other successor leaves it stale in `arc
  status` and `arc check`. To re-approve after `arc rewrite trailers`, or a map
  naming different content, run `arc snapshot` and record a verdict on the
  fresh patchset, or use `arc review --snapshot --verdict approved` with the
  work's contributor attribution. Snapshot records a new patchset whenever
  the latest patchset's approved head differs from the current branch head,
  even when its mapped head matches. Earlier verdicts remain bound to their
  own patchsets and stay stale for the rewritten head; a snapshot or rewrite
  grants no approval. While the mapped and approved heads differ, status and
  check direct the caller to snapshot, and an implicit verdict on the latest
  patchset refuses before recording anything. An explicit --patchset judgment
  may record historical work, retaining that patchset's review head and its
  stale coverage. Whether a successor differs by signature alone is
  judged when the map is recorded or imported, never when a change is read.

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
  event.

  A debt is a record of review that did not happen, carried with the
  coverage the work did have, its coordinates, and who produced it. A later
  independent verdict on the shipped revision discharges it; a discharge is
  fulfilment, never approval, and never rewrites the basis the integration
  was accepted on. See REVIEW COVERAGE AND DEBT.

  `arc explain <change>` reads a change's history through these, `arc
  candidate` and `arc context` write them, and `arc stats --provenance`
  counts how often each provenance record was written where it could have
  been.

CANDIDATES
    arc candidate register --tree <tree-or-commit> --brief <change>[@<event>]
                           --producer <actor>... [--parent <id>]...
                           [--adopts <id>]... [--episode <claim>]... [--id <id>]
                           [--evidence-file <file> [--custody-dir <dir>]]
                       Record an alternative answer to one brief version and
                       pin its tree at `refs/arc/candidate/<id>`.
    arc candidate show <id> | list [--brief <change>] [--json]
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
  when a rewrite dropped it. Registration and import require each held raw
  head object to be a commit with the claimed tree. Tags are not peeled; held
  tags, blobs and trees refuse as `evidence-head-type`, and a held commit with
  a different tree refuses as `evidence-tree-mismatch`. Absent commits remain
  external claims. Held originals have retention refs through rewrites and
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
  models or Git identities. A candidate's `producers`, a promoted patchset's
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
  remain readable and importable, including their received evidence, but grant
  no registration retry. An evidence publication failure names the durable
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
  registration's whose tree, declaration, and environment match. Each
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

  `--from-tapes` takes the reads from a `tapes-events/9` document or the
  `tapes-session/14` `.json` of a `tapes export` bundle, one per tool call
  with a `read` member, skipping each call with no stable id, path, or digest,
  each that failed, and each record already held, with one printed line.
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
    was against another commit holding that tree, and `verify --all
    --skip-green` reuses that run. A single `integrate` runs no
    gate; it checks that the merge it made carries the tree that was
    evaluated, and undoes it otherwise. A queue runs the ones with no answer
    at that tree, because it moves the target itself and every member behind
    the one it just landed is now evaluating a different merge.
    The rule names no integration shape: under `ff-only`, where no merge
    ships, a change behind its target still lists `merged-tree-unevaluated`
    beside `needs-rebase` in `check --json`, and `check` exits 11 for the
    rebase before it would exit 14.
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
    `status` runs the probe where it is evaluating and counts the evidence
    only when the two identities agree; a receipt from another environment is
    reported inapplicable, naming both identities, and a receipt with no
    identity is reported unknown. A probe that yields no identity there
    leaves every receipt for the gate not-green. Evidence that records no
    environment identity satisfies only gates that declare no probe, and a
    gate that declares none takes evidence from any environment. A probe is
    bounded by the gate's declared timeout, or thirty seconds when the gate
    declares none. The newest run at the evaluated tree under the declared
    gate and applicable environment decides; other runs cannot hide it. An
    attested run happened where arc observes nothing, so `--attest` takes the
    identity with `--environment <IDENTITY>`.
  - A hostname arc could not read is recorded as `unknown`, a host fact
    on verification and candidate-evaluation events.
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
    an eventual successor, are refused. Withdrawn maps supply no proof;
    re-record the actual map with `arc history rewrite` when recovery needs
    it. Verification replays its candidate event before appending or pinning
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
    <slug>`, or `--range <base>..<head>`. A round is its ordinal within that
    subject, so the loop of bounded rounds records on a fork or a bare commit
    range as readily as on a ledger change.
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
    `arc check` exits 5 when a required gate is not green at the head.
    `arc check` exits 6 for a closed change, a missing branch, or a target
      branch that cannot be resolved.
    `arc check` exits 7 while a prerequisite change is unresolved.
    `arc check` exits 11 when the target conflicts or ff-only ancestry
      requires rebasing; it shadows 14 when both are present.
    `arc check` exits 12 when a declared acceptance probe is not
      discriminating.
    `arc check` exits 13 while the change declares it is iterating.
    `arc check` exits 14 when the tree a merge would ship has no gate
      evidence.
    `arc check` exits 15 when the change's branch is a fork's: fork work is
      unintegrated by intent, and the boundary binds to the change rather
      than to the directory the command runs in.
    `arc check --tag` exits with the code of the first blocked change it
      lists, and 0 when every match is ready or closed.
    Arc exits 17 when a paired replica does not hold integration authority;
      the refusal names the holder or an offer in flight.

  A refusal lists its blockers, then `Next step:`: the command that does
  what comes next, with the change filled in and the `next_action` code in
  parentheses, as in `arc verify <change> --gate test (run_gate:test)`.
  Where no single command does it, the line says what to do and names the
  command that records it; an approval rejection prints the policy's reason
  in place of a code. `status`, `show`, `review`, `resume`, and `rescue`
  print the same line, and `--json` carries the code alone.

  Codes 1 and 2 are also reachable without a blocker at all: `arc` exits 1 on
  an internal error and 2 on a usage error, which argument parsing decides
  before any command runs.

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
  User configuration is `<ai-home>/arc/config.toml`; the AI data home is
  `~/.local/ai/` unless ARC_AI_HOME or AI_HOME names another. ARC_WORKTREES_DIR and
  ARC_DATA_ROOT override their keys, and ARC_DATA_DIR names one exact ledger
  directory for one repository, above both. That directory holds the change
  events and, under `custody/sha256/<digest>`, retained forge content.
  `arc config` prints the resolved paths. Only a missing configuration
  selects built-in settings. Access, filesystem and parse errors remain
  errors for every configuration consumer; advisory consumers name their
  fallback and the error on stderr.

    worktrees_dir = "~/.worktrees"       where change worktrees are created
    data_root = "<dir>"                  ledgers at <dir>/<repo-path-slug>/
                                         rather than <git-common-dir>/arc/
    [journals] dirs = { "<prefix>" = "<journal-dir>" }
                                         journal by path prefix; the longest
                                         matching prefix wins
    [journal] auto_log = true            begin, integrate, and close append a
                                         journal log event; a configuration
                                         that cannot be loaded turns it off
                                         and says why on stderr
    [identity] detect = true             an undeclared harness, session, and
                                         model are read from the session store
    [provenance] git_identity = "per-actor" | "shared"
    [change] checkout = "in-place" | "worktree", branch_prefix = "arc/"
    [integration] shape = "ff-only" | "no-ff", cleanup = false

  Policy is `.arc/policy.toml`, read from the change's target branch, and
  the operator policy at `<git-common-dir>/arc/operator-policy.toml`, outside
  every tree (`arc policy path|show|write`). Both apply, the stricter reading
  winning.

    [policy] forbid_self_approval = true
    [policy] require_declared_actor = true
    [policy] debt_count_threshold = <n>          debt turns advisory past n
    [policy] debt_age_threshold_seconds = <n>    or once the oldest is older
    [policy] worktree_free_floor_bytes = <n>     `arc begin` warns below it
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

  Gates are `[gates.<name>]` tables in `.arc/gates.toml` or the operator
  policy:

    command = "<shell command>"
    profiles = ["<profile>"]            omitted: required for every profile
    timeout = "10m"                     s, m, or h; omitted: unbounded
    environment = "<probe command>"     evidence counts only where the probe
                                        prints the same output

  Every patchset's resolved head is pinned by `refs/arc/keep/<change>/<patchset>`
  so Git's garbage collection cannot take it. Pins follow recorded rewrites
  and hold their successors. The commit a pin held, and any other commit the
  ledger cites as a patchset head or a verdict's revision, is held under
  `refs/arc/retained/<change>/<subject>/<commit>` by `arc rewrite` and
  `arc history rewrite --map`, as is a provider-observed commit a forge fact
  cites, a received source/tested/report commit, and an import that holds one.
  A pin or a retention ref is released
  only once its commit is reachable from the integration commit, where history holds it;
  a replaced commit is on no branch, so its ref stays while the ledger cites
  it, and closing a change without integrating it releases nothing. Release
  any other with `git update-ref -d`. A registered candidate's tree is pinned by
  `refs/arc/candidate/<id>` until `arc candidate retire` releases it.

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
    `arc-check/4`                    arc check --json
    `arc-inbox/11`                   arc inbox --json
    `arc-catchup/12`                 arc catchup --json
    `arc-journal-catchup/9`          arc journal catchup --json
    `arc-resume/8`                   arc resume --json
    `arc-explain/1`                  arc explain --json
    `arc-watch/1`                    arc watch --json, stdin of arc watch --exec
    `arc-config/1`                   arc config
    `arc-accept/1`                   arc accept --json
    `arc-integration-plan/1`         arc integrate --dry-run --json
    `arc-provisional-transition/2`   arc integrate --provisional --json
    `arc-brief/2`                    arc brief --json
    `arc-journal-artifact/3`         arc journal show --json
    `arc-journal-open/1`             arc journal open --json
    `arc-journal-inventory/6`        arc journal inventory --json
    `arc-rescue/5`                   arc rescue --json
    `arc-review/5`                   arc review --json
    `arc-findings/3`                 arc findings --format json
    `arc-blocker-status/1`           arc blocker-status --json
    `arc-metadata/1`                 arc metadata --json
    `arc-chain/4`                    arc chain --json
    `arc-candidate/1`                arc candidate show|list --json
    `arc-stats/1`                    arc stats --json
    `arc-stats-by-model/1`           arc stats --by-model --json
    `arc-stats-provenance/1`         arc stats --provenance --json
    `arc-changelog/1`                arc changelog --json
    `arc-changelog-render-request/1` stdin of a command changelog renderer
    `arc-forks/3`                    arc fork list --json
    `arc-doctor/6`                   arc doctor --json
    `arc-workspace/2`                arc workspace list|inbox --json
    `arc-workspace-backlog/19`       arc workspace backlog --json
    `arc-workspace-report/3`         arc workspace report --json
    `arc-workspace-inventory/2`      arc workspace inventory --json
    `arc-writability/1`              arc config --check-writable --json
    `arc-sandbox-clone/1`            arc sandbox clone --json
    `arc-sandbox-diff/1`             arc sandbox diff --json
    `arc-replica-id/1`               arc replica id --json
    `arc-replica/2`                  arc replica status --json
    `arc-journal-questions/4`        arc journal questions --json
    `journal-discussion/5`           arc journal discussion --json
    `journal-source/1`               arc journal source --json
    `arc-journal-filed/1`            arc journal filed --json
    `arc-journal-latest/1`           arc journal latest --json
    `arc-journal-scaffolds/1`        arc journal scaffolds --json

  Commitments, files:
    `arc-bundle/6`                   arc export / arc import
    `arc-bundle-objects/1`           arc export --objects-json
    `arc-custody-bundle/1`           arc export / import --custody-dir
    `arc-forge-observation/1`        arc forge observe --file
    `arc-forge-receipt/1`            arc forge observe|link|checks|pr-state --json
    `arc-received-evidence/1`        arc candidate register --evidence-file
    `arc-replica-bundle/3`           arc replica export / import
    `arc-replica-event/3`            one event inside an arc-replica-bundle
    `arc-journal-bundle/2`           arc journal export / import
    `journal-events/2`               events.jsonl, streamed by arc journal events
    `arc-journal-spool/2`            .arc/outbox/<ts>-<kind>-<topic>.json
    `arc-trailer-edits/1`            arc rewrite trailers --edits

  Imports accept `arc-bundle/5` and `arc-replica-bundle/2` alongside
  current export formats. Journal imports accept
  `arc-journal-bundle/1`.
  Their absent model provenance stays absent.

  A journal event omits the harness or session key nobody established.
  Owned claim and lane lifecycle events require their complete identities;
  an event lacking one is unsupported and cannot change ownership state.
  `journal-events/2` covers that and the `evidence` kind; `journal-events/1`
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
    store format 7                   .git/arc/config.json and the change ledger
    `arc-accept-checkout/1`           local administrative allocation recovery
    `arc-import-batch/1`             local atomic change import and authorization
    `arc-replica-import/3`           receipt of an imported replica bundle
    `arc-journal-exchange-import/1`  receipt of an imported journal bundle
    `arc-sandbox/2`                  .arc-sandbox.json
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
      --version`); a declared gate, renderer, or hook command is the
      project's or its caller's.
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

  arc <command> --help for any command's full contract. arc --help for all of them.
"#;

pub fn print() {
    print!(
        "{}",
        GUIDE.replace("{READ_ONLY_COMMANDS}", &read_only_commands())
    );
}

/// The read-only command table as indented guide lines, wrapped at the
/// guide's width.
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
