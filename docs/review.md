# Review, verdicts, debt, and audits

## Verdicts

Every `changes-requested` round classifies its root cause with one or more
`--cause` values. `brief` means the patchset faithfully exposed a missing,
false, or ambiguous premise; `executor` means it violated a correct applicable
brief; `integration-staleness` means later target work invalidated a brief and
implementation that were correct at their base. Other verdicts do not accept
causes. `arc review --json` and `arc stats --json` expose these classifications
without inferring them from verdict prose.

Each verdict says what it does to the verdicts already standing on the change.
`--relation supersedes`, the default, replaces the tips it observed;
`--relation corroborates` supports one without becoming a second authority,
which is what discharging a provisional approval is — a bare supersession
could not express the difference, and event order was deciding it. Two
verdicts replacing the same earlier verdict fork the chain and leave the
change contested: no verdict is authoritative until one supersedes them all,
`arc check` blocks and names that rather than reporting nobody reviewed, and
`arc review` and `arc status --json` carry `verdict_contested`. This is the
same shape `DispositionRecorded.supersedes` gives a contested finding.

An approving verdict can be owed corroboration rather than absent. `arc review
--verdict approved --provisional "<why>"` records one that gates like any
other — independence and staleness are unchanged, because an unproven reviewer
is still not the author — while saying it should not be relied on yet. arc
infers this from nothing: deciding which reviewers are proven would be a
routing opinion, and arc holds none. `arc query --provisional` lists the
approvals still owed corroboration, and corroboration is a second judgment
rather than one particular command — an independent approval of the same
patchset discharges it before the merge, an audit after — from neither the
reviewer being corroborated nor the change's own author.

## Dispositions

`arc resolve` accepts `--evidence` for a free-form explanation and,
independently, `--evidence-event <ID>` for a full event ID from the same
change. The event must be a `verification-recorded` or `verification-reused`
event; event-ID prefixes are not resolved. Both options may be supplied
together.

`arc reply` addresses a finding ID or a comment/finding event ID. Finding
replies appear beside their dispositions in findings JSON and beneath the
finding in `arc show`; comment replies render beneath their parent comment.
`arc findings <change> --format sarif` exports the open set to tooling, and
`arc findings <change> --audit` reads the audit set instead.

## Policy

Repository integration policy is declared in `.arc/policy.toml`. Policies are
disabled when the file or setting is absent. Set
`[policy] forbid_self_approval = true` to reject an approval when its effective
author matches the one recorded by `arc snapshot` for that patchset, or when
arc assumed either identity rather than someone declaring it — two invented
names that happen to differ do not show that two people acted. Actor identity
remains advisory; this comparison does not redesign or verify identity.
A rejected self-approval follows the no-valid-approval path and exits 3.

Where the policy is off the approval is recorded, and `arc review` and
`arc audit` name what the record does not otherwise show: the identity the
verdict was recorded as, the patchset that identity wrote, and whether arc
assumed the identity from git config. Such a verdict is a review that happened
rather than an independent one, and it leaves an independent-review debt owed.

Every event records `actor_source`: `flag`, `env`, or `git-fallback`. The last
means nobody declared an identity and arc took `git config user.name`, which
names whoever configured the checkout rather than whoever acted. The
substitution is announced on stderr the first time a command would record it,
because the ledger is append-only and that is the last moment to correct it.
Events written before arc recorded provenance carry no source; that is
*unknown* rather than assumed, and is compared by name as it always was.

Set `[policy] require_declared_actor = true` to refuse an event whose effective
author nobody claimed. `begin`, `verify`, and `integrate` check before they
create a branch, run a command, or merge, so a refusal never lands after the
work; every other append is guarded at the store. Reading is unaffected, as is
a bundle import, whose events are another repository's history being
transferred rather than this session's claim about who acted. A lead acting for
a declared `--on-behalf-of` subject satisfies the policy.

After integration the rule narrows: `arc audit` refuses an approving audit from
an identity arc assumed, which the auditor can fix by declaring itself, but not
one whose *authoring* identity was assumed — that is already on the ledger and
cannot be corrected, and refusing would leave the debt undischargeable rather
than making anyone independent. Such an audit warns that it shows a review
happened and not that it was independent.

### Dangerous surfaces

Which changes need an independent verdict is the project's call, declared in
the same file under `[danger]`:

```toml
[danger]
paths = ["src/store.rs", "src/commands/gatekeeping.rs", "src/**/*policy*.rs"]
acknowledged_safe = ["src/render.rs"]
source_roots = ["src/"]
```

A change touching a declared path needs a verdict from somebody other than its
author; elsewhere a self-recorded verdict satisfies the gate. Declare nothing
and the gate is uniform for every change. `*` matches within a path segment,
`**` across segments, and a trailing `/` names the whole subtree beneath a
directory.

The declaration is a judgement made once, in a reviewable commit, rather than
one made per change by the party under pressure to ship. `arc begin
--dangerous` raises a single change whatever it turns out to touch, and nothing
lowers it afterwards. Where the touched paths cannot be established the change
is treated as dangerous. `arc check` names the rule that fired: no declared
surfaces, an escalated change, a change touching a declared path, or one
touching none.

`acknowledged_safe` records a classification somebody made and a reviewer
accepted, which is different from a file nobody classified. Inside a declared
`source_roots` prefix classification is closed: every tracked file must match
`paths` or `acknowledged_safe`, and `arc doctor` fails on one that matches
neither with `danger-unclassified`. Without a declared root the list is
open-world and fails permissively — a file nobody classified matches nothing
and looks safe. `arc doctor` also reports a pattern that can never match as
`danger-path-matches-nothing` and a path declared both ways as
`danger-classification-conflict`.

Optional reviewer reminders live in the same file under `[review]`, for
example `checklist = ["exercise the failure path"]`. `arc show` renders these
advisory items only for reviewer and lead roles; they never block integration.

## Review coverage and post-integration audits

Whether somebody reviewed a change and whether somebody reviewed *what is about
to ship* are different claims, and only the second one matters at integration.
A review panel can run correctly for many rounds and still let the final round
of corrections ship unseen — an identity comparison reads that as clean.

So arc derives a **review map**: for every identity that filed a verdict or
finding, the newest patchset it saw, and whether that is the final one. `arc
status --json` carries it as `review_map`, and `arc show` renders it.

What that map implies is reported as **advisories**: `arc status --json` and
`arc check --json` (`arc-check/3`) carry `advisories`, each a stable `code`
with a `detail` line, and `arc check` prints them under `Advisories (never
blocking)`. The codes are `reviewer-behind-final-patchset`
(`Reviewer last saw ps-07; integrating ps-11`), `no-independent-reviewer`,
`reviewer-attribution-unknown`, `brief-author-only-review`, and
`debt-outstanding`. None of them changes readiness or the exit code:
plenty of changes legitimately ship with a single reviewer, and an
orchestrator's review is a valid review unless a project's policy says
otherwise. A reviewer that cannot be told apart from the patchset author
(neither side recorded `--on-behalf-of`) is reported as unknown attribution
rather than counted as independent or as self-review.

When no independent verdict is reachable, declaring **debt** records
the missing review instead of pretending it happened. It can stand in for an
absent verdict or rescue a self-approval rejected by `forbid_self_approval`:

```sh
arc integrate <change> --debt "no independent reviewer reachable"

# later, when a reviewer is available
arc inbox                                    # debt-owed bucket
arc catchup                                  # the same, with reasons
arc query --debt                             # IDs alone, for scripting
arc diff <change> --integrated               # the exact range that landed
arc audit <change> --verdict approved --body-file -
arc findings <change> --audit                # what the audit raised
```

The obligation is a ledger fact that survives closure, so the owed review is
findable instead of living in prose. `arc inbox` carries it in the one bucket
that includes integrated changes, `arc doctor` reports it as
`debt-outstanding`, and `arc chain` shows it beside reviewer coverage.

### What a debt records

A debt is a record a later reader can weigh, not a count. It names what kind of
deficit it is:

| kind | what it says |
| --- | --- |
| `nothing-read` | no verdict on any patchset of the change |
| `merge-resolution-unread` | an approved patchset, then a merge or rebase resolution nobody read |
| `repair-unread` | an approved patchset, then authored work nobody read |
| `contributor-only` | verdicts on the shipped patchset, all of them from its contributors |
| `independent-review` | a read by somebody independent, which nobody supplied |

Arc derives the kind from the ledger. `--kind <k>`, on `arc debt` and on
`arc integrate --debt`, declares one instead and wins over the derived value —
only the caller can say a merge resolution was what went unread, because the
ledger sees a resolution and a repair the same way.

The kind is the weight, carried as a label rather than a number. `arc query
--debt` orders by kind in the order above, then by age, and every summary row
splits its count by kind.

Beside the kind, a debt carries coordinates:

- **Coverage**: each verdict recorded on the shipped patchset — the reviewer,
  the model string kept whole, the effort its trailing `#suffix` names, and the
  routing version `arc review --route-version` or `arc audit --route-version`
  declared. An absent route version means the review was unrouted.
- **Production**: who recorded the brief version the shipped work answered, who
  recorded the patchset, and whether the two identities differ.

These are coordinates and nothing more. arc never scores the coordinates a
debt records, joins them against a roster, or orders two models against each
other.

Three rules keep the escape hatch from becoming a hole:

- **The waiver binds to the exact patchset head.** A debt declared for `ps-01`
  can supply its absent verdict or excuse its rejected self-approval, but stops
  applying the moment `ps-02` is snapshotted, exactly as an approval goes
  stale. Re-declaring is deliberate. A debt declared after integration carries
  no patchset and waives nothing.
- **An approving audit must come from another identity.** Otherwise the change
  ships on a self-approval and then clears its own record. Auditing into
  `changes-requested` is open to anyone — raising problems needs no
  independence.
- **An audit is a distinct event**, anchored to the integrated revision and
  refused while the change is still open. Attaching one can never rewrite the
  answer to "what shipped with what review", and audit findings stay out of the
  shipped set — `arc findings --audit` reads them. `arc reply` addresses either
  finding kind, while `arc resolve` records an integrated-only audit
  disposition for an audit finding. The ordinary disposition event remains
  open-only, so later audit work cannot rewrite shipped finding state.

