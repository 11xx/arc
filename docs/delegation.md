# Delegation: roles, runs, and rounds

## Execution roles

Delegated sessions can bind an execution boundary with
`ARC_ROLE=implementer|reviewer|lead` or the equivalent global `--role` flag.
Implementers may not run `review`, `audit`, `debt`, `resolve`, `hold`,
`release-hold`, `close`, or `integrate`; reviewers may not run `debt`,
`close`, or `integrate`; leads retain full access. Declaring debt is a
lead decision because an open declaration can supply an absent verdict or let
a policy-rejected self-approval gate. Role refusals happen before the command
takes a lock or writes an event.
An unset or empty role retains full access, exactly like `lead`.

Dependencies are live ledger relationships: a blocker is satisfied when it
closes as integrated, or when a superseding successor eventually closes as
integrated (including through a transitive supersession chain). Abandoned
prerequisites and superseded chains that cannot resolve to an integrated change
are reported as `wedged`; clear or retarget them with `arc metadata`.
Missing or still-open changes remain blocked; `arc check` and `arc integrate`
enforce this boundary.
`arc blocker-status` exposes the versioned `arc-blocker-status/1` dependency
payload. `arc is-blocked` has its own polling contract. `arc is-blocked` exits 0 when
the change is ready, 1 when it is blocked, and 2 when the lookup or ledger read
failed. Automation stops on the error rather than keeping to wait. Add or remove dependencies and tags append-only
after creation:

```sh
arc metadata radio-refill-fix --blocked-by radio-storage --tag '#radio'
arc metadata radio-refill-fix --remove-blocked-by radio-storage --remove-tag '#radio'
arc metadata radio-refill-fix --priority 20
```

Read the current derived metadata without appending an event. The default is a
concise text projection; `--json` emits the versioned `arc-metadata/1` shape:

```sh
arc metadata radio-refill-fix
arc metadata radio-refill-fix --json
```

Fleet executors can atomically select and claim work instead of racing a
separate query and claim:

```sh
arc take --tag '#radio' --ttl 2h
```

Inspect the whole tagged program without collapsing it into queue buckets:

```sh
arc chain '#radio'
arc chain '#radio' --json
arc chain '#radio' --review
```

`arc chain` includes open and closed members exactly once in dependency order,
reports their current brief plan bindings and the referenced plan history, and
names the same next ready change that `arc take --tag` would select. The JSON
form uses the `arc-chain/4` schema. The view is entirely derived and does not
infer aggregate completion, pauses, unopened slices, duplicate slices, or a
progress percentage.

With `--review`, each member also gets a compact lifetime summary with its
final-patchset verdict count alongside it. The JSON view includes the recorded
subject and `at_final` and `lifetime` windows, each containing distinct verdict
identities, verdict and finding counts, and ad hoc verification count.
`brief_author` names who wrote the brief this patchset was built from —
not the newest brief, and read through `--on-behalf-of` the same way a
verdict's identity is — and
`reviewed_only_by_brief_author` says whether every lifetime verdict came
from that identity (null when there is no brief, or no verdict to
attribute). `brief_author` is omitted when there is no brief, while
`reviewed_only_by_brief_author` is emitted as null, because there the
absence of an answer is itself the answer. Neither claims a review was
independent: in an orchestrated
chain the patchset subject is the executor and the verdict comes from the
lead, so identities always differ, and arc cannot know that a reviewer
directed the work. It reports who wrote what, and stops there.

`arc take` considers open, unheld changes whose blockers are integrated and
whose claims are absent, expired, or stale. It selects higher priorities first,
then the oldest change. `arc take` exits 2 when no change is ready. Repeated
`--tag` filters are conjunctive. `--json` returns the selected change's full status.
Selection and claim publication share the repository graph lock, so concurrent
`take` calls serialize and cannot receive the same change.

Sequential bundle import can expose a dependency cycle assembled across
stores. Arc reports each member as blocked but does not mutate imported
history automatically. Break the cycle explicitly by removing one edge with
`arc metadata <change> --remove-blocked-by <blocker>`; exact blocker IDs also
work when the blocker's own bundle is absent.

State-derived writes use persistent OS-backed advisory locks with bounded
acquisition. Dependency metadata takes a repository graph lock before its
per-change lock; integration takes a target-branch lock before its per-change
lock. This prevents concurrent cycles and target-worktree races without
waiting forever on re-entry. State-append locks wait briefly; the target lock
waits integration-scale because its holder is legitimately running a merge.
Verification runs its external gate before taking the short state-append lock.

Query and batch views avoid ad-hoc JSON filtering:

```sh
arc query --status open --target master --tag '#radio'
arc list --format wide
arc show --tag '#radio' --json
arc check --tag '#radio'
```

A cross-harness executor prompt describes the executor's own environment, so
it names the harness it is addressed to. A Codex executor works locally and
must never be told to invoke `codex exec` on itself.

### Delegated rounds

arc records the actor, harness, and model it is given, and holds no routing
opinion. Who to delegate to is the caller's policy.

`arc run` records delegated runs. A dispatch names exactly one subject —
`--change <id>`, `--fork <slug>`, or `--range <base>..<head>` — and naming
none or two is refused. A ledger change is one shape a delegation takes, not
the only one: the loop of brief, review, and targeted change request runs just
as often on a fork, which is outside the lifecycle by design, or on a bare
commit range in a repository whose ledger holds nothing yet.

```sh
arc run dispatch --route 'codex:gpt-5.6-luna#max' --worktree ../wt --fork spool-spike
arc run end <dispatch-event> --outcome completed --reviewed-head "$(git rev-parse HEAD)" \
  --raised-json raised.json --deferred-json deferred.json --collects def-01j0z
arc run list
```

A round is the ordinal of a dispatch within its subject, derived rather than
recorded. `run list` groups by subject, numbers the rounds, and shows each
round's reviewed head, raised count, and deferrals still open.

The ending is where a bounded round says what it left. `--raised-json` and
`--deferred-json` take a path or `-`, holding a JSON array of objects with a
`summary` and an optional `severity`; a deferral additionally requires a `why`
and may name its own `id`, and arc mints `def-<ulid>` when it does not. The
reason is required because a deferral without one cannot be told from a
finding that was missed. A deferral stays open until a later round on the same
subject collects it by ID with `--collects`, which is refused for an ID that
is not open on that subject. Open deferrals are surfaced by `arc inbox` and
`arc catchup`, since a deferral absent from the answer to "what is waiting" is
a deferral nobody will honor.

