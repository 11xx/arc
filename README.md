# arc

Persistent context and guarded workflow state over plain Git for agentic coding arcs.

`arc` is a local context and workflow-state manager for agentic coding, built
over plain Git. It gives agents and sessions a persistent shared understanding
of what is being worked on, what has been learned or decided, what remains to
be done, and what is actually safe to integrate.

Git owns content, branches, and history; `arc` owns the collaboration and
execution state Git deliberately lacks: changes, patchsets, briefs, handoffs,
findings, verdicts, verification evidence, holds, and guarded integration.

A cold session reconstructs the state of work with `arc catchup`, from the
journal and change ledger shared across worktrees and harnesses. Review
verdicts bind to exact patchsets, and integration checks findings, holds, and
required verification gates before merging. arc is a single local CLI: it
makes no network call and runs no daemon.

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
eval "$(arc env)"                      # record which harness, session, and model act
arc begin radio-refill-fix --title "Keep radio refill from restarting playback"
cd ~/.worktrees/<repo>-radio-refill-fix
arc brief radio-refill-fix --body-file spec.md   # the implementation contract
# ... implement, commit ...
arc done                               # snapshot, run every gate, check

# a reviewer, in any harness or session:
arc diff radio-refill-fix --findings
arc review radio-refill-fix --snapshot --verdict approved

arc check radio-refill-fix             # exit 0 = ready; any other code names the blocker
arc integrate radio-refill-fix --cleanup
```

`arc integrate` merges only when the head equals the approved patchset head,
no blocking finding is open, every required gate is green at that head, and
no hold is active, all checked atomically. Any new commit makes an approval
stale.

## Where the rest is

`arc` with no arguments prints the workflow guide: what the ledger owns, the
command lifecycle, profiles, exit codes, configuration files, and schemas.
`arc <verb> --help` is each command's full contract.

Session transcripts are read through
[tapes](https://github.com/11xx/tapes). The rules that decide what
integration permits are modelled independently in
[arc-model](https://github.com/11xx/arc-model), which replays generated
histories through the `arc` binary and compares its answers with the model's.

## License

[Unlicense](UNLICENSE) — public domain.
