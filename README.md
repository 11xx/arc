# arc

Change, review, and integration state over plain Git for agentic coding
arcs. Git owns content, branches, and history; `arc` owns the
collaboration objects Git deliberately lacks — changes, patchsets,
review findings, verdicts, verification evidence, holds, and a guarded
merge — as an append-only local ledger that every worktree and every AI
harness of one repository shares. No forge, no daemon, no database, no
web UI.

It exists so that the mechanical invariants of a multi-agent workflow
(approval bound to an exact patchset, blocking findings replayed
correctly, holds enforced across sessions, merges guarded against
unreviewed commits) live in code instead of in prompt discipline.
The `/arc` skill layer decides *what* to do; this CLI guarantees *what
happened and what is allowed*.

## The model

- A **change** is the unit of work (Gerrit's sense): a stable ID and
  slug that survive across revisions, tied to one branch and one
  integration target.
- A **patchset** is an immutable base/head snapshot of that branch,
  recorded with `arc snapshot`. Reviews and approvals bind to patchsets,
  never to moving branch names.
- **Findings, verdicts, comments, replies, dispositions,
  verifications, holds, and closures** are append-only JSON events under
  `<git-common-dir>/arc/`, one file per event, created exclusively so
  concurrent writers never clobber each other. The ledger is
  authoritative; everything else is a view.
- **Approval staleness is structural:** a verdict is valid only while
  the branch head equals the approved patchset head. Any new commit
  makes it stale.
- **Gates** are declared in `.arc/gates.toml` (committed). `arc verify`
  runs a gate and records command, exact revision, result, duration, and
  hostname — local evidence with provenance, the local analogue of
  required CI checks.
- **`arc integrate`** performs the merge only when, atomically checked:
  the head equals the approved patchset head, no blocking finding is
  open, every required gate is green at that exact head, and no hold is
  active. It merges the approved SHA (not the branch name) with
  `--no-ff`, then verifies the merge commit's parents. Refusals carry
  typed exit codes.

## Quick tour

```sh
arc begin radio-refill-fix --title "Keep radio refill from restarting playback"
# → branch arc/radio-refill-fix + worktree ~/.worktrees/<repo>-radio-refill-fix

cd ~/.worktrees/<repo>-radio-refill-fix
# ... implement, commit ...

arc snapshot radio-refill-fix          # record patchset ps-01
arc verify radio-refill-fix --gate test

# reviewer (any harness, any session) — one atomic call:
arc review radio-refill-fix --verdict changes-requested --findings-json - <<'EOF'
[{"blocking": true, "severity": "major", "summary": "stale batch can commit",
  "anchor": {"path": "src/ops.py", "line_start": 214}}]
EOF

# ... fix, then:
arc resolve radio-refill-fix f01ABC... --status resolved --commit HEAD
arc snapshot radio-refill-fix          # ps-02; old verdict is now stale
arc review radio-refill-fix --verdict approved

arc check radio-refill-fix             # exit 0 = ready
arc integrate radio-refill-fix --cleanup
```

`arc status <change>` prints the versioned `arc-status/1` JSON report —
the contract orchestrating agents program against. `arc show <change>`
renders the change as Markdown.

## Export / import

Move one change's complete ledger as a deterministic `arc-bundle/1`
JSON file:

```sh
arc export radio-refill-fix --output change.json
arc import change.json --dry-run
arc import change.json
```

Use `-` instead of a path for stdout or stdin. Re-exporting unchanged
events is byte-identical, and importing the same bundle again skips
identical events. A conflicting event makes the whole import write
nothing and exit 1. Missing Git commits are warnings rather than data
loss: available patchset heads are restored under
`refs/arc/keep/<change>/<patchset>`, while unavailable objects are
reported for separate transfer.

## Exit codes (`check`, and `integrate` refusals)

| Code | Meaning |
| ---- | ------- |
| 0 | ready / success |
| 1 | usage or internal error |
| 2 | open blocking findings |
| 3 | no valid approval for the current head (stale or missing) |
| 4 | hold active |
| 5 | required gates not green at head |
| 6 | closed change or malformed state |

## Gate declaration

```toml
# .arc/gates.toml
[gates.build]
command = "cargo build"

[gates.test]
command = "cargo test"
profiles = ["local", "forge", "release"]   # omit = required for every profile
```

## Identity

Every event records an actor, and optionally a harness and native
session ID: `--actor/--harness/--session` or `ARC_ACTOR`, `ARC_HARNESS`,
`ARC_SESSION`. Actor defaults to `git config user.name`.

## Configuration

arc treats `~/.local/ai/` as the AI data home (relocate it with
`AI_HOME`) and reads `~/.local/ai/arc/config.toml`:

```toml
worktrees_dir = "~/.worktrees"   # where change worktrees are created
data_root = "~/.local/ai/arc-data"  # optional: ledgers at <data_root>/<repo-path-slug>/
```

Environment variables override the file: `ARC_WORKTREES_DIR`,
`ARC_DATA_ROOT`, and `ARC_DATA_DIR` (an exact ledger directory for
exactly one repository — highest precedence). `data_root` keys each
repository by its slugged main path (the /thread convention:
`/home/x/code/y` → `-home-x-code-y`), so one root safely serves many
repositories — useful for sandboxing: point the paths somewhere
isolated and arc never writes outside them (worktrees, ledger) beyond
ordinary Git operations in the repository itself. `arc config` prints
the resolved paths as JSON.

Change derivation: `begin` targets the branch checked out in the
**primary worktree** (the main checkout — normally master/main), not
whatever branch the invoking worktree happens to be on. Deriving from an
open change's branch (stacking) requires an explicit `--target`.

## Storage and data-safety guarantees

- Ledger location: `<git-common-dir>/arc/` by default (shared by all
  worktrees); see Configuration for relocation.
- The ledger is append-only: arc never deletes or rewrites event files.
- Every reviewed head is pinned by its own `refs/arc/keep/<change>/<patchset>`
  ref, so Git GC cannot collect it — including earlier patchsets after a
  branch rewind. Pins are released only for heads proven reachable from
  the integration commit; abandoned or externally rewritten work stays
  pinned (release by hand with `git update-ref -d`).
- arc never passes `--force` to git: worktree removal refuses when
  dirty/untracked content is present, branch deletion refuses unmerged
  branches, and a failed merge is aborted back to the pre-checked clean
  state.
- A detected integration race (target moved) is reported, never
  "repaired" by rewriting refs.
- Git object IDs are stored as variable-length strings (SHA-256 safe).
- Anchors record path, side, blob OID, and line range; blob identity is
  what survives when line numbers drift.
- `arc` never rewrites source branches and never merges on its own —
  `integrate` is always an explicit invocation.
- Gates execute repo-committed commands (`.arc/gates.toml`): the trust
  level is the same as running `make` in that repository.

## Non-goals

Not a forge or forge clone, no hosted-PR parity claim, no daemon or web
UI, and no automatic multi-machine synchronization. Export/import moves
the ledger as one file; Git objects still travel separately. A forge-PR
projection is planned, while shared Git-ref sync remains deferred until
a real concurrent multi-machine need exists.

## Roadmap

See PLAN-02 in the arc-discussion thread archive. Shipped: local core +
policy engine (M1–M2), `/arc` skill wiring (M3), and deterministic
export/import bundles (M4). Next: forge projection — Forgejo/Codeberg
first, then GitHub (M5), and possibly absorbing the mechanical parts of
the /thread archive conventions as `arc thread` subcommands.

## License

[Unlicense](UNLICENSE) — public domain.
