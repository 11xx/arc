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

ORIENT (start here, in this order)
  arc catchup            Live state: ledger queue, journal backlog, lanes.
  arc journal open       The actionable backlog — work waiting for a session.
  arc resume <change>    One change's brief, live state, and journal context.
  arc inbox              Lead-facing queue across open changes.
  arc workspace backlog  The same question asked of every project at once.

  Work waiting for this project lives in two places. The ledger holds changes
  already open; the journal holds everything not yet opened as one. An empty
  inbox does not mean an empty queue — check both, which is what `catchup` does.

  Every command above answers for the project you are standing in. Some
  questions are comparisons — which project to open next, what has waited
  longest, where a verdict is the only thing missing — and those are answerable
  only across projects, which is what `workspace backlog` is for.

SETTLE A QUESTION (before it is work)
  arc journal note <topic> --kind discussion --body-file -
  arc journal position <file> --body-file -
  arc journal question <file> --placement opening|closing --option <a> --option <b> --body-file -
  arc journal answer <file> --question <id> --option <choice> --body-file -
  arc journal discussion <file>        Read stances, branches, and open questions.
  arc journal consume <file> --outcome done --decision <decision>
  arc begin <slug> --from-journal <file>

  The ledger records what happened and what is allowed; the journal records
  what people think. A question with more than one defensible answer belongs in
  a discussion before it belongs in a change — positions carry the model and
  harness that argued them, so a decision can be read later by who reached it
  and on what grounds, rather than surviving only in one session's transcript.

  Resolve a discussion as done, or promote the still-open discussion to work —
  never both. `consume --outcome done` cites a terminal decision; `begin
  --from-journal` consumes the open discussion as superseded by the change.

  A question is the part no model should settle. Placement decides when:
  `--placement opening` before anyone argues, so every participant starts from
  the same premise, or `closing` once the argument is in — never mid-argument,
  which would make you watch a run you delegated. Argue a closing question on
  both sides first (`position <file> --body-file - --question <id> --option <opt>`)
  and the answer picks between explored branches instead of labels.

RUN A CHANGE
  arc begin <slug> --profile <p>     Open a change: branch + worktree + record.
    --from-journal <file>            Open it from a journal item, consuming it.
    --blocked-by <change> --tag <t>  Declare a chain up front, at planning time.
  arc claim / stage / release-claim  Advisory liveness while implementing.
  arc snapshot                       Record the current head as a patchset.
  arc verify --gate <name>           Run a declared gate; record the evidence.
  arc verify --command <cmd>         Same, for an ad hoc probe.
  arc done                           Snapshot, run every gate, print check state.
  arc review --verdict <v>           Record a verdict (+ --findings-json -).
  arc resolve                        Dispose of a finding.
  arc check                          Integration preflight; exit code names the blocker.
  arc integrate [--tag <t>]          Guarded --no-ff merge once the gates are green.
  arc audit <change> --verdict <v>   Review an already-integrated revision.
  arc close                          Terminal outcome arc did not merge itself.

PROFILES (--profile, default local)
  direct   Bounded, reversible, one session and checkout. Implement, verify,
           review the diff, commit. No worktree or journal topic for uniformity.
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

  If no independent verdict is available, integrate with
  `arc integrate <change> --audit-debt "<why>"`. The debt can stand in for an
  absent verdict or rescue a self-approval rejected by repository policy. It
  binds to the exact patchset head declared, so new work needs a new
  declaration — it does not excuse the rest of the change's life.

  Coming back to owed work:

    arc inbox                       audit-owed bucket, including closed changes
    arc catchup                     the same, with each reason
    arc query --audit-debt          change IDs alone, for scripting
    arc diff <change> --integrated  the exact range that landed, for an audit
    arc audit <change> --verdict <v>          discharge it
    arc findings <change> --audit             what an audit raised

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
  - The journal lives outside the repo, so worktrees stay clean. Cross-session
    context goes there, never into tracked files.
  - arc holds no routing opinion. It records the --actor, --harness, and --model
    it is given; who to delegate to is the caller's policy, not arc's.

  arc <command> --help for any command's full contract. arc --help for all of them.
"#;

pub fn print() {
    print!("{GUIDE}");
}
