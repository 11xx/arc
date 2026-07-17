# Delegation routing

Use this document when writing or interpreting an arc plan. The model/effort
ladder selects an executor; this matrix determines how work reaches that
executor. Always identify the actor that will read the prompt before choosing a
mechanism.

## Routing matrix

| Actor | Task | Mechanism | Rules |
| --- | --- | --- | --- |
| Claude | Plan an arc chain | Write the arc JSON export and a detailed thread spec | Make dependencies and ownership explicit. Generated executor prompts stop before review, merge, or integration. |
| Claude | Delegate implementation to Codex | Use `/plan-for-codex`; Claude may invoke `codex exec` | Put `codex exec` only in instructions Claude itself will run. Do not ask the Codex executor to delegate again. |
| Claude | Review and merge | Review `base..head`, record the verdict, then use `arc integrate` when every gate passes | Do not skip review or merge automatically from an implementation prompt. |
| Codex | Execute a local arc | Import the bundle, read the thread spec, and work locally in the assigned worktree | Implement, verify, commit, run `arc snapshot`, and stop for review. Do not run `codex exec`, delegate, merge, or integrate. |
| Codex | Orchestrate sub-Codex work (rare and explicit) | Run `codex exec` with a self-contained plan and wait for its result | Do this only when the prompt explicitly assigns Codex the orchestrator role. Preserve Claude's review and merge boundary. |
| `/arc` skill | Coordinate the workflow | Use the `arc` CLI ledger, worktrees, blockers, snapshots, gates, reviews, and integration checks | Assign one writer per branch/worktree. Give Codex local-only executor prompts. The lead owns dependency order and integration. |

## Golden rule

Never tell an executor to delegate to itself.

Before generating a prompt, classify its recipient:

- **Codex executing:** local implementation only; no `codex exec`.
- **Claude delegating to Codex:** use `/plan-for-codex`; Claude may run the
  generated `codex exec` command.
- **Claude executing:** use the `/arc` workflow and the `arc` CLI directly.
- **Codex orchestrating:** use `codex exec` only when that rare role is named
  explicitly, never by implication.

## Actor rules

### Claude

Do:

- State whether a prompt is for Claude or for Codex before including commands.
- For a Codex executor, say that execution is local to the current harness and
  assigned worktree.
- Provide the bundle location, thread spec, done condition, verification gates,
  and the exact ready-for-review signal.
- Review the final `base..head` diff and retain control of `arc integrate`.

Do not:

- Put `codex exec` in a prompt that Codex will import and execute.
- Assume Codex will create another delegation layer.
- Tell an implementer to merge to the target branch.
- Treat a generated implementation prompt as approval to skip review.

A Codex executor prompt should use this shape:

```markdown
You're in a Codex harness. This is LOCAL execution only.

1. Import the arc bundle and read the thread spec.
2. Implement in the assigned worktree.
3. Run the specified build and test gates.
4. Commit the scoped changes and run `arc snapshot <change>`.
5. Signal: "Arc improvements ready for review."

DO NOT run `codex exec`, delegate to another harness, merge, or integrate.
```

### Codex

Do:

- Treat an imported implementation plan as local work unless it explicitly
  names Codex as an orchestrator.
- Import the bundle, inspect the repository and spec, implement in the assigned
  worktree, verify, commit, and snapshot.
- Stop before review and integration and report the commits and gate results.
- Flag a prompt as misrouted if it tells a local Codex executor to run
  `codex exec`.

Do not:

- Spawn another Codex from a normal executor prompt.
- Write to another agent's branch or worktree.
- Merge into the target branch or run `arc integrate` when Claude owns review.
- Work around an unclear actor assignment; ask the orchestrator to correct it.

### `/arc` skill

Do:

- Select the workflow profile and record the actor, executor, reviewer,
  integration owner, dependencies, and holds.
- Use the `arc` ledger as the source of truth for snapshots, gates, findings,
  verdicts, blockers, and integration readiness.
- Give every concurrent writer a separate branch and worktree.
- Emit local-only instructions when Codex is the implementer.

Do not:

- Confuse a workflow role with a model-routing decision.
- Put delegation commands in the executor's own prompt.
- Let an implementation agent silently become the reviewer or integration
  owner.
- Merge while a hold, blocker, stale approval, failed gate, or open blocking
  finding remains.

## Correct and incorrect delegation

### Correct: Claude invokes Codex

Claude prepares a `/plan-for-codex` handoff and runs `codex exec` itself. The
nested prompt tells Codex what to implement locally and where to report the
result.

### Incorrect: Codex is told to invoke itself

```markdown
You are the Codex executor. Run `codex exec` to implement this plan.
```

This creates an unnecessary nested harness, can exceed the parent harness's
timeout, and may leave work half-finished. Replace it with local worktree
instructions.

### Correct: Codex completes an arc implementation

Codex imports the bundle, reads the thread spec, edits and tests in its
worktree, makes scoped commits, runs `arc snapshot`, and signals that the arc is
ready for review. Claude later reviews and integrates.

### Incorrect: the implementation prompt merges

```markdown
After tests pass, run `arc integrate --cleanup` and merge to master.
```

The executor does not own that decision. It must stop after the snapshot so
Claude can review the exact patchset.

### Correct: `/arc` coordinates a dependency chain

The lead opens each change with its declared blocker, assigns local-only
executor prompts, and starts downstream implementation only when the ledger
reports it ready. Claude reviews and integrates each approved patchset.

### Incorrect: `/arc` shares a checkout

Two implementers are assigned to the same branch or worktree, or a downstream
executor is told to bypass a blocker. Give each writer isolated ownership and
honor the ledger state instead.
