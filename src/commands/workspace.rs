//! Cross-repository aggregation and rebase advice for a lead working across
//! many change ledgers. All read-only: no store is ever created while
//! scanning, and `restack` only prints commands — arc never rewrites branches.

use super::*;
use anyhow::ensure;
use serde::Serialize;

/// Quote one value as a single POSIX shell argument.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Render a parsed cutoff without discarding the precision that selected the
/// journal rows. UTC is rendered with `Z` so the value is accepted by the same
/// parser when a report's detail command is replayed.
fn normalized_cutoff(cutoff: chrono::DateTime<chrono::Utc>) -> String {
    cutoff.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
}

/// Only Git's explicit non-repository answer means there is no fork namespace.
fn has_git_repository(anchor: &Path) -> Result<bool> {
    let output = crate::gitio::git_command()
        .args(["rev-parse", "--git-common-dir"])
        .env("LC_ALL", "C")
        .current_dir(anchor)
        .output()
        .with_context(|| format!("cannot inspect Git repository at {}", anchor.display()))?;
    if output.status.success() {
        return Ok(true);
    }
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(128)
        && diagnostic.starts_with("fatal: not a git repository (or any")
    {
        return Ok(false);
    }
    anyhow::bail!(
        "cannot inspect Git repository at {}: {}",
        anchor.display(),
        diagnostic.trim()
    )
}

pub enum WorkspaceView {
    List,
    Inbox {
        scope: WorkspaceScope,
    },
    Backlog {
        since: Option<String>,
        items: bool,
        scope: WorkspaceScope,
        show_unreachable: bool,
        rank_by: RankBasis,
    },
}

/// The fact `workspace backlog` ranks projects by, descending. Every row
/// carries each of them as its own field, so choosing one never changes what
/// the others say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RankBasis {
    /// Verdicts owed plus decisions waiting on a person: the project's
    /// blocking facts. Coverage obligations are not blocking and are not
    /// counted here.
    Blocking,
    /// Primary work waiting to be picked up.
    Availability,
    /// Coverage obligations owed on shipped work.
    Coverage,
}

impl RankBasis {
    fn as_str(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::Availability => "availability",
            Self::Coverage => "coverage",
        }
    }

    fn value(self, project: &ProjectBacklog) -> usize {
        match self {
            Self::Blocking => project.blocking,
            Self::Availability => project.availability,
            Self::Coverage => project.coverage,
        }
    }
}

pub enum WorkspaceScope {
    Global,
    Under(PathBuf),
}

#[derive(Serialize)]
struct WorkspaceList {
    schema: &'static str,
    repos: Vec<RepoChanges>,
}

#[derive(Serialize)]
struct RepoChanges {
    repo: String,
    changes: Vec<WorkspaceRow>,
}

#[derive(Serialize)]
struct WorkspaceRow {
    change_id: String,
    slug: String,
    status: String,
    title: String,
    branch: String,
}

#[derive(Serialize)]
struct WorkspaceInbox {
    schema: &'static str,
    repos: Vec<RepoInbox>,
}

#[derive(Serialize)]
struct RepoInbox {
    repo: String,
    #[serde(flatten)]
    inbox: crate::inbox::Inbox,
}

/// One project's complete change observation: every open change classified
/// into its predicate buckets, with debt and outstanding deferrals, read from
/// the project's own checkout. The workspace inbox and the backlog's
/// per-project block both answer from this one derivation.
fn observe_changes(ctx: &Ctx, store: &Store, anchor: &Path) -> Result<crate::inbox::Inbox> {
    let project_ctx = ctx.with_cwd(anchor.to_path_buf());
    super::messaging::collect_inbox(&project_ctx, store, None)
}

/// Whether a project's complete change observation holds anything at all. A
/// held-only change or an uncollected deferred round is a reason for the
/// project to appear, not a case for the empty filter.
fn inbox_has_rows(inbox: &crate::inbox::Inbox) -> bool {
    !inbox.needs_review.is_empty()
        || !inbox.iterating.is_empty()
        || !inbox.changes_requested.is_empty()
        || !inbox.ready_to_integrate.is_empty()
        || !inbox.blocked.is_empty()
        || !inbox.held.is_empty()
        || !inbox.in_progress.is_empty()
        || !inbox.stalled.is_empty()
        || !inbox.debt_owed.is_empty()
        || !inbox.unclassified.is_empty()
        || !inbox.deferred.is_empty()
}

/// Every ledger this workspace can reach, with its label.
///
/// Two discovery modes, and the configured one always wins. With a `data_root`
/// the stores sit side by side and enumerate directly. Without one they live
/// inside each repository's Git common dir, where the journal registry is what
/// knows they exist at all.
fn workspace_stores() -> Result<Vec<(String, Store)>> {
    let cfg = crate::config::load()?;
    match cfg.data_root {
        Some(_) => data_root_stores(),
        None => registry_stores(&cfg),
    }
}

/// Ledgers found through the project registry.
///
/// A project the registry knows but cannot reach contributes no store, and
/// says so: `backlog` reports it in full, and these rollups at least name it
/// rather than letting it disappear. A project whose journal keeps a dead
/// anchor is exactly the case worth hearing about, since its ledger may be
/// perfectly healthy at a path nothing here can find.
fn registry_stores(cfg: &crate::config::Config) -> Result<Vec<(String, Store)>> {
    let mut stores = Vec::new();
    for project in crate::registry::projects(cfg)? {
        if project.is_orphan() {
            // Named by its journal directory, not by `label()`: for an orphan
            // that reads the dead anchor's last component, which identifies
            // nothing an operator can act on.
            eprintln!(
                "warning: skipping {}: its project is not at {}; \
                 `arc workspace backlog` reports it, `arc journal rebind` adopts it",
                project.journal_dir.display(),
                project
                    .anchor
                    .as_deref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "any single resolvable path".into())
            );
            continue;
        }
        let Some(root) = project.ledger.clone() else {
            continue;
        };
        match Store::open_at(&root) {
            Ok(Some(store)) => stores.push((project.label(), store)),
            Ok(None) => {}
            Err(error) => eprintln!("warning: skipping {}: {error:#}", project.label()),
        }
    }
    Ok(stores)
}

/// A `data_root` subdirectory that is an arc store, with its slug label.
fn data_root_stores() -> Result<Vec<(String, Store)>> {
    let data_root = crate::config::load()?
        .data_root
        .context("data_root is unset")?;
    let mut stores = Vec::new();
    let entries = std::fs::read_dir(&data_root)
        .with_context(|| format!("cannot read data_root {}", data_root.display()))?;
    let mut names: Vec<_> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let root = data_root.join(&name);
        match Store::open_at(&root) {
            Ok(Some(store)) => stores.push((name, store)),
            Ok(None) => {} // not an arc store — skip silently
            Err(error) => eprintln!("warning: skipping {name}: {error:#}"),
        }
    }
    Ok(stores)
}

/// The resolved shape of one backlog query, kept so the detail hint can name
/// the command that reproduces it exactly. `ResolvedWorkspaceScope` holds a
/// path it owns; this borrows the resolution the report already built.
enum BacklogScopeRef {
    Global,
    Under(String),
}

struct BacklogSelection {
    scope: BacklogScopeRef,
    /// The parsed cutoff in replayable RFC 3339 form, when one was supplied.
    since: Option<String>,
    show_unreachable: bool,
    /// The ranking basis the report used. The replay names it when it is not
    /// the default, so repeating the footer cannot silently reorder rows.
    rank_by: &'static str,
}

/// How the journal tiers in this report were selected: the boundary, what the
/// counts mean, and where the undated rows went.
#[derive(Serialize)]
struct JournalSelection {
    /// The cutoff, normalized to RFC 3339 with accepted fractional precision;
    /// null reports the whole queue.
    since: Option<String>,
    /// `arrivals` under a cutoff, `outstanding` without.
    journal_counts: &'static str,
    /// Whether a cutoff is active — exactly `since.is_some()`, carried as a
    /// field so a consumer need not re-derive presence from a nullable.
    includes_unknown_time: bool,
}

impl BacklogSelection {
    /// The command that re-runs this report as itemized JSON. The expanded
    /// form carries the resolved scope — explicit `--under` for a scoped
    /// query, `--global` otherwise — because the bare guide's examples do not
    /// say where a `--here` report would look if typed elsewhere. Every value
    /// supplied to the shell is quoted, including the normalized cutoff.
    fn detail_command(&self) -> String {
        let mut parts = vec!["arc workspace backlog".to_string()];
        match &self.scope {
            BacklogScopeRef::Global => parts.push("--global".to_string()),
            BacklogScopeRef::Under(path) => parts.push(format!("--under {}", shell_quote(path))),
        }
        if let Some(since) = &self.since {
            parts.push(format!("--since {}", shell_quote(since)));
        }
        if self.show_unreachable {
            parts.push("--unreachable".to_string());
        }
        if self.rank_by != RankBasis::Blocking.as_str() {
            parts.push(format!("--rank-by {}", self.rank_by));
        }
        parts.push("--items --json".to_string());
        parts.join(" ")
    }
}

fn repo_states(store: &Store) -> Result<BTreeMap<String, ChangeState>> {
    let mut states = BTreeMap::new();
    let rewrites = store.rewrites()?;
    for change_id in store.list_change_ids()? {
        let events = store.load_events(&change_id)?;
        states.insert(change_id, state::reduce_following(&events, &rewrites)?);
    }
    Ok(states)
}

pub fn workspace(ctx: &Ctx, view: WorkspaceView, json: bool) -> Result<i32> {
    match view {
        WorkspaceView::List => workspace_list(&workspace_stores()?, json).map(|()| 0),
        WorkspaceView::Inbox { scope } => workspace_inbox(ctx, scope, json).map(|()| 0),
        WorkspaceView::Backlog {
            since,
            items,
            scope,
            show_unreachable,
            rank_by,
        } => workspace_backlog(
            ctx,
            since.as_deref(),
            items,
            scope,
            show_unreachable,
            rank_by,
            json,
        ),
    }
}

/// What to say when the rollup has no repository to show.
///
/// Printing nothing is the same shape as a command that died with its output
/// swallowed, and this rollup used to refuse loudly when it could not run. But
/// an empty rollup is not proof of an empty registry: a project with no ledger,
/// or one whose journal points somewhere gone, is registered and still
/// contributes no store. Claiming "nothing is registered" there would replace
/// silence with something worse — a confident false statement.
fn nothing_found() -> String {
    let registered = crate::config::load().ok().and_then(|cfg| {
        let root = crate::registry::journals_root(&cfg).display().to_string();
        crate::registry::projects(&cfg)
            .ok()
            .map(|projects| (projects.len(), root))
    });
    match registered {
        Some((0, root)) => format!("no projects found: nothing is registered under {root}"),
        Some((count, _)) => {
            format!("no open changes: {count} project(s) registered, none with a ledger to report")
        }
        None => "no projects found".to_string(),
    }
}

fn workspace_list(stores: &[(String, Store)], json: bool) -> Result<()> {
    let mut repos = Vec::new();
    for (repo, store) in stores {
        let states = repo_states(store)?;
        let changes = states
            .values()
            .filter(|state| !state.is_closed())
            .map(|state| WorkspaceRow {
                change_id: state.change_id.clone(),
                slug: state.slug.clone(),
                status: change_status(state).to_string(),
                title: state.title.clone(),
                branch: state.branch.clone(),
            })
            .collect();
        repos.push(RepoChanges {
            repo: repo.clone(),
            changes,
        });
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&WorkspaceList {
                schema: "arc-workspace/1",
                repos,
            })?
        );
    } else {
        if repos.is_empty() {
            println!("{}", nothing_found());
            return Ok(());
        }
        for repo in &repos {
            println!("# {}", repo.repo);
            if repo.changes.is_empty() {
                println!("  (no open changes)");
            }
            for row in &repo.changes {
                println!(
                    "  {}  [{}] {} ({})",
                    row.change_id, row.status, row.title, row.branch
                );
            }
        }
    }
    Ok(())
}

/// The inbox rollup for every registered project in scope. Each project is
/// observed from its own checkout, so gates, policy, and live heads are the
/// project's own and the rollup answers what a per-project tour would.
fn workspace_inbox(ctx: &Ctx, scope: WorkspaceScope, json: bool) -> Result<()> {
    let cfg = crate::config::load()?;
    let scope = ResolvedWorkspaceScope::resolve(scope)?;
    let mut repos = Vec::new();
    for project in crate::registry::projects(&cfg)? {
        if !scope.includes(project.anchor.as_deref()) || !project.reachable {
            continue;
        }
        let Some(root) = project.ledger.clone() else {
            continue;
        };
        let Some(store) = Store::open_at(&root)? else {
            continue;
        };
        let anchor = project
            .anchor
            .clone()
            .expect("a reachable project has an anchor");
        repos.push(RepoInbox {
            repo: project.label(),
            inbox: observe_changes(ctx, &store, &anchor)?,
        });
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&WorkspaceInbox {
                schema: "arc-workspace/1",
                repos,
            })?
        );
    } else {
        if repos.is_empty() {
            println!("{}", nothing_found());
            return Ok(());
        }
        for repo in &repos {
            println!("# {}", repo.repo);
            for (name, rows) in repo.inbox.sections() {
                if rows.is_empty() {
                    continue;
                }
                println!("  ## {name}");
                for row in rows {
                    println!("    {}  {}", row.change_id, row.title);
                }
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct Backlog {
    schema: &'static str,
    scope: BacklogScope,
    /// When this observation ran and over what interval. Sequential
    /// construction — projects are read one after another — means the
    /// timestamp on one project's rows is not the timestamp on another's:
    /// these are the bounds of the reading, not a freshness guarantee, and
    /// no atomic-snapshot claim is made.
    observation: Observation,
    /// The fact the project order was taken from, so a consumer reads the
    /// basis rather than inferring one.
    ordering: Ordering,
    /// What was discovered, selected, skipped, observed empty, observed with
    /// facts, and failed. `discovered = selected + skipped`, and
    /// `selected = empty + non_empty + failed`.
    collection: CollectionManifest,
    /// How the journal tiers were selected: the cutoff, what the counts mean,
    /// and whether undated rows ride inside them. The scope object stands
    /// beside it unchanged.
    selection: JournalSelection,
    summary: BacklogSummary,
    projects: Vec<ProjectBacklog>,
    unreachable: Vec<UnreachableProject>,
}

#[derive(Serialize)]
struct Ordering {
    /// The row field projects are ranked by, highest first. One of
    /// `blocking`, `availability`, or `coverage`.
    basis: &'static str,
    direction: &'static str,
}

/// What the collection observed and what it could not: the boundary every
/// count in the report is relative to, and the failures that make this report
/// partial. A failed project still contributes the components that were read,
/// so a failure never reads as healthy emptiness.
#[derive(Serialize)]
struct CollectionManifest {
    /// Registry projects in the observed universe.
    discovered: usize,
    /// Projects the scope selected for observation.
    selected: usize,
    /// Projects outside the scope.
    skipped: usize,
    /// Selected projects observed with no facts at all.
    empty: usize,
    /// Selected projects observed with at least one fact and no failure.
    non_empty: usize,
    /// Selected projects with at least one failed component, unreachable
    /// anchors included.
    failed: usize,
    /// One entry per failed component; a project may name several.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failures: Vec<CollectionFailure>,
}

#[derive(Serialize)]
struct CollectionFailure {
    project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor: Option<String>,
    /// `anchor`, `ledger`, `changes`, `journal`, or `forks`.
    component: &'static str,
    reason: String,
}

/// One component of a project observation that could not be read. The rest of
/// the row still reports what it could.
#[derive(Serialize)]
struct ComponentFailure {
    component: &'static str,
    reason: String,
}

#[derive(Serialize)]
struct BacklogSummary {
    projects: usize,
    needs_review: usize,
    no_patchset: usize,
    debt_owed: usize,
    /// The debt count split by what each obligation says is missing, in
    /// severity order. A workspace total says how much is owed and nothing
    /// about what any of it owes. Derived from each row's effective kind, so
    /// grouping rows by `effective_missing` reproduces this split.
    debt_owed_by_kind: Vec<crate::inbox::DebtKindCount>,
    /// Obligations counted under the legacy default: their events recorded
    /// no kind, and the meaning every reader gives them is independent-review
    /// debt. A subset of `debt_owed`, not a separate population.
    legacy_debt_owed: usize,
    /// Unanswered questions sitting on open artifacts across all projects:
    /// decisions a person could settle now.
    decision_questions: usize,
    /// Unanswered questions on consumed, archived, or missing artifacts:
    /// unresolved records, reported but not ranked as waiting decisions.
    unresolved_question_records: usize,
    /// Findings delegated rounds deferred and no later round collected,
    /// across every project.
    deferred: usize,
    /// Active forks across every project. Orientation only: a fork is work
    /// somebody chose to keep unintegrated, not a queue waiting to merge.
    fork_count: usize,
    open_items: usize,
    later_items: usize,
    feature_requests: usize,
    /// Journal rows whose filename stamp does not parse, selected into the
    /// tiers above. They ride inside the tier counts — a delta that dropped
    /// what it could not date would under-report — and are counted here so
    /// the inclusion is visible rather than accidental.
    unknown_time_items: usize,
    unreachable: usize,
}

impl BacklogSummary {
    fn derive(projects: &[ProjectBacklog], unreachable: &[UnreachableProject]) -> Self {
        Self {
            projects: projects.len(),
            needs_review: projects
                .iter()
                .map(|project| project.needs_review.len())
                .sum(),
            no_patchset: projects
                .iter()
                .map(|project| project.no_patchset.len())
                .sum(),
            debt_owed: projects.iter().map(|project| project.debt_owed.len()).sum(),
            debt_owed_by_kind: crate::inbox::debt_kind_counts(
                projects
                    .iter()
                    .flat_map(|project| project.debt_owed.iter())
                    .map(|debt| Some(debt.effective_missing)),
            ),
            legacy_debt_owed: projects
                .iter()
                .flat_map(|project| project.debt_owed.iter())
                .filter(|debt| debt.missing_basis == DebtMissingBasis::LegacyDefault)
                .count(),
            decision_questions: projects
                .iter()
                .map(|project| project.decision_questions)
                .sum(),
            unresolved_question_records: projects
                .iter()
                .map(|project| project.open_questions.len() - project.decision_questions)
                .sum(),
            fork_count: projects.iter().map(|project| project.fork_count).sum(),
            deferred: projects
                .iter()
                .map(|project| project.changes.deferred.len())
                .sum(),
            open_items: projects.iter().map(|project| project.open_items).sum(),
            later_items: projects.iter().map(|project| project.later_items).sum(),
            feature_requests: projects
                .iter()
                .map(|project| project.feature_requests)
                .sum(),
            unknown_time_items: projects
                .iter()
                .map(|project| project.unknown_time_items.len())
                .sum(),
            unreachable: unreachable.len(),
        }
    }

    fn render(&self) {
        let unknown = if self.unknown_time_items > 0 {
            format!(", {} unknown-time", self.unknown_time_items)
        } else {
            String::new()
        };
        let legacy = if self.legacy_debt_owed > 0 {
            format!(", {} legacy-untyped", self.legacy_debt_owed)
        } else {
            String::new()
        };
        let questions = if self.unresolved_question_records > 0 {
            format!(
                "; {} decision question(s), {} unresolved record(s)",
                self.decision_questions, self.unresolved_question_records
            )
        } else if self.decision_questions > 0 {
            format!("; {} decision question(s)", self.decision_questions)
        } else {
            String::new()
        };
        println!(
            "summary: {} projects; {} needs-review; {} debt-owed{} ({}); {} no-patchset; journal {} open, {} later, {} feature-request{unknown}{questions}; {} deferred; {} unreachable",
            self.projects,
            self.needs_review,
            self.debt_owed,
            legacy,
            crate::inbox::DebtKindCount::render(&self.debt_owed_by_kind),
            self.no_patchset,
            self.open_items,
            self.later_items,
            self.feature_requests,
            self.deferred,
            self.unreachable,
        );
    }
}

#[derive(Serialize)]
struct Observation {
    started_at: String,
    finished_at: String,
    /// How the report was built: projects read one after another in one
    /// pass. Arithmetic agreement between project rows and the summary is
    /// checked over the emitted rows; it cannot establish that the
    /// underlying state did not move mid-read, and this report does not
    /// claim it did.
    consistency: &'static str,
}

#[derive(Serialize)]
struct BacklogScope {
    mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    under: Option<String>,
}

enum ResolvedWorkspaceScope {
    Global,
    Under(PathBuf),
}

impl ResolvedWorkspaceScope {
    fn resolve(scope: WorkspaceScope) -> Result<Self> {
        match scope {
            WorkspaceScope::Global => Ok(Self::Global),
            WorkspaceScope::Under(path) => {
                let path = std::fs::canonicalize(&path).with_context(|| {
                    format!("cannot resolve workspace scope {}", path.display())
                })?;
                ensure!(
                    path.is_dir(),
                    "workspace scope is not a directory: {}",
                    path.display()
                );
                Ok(Self::Under(path))
            }
        }
    }

    fn includes(&self, anchor: Option<&Path>) -> bool {
        match self {
            Self::Global => true,
            Self::Under(root) => anchor.is_some_and(|anchor| {
                anchor
                    .canonicalize()
                    .unwrap_or_else(|_| anchor.to_path_buf())
                    .starts_with(root)
            }),
        }
    }

    fn view(&self) -> BacklogScope {
        match self {
            Self::Global => BacklogScope {
                mode: "global",
                under: None,
            },
            Self::Under(path) => BacklogScope {
                mode: "under",
                under: Some(path.display().to_string()),
            },
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Global => "global".to_string(),
            Self::Under(path) => format!("under {}", path.display()),
        }
    }
}

/// One change awaiting a verdict, with what a reader needs to judge its age
/// and weight without opening it.
#[derive(Serialize)]
struct ReviewOwed {
    change_id: String,
    recorded_by: String,
    on_behalf_of: Option<String>,
    recorded_model: Option<String>,
    recorded_harness: Option<String>,
    recorded_session: Option<String>,
    /// Patchsets recorded. Always at least one: a change with none is
    /// reported under `no_patchset`, because its next step is work.
    patchsets: usize,
    /// Age of the newest patchset, in days.
    waiting_days: u64,
    /// The verdict a newer patchset superseded, when there was one. Absent
    /// means the change has never been reviewed.
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_verdict: Option<String>,
    /// Commits the target has taken since the latest patchset's base. Unknown
    /// when either revision cannot be read.
    behind_target: Option<usize>,
    /// Paths changed by both the patchset and target movement since their
    /// shared base. Unknown when either range cannot be read.
    target_path_overlap: Option<Vec<String>>,
}

/// One outstanding review obligation, carrying the facts that decide whether
/// it can be discharged and by whom.
#[derive(Serialize)]
struct DebtOwed {
    change_id: String,
    declared_at: chrono::DateTime<chrono::Utc>,
    age_days: u64,
    /// What the versioned obligation says is missing. Absent on an
    /// obligation declared before the kind was recorded, whose meaning is
    /// independent-review debt.
    #[serde(skip_serializing_if = "Option::is_none")]
    missing: Option<DebtMissing>,
    /// What this report counts the obligation as, whether or not the event
    /// recorded a kind: a legacy shape is independent-review debt by the
    /// meaning every reader already gives it. Grouping rows by this field
    /// reproduces the summary's kind split.
    effective_missing: DebtMissing,
    /// Whether `effective_missing` came from a recorded kind or from the
    /// legacy default.
    missing_basis: DebtMissingBasis,
    /// Whether the obligation carries its kind. An obligation without one
    /// cannot be filtered by what it owes.
    typed: bool,
    /// What review the shipped work did have, at the coordinates it was cast
    /// at. Absent on the legacy shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<Vec<DebtCoverage>>,
    /// Who set the contract and who answered it. Absent on the legacy shape,
    /// and when nothing was snapshotted.
    #[serde(skip_serializing_if = "Option::is_none")]
    production: Option<DebtProduction>,
    declared_by: String,
    on_behalf_of: Option<String>,
    declared_model: Option<String>,
    declared_harness: Option<String>,
    declared_session: Option<String>,
    /// Paths the unreviewed revision changed. Two obligations naming one path
    /// are two unread readings of the same code.
    surfaces: Option<Vec<String>>,
}

/// Where an obligation's effective kind came from.
#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum DebtMissingBasis {
    /// The event recorded a kind; effective_missing is that kind.
    Recorded,
    /// The event predates kinds; effective_missing is the legacy default,
    /// independent-review.
    LegacyDefault,
}

#[derive(Serialize)]
struct ProjectBacklog {
    project: String,
    anchor: String,
    /// The journal directory this project's questions and items were read
    /// from: the same path `arc journal dir` resolves inside the project.
    journal_dir: String,
    /// The project's blocking facts: verdicts owed plus decisions waiting
    /// on a person. Coverage obligations are not blocking and are counted
    /// apart, so a completed project holding routine debt does not rank as
    /// blocked on a decision.
    blocking: usize,
    /// Primary work waiting to be picked up. The tier counts below it are
    /// the same population, split by kind.
    availability: usize,
    /// Coverage obligations owed on shipped work: the review the work still
    /// owes, not whether work is stuck.
    coverage: usize,
    /// Changes whose next step is a verdict rather than more work: a
    /// patchset exists and no verdict answers it.
    needs_review: Vec<ReviewOwed>,
    /// Open changes carrying no patchset. Their next step is work, so they
    /// are not waiting on a person and do not count as blocked.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    no_patchset: Vec<String>,
    debt_owed: Vec<DebtOwed>,
    open_items: usize,
    later_items: usize,
    feature_requests: usize,
    /// Every open change with the predicate buckets it satisfies, the debt
    /// split by kind, and the outstanding round deferrals: the complete
    /// per-project observation `arc inbox` performs, taken from this
    /// project's checkout. The compact queues stay for the review and debt
    /// facts they carry in detail.
    changes: crate::inbox::Inbox,
    /// This project's selected rows whose stamp does not parse, included in
    /// the tier counts above. Empty without an active cutoff, which counts
    /// everything.
    #[serde(skip_serializing_if = "TierIsEmpty::is_empty")]
    unknown_time_items: Vec<crate::journal::ArtifactEntry>,
    /// Every unanswered question in this project's journal, whatever became
    /// of the artifact it sits on. A question on an open artifact is waiting
    /// on a person; one on a consumed or archived artifact is an unresolved
    /// record, reported rather than silently dropped, and never read as
    /// permission to reopen the artifact.
    open_questions: Vec<crate::journal::WorkspaceQuestion>,
    /// The subset of `open_questions` sitting on open artifacts: waiting
    /// decisions, the count project priority uses.
    decision_questions: usize,
    /// How many active decisions came from the opening half versus the closing
    /// half of their debates, so a reader can tell a premise still unsettled
    /// from a verdict being held open.
    opening_question_count: usize,
    closing_question_count: usize,
    /// Active forks of this project, using the read-only fork projection.
    /// Forks are orientation, never obligation: they add nothing to the
    /// blocked/decision score, and retired forks remain history.
    #[serde(skip_serializing_if = "TierIsEmpty::forks_empty")]
    forks: Vec<crate::commands::fork::ForkEntry>,
    fork_count: usize,
    /// The primary tier's oldest entry, in days. A one-item queue never looks
    /// like a backlog from inside the project; across projects it is visible.
    oldest_open_days: Option<u64>,
    /// Paths more than one outstanding obligation names, with the changes
    /// naming them. Reviewing one such change is not reviewing that path.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    shared_surfaces: BTreeMap<String, Vec<String>>,
    /// Every artifact behind the counts above, when `--items` asked for
    /// them. The same artifacts the counts are taken over, so the two cannot
    /// disagree. Absent unless asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    items: Option<BacklogItems>,
    /// Components of this observation that failed. The row still reports
    /// what was read; an empty bucket beside a listed failure is not
    /// evidence of emptiness.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failures: Vec<ComponentFailure>,
}

/// `skip_serializing_if` needs a path; a one-arm impl names the predicate.
struct TierIsEmpty;

impl TierIsEmpty {
    fn is_empty(tier: &[crate::journal::ArtifactEntry]) -> bool {
        tier.is_empty()
    }

    fn forks_empty(forks: &[crate::commands::fork::ForkEntry]) -> bool {
        forks.is_empty()
    }
}

#[derive(Serialize)]
struct BacklogItems {
    open: Vec<crate::journal::ArtifactEntry>,
    later: Vec<crate::journal::ArtifactEntry>,
    feature_requests: Vec<crate::journal::ArtifactEntry>,
}

impl ProjectBacklog {
    /// A fork is not an obligation, but its presence is still a project fact
    /// that the workspace report must retain when every obligation tier is
    /// empty. Coverage counts, and so does any change bucket: a completed
    /// project's routine debt, a held-only change, and an uncollected
    /// deferred round are each a real row even when nothing is blocking.
    fn is_empty(&self) -> bool {
        self.blocking == 0
            && self.coverage == 0
            && !inbox_has_rows(&self.changes)
            && self.no_patchset.is_empty()
            && self.open_items == 0
            && self.later_items == 0
            && self.feature_requests == 0
            && self.open_questions.is_empty()
            && self.fork_count == 0
    }
}

#[derive(Serialize)]
struct UnreachableProject {
    slug: String,
    journal_dir: String,
    anchor: Option<String>,
    reason: &'static str,
}

impl UnreachableProject {
    fn is_temporary_or_scratch(&self) -> bool {
        let Some(anchor) = self.anchor.as_deref().map(Path::new) else {
            return false;
        };
        anchor.starts_with(std::env::temp_dir())
            || anchor.starts_with("/var/tmp")
            || anchor
                .components()
                .any(|component| component.as_os_str() == "scratchpad")
    }

    fn render(&self) {
        println!("  {}  {}", self.slug, self.reason);
        println!("    journal: {}", self.journal_dir);
        println!("    adopt from the project's new location: arc journal rebind <dir>");
    }
}

fn identity_text(
    verb: &str,
    actor: &str,
    on_behalf_of: Option<&str>,
    model: Option<&str>,
    harness: Option<&str>,
    session: Option<&str>,
) -> String {
    let mut parts = vec![format!("{verb} by {actor}")];
    if let Some(subject) = on_behalf_of {
        parts.push(format!("for {subject}"));
    }
    if let Some(model) = model {
        parts.push(format!("model {model}"));
    }
    if let Some(harness) = harness {
        parts.push(format!("via {harness}"));
    }
    if let Some(session) = session {
        parts.push(format!("session {session}"));
    }
    parts.join(", ")
}

/// Observe one reachable project into its backlog row.
///
/// Components are read independently: a failed ledger read still leaves the
/// journal facts readable, and each failure is named on the row so the empty
/// half cannot pass for an empty project.
fn project_row(
    ctx: &Ctx,
    project: &crate::registry::Project,
    anchor: &Path,
    cutoff: Option<chrono::DateTime<chrono::Utc>>,
    show_items: bool,
) -> ProjectBacklog {
    let mut failures: Vec<ComponentFailure> = Vec::new();

    let (queues, changes) = match &project.ledger {
        Some(root) => {
            let queues = match ledger_queues(root) {
                Ok(queues) => queues,
                Err(error) => {
                    failures.push(ComponentFailure {
                        component: "ledger",
                        reason: format!("{error:#}"),
                    });
                    LedgerQueues::default()
                }
            };
            let changes = match Store::open_at(root) {
                Ok(Some(store)) => match observe_changes(ctx, &store, anchor) {
                    Ok(inbox) => inbox,
                    Err(error) => {
                        failures.push(ComponentFailure {
                            component: "changes",
                            reason: format!("{error:#}"),
                        });
                        crate::inbox::Inbox::new(None)
                    }
                },
                Ok(None) => crate::inbox::Inbox::new(None),
                Err(error) => {
                    failures.push(ComponentFailure {
                        component: "ledger",
                        reason: format!("{error:#}"),
                    });
                    crate::inbox::Inbox::new(None)
                }
            };
            (queues, changes)
        }
        None => (LedgerQueues::default(), crate::inbox::Inbox::new(None)),
    };

    let open_queue = match crate::journal::collect_open_in(ctx, &project.journal_dir, anchor, None)
    {
        Ok(queue) => queue,
        Err(error) => {
            failures.push(ComponentFailure {
                component: "journal",
                reason: format!("{error:#}"),
            });
            crate::journal::OpenItems::default()
        }
    };

    // Under --since the counts mean "filed since", not "outstanding": a
    // delta that reported the whole queue beside a delta heading would read
    // as a full report and be believed as one. Rows whose stamp does not
    // parse ride inside the tiers either way and are mirrored into their
    // own count, so the inclusion is a stated fact.
    let tiers = crate::journal::TierSelection::of(&open_queue, cutoff);
    let open_items = tiers.open.len();
    let later_items = tiers.later.len();
    let feature_requests = tiers.feature_requests.len();
    let unknown_rows: Vec<crate::journal::ArtifactEntry> = tiers
        .unknown_time
        .iter()
        .map(|entry| (*entry).clone())
        .collect();
    let backlog_items = if show_items {
        Some(BacklogItems {
            open: tiers.open.iter().map(|entry| (*entry).clone()).collect(),
            later: tiers.later.iter().map(|entry| (*entry).clone()).collect(),
            feature_requests: tiers
                .feature_requests
                .iter()
                .map(|entry| (*entry).clone())
                .collect(),
        })
    } else {
        None
    };
    // Question obligations are reported in full, like review and debt:
    // only the journal artifact tiers are filtered by --since, because a
    // decision that predates a delta can still be blocking work now.
    let questions = open_queue.questions.clone();
    let (opening_question_count, closing_question_count) = questions
        .iter()
        .filter(|entry| entry.disposition == crate::journal::QuestionDisposition::Open)
        .fold((0, 0), |(opening, closing), entry| {
            if entry.question.placement == "opening" {
                (opening + 1, closing)
            } else {
                (opening, closing + 1)
            }
        });
    let decision_questions = opening_question_count + closing_question_count;
    // Fork inventory uses the same read-only resolver as `fork list`,
    // run from the project's anchor: no checkouts are created, no forks
    // retired, and no readiness is inferred. A reachable non-Git anchor
    // has journal facts but no repository fork namespace, so it has no
    // inventory to query. A Git repository is probed independently of
    // its ledger: a failed inventory is returned with project context,
    // an unreadable ahead count stays null rather than becoming zero,
    // and retired forks remain history.
    let forks: Vec<crate::commands::fork::ForkEntry> = match has_git_repository(anchor) {
        Ok(false) => Vec::new(),
        Ok(true) => {
            let fork_ctx = ctx.with_cwd(anchor.to_path_buf());
            match crate::commands::fork::list_entries(&fork_ctx) {
                Ok(entries) => entries
                    .into_iter()
                    .filter(|entry| entry.retired.is_none())
                    .collect(),
                Err(error) => {
                    failures.push(ComponentFailure {
                        component: "forks",
                        reason: format!(
                            "cannot inventory forks for {}: {error:#}",
                            anchor.display()
                        ),
                    });
                    Vec::new()
                }
            }
        }
        Err(error) => {
            failures.push(ComponentFailure {
                component: "forks",
                reason: format!("cannot inventory forks for {}: {error:#}", anchor.display()),
            });
            Vec::new()
        }
    };
    let fork_count = forks.len();
    let blocking = queues.needs_review.len() + decision_questions;
    let availability = open_items;
    let coverage = queues.debt_owed.len();
    ProjectBacklog {
        project: project.label(),
        anchor: anchor.display().to_string(),
        journal_dir: project.journal_dir.display().to_string(),
        blocking,
        availability,
        coverage,
        needs_review: queues.needs_review,
        no_patchset: queues.no_patchset,
        shared_surfaces: queues.shared_surfaces,
        debt_owed: queues.debt_owed,
        open_items,
        later_items,
        feature_requests,
        changes,
        unknown_time_items: unknown_rows,
        open_questions: questions,
        decision_questions,
        opening_question_count,
        closing_question_count,
        forks,
        fork_count,
        // Age is a property of the whole queue, so it would contradict
        // counts that mean "filed since". A delta reports arrivals only.
        oldest_open_days: cutoff
            .is_none()
            .then(|| open_queue.oldest_open_days())
            .flatten(),
        items: backlog_items,
        failures,
    }
}

/// One backlog across every project the registry knows, ledger and journal
/// together.
///
/// Ranked by what is blocked, never by comparing items across projects: arc
/// records no priority that spans repositories, and inventing one here would
/// be a routing opinion rather than a derived fact.
fn workspace_backlog(
    ctx: &Ctx,
    since: Option<&str>,
    show_items: bool,
    scope: WorkspaceScope,
    show_unreachable: bool,
    rank_by: RankBasis,
    json: bool,
) -> Result<i32> {
    let cfg = crate::config::load()?;
    let scope = ResolvedWorkspaceScope::resolve(scope)?;
    let cutoff = match since {
        Some(raw) => Some(
            crate::journal::parse_since(raw)
                .with_context(|| format!("cannot read --since {raw:?}"))?,
        ),
        None => None,
    };
    // The report is built from `scope` after this; the hint records where it
    // was resolved to, which is the same fact in the form a command needs.
    let selection = BacklogSelection {
        scope: match &scope {
            ResolvedWorkspaceScope::Global => BacklogScopeRef::Global,
            ResolvedWorkspaceScope::Under(path) => {
                BacklogScopeRef::Under(path.display().to_string())
            }
        },
        since: cutoff.map(normalized_cutoff),
        show_unreachable,
        rank_by: rank_by.as_str(),
    };
    let mut projects = Vec::new();
    let mut unreachable = Vec::new();
    let mut failures: Vec<CollectionFailure> = Vec::new();
    let mut discovered = 0usize;
    let mut selected = 0usize;
    let mut skipped = 0usize;
    let mut empty = 0usize;
    let mut non_empty = 0usize;
    let mut failed = 0usize;
    let observed_at = chrono::Utc::now();

    for project in crate::registry::projects(&cfg)? {
        discovered += 1;
        if !scope.includes(project.anchor.as_deref()) {
            skipped += 1;
            continue;
        }
        selected += 1;
        if !project.reachable {
            // An orphan holds work nobody can reach; a merely empty journal at
            // a vanished path is housekeeping, not a finding. Either way the
            // census counts it as a failed observation, because an anchor that
            // is not there is not a project with no work.
            let reason = match project.anchor {
                Some(_) => "anchor does not exist",
                None => "journal name resolves to no single path",
            };
            if project.is_orphan() {
                unreachable.push(UnreachableProject {
                    slug: project.slug.clone(),
                    journal_dir: project.journal_dir.display().to_string(),
                    anchor: project.anchor.as_ref().map(|p| p.display().to_string()),
                    reason,
                });
            }
            failures.push(CollectionFailure {
                project: project.label(),
                anchor: project.anchor.as_ref().map(|p| p.display().to_string()),
                component: "anchor",
                reason: reason.to_string(),
            });
            failed += 1;
            continue;
        }
        let anchor = project
            .anchor
            .clone()
            .expect("a reachable project has an anchor");
        let entry = project_row(ctx, &project, &anchor, cutoff, show_items);
        for failure in &entry.failures {
            failures.push(CollectionFailure {
                project: project.label(),
                anchor: Some(anchor.display().to_string()),
                component: failure.component,
                reason: failure.reason.clone(),
            });
        }
        if !entry.failures.is_empty() {
            // A project whose read failed is neither empty nor healthy: its
            // partial facts stay in the report beside the named failure.
            failed += 1;
            projects.push(entry);
        } else if entry.is_empty() {
            empty += 1;
        } else {
            non_empty += 1;
            projects.push(entry);
        }
    }
    projects.sort_by(|a, b| {
        rank_by
            .value(b)
            .cmp(&rank_by.value(a))
            .then_with(|| b.blocking.cmp(&a.blocking))
            .then_with(|| b.availability.cmp(&a.availability))
            .then_with(|| b.coverage.cmp(&a.coverage))
            .then_with(|| a.project.cmp(&b.project))
    });
    let summary = BacklogSummary::derive(&projects, &unreachable);
    let collection = CollectionManifest {
        discovered,
        selected,
        skipped,
        empty,
        non_empty,
        failed,
        failures,
    };
    let partial = collection.failed > 0;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Backlog {
                schema: "arc-workspace-backlog/18",
                scope: scope.view(),
                observation: Observation {
                    started_at: observed_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    finished_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    consistency: "sequential",
                },
                collection,
                ordering: Ordering {
                    basis: rank_by.as_str(),
                    direction: "descending",
                },
                selection: JournalSelection {
                    since: cutoff.map(normalized_cutoff),
                    journal_counts: if cutoff.is_some() {
                        "arrivals"
                    } else {
                        "outstanding"
                    },
                    includes_unknown_time: cutoff.is_some(),
                },
                summary,
                projects,
                unreachable,
            })?
        );
        return Ok(if partial { 16 } else { 0 });
    }

    println!("scope: {}", scope.text());
    println!("ordering: {} (descending)", rank_by.as_str());
    println!(
        "collection: {} discovered, {} selected ({} skipped); {} non-empty, {} empty, {} failed",
        collection.discovered,
        collection.selected,
        collection.skipped,
        collection.non_empty,
        collection.empty,
        collection.failed
    );
    for failure in &collection.failures {
        println!(
            "  failed {} [{}]: {}",
            failure.project, failure.component, failure.reason
        );
    }
    if let Some(raw) = since {
        println!("since {raw}: journal counts are what was filed since, not what is outstanding");
        if summary.unknown_time_items > 0 {
            println!(
                "  {} item(s) with an unreadable timestamp are included and counted as unknown-time",
                summary.unknown_time_items
            );
        }
    }
    summary.render();
    if projects.is_empty() && unreachable.is_empty() {
        println!("nothing outstanding in this workspace scope");
        // Over an empty scope too: this is when a reader is likeliest to
        // wonder whether the query found nothing or the report was trimmed.
        println!("detail: {}", selection.detail_command());
        return Ok(if partial { 16 } else { 0 });
    }
    for project in &projects {
        println!("# {} ({})", project.project, project.anchor);
        let buckets: Vec<String> = project
            .changes
            .sections()
            .iter()
            .filter(|(name, rows)| {
                !rows.is_empty() && !matches!(*name, "needs-review" | "debt-owed")
            })
            .map(|(name, rows)| format!("{name} {}", rows.len()))
            .collect();
        let deferred = project.changes.deferred.len();
        if !buckets.is_empty() || deferred > 0 {
            println!(
                "  changes: {}{}",
                buckets.join(" · "),
                if deferred > 0 {
                    format!(" · deferred {deferred}")
                } else {
                    String::new()
                }
            );
        }
        for change in &project.needs_review {
            let seen = match &change.superseded_verdict {
                Some(verdict) => format!(", {verdict} superseded"),
                None => String::new(),
            };
            let stale = match change.behind_target {
                Some(0) => String::new(),
                Some(behind) => format!(", {behind} behind target"),
                None => ", target distance unknown".to_string(),
            };
            let overlap = match &change.target_path_overlap {
                Some(paths) if paths.is_empty() => String::new(),
                Some(paths) => format!(", {} overlapping paths", paths.len()),
                None => ", target path overlap unknown".to_string(),
            };
            println!(
                "  needs-review  {}  waiting {}d{seen}{stale}{overlap}, {}",
                change.change_id,
                change.waiting_days,
                identity_text(
                    "recorded",
                    &change.recorded_by,
                    change.on_behalf_of.as_deref(),
                    change.recorded_model.as_deref(),
                    change.recorded_harness.as_deref(),
                    change.recorded_session.as_deref(),
                )
            );
        }
        for change in &project.no_patchset {
            println!("  no-patchset   {change}  open, nothing recorded to review");
        }
        for change in &project.debt_owed {
            println!(
                "  debt-owed     {}  {}d, {}, {}{}{}",
                change.change_id,
                change.age_days,
                crate::render::debt_line(
                    Some(change.effective_missing),
                    change.production.as_ref(),
                    change.coverage.as_deref()
                )
                .trim_end_matches(','),
                if change.missing_basis == DebtMissingBasis::LegacyDefault {
                    " (legacy event, no kind recorded), "
                } else {
                    ""
                },
                identity_text(
                    "declared",
                    &change.declared_by,
                    change.on_behalf_of.as_deref(),
                    change.declared_model.as_deref(),
                    change.declared_harness.as_deref(),
                    change.declared_session.as_deref(),
                ),
                if change.surfaces.is_none() {
                    ", surfaces unknown"
                } else {
                    ""
                }
            );
        }
        // The text view is read to decide what to open next, so it names the
        // most-carried paths and counts the rest. `--json` carries them all.
        let mut shared: Vec<_> = project.shared_surfaces.iter().collect();
        shared.sort_by(|(left_path, left), (right_path, right)| {
            right
                .len()
                .cmp(&left.len())
                .then_with(|| left_path.cmp(right_path))
        });
        for (surface, changes) in shared.iter().take(SHARED_SURFACES_SHOWN) {
            println!(
                "  shared        {surface}  unread by {} changes: {}",
                changes.len(),
                changes.join(", ")
            );
        }
        if let Some(rest) = shared
            .len()
            .checked_sub(SHARED_SURFACES_SHOWN)
            .filter(|rest| *rest > 0)
        {
            println!("  shared        +{rest} more paths carried by more than one obligation");
        }
        let age = match project.oldest_open_days {
            Some(days) => format!(", oldest {days}d"),
            None => String::new(),
        };
        println!(
            "  journal       {} open, {} later, {} feature-request{}",
            project.open_items, project.later_items, project.feature_requests, age
        );
        if project.decision_questions > 0 || !project.open_questions.is_empty() {
            println!(
                "  questions     {} waiting on open artifact(s) ({} opening, {} closing), {} on consumed/archived",
                project.decision_questions,
                project.opening_question_count,
                project.closing_question_count,
                project.open_questions.len() - project.decision_questions,
            );
            for question in &project.open_questions {
                let disposition = match question.disposition {
                    crate::journal::QuestionDisposition::Open => "open",
                    crate::journal::QuestionDisposition::Consumed => "consumed",
                    crate::journal::QuestionDisposition::Archived => "archived",
                    crate::journal::QuestionDisposition::Missing => "missing",
                };
                println!(
                    "    [{}] {}  {}  {}",
                    disposition,
                    question.question.file,
                    question.question.question,
                    question.question.heading.as_deref().unwrap_or(""),
                );
            }
        }
        for fork in &project.forks {
            let ahead = fork
                .ahead
                .map(|count| format!("+{count}"))
                .unwrap_or_else(|| "+?".to_string());
            println!(
                "  fork          {}  {}  {} over {}",
                fork.slug, fork.branch, ahead, fork.base_branch
            );
        }
        if show_items {
            if let Some(items) = &project.items {
                for item in items
                    .open
                    .iter()
                    .chain(items.later.iter())
                    .chain(items.feature_requests.iter())
                {
                    crate::journal::render_open_entry(item);
                }
            }
        }
    }
    if !unreachable.is_empty() {
        let temporary = unreachable
            .iter()
            .filter(|project| project.is_temporary_or_scratch())
            .count();
        let durable = unreachable.len() - temporary;
        println!(
            "maintenance: {} unreachable journals ({temporary} temporary/scratch, {durable} other)",
            unreachable.len()
        );
        if show_unreachable {
            println!("unreachable:");
            for project in &unreachable {
                project.render();
            }
        } else {
            for project in unreachable
                .iter()
                .filter(|project| !project.is_temporary_or_scratch())
            {
                project.render();
            }
            if temporary > 0 {
                println!(
                    "  {temporary} temporary/scratch journals hidden; rerun with --unreachable to expand"
                );
            }
        }
    }
    // The footer closes the report, after every project section. Only the
    // human path carries it: JSON is one parseable value and must not grow a
    // trailing line.
    println!("detail: {}", selection.detail_command());
    Ok(if partial { 16 } else { 0 })
}

/// The two ledger buckets a lead reads across projects: what awaits a verdict,
/// and what shipped owing one.
/// What one project's ledger owes, read once so the buckets cannot disagree.
#[derive(Default)]
struct LedgerQueues {
    needs_review: Vec<ReviewOwed>,
    no_patchset: Vec<String>,
    debt_owed: Vec<DebtOwed>,
    shared_surfaces: BTreeMap<String, Vec<String>>,
}

fn ledger_queues(root: &Path) -> Result<LedgerQueues> {
    let Some(store) = Store::open_at(root)? else {
        return Ok(LedgerQueues::default());
    };
    let states = repo_states(&store)?;
    let now = chrono::Utc::now();
    let mut needs_review = Vec::new();
    let mut no_patchset = Vec::new();
    let mut debt_owed = Vec::new();
    for state in states.values() {
        // Audit debt outlives integration, so it is asked of every change;
        // a review verdict is only owed while the change is still open.
        if state.debt_outstanding() {
            if let Some(debt) = &state.debt {
                debt_owed.push(DebtOwed {
                    surfaces: crate::commands::messaging::debt_surfaces(root, state, debt),
                    change_id: state.change_id.clone(),
                    declared_at: debt.declared_at,
                    age_days: days_between(debt.declared_at, now),
                    missing: debt.missing,
                    effective_missing: debt.missing.unwrap_or(DebtMissing::IndependentReview),
                    missing_basis: if debt.missing.is_some() {
                        DebtMissingBasis::Recorded
                    } else {
                        DebtMissingBasis::LegacyDefault
                    },
                    typed: debt.missing.is_some(),
                    coverage: debt.coverage.clone(),
                    production: debt.production.clone(),
                    declared_by: debt.actor.clone(),
                    on_behalf_of: debt.on_behalf_of.clone(),
                    declared_model: debt.model.clone(),
                    declared_harness: debt.harness.clone(),
                    declared_session: debt.session.clone(),
                });
            }
        }
        if !state.is_closed() && crate::inbox::needs_review(state) {
            // A change with no patchset is waiting on work, not on a person.
            // Reporting it beside changes that carry a reviewable revision
            // makes a queue of empty changes read as review backlog.
            match state.latest_patchset() {
                Some(patchset) => {
                    let (behind_target, target_path_overlap) =
                        target_movement(root, state, patchset);
                    needs_review.push(ReviewOwed {
                        change_id: state.change_id.clone(),
                        recorded_by: patchset.actor.clone(),
                        on_behalf_of: patchset.on_behalf_of.clone(),
                        recorded_model: patchset.model.clone(),
                        recorded_harness: patchset.harness.clone(),
                        recorded_session: patchset.session.clone(),
                        patchsets: state.patchsets.len(),
                        waiting_days: days_between(patchset.created_at, now),
                        superseded_verdict: state
                            .latest_verdict()
                            .and_then(|verdict| wire_name(&verdict.verdict)),
                        behind_target,
                        target_path_overlap,
                    });
                }
                None => no_patchset.push(state.change_id.clone()),
            }
        }
    }
    needs_review.sort_by(|a, b| a.change_id.cmp(&b.change_id));
    no_patchset.sort();
    debt_owed.sort_by(|a, b| a.change_id.cmp(&b.change_id));
    Ok(LedgerQueues {
        shared_surfaces: shared_surfaces(&debt_owed),
        needs_review,
        no_patchset,
        debt_owed,
    })
}

/// Paths named by more than one outstanding obligation. Debt is recorded per
/// change, so a path carried by several is invisible from any one of them,
/// and reading the change that finally touches it reads only the newest of
/// the readings nobody has done.
fn shared_surfaces(debts: &[DebtOwed]) -> BTreeMap<String, Vec<String>> {
    let mut by_path: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for debt in debts {
        let Some(surfaces) = &debt.surfaces else {
            continue;
        };
        for surface in surfaces {
            by_path
                .entry(surface.clone())
                .or_default()
                .push(debt.change_id.clone());
        }
    }
    by_path.retain(|_, changes| changes.len() > 1);
    by_path
}

/// Target distance measures integration staleness; overlapping paths name
/// direct file overlap. Semantic conflicts can cross files and require gates
/// against the combined tree. Each probe preserves failure independently.
fn target_movement(
    root: &Path,
    state: &ChangeState,
    patchset: &crate::state::Patchset,
) -> (Option<usize>, Option<Vec<String>>) {
    let behind_target = crate::gitio::ahead_count(root, &patchset.base, &state.target_branch).ok();
    let target_path_overlap =
        crate::gitio::changed_paths(root, &patchset.base, &state.target_branch)
            .ok()
            .zip(crate::gitio::changed_paths(root, &patchset.base, &patchset.head).ok())
            .map(|(target, patchset)| {
                let patchset: BTreeSet<_> = patchset.into_iter().collect();
                target
                    .into_iter()
                    .filter(|path| patchset.contains(path))
                    .collect()
            });
    (behind_target, target_path_overlap)
}

/// How many shared paths the text view names before it starts counting.
const SHARED_SURFACES_SHOWN: usize = 3;

/// The wire spelling of a serde enum, so the report never carries a second
/// hand-written copy of a name the model already defines.
fn wire_name<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_value(value)
        .ok()?
        .as_str()
        .map(str::to_owned)
}

/// Whole days between two instants, floored, and never negative: a clock that
/// disagrees with a recorded stamp must not read as a negative age.
fn days_between(from: chrono::DateTime<chrono::Utc>, to: chrono::DateTime<chrono::Utc>) -> u64 {
    (to - from).num_days().max(0) as u64
}

/// Print the exact, safe rebase command for every open change that depended on
/// a now-integrated change. arc never executes it: rewriting a branch is always
/// the operator's explicit action.
pub fn restack(ctx: &Ctx, reference: &str, advise: bool) -> Result<()> {
    if !advise {
        bail!("restack only supports --advise; arc never rewrites branches");
    }
    let store = ctx.store()?;
    let (change_id, state) = ctx.load_state(&store, reference)?;
    let states = ctx.load_all_states(&store)?;
    let dependents: Vec<&ChangeState> = states
        .values()
        .filter(|candidate| !candidate.is_closed() && candidate.blocked_by.contains(&change_id))
        .collect();

    if !state.is_closed() {
        println!("note: {change_id} is not integrated yet; restack advice applies once it lands");
    }
    if dependents.is_empty() {
        println!("nothing to restack: no open dependents of {change_id}");
        return Ok(());
    }
    for dependent in dependents {
        println!("# {} ({})", dependent.change_id, dependent.slug);
        match &dependent.worktree {
            Some(worktree) => println!(
                "  git -C {worktree} rebase --onto {} {}",
                dependent.target_branch, dependent.base
            ),
            None => println!(
                "  git rebase --onto {} {} {}  # no worktree recorded; run in a checkout of {}",
                dependent.target_branch, dependent.base, dependent.branch, dependent.branch
            ),
        }
    }
    Ok(())
}
