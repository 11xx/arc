//! The bare-`arc` guide: what an agent needs to drive a change end to end
//! without reading anything else first.
//!
//! `--help` is the reference — every command, every flag. This is the
//! orientation: what arc owns, which commands matter in what order, and the
//! handful of rules that change what a session should do. Anything that
//! belongs to one command's contract stays in that command's `--help`.

pub const GUIDE: &str = r#"arc — change, review, and integration state over plain Git.

Git owns content, branches, and history. arc owns what Git lacks: changes,
patchsets, findings, verdicts, verification evidence, claims, holds, and a
guarded merge — one append-only JSON event per fact, under the repo's Git
common dir. Every list, status, and inbox is derived; nothing is rewritten.

SAY WHO YOU ARE (before the first write)
  eval "$(arc env)"                    Detect harness, session, and model.
  export ARC_ACTOR=<name> ARC_HARNESS=<claude|codex|opencode|pi> \
         ARC_SESSION=<id> ARC_MODEL=<model[#effort]>

  Every event records who wrote it. Nothing refuses an undeclared identity by
  default — the write succeeds and arc records an actor nobody claimed, which
  is discovered later by a reader who cannot tell whose work it was.

  `arc env` only detects a harness that exports its own session variable, and
  not every harness does; it exits non-zero and prints the export template
  when it cannot. That is the normal path for setting them by hand, not a
  failure. `[policy] require_declared_actor` makes an undeclared identity a
  refusal instead of a record.

ORIENT (start here, in this order)
  arc catchup            Live state: ledger queue, journal backlog, lanes.
  arc journal open       The actionable backlog — work waiting for a session.
  arc journal verified <file> [--note <text>]
                         Record a source check at the project's anchor head.
  arc resume <change>    One change's brief, live state, and journal context.
  arc inbox              Lead-facing queue across open changes.
  arc workspace backlog  The same question asked of every project at once.

  Work waiting for this project lives in two places. The ledger holds changes
  already open; the journal holds everything not yet opened as one. An empty
  inbox does not mean an empty queue — check both, which is what `catchup` does.

  Every inbox bucket is an independent predicate, so a change in a state none
  of them anticipates would land nowhere and the queue would read as empty
  while work waited. The `unclassified` bucket catches exactly that and names
  the reason. A row there is a gap in the derivation rather than a resting
  place: it means arc failed to classify open work, and it is worth reporting.

  Every command above answers for the project you are standing in. Some
  questions are comparisons — which project to open next, what has waited
  longest, where a verdict is the only thing missing — and those are answerable
  only across projects, which is what `workspace backlog` is for.

  A source check is worth keeping only with the revision it checked: `journal
  verified <file>` records that fact, and the open queue says so on the row.
  Nothing more is said while the stamp holds — that is what verified means;
  the row speaks up once the anchor head has moved past it.

SETTLE A QUESTION (before it is work)
  arc journal note <topic> --kind discussion --body-file -
  arc journal position <file> --body-file - [--stance <for|against|amend>]
  arc journal question <file> --placement opening|closing --option <a> --option <b> --body-file -
  arc journal questions                Every question waiting on a person.
  arc journal scaffolds [--show <n>]   What a write prepends, before it does.
  arc journal answer <file> --question <id> --option <choice> --body-file -
    --other "<answer>"                 Settle it outside the options offered.
  arc journal discussion <file>        Read stances, branches, and open questions.
  arc journal consume <file> --outcome done --decision <decision>
  arc journal <kind> <topic> --body-file -   One verb per kind; `arc journal
                                       --help` is the registry. `arc fr` is
                                       the top-level alias for one of them.
  arc begin <slug> --from-journal <file>

  The ledger records what happened and what is allowed; the journal records
  what people think. A question with more than one defensible answer belongs in
  a discussion before it belongs in a change — positions carry the model and
  harness that argued them, so a decision can be read later by who reached it
  and on what grounds, rather than surviving only in one session's transcript.

  A proposal with one defensible answer is not a discussion, it is a feature
  request: `arc fr` files it, `arc journal open` lists it back, and `begin
  --from-journal` turns it into work when its turn comes.

  Every kind is a verb under `arc journal`, so the closed set is legible from
  `--help` rather than from a flag's values; `note --kind <k>` remains the
  same write underneath. `discussion` has no write verb on purpose — one is
  argued and read far more often than created, so `position`, `question`,
  `answer` and the `discussion` summary are its surface, and `note --kind
  discussion` opens one.

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

  A question is the part no model should settle. arc records that one is
  waiting; it never asks. Raising it is the agent's job, through whatever
  prompt its harness offers — `arc journal questions --json` carries the
  options and the branches already argued, which is everything a prompt needs
  — and `answer` is where the reply comes back. When the answer is none of
  the options, `--other "<answer>"` records it in the answerer's own words
  and marks that the menu was stepped outside, because a menu somebody had to
  leave was framed wrong and the next one should know. Placement decides when:
  `--placement opening` before anyone argues, so every participant starts from
  the same premise, or `closing` once the argument is in — never mid-argument,
  which would make you watch a run you delegated. Argue a closing question on
  both sides first (`position <file> --body-file - --question <id> --option <opt>`)
  and the answer picks between explored branches instead of labels.

RUN A CHANGE
  arc begin <slug> --profile <p>     Open a change: branch + worktree + record.
    --from-journal <file>            Open it from a journal item, consuming it.
    --blocked-by <change> --tag <t>  Declare a chain up front, at planning time.
    --no-worktree                    Take over this checkout, if it can be taken.
  arc claim / stage / release-claim  Advisory liveness while implementing.
  arc snapshot                       Record the current head as a patchset.
  arc verify --gate <name>           Run a declared gate; record the evidence.
  arc verify --command <cmd>         Same, for an ad hoc probe.
  arc done                           Snapshot, run every gate, print check state.
  arc review --verdict <v>           Record a verdict (+ --findings-json -).
    --provisional <why>              It gates, and owes a second judgment.
    --relation corroborates          Support the standing verdict, not replace it.
  arc resolve                        Dispose of a finding.
    --evidence-event <id>            Cite the verification run that justifies it.
  arc check                          Integration preflight; exit code names the blocker.
  arc integrate [--tag <t>]          Guarded --no-ff merge once the gates are green.
  arc audit <change> --verdict <v>   Review an already-integrated revision.
  arc close                          Terminal outcome arc did not merge itself.

  `--no-worktree` means in place, not nowhere: a clean checkout already on the
  target is checked out onto the new branch and recorded as the change's
  worktree, so the next command infers the change without being told. A dirty
  checkout, or one standing elsewhere, is left exactly as it was — the change
  still opens, and arc prints the Git command that finishes the switch.

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
  arc journal handoff <topic> --body-file -    Stopped midstream.
  arc journal conclusion <topic> --body-file - Finished the thing.
  arc journal memory <topic> --body-file -     Learned a durable fact.
  arc journal consume <file> --outcome done    Drain what you resolved.
  arc journal archive --consumed               Move the drained to cold storage.

  ORIENT above is fed entirely by these writes: `catchup` surfaces memories,
  `journal open` lists what a handoff parked.

  Which one is decided by how the work ended, not by how much there is to say.
  Stopped midstream — a handoff, naming branch, worktree, current head, what
  is done, what is open, the next action, and the gate command. Finished — a
  conclusion. Learned something that will be true next month and is not in the
  code — a memory, which every later `catchup` shows. Never label unfinished
  work a conclusion.

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

  Independence is judged against the patchset a reviewer actually read, not
  against whatever is newest. Otherwise a later snapshot by somebody else
  would retroactively turn a self-review into an independent one.

  Which changes need one is the project's call, declared in
  `.arc/policy.toml` under `[danger] paths`. A change touching a declared
  path needs a verdict from somebody other than its author; elsewhere a
  self-recorded verdict satisfies the gate. Declare nothing and the gate stays
  uniform, exactly as before. `arc begin --dangerous` raises a single change
  whatever it turns out to touch, and nothing lowers it afterwards — a project
  decides its own gate in advance, rather than a change deciding it under
  pressure to ship. `arc check` names the rule that fired, and `arc doctor`
  reports a declared literal that can never match — one that names nothing, or
  a directory, since declared paths are matched against changed files.

  If no independent verdict is available, integrate with
  `arc integrate <change> --audit-debt "<why>"`. The debt can stand in for an
  absent verdict or rescue a self-approval rejected by repository policy. It
  binds to the exact patchset head declared, so new work needs a new
  declaration — it does not excuse the rest of the change's life.

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

    arc inbox                       audit-owed bucket, including closed changes
    arc catchup                     the same, with each reason
    arc query --audit-debt          change IDs alone, for scripting
    arc query --provisional         approvals still owed corroboration
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

RULES THAT CHANGE WHAT YOU DO
  - A verdict binds to the exact approved patchset head. Any new commit makes
    the approval stale until a fresh verdict on a new snapshot.
  - The ledger gates; claims, stages, and lanes are advisory signals, never locks.
  - Give every concurrent writer its own branch and worktree. Integration and
    shared refs belong to the lead alone.
  - An executor's first act is `arc config --check-writable`; a nonzero exit
    means stop, not work around. It releases its claim whenever it stops, for
    any reason, so a live claim always means live work.
  - An executor that hangs never reaches its own release. Before leaving a
    delegated run unattended, arm `arc watch <change> --until stalled`; silence
    is unknown, not healthy. `arc rescue <change> --take` recovers it.
  - `arc watch <change> --until` accepts `snapshot`, `stalled`, `reviewed`,
    `approved`, `gates-green`, `ready`, `blocked`, `brief-recorded`,
    `integrated`, and `closed`. `approved` returns on the latest approving
    verdict, including a provisional approval and its recorded reason;
    `gates-green` checks every required gate at the current head; `blocked`
    and `brief-recorded` name the events that recorded those facts.
  - The journal lives outside the repo, so worktrees stay clean. Cross-session
    context goes there, never into tracked files.
  - arc holds no routing opinion. It records the --actor, --harness, and --model
    it is given; who to delegate to is the caller's policy, not arc's.

  arc <command> --help for any command's full contract. arc --help for all of them.
"#;

pub fn print() {
    print!("{GUIDE}");
}
