# Configuration, storage, and the sandbox

## Configuration

arc treats `~/.local/ai/` as the AI data home (relocate it with
`AI_HOME`) and reads `~/.local/ai/arc/config.toml`:

```toml
worktrees_dir = "~/.worktrees"   # where change worktrees are created
data_root = "~/.local/ai/arc-data"  # optional: ledgers at <data_root>/<repo-path-slug>/

[provenance]
git_identity = "per-actor"         # default; use "shared" for one Git identity
```

Environment variables override the file: `ARC_WORKTREES_DIR`,
`ARC_DATA_ROOT`, and `ARC_DATA_DIR` (an exact ledger directory for
exactly one repository — highest precedence). `data_root` keys each
repository by its slugged main path (the project-journal convention:
`/home/x/code/y` → `-home-x-code-y`), so one root safely serves many
repositories. `arc config` prints the resolved paths as JSON, and
`arc doctor` opens with them.

### Sandbox

`ARC_SANDBOX=<prefix>`, or `--sandbox <prefix>` on any command, stands in for
the home directory wherever arc derives a default path, so one value moves
every root arc writes: journal, registry, configuration, worktrees, temp, and
the ledger wherever a `data_root` places it. A configured `~/…` path expands
against the same prefix. A variable naming one exact directory — `AI_HOME`,
`ARC_WORKTREES_DIR`, `ARC_DATA_ROOT`, `ARC_DATA_DIR`, `ARC_JOURNAL_DIR` — still
means the directory it names: the prefix replaces defaults, not statements.
The prefix must be absolute, and `--sandbox` exports the variable, so a gate,
a hook, or a nested `arc` inherits the sandbox.

The repository arc was pointed at is still read and written as a Git
repository. So rehearsing something destructive — a history rewrite, a bulk
debt discharge, a schema migration — runs in a copy:

```
arc sandbox clone ~/sandboxes/rewrite    # repository, ledger, journal, config
ARC_SANDBOX=~/sandboxes/rewrite arc catchup   # from the copy's checkout
arc sandbox diff ~/sandboxes/rewrite     # what its ledger, journal, refs gained
arc sandbox discard ~/sandboxes/rewrite  # only where arc recorded a sandbox
```

The copy's refs are the source's under their own names, it has no remote to
push back through, its objects are copied rather than hardlinked, and it
inherits no path from the source's configuration — the prefix supplies every
root instead. It carries committed state: uncommitted work is not copied, and
an open change's recorded checkout is the source's until one is made in the
sandbox.

The committed `.arc/policy.toml` may set the same `[provenance]` table for a
repository. `per-actor` compares the claim actor with the snapshot author and
committer; `shared` omits `provenance_mismatch` because that comparison does
not apply. A delegated snapshot can instead declare its subject with
`--on-behalf-of`.

Before starting an executor in a restricted environment, run
`arc config --check-writable`.
It probes the ledger root, lock, event-path, and Git-ref writes without adding
an event; `--json` emits `arc-writability/1` for automation and stops at the
first blocked path. It also probes committing, in a throwaway repository so the
target gains no commit, because an environment that cannot commit otherwise
discovers it only once a slice is ready to land.

Committing and signing are reported apart, and only the writability checks
decide the exit code. The `commit` check makes an unsigned commit and answers
writability alone. The `signing` check is advisory: it says `not required`
where the repository's resolved `commit.gpgsign` is off, and otherwise carries
the repository's signing key so it exercises the credential the real commit
will use, ignoring a global signing policy the repository overrides. An
unreachable credential prints as `warn: signing: ...` with gpg's own reason and
leaves the exit code alone, because the process that makes the work need not be
the one that signs it — but a project whose `commit.gpgsign` is on still needs
signing working somewhere before its commits can land. In `--json` that row
keeps `"ok": false` and carries `"advisory": true`, so a consumer can tell a
warning from a blocked path.

Change derivation: `begin` targets the branch checked out in the
**primary worktree** (the main checkout — normally master/main), not
whatever branch the invoking worktree happens to be on. Deriving from an
open change's branch (stacking) requires an explicit `--target`.

`arc query` filters by lifecycle status, target, tags, latest verdict, opening
actor, and opening harness. Every filter is an explicit flag: the acting
identity in `ARC_ACTOR` and `ARC_HARNESS` is recorded on writes and never
narrows a read, so `arc query --debt` enumerates every outstanding obligation
whatever session runs it. Narrow to one harness with `--harness <name>`.
`arc list --format compact|wide|json` supplies
pipe-friendly IDs, a scannable orchestration table, or structured rows.

## Storage and data-safety guarantees

- Ledger location: `<git-common-dir>/arc/` by default (shared by all
  worktrees), relocatable with the configuration keys above.
- The ledger is append-only. arc never deletes or rewrites an event file.
- Every reviewed head is pinned by its own `refs/arc/keep/<change>/<patchset>`
  ref, so Git GC cannot collect it — including earlier patchsets after a
  branch rewind. Pins are released only for heads proven reachable from
  the integration commit; abandoned or externally rewritten work stays
  pinned (release by hand with `git update-ref -d`).
- arc never force-removes a checkout it did not create. Worktree removal
  refuses while dirty or untracked content is present, `fork retire --force`
  is the operator's decision to get past that, and the one forced removal arc
  performs on its own is the scratch checkout it creates to evaluate a merge.
  Branch deletion refuses unmerged branches, and a failed merge is aborted
  back to the pre-checked clean state.
- A detected integration race (target moved) is reported, never
  "repaired" by rewriting refs.
- Git object IDs are stored as variable-length strings (SHA-256 safe).
- Anchors record path, side, blob OID, and line range; blob identity is
  what survives when line numbers drift.
- arc rewrites a branch only through `arc rewrite`, which records the map it
  produced, and merges only through `arc integrate` when every required gate
  is green. Both are explicit invocations: nothing arc does on the way to
  either moves a branch by itself.
- Gates execute repo-committed commands (`.arc/gates.toml`): the trust
  level is the same as running `make` in that repository.
- Derived views (`list`, `inbox`, `status`, `query`) replay the ledger on
  every invocation and hold no persistent cache. This stays fast in practice
  (measured ~8 ms for `list` and `inbox` over 300 changes). Any future
  acceleration must live under a `derived/` cache that is deletable,
  rebuildable, and never authoritative — the ledger remains the only source
  of truth.
- **Unix-only, deliberately.** arc relies on POSIX semantics for its safety
  guarantees: `0700` private directories, atomic hard-link event publication,
  and process-group kill for gate timeouts. There is no Windows port and one
  is out of scope; run it under WSL there.

## Git integration

Git hooks are strictly opt-in: arc never installs them silently. `arc hooks
install [--force]` writes two scripts into the resolved hooks directory (it
honors `core.hooksPath`), each a two-liner delegating to `arc hook-run`. It
refuses to overwrite a foreign hook unless `--force` is given, which first
saves the original as `<hook>.pre-arc`. `arc hooks uninstall` removes only
arc-authored scripts (a marker comment identifies them), and `arc hooks
status` reports whether each hook is absent, arc-managed, or foreign.

Both hooks are advisory. An arc-managed Git hook always exits 0, so it can
never block a commit.
The `post-commit` hook, on a change branch, prints a notice when the new
commit has staled an approval bound to the snapshot it was recorded on, or
warns when the branch's change is already closed. The `prepare-commit-msg`
hook appends an `Arc-Change: <change-id>` trailer on a change branch when one
is not already present, giving commits durable linkage back to their change.

Independent of hooks, `arc query --commit <revision>` reports the changes
whose patchset heads or integration/closure commit match a revision (a unique
prefix is accepted). It searches the ledger only; it does not scan commit
trailers.

