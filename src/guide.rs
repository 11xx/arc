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
  eval "$(arc env)"                    Detect harness, session, and model.
  export ARC_ACTOR=<name> ARC_HARNESS=<claude|codex|opencode|pi> \
         ARC_SESSION=<id> ARC_MODEL=<model[#effort]>

  Every event records who wrote it. Most writes accept an undeclared
  identity. In that case arc records an actor nobody claimed:
  `<harness>:<session>` when both are known, else `git config user.name`.
  Either is assumed rather than declared, so it cannot be the independent
  party to an approval.

  `arc env` detects a harness by the session variable it exports; not every
  harness exports one. It reads that harness's own session store for the
  model, honouring the store's own override — `CLAUDE_CONFIG_DIR`,
  `CODEX_HOME`, `PI_CODING_AGENT_SESSION_DIR`, `PI_CODING_AGENT_DIR` — before
  the default under `$HOME`, and reports whether the store corroborates the
  session. Events record that verdict beside the session they carry.
  OpenCode v2 (`opencode2`) is recognized without one — by
  `OPENCODE_TERMINAL` or its process ancestry — and prints the harness
  export with the session left as a comment to set by hand. With nothing to
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
                       and is versioned `arc-workspace-inventory/1`.
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

  Plans carry portable planned-by metadata. A brief selecting --plan-ref and
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
  arc inbox              Lead-facing queue across open changes, unowned
                         branches and worktrees, and the journal backlog.
  arc workspace backlog  The same question asked of every registered project.
    --here | --under <path>  Restrict it to one workspace directory tree.
    --rank-by blocking|availability|coverage
                         Order rows by one recorded fact; the report states
                         the basis it used.

  Work waiting for this project lives in two places. The ledger holds changes
  already open; the journal holds everything not yet opened as one. An empty
  inbox does not mean an empty queue — check both, which is what `catchup` does.

  Every inbox bucket is an independent predicate, so a change in a state none
  of them anticipates would land nowhere and the queue would read as empty
  while work waited. The `unclassified` bucket catches exactly that and names
  the reason. A row there is a gap in the derivation rather than a resting
  place: it means arc failed to classify open work, and it is worth reporting.

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
  arc journal scaffolds [--show <n>]   What a write prepends, before it does.
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
                         the repaired log is published.
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
  arc claim / stage / release-claim  Advisory liveness while implementing.
    --takeover                       Displace a claim that may be taken over:
                                     a stale one on a change, an expired one
                                     on an artifact.
    --because <reason>               Displace one that may not be, stating why
                                     its holder is gone. `harness-status-absent`
                                     and `delegate-exit:<handle>` are the
                                     expected evidence; any text is accepted.
  arc snapshot                       Record the current head as a patchset.
    --journal-ref <file>             Name a journal artifact that framed the
                                     work; recorded with the digest read now.
    --thread <scheme:id>             Name the external thread it belongs to.
                                     Identifiers only, never transcript text.
  arc verify --gate <name>           Run a declared gate; record the evidence.
  arc verify --command <cmd>         Same, for an ad hoc probe.
  arc verify --against <branch>      Run every required gate on the merge with
                                     that branch, not on this head.
    --attest --environment <id>      Record a run arc did not perform, naming
                                     the environment it applies to.
    --falsified-by <id> --predicted <why>
                                     Name the failure this pass answers.
  arc done                           Snapshot, run every gate, print check state.
  arc rebase [--verify]              Replay the branch onto its target, snapshot
                                     the new head, name the gates it owes.
  arc policy show                    Show effective project and operator rules,
                                     with the file that declared each rule.
  arc policy path                    Print the operator policy path for this repo.
  arc policy write --body-file FILE  Replace that local policy from TOML.
  arc review --verdict <v>           Record a verdict (+ --findings-json -).
    --provisional <why>              It gates, and owes a second judgment.
    --relation corroborates          Support the standing verdict, not replace it.
  arc resolve                        Dispose of a finding.
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
                                     Reclaim an unresolved offer with a reason.
  arc journal export <file>... --output <file>
                                     Export selected journal artifacts with
                                     the events recorded about them.
  arc journal import <file> [--dry-run]
                                     File a journal bundle into this journal.
  arc audit <change> --verdict <v>   Review an already-integrated revision.
  arc close                          Terminal outcome arc did not merge itself.

  A gate runs where its change lives. `verify`, `snapshot --verify`, `done`,
  and `rebase --verify` execute the command in the change's recorded worktree
  and record the evidence at that worktree's head, whichever checkout of the
  repository the command was typed in; the run says which tree it used when
  that is not the one you are standing in. Which gates are required is read
  where you stand, so `status` and `verify` agree on what is owed. A change
  whose worktree is gone has nowhere to gate: give it one with `git worktree
  add`, or record evidence arc did not run with `verify --attest`.

  A profile with no declared gate runs none: `done` still snapshots and
  prints the check state, saying plainly that no gate is declared rather
  than reporting a pass, and `verify --all` and `verify --against` still
  refuse because there is nothing to run.

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

  When no checkout holds the target, `integrate` takes over the change's own
  checkout if it still holds the change branch, checks the target out there,
  merges, and leaves the checkout on the target. The dry run says it would.
  Any other shape keeps the refusal naming the missing target checkout.

  `--no-worktree` means in place, not nowhere: a clean checkout already on the
  target is checked out onto the new branch and recorded as the change's
  worktree, so the next command infers the change without being told. A dirty
  checkout, or one standing elsewhere, is left exactly as it was — the change
  still opens, and arc prints the Git command that finishes the switch.

PAIR REPLICA STORES
  A store keeps its own repository ID as event provenance. `replica init`
  records the first replica and logical project ID. `replica pair` records an
  operator-named peer by its repository ID; that peer adopts the project only
  by importing the exported pairing event. Replica files contain identities
  and authority events, while checkout paths remain local to their store.

  The first replica holds integration authority. `authority offer` relinquishes
  it when the offer is recorded; the named replica acquires it when it imports
  the offer file. Export the recipient's replica events back to the offering
  store to report the acquisition there. Reclaiming an unresolved offer
  requires a reason, and exporting that event reports the reclaim to peers.
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

  Compaction is lossy compression chosen by something that does not know what
  will be needed. The session doing the work does. A fact filed here is in the
  ledger, so there is nothing for it to survive: `arc resume` hands it back to
  a compacted or cold successor.

  File the moment a premise is checked or an approach is abandoned. A rejected
  approach is the highest-value kind and the least likely to be re-derived —
  a cold session will cheerfully try it again. Keep the kinds honest: a
  hypothesis recorded as verified is worse than not recording it.

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

  A session journals at four moments, and none of them is the end. A premise
  checked or an approach abandoned is kept as it happens (`arc keep`), while
  the reasoning that produced it is still recoverable. A filed claim found
  wrong is corrected in the same breath as learning so (`journal correct`, or
  `journal retract` where the entry should stand withdrawn), because a false
  artifact read as authoritative is worse than no artifact. A round that
  closes having deliberately left work records what it left (`run end
  --note`), since a deferral living only in a transcript was dropped rather
  than deferred. A session stopping midstream writes a handoff (`journal
  handoff --derive`), which is cheap enough to write at any interruption
  because only what was learned has to be written by hand.

PROFILES (--profile, default local)
  direct   Bounded, reversible, one session and checkout. `--no-worktree` uses
           a clean target checkout in place; otherwise it remains untouched.
           Implement, verify, review the diff, commit. No journal topic.
  local    Spans sessions or roles, or needs host-local evidence CI cannot
           reproduce. Dedicated branch/worktree, fresh review where possible,
           --no-ff merge.
  forge    A hosted PR adds a remote record, inline discussion, clean runner
           evidence, or branch protection. local underneath; project only public
           integration facts into the PR.
  release  Deployment, publishing, irreversible migration, security-sensitive
           work. forge plus explicit release and rollback gates.

  Promote when the work reveals more scope, risk, or concurrency; record why.
  Never keep an undersized profile just because the change started there.

WHEN NO INDEPENDENT REVIEWER IS REACHABLE
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

  For ordinary work outside the independent-review scope, status offers the
  lead a choice: review_options carries declare_debt first, then
  request_review. The list is guidance — reading it writes nothing, and the
  user or lead runs arc debt themselves. A required-review or unknown-danger
  change offers review alone. A current `changes-requested` or `comment-only`
  verdict is its own next action and offers no debt route: a waiver records a
  missing review, not a way past a refusal.

  `arc review <change>` prints the same current `review_options` alongside the
  verdict history, so a reader does not have to switch views to find the
  available guidance.

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
  path declared dangerous by either file is dangerous. A change touching a declared
  path needs a verdict from somebody other than its author; elsewhere a
  self-recorded verdict satisfies the gate. Declare no danger paths and the
  gate stays uniform. `arc begin --dangerous` raises a single change
  whatever it turns out to touch, and nothing lowers it afterwards — a project
  decides its own gate in advance, rather than a change deciding it under
  pressure to ship. `arc check` names the rule that fired, and `arc doctor`
  reports a declared literal that can never match — one that names nothing, or
  a directory, since declared paths are matched against changed files.

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

  Work sent to a repository you do not own is decided by its receiver.
  `arc external verdict <change> --verdict <v> --decided-by <who>
  --reference <ref> --revision <rev>` records that decision beside, never as,
  a verdict arc witnessed: an approval gates only the revision it names and
  never alone on a dangerous path, a change request carries findings, and a
  rejection of the head closes the change. A `[contribution] history =
  "squash"|"preserve"` declaration makes `integrate` record the change ready to
  send instead of merging it, refusing merge commits and, under squash, more
  than one commit; `arc squash <change> -m <message>` makes that one commit as
  a new patchset with its own gates and verdict.

  If no independent verdict is available, integrate with
  `arc integrate <change> --debt "<why>"`. A debt already in force routes to
  integration: status says integrate beside the flagged waiver, check drops
  the review-queue advisory, and the inbox keeps one lead row — until the
  head moves past the covered patchset, which restores request_review. The debt can stand in for an
  absent verdict or rescue a self-approval rejected by repository policy. It
  binds to the exact patchset head declared, so new work needs a new
  declaration — it does not excuse the rest of the change's life.

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
    arc rewrite sign [--key <id>] [--from <rev>] [--dry-run] [--retag]
                       Recreate every commit from --from through the branch
                       head so one key signs them all, move the branch, arc's
                       refs and the local branches and tags that point into
                       the range, and record the map. --from defaults to the
                       oldest commit whose signature is missing or made by
                       another key; --dry-run prints the map, the tags it
                       would re-point or leave alone, and stops; --retag
                       recreates the annotated tags whose targets were
                       rewritten. Run again to finish one that was
                       interrupted.
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
  one. `--retag` recreates them, carrying name, message, tagger and date, and
  signing where the original was signed.

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
  comparison between the two has an answer. Approval does not travel: a
  verdict binds to an exact head, so re-snapshot and re-approve.

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
    shared refs belong to the lead alone.
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
    is unknown, not healthy. `arc rescue <change> --take` recovers it, and
    `arc rescue <change> --transcript` reads the claimed session's latest
    operator turns through `tapes` or arc's own readers, both taking the
    newest 4 MiB of a recording file, and names the reader that answered, the
    readers that declined, and the bound a read stopped at.
  - `arc watch <file.md> --until stalled` arms the same wait over an artifact
    claim, and `arc rescue <file.md> [--take]` reports where the work stopped
    and takes it over. An artifact answers only `stalled`; the rest of the
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
    was against another commit holding that tree. A single `integrate` runs no
    gate; it checks that the merge it made carries the tree that was
    evaluated, and undoes it otherwise. A queue runs the ones with no answer
    at that tree, because it moves the target itself and every member behind
    the one it just landed is now evaluating a different merge.
  - A snapshot may record the context that framed the work: `--journal-ref
    <file>` links a journal artifact by filename with the body digest read at
    record time, refusing a name that resolves to no artifact, and `--thread
    <scheme:id>` records an opaque external thread reference. `arc show`,
    `arc log`, and `arc status --json` render them, bundles carry them, and
    `arc journal inventory` names the patchsets that cite an artifact. Arc
    records identifiers only, never transcript text: retaining a recording is
    not arc's promise, and a remote recording is read on the machine that
    holds it.
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
    declares none. An attested run happened where arc observes nothing, so
    `--attest` takes the identity with `--environment <IDENTITY>`.
  - A gate that passed is not evidence that it could have failed. Watch it
    fail first, then record the pass with `--falsified-by <failing-event>
    --predicted "<why it should fail>"`; the gate line then reads
    `discriminating` instead of `undiscriminated`. Advisory: it changes no
    result and no exit code, and arc infers it from nothing.
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
  - arc holds no routing opinion. It records the --actor, --harness, and --model
    it is given; who to delegate to is the caller's policy, not arc's.

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
    `arc check` exits 6 for a closed change, a missing branch, or malformed
      state.
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
    `arc forge link` exits 10 when the observed tuple or the declared policy
      does not match, appending no event.
    `arc history resolve` exits 2 when nothing moved the revision.
    `arc restack --advise` exits 0 when the change has no dependents.
    `arc workspace backlog` exits 16 when any selected project's observation
      failed; the rows that were read still print.
    `arc doctor` exits 1 when problems are present and 0 for a clean or
      advice-only ledger.
    A rejected self-approval follows the no-valid-approval path and exits 3.
    An arc-managed Git hook always exits 0, so it can never block a commit.
    A repeat of a recorded source item writes nothing, prints the existing
      entry, and exits 0.
    A spooled write prints `spooled: <path>` and exits 0.

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
    arc never makes a network call.
    arc never refuses to create a worktree over disk space.
    arc records no configuration history, only the inputs to one
      irreversible decision.

  Refusals worth knowing before they happen:
    arc refuses a bundle written by a newer arc rather than skipping
      lifecycle events it does not know.
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
    arc records the actor, harness, and model it is given, and holds no
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
