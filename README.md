# arc

Change, review, and integration state over plain Git for agentic coding arcs.
Git owns content, branches, and history; `arc` owns the collaboration objects
Git deliberately lacks — changes, patchsets, review findings, verdicts,
verification evidence, holds, and a guarded merge — as an append-only local
ledger that every worktree and every AI harness of one repository shares. No
forge, no daemon, no database, no web UI. It exists so that the mechanical
invariants of a multi-agent workflow (approval bound to an exact patchset,
blocking findings replayed correctly, holds enforced across sessions, merges
guarded against unreviewed commits) live in code instead of in prompt
discipline.

## Install

arc builds to a single binary and is Unix-only, because its safety guarantees
rest on POSIX semantics: `0700` private directories, atomic hard-link event
publication, and process-group kill for gate timeouts.

```sh
cargo install --path .          # from a checkout of this repository
```

Released versions are the calendar date of publication in the `YYYY.M.D`
shape, written without leading zeros. One release is cut per date, so a
second release waits for the next one.

Optional shell completions and man page:

```sh
arc completions <bash|zsh|fish> > <completion-path>   # e.g. ~/.zfunc/_arc
arc mangen <dir>                                      # writes <dir>/arc.1
```

## One change, end to end

Say who you are first. Every event records an actor, and by default nothing
refuses an undeclared one — the write succeeds and the ledger names whoever
configured the checkout.

```sh
eval "$(arc env)"                      # detect harness, session, and model
export ARC_ACTOR=<name> ARC_HARNESS=<claude|codex|opencode|pi>
```

Then open a change, work it, and land it:

```sh
arc catchup                            # what is waiting: ledger + journal
arc begin radio-refill-fix --title "Keep radio refill from restarting playback"
# → branch arc/radio-refill-fix + worktree ~/.worktrees/<repo>-radio-refill-fix

cd ~/.worktrees/<repo>-radio-refill-fix
arc brief radio-refill-fix --body-file executor-spec.md   # the implementation contract
arc show                               # brief, state, findings, next action
arc stage implementing --claim         # advisory liveness while working
# ... implement, commit ...

arc done                               # snapshot → run every gate → check

# a reviewer, in any harness or session:
arc diff radio-refill-fix --findings   # the patchset diff, with anchor drift
arc review radio-refill-fix --snapshot --verdict changes-requested \
  --cause executor --body "The concurrency path still permits a stale commit." \
  --findings-json -                    # a JSON array of findings on stdin

# ... fix, then:
arc resolve radio-refill-fix f01ABC... --status resolved --commit HEAD
arc review radio-refill-fix --snapshot --verdict approved

arc check radio-refill-fix             # exit 0 = ready; any other code names the blocker
arc integrate radio-refill-fix --cleanup
```

`arc integrate` merges only when, atomically checked: the head equals the
approved patchset head, no blocking finding is open, every required gate is
green at that exact head, and no hold is active. A verdict binds to the exact
patchset head it approved, so any new commit makes it stale.

## Where the rest is

**The CLI is the whole of what a session needs.** `arc` with no arguments
prints the workflow guide — what the ledger owns, the command lifecycle in
order, how to pick a profile, the exit codes, and the invariants that change
what a session should do. `arc <verb> --help` is each command's full contract.
Nothing an agent must know to act correctly lives only in a document.

```sh
arc                    # the workflow guide
arc <verb> --help      # one command's contract
arc catchup            # what is waiting right now
```

`docs/` is the long form, as verbose as it needs to be, one page per area:

| Page | What it covers |
| --- | --- |
| [QUICKSTART](docs/QUICKSTART.md) | A foreign repository from install to a first integrated change |
| [changes](docs/changes.md) | The model, the lifecycle, patchsets, briefs, context awareness |
| [gates](docs/gates.md) | Gate declaration, evidence, trees, `verify --against`, falsification, exit codes |
| [review](docs/review.md) | Verdicts, dispositions, policy, dangerous surfaces, coverage, debt, audits |
| [journal](docs/journal.md) | Artifacts, discussions, questions, claims, amendments, lanes, the spool |
| [delegation](docs/delegation.md) | Execution roles, dependencies, chains, runs, rounds, deferrals |
| [forks](docs/forks.md) | Worktrees outside the change lifecycle, and what they cost |
| [workspace](docs/workspace.md) | Cross-project views, scaffolds, acceptance probes, restack, bundles |
| [forge](docs/forge.md) | Recording and validating the forge facts an agent observed |
| [identity](docs/identity.md) | Actor, harness, session, model, and acting for a subject |
| [configuration](docs/configuration.md) | Config, sandbox, storage guarantees, Git hooks |
| [history](docs/history.md) | Rewrites, and how derived readings follow revisions forward |
| [schemas](docs/schemas.md) | Every schema version, and which are commitments |
| [changing arc](docs/changing-arc.md) | Rules for working on arc itself, non-goals, roadmap |

## License

[Unlicense](UNLICENSE) — public domain.
