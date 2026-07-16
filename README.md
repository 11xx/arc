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

## Storage and safety notes

- Ledger location: `<git-common-dir>/arc/` (shared by all worktrees);
  `ARC_DATA_DIR` overrides it for bare repos or read-only common dirs.
- Reviewed heads are pinned by `refs/arc/keep/<change-id>` so Git GC
  cannot collect them after branch deletion; the ref is dropped on
  closure.
- Git object IDs are stored as variable-length strings (SHA-256 safe).
- Anchors record path, side, blob OID, and line range; blob identity is
  what survives when line numbers drift.
- `arc` never rewrites source branches and never merges on its own —
  `integrate` is always an explicit invocation.

## Non-goals

Not a forge or forge clone, no hosted-PR parity claim, no daemon or web
UI, no multi-machine sync (a deterministic export/import bundle and a
forge-PR projection are planned; shared Git-ref sync is deferred until a
real concurrent multi-machine need exists).

## Roadmap

See PLAN-02 in the arc-discussion thread archive. Shipped: local core +
policy engine (M1–M2). Next: `/arc` skill wiring (M3), export/import
(M4), forge projection — Forgejo/Codeberg first, then GitHub (M5), and
possibly absorbing the mechanical parts of the /thread archive
conventions as `arc thread` subcommands. License: to be chosen before
publishing.
