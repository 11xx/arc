# arc

Persistent context and guarded workflow state over plain Git for agentic coding arcs.

`arc` is a local context and workflow-state manager for agentic coding, built
over plain Git. It gives agents and sessions a persistent shared understanding
of what is being worked on, what has been learned or decided, what remains to
be done, and what is actually safe to integrate.

Git owns content, branches, and history; `arc` owns the collaboration and
execution state Git deliberately lacks: changes and their patchsets, review
findings and verdicts, verification evidence, and guarded integration.

A cold session reconstructs the state of work with `arc catchup`, from the
journal and change ledger shared across worktrees and harnesses. Review
verdicts bind to exact patchsets, and integration checks findings, holds, and
required verification gates before merging. arc is a single local CLI that
runs no daemon and makes no network call: every transport it offers is a
file the caller moves.

## Install

arc builds to a single binary and is Unix-only, because its safety guarantees
rest on POSIX semantics: `0700` private directories, atomic hard-link event
publication, and process-group kill for gate timeouts.

```sh
cargo install --git https://github.com/11xx/arc --locked
```

From a checkout, run `cargo install --path . --locked`. The package is named
`arc-ledger`; the installed command is `arc`. The `arc-ledger` release on
crates.io is a September 2026 snapshot.

## Pick up where a session left off

```sh
arc catchup                  # project state and work waiting for attention
arc journal open             # decisions to settle and work to pick up

# keep a concrete next step after the chat ends
arc journal todo parser-diagnostics --body-file - <<'EOF'
# Explain malformed configuration values

Report the key and source location when a value cannot be parsed.
EOF
```

Discussions, plans, and handoffs keep the context behind the work, and
`arc begin <slug> --from-journal <file>` turns a queued item into a tracked
change.

## One change, end to end

```sh
arc begin radio-refill-fix --title "Keep radio refill from restarting playback" \
  --worktree ../radio-refill-fix
cd ../radio-refill-fix
arc brief radio-refill-fix --body-file - <<'EOF'   # the implementation contract
Refill the queue without restarting the track that is playing.
Cover it with a regression test.
EOF
# ... implement, commit ...
arc done                               # snapshot, run the gates the profile requires, check

# a reviewer, in any harness or session:
arc diff radio-refill-fix --findings
arc review radio-refill-fix --snapshot --verdict approved

arc check radio-refill-fix             # show what blocks integration
arc integrate radio-refill-fix --cleanup
```

`arc integrate` merges only the patchset a verdict approved: a new commit
makes the approval stale, unless a recorded rewrite changed nothing but
signatures. It also requires every gate the profile names to be green for the
tree the merge would ship, no open blocking finding, no active hold, and every
prerequisite integrated. `--debt <reason>` integrates without a verdict and
records the review still owed; policy decides where an independent verdict is
required instead.

[arc-model](https://github.com/11xx/arc-model) models these authorization
rules independently and checks the `arc` binary against them on generated
histories.

## See what a change knew

A merged diff says what changed, not what the work read, what it rejected, or
what review it shipped on. `arc explain` answers that from the ledger and
journal, and marks each row by how it is known — recorded, declared, inferred,
absent, or unavailable — so nothing reads as stronger than its source:

```sh
arc explain radio-refill-fix           # contract, context, reads, alternatives, evaluation, coverage
arc explain radio-refill-fix --json    # the same, as arc-explain/1
```

When several attempts answer one brief, `arc candidate register` records each
without opening a change, and `arc candidate select` validates a named choice
before promoting it into an ordinary patchset reviewed like any other.

## Where the rest is

`arc` with no arguments prints the workflow guide, and `arc <verb> --help` is
each command's full contract.

## License

[Unlicense](UNLICENSE) — public domain.
