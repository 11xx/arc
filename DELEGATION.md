# Delegation routing

Use this document when writing or interpreting an arc plan. The model/effort
ladder selects an executor; this matrix determines how work reaches that
executor. Always identify the actor that will read the prompt before choosing a
mechanism.

This file is the canonical mechanism-routing reference: harness instruction
sets (global CLAUDE.md, the /arc skill, executor prompt templates) should
reference it rather than restate it. Model and effort selection stays with the
model/effort ladder; this document owns how the selected executor is invoked
and what context it receives.

Machine-readable form: `agent-routing delegation` (data at
`ai-agent-skills/manifests/routing/delegation.toml`; the model/effort ladder
is `agent-routing models`). Keep this prose and that data in sync when either
changes.

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
2. Claim the change and record typed progress stages in the assigned worktree.
3. Implement in the assigned worktree.
4. Run the specified build and test gates.
5. Commit the scoped changes and run `arc snapshot <change>`.
6. Signal: "Arc improvements ready for review."

DO NOT run `codex exec`, delegate to another harness, merge, or integrate.
```

### Codex

Do:

- Treat an imported implementation plan as local work unless it explicitly
  names Codex as an orchestrator.
- Import the bundle, inspect the repository and spec, claim the change, record
  typed stages, implement in the assigned worktree, verify, commit, and
  snapshot.
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

## Native subagents and context routing

Harness-native subagent features are not a substitute for this matrix. As of
2026-07, Codex (GPT-5.6) subagents by default inherit the whole conversation
plus the parent's model family and reasoning level. Both defaults conflict
with the routing policy:

- Effort inheritance is the wrong direction. A high-effort orchestrator
  spawning same-effort context gatherers is the most expensive possible
  configuration; the ladder routes gathering and extraction down (low/Luna)
  and implementation to the cheapest tier the spec quality allows (Terra
  high for planned slices).
- Whole-conversation inheritance is unscoped context transfer. It pays for
  the entire session history in every child and buries the task signal.
  Delegated work gets a curated, self-contained brief: the spec, the done
  condition, the gates, and nothing else. That is what arc bundles and
  thread specs are for.

So: do not route work through native subagent defaults. Invoke executors
explicitly (`codex exec` with a chosen model and effort, an arc bundle plus
spec, or an orchestration script) where model, effort, and context are all
stated. Revisit if a harness ships per-subagent model/effort/context
controls and they prove reliable.

The strongest known pattern is programmatic orchestration: generate a
dedicated one-off script that spawns each agent with an explicit prompt,
model, effort, and input set, and encodes the control flow (fan-out,
gates, joins) as code rather than conversation. Claude Code's workflow
scripts work this way. An arc chain is the ledger-backed equivalent:
deterministic dependency order, curated per-executor specs, machine-checked
gates. Prefer either over implicit inheritance.

## Executor backends (harness adapters)

The matrix above routes *roles*; this section routes *invocations*. A
backend is a CLI a lead can drive programmatically to run a selected model.
Each backend has an adapter contract — the facts a lead must know before
composing a command. A backend is routable only after every contract row
has been probed and recorded here; until then it is "unprobed" and must not
be selected.

Adapter contract (probe checklist for onboarding any new harness):

1. **Non-interactive entrypoint** — the `claude -p` / `codex exec`
   equivalent, and how the prompt is passed (argument, stdin, file).
2. **Model and effort selection** — explicit flags; never rely on the
   harness default or fuzzy matching in delegation commands.
3. **Working directory** — a `-C`-style flag, or `cd` before invoking.
4. **Final-message capture** — where the answer lands (stdout, `-o` file)
   and how noisy the channel is.
5. **Isolation** — OS sandbox, tool allowlist, or nothing; what write mode
   actually fences.
6. **Clean-room switches** — how to suppress ambient config, sessions,
   extensions, and context files so the curated brief is the whole input.
7. **Resume** — continuing a prior run for follow-ups.
8. **Quota identity** — which subscription pool the backend draws from and
   the exact usage-limit error signature. Two backends on one pool are not
   fallbacks for each other's limits.

### Verified backends (2026-07-17)

| Contract | codex CLI (`codex exec`) | pi (`pi -p`), v0.80.10 |
| --- | --- | --- |
| Entrypoint | `codex exec "<prompt>"` | `pi -p "<prompt>"`; `@file` args inline files (specs) |
| Model | `-m gpt-5.6-{sol,terra,luna}` | `--provider openai-codex --model gpt-5.6-{sol,terra,luna}` |
| Effort | `-c model_reasoning_effort="low\|medium\|high\|xhigh"` | `--thinking off\|minimal\|low\|medium\|high\|xhigh\|max` (or `:level` model suffix) |
| Working dir | `-C <dir>` | none — `cd` first |
| Capture | `-o <file>`; stdout is the full transcript (noisy) | stdout is exactly the final message |
| Isolation | OS sandbox: `-s read-only` / `-s workspace-write` (+ `writable_roots`) | **none**; read-only ≈ `--tools read,grep,find,ls` (no bash, so no git/shell inspection); write mode is unsandboxed user access |
| Clean-room | curated prompt; reads repo AGENTS.md | `--no-session --no-extensions --no-skills --no-context-files` |
| Resume | `codex exec resume --last` | `--session <id>` / `-c` / `--fork` |
| Quota | ChatGPT/Codex subscription; "You've hit your usage limit … try again at HH:MM" | **same** ChatGPT/Codex pool via the `openai-codex` login — not a quota fallback for codex CLI limits |

Backend selection between the two GPT routes:

- **codex CLI is the default for GPT work**, and mandatory-by-default for
  write mode: its OS sandbox is a real fence; pi's write mode has none.
  Route write-mode work through pi only when the codex CLI itself is the
  blocker (broken install, incompatible flag, harness without codex
  access), and then only inside a dedicated worktree with explicit
  scope fencing in the prompt.
- **pi's niche is provider plurality and lead portability**: one adapter
  reaches every provider the user has logged in (today only
  `openai-codex`; Anthropic/Copilot/xAI logins would appear in the same
  `--provider` flag), and any harness that can run a shell can drive it.
  A non-Claude, non-codex lead delegating to GPT goes through pi.
- pi "read-only" (`--tools read,grep,find,ls`) is tool-level, not
  OS-level, and excludes bash entirely: file reading and search work,
  `git log`/build commands do not. Investigations needing shell stay on
  `codex exec -s read-only`.
- Liveness, watchdog, and one-prompt-source stdin rules below apply to
  every backend. pi's stdin behavior under `-p` is unprobed: apply the
  same `< /dev/null` discipline defensively.
- opencode: **unprobed** — run the contract checklist before first use.

## Launcher hygiene and liveness

Rules from the 2026-07-17 wave-2 stall postmortem (a `codex exec` blocked
forever reading a stdin pipe another heredoc had opened; no channel showed
the difference between hung and working for seven minutes).

Launching a non-interactive executor:

- Give `codex exec` exactly one prompt source. Prompt as an argument means
  stdin MUST be closed: append `< /dev/null`. Prompt via stdin means a
  single heredoc feeding codex and nothing else. Never let an executor
  launch share a compound command with any other stdin consumer or
  producer (heredocs, pipes) — journal appends and launches are separate
  commands.
- Redirect output to a log file per executor; the log is the liveness
  probe.

Arm two watchdog tiers at launch, always:

- **First-output (seconds):** the executor banner appears within seconds
  of a healthy start. Log still near-empty after 60 s = launch failure;
  kill and relaunch, do not wait.
- **Progress (minutes):** a log that stops growing for longer than the
  task plausibly needs is a wedge, not deep thought. Check interim output
  before assuming work is happening; a hung process and a working one are
  otherwise indistinguishable.

Executors claim immediately and record typed progress with their native
identity:

```sh
ARC_HARNESS=codex ARC_SESSION="${CODEX_THREAD_ID:?}" arc claim <change>
ARC_HARNESS=codex ARC_SESSION="${CODEX_THREAD_ID:?}" arc stage <change> started
# after reading the executor spec
ARC_HARNESS=codex ARC_SESSION="${CODEX_THREAD_ID:?}" arc stage <change> spec-read
```

Continue with `implementing`, `verifying`, or `blocked-on --note <reason>` as
work changes state. Repeating a stage is a heartbeat; changing it is progress.
Leads consume raw events with `arc events --follow --change <change>`, inspect
the time-derived claim object in `arc status`, or wait with
`arc watch <change> --until stalled|snapshot|ready|integrated|closed`. A stale
claim is still live and owned; an expired one is not. Claims remain advisory:
an identified lead may release a live foreign claim, and `arc integrate` warns
about but does not block on one.

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

Codex imports the bundle, reads the thread spec, claims the change, records
typed stages, edits and tests in its worktree, makes scoped commits, runs
`arc snapshot`, and signals that the arc is ready for review. Claude later
reviews and integrates.

### Incorrect: the implementation prompt merges

```markdown
After tests pass, run `arc integrate --cleanup` and merge to master.
```

The executor does not own that decision. It must stop after the snapshot so
Claude can review the exact patchset.

### Correct: `/arc` coordinates a chain

A **chain** is a tagged blocked-by series: ready siblings may run in parallel,
while dependent members wait mechanically for their prerequisites. The lead
opens each change with its declared blocker, assigns local-only executor
prompts, and starts downstream implementation only when the ledger reports it
ready. Claude reviews and integrates each approved patchset.

### Incorrect: `/arc` shares a checkout

Two implementers are assigned to the same branch or worktree, or a downstream
executor is told to bypass a blocker. Give each writer isolated ownership and
honor the ledger state instead.
