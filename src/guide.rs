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

  `arc env` leaves ARC_MODEL unset. Each write resolves the acting session's
  model. Declare one with `--model` or export ARC_MODEL; the declaration is
  recorded with any observed disagreement. `arc show --json` and `arc log`
  show the model evidence.

  Every event records who wrote it. Most writes accept an undeclared
  identity. In that case arc records an actor nobody claimed:
  `<harness>:<session>` when both are known, else `git config user.name`.
  Either is assumed rather than declared, so it cannot be the independent
  party to an approval.
  The ledger's append guard reads `require_declared_actor` from the invoking
  checkout once per command, including for repository-wide events with no
  change target. Integration also checks the change target's policy before
  merging.

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
  is set, rather than choosing by variable order. It reads that harness's own
  session store for the model, honouring the store's own override —
  `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_SESSION_DIR`,
  `PI_CODING_AGENT_DIR` — before the default under `$HOME`, and reports
  whether the store corroborates the exact canonical session id. An
  ambiguous or unreadable lookup remains unresolved rather than absent.
  Events record that verdict beside the session they carry. It exports the
  harness, session, and session link it establishes and explicitly unsets
  each it does not; the model is a comment and ARC_MODEL is always unset. Pi re-sets its
  live recording file, model, and reasoning level for each tool call, so those
  answer while `PI_SESSION_ID` is the acting session, and a Claude subagent
  shares its parent's session id, so while the session has an unfinished
  subagent recording no model is named and the line says why.
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
                       digest, are one `shared-plan-slice` fact. `--help`
                       lists every attention rule. Versioned
                       `arc-workspace-report/3`.
  cd <anchor>          Enter one project named by the report, then orient there.

ORIENT INSIDE A PROJECT (start here, in this order)
  arc catchup            Live state: ledger queue, deferrals delegated rounds
                         left open, journal backlog, forks, unowned branches
                         and worktrees, and what the change and fork
                         worktrees occupy — apparent size, with the mount's
                         free space. A path several changes record names them
                         all, and a change not checked out there carries the
                         command that would let it gate.
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
  arc journal open       The actionable backlog — work waiting for a session.
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
  arc claim <file.md>    Claim a journal artifact, the way a change is
                         claimed: a lease with a TTL, renewed by working it,
                         released with `arc release-claim <file.md> --outcome
                         paused|abandoned|expired`. `arc stage <file.md>
                         <stage>` records typed progress against it.
                         Artifact claims require a declared actor, harness,
                         and session.
  arc journal checkpoint <file.md> --body-file -
                         Where the work stands and what a successor should
                         read, appended to the artifact and recorded with a
                         digest of the block. `--next`, `--gate`, and
                         `--blocker` carry the structured half; `--supersedes
                         <checkpoint-id>` corrects an earlier one without
                         hiding it.
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
  readable so a failed reduction cannot silently remove an entry.

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
  arc doctor             Opens with the roots this invocation reads and
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
  directory — AI_HOME, ARC_WORKTREES_DIR, ARC_DATA_ROOT, ARC_DATA_DIR,
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
  arc journal suggest <file> --question <id> --option <opt> --body-file -
                         Suggest an option without settling the question;
                         every question view shows it beside the options.
  arc journal latest <topic> [--kind <k>]
                         One topic's newest artifact, hot before cold; inside
                         one second the successor relation and recording order
                         decide, never the filename suffix.
  arc journal correct <file> --target <t> --field <f> --value <v> [--note]
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
  arc journal consume <file> --outcome done --decision <decision>
  arc journal transition <file> --to discussion [--dry-run]
    Change a live artifact's kind as one guarded operation: a typed successor
    with a `supersedes` link, the source retired. Promotion to a code change
    stays with `begin --from-journal`, never a kind conversion.
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
  arc begin <slug> --profile <p>     Open a change: branch + worktree + record.
    --from-journal <file>            Open it from a journal item, consuming it.
    --blocked-by <change> --tag <t>  Declare a chain up front, at planning time.
    --no-worktree                    Take over this checkout, if it can be taken.
    --base <rev>                     Start from <rev>, not the target head.
                                     Without it, a target behind its upstream
                                     (as the last fetch left it) is a warning
                                     naming both revisions and the fix.
  arc brief --body-file <f>          Record the next contract version.
    --caused-by <kind:ref>           Why it changed. v1 refuses a cause; every
    --cause-note <text>              later version requires one of these.
  arc claim / stage / release-claim  Advisory liveness while implementing.
    --takeover                       Displace a claim that may be taken over:
                                     a stale one on a change, an expired one
                                     on an artifact.
    --because <reason>               Displace one that may not be, stating why
                                     its holder is gone. `harness-status-absent`
                                     and `delegate-exit:<handle>` are the
                                     expected evidence; any text is accepted.
  arc snapshot                       Record the current head as a patchset.
    --contributors <a,b> | --solo    Declare who wrote it; required while
                                     another actor holds a live claim.
    --journal-ref <file>             Name a journal artifact that framed the
                                     work; recorded with the digest read now.
    --thread <scheme:id>             Name the external thread it belongs to.
                                     Identifiers only, never transcript text.
  arc verify --gate <name>           Run a declared gate; record the evidence.
  arc verify --command <cmd>         Same, for an ad hoc probe.
  arc verify --against <branch>      Run every required gate on the merge with
                                     that branch, not on this head.
                                     A textual conflict refuses before any
                                     gate runs or evidence is recorded;
                                     rebase first.
    --skip-green                    Reuse passing evidence with --all at the
                                     head's tree, or --against at the merged
                                     tree, whichever commit it ran on.
                                     The reuse records its tree; evidence
                                     with neither tree nor tested_tree is
                                     rerun. Replays of reuse events without
                                     a tree require matching revisions.
    --attest --environment <id>      Record a run arc did not perform, naming
                                     the environment it applies to.
    --falsified-by <id> --predicted <why>
                                     Name the failure this pass answers.
  arc done                           Snapshot, run every gate, print check state.
    --contributors <a,b> | --solo    As on `arc snapshot`.
  arc rebase [--verify]              Replay the branch onto its target, snapshot
                                     the new head, name the gates it owes.
  arc policy show                    Show effective project and operator rules,
                                     with the file that declared each rule.
  arc policy path                    Print the operator policy path for this repo.
  arc policy write --body-file FILE  Replace that local policy from TOML.
  arc review --verdict <v>           Record a verdict (+ --findings-json -).
    --snapshot                       Snapshot the clean worktree first;
      --contributors <a,b> | --solo  attribute that patchset, as on
                                     `arc snapshot`.
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
  arc integrate <c>...               Guarded --no-ff merge once the gates are green.
    <c> <c> <c>, or --tag <t>        Land a queue, in dependency order.
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
                                     File a journal bundle into this journal.
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

  A profile with no declared gate runs none: `done` still snapshots and
  prints the check state, saying plainly that no gate is declared rather
  than reporting a pass, and `verify --all` and `verify --against` still
  refuse because there is nothing to run.

  While a change declares `iterating`, `check` reports `iterating` and
  suppresses `no-valid-approval`; every other blocker still applies.
  `arc iterating <change> --off` restores the approval check.

  A findings batch (`--findings-json` on `review`, `audit`, and `external
  verdict`) is read for `blocking`, `severity`, `summary`, `body`, and
  `anchor`. A field that looks like a misspelling of one its finding omits,
  such as `blocker`, refuses the batch: the finding would record the omitted
  field's default, and a non-blocking finding lets an approval through. Any
  other unknown field is ignored with a warning naming it; `id` is ignored
  silently, since arc assigns finding IDs.

  Each acceptance probe declared on the patchset's brief blocks readiness
  until baseline evidence fails at that brief's base and final evidence passes
  at the patchset's head. Both runs must name that brief and probe; the newest
  run for each phase at its required revision decides. A brief with no base,
  or with a base equal to the patchset head, cannot discharge a probe. The pair
  proves discrimination, not relevance.

  `arc rebase` is what `check` names when the target moved with conflicting
  changes. A conflict stops it and leaves the rebase in progress — the partial
  resolution is yours, and aborting would throw it away — with the conflicting
  files and the commands that finish the replay. Resolve, `git rebase
  --continue`, then `arc snapshot --verify`.

  Several changes, or `--tag`, make `integrate` a queue: dependency order, and
  per member the two repairs that need no judgement — replaying a branch its
  target moved under, and running the gates that have no answer at the tree
  the merge would ship. It stops at the first member that needs a person: a
  conflict, a red gate, a missing verdict. The exit code is that member's own
  blocker, and the closing summary names what landed with its merge revision,
  what stopped the run and why, and what was never attempted. `--dry-run`
  reports the same plan without replaying, running, or merging anything.
  `--into`, `--message`, and `--debt` stay single-change: a queue lands several
  merges, each into its own recorded target, and none of the three has one
  thing to name — a debt reason least of all, since it is a judgment about one
  patchset. A queue is for changes already green or already carrying a verdict.

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
  branch and its preceding revision, and its authorization basis: the
  approving verdict and its provisional reason when present, an external
  approval when consumed, one passing verification event per required gate,
  each prerequisite's satisfying closure, the empty blocking-finding and hold
  vectors, the normalized gate and policy values consumed, and the danger
  determination. A debt declaration is recorded in the basis only when its
  waiver supplied the approval or let it stand. `integrate --dry-run` prints
  the basis it would record. Before merging, readiness is recomputed and the
  basis rebuilt; if readiness fails or the basis differs, nothing is written.

  A closed change is judged at the head its closure recorded, or at its
  newest patchset's when the closure recorded none. `status`, `show`, and
  `check` read neither its branch nor its target, so deleting the branch or
  moving the target after closure leaves the head its verdict and gate
  evidence are read against where it was.

  The decision is one function of facts observed under the target and change
  locks: a plan naming the target revision, the approved head, the tree the
  merge must ship, and the basis to record, or a refusal. The merge runs from
  the plan and is confirmed against it. `integrate --dry-run --json` prints
  that plan, decided the same way, as `arc-integration-plan/1`. Handing it
  back with `integrate --expect-basis <file>` never changes the decision; it
  names what moved since — the approved head, the target revision, the gate
  or policy declarations, or the approval — in one line beside a refusal, or
  as a warning when the integration still proceeds.

  When no checkout holds the target, `integrate` takes over the change's own
  checkout if it still holds the change branch, checks the target out there,
  merges, and leaves the checkout on the target. The dry run says it would.
  Any other shape keeps the refusal naming the missing target checkout.

  `--no-worktree` means in place, not nowhere: a clean checkout already on the
  target is checked out onto the new branch and recorded as the change's
  worktree, so the next command infers the change without being told. A dirty
  checkout, or one standing elsewhere, is left exactly as it was — the change
  still opens, and arc prints the Git command that finishes the switch.

  Release copy is recorded per change and rendered at release; arc imposes
  no file convention. While an open change has no changelog record, `arc
  status`, `arc check`, and `arc integrate` advise one under the code
  `no-changelog-entry`, never as a blocker. Recording `--none --reason` that
  the change needs no entry answers the advice and projects nothing; the
  latest record wins. The built-in renderer replaces the `## [Unreleased]`
  block of the target `.arc/changelog.toml` names, `CHANGELOG.md` by default.
  A project whose file follows another convention selects `renderer =
  "command"` with a `renderer_command` argv. That command runs from the
  repository root without a shell, with the authority of whoever runs `arc
  changelog`, reads an `arc-changelog-render-request/1` document on stdin,
  and answers on stdout: the rendering for a read, and for `--write` the
  complete replacement file, written atomically after it exits 0.

PAIR REPLICA STORES
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
  Imports have a dry-run preview, are idempotent, and record a receipt with the
  source replica and bundle digest. A paired replica without authority is
  refused by `integrate`.

  Journal artifacts travel between paired replicas as a versioned bundle:
  each body, the events recorded about it, its digest, the exporting replica,
  and every artifact the selection references by filename. An import is
  all-or-nothing: the whole bundle is validated before anything is written.
  An artifact the receiving journal holds in a different storage tier, or at a
  different digest, refuses the whole import; an artifact that is not there
  lands in the hot journal or the cold archive the bundle records, and its
  events keep the identity they were recorded under. A live claim arriving on
  an artifact a live local claim holds is reported as a contest naming both
  replicas, and nothing on either claim's behalf is recorded. Importing the
  same bundle twice changes nothing; the receipt names the source replica and
  bundle digest.

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
  arc journal memory <topic> --body-file -     Learned a durable fact.
  arc journal consume <file> --outcome done    Drain what you resolved.
  arc journal archive --consumed               Move the drained to cold storage.

  ORIENT above is fed entirely by these writes: `catchup` surfaces memories,
  `journal open` lists what a handoff parked.

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
  artifact read as authoritative is worse than no artifact. A round that
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
  direct   Bounded, reversible, one session and checkout. `--no-worktree` uses
           a clean target checkout in place; otherwise it remains untouched.
           Implement, verify, review the diff, commit. No journal topic.
  local    Spans sessions or roles, or needs host-local evidence CI cannot
           reproduce. Dedicated branch/worktree, verdict or debt per REVIEW
           COVERAGE AND DEBT, --no-ff merge.
  forge    A hosted PR adds a remote record, inline discussion, clean runner
           evidence, or branch protection. local underneath; project only public
           integration facts into the PR.
  release  Deployment, publishing, irreversible migration, security-sensitive
           work. forge plus explicit release and rollback gates.

  Promote when the work reveals more scope, risk, or concurrency; record why.
  Never keep an undersized profile just because the change started there.

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
  than one commit; `arc squash <change> -m <message>` makes that one commit as
  a new patchset with its own gates and verdict, attributed by `--contributors`
  or `--solo` as on `arc snapshot`. Over another actor's live claim an
  unattributed squash refuses before the branch moves. A failed squash commit, a
  changed commit tree, or tracked edits left by a hook restore the original
  head, index, and tracked files. Untracked files created by hooks are retained
  for inspection. Paths obstructing restoration are moved under
  `<git-common-dir>/arc/squash-recovery/`; the command prints the recovery
  directory before moving them.

  A debt is declared with `arc debt <change> --reason <why>`, or at the merge
  with `arc integrate <change> --debt "<why>"`. A debt already in force routes to
  integration: status says integrate beside the flagged waiver, check drops
  the review-queue advisory, and the inbox keeps one lead row — until the
  head moves past the covered patchset, which restores request_review. The debt can stand in for an
  absent verdict or rescue a self-approval rejected by repository policy. It
  binds to the exact patchset head declared, so new work needs a new
  declaration — it does not excuse the rest of the change's life. A refused
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

  A review finding left open when the change shipped stays open in what
  shipped. `arc resolve` on the integrated change records its later
  disposition, such as `resolved --commit <fix>`, `obsolete`, or
  `accepted-risk`, beside that state, and readers show both: open at ship,
  and how it stood after integration. A finding resolved, accepted-risk, or
  obsolete at integration takes none. Such a disposition is not a verdict
  and discharges no debt.

  Where `forbid_self_approval` is off, an approving verdict from the identity
  that wrote the work is recorded rather than refused, and `arc review` and
  `arc audit` both name the match: who it was recorded as, which patchset that
  identity wrote, and whether arc assumed the identity rather than anyone
  declaring it. Such a
  verdict is a review that happened, not an independent one, and it leaves an
  independent-review debt owed.

REVIEW AND REPAIR
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
    arc instructions git [--check <file>]
                       Print the portable contribution-trailer convention
                       `arc-contribution-trailers/1`: `Planned-by`,
                       `Implemented-by`, `Reviewed-by`, and `Orchestrated-by`
                       name material contributions in agent
                       `harness:model[#effort]` or human `Name <email>` form.
                       `--check <file>` reports malformed role values and
                       keys outside the convention. It never rewrites a
                       message, and exits 1 when a role key carries a
                       malformed value.

HISTORY REWRITES
    arc rewrite sign [--key <id>] [--from <rev>] [--include-shared]
                     [--dry-run] [--retag]
                       Recreate every commit from --from through the branch
                       head so one key signs them all, move the branch, arc's
                       refs and the local branches and tags that point into
                       the range, and record the map. --from is inclusive. On
                       a tracked change branch its default is the oldest
                       commit not signed by the key in target..head; outside
                       a change it searches the whole history. Commits
                       reachable from the target or another local branch are
                       refused, naming the commit and ref, including in
                       --dry-run. --include-shared permits them and searches
                       the whole history by default, naming stranded refs.
                       --dry-run prints the map, the tags it
                       would re-point or leave alone, and stops; --retag
                       recreates the annotated tags whose targets were
                       rewritten, signed like the commits. Run again to
                       finish one that was interrupted.
    arc rewrite trailers --from <rev> [--drop <key>] [--append <line>]
                       Edit the trailer block of every commit message from
                       --from through the branch head, and carry the result
                       through the same map, refs and record. --drop removes
                       every trailer with that key, matched without case;
                       --append adds a `Key: value` line the block does not
                       already carry; both repeat, and at least one is
                       required. --from is named rather than inferred.
                       --key, --no-sign, --dry-run and --retag mean what they
                       mean above.
    arc history rewrite --map <file> --reason <why>
                       Record a rewrite performed elsewhere, from its commit
                       map.
    arc history resolve <rev>     Where a recorded revision ended up.
    arc history withdraw <event-id> --reason <why>
                       Withdraw one history-rewritten repository event. Its
                       map does not participate in resolution or readiness;
                       refs are not moved back. Output names recorded ref
                       moves, or says when that information is absent.
                       Other event types are refused. A withdrawal travels
                       with the map in full and delta exports, and importing
                       replicas honour it even when they already hold the
                       map. A delta can carry repository events with an empty
                       change suffix. A correct map can be recorded for the
                       same revisions. Stored events remain intact.

  Only the signature and the commit ids change: tree, parents, author,
  committer, dates, encoding, message and any other header the commit carries
  — `mergetag`, on a merge of a signed tag — travel through untouched, so a
  commit recreated without signing has the id it started with. The rewrite
  names each commit that carried such a header. Trees are identical either
  way, which is why tree-keyed gate evidence counts the same on both sides of
  a signing rewrite.

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

  Every ref moves in one Git transaction, so a rewrite leaves the branch and
  arc's evidence refs on one history or leaves them all alone. The map and
  the ref moves are written to `repository/rewrite-intent.json` before any of
  it is applied, and the file is removed once the map is recorded, so an
  interrupted rewrite is finished by running it again rather than repeated —
  repeating it would sign the same commits afresh and record a second
  successor for each. A ref moved by something else in the meantime stops the
  rewrite, named.

  Recorded revisions keep saying what they said; every derived reading follows
  them forward, and `arc doctor` reports `unresolved-revision` where that
  leads nowhere. A recorded revision resolves on an exact match, or on an
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
  status` and `arc check`: re-approve after `arc rewrite trailers`, or a map
  naming different content. Whether a successor differs by signature alone is
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

  Candidate events, evaluations, selections, and promotions among them, are
  repository events, so every `arc export` carries them
  and `arc import` judges them with the ledger they join before writing
  anything. An imported registration whose tree this object store lacks stays
  unpinned. `arc doctor` advises on a pin with no registration and on
  undeclared `[candidates] evaluation_reuse`, and reports a registration a
  root reaches whose pin is gone.

CONTEXT
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
  - A lane's owner is a harness and a session together. Harnesses mint session
    strings independently, so neither half names an owner on its own.
  - A lane is occupancy of a topic; a claim on a journal artifact is occupancy
    of that file. Both render on a row and neither becomes the other: work with
    no artifact still takes a lane.
  - An artifact claim expires by its lease alone. A change's stages are
    budgeted and a stage over budget reads `stale`; an artifact has no stages
    to budget, so `expired` is when it becomes reclaimable, and `--takeover`
    is what displaces it.
  - A claim that is not yet reclaimable is displaced only by `--takeover
    --because <reason>`, and the reason is recorded on the displaced claim,
    printed wherever it is rendered, and verified by nobody. The evidence a
    holder is gone — `harness-status-absent`, `delegate-exit:<handle>` — is
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
    bound, or the cause and reason when lookup or reading cannot finish.
  - `arc watch <file.md> --until stalled` arms the wait over an artifact
    claim, which has no stages and stalls when its lease runs out, and
    `arc rescue <file.md> [--take]` reports where the work stopped and takes
    it over. An artifact answers only `stalled`; the rest of the
    vocabulary asks about patchsets and verdicts.
  - `arc watch <change> --until` accepts `snapshot`, `stalled`, `reviewed`,
    `approved`, `gates-green`, `ready`, `blocked`, `brief-recorded`,
    `integrated`, and `closed`. `approved` returns on the latest approving
    verdict, including a provisional approval and its recorded reason;
    `gates-green` checks every required gate at the current head; `blocked`
    and `brief-recorded` name the events that recorded those facts.
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
  - A snapshot records the context that framed the work. Each journal link
    names an artifact by filename with the body digest read at record time,
    and a `via` saying where it came from. With no `--journal-ref`, a
    snapshot (`snapshot`, `done`, and the ones `squash` and `review
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
    holds it as `<journal-dir>::<file>`, or that form when none does. The
    change records the reference as given, with digests read from the owning
    journal. A cross-project `begin --from-journal` or `brief
    --plan-ref` appends a `promoted` event to the owning journal naming this
    repository id, change, and checkout, and a non-plan source is consumed
    there as superseded by that change. The owning journal's `inventory`,
    `open`, and workspace views list the promotion with its `repository_id`,
    its status read from the promoting ledger: `unknown` when that ledger
    cannot be read, which never counts as closed. Only the owning project
    consumes, archives, unarchives, or transitions an artifact; a qualified
    reference to one is refused naming the owning journal.
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
    A passing gate row is `discriminating` when any passing evidence for that
    gate at the counted tree (or revision when the tree is unresolved) names
    a falsification. A later pass without one does not retract it.
  - The journal lives outside the repo, so worktrees stay clean. Cross-session
    context goes there, never into tracked files. A sandbox that cannot reach
    it is not a reason to leave the record in a transcript: every kind verb
    and `journal log` take `--spool`, and a write spools by itself when the
    journal cannot be written. It parks in `.arc/outbox/` and prints
    `spooled: <path>`; `arc journal spool --promote` files it later with the
    identity it was spooled with. `arc config --check-writable` says which of
    the two will happen before you write.
  - An artifact is named `<timestamp>-<topic>-<kind>.md` at second resolution,
    so two writes of one topic and kind in one second want one name. The second
    takes a numeric suffix — `-2`, `-3` — before the extension, and every write
    prints the name it got. A name counts as taken in cold storage as well as
    the live directory, so archiving the first never frees its name for a
    second that would then collide with it. The suffix names no part of the
    artifact: its timestamp, topic and kind read exactly as written.
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
  `arc check` is the integration preflight and its code names the blocker.
  `integrate` and `rebase` refuse in the same vocabulary, and a queue exits
  with the code of the member that stopped it, so one table reads the same
  way whether one change was integrated or twenty.

    `arc check` exits 0 when the change is ready to integrate.
    `arc check` exits 2 while a blocking finding is open.
    `arc check` exits 3 when no valid approval covers the current head.
    `arc check` exits 4 while a hold is active.
    `arc check` exits 5 when a required gate is not green at the head.
    `arc check` exits 6 for a closed change, a missing branch, a target branch
      that cannot be resolved, or malformed state.
    `arc check` exits 7 while a prerequisite change is unresolved.
    `arc check` exits 11 when the target moved with conflicting changes and
      the branch needs rebasing.
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
  `~/.local/ai/` unless AI_HOME names another. ARC_WORKTREES_DIR and
  ARC_DATA_ROOT override their keys, and ARC_DATA_DIR names one exact ledger
  directory for one repository, above both. `arc config` prints the resolved
  paths.

    worktrees_dir = "~/.worktrees"       where change worktrees are created
    data_root = "<dir>"                  ledgers at <dir>/<repo-path-slug>/
                                         rather than <git-common-dir>/arc/
    [journals] dirs = { "<prefix>" = "<journal-dir>" }
                                         journal by path prefix; the longest
                                         matching prefix wins
    [journal] auto_log = true            begin, integrate, and close append a
                                         journal log event
    [identity] detect = true             an undeclared harness, session, and
                                         model are read from the session store
    [provenance] git_identity = "per-actor" | "shared"

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
    [candidates] evaluation_reuse = "matching-coordinates" | "never"
                                                 no default; `never` wins

  Gates are `[gates.<name>]` tables in `.arc/gates.toml` or the operator
  policy:

    command = "<shell command>"
    profiles = ["<profile>"]            omitted: required for every profile
    timeout = "10m"                     s, m, or h; omitted: unbounded
    environment = "<probe command>"     evidence counts only where the probe
                                        prints the same output

  Every reviewed head is pinned by `refs/arc/keep/<change>/<patchset>` so
  Git's garbage collection cannot take it. A pin is released only once its
  head is reachable from the integration commit; release any other with
  `git update-ref -d`. A registered candidate's tree is pinned by
  `refs/arc/candidate/<id>` until `arc candidate retire` releases it.

SCHEMAS
  Every structured surface carries a `schema` string `<name>/<n>`. A shape
  change takes one new version per release, counted from the last release:
  adding a field as much as removing, renaming, or redefining one. A surface
  already bumped for the unreleased version keeps that number for further
  shape changes. A commitment is a shape callers outside arc read, and its
  version is a promise. An internal shape is arc's own on-disk bookkeeping;
  parsing one means tracking arc's implementation.

  A stored input format is versioned from the reader's side: its version
  marks what a reader must accept, so a new optional field that leaves every
  older file valid keeps the version, and removing a field or making one
  required takes the next. `journal-events/1` is one.

  Commitments, derived views:
    `arc-state/3`                    arc show --json
    `arc-status/27`                  arc status
    `arc-check/3`                    arc check --json
    `arc-inbox/10`                   arc inbox --json
    `arc-catchup/12`                 arc catchup --json
    `arc-journal-catchup/8`          arc journal catchup --json
    `arc-resume/8`                   arc resume --json
    `arc-explain/1`                  arc explain --json
    `arc-integration-plan/1`         arc integrate --dry-run --json
    `arc-brief/2`                    arc brief --json
    `arc-journal-artifact/2`         arc journal show --json
    `arc-journal-inventory/6`        arc journal inventory --json
    `arc-rescue/5`                   arc rescue --json
    `arc-review/4`                   arc review --json
    `arc-findings/2`                 arc findings --format json
    `arc-blocker-status/1`           arc blocker-status --json
    `arc-metadata/1`                 arc metadata --json
    `arc-chain/4`                    arc chain --json
    `arc-candidate/1`                arc candidate show|list --json
    `arc-stats/1`                    arc stats --json
    `arc-stats-by-model/1`           arc stats --by-model --json
    `arc-stats-provenance/1`         arc stats --provenance --json
    `arc-changelog/1`                arc changelog --json
    `arc-changelog-render-request/1` stdin of a command changelog renderer
    `arc-forks/2`                    arc fork list --json
    `arc-doctor/5`                   arc doctor --json
    `arc-workspace/1`                arc workspace list|inbox --json
    `arc-workspace-backlog/19`       arc workspace backlog --json
    `arc-workspace-report/3`         arc workspace report --json
    `arc-workspace-inventory/2`      arc workspace inventory --json
    `arc-writability/1`              arc config --check-writable --json
    `arc-sandbox-clone/1`            arc sandbox clone --json
    `arc-sandbox-diff/1`             arc sandbox diff --json
    `arc-replica-id/1`               arc replica id --json
    `arc-replica/2`                  arc replica status --json
    `arc-journal-questions/3`        arc journal questions --json
    `journal-discussion/4`           arc journal discussion --json
    `journal-source/1`               arc journal source --json
    `arc-journal-latest/1`           arc journal latest --json
    `arc-journal-scaffolds/1`        arc journal scaffolds --json

  Commitments, files:
    `arc-bundle/6`                   arc export / arc import
    `arc-replica-bundle/3`           arc replica export / import
    `arc-replica-event/3`            one event inside an arc-replica-bundle
    `arc-journal-bundle/2`           arc journal export / import
    `journal-events/1`               events.jsonl, streamed by arc journal events
    `arc-journal-spool/1`            .arc/outbox/<ts>-<kind>-<topic>.json

  Imports accept `arc-bundle/5` and `arc-replica-bundle/2` alongside
  current export formats. Journal imports accept
  `arc-journal-bundle/1`.
  Their absent model provenance stays absent.

  Internal:
    store format 7                   .git/arc/config.json and the change ledger
    `arc-replica-import/2`           receipt of an imported replica bundle
    `arc-journal-exchange-import/1`  receipt of an imported journal bundle
    `arc-sandbox/2`                  .arc-sandbox.json
    `journal-binding/1`              bindings.jsonl

WHAT ARC WILL NOT DO
  Ledger and repository:
    arc never deletes or rewrites an event file.
    arc rewrites a branch only through `arc rewrite`, which records the map
      it produced, and merges only through `arc integrate` when every
      required gate is green.
    arc never force-removes a checkout it did not create. Worktree removal
      refuses while dirty or untracked content is present, `fork retire
      --force` is the operator's decision to get past that, and the one
      forced removal arc performs on its own is the scratch checkout it
      creates to evaluate a merge.
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
    arc refuses a bundle written by a newer arc rather than skipping
      lifecycle events it does not know.
    `arc import` reads `arc-bundle/6` and `arc-bundle/5`.
      Absent optional fields stay absent; other bundle schemas are refused.
    arc refuses a gate run only when the change's recorded worktree is
      missing or its HEAD is not the branch head. A run started from another
      checkout of the repository is redirected to the recorded worktree and
      says so.
    arc refuses a `--from-journal` source that is missing, non-actionable, or
      already consumed.
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
    print!("{GUIDE}");
}
