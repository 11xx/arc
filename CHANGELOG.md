# Changelog

Notable user-facing changes, one entry per integrated arc change. The
append-only ledger is the source of truth (`arc list`, `arc show <change>`);
this file is its human-readable projection. Each change records its entry
with `arc changelog <change> --category <category> --body-file <file>`, and
the `[Unreleased]` block is generated from those entries by
`arc changelog --write`, never edited by hand.

The format loosely follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added

- `arc cost evidence --output-dir DIR` writes an export too large for one
  evidence file as a directory tapes reads with `tapes usage ... --evidence
  DIR`: `evidence-0001.jsonl`, `evidence-0002.jsonl`, ... in name order,
  each the export's header followed by at most 10,000 records, which keep
  the export's order. DIR must be absent or an empty directory and is
  checked before the ledger is read; the files are moved into place
  together, so a refused or failed export leaves DIR as it was, and
  standard output stays empty. Standard output still carries one file, and
  an export past 10,000 records is refused there naming `--output-dir`.

- `arc cost evidence` exports what `arc cost show` rests its figures on as
  a `tapes-attribution-evidence/1` file, so tapes can give arc's figure for
  the same sessions. It takes `cost show`'s selection (export `--all` for
  exact figures) and writes, for every session with a figure, a binding for
  each run of an executor, an event for each event of a participant divided
  by its events, or a whole-session binding, with the change's commits and
  the repository's origin URL, plus one lineage record for each dispatching
  and executor session pair. A summary on standard error counts what was
  written and what was not.

- `arc cost show --allocate following` gives what remains unattributed
  between two items to the next item, labelled `allocated` beside the
  conserving figures.

- A native subagent can be bound to the work it served. `arc run bind
  --parent-session <session>` names a Claude Agent-tool subagent by the
  agent id its launch reports, or a Codex `thread_spawn` child by its own
  thread id, and refuses unless exactly one recording names both the child
  and that parent. Binding a delegated-agents mirror row refuses and names
  the agent-delegate run it mirrors. `--executor-role reviewer` records a
  bound session as a reviewer, which the commit cost view's `Reviewed-by`
  then answers.

  `arc cost show` measures a bound child as its own executor row. A Claude
  lead with native children counts only its own requests, because its
  recorded total also covers its children. Every parent lists its children
  under `family`: the bound rows, those bound to work the view does not
  select, and the consumption no binding assigns, which stays outside every
  total. For a Claude lead, `family` also shows whether its requests and
  its children's add up to the recorded family union, and keeps the
  residual as the parent's.

- `arc cost show` divides a session that served several work items by its
  spans: each executor run from its dispatch to its recorded end, and each
  participant's events on a work item from the first to the last. A
  change's total now includes its share of every lead, reviewer and reused
  executor session that worked on it, and each run of a multi-run executor
  has its own figure. Requests in overlapping spans, outside every span, or
  with no recorded time are listed under `unattributed shared` with a fixed
  reason, and the shares plus that part always add up to the session's
  whole figure within one accounting basis. `--allocate even` divides
  overlapping requests evenly among the overlapping items, labelled
  `allocated` beside the conserving figure. `cost retain` keeps the span
  figures, so a split survives source rotation.

  Every figure also carries a list-price estimate from a
  `tapes-price-table/1` table named by `--prices`, `TAPES_PRICES` or
  `~/.config/tapes/prices.toml`; the table's money columns lead with it
  beside the harness-written estimate, and without a table the footer says
  how to install one.

- `arc cost show` prints a compact table by default: one row per session
  with its agent (`harness:model#effort`), roles, a short session id, runs
  and spans, the token total, the harness-written dollar estimate and a
  status (`complete`, `subtotal`, `shared ×N`, `live`, `unmeasured` or
  `withheld`), followed by the total, the sessions shared outside it, and a
  count of qualifications. `--detail` prints the full per-category report,
  and `--by agent|role|model|harness|change|session` adds a subtotal per
  group without adding incompatible accounting domains.

  It also reports over a range of work: several change ids and journal
  artifacts at once, `--tag`, `--footprint <harness:id>` for every change
  and journal item one session worked on, `--since`/`--until`, and `--all`,
  counting each session once across the selection. Sessions are read in
  parallel.

  A bound executor whose dispatch has no recorded `arc run end`, or that
  `agent-status` still lists, is now shown as live and its figure stays
  incomplete; one whose runs all ended keeps its figure.

- `arc journal relocate <file>... --to DIR --reason` moves artifacts out of
  a live journal into a directory outside arc, removing the body from hot
  storage, the cold archive, and any prepared transition copy, and leaves a
  `relocated` tombstone naming the destination, reason, and digest that
  `show`, `filed`, doctor, and supersedes links report. Open claims,
  unfinished transitions, and unanswered questions refuse until
  acknowledged, and exchange neither carries nor restores a relocated
  artifact. `arc journal redact <file> (--replace OLD --with NEW)... |
  --body-file F --reason` rewrites passages of a hot or archived body in
  place and records digests, never the removed text; journal export and
  import keep the redacted revision instead of refusing it as an edit of
  the base.

- `arc workspace retire` takes a registration whose project is gone out of
  the workspace views, with a recorded reason, after showing what it still
  holds; it refuses while unresolved artifacts, live claims, or a ledger's
  open changes and debts remain, unless `--archive-unresolved` shelves the
  artifacts in the cold archive. `--dry-run` writes nothing, and `arc
  workspace restore` reverses it. Backlog, report, and inventory count
  retired registrations as `collection.retired` instead of failing on their
  missing anchors, and list them under `--retired`.

- Verification and candidate gates retain private combined diagnostics with
  size limits, full-stream and retained-byte digests, truncation metadata,
  and bounded local retention. Status exposes capture metadata alongside
  the short output tail; exports carry metadata without diagnostic bytes.

- Export and import offer JSON operation summaries with structured custody
  counts. Acceptance JSON retains the latest custody transfer summary,
  distinguishing planned installation, completed installation and a sidecar
  that was not supplied. Export summary JSON requires a file destination
  for the bundle.

- Read stateless workflow topics through arc guide, with an indexed
  command-help map, bounded topic pages, complete explicit --all output and
  literal case-insensitive search across guide text and command help.
  Instruction reads work outside a repository and with malformed
  configuration under every execution role.

- `arc query --assurance` lists, for every integrated change, the reads it
  received (model, effort, verdict or audit, time, route version) and where
  its debt stands, as `arc-assurance/1` with `--json`. The record is
  derived from the verdict and audit events the ledger already holds, so
  past integrations have one and nothing new is stored. With `--stale`, arc
  asks `agent-routing frontier` at read time whether a stronger eligible
  reviewer than every read a change holds can be seated, and lists each
  change that is not known fresh, stale work first, with its frontier gap,
  the share of its lines still live at HEAD (`git blame`, cached once per
  change), its exposure to the policy's dangerous paths and the age of its
  newest read. A change with no eligible read is owed, one routing cannot
  place is unknown with a reason and never fresh, and staleness gates
  nothing.

- arc cost show --commit <rev> resolves a commit to its change and shows
  the change figures, shared by its commits, with each role trailer mapped
  to the sessions recorded for it (arc-cost-commit/1). arc cost note writes
  that view as a refreshable local git note under refs/notes/arc-cost,
  never pushed.

- `arc cost show` measures participants: sessions no dispatch binds whose
  ledger events act on the selected work, typically the lead that reviewed,
  integrated or dispatched it. A participant is measured whole through the
  same reader as an executor, or from retained facts once its source is
  gone, and `arc cost retain` keeps its facts on the same terms. Its roles
  are the event kinds it wrote: snapshot and verdict are a reviewer's,
  integration, debt and closure an integrator's, dispatch an
  orchestrator's. It is never split: a participant counts as direct only
  for an item that holds every change its events name, and is otherwise
  listed under `shared` and in `participants` with its whole figure, roles
  and span count. A union counts it once when the selection covers its
  whole membership. Its figure is as of its read frontier and reports
  incomplete measurement, since a recording cannot show whether the session
  is still running. A session the store cannot resolve stays outside
  assignment coverage with its event count, and acting events with no
  session identity are counted as `unattributed_events`. `arc-cost/1` gains
  these fields.

- `arc integrate --dry-run --debt` now plans the integration as it reads
  once the debt is declared. The plan, in text and in
  `arc-integration-plan/1` JSON (`debt`), names the kind, reason, bound
  patchset and, for `ineligible-review`, the named read, qualifiers and
  assessment basis, and writes no event. A declaration the real run would
  refuse (unknown qualifier, no read to name, an eligible assessment) is
  refused with the same message. A provisional execution in text mode that
  refuses after declaring a debt now ends with the debt event id and its
  publication state, as its JSON receipt already did.

- `ineligible-review` is a debt kind for a read that happened, by a
  reviewer routing does not count as independent of the author. `arc
  integrate --debt <reason> --kind ineligible-review` (and `arc debt`)
  records the read, the verdict event named by `--read` or the latest on
  the covered patchset, and why it does not count: qualifiers from the
  closed set `same-model`, `same-family`, `below-floor`, `floor-unknown`
  and `reviewer-unseated`, taken from an `agent-routing assess --json` file
  (`--assessment`, recorded as assessed) or named by `--qualifier`
  (recorded as declared), with an optional route version. arc validates the
  shape and stores no tier, floor or family. Only an audit carrying an
  eligible assessment (`arc audit --assessment`) clears the debt; any other
  audit is listed beside the open obligation. Debt reads show the
  qualifiers, the reviewer's model, the basis and the route version, and
  order a `below-floor` read first within the kind.

- `arc cost` reads tapes' integrated accounting. A rewound Pi recording
  counts each consumption event once across every retained branch, with the
  events, their per-counter coverage and their identity reported and
  retained. Token counts also total across harnesses in tapes' disjoint
  categories, each with its harness's convention, partitioned by accounting
  basis; native counters still add only within one harness and a raw input
  sum stays refused. A Claude session's recorded total is reconciled per
  counter with the requests its recording family identifies, reporting the
  signed residual with tapes' statuses and reasons; a conflict qualifies
  the counter it understates and a withheld counter has no residual. A
  harness-written zero price beside usage stays unpriced, with pricing
  coverage reported. Retained facts carry only fixed categories, so older
  or tampered facts degrade field by field.

- `arc cost show <change|journal-file> [--json]` reports what the executor
  sessions bound to a change's or a journal artifact's runs consumed, read
  through the pinned tapes reader, and records nothing. A journal artifact
  selects its `supersedes` lineage and the changes begun from it. The
  report gives direct and inclusive totals per work item and the union
  across the selection, counting each session once, with sessions shared
  between items listed beside them. Every total names its quantity kind: a
  harness-written dollar figure is a harness-calculated estimate, and a raw
  input token sum across harnesses is refused. Measurement, assignment, and
  identity coverage are reported apart; an unbound dispatch withholds its
  work totals, a missing amount stays absent rather than zero, and an
  unbound session such as a lead is named outside assignment coverage. `arc
  cost retain` keeps each bound session's normalized facts with the reader
  version and the source frontier and digest, so the report reproduces
  after the source rotates. `--json` emits `arc-cost/1`.

  Recorded parent, child, and fork lineage determines identity coverage per
  direct, inclusive, and union figure. Potentially overlapping sessions
  withhold exact unions and expose bounds from the largest member to their
  sum, while retaining individual session figures. Failed or bounded
  lineage reads remain unknown. Retained lineage reproduces coverage after
  source rotation; partial measurements qualify bounds as known recorded
  amounts.

  `arc run bind --forked-from <session>` records a fork origin in the
  executor harness, with the same validation and immutable retry rules as
  the executor session. `arc run list` shows the origin. Cost identity
  groups include declared fork origins, including Claude forks whose
  recordings carry no native fork relation. An origin outside the figure
  adds no consumption; binding relations persist through cost retention and
  source rotation.

  Conflicting bindings, pre-dispatch consumption and unknown source
  openings make assignment incomplete and withhold work totals, while
  preserving session measurements. Retained facts carry source-start
  evidence, plain session relatives and fixed reason categories. Older
  facts are sanitized for display; unsafe retained lineage or unclassified
  reasons refuse bundle transport without rewriting events. Current
  unreadable sources stay visible when retained observations supply the
  measurement.

  Journal membership resolves canonical journal and artifact identities
  across reference aliases and transitions. Bare, qualified and equivalent
  directory spellings select the same runs and changes. Unrelated names
  outside a readable lineage need no resolution. Unavailable membership
  qualifies selection and withholds dependent work totals while readable
  session figures remain available; original references remain provenance.

  Every quantity carries its accounting basis. Incompatible harness or
  basis domains withhold combined quantities and expose partitions.
  Completeness is per category; request and per-model sums without category
  coverage remain qualified known subtotals. Zero estimates beside nonzero
  or unknown usage remain unmeasured. Token arithmetic preserves integers
  and refuses overflow; known overlap bounds retain the same basis and
  category qualifications.

  Imported binding locators refuse transport. Held nonportable bindings
  refuse run listings and rebinding with fixed diagnostics that preserve
  the ledger facts without echoing locators. Cost reads qualify affected
  assignments and keep readable session identities. Conflicting tuple
  listings show every fork origin.

  Cost queries preserve readable records beside unavailable journal
  references, corrupt ledger records and unreadable published imports.
  Selection and ledger coverage name fixed reason categories. Ambiguous
  membership withholds only dependent work totals; independent change costs
  and individual session figures remain available. Retention skips
  unavailable sources individually and keeps healthy sessions. A query
  refuses only when no target metadata or relevant dispatch or session can
  be read.

- A delegated run records which backend session executed it. `arc run
  bind <dispatch> --executor-harness <h> --executor-session <id>
  [--handle <handle>]` writes the binding at any time after the dispatch,
  its ending included, and prints the event id. The same values again
  record nothing and print the binding already held; different values,
  placeholders, and malformed values refuse. A follow-up or resume is its
  own dispatch with its own binding. `arc run dispatch --journal <file>`
  names a journal artifact as the run's subject, resolved as `begin
  --from-journal` resolves it, and `arc run list` shows each run's journal
  subject, its names across kind transitions, and its binding or `unbound`.

  Imported equal bindings are duplicate observations. Distinct executor
  tuples remain visible as binding conflicts in run listings and the
  binding read model; a conflicted dispatch cannot establish exclusive cost
  assignment.

  Binding values refuse recording locators without echoing the supplied
  value.

- Record forge delivery, disclosure, compact comment provenance and local
  acceptance relationships. `message --delivery-file` accepts
  `arc-forge-delivery/1`, keeping delivery, acknowledgement and acted-on
  evidence separate, with idempotent attempts and explicit uncertainty.
  `context declare --disclosure-file` accepts `arc-disclosure/1`, binding
  selected Git, forge and private-ledger support, omissions, attribution,
  outgoing bytes and exact retained readback. Material authors and
  correction editors stay distinct; optional scoped as-of cost references
  retain their basis and coverage without inventing monetary amounts.

  Offline forge and ordinary workflow views expose typed records, exact
  source aliases, current receipt availability and connected
  candidate/manual-origin acceptance paths. Local integration and provider
  closed-unmerged or merged lifecycle remain independent. New bindings
  reject invalid or conflicting observations; later conflicts remain
  visible beside retained declarations and judgments. Private exports carry
  related source/predecessor histories, custody and held original Git
  support, while imports validate introduced facts before publication and
  report any partial publication effects. These evidence records grant no
  local gate, verdict, selection or integration authority.

- Retain exact forge observation/generation sources on local findings,
  reviews and brief causes. Reconstruct recorded pull requests offline with
  historical freshness, per-source custody availability, review drift and
  exact body bytes. Missing and corrupt bytes remain distinct; verdicts
  without a recorded forge frontier retain unknown drift. Forge content
  edits preserve local judgment and integration authority.

- Record evidence received from an external run on a candidate. `candidate
  register --evidence-file FILE` takes an `arc-received-evidence/1`
  document and records it as `candidate-evidence-received` beside the
  registration, written once: the private run handle, the packet or brief
  reference, the source, tested and report heads (each a commit and tree),
  provenance by agent role, receipt references into custody, commands with
  reported results, the environment, the head the results apply to, and
  explicit omissions. `--custody-dir` copies the receipt files into custody
  after verifying each against its reference, and a mismatch records
  nothing.

  The evidence is the sender's claim and never an evaluation: it creates no
  `candidate-verified` or `verification-recorded` event, and `candidate
  select` still needs an evaluation at the shipped tree. Provenance keeps
  the agent role, the native producer and model with their source
  coordinates, the forge account, the review sender and the Git author and
  committer as separate fields, so one forge account behind several roles
  stays several entries, and a candidate's producers, contributors and
  `--on-behalf-of` keep their roles.

  `candidate show` and `list` render it as `received_evidence` with
  `origin: external`: each head as reported beside its resolution through
  recorded rewrites, whether the tested tree is the registered tree,
  receipts with their custody availability, and omissions. Export carries
  the evidence with the repository events, lists the commits and trees it
  names where the exporter holds them, and ships the receipts in the
  custody sidecar.

  Registration and import validate held received head objects as raw
  commits with the claimed trees; tags are not peeled, and absent heads
  remain external claims. Held source, tested and report originals have
  retention refs through rewrites, import and garbage collection. An
  optional original requested-evidence SHA256 is recorded atomically with
  the registration. Explicit-ID retries must match that request, the
  registration fields and declarant; a deliberate omission cannot gain
  evidence later. Older records and their evidence remain readable and
  importable without granting a registration retry. Evidence-publication
  failures name the durable registration event, including on a repeated
  partial failure.

- Hold retained forge content in ledger custody. `forge observe
  --custody-dir` copies adapter-materialized files into custody, each
  verified for length and SHA-256 before any event or file is written, and
  every custody read re-verifies. Receipts report each reference as
  available, unavailable or corrupt, and absent bytes never read as empty.

  `export --custody-dir` writes an `arc-custody-bundle/1` sidecar whose
  manifest digest the bundle binds, staged beside its directory and moved
  into place only once the bundle is written. `import --custody-dir` and
  `accept --custody-dir` validate the sidecar before any effect and install
  it once the events are published. Corrupt content, unsafe paths and
  unbound manifests refuse; unavailable content stays unavailable and the
  summary says incomplete. A delivery that binds a sidecar none was
  supplied for says custody is unavailable, names the manifest and the
  flag. Neither side initializes a replica.

  `arc doctor` advises on custody files whose bytes do not match their
  digest and on temporary files an interrupted write left behind, and
  changes nothing.

  Provider-observed commits that a repository holds are retained across
  rewrites, pinned on import and listed in the export object closure as
  `observed-commit`, so a receiver is never required to hold a commit the
  exporter never fetched. Bundles listing the earlier `cited-commit` and
  `integration-commit` roles still import.

- Admit typed forge observations with acquisition coverage,
  retained-content reference claims, idempotent read receipts and explicit
  content-generation predecessors. Bind forge link, checks and lifecycle
  projections to an observation's PR identity and raw head; legacy flags
  remain unretained assertions with unknown acquisition time. Observation
  admission grants no readiness or integration authority, and observation
  events travel through change bundles.

  Preserve explicit hosted-check bindings during imports, including an
  absent link. Infer unbound legacy checks and lifecycle only from an
  earlier link in their recording repository and an unambiguous history.
  Compare forge applicability against the patchset's approval anchor
  through content and trailer rewrites.

  Validate observations and sourced projections at append and import
  admission. Replay coalesces identical replica reads with event aliases
  and retains conflicting or invalid documents with diagnostics. Unusable
  sources and unresolved projections cannot establish forge readiness;
  their stored facts remain readable.

- Repositories can declare `none = "<reason>"` in `.arc/changelog.toml` to
  suppress `no-changelog-entry` advice, including integration advice, for
  every change. `arc check` displays the declaration and its reason, with
  an optional `changelog_none` reason in `arc-check/4` JSON. The
  declaration is read from the invoking checkout, like debt advisory
  thresholds. It requires a non-empty, non-placeholder reason and conflicts
  with explicit target or renderer keys. Changelog entry recording and file
  writes refuse under the declaration; per-change `--none --reason` remains
  valid and reads remain available. Without the file, the target remains
  `CHANGELOG.md`. Older arc builds reject a configuration carrying `none`
  as an unknown field.

- `arc journal evidence <file> --body-file <f|->` appends an attributed,
  timestamped `### Evidence` block to any live artifact, so a todo, feature
  request, or plan gains a finding, an option, or a revised section without
  a successor and a superseding consume. `arc journal show` prints the
  block, and `arc journal filed` counts the blocks per item (`evidence` in
  `arc-journal-filed/1`) and treats them as activity, so an item that
  gathers evidence does not go stale. Evidence states no stance and never
  enters a discussion's tally; `journal position` still argues a discussion
  and `journal correct` still replaces a field. A consumed artifact refuses
  and names the successor to file. The block is recorded as an `evidence`
  event, the first `journal-events/2` line: a reader of `journal-events/1`
  skips it and `journal doctor` reports it as `unknown-jsonl-event`, and a
  journal import refuses a bundle carrying one, while every other event
  stays `journal-events/1`.

- `arc rewrite trailers --edits <file>` replaces or drops one exact trailer
  line of one named commit, so a placeholder such as `Planned-by:
  codex:unknown` can be corrected without touching the valid lines under
  the same key or any other commit. The file is `arc-trailer-edits/1`:
  tab-separated `<full sha> replace|drop <exact old line> <new line>`, with
  blank lines and `#` comments skipped. An edit applies only to its commit,
  which has to be in the `--from` range, and only to a trailer line whose
  trimmed text equals the old line and occurs once there; a new line on a
  role key, or on `Assisted-by` without its `(role)`, has to be a fresh
  coordinate. Any violation refuses before a commit is recreated,
  `--dry-run` included, naming the file and line, and `--dry-run` lists
  each edit. Edits apply before `--drop`, then `--append`. Recreated
  commits keep their trees, authors, committers and dates, and signing,
  pins, retention and tags behave as for any rewrite. The
  `placeholder-contribution-trailers` advisory names this form instead of
  `--drop <key>`.

- Paired replicas without integration authority can explicitly prepare
  unsigned staging merges with durable publication obligations, recover
  interrupted ref transitions, stack applicable local provisional
  prerequisites, and reconcile canonical or asserted acceptance. Canonical
  policy and shared review eligibility govern preparation and settlement;
  status, catchup, and workspace report expose unresolved deliveries
  separately from review debt.

- Add arc accept to admit and prepare fetched change deliveries under local
  integration authority: bind a replica-local checkout, sign exclusive
  commits, preserve delivered snapshot attribution and contract
  coordinates, run local gates and bound baseline/final probes, and return
  a retryable preparation transcript with readiness and any local judgment
  still owed.

- Authorize file imports with operator-enrolled transport-asserted
  publishers, exact received-byte digests, exporter and origin
  capabilities, and atomic private admission receipts; scope review,
  waivers, debt discharge and corroboration to receiver-authorized origins,
  preserving local verdict precedence and reporting provenance in review,
  status and check.

- Catchup lists recent project sessions across harnesses and marks recent
  activity without filed artifacts or a naming handoff or conclusion.
  Journal filed groups artifacts by their writing session, retains
  distillation sources, and reports promotion, consumption, and recorded
  inactivity with fate counts. Catchup and workspace inbox surface
  actionable filings untouched for more than fourteen days, including
  projects without a change ledger.

- Operator-wide journal scope supports every journal kind and on-demand
  reads from any directory with --global, lists global memory titles and
  ages beneath project memories in catchup, promotes project memories with
  bidirectional links and superseded source records, and exchanges global
  journals through bundles without requiring a Git repository.

- Integration cleanup accepts integration.discard = "artifacts" or
  "ignored" and --discard <artifacts|ignored>, which enables cleanup for a
  single change or queue. The ignored scope discards Git-ignored files and
  directories while retaining tracked edits, untracked files, .arc/ outbox
  content and checkout ownership guards; discarded entries and dry-run
  cleanup scopes are reported.

- Record explicit planning and orchestration roles on changes with roles,
  begin, and brief; project portable contribution trailers from assertions
  and declared snapshot identities, report unresolved coordinates, and
  advise before integration when commits omit asserted lines without
  blocking authorization or backfilling review credit.

- Change bundles include a role-labelled Git object list derived from their
  exported events, including delta exports, retained evidence trees and
  synthesized verification merges. Export can write the list separately
  with --objects-json and reports counts by role. Import requires every
  listed object before writing events or refs; --provenance-only permits an
  event import while reporting unavailable objects, and bundles without the
  list retain advisory commit checks.

- Build revision and tracked-tree state are recorded at compilation and
  reported by arc env and arc doctor, so binaries sharing a release version
  can be distinguished. Text catchup warns when the running build differs
  from HEAD in an arc-ledger checkout, identifies ancestor builds as older,
  and marks dirty builds; release versions remain unchanged.

- Journal artifact imports reconcile append revisions between paired
  replicas, retain longer prefix bodies, and merge concurrent
  event-identified sections in deterministic timestamp and identity order
  while preserving provenance and idempotent receipts; edited bases or
  shared blocks refuse the whole bundle with both digests and the first
  differing block.

- Post-integration cleanup recognizes ignored build-artifact directories by
  what their tools write inside them, never by name: a valid CACHEDIR.TAG
  (Cargo, Meson, uv, pytest and others), a Cabal `dist-newstyle` without
  dependency checkouts, `*.egg-info` metadata, `__pycache__` holding only
  tagged bytecode, and layered `integration.disposable` path-and-marker
  rules for anything else, such as CMake build directories. Cleanup reports
  each discarded directory with what identified it, and keeps the ownership
  requirements and any checkout holding other local content.

- Integration applies the resolved integration.cleanup style default from
  user configuration, target project defaults, and operator defaults to
  each selected change. The --cleanup and --no-cleanup flags override that
  default and are mutually exclusive.

- Resolve integration shape and inspect checkout, branch prefix, shape and
  cleanup style through flag, operator defaults, target project defaults,
  user configuration and built-in values. Project and operator shape
  constraints intersect, invalid or disallowed values identify their
  sources, and integration history folds restrictively with contribution
  history. arc config exposes arc-config/1 with winning and overridden
  values and binding constraints.

- The document `arc watch --json` prints and `arc watch --exec` reads on
  stdin carries `schema: "arc-watch/1"` on every outcome (reached for one
  change, `--any`, `--all`, a timeout, and a journal artifact), and the
  guide's SCHEMAS section lists it.

- `arc status` and `arc check` name an open change that has no changelog
  record as the never-blocking `no-changelog-entry` advisory, and `arc
  changelog <change> --none --reason <why>` records that a change needs no
  entry: it answers the advice in status, check, and integrate, and the
  projection emits nothing for it.

- `arc begin` warns on stderr when the target branch lacks commits its
  configured upstream holds, as the last fetch left the remote-tracking
  ref: a target strictly behind gets both revisions, the fast-forward that
  brings it level (`git merge --ff-only` in the checkout holding it, or
  `git fetch . <upstream>:<target>` when none does), and `arc rebase` for
  the change; a diverged target is named as having no fast-forward. The
  change still opens, and an explicit `--base`, a level or ahead target, or
  one tracking no upstream gets no warning.

- `arc integrate <change> --dry-run --json` prints the integration plan as
  `arc-integration-plan/1`: the authorization basis the integration event
  would record, the target revision it was decided at, the approved head,
  and the tree the merge would ship, decided by the same function as the
  merge. `arc integrate <change> --expect-basis <file>` hands that plan
  back; the fresh decision alone still decides, and when the approved head,
  the target revision, the gate or policy declarations, or the approval
  moved since, a refusal adds one line naming what moved and a permitted
  integration proceeds with a warning naming it.

- `arc candidate verify`, `select`, and `promote`: a candidate's tree is
  evaluated against the required gates in a scratch checkout; a named
  selection is validated against every ground (reuse policy, target,
  per-gate evidence at the shipped tree, and the brief's `--must-read`
  requirements met by tool reads) with each failure reported by code; and a
  permitted selection is promoted into an ordinary patchset by a locked
  transaction whose interruption `arc doctor` reports and `candidate
  promote` completes or discards. `arc brief --must-read` declares read
  requirements (`arc-brief/2`), selections and promotions keep candidates
  from retirement, `arc explain` lists a promoted candidate's siblings, and
  `arc workspace report` flags `candidates-unjudged`.

- `arc context` records what a change or a candidate read and relied on,
  each with how it is established. `arc context read` records a tool's read
  record — the returned bytes' `sha256:`, the path, and the recorded range
  (whole, lines, or unknown) — by hand or from a `tapes events --json`
  document or `tapes export` bundle, skipping failed calls and reads with
  no path or digest with a printed reason. With `--at`, the blob a path
  names at a revision is recorded only as an inference, when the bytes
  match. `arc context declare` records an attributed cites, relies-on, or
  considers claim, whose citation must name a recorded read; `arc context
  capture` records the provider's pinned or unpinned report. `arc explain`
  fills observed reads, marking a read `at risk` until its recording is
  reported pinned, and `arc candidate show` lists a candidate's relations.

- A change can be opened from another project's journal. `begin
  --from-journal`, `brief --plan-ref`, and `snapshot --journal-ref` accept
  `<journal-dir>::<file>`, where `<journal-dir>` is the absolute path the
  owning project's `arc journal dir` prints. The artifact is read hot, then
  cold, from that journal, and the reference is recorded as given. A
  directory that is not a journal, or a file it does not hold, is refused
  naming both. The owning journal records the promotion as a `promoted`
  event naming the promoting repository id, change, and checkout, and
  consumes a non-plan source as superseded by that change. Its `inventory`,
  `open`, and workspace views list the promotion with its `repository_id`,
  with the status read from the promoting ledger. Journal writes that
  change an artifact's disposition refuse a reference to another project's
  journal, naming it. `arc-workspace-backlog/19`,
  `arc-workspace-inventory/2`, and `arc-catchup/12` carry the promotion
  fields.

- `arc stats --provenance` reports how often each provenance record is
  written where it could have been: falsification on passing gate evidence
  (declared, inferred, none, and none after an earlier failure), journal
  links on patchsets split by whether the change was opened from a journal
  artifact and counted by `via`, integrated changes with a rejected
  alternative, plan-linked briefs, and cited kept facts. Every count
  carries its population; `--json` emits `arc-stats-provenance/1`.

- `arc candidate register|show|list|judge|retire`: register alternative
  answers to a brief as repository-scoped candidates that open no change,
  pinned at `refs/arc/candidate/<id>` and carried by every `arc export`;
  `arc-candidate/1`, store format 7, the `[candidates] evaluation_reuse`
  policy key, and `arc doctor` checks for unregistered candidate refs and
  rooted candidates whose pin is gone.

- `arc keep --cites <event>` names the verification, verdict, finding,
  disposition, or earlier kept fact on the same change that a kept fact
  rests on. Each id is checked when the fact is kept and refused if it
  names nothing on the change or another kind of event. `arc-state/3`,
  `arc-status/27`, `arc-resume/8`, and `arc-bundle/6` carry the citations;
  import still reads `arc-bundle/5`.

- `arc workspace report` flags two or more open changes whose in-force
  briefs share a plan and slice, or a body digest, as the
  `shared-plan-slice` attention fact (`arc-workspace-report/3`), and `arc
  workspace report --help` lists every attention rule the report can emit.

- `arc explain <change> [--at <event>] [--json]` renders what one change
  knew and what it was accepted on: its contract, supplied journal context,
  declared facts, observed reads, rejected alternatives, gate evaluation,
  coverage at acceptance, and later knowledge. Every row carries a standing
  (`recorded`, `declared`, `inferred`, `absent`, `unavailable`) no stronger
  than its source; journal references are checked against the journal now
  as `same`, `amended`, or `missing`; coverage stays as of the integration
  event while later audits and debt discharges are listed apart. JSON is
  `arc-explain/1`. The command writes nothing.

- A passing verification records, as `falsification_inferred`, the newest
  earlier failure of the same gate (or command) on its change. The
  inference decides nothing: only a declared `--falsified-by` makes a gate
  discriminating. Schemas: arc-state/3, arc-status/27, arc-rescue/5,
  arc-bundle/6 (import still reads arc-bundle/5).

### Changed

- `arc cost show` reads sessions faster, with the same figures: on one
  ledger, one change went from about 2.2 s to 1.4 s and `--all` from about
  12.5 s to 12.0 s. A large recording's lines now decode ahead in parallel
  batches, which raises `--all`'s peak memory by about 130 MB.

- `arc catchup` builds each open change's status report once, rather than
  again for every listed row whose review subject it prints.
- `arc catchup` names the touched paths of every owed debt over the same
  commit range. The first such debt used to take the paths and leave the
  rest with unknown surfaces, missing from the debt lines beneath an open
  change that edits those paths.

- `arc catchup` and `arc inbox` run the same number of Git commands however
  many changes are open: the merge, ancestry, commit-range,
  worktree-cleanliness and touched-path reads for every listed change are
  answered by a fixed set of batched Git commands.

- `arc cost show` reads every session through one set of session stores per
  reader thread, so a whole-ledger view walks each store and reads
  recording headers far fewer times.

- `arc catchup` reads each journal once and runs a fixed number of Git
  processes however many changes and debts the ledger holds; on a ledger
  with 46 owed debts it runs about a third of the Git processes and takes
  half the time.

- `arc cost show --commit`, `arc cost note`, `arc catchup` and the other
  one-shot ledger readers (`status`, `show`, `list`, `query`, `inbox`,
  `explain`, `stats`, `resume`, the workspace reports and `journal
  catchup`) read the ledger once per command instead of once per change or
  section, and locating a commit's change no longer starts a Git process
  per change. On a ledger of 560 changes, `cost show --commit HEAD` drops
  from about 2 s to under 0.9 s, and `catchup` from about 9 s to about 3 s;
  `catchup` now discovers harness recordings while it reads the ledger and
  journal. Output is unchanged.

- A debt's production record names the `Implemented-by` values of the
  commits in the patchset's range, read when the debt is declared and kept
  verbatim as `trailer_implementers`, beside the identity that recorded the
  patchset. A lead who snapshots and integrates an executor's commits is
  the recorded implementer, and the trailers say who did the work. A range
  with none records an empty list, shown as "no Implemented-by trailers";
  `arc catchup`, the workspace backlog, `arc explain` and `arc show` show
  the list wherever it is empty or differs from the recorder. Records kept
  earlier read unchanged.

  `arc audit` and `arc review` compare the reviewer's model, ignoring
  effort, with the models the range's `Implemented-by` lines name. A match
  warns; where `forbid_self_approval` applies, an approving audit is
  refused and a review does not gate, as for a contributor match.
  `--verdict changes-requested` stays open to anyone. An
  `ineligible-review` debt clears only when the audit's assessment lists
  authors answering to the model of every `Implemented-by` line in the
  range; otherwise the audit is recorded with binding `authors-uncovered`,
  names the uncovered models in `uncovered_authors`, and the debt stays
  open. Where the range's lines cannot be established (no list kept and the
  range does not read) the binding is `authors-unread`, never `bound`, and
  the debt stays open.

- `arc cost show` qualifies every counter, disjoint category and dollar
  figure of a session when tapes reports that a record carried usage no
  reading counted, with the fixed reason "some records carrying usage were
  not read". Records that carry no usage, such as Claude's attachment
  notes, qualify nothing, so a session is not qualified merely for holding
  unrecognized record types. `arc audit --assessment` also binds the
  assessment to the auditor: the audit's recorded model (and effort, where
  both sides name one) must match the reviewer's `identities` and `effort`
  in the `agent-routing assess --json` file. Only the outcome (`bound`,
  `model-mismatch`, `effort-mismatch`, `model-unrecorded` or
  `identities-unstated`) is kept. An audit whose binding is not `bound` is
  recorded and listed among the later reads with its reason, and does not
  clear an `ineligible-review` debt. tapes is pinned at 3b168bf.

- `arc cost` reads tapes' typed measurement completeness instead of
  re-deriving it. Each counter and disjoint category carries one fixed
  reason per reason tapes names (missing counter or price, written zero
  price, stopped request, unreadable record, read window, counter reset,
  unestablished scope, unattributed request, undetermined category), and
  the disjoint categories retain their own gaps beside their amounts. A
  Claude request sum or recorded total whose counters every counted event
  or model recorded is now complete per counter, where a counter without
  per-event coverage was a known subtotal. No count of prose notes
  qualifies a figure; only a reader's `--latest` selection warnings are
  counted as warnings.

- `arc run list --json` changes its top-level shape from a bare array to an
  `arc-run-list/1` object carrying `schema` and `runs`. The `runs` array
  retains all run fields, including `binding.conflicts`, `journal` and
  `journal_lineage`.

- arc no longer writes the word `unknown` for an identity it could not
  establish. A journal event omits the `harness` or `session` key nobody
  established, and every event arc writes is `journal-events/2`
  (`journal-events/1` events are still read, relayed unchanged, and an
  `unknown` in either key reads as absent). A write with no `--actor`,
  `ARC_ACTOR`, harness session or `git config user.name` refuses, naming
  `--actor` and `ARC_ACTOR`, and a read proceeds. Claim and lane refusals
  test absence, not the string. The spool, questions, discussion and
  catchup surfaces take the next version where they carry these keys:
  `arc-journal-spool/2`, `arc-journal-questions/4`, `journal-discussion/5`
  and `arc-journal-catchup/9`.

- `arc --version` prints the release on its first line and `build: <commit>
  (clean|dirty)`, or `build: unknown`, on a second. `-V` prints the release
  line alone.

- Closes the closed-world danger classification: six source files join
  [danger] paths and operation_output.rs joins acknowledged_safe, so arc
  doctor reports no danger-unclassified.

- A commit that a verdict read, or that a patchset recorded as its head,
  now stays reachable after a rewrite. `arc rewrite sign`, `arc rewrite
  trailers` and `arc history rewrite --map` hold each such commit they
  replace under `refs/arc/retained/<change>/<subject>/<commit>`, created in
  the same ref transaction as the moves, restored with them on a failed
  recording, finished with them on resume, and named by the rewrite event's
  `retained` map. The keep pin still moves onto the successor; a retention
  ref never does. `arc doctor` reports a retention ref whose object is gone
  (`retained-object-missing`) and a cited commit no ref reaches
  (`unretained-commit`). `arc export` lists the retained commits under the
  `retention-commit` role, `arc import` holds them, and integration
  releases a retention ref only where history holds its commit.

- `arc trailers` never prints a placeholder or a session id as a
  contribution coordinate. A contributor named by its harness session
  (`codex:<uuid>`), a recorder whose model is `unknown`, or a historical
  `Planned-by: codex:unknown` assertion is omitted and named in a note with
  the value, why (placeholder, session id, unresolved or undeclared) and
  the remedy, such as `arc roles <change> --withdraw
  'Planned-by=codex:unknown'`. A recorder whose actor arc derived from the
  harness session is credited only after it declares the author: the note
  names its recorded model and the `arc snapshot --solo`, `--contributors`
  and `snapshot --amend` remedies. `--solo` under a derived actor projects
  the recorder's `harness:model`.

  `arc roles --planned-by` and `--orchestrated-by`, and `arc brief
  --planned-by`, accept no value, as `arc begin` does. An omitted value
  records a declared `Name <email>` actor's own form, or else the resolved
  `harness:model` under a declared or session-derived actor; an identity
  assumed from git config, or an unresolved harness or model, refuses.

  `arc check` and `arc integrate` advise under
  `placeholder-contribution-trailers` when a commit in `target..head`
  carries a role line with a placeholder or a session id, naming each
  commit and line. The advice never changes readiness or the exit code. The
  `missing-contribution-trailers` advice no longer suggests appending an
  assertion that is not a fresh coordinate.

- A write refuses a placeholder declaration (`unknown`, `none`, `n/a`,
  `tbd`, `<synthetic>` and the like) as the harness, session, actor or
  on-behalf-of subject, and a placeholder or a session id as the model,
  before it records anything and under `--dry-run` too; the refusal names
  the remedy, such as unsetting `ARC_MODEL` so arc records the observed
  model. Reads and Git hooks are unaffected.
- A model the session store or Pi's live values report as a placeholder or
  a session id is no model: `arc env` names it as unavailable with the
  value it carries, writes record no model from it, and a live Pi value of
  that kind leaves the recording to answer.
- Role assertions, `--contributors` and `--solo`, planner JSON (typed, in
  body headers, or inherited by a plan successor), `journal correct`
  identity fields and `journal reattribute` replacements refuse placeholder
  and session-id values. `rewrite trailers --append` refuses any role line
  that is not a fresh coordinate, malformed ones included, before any
  commit is recreated. Withdrawing a historical assertion still works, and
  historical records read unchanged.
- `arc instructions git --check` reports a readable role value that carries
  a placeholder or a session id, or names a person without an email, as
  `placeholder` and exits 1. The convention is
  `arc-contribution-trailers/2`.

- A session lookup that fails says why. `arc env` appends the reason to its
  unresolved model and session lines, and events carry it in a new
  `session_resolution_reason` field beside an `uncorroborated` or
  `unresolved` verdict. A session id that only prefixes a recording, an
  unreadable recording, and an unreadable Claude subagent lineage are
  unresolved rather than read as no recording or no subagent, so no model
  is named for them; `arc rescue` reports the prefix case as unreadable
  with both ids. Writes that declare `ARC_HARNESS` and `ARC_SESSION` record
  the store's verdict as detected ones do. A configuration that cannot be
  loaded leaves identity detection off with a warning on stderr.

- Baseline acceptance probes run in an automatically created detached
  checkout at the brief base when the invoking checkout is at another
  revision. Arc reports the scratch path and free space, removes the
  checkout after verification, and prints a manual checkout recipe if
  creation fails; attested probes and checkouts already at the base retain
  their in-place behavior.

- Begin resolves checkout and branch prefix from user, target project, and
  operator defaults. The built-in checkout is in place for every profile;
  --worktree optionally selects a path and --no-worktree explicitly selects
  in-place switching. Adopted branches retain their existing checkout
  behavior, and recorded change branches are excluded from implicit
  integration-target discovery regardless of prefix configuration.

- Local integration defaults to guarded fast-forward through --shape
  ff-only, refusing diverged targets with rebase and review guidance;
  --shape no-ff selects a merge commit. Integration events, state, plans
  and logs name fast-forward, merge or already-contained topology, and
  queues stop at the first non-stacked member under ff-only.

- Brief revisions preserve their recorded base unless --base selects
  another revision and report the selected base. Repeatable --probe
  NAME=COMMAND and --withdraw-probe NAME edit acceptance probes while
  carrying other declarations forward; --probes-json replaces the complete
  array.

- begin --no-worktree switches the invoking checkout to the change branch
  while preserving staged, tracked, and untracked work when Git can carry
  it across; overwrite conflicts and branches checked out elsewhere fail
  visibly without opening a change.

- `arc workspace report --json` is schema `arc-workspace-report/4`: each
  review-debt row carries the declared `reason`, the `patchset_id` it names
  (null when none), `declared_at` and `declared_by`. `arc workspace backlog
  --json` is schema `arc-workspace-backlog/20`, whose debt rows carry
  `reason` and `patchset_id`. A `/3` report is refused as a `--previous`
  baseline.

- Human `Next step:` lines in `check`, integrate refusals, `status`,
  `show`, `review`, `resume`, and `rescue` print the command that does the
  next step, with the change filled in and the `next_action` code in
  parentheses (for example `arc verify <change> --gate test
  (run_gate:test)`), instead of the bare code; `--json` still carries
  `next_action` as the code alone.

- Input rules are stated in `--help`: `arc review` says `--cause` is
  required with `--verdict changes-requested` and refused otherwise, and a
  malformed findings batch names the help that states its shape; `arc
  brief` says which version a write records and that v1 refuses a cause
  while every later version requires one; `arc journal plan --planned-by`
  names the planner fields, with effort carried in `model` as
  `<model>#<effort>`, and its unknown-field refusal names them too.
  Recording a changelog entry, or `--none`, infers an omitted CHANGE from
  the branch or worktree instead of refusing.

- `arc watch --until stalled` names what it measured: the reached line
  reads `reached: stalled (stage <stage> for <age>, budget <budget>; reset
  by arc stage or arc snapshot)`, per member under `--tag`, and the JSON
  object and `--exec` diagnostic carry `stage`, `age_seconds`, and
  `budget_seconds`. The help and guide state that stalled is a stage clock
  restarted only by `arc stage` or a snapshot under the claim, not by
  output, logs, or claim renewals.

- `arc doctor --help` and `arc journal doctor --help` state their exit
  status: 0 for a clean or advice-only report, 1 when a problem is reported
  or the store cannot be read, 2 on a usage error. `arc check --help` lists
  every exit code by blocker in precedence order, rendered from the blocker
  table itself, and states that `--tag` exits with the first blocked
  change's code.

- The guide tells concurrent writers never to run `git stash`, because
  every worktree of a repository shares `refs/stash`; work in progress is
  committed and amended, or copied aside.

- A journal reference given to `--journal-ref`, `--from-journal`,
  `--plan-ref` or `--must-read` that names a path is refused naming the
  reference that resolves it: the bare filename for this project's journal,
  `<journal-dir>::<file>` for another's. One that names a filename this
  project's journal does not hold is refused naming each other known
  journal that holds it in the `<journal-dir>::<file>` form, or that form
  when none does. A journal write given a path into another project's
  journal names the journal that owns it.

- A discussion opened with `arc journal note --kind discussion --body-file`
  places the opening body under `## The question`, above `## Positions`, so
  every position appended follows that heading and the topic statement no
  longer reads as the first position. Scaffold templates mark the body's
  place with a `{{body}}` line; a template without one is followed by the
  body as before. Existing discussions stay as filed.

- The guide names a fifth journaling moment: anything worth doing outside
  the current change is filed in the owning project's journal when it is
  noticed, with `--source` and `--item-key`, and the end of a session
  checks that nothing is left rather than being when filing happens.

- The guide states the review default once, under a section renamed REVIEW
  COVERAGE AND DEBT: for ordinary work the lead's own verdict, or a
  declared debt where the lead contributed or policy refuses its approval,
  is the review of record, and an independent reviewer is asked for only
  where policy requires one or on request. The `local` profile row and
  README point at that rule instead of suggesting a fresh review first.

- The guide states the history model — what a registration, a selection, a
  relation's standing, a retention root, and a debt are and are not — and
  the network guarantee as an invariant: every transport arc offers is a
  file the caller moves, and the programs it runs on its own behalf are
  local.

- `arc explain` reads the provenance its sources record: the opening
  artifact is `recorded` and resolves as `same`, `amended`, or `missing`
  when its digest was captured at open (`declared` otherwise); a counted
  pass lists its inferred falsification as `inferred` beside any declared
  one; each patchset link shows how it was supplied (`via`); and kept facts
  show the events they cite, still `declared`.

- arc env no longer suggests exporting ARC_MODEL when no session is
  detected; each write resolves its own model.

- Leave ARC_MODEL unset during identity bootstrap and report model
  observations as comments. Resolve models once per write invocation,
  retaining source, coordinate, turn relation, and declared/observed
  disagreements in arc show --json and arc log. Share arc-bundle/6 and
  arc-state/3 across unreleased shape changes; import ledger /5, replica
  /2, and journal /1 alongside current formats. Optional model provenance
  keeps store format 6.

- `arc snapshot` and `arc done` without `--journal-ref` link the journal
  artifact the change was opened from and the plan its brief names, each
  with the digest read at record time and a `via` of `begin` or `brief`;
  flagged links record `via: flag` and replace the defaults. A default that
  no longer resolves is skipped with a warning. `arc begin --from-journal`
  records the opening artifact's digest. Schemas: `arc-state/3`,
  `arc-status/27`, `arc-journal-inventory/6`, `arc-bundle/6` (import still
  reads `arc-bundle/5`).

- `arc rewrite sign --retag` and `arc rewrite trailers --retag` now sign
  every tag they recreate with the key the commits are signed with
  (`--key`, or Git's configured signing key), whether or not the original
  tag was signed, keeping its message, tagger and tagger date. `--no-sign`
  leaves recreated tags unsigned, and `--dry-run` says which tags would be
  signed.

- Brief probe warnings and review checklists use the change target's
  declarations. Gate declaration edits require independent review alongside
  policy declaration edits.

- The `docs/` pages are retired; `arc` and `arc <verb> --help` are the
  reference. The guide gains a FILES section (user configuration, every
  policy and gate key, keep refs), a SCHEMAS register of every schema
  version with its surface and stability, and the commands each execution
  role is refused. `events --exec`, `watch --exec`, `claim --stage-budget`,
  `hooks install`, and `journal dir` state their hook environment, default
  budgets, hook behaviour, and resolution order in their help.

### Fixed

- Reads under a model named by the debt production record's Implemented-by
  trailers, at any effort, leave review debt owed even where self-approval
  is allowed. Audits and pre-merge verdicts use the same independence rule.

- `arc cost evidence` drops the query and fragment of the `origin` URL it
  writes as `repository`, where a token could pass through, and leaves out
  a `FILE://` origin as it leaves out `file://`. An export tapes would stop
  reading before its end, more than 10,000 records after the header or a
  line longer than 256 KiB, is refused and writes nothing. The help and
  guide state the order the file is written in, lineage first and then each
  session's records by time, and that an unchanged ledger's exports also
  differ in the end of each span of a run still open.

- `arc cost show` names a session whose read failed because its store
  stayed busy or unavailable through every attempt tapes makes, such as a
  locked OpenCode database, unmeasured with `usage read failed transiently;
  a later read may succeed`, apart from a read that failed for good, which
  keeps `usage read failed`.

- `arc cost show` names a Claude cloud session recorded as a subject,
  `claude:cse_…`, as a session: when nothing on this machine holds its
  recording, it is an unmeasured participant, `not located on this
  machine`, and the totals it would join are withheld instead of reading as
  complete without it.
- `arc cost show --commit` describes each session behind a trailer with the
  status word the change's table gives its row (`status`), keeping the
  reader's `measurement` status beside it in the JSON. One row no longer
  reads `incomplete` there and `complete` in the table.
- A figure whose counters cover less than the session names that in words:
  `the counters cover only the window the read reached, …` or `the counters
  cover only what followed their last reset, …`, never a kebab coverage
  name inside a sentence. Facts retained with the earlier wording read back
  in the new words.

- A list price priced share by share, where a recording splits a figure by
  several models, prices each model's cache writes by the TTLs its
  recording names, as `tapes usage --prices` does, instead of leaving them
  TTL-unrecorded between the 5-minute and the 1-hour rate.

- A list price names only the reasons tapes names for a category a share or
  a whole figure holds no amount for, such as a Codex uncached input the
  counters leave undetermined; it no longer adds that some counted events
  did not record the category.

- A figure one model served whole is qualified with the
  unattributed-request reason where the read leaves attribution uncertain,
  as tapes qualifies it, including from retained facts and in a divided
  session's segments.

- `arc cost show` reads an `--on-behalf-of` subject written as
  `harness:<session>:<model>#<effort> (<label>)` as the session it names,
  measured as a participant and attributed to the model the subject
  declares. Such reviewers were dropped, with their consumption, from every
  figure.
- A session a dispatch bound as implementer that recorded verdicts,
  integrations or dispatches of its own holds those roles beside its
  binding's, in the table and for `arc cost show --commit` trailers: its
  verdicts answer a `Reviewed-by` trailer naming their model, which read
  `no session recorded`. Its own snapshots still make it no reviewer.
- A subject whose id is shaped as its harness shapes a session id (a UUID,
  an OpenCode `ses_…`) and that nothing on this machine locates is named as
  an unmeasured participant, `not located on this machine`, so the totals
  it would join are withheld beside their known subtotals instead of
  reading as complete without it.

- `arc cost show --footprint` selects every change an event of any kind
  names the session on, its own or recorded on its behalf, the same work
  its membership holds; a reviewer whose only event on a change was a
  disposition recorded for it is counted whole in its footprint instead of
  `shared ×N` with the footprint total withheld.
- `arc cost show --commit` matches a `Reviewed-by`, `Planned-by` or
  `Orchestrated-by` trailer to a participant by its own model: the models
  its own events declared, else the one its recording says served it, never
  the lead's model on an event recorded on its behalf. A participant no
  source gives a model is listed as unmatched rather than reading `no
  session recorded`.
- A reviewer recorded `--on-behalf-of` an OpenCode session (`ses_…`) or a
  Pi session with a named id is a participant: a subject names a session
  when its harness's recording is found, whatever shape that harness gives
  its ids.
- A dispatch route naming another harness than its executor's, such as
  `codex:gpt-6.1-sol#high` bound to a Claude session, is shown beside the
  agent, `claude:claude-opus-5-5 (route codex:gpt-6.1-sol#high)`, instead
  of `claude:codex:gpt-6.1-sol#high`.

- `arc cost show` prices a Pi session that kept an abandoned branch by the
  model each of its requests names, as tapes prices it. A request on an
  abandoned branch served by another model than the active branch is no
  longer priced at the active model's rates, which could report a complete
  amount or a bound below what the session cost; retained facts that keep
  no per-model shares for such a session leave its list price absent with
  the reason `some requests could not be attributed to a model`.

  `arc cost show` no longer prices a request no model is named for as the
  model that served the rest of the session. Where tapes reports uncertain
  model attribution and a session's per-model split leaves requests to no
  model, as when a Codex request precedes any model selection, the list
  price covers only the split's shares and is a known subtotal with the
  reason `some requests could not be attributed to a model`, never
  complete.

- `arc cost show` counts a reviewer whose verdict or audit a lead recorded
  with `--on-behalf-of <harness>:<session id>`. The reviewer is a
  participant of each change it reviewed and is no longer missing from
  every figure and footprint. It is counted whole where the selection holds
  all of that work, and withheld, by name, where it does not. Those events
  carry the lead's time, so they never divide the reviewer. The agent cell
  names the harness once when the dispatch route already carries it:
  `claude:claude-opus-5-5#medium`, not
  `claude:claude:claude-opus-5-5#medium`. An `Implemented-by` trailer now
  finds the run whose route has that form.

- `arc cost show --prices` prices a model served at several efforts by
  every request it served, so a figure whose requests are all timed is a
  complete amount under a version 2 table's time-of-day, long-context or
  validity terms instead of a range, and an item's share or the
  unattributed part of a divided session keeps each of its segments'
  models. A session served by one model at several efforts shows that
  model, without an effort, in the agent column.

- `arc cost show` prices a Pi figure under a `tapes-price-table/2` table's
  time-of-day, long-context and validity terms by each of its requests at
  its own time, on every branch its recording retains, instead of leaving
  every Pi figure a known subtotal at the least it can cost.

  `arc cost show` shows the most a list price in doubt can cost beside the
  least. A cache write with no recorded TTL, a request term the figure's
  requests do not settle, or billing the recording does not establish
  leaves a known subtotal with its range under `bound`, or `known_bound`
  where other reasons qualify the figure; totals add both ends, and the
  table shows such a cell as `least–most`.

- `arc cost show` prices a `tapes-price-table/2` table's terms set by the
  request: a long-context surcharge applies to each request whose prompt
  passes the threshold under the billing the recording establishes, with no
  surcharge for subscription usage, and time-of-day rates and validity
  windows apply at each request's time. A figure whose requests are not
  held one by one, such as retained facts, shows the least it can cost as a
  known subtotal with the reason, never a complete amount at flat rates.
  The list-price figure names the table's own schema.

  `arc cost show` no longer reuses a participant's retained working-stretch
  estimate under a different `--idle` gap and labels it with the new gap:
  retained facts estimated at another gap estimate nothing for that view,
  with the reason `retained facts were estimated at another idle gap`, and
  their measured split stands.

  `arc cost show --idle` refuses a value containing a non-ASCII character
  as a usage error naming the value, instead of panicking.

- A session's unassigned native children figure no longer reads complete
  when one of those children has no figure: it reads absent beside its
  known subtotal and names the child.

- `arc cost retain` reads the repository ledger once per run.

- `arc cost show --commit` and `arc cost note` no longer answer "no change
  records this commit" when a change's state could not be read: they name
  each unreadable change on stderr and in the answer, list them under
  `lookup` in the JSON view, and mark a match found beside one as possibly
  not unique.

- A list-price estimate of a share in which one model served at several
  efforts now prices all of that model's consumption, not only its first
  effort's.

- `arc cost show` no longer reports a complete total when a session that
  acted on the work could not be measured. Such a session is listed as an
  unmeasured participant, every total it would join shows only its known
  subtotal with the session named as missing, and measurement and
  assignment coverage both read incomplete.

  It also asks `agent-status` whether each participant session is still
  running. A session it lists is live and its figure stays incomplete; a
  Claude, Codex or Pi session it no longer lists has ended and prices
  complete. When `agent-status` is missing, fails, does not answer within
  two seconds, or has no registry owner, the figure stays incomplete and
  says why. `arc cost retain` records the answer with the facts it keeps,
  so facts retained after a session ended price complete once the recording
  is gone, while facts retained during the session stay incomplete.
  `ARC_AGENT_STATUS` names another program to ask.

- Codex model resolution searches bounded metadata pages beyond the
  transcript tail and checkpoints unchanged recordings. Later model or
  effort changes invalidate the checkpoint; unresolved scans expose a
  repeatable recovery path without guessing attribution.

- Gate and environment-probe timeouts also bound inherited output-pipe
  cleanup, including descendants outside the original process group.
  Snapshot verification and done reuse the newest green evidence at the
  same tree; explicitly selected gates rerun.

- Doctor reports unreadable project and operator policies as problems with
  their file and parse error, including when authorization inspection
  cannot proceed.

- New candidate evidence attachments require the registration's original
  request and matching declarant before import publication; held legacy
  attachments remain readable with explicit unbound standing. Context reads
  accept the current pinned Tapes event and session schemas alongside the
  established legacy schemas.

- Snapshots honor an explicit represented subject at unchanged content,
  preserve attribution on omitted or equal subjects, and name the effective
  subject in their output. Candidate retirement documentation and
  regression fixtures distinguish retained evaluation evidence from content
  pins, including later reuse after supplier retirement.

- `arc events` and `arc export` read a ledger that cannot be written. They
  take the event-ID lock shared and read-only, and never create it, so a
  read-only ledger replays and a reader leaves no lock metadata behind.
- A commit map is read without regard to letter case: `arc history rewrite`
  records hashes in lowercase and retention holds a cited commit an
  uppercase map names, in full or abbreviated, so garbage collection cannot
  remove it. Recorded maps are compared the same way.
- `arc doctor` reports a dangling revision that a falsification cites even
  when an admitted rewrite proves the failure's own revision the same
  commit.
- A debt declaration that predates the coverage session field reads each
  entry's session from the verdict it captured, so a recording key is no
  longer presented as a model.

- Journal integrity. A pending successor, one whose publication evidence is
  incomplete, is refused as a `journal transition` source and as a claim
  target. A position's stance reads identically in the discussion tally, in
  `journal show --json`, and in the `--stance` duplicate check: an indented
  or fenced `Position:` line is prose, and entry bodies keep their
  indentation. An entry ID names one entry, so a fenced quotation of a
  recorded position, evidence, or question heading no longer creates a
  second entry. Lane, claim, and discussion activity take the latest
  observed timestamp, so a spooled write promoted late cannot age them.
  `journal export` carries every log record written about an artifact,
  including records this build cannot read, so a receiver holds the
  artifact back instead of admitting a copy without them. A question
  answered again after a retraction is settled by its new answer:
  amendments of `answer:<question id>` bind to the answer they were made
  against, recorded as the optional `settlement` event field. `journal
  doctor --json` help describes the report it emits, and the
  `arc-journal-open/1` description sits on `journal open --json`.

- A command changelog renderer's request carries the text-side projection:
  each entry's recorded session is null and its id is masked as `[session]`
  in the other recorded values, as `--provenance` text already did.
  `--json` keeps full session provenance.

- `arc rewrite trailers --append` refuses an `Assisted-by` value that is no
  fresh coordinate, a placeholder, empty or the invoking session's id, as
  it does a role line and as `--edits` does.

- An unreadable or invalid `.arc/changelog.toml` no longer aborts `arc
  check` or `arc check --tag`: it warns on stderr, declares nothing and
  leaves readiness and the exit code unchanged, as `arc status` and
  integration advice already did.

- Forge import judges each received projection against the read, link and
  declaration it was recorded with, and each received delivery or
  disclosure source against the original read it names, so a re-export
  whose history interleaved replicas imports into a fresh receiver. Local
  writes are still judged against current state.

- Disclosure Git support takes only a held raw commit named by its full
  lowercase object ID. A held tag or blob, an uppercase spelling, or an
  abbreviation is refused at declaration and never pinned or exported as
  support, and each commit is pinned before the declaration is recorded, so
  a failed pin records nothing. The compact provenance line reads "Review
  cost: unmeasured." only when no cost reference exists; otherwise it names
  the reference's basis and as-of.

- `arc forge show --observation` and `--at` views describe the frontier
  they name. A frontier reports the checks rollup recorded there instead of
  null; `--at` on show, status and explain lists only the change events,
  run references, local records and their source and byte references the
  bound had recorded; a provider lookup applies `--observation` to the
  changes that hold the read and refuses only when none does; and a change
  outside a forge round trip no longer renders a Forge round trip section.

- Custody never follows a symlinked `custody/` or `custody/sha256`
  directory: observe, import, candidate registration, availability reads
  and `arc doctor` refuse it, and an import refuses before publishing
  events. Exports refuse overlapping `--output`, `--objects-json` and
  `--custody-dir` destinations and publish their artifacts together or
  restore what they replaced, so a failed sidecar publication no longer
  leaves a bundle bound to a missing sidecar.

- An import judges only the received-evidence events a bundle adds against
  the objects held here, and its refusal names the event. Evidence already
  held that a later-arriving commit contradicts no longer refuses every
  import; `arc candidate show` reports it as `diagnostics` and `arc doctor`
  as `held-evidence-mismatch` advice.
- Registration and import refuse placeholder and session-id coordinates in
  received evidence, and disclosure attribution (author, reviewer, editor,
  discloser, role participants, human comment authors) refuses placeholders
  and session ids; absence stays null.

- A review debt now reads where it stands. `arc show` prints `Declared, not
  yet integrated` for a debt on a change that has not integrated, where it
  printed `Owed (discharged)` beside `Coverage: none`; `Owed` after
  integration; and `Owed (discharged)` with its discharger only once an
  eligible review has answered it. `arc explain` gains a `debt` row on a
  change that has not integrated. `arc status --json` and `arc chain
  --review --json` carry `debt_standing` (`declared`, `owed`, `discharged`)
  beside `debt_outstanding`, and `arc show --json` carries `debt.standing`.
  `debt_outstanding`, the debt queues and the waiver keep their meaning.
  The schemas become `arc-chain/5` and `arc-explain/2`.

- Classify received-evidence validation as dangerous under the closed
  source policy.

- Require matching complete identities for journal claim and lane lifecycle
  events, preserve identity whitespace, keep structural positions out of
  evidence, preflight artifact and lane renewal input, and share quotation
  recovery across question and answer readers. Honour renewed claim TTLs,
  retain omitted renewal durations, and serialize lane state changes under
  the transition lock.

- Mark missing, placeholder, and session-id producer models as unrecorded
  in text views and derived JSON identities, including journal artifact and
  entry writers, filed and recent sessions, suggestions, and verification
  stamps. Preserve raw JSON model values beside an optional model_recorded:
  false flag. Validate review coverage and audit and verdict detail models
  against their originating sessions.

- Validate Claude plugin child endings against the parent's native session
  before resolving its model, and refuse non-regular recordings and child
  metadata without waiting for FIFO writers.

- Evidence blocks accept actor/model corrections and retractions. Journal
  doctor reconciles position and evidence blocks with typed events for all
  artifact kinds in hot and cold storage. Artifact JSON lists positions,
  evidence, corrections, and retractions. Evidence appends require a live
  artifact in hot storage. Fork JSON uses arc-forks/3 with a null
  base_branch when no base is established; missing claim coordinates cannot
  match a recorded owner. Unreadable hostnames retain the documented
  unknown host fact, and legacy placeholder harnesses/sessions do not
  identify reattribute sibling references.

  Journal entry readers treat indented headings and fence markers as code
  examples. Recorded position and evidence markers terminate quotation
  fences, keeping later entries visible to artifact JSON, discussion
  tallies, and doctor.

- Refuse repository change references consumed by optional contribution
  role flags, with an argument-order hint.
- Omit session identifiers from changelog provenance text, including IDs
  embedded in derived actors, while retaining structured provenance.
- Preserve space for lookup failure causes by stripping tapes' quoted query
  prefix before bounding diagnostics.
- Batch commit-range reads for contribution-trailer advice and history
  rewrites.
- Keep watch Git probe deadlines bounded when descendants retain inherited
  output pipes or block a piped input writer; terminate the probe process
  group and detach unfinished pipe workers at the deadline.
- Mask session identifiers within changelog provenance metadata values
  while preserving the provenance prefix and field names, including when a
  session value overlaps a label.

- Journal imports admit understood artifacts from mixed-version bundles,
  report each held-back artifact with its unsupported event kind and
  schema, and retry it on later imports. Admission preserves whole
  artifacts and their reference closure.

  Journal state replays in event-log append order, including imported and
  spool-promoted events. Observation timestamps remain provenance, and
  equal or reversed timestamps preserve append order.

  Change history checksums and import authorization digests use sorted JSON
  object keys. Journal bundle validation preserves struct-ordered and
  opaque event payloads independently of ledger serialization.

  Partial journal imports deduplicate understood physical log records even
  when question-state replay rejects them. Repeated imports preserve the
  event log and receipt event counts.

- Preserve provider-observed forge SHAs beside nullable local rewrite
  successors, and restrict hosted-check applicability to the recorded PR
  identity and observed head.

- Require every configured fetch source of an upstream tracking ref to name
  the change branch before permitting a rewrite without --include-shared.

- `arc env` and `arc begin` report unreadable Claude subagent enumeration
  or child endings as unresolved session lookups, without attributing the
  parent's model.

- Order local event ID issuance across processes through a shared ledger
  lock and durable sequence recovery. Preserve imported identities and
  provisional recovery; event cursors and change-export prefixes wait below
  unresolved reservations. Following event streams discover late imports
  below their initial cursor. Refuse exhausted event issuance while keeping
  non-event identifier allocation independent.

- Acceptance refusals for a branch head that differs from its delivery name
  the publishing-replica remedy: snapshot the rebased head with the
  implementing contributors, export again, and accept the new bundle.
  Replica guidance and acceptance help describe the same requirement.

- Refuse promotion of incomplete journal transition successors and
  distinguish pending recovery in backlog projections.
- Confirm the durability of matching staged transition bodies before
  recording relations.
- Reject forbidden planner coordinates on reads while preserving malformed
  correction provenance.
- Keep privilege-scope instrumentation out of production ledger reads.
- Preserve durable debt receipts through subsequent ledger-read failures.
- Reuse imported debts by confirming their exact backing publication.

- Configuration loading preserves filesystem errors instead of silently
  selecting defaults. Journal model repairs validate against the effective
  target session. Trailer rewriting rejects the invoking session as a model
  in appends and per-commit replacements. Fresh agent coordinates require
  nonempty harness, model and supplied effort segments while historical
  readers remain permissive.

- Replay rewrite-proven falsification recovery in full and delta imports
  and claim contests using admitted rewrite authority. Doctor accepts the
  same identity proof while preserving diagnostics for recorded revisions.

- Doctor retains collected findings and reports
  retention-reachability-failed when Git cannot complete its reachability
  walk.
- External history maps preserve retention refs after published or
  uncertain append failures and withdraw them only for confirmed absent
  events.
- Retention recognizes abbreviations against the full cited commit width,
  including SHA-256 prefixes longer than 39 characters.
- Exact-line trailer edits accept JSON-quoted fields to encode internal
  tabs, quotes, and backslashes while preserving unquoted fields and valid
  same-key attribution.

- `arc rewrite sign` and `arc rewrite trailers` take a remote-tracking ref
  as the change branch's own work only when its fetch source, read from
  `remote.<name>.fetch`, is a branch with the change branch's name. A
  branch fetched onto the change branch's tracking name is protected, a ref
  no refspec writes stays protected, and a target's branch fetched under
  another tracking name is protected as the target.

- A provisional integration run with `--json --debt` now reports the debt
  event it attempted when publishing that event fails. The report names
  whether the event is `durable`, `visible-unconfirmed`, `absent` or
  `unknown`, and a retry that answers with a visible event syncs it. The
  transition schema moves to `arc-provisional-transition/2`.

- A local event is refused rather than issued with an id below its
  predecessor when no later event id exists.

- `arc doctor` reports a change whose events fail to reduce as the problem
  `unreplayable-change`, naming the change with the reduction error, and
  exits 1. A change no reader can load was previously passed over without a
  finding.

- Reading the ledger no longer slows with every accepted delivery. Once any
  import carried an admission receipt, every aggregate read (`arc list`,
  `arc show`, `arc status`, `arc doctor`, the hook lookups) digested every
  event of every change once per change, so a ledger with admitted imports
  took tens of seconds to list. The import privilege scope is now derived
  once per read and digests only the events a receipt names; admission,
  ambiguity and refusal outcomes are unchanged.

- Identity detection and the lookups beside it name the cause of a failure
  where they used to drop it. When an unfinished subagent withholds the
  model, `arc env` names each unfinished child by the agent id and role its
  store records, three at most and then a count; no other model stands in
  for it. An ambiguity that rests on a process ancestry `arc env` could not
  read says which variable's ancestry failed and why, and a live Pi
  variable that is set but not valid Unicode is named rather than read as
  unset, leaving no model named. `arc rescue --transcript` reasons are one
  bounded line. A configuration that cannot be loaded now warns on stderr
  where journal auto-log, worktree accounting, and the workspace rollup
  used to carry on without saying so, and a journal-reference refusal says
  when the other projects' journals could not be searched.

- A planner correction that fails validation when read is no longer
  dropped. `arc journal show --json` reads `planner_status: malformed`,
  credits no planner, and names the correction in
  `planner_correction_event`, which also names the correction in force when
  the status is `corrected`. The schema is `arc-journal-artifact/3`.

  A write refused for an incomplete exchange in the global journal names
  `arc journal --global import <bundle>`, and a prepared append in it `arc
  journal --global log recovery`, instead of the project journal's
  commands.

- A recording file named for one session that records another reads as
  unresolved, with both ids named in the reason, instead of as no recording
  for the session. `arc env` reports it, and writes under that identity
  record `session_resolution: unresolved` with the reason. A failed
  lookup's reason also leaves out the store-coverage lines tapes appends,
  so the cause stays in the bounded text.

- Preserve verification falsification references across recorded commit
  rewrites. References retain the failed event's original revision,
  projected references recover only through active rewrite proof, and
  candidate evidence is validated before append.

- Journal promotion cleanup remains recoverable through every
  metadata-removal boundary, including committed relations whose bodies are
  archived. Kind transitions publish complete successor bodies atomically
  and exclude unverified successor prefixes from filed history and session
  handoffs.

- A snapshot at a head whose recorded rewrite invalidated approval creates
  a fresh patchset, even when the previous patchset's mapped head already
  matches. Status and check direct the caller to snapshot; an implicit
  verdict on the stale patchset refuses with snapshot instructions. A
  verdict on the fresh patchset, including through review --snapshot, can
  approve the rewritten head. Earlier verdicts and debt waivers retain
  their review head and stay stale; explicit historical judgments,
  signature-only approval equivalence, retention pins, and rewrite history
  remain intact.

  Rewrites protect target and other branch history while allowing the
  change's own same-name remote refs. A configured upstream qualifies only
  when its remote source branch has the change branch's name, including
  fetch-refspec aliases; target protection takes precedence. Shared-history
  permission never widens the default target..head signing range. Refusal
  advice preserves the default or explicitly selected range, lists its
  affected refs, and names protected target commits in explicit ranges.
  Shared-history checking uses batched reachability. Dry-run checks append
  policy as well as map consistency. Contradictions leave refs and the
  ledger unchanged. Unpublished recording failures restore the current
  attempt's ref moves and retain its intent for retry; resume diagnostics
  distinguish previously applied refs. Published or uncertain events retain
  applied refs and recovery evidence.

- Catchup queries each session backend once, retains readable sessions from
  capped scans, and warns when incomplete coverage can hide sessions inside
  the displayed ranking. Journal filed and catchup count transition
  successors only when a readable body carries the recorded supersedes
  link; pending intents retain writer activity and a diagnostic without
  supplying filings or naming handoffs.

- Recover interrupted memory promotions through the shared staged journal
  transaction, publish complete destination bodies exclusively, verify both
  journals before retiring the intent, and detect or complete torn trailing
  event appends without losing records.

- Events one arc command records sort in the order it wrote them, so a
  change opened with role assertions always replays its opening first.

- Rank recent sessions by recorded activity across complete candidate scans
  before applying the display limit, and retain transition-created
  artifacts, their superseded origins, writing-session filing counts, and
  naming handoffs in filed history and catchup, including imported and
  spool-promoted provenance.

- Keep catchup context readable per journal source, isolate CLI fixtures
  from inherited root overrides, and recover interrupted project-to-global
  memory promotions using one durable destination and event intent.
  Promotion refuses pending exchanges and changed recovery inputs, and
  claimed memories name an accepted release-claim operation.

- Claude model flush tests synchronize on a confirmed model-less lookup and
  verify resolution on retry, independently of filesystem access-time
  policies.

- Refuse every journal write while an exchange transaction is pending,
  including provenance repair and spool promotion, and name the transaction
  and import remedy. Keep journal reads available and refuse recovery that
  would overwrite unexpected body or event-log changes.

- Make cross-repository import tests deliver their cited Git objects
  explicitly and synchronize the Claude model-flush fixture with its first
  recording read, preserving separate coverage of the one-second model wait
  bound.

- Contribution trailers attribute implementation to declared patchset
  contributors or the snapshot subject, preserve declared person identities
  without agent coordinates, and omit unresolved subject coordinates
  instead of borrowing the ceremony recorder’s model. Trailer rewrite
  advice is checked while changes are open, with snapshot and authorization
  refresh and regression coverage for dropping asserted lines.

- Bundle import checks require locally available Git objects without
  fetching from promisor remotes, keeping dry runs free of object writes.
  Standalone export object lists use an arc-bundle-objects/1 envelope
  containing schema and objects fields.

- Journal revision exchange distinguishes individual answer appends,
  including answers to reopened questions that repeat a choice, and
  recovers interrupted imports as a unit of artifact bodies, events, and
  receipt so retries converge after write failures without manual journal
  repair.

- Workspace report and backlog JSON use arc-workspace-report/3 and
  arc-workspace-backlog/19, advancing each schema once from the preceding
  release while retaining their shapes and schema-strict report
  comparisons.

- Claude model resolution waits up to one second, polling every 50
  milliseconds, for a corroborated recording to flush its first
  model-bearing record, so headless tool calls can resolve the acting
  model. Other store answers return immediately, and an unavailable-model
  diagnostic identifies the expired wait. The env guide and help document
  the bounded wait and the evidence limitation that leaves parent and
  subagent tool shells indistinguishable.

- Baseline scratch checkouts resolve relative worktrees directories against
  the invoking checkout root and use one absolute normalized path for
  creation, cleanup and manual commands. Failed creation confirms its
  worktree registration is absent before offering recreation commands,
  preserving unrelated registrations and reporting retained allocations
  with their removal command.

- Failed baseline scratch creation preserves unrelated worktree
  registrations while removing only its own allocation and administrative
  entries. Adoption recovery attaches all option values so titles and other
  values beginning with a hyphen replay successfully.

- Queue integration reads each member’s landing append policy before
  preparation, preventing gate evidence and synthesized commits when an
  earlier landing requires a declared actor. Unresolved queue projection
  review refusals preserve the local or external reviewer’s required next
  decision without suggesting a local approval can clear an external veto.

- Worktree population recovery preserves the intended opening options and
  resolved target and base when adopting retained work. Scratch baseline
  probes reject dirty-tree waivers before recording so they cannot waive
  evidence at an untested head, while baselines executed at their base
  retain revision-bound waivers. Failed scratch checkout creation cleans
  allocations left by initialization hooks; cleanup failures identify
  retained paths and provide removal commands.

- Queue integration checks invoking-store and destination actor
  requirements before replay or gate preparation. Dry runs evaluate
  approval, danger and provenance against resolved projected destination
  declarations and retain unconditional review refusals when destination
  policy remains conditional.

- Worktree creation supplies native Git post-checkout creation arguments.
  Population and hook failures retain the branch and worktree, report
  adoption and discard commands, and remove matching owner records for
  unopened changes.

- Preserve durable journal spooling when optional cleanup ownership cannot
  be recorded, refuse fast-forwards that change initialized submodules
  regardless of Git ignore settings, and require destination actor
  authorization and declaration agreement at the captured transition
  revision.

- Integration reports ref, HEAD, index and checkout recovery state for
  failures after checkout takeover or Git mutation, enforces the fresh
  destination declared-actor policy under locks and during authorization
  confirmation, refuses fast-forwards changing initialized submodules
  before Git mutation, and binds ready-to-send reuse to its patchset,
  effective destination and evaluated revision, contribution history and
  authorization while projecting those coordinates into change state.

- Bind worktree ownership to the Git administrative directory allocated
  before checkout hooks run, and verify the registered checkout retains
  that identity before granting creation authority. Hook replacements
  remain unowned and are kept by cleanup; fork creation verifies the same
  identity before recording its marker.

- Resolve integration destinations through local branch refs despite
  same-named tags, reject known queue history and lifecycle constraints
  before replay or gate preparation, keep later destination policy and
  ancestry checks conditional after divergent planned merges, and remove
  empty outbox metadata only with recorded ownership and successful spool
  promotion.

- Guarded integration binds fast-forwards to the observed destination with
  expected-old ref updates, isolates inherited merge-shaping settings,
  reports checkout rollback and closure publication recovery states,
  applies destination declarations throughout authorization and
  verification, and follows supersession successors through projected queue
  closures.

- Record checkout creation from the worktree creation path and require a
  matching change and ownership token in the checkout’s Git administrative
  directory before integration cleanup removes it, preserving in-place
  operator checkouts and same-branch replacements.

- Validate integration style and history against each guarded authorization
  policy snapshot while retaining explicit flags; resolve queued
  constraints and squash history against projected dry-run targets, include
  style and history refusals in queue summaries, reject malformed
  constraint tables consistently in config and integration, and remove
  promoted change checkouts holding only disposable journal outbox metadata
  while preserving other local content.

- Align default fast-forward readiness across status, check, integration
  and queue plans; evaluate alternative shapes with check --shape, refuse
  locked divergence before declaring debt while retaining expected-basis
  diagnostics, stop queue projection at known blockers, update target
  checkouts with guarded Git fast-forward merges, report recovery state
  after failures, and name the selected destination in rebase guidance.

- Integration cleanup retains replacement checkouts unless their current
  Git registration holds the change branch, and preserves checkouts
  containing tracked edits or any untracked or ignored content, including
  build output, naming the retained paths.

- Integration cleanup preserves adopted, legacy, dirty, untracked,
  out-of-sandbox, and invoking worktrees, retains branches checked out in
  kept worktrees, and reports cleanup failures as warnings without failing
  recorded integrations or stopping queues.

- Accept an explicit ancestor with close --assert-integrated
  --target-before for heads of any parent count, preserving the asserted
  prior target after a fast-forward onto a merge head and refusing
  non-ancestor bases without recording events.

- Close reports a recorded closure successfully when retention ref cleanup
  fails, warning with the remaining pin prefixes after an asserted external
  integration or other closure.

- Probe-only brief edits preserve the preceding contract body, omitted
  title and plan metadata, captured plan provenance, and mandatory reads
  across named probe updates, withdrawals, and JSON probe replacements. A
  probe-only edit requires an existing brief; explicit body or scaffold
  input records a replacement contract.

- Integration warns when retention head or tree ref deletion fails because
  of a ref lock or rejecting Git hook, preserves the recorded integration
  success, and continues queued integrations.

- In-place begin refuses switches that would overwrite ignored local files,
  preserving their contents and leaving the checkout and change unopened
  for both explicit base revisions and adopted branches.

- Explain unavailable model evidence when a session has no recording in its
  configured store, and identify unfinished Claude subagent shell ambiguity
  independently of recording timestamps.

- Preserve container-relative indentation for changelog bodies beginning
  with an indented fence, retain empty list item markers, and keep indented
  code blocks and their internal line breaks intact during Markdown
  rendering.

- Closed change reports identify the shipped patchset as the head-drift
  comparison basis, including when an asserted integration selected an
  earlier snapshot; status retains the literal newest-snapshot comparison
  alongside the explicit comparison patchset and match result.

- Refuse bare artifact filenames held only by another known journal in
  claim, stage, release-claim, watch, and rescue, naming the owning
  journals and projects instead of treating the filename as a change ID.

- Classify every read-only CLI leaf by its complete command path so ledger
  reports, analytics, diffs, resume summaries and read-only subcommands use
  append-refusing handles without loading the invoking checkout append
  policy; require every leaf to have an explicit read-only or writing
  classification and exercise the read-only surface against malformed
  policy.

- The guide lists rebase snapshots among the snapshots that inherit begin
  and brief journal links, rebase help explains those defaults, and inbox
  help names the current schema with a regression check against
  INBOX_SCHEMA.

- Integration reports the landed revision and closure event before
  retention cleanup, and warns about cleanup failures without treating a
  recorded integration or queue member as failed.

- Snapshot verification rejects duplicate and undeclared gate names before
  recording a patchset, leaving ledger events unchanged for invalid
  selections.

- Stamp store format 7 for imported candidate evaluations, selections, and
  promotions even when their registration is absent, using one shared
  candidate event list for typed and raw writes.

- Changelog rendering accepts a tab after a list marker, as in `-\t```` or
  `1.\t~~~`, and keeps every rendered line at its column within the block
  that holds it: an item keeps the width of the whitespace after its
  marker, a paragraph stays in its item, and in a bare body the indentation
  in front of a kept line is written as spaces at its source columns, so a
  tab-indented fence line reads as the same content, closer, or item text
  after the entry gains its bullet.

- `query --commit`, `messages`, and `catchup` read the ledger without
  parsing the invoking checkout's append policy, so a checkout carrying a
  malformed `.arc/policy.toml` no longer fails them. One table now decides
  which commands are read-only: every path such a command reaches gets a
  ledger handle that never parses that policy and refuses any append, and
  the guide prints its list of read-only commands from the same table.

- The undischargeable-probe recovery that `check` prints names each
  contributor as its own `--contributors=` argument, so an identity
  beginning with `-` or holding a space is recorded exactly instead of
  failing to parse after the new brief is recorded; an identity the flag
  cannot spell, one holding a comma or edge whitespace, is left as a
  placeholder the line names rather than split into several contributors.
  The snapshot-only step is printed only when the later brief is one the
  probe can discharge at; a later brief that still has no usable base gets
  the full repair.

- Changelog rendering measures fence and list-item indentation in columns
  with a tab stop every four, so a tab-indented run inside a fenced block
  stays content instead of closing it, and a fence opened right after a
  list marker, such as `- ```` or `1. ~~~`, keeps its lines verbatim
  through its closer instead of being refilled as prose.

- `arc review` and `arc rebase` no longer fail after they have recorded.
  Whether an approval gates, and which gates a replayed head owes, are
  evaluated after the verdict or patchset is written; when that evaluation
  fails, for example because the target's `.arc/policy.toml` does not
  parse, the command prints a warning naming what it recorded and succeeds
  instead of exiting with an error while keeping the patchset, its
  retention ref, and the verdict.

- The recovery `check` prints for an acceptance probe that cannot discharge
  ends with an `arc snapshot` that declares the latest patchset's
  contributors, so it no longer refuses under a live claim held by another
  actor after the new brief version is already recorded. Once a brief newer
  than the patchset's exists, the step is that attributed snapshot alone,
  so a recovery stopped between its two commands resumes instead of
  recording another brief version.

- `show --tag`, `check --tag`, `list`, `query`, `blocker-status`,
  `is-blocked`, and `inbox` read the ledger without parsing the invoking
  checkout's append policy, so a target checkout carrying a malformed
  `.arc/policy.toml` no longer fails them, and a derived handoff keeps its
  change, stage, and claim facts there. Every command that can record still
  refuses a malformed policy.

- `arc journal reattribute` checks ownership before reading the local event
  log: a `<journal-dir>::<file>` reference is refused naming its owning
  journal, a bare filename only another known journal holds is refused
  naming that journal and its project even from a project whose journal has
  recorded nothing, and a filename no journal holds gets the plain
  missing-artifact refusal instead of a failure to read `events.jsonl`.

- `arc review --snapshot` holds the change lock from the patchset it
  records until the verdict on that patchset is recorded, so a command
  acting on the change in between reports the lock busy instead of leaving
  a refused review behind a recorded patchset. `arc done` checks the
  attribution, `--thread`, and every `--journal-ref` before it moves the
  caller's claim to `verifying`, so an input it refuses leaves the claim's
  stage and clock unchanged. `arc rebase`, `arc rewrite`, and `arc
  integrate` advise committing the work in progress or copying it aside on
  a dirty worktree, never `git stash`, whose ref every worktree of a
  repository shares.

- `status`, `show`, `check`, `explain`, `findings`, and `log` no longer
  fail when the invoking checkout carries a malformed `.arc/policy.toml`:
  they open the ledger without parsing the append policy, so a closed
  change reported from its target checkout answers to the declarations its
  closure recorded. Every command that can record still parses that policy
  and refuses a malformed one, and the debt advisory treats an unreadable
  policy as declaring no threshold.

- The built-in `arc changelog --write` renderer keeps every line of a fence
  left open at the end of an entry, trailing whitespace included; measures
  a fence's opening and closing indentation from the block that holds it,
  the entry or a list item's text, as CommonMark does, so a run indented
  four columns into that block stays fenced content; and never starts a
  refilled line with a word Markdown would read as opening a block, such as
  a lone `-`, `1.`, `#`, or `>`.

- `arc journal archive`, `arc journal unarchive`, `arc journal position
  --archived`, and `arc journal reattribute` naming a bare filename that
  neither this project's journal nor its cold archive holds, but another
  known journal's hot or cold storage does, are refused naming each such
  journal and its project, as the other owner-only journal writes are; a
  filename no journal holds keeps the plain refusal.

- The `Next step:` line for an acceptance probe that cannot discharge
  prints a command that runs as printed: a brief version with `--base` at
  the revision the work started from, a `--cause-note`, and the probes
  restated as `--probes-json`, followed by the `arc snapshot` that binds
  the patchset to it. It previously printed a bare `arc brief --body-file`,
  which refused for want of a cause, based the brief on the invoking HEAD,
  and dropped the probes.

- Unresolved findings stay visible wherever arc lists them: `arc findings
  --format sarif` and `arc diff --findings` keep a review or audit finding
  marked still-open, disputed, or contested, and drop one only when its
  applicable history (the dispositions recorded after integration when
  there are any, otherwise its own) has a single resolved, accepted-risk,
  or obsolete tip; `arc review` and its JSON list every finding that
  shipped unresolved, with the status it shipped with and its later
  dispositions.

- A review finding disposed of after its change shipped is reported and
  guarded consistently: `arc findings --format sarif` keeps it while its
  later disposition is still-open, disputed, or contested, and drops it
  only once that disposition is resolved, accepted-risk, or obsolete; `arc
  review --json` carries a standalone finding's later dispositions in its
  open findings; and `arc import` refuses a bundle that records a
  post-integration disposition, or any audit, before its change integrated,
  or a post-integration disposition on a finding that was resolved,
  accepted-risk, or obsolete when the change shipped.

- Refusals that wrote first now refuse before writing: `arc review
  --snapshot` validates the findings batch, its anchors included, and the
  approval restriction before recording a patchset, so a refused review no
  longer records a patchset, a retention ref, or a new attribution that
  stales the standing approval; `arc squash` and `arc rebase` hold the
  change lock from their claim check until the patchset is recorded, so a
  claim attempted while the commit or replay runs reports the lock busy
  instead of refusing the patchset after the branch moved; and `arc done`
  rejects invalid --contributors before moving the claim to `verifying`.

- The built-in changelog renderer keeps fenced code intact: a fence closes
  only on a run of its own character at least as long as the opener, so a
  four-backtick example holding a three-backtick fence survives, and fenced
  lines keep their trailing whitespace. Numbered list items (`1.` or `1)`)
  refill one per number and keep their numbers, while a wrapped line that
  merely begins with a number other than 1 stays in its paragraph.

- A closed change is reported against the declarations it closed under:
  `status`, `show`, and `check` read its target's gates and policy at the
  target revision its closure recorded, plus the gates its shipped head
  adds, so a gate the target declares after integration is no longer listed
  as owed and declarations the target breaks later no longer make the
  report fail. A replay with `--at` to the closure or later judges the head
  the closure recorded, so an asserted integration of an earlier patchset
  is no longer replayed at a newer one.

- `arc begin --no-worktree` on a dirty checkout no longer advises `git
  stash`, which every worktree shares; it names committing or copying the
  work aside before `git checkout`, or `git worktree add` for the branch.
  The stale-target warning under `begin --adopt` names the merge base the
  change took instead of saying it is based on the local target. The guide
  and `arc watch --help` say artifact claims take stages without stage
  budgets, rather than that they have no stages. Journal writes naming a
  bare filename that only another known journal holds are still refused,
  and now name that journal and its project as where the write runs.

- `verify --skip-green` reuses a gate only when the newest evidence that
  applies to the tree is a pass carrying its tree; a newer failure recorded
  against a commit holding that tree, without tree fields, now reruns the
  gate instead of letting an older pass skip it, on both the head path and
  `--against`.

- `arc resolve` disposes of a review finding left open when its change
  shipped: on an integrated change it records a separate post-integration
  disposition beside the finding state the change shipped with, which stays
  as it shipped, and `arc findings`, `arc review`, and `arc show` report
  both. A finding resolved, accepted-risk, or obsolete at integration takes
  none, and the disposition discharges no review debt. The new
  `after_integration` field moves `arc findings --format json` to
  `arc-findings/3` and `arc review --json` to `arc-review/5`.

- The guide no longer says `arc check` exits 6 for malformed state; state
  that fails to load exits 1, and exit 6 names only a closed change, a
  missing branch, or a target branch that cannot be resolved, matching `arc
  check --help`.

- A findings batch now checks each anchor for unknown fields: a near-miss
  of an anchor field the anchor omits, such as `line` for `line_start` or
  `lines` for `line_start` and `line_end`, refuses the batch instead of
  recording an anchor without its line, and any other unknown anchor field
  is ignored with a warning naming the finding.

- `arc rebase` accepts `--contributors` and `--solo` and records the
  replayed head attributed to them. Over another actor's live claim, a
  rebase without either refuses before the branch moves instead of
  replaying and then failing to record the new head; the refusal names `arc
  rebase` among the commands that accept the flags.

- A `--findings-json` batch for `arc review`, `arc audit`, or `arc external
  verdict` is refused when a finding carries an unknown field that looks
  like a misspelling of one it omits, such as `blocker` for `blocking`,
  which would otherwise record a non-blocking finding that an approval
  could carry. Any other unknown field is still accepted but now warned
  about on stderr, naming the field and the fields a finding reads; a
  supplied `id` is ignored silently, since arc assigns finding IDs.

- `arc review --snapshot` and `arc squash` accept `--contributors` and
  `--solo` and record them on the patchset they write, so both work over
  another actor's live claim; the claim refusal names every command that
  takes the flags, and a squash it refuses leaves the branch untouched
  instead of failing after the rewrite.

- `verify --all --skip-green` reuses passing evidence recorded at another
  commit holding the head's tree, as readiness already counts it, so a
  reworded or re-signed commit or a rebase onto an evaluated merge no
  longer reruns gates that `status` and `check` report green.

- A refused `arc integrate --debt` retried with the same reason reuses the
  debt its first attempt recorded instead of appending another identical
  `debt-declared` event, and `arc debt` declaring the undischarged debt in
  force again unchanged prints that debt rather than recording it twice. A
  refusal from `arc integrate` or `arc check` names every blocker on its
  first line, so a reader who keeps one line of it still learns why. A
  value starting with `-` given to an option that takes one (`--evidence
  "--at …"`) is refused with a tip naming the attached form
  `--evidence=--at …`, rather than a similarly spelled option or `-- --at
  …`.

- A closed change is judged at the head its closure recorded, or its newest
  patchset's when the closure recorded none, instead of at its branch and
  target refs: `arc show` and `arc status` no longer report the head as
  moved past the newest patchset, every gate as not green at head, and
  BranchMissing or GatesNotGreen blockers once `arc integrate --cleanup`
  (or `git branch -d` after `arc close --assert-integrated`) deletes the
  branch, or once the target moves past the merge.

- `arc status --json` no longer reports `approval_waived_by_debt` for a
  waived self-approval when an external `changes-requested` or `rejected`
  verdict refuses the head: the waiver is reported as load-bearing only
  while the approval it rescued could stand. Readiness and integration were
  already refused in that state and are unchanged.

- `arc done` accepts `--contributors` and `--solo` and passes them to the
  snapshot it records, so a change another actor holds a live claim on can
  be finished with `arc done --contributors <actors>` or `arc done --solo`.
  The refusal that requires them names `arc snapshot` and `arc done` as the
  commands that accept them.

- The built-in changelog renderer refills each paragraph and list item of a
  recorded entry to 75 columns as one unit, so an entry renders the same
  whatever column its author wrapped it at; authored list markers, nesting,
  and fenced blocks keep their structure.

- An external approval never approves a change touching a dangerous path on
  its own, whatever `forbid_self_approval` says: a local approval must
  stand beside it. A repository that declares no danger paths keeps its
  uniform gate. A debt declared beside an approval that stands without it
  authorizes nothing: the integration basis leaves it out and
  `approval_waived_by_debt` stays false, while the obligation is still owed
  and listed by `arc query --debt`.

- Verification reuse records the answered tree, allowing passing evidence
  from a different revision with identical content to replay. Reuse events
  without a tree retain revision validation. Aggregate change views retain
  healthy rows and report unreadable changes with their errors on stderr;
  direct reads still fail and unreadable prerequisites remain unresolved.

- Scope signing rewrites on tracked change branches to target..head and
  refuse commits reachable from the target or another local branch before
  recreating them, including dry runs. Permit deliberate shared-history
  rewrites with --include-shared.

  Withdraw incorrect rewrite maps by event ID without undoing ref moves.
  Full and delta bundles carry withdrawals, including when the change
  suffix is empty, so importing replicas ignore withdrawn maps and accept
  correct replacements. Generated maps record ref moves for recovery
  output; store format 7 protects readers from ignoring withdrawals.

- Choose the newest gate run for the evaluated tree, current declaration,
  and probe identity so a later run from another environment cannot hide an
  applicable pass.

- The refusal for an empty `--waive-dirty` reason reads as one sentence,
  with a single space where a run of spaces had crept in.

### Performance

- Aggregate ledger reads share repository events across changes, avoiding
  repeated replay while catchup reconstructs project state.

### docs

- The guide and `check --help` state that a change behind its target under
  `ff-only` lists `merged-tree-unevaluated` beside `needs-rebase` and
  exits 11. The guide's orientation and `journal show`, `list`, and `dir`
  help teach reading a known journal artifact (hot, then cold) without a
  filesystem search.

## [2026.9.29] - 2026-09-29

### Added

- A private session link travels with identity. `--session-link`, backed by
  `ARC_SESSION_LINK`, is recorded as `session_link` on every ledger and
  journal event written with it set, and on the records derived from them.
  For a Claude identity, `arc env` derives it from Claude Code's Remote
  Control bridge session id, and clears it when there is none. The link is
  never written into commit or merge messages, trailers, the changelog, or
  forge text.

- `.arc/changelog.toml` can select `renderer = "command"`: a
  `renderer_command` argv, run from the repository root without a shell and
  with the invoking user's authority, receives the projection as an
  `arc-changelog-render-request/1` document on stdin and answers with the
  rendering, or with the complete replacement target for `--write`. It is
  bounded by `renderer_timeout` (default 60s, the process group killed at
  the deadline) and a 16 MiB output cap; any failure to answer exits 1 and
  leaves the target byte-identical.

- Selected journal artifacts move between paired replicas as a versioned
  bundle. `arc journal export FILE... --output FILE` carries each body, the
  events recorded about it, and the artifacts it references by filename;
  `arc journal import FILE [--dry-run]` files a bundle all-or-nothing and
  records a receipt naming the source replica and bundle digest, so
  importing
  it twice changes nothing.

- A [contribution] history policy makes integrate record a change ready to
  send instead of merging it, refusing merge commits and, under squash,
  more than one commit; arc squash makes that single commit as a new
  patchset with its own gates and verdict.

- Operators record decisions made outside arc with arc external verdict:
  the decider, an opaque source reference, and the exact revision. An
  external approval gates only that revision, never alone on a dangerous
  path; a change request carries findings; a rejection of the head closes
  the change.

- Combine repository policy with a local operator file in the Git common
  directory so contributions remain free of operator configuration. Report
  each rule's source and refuse conflicting gate commands.

- Pair independent stores under a logical project identity and move
  integration authority through imported offers; refuse contested live
  claims and keep foreign checkout paths local.

- `arc snapshot` and `arc done` accept `--journal-ref <file>` links to
  journal artifacts that framed the work, recorded as filename plus body
  digest read at record time and refused when the name resolves to no
  artifact, and `--thread <scheme:id>` for an opaque external thread
  reference. `arc show`, `arc log`, and `arc status --json` render them,
  bundles carry them, and `arc journal inventory` names the patchsets that
  cite an artifact. Arc records identifiers only, never transcript text.

- Gate declarations may name an `environment` probe whose stdout identifies
  the environment a gate's evidence applies to. `arc verify` records a
  digest of a successful, non-empty probe run beside the evidence, and
  readiness counts that evidence only where the same probe yields the same
  identity; a receipt from another environment is reported inapplicable,
  naming both identities, and a receipt with no identity is reported
  unknown. A probe that fails, prints nothing, or overruns its bound leaves
  the gate's receipts not-green and names the failure. `arc verify
  --attest` names the identity with `--environment` for a run arc did not
  observe. A gate with no probe takes evidence from any environment.

- `arc instructions git` prints `docs/contribution-trailers.md`, the
  canonical `arc-contribution-trailers/1` specification: `Planned-by`,
  `Implemented-by`, `Reviewed-by`, and `Orchestrated-by` name material
  contributions in agent `harness:model[#effort]` or human `Name <email>`
  form, one line per role and identity, with canonical casing and order,
  retained unknown and legacy `Assisted-by` values, and the rule that
  importing these strings never grants approval or discharges debt. It runs
  outside any repository with no configuration, ledger, journal, identity,
  or network, and embeds the page at build time so there is one copy. `arc
  instructions git --check <file>` distinguishes a malformed role value
  (exit 1) from a key outside the convention and never rewrites the
  message. The `Review-deferred:` marker is deliberately not part of
  version 1.

- Worktree accounting names every open change a path holds, attributes the
  size to the path once, and for a change whose branch is not the one
  checked out there prints the same `git -C <path> checkout <branch>`
  command `arc verify` names, so a stacked series through one checkout is
  reported rather than hidden. `arc doctor`'s usage advice names the same
  owners, and its advice codes and problems are unchanged. `arc-catchup`
  moves to 9.

- `arc inbox` and `arc catchup` report the branches and worktrees no open
  change and no active fork owns: local branches split into unmerged work
  and merged cleanup candidates, and registered checkouts, each with its
  age, distance from the integration target, holding worktree, dirty and
  untracked file counts, and the action that gives it an owner (`arc begin
  <slug> --adopt <branch>`, `arc fork adopt <slug> --branch <branch>`, or
  `git branch -d <merged ref>`). The scan reads refs, `git worktree list`,
  and index stat data only — no tree is walked and no file content is read.
  `arc-inbox` moves to 10 and `arc-catchup` to 8.

- `arc workspace inventory [--storage hot|archived|all]` reconciles every
  registered project's stores: each artifact is a row keyed by project and
  filename, carrying the store it sits in, the resolution the events record
  (absent when none does), the transition successor that superseded it, its
  promotions, and an explanation — `present`, `terminal`, `archived`, or
  `superseded`. Nothing is classified from a missing row alone, so a
  completed item and a shelved one are distinguished and a legacy
  artifact's unrecorded resolution stays unknown. It shares the workspace
  scope and collection manifest, exits 16 on a partial collection, and is
  versioned `arc-workspace-inventory/1`.

- `arc workspace backlog` carries a `collection` manifest: how many
  registered projects were discovered, selected, skipped by scope, observed
  empty, observed with facts, and failed, with one entry per failed
  component (an unreachable anchor, ledger, change observation, journal, or
  fork inventory). A failed component no longer aborts the collection or
  reads as emptiness: the project keeps the facts that were read, names its
  failure, and the command exits 16 to say the report is partial.
  `arc-workspace-backlog` moves to 18.

- `arc workspace backlog` carries every open change per project with the
  predicate buckets it satisfies — held, iterating, changes-requested,
  dependency-blocked, stalled, ready-to-integrate, and unclassified — plus
  the per-kind debt split and outstanding round deferrals, observed from
  each project's own checkout; a project whose only fact is a held change
  or an uncollected deferral stays on the report. `arc workspace inbox`
  takes the same `--under`/`--here`/`--global` scope as the backlog and
  observes each project from its own anchor rather than from recorded heads
  under default policy. `arc-workspace-backlog` moves to 17.

- A plan whose promotions have all closed (integrated or abandoned) reads
  `promotions: closed` in `journal open`, `journal catchup`, and `arc
  catchup` while staying live and listed; an untouched plan and one with
  work still open keep the ordinary row. `integrate` and `close` print the
  `arc journal consume` command for a `journal_ref` plan whose last open
  promotion they just closed, and nothing is consumed automatically.
  `arc-catchup` moves to 7, `arc-journal-catchup` to 8,
  `arc-journal-inventory` to 4, and `arc-workspace-backlog` to 15 for the
  added row state.

- `arc journal reattribute <file>
  --set-actor/--set-harness/--set-session/--set-model [--dry-run]` repairs
  one artifact's recorded creation authorship. It changes only the named
  fields, validates the whole log, retains `events.jsonl.bak`, and
  publishes by fsync and rename. Repair and all event appends share a write
  lock so successful concurrent appends survive replacement. Other event
  kinds and delegated creation records are refused; dry-run reports the
  selected record and other references without changing history.

- `arc journal suggest <file> --question <id> --option <opt> --body-file -`
  records a typed suggestion: the suggesting identity and the reason, shown
  beside the question in `journal questions` and `journal discussion` as
  `suggested: <option> by <identity>`. A suggestion never settles a
  question and never counts toward the stance tally, and an `answer` that
  names a different option warns that it departed from the standing
  suggestions. A suggestion naming an option the question never offered, or
  a question already answered, is refused. `journal questions --json` moves
  to `arc-journal-questions/3` and `journal discussion --json` to
  `journal-discussion/4` for the added `suggestions`.

- `arc journal` read-only commands run from inside the project's journal
  directory: the directory's binding names the project, and the command
  answers exactly as it would from that checkout, `journal dir --explain`
  included. A journal write from there refuses and names the checkout to
  run from, and a journal directory that records no project says so instead
  of listing the resolution sources it tried.

- Detected identity is corroborated against the harness's own session store
  and reported: `arc env` says whether the store holds the session, every
  event records the verdict as `session_resolution` beside `session`, and
  an id no store holds is uncorroborated rather than left to be inferred
  from an absent model. Claude session history resolves under
  `CLAUDE_CONFIG_DIR`, and when several project directories hold the
  session the most recently modified recording answers, with equal
  timestamps falling to path order.

- `arc begin <slug> --from-fork <fork>` promotes fork work onto an ordinary
  change: its own branch from the integration target with the fork's
  commits
  replayed onto it, and a recorded link naming the fork and the source
  base,
  head, and tree. The link grants no review credit, the fork keeps its
  branch,
  worktree, and marker, and `arc resume` lists the artifacts filed under
  the
  fork's topic.

### Changed

- arc builds its session reads on `agent-tapes-core` from tapes' Git
  repository at a pinned revision, so a clean clone builds with no machine
  configuration. The package is not published to crates.io (`publish =
  false`).

- Read harness session recordings through the linked tapes library instead
  of arc's own per-harness store readers and a `tapes` subprocess. `arc
  env` model detection and `arc rescue --transcript` keep their output
  shapes, and a session id resolves by exact match rather than substring.

- `arc export --since <checksum>` ships only the events after a prefix the
  receiving store already holds (`arc-bundle/3`), so a long exchange stops
  re-sending history it has already delivered; a suffix over a different or
  unheld prefix refuses with nothing written.

- Package and CLI metadata describe arc as "Persistent context and guarded
  workflow state over plain Git for agentic coding arcs." `journal open`
  keeps the help for claimed rows, and `workspace list` keeps the healthy
  rows
  when another project cannot be read.

- `arc workspace backlog` ranks projects by the fact `--rank-by` names —
  `blocking` (verdicts owed plus decisions waiting on a person) by default,
  or `availability` (primary work ready to pick up) or `coverage` (review
  obligations owed on shipped work). Every project row carries those three
  as separate fields, so a completed project holding routine coverage debt
  no longer ranks as blocked on a decision, and both the JSON `ordering`
  object and the text report state the basis used. `arc-workspace-backlog`
  moves to 16.

- `journal checkpoint` treats a claim's later checkpoints as replacing its
  earlier ones, so ordinary sequential progress leaves one current
  continuation without a `--supersedes` edge, and it follows `--supersedes`
  across claims, so a takeover converges the tip an earlier identity left.
  `--supersedes` repeats in one checkpoint, converging several ended claims
  at once; the event carries the extra targets as an optional
  `supersedes_checkpoints` list beside the first, which readers that
  predate the field still follow. `journal doctor` reports
  `contested-checkpoint-tip` only when tips remain across claims.
  `arc-journal-inventory` moves to 3, `arc-journal-catchup` to 7, and
  `arc-workspace-backlog` to 14.

- Identity: with no --actor or ARC_ACTOR and a known harness and session,
  events record the actor as <harness>:<session> with actor_source
  "derived" and keep git user.name as "operator". A derived actor is
  assumed: require_declared_actor refuses it and it cannot be the
  independent party to an approval.

### Removed

- Session identity no longer records `child_session`, and `arc env` no
  longer reports it. `CLAUDE_CODE_CHILD_SESSION` is set in the tool shells
  of an ordinary lead Claude Code session, not only a subagent's, so the
  mark named no parentage: it was true for leads and subagents alike. Store
  corroboration (`session_resolution`) and `CLAUDE_CONFIG_DIR` resolution
  are unchanged.

### Fixed

- `arc integrate`, `check`, `verify`, and `status` judge a change by the
  gate and policy declarations committed on its target branch, wherever the
  command is run. A change that deletes or edits a gate in
  `.arc/gates.toml`, or loosens `.arc/policy.toml`, no longer integrates
  under its own rules from its worktree; it still owes the gates its own
  branch adds. `arc status` reports where the checkout, the change, and the
  target disagree (`declaration_notes`, `arc-status/26`).

- An approval now survives a recorded history rewrite only when the
  successor differs from the approved head in nothing but its signature. A
  map naming a different tree, a reworded message, a changed author or a
  commit arc could not compare leaves the approval stale in `arc status`
  and `arc check`. `arc rewrite sign` keeps approvals; `arc rewrite
  trailers` does not. Rewrites recorded before this judgement was stored,
  and bundles from other repositories, verify nothing until re-recorded.

- `arc changelog --write` fills an `[Unreleased]` block that ends the file,
  so a project that has never released gets its first section written. A
  missing target, or one with no `## [Unreleased]` heading, writes nothing,
  names the file and the reason on stderr, and exits 1 instead of printing
  the projection as if it had written.

- Workspace reports record failed departure reads in the collection
  manifest, mark affected departures unobserved, and exit 16.

- Arc requires an exact canonical session ID before attributing a model or
  reading turns, and reports ambiguous or unreadable rescue lookups with a
  cause and reason.

- Keep local review refusals blocking beside external approvals; display
  gate environment probes in policy output; advance empty store formats
  only when required events are recorded; restore tracked state after
  failed or hook-modified squash commits, preserving obstructing hook
  output in a reported recovery directory.

- Require recipient confirmation before replica authority returns to an
  offerer; reclaim requests leave the origin blocked, select the current
  acquired grant despite older forwarded requests, and refuse legacy
  unilateral reclaim events.

- Read Pi's live model and reasoning level while `PI_SESSION_ID` is the
  acting session, and corroborate a session from the live recording file
  outside the configured store. Withhold a Claude session's model while it
  has an unfinished subagent recording, pair a detected model only with the
  session it answers for, and unset every identity field `arc env` cannot
  establish so evaluating it leaves no stale value behind.

- Detect the harness that owns the process by its ancestry rather than by a
  fixed variable order, so a pi or codex run inside another harness's tool
  shell records its own session id. Where several harnesses' variables are
  present and the ancestry names no owner, `arc env` reports the ambiguity
  and leaves harness, session, and model unset.

- `arc done` snapshots and prints the check state when no gate is declared
  for
  the change's profile, saying that no gate was run, and every view names
  the
  missing declaration instead of reporting gates passing.

- `integrate` merges in the change's own checkout when `begin
  --no-worktree`
  left the target on no checkout, checking the target out there and leaving
  the checkout on it as it stood before begin; a dry run says so.

- `integrate` closes a change whose approved head the target already
  contains,
  including a verification-only slice sharing its predecessor's head, at
  the
  target revision that holds it and without fabricating a merge commit; the
  changes behind it integrate normally.

- `integrate` refuses a target checkout only where the merge could change
  or
  lose bytes there: tracked modifications still block, untracked and
  ignored
  paths the merge does not write are left in place and named, and a path
  the
  merge would write that is already untracked or ignored is refused by
  name.

- `arc env` appends the Claude turn's effort to `ARC_MODEL`, and no longer
  reports `<synthetic>` as the model when an API error ends the transcript.

- A ledger whose changes directory cannot be read is an error, never an
  empty ledger: `workspace backlog` names it and exits 16, and `workspace
  inbox` warns and skips that project instead of aborting. An empty,
  never-bound journal at a vanished path counts as empty rather than
  failed.

- `arc journal latest` breaks a tie inside one timestamp second by the
  recorded transition relation — the terminal successor answers — and then
  by the order the event log filed the artifacts; a numeric filename suffix
  is never an ordering of its own. A plan transitioned to `later` and back
  to `plan` within a second now resolves to the successor that carries the
  current metadata instead of the retired original. Hot storage still wins
  over the cold archive and distinct-second ordering is unchanged.

- `arc rescue --transcript` reads the claimed session's latest operator
  turns through both the `tapes` CLI and arc's own readers under one window
  — the newest 4 MiB of the recording file — so `--tail N` counts the same
  turns whichever reader answers. The rendering names the reader that
  supplied the turns and the readers that declined, and a read that stopped
  before the start of the recording states the window it rested on: the
  bytes it skipped, or the `tapes show --full` command that reaches the
  unread text. An empty answer carries a `cause` field naming an unknown
  identity, an absent recording, or text outside the read window, so a
  `count: 0` consumer separates them without parsing prose.

- `arc fork adopt <slug> --branch <branch>` records any local branch as a
  fork
  and keeps the name it carries, and refuses a branch an open change
  records,
  naming the change rather than leaving it unintegrable. A fork's listing
  reports
  when it opened, its head, the uncommitted and untracked counts in its
  checkout,
  and the changes promoted from it; adoption reports the checkout Git
  records for
  the branch, the same one the listing names. The refusal for a change on a
  fork
  branch names the branch and its fork, not the directory the command runs
  in.

- A change whose branch is a fork's is refused by `arc integrate` and `arc
  check`
  from every directory, with a typed `fork-branch` blocker and exit code
  15, and
  `begin` refuses to open one. The fork boundary previously read the
  caller's
  current directory, so a change on a fork branch integrated from the
  project
  root and an ordinary change was refused from inside a fork worktree.

## [2026.9.9] - 2026-09-09

### Added

- Journal inventory exposes one read-only projection of storage, lifecycle,
  questions, claims, and promotions.

- Unresolved discussions can be shelved and restored with explicit cold-
  storage and recovery facts.

- Plans carry portable planner provenance and selected briefs retain
  immutable source snapshots.

### Changed

- Describe Arc as durable project context and change coordination across
  its package metadata, README, and CLI introductions. Include a journal
  example and registry installation instructions for arc-ledger.

### Fixed

- Validate journal storage recovery evidence, serialize planner metadata
  transport, and share inventory facts across journal and workspace views.
  Preserve planner credit and distinguish unknown observations from empty
  results.

- The review JSON contract is versioned as arc-review/3 for both option-
  bearing and option-free reports.

### fix

- Repair review guidance for refusing verdicts and shipped contributors;
  preserve workspace fork visibility, read failures, question subtotals,
  cutoff quoting/precision, and journal-only verification. Keep non-Git
  journal anchors usable and expose the review subject and choices
  consistently in read views.

### provenance

- A lead can now see the exact identities the independence check compares,
  use the explicit contributor path before review, and get the concrete
  blocker when attribution cannot be amended. arc status carries
  review_subject — invoker, effective author, the effective contributor
  set, and whether it was declared or synthesized — derived from the same
  methods the gate calls. arc show and arc check name those identities
  beside an approval rejection. The attribution-amendment refusal names the
  verdict event and its author, or the terminal closure, and the documented
  repair sequence (capture, inspect, amend the full set before any verdict,
  independent review) is in docs/review.md. A run dispatch stays activity,
  not authorship.

### review guidance

- Review guidance now matches the approval state it reports. A clean change
  whose approval requirement is already satisfied by an in-force waiver
  routes to integration: status says integrate beside the flagged waiver,
  check drops the review-queue advisory, and the inbox keeps one lead row.
  For ordinary work outside the independent-review scope, status offers the
  lead an explicit choice — review_options carries declare_debt first,
  request_review second — while required-review and unknown-danger scope
  keeps the conservative review-only path. The list is guidance and writes
  nothing; reading it moves no events. The refusal a waiver does not cover
  keeps the policy's own reason, and a head moving past the covered
  patchset restores request_review.

### workspace backlog

- Workspace backlog selection and provenance are now explicit. Journal
  timestamps read through one parser over the canonical Z stamp and the
  legacy form without it; artifact rows carry filed_at and
  timestamp_status, so a malformed name stays visible with null time
  instead of being dated or dropped. The workspace JSON states its
  selection — normalized since, whether counts mean arrivals or outstanding
  — and counts unknown-time rows separately. Debt rows distinguish a
  recorded kind from the legacy default and the summary names the legacy
  subset. Project rows name their journal_dir, carry every unanswered
  question with its artifact disposition, count waiting decisions into
  priority, and inventory active forks with no obligation weight. The
  report states its observation interval and sequential consistency, and
  writes nothing.

### workspace discovery

- The human workspace backlog report ends with a detail footer naming the
  exact command that re-runs the same selection as itemized JSON — resolved
  scope, normalized --since, and --unreachable, with paths quoted for the
  shell. arc config reports journal_dir_for_cwd from the same resolver arc
  journal dir answers, or null with the diagnostic when no journal anchors
  the directory. The guide names journal dir as the authoritative journal
  location and combines its workspace --items/--json examples into one.

## [2026.9.4] - 2026-09-04

### Added

- `arc rewrite trailers --from <rev> [--drop <key>] [--append <line>]`
  edits the trailer block of every commit message in range and carries the
  result through the same map, ref transaction, intent and record as `arc
  rewrite sign`. A commit is recreated only where the edit changed its
  message or its ancestry moved, so a commit the edit did not reach keeps
  the id it had and stays out of the map. The block is the last paragraph
  when every line in it is a `Key: value` line or an indented continuation,
  never the subject, and messages are edited as bytes so a message that is
  not UTF-8 comes through unchanged.

  `--dry-run` on a rewrite also reports each annotated tag it would
  re-point, or leave alone, in the words the real run uses — the half of a
  rewrite that is a decision about a release rather than a consequence of
  the commits. Planning the ref moves no longer writes tag objects, so a
  preview describes a plan instead of committing to one.

### Changed

- The package on crates.io is `arc-ledger`; the binary it installs, the
  repository, and every command are still `arc`. The bare name belongs to an
  unrelated crate.

- Every independence surface reads an assumed identity the same way. An
  approval is refused when its effective author matches a contributor on
  the patchset or when arc assumed the reviewing identity from git config;
  an assumed *authoring* identity no longer refuses a reviewer that
  declared a different name, so `arc status` before a merge and `arc audit`
  after it agree on who is independent of whom. Reviewer coverage marks
  attribution unknown whenever the covering verdict's identity was assumed,
  which the `reviewer-attribution-unknown` advisory reports. `arc status
  --json` is `arc-status/17`: `review_map[].attribution_unknown` carries
  the wider meaning.

- `arc changelog --write` declines to replace an `[Unreleased]` block that
  holds a line no recorded entry produced, naming each one, and
  `--keep-unrecorded` writes the projection below those lines under an
  `<!-- unrecorded -->` marker instead.
- Integration advises when a change closes with no changelog entry
  recorded, naming the command that records one.

### Fixed

- `arc changelog --write` judges a release block paragraph by paragraph, on
  the words a paragraph holds rather than the column it is wrapped at, so
  an entry the file wraps differently from the projection still reads as
  recorded. The refusal names each unaccounted paragraph, and
  `--keep-unrecorded` keeps one whole.

- A finding carries the provenance of the identity that filed it, so
  reviewer coverage places a reviewer known only from its findings by the
  same rule it places one that cast a verdict: an identity arc assumed from
  `git config user.name` is attribution nobody can place, and it neither
  satisfies the independent-review advisory nor reads as a self-review by
  name. `arc findings --format json` is `arc-findings/2` and `arc review
  --json` is `arc-review/2`, both carrying `actor_source` on a finding.

- `arc doctor` no longer counts the primary checkout among the registered
  worktrees of closed changes. A change opened with `begin --no-worktree`
  records the repository itself, and the advice reads as `git worktree
  remove` on the repository the caller is standing in.

  `arc catchup` under a sandbox leaves out a waiting spool held in a
  worktree the prefix does not admit. `arc journal spool --promote` refuses
  to file from such a worktree, so listing it named a countdown the run
  could not stop.

  `docs/schemas.md` lists the sandbox marker at the version arc writes.
  Every schema constant the crate defines is asserted to appear in that
  page, so a version bumped in code and nowhere else fails the test suite.

- `arc rewrite sign` moves every ref in one Git transaction, the branch
  included, so a rewrite leaves the branch and arc's evidence refs on one
  history or leaves them all alone. It writes the computed map and the ref
  moves to `repository/rewrite-intent.json` before applying any of it,
  records the mapping event once the transaction has committed, and removes
  the intent file once it has — so an interrupted rewrite is finished by
  running it again rather than recreated, which would sign the same commits
  afresh and record a second successor for each. A ref moved by something
  else in the meantime stops it, named; `--dry-run` writes no intent.

  Recorded rewrites that contradict each other make the map unreadable, and
  every projection built on one refuses instead of answering as though no
  rewrite had been recorded — which reported every revision at its
  pre-rewrite value with nothing to say the map had been consulted. `arc
  doctor` reports the contradiction as `invalid-rewrite-mapping`.

  A recorded revision resolves on an exact match, or on an abbreviation of
  seven hex digits or more that exactly one recorded revision answers; an
  ambiguous abbreviation is refused naming the candidates rather than
  resolved to whichever came first.

  A commit is recreated from its own header bytes, so a header order other
  than the common one, and an identity line with whitespace of its own,
  survive a rewrite that claims only to have re-signed it.

- A command run under a sandbox refuses a recorded checkout that lies
  outside the prefix and the repository it was pointed at, so a clone can
  no longer run gates in or rebase the source checkout; `arc sandbox
  discard` validates the marker's prefix and every recorded path before
  removing anything.

## [2026.9.3] - 2026-09-03

### Added

- `ARC_SANDBOX=<prefix>`, or `--sandbox <prefix>` on any command, stands in for
  the home directory wherever arc derives a default path, so one value moves
  every root arc writes: journal, registry, configuration, worktrees, temp, and
  the ledger wherever a `data_root` places it. A variable naming one exact
  directory still means it. `arc sandbox clone <prefix>` copies a project —
  repository, ledger, journal, configuration — so the copy answers the way the
  source does, with no remote back to it and no inherited path;
  `arc sandbox diff <prefix>` reports what its ledger events, journal events,
  and refs differ by; `arc sandbox discard <prefix>` removes a prefix arc
  recorded as a sandbox, and nothing else. `arc doctor` opens with the roots
  the invocation reads and writes, and its JSON report is `arc-doctor/3`.

- `arc rewrite sign [--key <id>] [--from <rev>] [--dry-run] [--no-sign]`
  recreates every commit from `--from` through the branch head so one key
  signs them all, then moves the branch, every `refs/arc/` ref that pins
  evidence, and the local branches and tags whose tips it rewrote, and records
  the map. `--from` defaults to the oldest commit whose signature is missing or
  made by another key. Tree, parents, author, committer, dates, encoding and
  message bytes travel through unchanged, so a commit recreated unsigned has
  the id it started with. It refuses a dirty worktree, a detached HEAD, and an
  ancestry it cannot rebuild, and names each branch left sitting on the
  replaced line together with the `git rebase --onto` that replays it.

- Every derived reading follows recorded revisions forward through the
  recorded rewrites, so a status, check, or queue computed after a rewrite
  answers what it answered before: patchset bases and heads, brief anchors,
  gate evidence and the target a merged tree was evaluated against,
  falsification anchors, dirty-tree waivers, audits, closures with their
  source head, target-before and prerequisites, the forge's observed heads,
  and a journal artifact's `verified` stamp.

- `arc doctor` reports `unresolved-revision` where a recorded rewrite claims a
  revision moved to a commit that is missing too, which previously read as the
  healthiest of the three answers. Its revision scan also covers the
  dirty-tree waiver, merged-tree targets, falsification anchors, integrated
  source heads and target-before, and prerequisite closures.

- `arc claim --takeover --because <reason>` displaces a claim that is not yet
  reclaimable — inside its stage budget on a change, still leased on a journal
  artifact — recording the stated reason on the displaced claim and verifying
  none of it. Without a reason the refusal stands and names the flag and the
  evidence a caller is expected to have (`harness-status-absent`,
  `delegate-exit:<handle>`); `--because` without `--takeover` is refused. The
  reason renders with the displacement in `arc show`, the timeline, the claim
  listings, and `arc status --json`.

- Work on a journal artifact is a claim, the same object the ledger uses for
  a change with the artifact filename as its subject. `claim`, `release-claim`,
  `stage`, `rescue --take`, and `watch --until stalled` accept an artifact
  filename ending in `.md`; both subjects reduce to one claim state and are
  timed by one function. `journal checkpoint <file> --body-file -` appends a
  `### Work checkpoint` block and records its digest with `--next`, `--gate`,
  `--blocker`, and `--supersedes`. `journal consume` and `journal transition`
  refuse while a claim is open and take `--acknowledge-claim <id>`; the claims
  they end carry `ended_by` and no invented owner outcome, while `begin
  --from-journal` closes the invoker's own as `promoted`. `journal open` rows
  and `--json` show who holds an artifact and whether it can be picked up.

- `arc run dispatch` binds a delegated run to one subject — `--change <id>`,
  `--fork <slug>`, or `--range <base>..<head>` — and refuses a dispatch naming
  none or two. `arc run end` records what the round reviewed and produced:
  `--reviewed-head <sha>`, `--raised-json <path|->`, `--deferred-json
  <path|->`, and repeatable `--collects <deferral-id>`. A deferral requires a
  `why` and is minted `def-<ulid>` when it names no id, and stays open until a
  later round on the same subject collects it. `run list` groups by subject
  and numbers the rounds, showing each round's reviewed head, raised count,
  and deferrals still open. `arc inbox` and `arc catchup` gain a `deferred`
  section naming each open deferral's subject, round, reason, and age
  (`arc-inbox/8`, `arc-catchup/4`), since a deferral absent from the answer to
  "what is waiting" is a deferral nobody will honor.

- Every journal kind verb and `arc journal log` accept `--spool`, and a write
  spools on its own when the journal directory cannot be created or written.
  The journal lives outside the repository, so an executor sandboxed to the
  repository it is editing could not record what it deferred; the write now
  lands in `<repo toplevel>/.arc/outbox/` as an `arc-journal-spool/1` file
  instead of failing. `arc journal spool` lists what is waiting and
  `--promote` files each into the journal oldest first, keeping the spooled
  identity verbatim and naming the promoting caller in a new optional
  `promoted_by` journal-event field; an unparseable spool file is reported and
  kept. `arc config --check-writable` reports whether writes reach the journal
  or will spool.

- `arc journal delivered <file> --question <id> --to
  person|anyone|delegate:<name> [--handle <opaque>]` records that a caller
  delivered an open question to somebody; arc keeps the fact and sends
  nothing. A question recorded who may settle it but never whether anybody
  was asked, so the queue could not separate prompting work still to do from
  an answer merely pending. Every question view now carries `delivery`
  (`unknown|unasked|delivered|answered`) and the full `deliveries` list, and
  the text listing leads each question with its state. Deliveries accumulate:
  asking twice records twice and erases nothing. A delivery is refused when
  the question is unknown, already answered, or waiting on a named delegate
  and addressed to anyone else. A typed `capability` event marks where the
  journal began recording a facility, appended once by the first write that
  can record it — posing as much as delivering — so a question posed by this
  build reads `unasked` at once and only one older than the record reads
  `unknown`; `journal doctor` validates both events. `journal questions
  --json` becomes `arc-journal-questions/2` and carries the marker.

- Gate evidence binds to the tree a command ran against rather than to the
  commit carrying it, so a change that is behind its target cannot ship
  content nothing has evaluated. `check` refuses with
  `merged-tree-unevaluated` (exit 14) and names `arc verify --against
  <branch>`, which synthesizes that merge, runs every required gate in a
  scratch checkout it removes whatever the gates do, and records the result at
  the merged tree beside the target head it was computed from. The evidence is
  spent when the target moves again; a head already on the target tip needs
  nothing new; `needs-rebase` still means a textual conflict alone. `integrate`
  checks the merge it made carries the evaluated tree and undoes it otherwise,
  and still never runs a gate. `arc-status/15` adds `merged_tree` to the report
  and `evaluated_tree` to each gate, both shown by `show` and `check --explain`.
- Every `arc journal` write verb and `arc journal log` accept `--source
  'harness=<h> session=<id> ts=<rfc3339>'` and `--item-key <key>`, so an item
  distilled out of a recorded session carries where it came from. `turn`,
  `schema`, and `coverage` are recorded only when an emitter supplies them; no
  transcript text, credential, or digest enters the journal. A write whose
  recording and key were already recorded writes nothing and prints the
  existing entry with its disposition — `open`, `no-action`, or
  `consumed:<outcome>` — so rescanning the same endings neither duplicates an
  item nor re-reads one already judged. `--outcome no-action` on `journal log`
  records that an item needs no work at all, answering a later scan without
  putting an artifact in any queue. `arc journal source --harness <h>
  --session <id> [--item-key <k>] [--json]` lists what one recording produced
  here, versioned `journal-source/1`; `arc journal source-attach <file>
  --source '<spec>'` records a further recording as evidence behind an
  existing artifact. `journal show` prints an artifact's sources on the
  diagnostic stream, leaving stdout the body verbatim, and `journal open
  --json` carries them as `sources`.

- `arc journal handoff <topic> --derive` prepends a `## State (derived at
  <ts>)` section read from the repository: repository and worktree paths,
  branch and short head, the change's target with commits ahead and behind,
  clean or dirty, the inferred change with its stage and claim standing, open
  queue depth, and the installed build beside the repository head. Facts that
  cannot be read render as `unknown` rather than being omitted. `--derive` is
  refused outside a Git repository, and refuses before the body is read so
  `--body-file -` never waits on stdin. Writing that half by hand is most of
  what made a handoff expensive enough to skip; the guide now also names the
  four moments at which a session journals rather than leaving it to the end.
- `arc journal correct <file> --target <t> --field <f> --value <v>` and
  `arc journal retract <file> --target <t> --body-file -` amend a recorded
  entry without rewriting it. A wrong entry could only be answered by another
  entry arguing with it, and a consumed artifact could not be corrected at
  all; both operations stay open on one. The block stays where it was filed
  and the effect lives in the derived views: the stance tally, the branches
  argued under a question, and the participant identities read the corrected
  value, `journal open` and `catchup` read the corrected title, a retracted
  position leaves the tally, and a retracted answer reopens its question.
  `journal doctor` reports an amendment naming an entry the artifact never
  recorded. Open-queue rows carry `[amended ×N]` in text and `amendments: N`
  in JSON for the positions standing on a filed claim.

- `arc verify --falsified-by <event-id> --predicted <reason>` lets passing
  evidence name the failing run it answers, so a gate watched to fail and then
  fixed stops reading like one that has only ever passed. The reference must be
  a failing verification of the same gate or command on the same change; its
  revision is read from that event. Gate rows gain `discrimination`
  (`discriminating` or `undiscriminated`) plus the reference and, where they
  differ, `discrimination_event_id` beside `evidence_event_id`; passing gate
  lines say which they are. Discrimination is derived over every passing
  record for that gate at the counted revision, so rerunning a gate does not
  retract what an earlier run established against the same tree. Advisory:
  readiness, gate results, and exit codes are unchanged. Status JSON is
  `arc-status/14`.
- A debt records what kind of deficit it is, what review the work did have, and
  who produced it. `DebtMissing` gains `nothing-read`,
  `merge-resolution-unread`, `repair-unread` and `contributor-only`; arc
  derives the kind from the ledger and `--kind <k>` on `arc debt` and
  `arc integrate --debt` declares one instead. Coverage carries the reviewer's
  effort and the routing version `arc review --route-version` and
  `arc audit --route-version` record, and production names who recorded the
  brief version the shipped work answered and who recorded the patchset.
  `arc query --debt` orders by kind then age, and the inbox, catchup and
  workspace summary rows split their count by kind. JSON is `arc-inbox/7` and
  `arc-workspace-backlog/11`.
- Journal events record the `--on-behalf-of` subject beside the invoking
  actor, so a note, position, question, answer, verification, lane, or
  consumption a lead files for an executor no longer reads as the lead's own.
  `journal events` emits `on_behalf_of`; `discussion --json` carries the actor
  and the subject on each position and answer, `questions --json` on each
  question, and the verification stamps in `journal open` and `catchup` on each
  row. Prose headings are unchanged: the subject is never substituted for the
  identity that argued. `discussion --json` is `journal-discussion/2`: each
  round position is an object carrying its id and that identity pair, where it
  was a bare id string, and an answer is an object carrying its option and the
  same pair.
- `arc catchup` and `arc doctor` measure fork checkouts beside the open
  changes' worktrees, under their own key and total. A fork sits outside the
  change lifecycle, so nothing retires its worktree and it is routinely the
  longest-lived checkout on disk; fork bytes are never summed into the change
  total, and a retired fork whose worktree was kept stays in the accounting
  after it leaves the live-fork listing. Every total names the method that
  produced it: sizes are `du` apparent size, physical cost is reported as
  unknown with the reason named where the filesystem gives one, and the
  worktrees root's mount is reported with its free space. An absent `findmnt`
  leaves the filesystem type unknown instead of guessed.

- `arc begin` and `arc fork begin` print what the worktrees root has left
  before creating a worktree, and warn when the mount is close to full.
  Advice, never a refusal — a filesystem that fills reports as success
  everywhere else.

- `arc fork thread <slug>` prints the harness, session, model, and actor the
  fork's marker recorded, with the command that reopens that session where
  the harness has a stable resume form. Fields the marker does not carry
  print as absent.
- `arc workspace backlog --here|--under <path>` gives a non-Git workspace root
  a first command: it filters the registered projects by canonical anchor,
  keeps missing anchors inside that path visible, and states the active scope
  in text and JSON. `--global` names the existing all-project view explicitly;
  JSON is `arc-workspace-backlog/8`.

- `arc fork` — fork this repository and start working, as one command
  instead of a hand-assembled worktree, branch, and unspoken contract.
  `begin` creates a `fork/<slug>` worktree outside the change lifecycle
  and journals its marker; `adopt` journals a hand-made fork rather than
  leaving it invisible; `retire <slug> <outcome>` records the disposition
  (merged, dropped, kept), removes the worktree, and keeps the branch;
  `list` reports every fork from markers and branches together, with the
  ahead-count over its base. `catchup` lists open forks beside the
  ledger queue, and `integrate` refuses inside a fork worktree with the
  way out named: a fork is unintegrated by intent, the operator decides
  what to merge, rebase, or discard, and a fork is a fork until the
  operator says otherwise.

- A question carries who may settle it: `--settle-by anyone` opens it to
  any session, `--settle-by delegate --delegate <name>` records the
  session or actor delegated the call, and the absence of the flag stays
  the classic default — a person. The prose claimed no model may ever
  answer, while nothing in the mechanism enforced it and delegating the
  call was the ordinary way it got settled; the field records what
  actually decides. `journal questions` (text and JSON) and the catchup
  orientation carry the fact, so an agent knows whether it may answer or
  must prompt, and a delegate knows the question waits on it.

- The discussion view reads what the write allows. `journal position`
  accepts any live artifact — only branch positions need a discussion —
  but `journal discussion` refused to open the very files positions were
  landing on, leaving a recorded, attributed stance with no reader.
  Positions, participants, and rounds now render for any artifact;
  question and resolution sections stay empty where the write side never
  created them.

- `arc journal transition <file> --to <kind>` changes a live artifact's <file> --to <kind>` changes a live artifact's
  workflow kind as one guarded operation instead of a hand-composed
  sequence: a typed successor is written carrying a machine-readable
  `supersedes` link (and the inherited body, unless `--body-file` says
  otherwise), a `transition` event records source, successor, and
  provenance, and the source is consumed as superseded. Closed,
  consumed, same-kind, and same-second-collision sources are refused;
  `--dry-run` prints the successor and every lifecycle effect without
  writing; `journal doctor` treats the new event as known state. The
  transition matrix is deliberately narrow — a decision is how a
  discussion ends, not what it becomes, and promotion to a code change
  remains `begin --from-journal`.

### Fixed

- `arc fork begin` standing on a fork takes its base from the branch the
  repository presents as its integration branch, and refuses naming
  `--from <branch>` when none is discoverable, so a repository whose
  integration branch carries any other name never has an unrelated or
  missing branch chosen for it.

- The refusal for `arc rescue --transcript` on a journal artifact reads as
  one sentence instead of carrying a gap of literal spaces.

- `arc query` filters only on flags the caller typed. `--actor` and
  `--harness` name the acting identity on every command and a filter on this
  one, so the identity exported for recording was narrowing the read: each
  harness saw only the changes it had opened, and `--debt` hid the
  obligations owed elsewhere.

- A journal lane's owner is the harness and the session together, not the
  session alone. Two harnesses can mint the same session string, and either
  could previously refresh, replace, renew, close, or be annotated as
  `this session` on the other's lane; lane writes now require a declared
  harness as well as a session. Stale takeover is unchanged.
- `arc env` recognizes `CLAUDE_CODE_SESSION_ID`, the variable Claude Code
  actually exports into its tool shells, beside the hand-set
  `CLAUDE_SESSION_ID`. A Claude Code session that followed the guide's
  first step got the manual template and no identity; the ledger shows the
  result as Claude sessions naming themselves four different ways. The test
  helper also strips every harness session variable, so the suite detects
  its fixtures rather than whichever harness happens to run it.

### Changed

- Every journal artifact opens on a heading. A kind write with no `--title`
  whose body opens on prose is headed from its topic slug, announced on the
  diagnostic stream so stdout stays the artifact path; a body that already
  opens on a heading and an explicit `--title` are untouched. A queue row
  reads its description from that heading, and a row naming nothing is one
  nobody picks up.

- `arc journal verified` run inside a fork worktree stamps the fork's own
  head and records `verified_scope: fork:<slug>`, and the open-queue row
  reads `[verified at <rev> in fork <slug> ...]`. A fork carries commits the
  anchor does not, so the previous anchor stamp credited the check to source
  nobody had opened. A fork-scoped stamp claims no anchor comparison, since a
  revision off the anchor's line of history cannot be compared with it.
  Outside a fork nothing changes.

- Change, patchset, debt, and workspace views retain the model, harness, and
  native session already carried by their source events. `show --json` exposes
  opening identity, review rows name the latest patchset's recording identity
  and represented subject, and debt rows name the obligation identity; absent
  provenance stays null rather than being inferred. Workspace JSON is
  `arc-workspace-backlog/10`.

- `arc workspace backlog` opens with aggregate project, ledger, journal, and
  unreachable totals instead of making each reader reduce the rows. Temporary
  and scratch orphan journals collapse into one maintenance count by default;
  `--unreachable` expands them all, durable missing anchors stay named, and
  JSON keeps the complete list plus `summary` in `arc-workspace-backlog/9`.

- Fork identity is classified under the same independent-review boundary as
  fork creation and retirement. The resolver decides which recorded fork a
  lifecycle operation addresses, so it carries the authority of its callers.

- `arc workspace backlog` separates integration staleness from direct path
  overlap: review entries carry both target commit distance and paths changed
  on both sides. Failed target and debt-range probes remain explicit `unknown`
  values instead of becoming zero or an empty set. JSON is
  `arc-workspace-backlog/7`.

- The review-obligation surface is named `debt` end to end: the
  `--debt` flag on `integrate` and `query`, the `arc debt` subcommand,
  the `debt` status fields, the `debt-outstanding`/`debt-summary`/
  `debt-touched` advisory codes, the `debt-owed` inbox bucket, and the
  Rust types. The tool stops using two words for one thing. Wire formats
  that moved carry schema bumps (`arc-status/13`, `arc-chain/4`,
  `arc-inbox/6`); the untyped `audit-debt-declared` event type is what
  earlier builds wrote and keeps its serialized name forever, so
  nothing already recorded is lost.

### Added

- Consuming a discussion reports the two facts `arc journal discussion`
  renders that say a decision was never tested: every position came from
  one participant, and nobody answered the last position. Warnings rather
  than refusals — one voice is a legitimate way to settle something — and
  `--outcome discarded` earns them as much as `done`. Both facts are
  derived once and shared with the view, so the two cannot disagree.

- A verdict says what it does to the verdicts already standing: `arc review
  --relation supersedes` (the default) replaces the tips it observed, and
  `--relation corroborates` supports one without becoming a second
  authority — which is what discharging a provisional approval is.
  `latest_verdict` is now the sole tip of that graph rather than whichever
  verdict arrived last, so event order no longer decides authority; two
  verdicts replacing one tip leave the change contested, `arc check` blocks
  and names which forked, and `verdict_contested` is carried on the status
  and review views. A changelog entry likewise names the entry it replaces,
  and `arc changelog` says so instead of printing the same message twice.

- `arc journal position --stance <for|against|amend>` writes the stance
  line above the body and records the stance on the typed `position` event,
  so a tally cannot undercount because an author opened with a heading. A
  hand-written stance line still works, and passing both is refused before
  anything is written. The `discussion` scaffold teaches the flag, and the
  undercount message names it.

- `arc journal verified <file> [--note <text>]` records that an open
  artifact was checked against source at the project anchor's head, as a
  typed event in the journal's log rather than in the prose a person edits.
  `journal open` and `workspace backlog --items` name the checked revision,
  its age, and who checked it, and speak up only once the anchor has moved
  past the stamp. JSON moves to `arc-workspace-backlog/3`.

- `arc watch --until` accepts `approved`, `gates-green`, `blocked`, and
  `brief-recorded`. `approved` returns on the latest approving verdict
  against the latest patchset — including a provisional approval, whose
  reason it carries into both renderings, because a provisional approval
  gates and a watcher refusing to return on one would wait for a state the
  change may never reach. `gates-green` is every required gate at the
  change head, which is when a lead can start reviewing rather than when a
  reviewer is done.

- `arc resolve --evidence-event <id>` cites the verification event that
  justifies a disposition, beside the free-text `--evidence` rather than
  instead of it. The id must name a `verification-recorded` or
  `verification-reused` event on the same change; an id naming something
  else and an id naming nothing are separate refusals. Carried on
  `DispositionRecorded` and `AuditDispositionRecorded`, and named in the
  timeline, the finding markdown, and JSON.

- `arc rescue --transcript` reads a session through the `tapes` CLI when it
  is installed, falling back to arc's own claude/codex/pi readers
  otherwise, and names which reader answered (`arc-rescue/2`). This is what
  covers OpenCode sessions, and arc keeps working where `tapes` is not
  installed.

- `arc journal incident <topic>` records something that went wrong in the
  running of work rather than in the code, so a decision whose revisit
  trigger reads "when X happens" can fire on evidence. It is a record
  rather than work and never enters an actionable tier.

- `arc journal consume --decision <project>::<artifact>.md` cites a
  decision or conclusion in another registered project's journal, recording
  that project, the artifact's kind, and a digest of its bytes. A
  conclusion now resolves any non-discussion actionable artifact; a
  discussion still requires a decision.

- A change can declare that integration is not the goal: `arc begin
  --iterating` and `arc iterating <change> [--off]` record it in the
  ledger. `arc check` then reports a typed `iterating` blocker (exit 13)
  instead of a missing approval, `arc integrate` refuses and names the way
  out, and `arc inbox` buckets it separately. Schemas move to
  `arc-status/12` and `arc-inbox/5`.

- `arc workspace backlog --items` names every actionable artifact behind
  the counts — identity, kind, tier and per-item age — so a cross-project
  sweep is answerable in one invocation. JSON is `arc-workspace-backlog/2`.

- Brief readers now say when a brief's base revision no longer matches the
  change head: behind by N commits, or detached when the base is not in the
  change's history. Surfaced by `arc status`, `arc resume`, `arc rescue`
  and `arc brief`, and carried in the report as `brief.base_drift`
  (`arc-status/11`).

- `arc watch --until reviewed` returns when any verdict is recorded against
  the patchset under review, whatever it concluded, naming the verdict
  event. `ready` could not express this: a review asking for changes never
  satisfies it, so a wait on a dispatched review ran to timeout with "still
  working", "changes requested", and "reviewer died" indistinguishable.

- `arc verify --waive-dirty "<reason>"` lets evidence from a dirty worktree
  satisfy a gate, bound to the revision it was declared at so the next
  commit ends it, and reported by `arc status` only while it is in force.
  Recorded evidence now separates tracked from untracked dirt
  (`arc-status/10`), so the waiver reason is self-evident and the frequency
  of untracked-only wedges is measurable.

- `.arc/policy.toml` accepts `danger.source_roots` and
  `danger.acknowledged_safe`, closing danger classification over declared
  roots: `arc doctor` fails on a tracked file inside a root that is
  classified neither dangerous nor acknowledged-safe, and on one classified
  as both. Projects that declare no source root keep the open-world list
  they had.

- `arc keep --kind verified|rejected|constraint|hypothesis` records a fact
  the work discovered, and `arc resume` hands it back under `## Kept
  Context`, so a compacted or cold session does not re-derive it or re-try
  an approach already rejected. Carried on `arc status --json` as `kept`.

- `arc journal scaffolds [--show <name>] [--json]` lists every scaffold a
  write can prepend, marks which kinds default to one, and prints a
  scaffold's body before an append-only artifact commits to it; a
  repository template is shown as shadowing a built-in of the same name.

- Each journal artifact kind has its own subcommand (`arc journal todo`,
  `arc journal handoff`, `arc journal decision`, …), making `arc journal
  --help` the kind registry; `note --kind` stays canonical underneath and
  now defaults to `note`.

- `arc journal questions [--json]` lists every question waiting on a
  person, with its options and argued branches; `arc catchup` reports them
  first. `arc journal answer --other "<answer>"` settles a question outside
  the options it offered.

- `arc review --verdict approved --provisional "<why>"` records a verdict
  that gates but is owed corroboration. Independence and approval staleness
  are unchanged; the change carries a recorded obligation until somebody
  else supplies a second judgment — an independent approval of the same
  patchset, or an audit — and neither the reviewer being corroborated nor
  the change's author can supply it. `arc query --provisional` lists the
  outstanding ones, `arc status` reports
  `provisional_approval_outstanding`, `arc check` names the reason, and the
  merge's authorization basis records that it rested on a provisional
  verdict.

- `arc journal latest <topic> [--kind <kind>] [--json]` resolves the newest
  artifact filed under one topic, hot storage before the cold archive.

- `arc fr <topic>` files a feature request in the project journal — the
  top-level alias for `arc journal note --kind feature-request`. Alongside
  it, `arc log` accepts `--oneline` and points at `git log` instead of
  failing argument parsing, and a change that cannot be inferred now names
  the open changes and their worktrees.

- `journal question`, `journal answer`, and `position --question/--option`:
  a discussion can hold a question only a person settles, answered
  `opening` before anyone argues or `closing` once the argument is in, with
  positions argued under each option so a closing choice is made between
  explored branches rather than labels.

- Bare `arc` now teaches the discussion path — `journal note --kind
  discussion`, `journal position`, `journal discussion`, `journal consume
  --decision`, and `begin --from-journal` — beside the change lifecycle it
  already taught.

- `arc history rewrite` records a Git history rewrite as a
  repository-scoped fact carrying the operator's commit map, and `arc
  history resolve` follows a recorded revision forward. `doctor` reports
  moved revisions as `revision-rewritten` rather than dangling, and `diff`
  renders the surviving commit. Events are never migrated.

- A guarded merge records the authorization basis it was taken on: the
  approving verdict, per-gate evidence, prerequisite closures, the empty
  blocking-finding and hold vectors, and the normalized gate and policy
  values actually consumed. `arc integrate --dry-run` prints it without
  writing.

- `arc diff <change> --integrated` renders the exact range an integration
  recorded — where the target stood before, to the commit that landed —
  which is the range an audit reviews. Closures with no recorded range take
  an explicit `--base`.

- `arc workspace` no longer requires a configured `data_root`. Without one,
  projects are enumerated from the journal root — each journal records the
  project it belongs to — so `list` and `inbox` work under the default
  per-repository layout, and `arc begin` registers a project so opening a
  change is enough to make it discoverable.

  `arc workspace backlog` reports ledger and journal backlog across every
  known project: changes awaiting a verdict, audit debt outstanding, the
  journal's three tiers, and the oldest open item. Projects rank by what is
  blocked; items are never ranked across projects. A journal whose project
  can no longer be found is reported with the `journal rebind` that adopts
  it, rather than skipped. `--since` turns it into a delta of arrivals,
  with blocked work still reported in full.

- Locally executed verifications record the worktree tree they ran against
  and whether it was dirty; dirty or mid-run-changed evidence is displayed
  but never counts as green; and a patchset re-snapshotted at the same head
  is a rebind rather than a rework round.

- `arc doctor` reports recorded revisions Git can no longer resolve, `arc
  watch --json` names the change, condition and satisfying event (and
  reports a timeout as JSON), and `arc events --tag` follows a tagged
  program as one stream.

- Events, patchsets and verdicts record `actor_source` (`flag`, `env`, or
  `git-fallback`); a fallback identity is announced on stderr before it is
  recorded; `forbid_self_approval` fails closed when either side's identity
  was never declared; and an opt-in `[policy] require_declared_actor`
  refuses to append an event whose author nobody claimed.

- `arc stats --by-model` reports one row per delegated identity — changes
  touched, patchsets contributed, rework rounds opened, verdicts issued —
  keyed on the `--on-behalf-of` subject, with unattributed work in its own
  row.

- A journal records the project it belongs to in `bindings.jsonl`; `arc
  journal doctor` reports an orphaned or split journal; and `arc journal
  rebind <dir>` adopts an orphan, recording the move and refusing a target
  that already holds content.

- `arc brief` warns when an acceptance probe runs a gate the change must
  pass; `arc check` and `arc status` name a probe that cannot discharge
  because its brief has no usable base; `--probes-json` accepts inline
  JSON; `arc verify --gate` points at a declared probe of the same name;
  and `--change` works wherever the change positional is optional.

- Finding bodies and anchors now reach every read surface: `arc findings`
  prints them, `arc status --json` carries body, anchor, patchset and
  reporter, SARIF carries the body as its message, `arc log` renders
  findings filed inside a review batch, and blocking findings name the
  patchset they predate.

- `arc journal note --kind discussion` prepends the discussion scaffold by
  default (`--no-scaffold` opts out), and `arc journal discussion` reports
  position blocks that state no stance as `unstated` instead of silently
  undercounting the tally.

- The audit-debt waiver now binds to the patchset it was declared for, so a
  single declaration no longer excuses self-approval for the rest of a
  change's life. Outstanding obligations surface in `arc inbox` (new
  `audit-owed` bucket, `arc-inbox/3`), `arc catchup`, `arc doctor`
  (`audit-debt-outstanding`), and `arc chain` (`arc-chain/2`, with
  per-reviewer coverage of the final patchset). `arc findings --audit`
  lists post-integration findings. An approving audit is refused from the
  identity that authored the work.

- Review coverage (`review_map`) reports which patchset each reviewer
  identity actually saw, with advisory warnings in `arc check` and `arc
  show` when nobody independent covers the final patchset. New `arc audit`
  records a post-integration review against the integrated revision as a
  distinct event, and `arc integrate --audit-debt`/`arc audit-debt` record
  a review a change owes so it can ship without an independent verdict;
  `arc query --audit-debt` lists outstanding obligations. `arc review` now
  reports an approval that cannot gate instead of silently recording it.
  Status schema is `arc-status/6`.

- `arc catchup` orients a session with ledger buckets, journal backlog,
  lanes, and memories in one call. `arc inbox` now names the journal
  backlog beside its ledger buckets (`arc-inbox/2`). Bare `arc` prints a
  workflow guide covering the lifecycle, profile selection, and the
  invariants that change what a session does. `arc journal open` explains
  its three tiers.

- `arc watch --tag <tag>` watches a whole series, returning on the first
  member to reach a condition (`--any`, which names it) or once every
  member has (`--all`) (`watch-tag-scope`).

- `arc config --check-writable` also probes commit capability in a
  throwaway repository, signing the probe only when the repository resolves
  `commit.gpgsign` to true and ignoring a global policy the repository
  overrides; failure details carry the underlying cause
  (`preflight-commit`).

- A brief version records why it exists: `--caused-by
  finding:<id>|verdict:<event>|blocked-on:<event>` and `--cause-note`,
  required from v2 and validated against the referenced object's kind
  (`brief-causes`).

- Require blocked-on stages to identify a canonical brief, finding, change,
  or external referent.

- Derive completed rework rounds and first-pass approvals from patchset and
  verdict history.

- Require typed root causes for requested-rework verdicts and tally them in
  review statistics.

- Declare brief-bound acceptance probes and record typed baseline and final
  evidence.

- Correlate multi-gate verification with manifests, terminal results, and
  explicit evidence reuse.

- Bind snapshots to the exact brief version they implement.

- Anchor each brief version to the full revision it was checked against.

- Configure the built-in changelog target with contained writes and safe
  stdout fallback.

- Preserve complete changelog recording provenance in JSON and optional
  human projections.

- Resolve journals from explicit path-prefix anchors and explain the
  selected source.

- Report registered worktrees that belong to closed changes without
  removing them.

- Add `arc chain <tag>` text and JSON views that show dependency-ordered
  members, plan history, and the next ready change.

- Link briefs to optional plan artifacts and slices, preserving the
  association across status output and bundle round-trips.

- Render discussion positions grouped into rounds by reply depth, name the
  participants of each round, and flag positions nothing has answered.
  Derived entirely from existing `position_id`/`ref` data — no new event,
  field, or schema version.

- Add terminal `decision` journal artifacts and let resolved discussions
  link to them with `journal consume --outcome done --decision <filename>`.

- Add read-only `arc metadata <change>` text and `arc-metadata/1` JSON
  projections, including for closed changes.

- Add a read-only `arc review <change>` view with verdict history, review
  findings, approval freshness, next action, and versioned JSON output.

- Add opt-in `arc rescue --transcript [--tail N]` output for bounded
  recovery of the claimed session's latest operator turns.

- Add `arc rescue` to report abandoned foreign-session work and safely take
  over stale claims.

- `arc changelog` records change-scoped release copy in the ledger and
  projects
  integrated entries into the generated `[Unreleased]` block, eliminating
  cross-change changelog conflicts (`changelog-projection`).

### Changed

- `arc begin --no-worktree` means in place, not nowhere: a clean checkout
  already on the requested target is checked out onto the new branch and
  recorded as the change's worktree, so the next command infers the change
  without being told. A dirty checkout, or one standing on another branch,
  is left exactly as it was — the change still opens, and arc prints the
  Git command that finishes the switch, naming which of the two situations
  it found.

- The `reviewer` scaffold says that a change with a runtime surface is
  reviewed by running it, and that a diff review of one is not a review. It
  names what a diff cannot show — wording, ordering, filtering, the sign of
  a computed signal, and the volume of what is printed — and asks for both
  the human and `--json` renderings against a real ledger.

- Bare `arc` teaches identity before the first write (`SAY WHO YOU ARE`)
  and what to file when a session ends (`END A SESSION`); `arc env --help`
  names the session variables it detects and explains that its non-zero
  exit reports identity must be set by hand.

- `arc verify` refuses to run a gate or a final acceptance probe away from
  the change's head, where the evidence would never be counted, and names
  the step that fixes it — a worktree path, `git checkout`, or attestation.
  Baseline probes keep their own base-revision rule; attested evidence
  warns instead of refusing.

- `arc journal consume` refuses while a typed question on the artifact is
  unanswered (`--drop-questions` overrides); `arc begin --from-journal`
  warns and names the open question ids.

- Integration is recorded as `change-integrated` (arc guarded the merge) or
  `integration-asserted` (somebody else performed it), both naming the
  patchset, head, target branch and prior target state. `arc close
  --integrated` becomes `--assert-integrated`; `change-closed` now carries
  only abandoned and superseded outcomes, and historical integrated
  closures read as `legacy-unclassified`.

- `arc check` reports typed advisories (`arc-check/2`) in place of untyped
  coverage warnings, including `brief-author-only-review` and
  `audit-debt-outstanding`. Advisories never change readiness or the exit
  code.

- The `sol-low` brief scaffold teaches the differential probe rule —
  declare probes, observe the baseline fail and the head pass, and treat
  the absence of probes as no contract recorded — replacing the stale claim
  that probe evidence never gates integration.

- `arc chain --review` reports `brief_author` and
  `reviewed_only_by_brief_author` in place of `non_self_verdict`, which
  measured identity inequality and reported it as independence
  (`arc-chain/3`).

- Holds are independent: `arc hold` prints the event identifying the hold
  it set, `arc release-hold <change> <hold>` releases exactly that one, and
  other holds stay in force. Status reports every active hold
  (`arc-status/7`: `hold` becomes `holds`; `blocker_summary.hold.reason`
  becomes `reasons`).

- `arc workspace list` and `inbox` say when they have nothing to show,
  distinguishing an empty registry from one whose projects have no ledger,
  instead of printing nothing.

- `arc review --patchset` accepts the revision a reviewer read, not only a
  patchset id, and refuses an ambiguous or unknown one rather than falling
  back to the latest. A verdict recorded without it claims the newest
  patchset, which is not always what was reviewed.

- Closed-change append refusals now come from the permission table rather
  than twelve hand-placed guards, so the refusal names which closure state
  applies and why (`change <id> is <outcome>; event is open-only`) instead
  of a bare "is closed" (`append-policy-single-authority`).

- Require declared acceptance probes to fail at their brief base and pass
  at the patchset head.

- Accept free-form changelog categories while rendering conventional and
  custom headings deterministically.

- Permit immutable observations, discussion, and liveness cleanup after
  change closure while freezing work state.

- Rename `arc journal append` to `arc journal position` as a clean break
  while preserving the typed `position` event and artifact format.

- Retire the done, inbox, and spec journal artifact kinds for new notes
  while preserving historical parsing and reporting them as doctor advice.

- Prompt sol-low delegated briefs to define runnable acceptance probes,
  record final-head evidence, and stop rather than edit a defective probe.

- Allow one plan artifact to reference multiple changes without automatic
  consumption or brief seeding.

- Allow review verdicts to carry optional reasoning from inline text or a
  file, and expose it in show, log, and status output.

### Fixed

- A project the workspace rollup counts can now be opened by `arc journal
  open`. After Git discovery and a `[journals.dirs]` prefix both fail, the
  resolver takes a journal whose recorded binding names this canonical
  directory — two such bindings are refused as ambiguous — and otherwise
  the journal this directory's own slug names, which is the one arc would
  itself create here and the only way to reach journals written before
  bindings existed. A journal already bound elsewhere is never taken. The
  failure now names every source it checked, and says that a path-prefix
  entry covering a directory with repositories beneath it shadows their
  discovery.

- `arc changelog --write` wraps the block it generates at 75 columns, the
  width the rest of the file uses, with continuations indented under their
  marker, blank lines between entries, and paragraph breaks inside an entry
  preserved. A body that already leads with a list marker keeps the markers
  and nesting its author chose, and an over-long token overflows rather
  than being split. Entries ran to several hundred columns before, so the
  block was rewrapped by hand and the next projection undid it.

- A stacked change's patchset now records its declared opening base rather
  than the merge base against the integration target, so a stacked member's
  diff contains only what that member added instead of its whole ancestry.

- `arc doctor` checks danger classification against the whole repository,
  whatever directory it runs from. The scan previously ran `git ls-files`
  from the caller's directory, which lists only that subtree under names no
  declared source root matches, so the closed world silently stopped being
  checked and `doctor` reported nothing wrong. `src/commands/mod.rs` and
  `src/commands/history.rs` move to `danger.paths`: the first enforces
  append permissions and builds events, the second records the rewrite
  facts deciding whether a revision was moved or lost.

- A dirty-tree waiver excuses dirt that was observed, never cleanliness
  recorded as unknown. `verify --all --parallel` records cleanliness as
  unknown by design, and a waiver naming the same revision previously made
  that evidence green. `--waive-dirty` is now refused with `--parallel`
  rather than recording a waiver that could never apply.

- `arc journal position` and `arc journal answer` record the declared
  `--actor` on the typed event and name it in the heading when no model
  argued, so a person acting directly is no longer rendered `(unknown,
  ...)` and attributed to nobody. Where an actor is declared without a
  model, the heading now names the actor rather than the harness.

- `arc review` names a provisional approval in its verdict history, with
  the reason it was recorded and whether corroboration is still
  outstanding. The qualification was previously reported only when the
  verdict was written, so the read view showed a provisional approval as an
  unqualified one.

- `arc doctor` no longer hangs on repositories whose ledger records more
  revisions than a pipe buffer holds. The dangling-revision scan sent its
  whole query to `git cat-file` before reading any answer, deadlocking
  against it once the answers filled the pipe.

- The help-copy guard walks nested command groups, not only top-level
  commands, and reads clap's two-column layout; the twenty-eight
  undescribed flags and positionals it found under `forge`, `journal lane`,
  and `history resolve` are described. `arc finding --blocking` names which
  dispositions actually release the gate — resolved, accepted-risk,
  obsolete — rather than implying any disposition does.

- Every flag and positional in every command carries a description, guarded
  by a probe that walks the whole command list. A top-level kind name (`arc
  todo`, `arc handoff`) redirects to its journal verb instead of clap's
  spelling guess; `arc journal show <topic>` points at `arc journal
  latest`; and `arc fr` shares `arc journal feature-request`'s argument
  help by construction rather than restating it.

- `arc journal discussion` no longer skips every position after one whose
  body leaves a Markdown fence unclosed, which under-reported both the
  stance tally and the per-branch argued counts.

- A status report derived from the ledger alone now resolves the danger
  scope instead of assuming it: `git diff` needs the objects the recorded
  base and head name, not a working tree, so `arc status --at` no longer
  over-reports that an independent verdict is required. `arc doctor` also
  reports a declared danger path naming a directory — it exists, so the
  previous check passed it, and it can still never match, since declared
  paths are compared against changed files.

- Audit debt is discharged by any independent verdict on the revision that
  shipped, recorded after the debt was declared, rather than only by `arc
  audit`. Reviewing before merging no longer forces a choice between a
  standing debt for a review that happened and a post-integration audit
  that did not. Reviewer independence is judged against the patchset the
  reviewer read rather than the newest one, so a later snapshot by another
  author no longer relabels an earlier self-review as independent.

- Question settlement now enforces discussion-only targets, argued closing
  branches, immutable answered branches, serialized concurrent transitions,
  and semantic validation of hand-written events. Audit-debt waivers bind
  to the exact branch head, and waiver-only integration records advance and
  repair the store-format barrier. The workflow guide prints complete,
  mutually exclusive resolution and promotion commands.

- `journal discussion` now reports `unplaced`: position blocks the reply
  graph cannot see, because they carry no `position_id`. The tally reads
  the file and the graph reads the event log, so a hand-written block — or
  one recorded before ids existed — was counted and never reportable as
  answered.

- PR lifecycle facts bind to the link and head they were observed at. After
  a relink, `pr_state` reads unknown and `forge_ready` false until the new
  PR is observed, instead of inheriting the superseded PR's state. `arc
  forge pr-state` requires a recorded link and takes the head from it;
  `--link` names it explicitly.

- A gate that is not green at head now says why on every human surface
  (`resume`, `rescue`, `check`), instead of rendering the raw `pass` result
  that contradicted readiness. Dirty or moved-tree evidence advises
  `clean_worktree:<gate>` rather than a rerun that would record the same
  unusable evidence.

- `arc journal doctor`'s split-journal advice reads as one sentence, so the
  rebind command inside it can be copied as printed.

- Local gate evidence now requires a retained tested tree and known-clean
  provenance before it can be green or reused; shared parallel runs remain
  provenance-unknown and non-green, and failed tree pinning stays visible
  as unknown. Discussion docs and scaffolds teach the current position
  command and stable IDs.

- Reject imported future-tag events with incomplete envelopes before they
  can leave typed ledger reads unusable.

- Harden audit debt selection and role boundaries, preserve attributed
  review coverage, replay audit state through bundles, filter owed-review
  inbox rows consistently, and let audit findings be discussed and
  dispositioned without mutating shipped findings.

- Render every projected changelog entry as a list item, leaving authored
  markers and wrapped continuations intact (`changelog-render-normalise`).

- Require and retain the exact external execution context for attested
  verification.

- Allow changelog entries to be recorded after a change has been
  integrated.

- Group repeated doctor advice and omit historical expired claims from
  closed changes.

- Preserve displaced claim identity whenever stale or expired work is
  replaced.

- Reject malformed changelog events during bundle import before writing
  ledger events or retention references.

- Preserve additive findings JSON and attach replies deterministically by
  finding identity regardless of ledger event ordering.

- Restore retired journal kinds as read-side list and open filters, keep
  discussion derivation consistent with recognized events, version
  discussion summary JSON, and preserve capped depth for deep reply chains.

- Show replies beneath their findings and include structured reply threads
  in findings JSON.

- Allow projects with a shared Git identity to omit inapplicable delegated
  provenance mismatch warnings.

- Recompute each new patchset base from its head and target branch so
  rebased changes render only their own diff.

## [0.1.0] - 2026-07-20

### Fixed

- Piped output dies silently when the reader goes away instead of panicking
  with a broken-pipe error (e.g. `arc list --format compact | head`)
  (`sigpipe-exit`).
- Event publication fsyncs the containing directory after linking the event
  file, preserving acknowledged entries across power loss (`store-dir-fsync`).
- Attested verification events omit unobserved exit codes and durations
  (`attested-evidence-honesty`).
- Free-text `thread journal` messages are never promoted into typed
  events, and doctor counts only artifact-shaped file fields as
  references (`journal-log-fix`).

### Changed

- Role refusals name the role that can perform the command, and top-level
  nested leaf verbs suggest their complete command path
  (`cold-start-ergonomics`).
- The journal drops every thread spelling: no `arc thread` or nested
  `journal` aliases, and the storage tier is `ARC_JOURNAL_DIR`,
  `[journals]`, `<ai_home>/journals/`, and `journal-events/1` in
  `events.jsonl`; the legacy `journal.md` merge-read is removed, so
  existing archives need a one-time migration (`journal-clean-break`).
- The thread surface is renamed to the project journal: `arc journal` with
  nested `log` replaces `arc thread` with nested `journal`
  (`journal-rename`).
- The thread journal is now a typed `thread-journal/1` JSONL event log while
  legacy Markdown journals remain readable (`journal-jsonl`).

### Added

- `arc completions <shell>` and `arc mangen <dir>` generate shell completions
  and a man page; `docs/QUICKSTART.md` and a README Installation section cover
  foreign-repo setup; arc is documented as Unix-only (`release-polish`).
- `arc workspace list|inbox` aggregates open changes and inbox rollups across a
  configured `data_root`; `arc brief --scaffold <sol-low|sol-high|reviewer>`
  (or a repo `.arc/templates/<name>.md`) prepends a delegation-fenced template;
  and `arc restack <change> --advise` prints the exact rebase commands for open
  dependents without executing them (`workspace-scaffolds`).
- `arc verify --all --skip-green` skips gates already green at the exact
  current head (observed or attested), printing `skipped (green at head)` per
  gate, to avoid re-running expensive gates. A measurement (300 changes:
  `list` ~8.5 ms, `inbox` ~8.0 ms — well under budget) showed derived views
  need no on-disk cache yet, so the closure/summary caches are deferred
  (`view-cache`).
- Opt-in Git hook pack: `arc hooks install|uninstall|status` manages
  `post-commit` (stale-approval and closed-branch notices) and
  `prepare-commit-msg` (`Arc-Change:` trailer) scripts that always exit 0, plus
  `arc query --commit <rev>` to find changes by patchset or integration commit
  (`git-hooks-changeid`).
- Global `--on-behalf-of <subject>` (`ARC_ON_BEHALF_OF`) records the subject a
  lead runs delegated ceremony for while `actor` stays the invoker; the
  effective author (`on_behalf_of.unwrap_or(actor)`) drives `forbid_self_approval`
  and is rendered in show/log/status. Claim ownership still matches the invoker
  tuple. The event field is additive (`ceremony-provenance`).
- `arc begin --from-journal <artifact>` opens a change from an open actionable
  journal item, stamping `journal_ref` and consuming the item as superseded;
  opt-in `[journal] auto_log` narrates begin/integrate/close into the journal
  (advisory, warns on failure); and `journal open` annotates items taken up by
  an open change (`journal-bridge`).
- `arc stats [--change | --tag | --all] [--json]` projects ledger durations
  and counts — per-change wall time, stage and gate timing, review latency,
  findings, and patchset count, plus aggregate median/p90 and suggested stage
  budgets (`arc-stats/1`) (`ledger-stats`).
- `arc log [CHANGE] [--reverse]` prints one line per ledger event; `--at
  <event-id>` on `arc show`/`arc status` replays a change to that point;
  `arc check --explain` lists every gate condition (and `--json` every
  blocker), and `arc integrate --dry-run` simulates a merge without writing
  (`timeline-views`).
- `arc diff --between ps-A ps-B` and `--since-approved` make re-review deltas
  explicit; `arc findings --format json|sarif` exports current dispositions and
  open findings for external review tooling (`interdiff-sarif`).
- `arc diff [CHANGE] [--patchset ps-NN] [--stat] [--findings] [-- <path>...]`
  renders native patchset diffs with unresolved finding-anchor drift markers
  (`arc-diff`).
- `arc config --check-writable [--json]` probes the local ledger and Git-ref
  paths before a sandboxed executor starts work (`ledger-writability-probe`).
- Status/resume field projection, discussion-event ID prefixes, and file-backed
  lane, stage, and hold text input (`status-field-projection`).
- Resumable event cursors, non-fatal event and watch command hooks, and
  first-winner multi-condition watches (`event-hooks-cursor`).
- Atomic `arc take` scheduling claims the highest-priority ready change, with
  additive priority metadata and priority-aware queue ordering (`take-next`).
- Composed transitions cover claim-and-stage, snapshot-and-verify, snapshot-and-review, implementation completion without integration, and deterministic parallel gate execution (`composed-transitions`).
- Change-worktree inference, explicit harness environment bootstrap, resumable context, and a statusline prompt (`cwd-context`).
- Opt-in repository policy rejects self-approval using snapshot and verdict actor strings (`self-approval-policy`).
- Verification captures final output tails and enforces optional per-gate process-group timeouts (`gate-evidence-output`).
- Read-only `arc doctor [--json]` ledger integrity and housekeeping checks
  (`ledger-doctor`).
- Read-only `thread doctor [--json]` archive health and housekeeping checks
  (`thread-doctor`).
- Change-scoped implementation briefs stored in the ledger and carried by
  export/import bundles (`change-briefs`).
- Shared project memory artifacts listed by `thread memories` and always
  surfaced by hot `catchup` (`thread-memory`).
- Advisory session work lanes in the thread archive: `thread lane
  open|renew|close|list`, heartbeat-free liveness from owner journal
  activity, stale takeover, and lane occupancy surfaced in `thread open`
  and `catchup` (`thread-lanes`).
- Dependency-ordered tagged series integration via `integrate --tag`
  (`series-integration`).
- Lower-priority `later` tier in the thread open queue
  (`thread-later-tier`).
- Active executor claims shown as an `in-progress` inbox bucket
  (`inbox-in-progress`).
- `verify --all` runs every gate declared for the change profile
  (`verify-all`).
- `check` reports needs-rebase from merge simulation (`check-needs-rebase`).
- Verification attestation (`verify --attest`) and the deferred
  review-note sweep (`deferrals-sweep`).
- Cold sibling archive for thread directories (`thread-archive`) and
  open-item tracking with `thread open`/`consume` (`thread-open-items`).
- Thread archive mechanics absorbed as `arc thread` (`thread-mechanics`).
- Message events and the lead-facing orchestrator inbox
  (`messaging-inbox`); messages and assignment in status JSON
  (`status-messages`).
- Forge projection: observed hosted-PR facts with fail-closed repository
  tuple validation (`forge-projection`).
- Executor claim leases and the typed stage protocol
  (`claim-stage-protocol`).
- Implementer/reviewer/lead role enforcement (`role-enforcement`).
- Event replay and follow plus watch probes (`arc-watch-events`).
- Deterministic export/import bundles, M4 (`m4-export-import`).

### Changed

- Blocker release and wedged prerequisite semantics
  (`blocker-release-semantics`).
- Delegation and blocker UX improvements (`delegation-blocker-ux`).
- Orchestration status and recovery UX polish
  (`orchestration-status-polish`).
- Destructive-action hardening and path configuration
  (`safety-and-path-config`).

### Internal

- Monolithic `tests/cli.rs` and `commands.rs` split by area (`test-split`).
