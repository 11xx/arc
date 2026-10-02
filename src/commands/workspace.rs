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
    Inventory {
        scope: WorkspaceScope,
        storage: StorageSelection,
    },
    Backlog {
        since: Option<String>,
        items: bool,
        scope: WorkspaceScope,
        show_unreachable: bool,
        rank_by: RankBasis,
    },
    Report {
        scope: WorkspaceScope,
        previous: Option<PathBuf>,
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

/// Which stores a workspace reconciliation reads. The choice is explicit so a
/// consumer never has to infer from a missing row whether work finished or was
/// shelved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StorageSelection {
    /// The hot journal directory only.
    Hot,
    /// The cold archive only.
    Archived,
    /// Both stores, each row naming the one it came from.
    All,
}

impl StorageSelection {
    fn stores(self) -> &'static [bool] {
        match self {
            Self::Hot => &[false],
            Self::Archived => &[true],
            Self::All => &[false, true],
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Archived => "archived",
            Self::All => "all",
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

pub fn workspace(ctx: &Ctx, view: WorkspaceView, json: bool) -> Result<i32> {
    match view {
        WorkspaceView::List => workspace_list(&workspace_stores()?, json).map(|()| 0),
        WorkspaceView::Inbox { scope } => workspace_inbox(ctx, scope, json).map(|()| 0),
        WorkspaceView::Inventory { scope, storage } => {
            workspace_inventory(ctx, scope, storage, json)
        }
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
        WorkspaceView::Report { scope, previous } => {
            workspace_report(ctx, scope, previous.as_deref(), json)
        }
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
        let states = match store.readable_states() {
            Ok(states) => states,
            Err(error) => {
                eprintln!("warning: skipping {repo}: {error:#}");
                continue;
            }
        };
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
        let anchor = project
            .anchor
            .clone()
            .expect("a reachable project has an anchor");
        let observed = Store::open_at(&root).and_then(|store| match store {
            Some(store) => observe_changes(ctx, &store, &anchor).map(Some),
            None => Ok(None),
        });
        match observed {
            Ok(Some(inbox)) => repos.push(RepoInbox {
                repo: project.label(),
                inbox,
            }),
            Ok(None) => {}
            Err(error) => eprintln!("warning: skipping {}: {error:#}", project.label()),
        }
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
    /// Selected projects with at least one failed component, orphaned
    /// anchors included.
    failed: usize,
    /// One entry per failed component; a project may name several.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failures: Vec<CollectionFailure>,
}

/// Render the collection boundaries every workspace projection shares.
fn render_collection(collection: &CollectionManifest) {
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

/// Whether an anchor sits where probes and scratch sessions live — the
/// system temporary directory, `/var/tmp`, or any `scratchpad` directory — so
/// its disappearance is housekeeping rather than lost work.
pub(super) fn is_temporary_or_scratch_anchor(anchor: &Path) -> bool {
    anchor.starts_with(std::env::temp_dir())
        || anchor.starts_with("/var/tmp")
        || anchor
            .components()
            .any(|component| component.as_os_str() == "scratchpad")
}

impl UnreachableProject {
    fn is_temporary_or_scratch(&self) -> bool {
        self.anchor
            .as_deref()
            .is_some_and(|anchor| is_temporary_or_scratch_anchor(Path::new(anchor)))
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

/// Every artifact in the selected stores across the workspace, each with why
/// it is where it is. A reconciliation says what finished, what was shelved,
/// and what a transition superseded, so a hot row's absence is never read as
/// completion.
fn workspace_inventory(
    ctx: &Ctx,
    scope: WorkspaceScope,
    storage: StorageSelection,
    json: bool,
) -> Result<i32> {
    let cfg = crate::config::load()?;
    let scope = ResolvedWorkspaceScope::resolve(scope)?;
    let observed_at = chrono::Utc::now();
    let mut projects = Vec::new();
    let mut unreachable = Vec::new();
    let mut failures: Vec<CollectionFailure> = Vec::new();
    let mut discovered = 0usize;
    let mut selected = 0usize;
    let mut skipped = 0usize;
    let mut empty = 0usize;
    let mut non_empty = 0usize;
    let mut failed = 0usize;

    for project in crate::registry::projects(&cfg)? {
        discovered += 1;
        if !scope.includes(project.anchor.as_deref()) {
            skipped += 1;
            continue;
        }
        selected += 1;
        if !project.reachable {
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
        let mut row_failed = false;
        let mut entries = Vec::new();
        for archived in storage.stores() {
            match crate::journal::inventory_artifacts(ctx, &project.journal_dir, &anchor, *archived)
            {
                Ok(items) => entries.extend(items),
                Err(error) => {
                    row_failed = true;
                    failures.push(CollectionFailure {
                        project: project.label(),
                        anchor: Some(anchor.display().to_string()),
                        component: if *archived { "archive" } else { "journal" },
                        reason: format!("{error:#}"),
                    });
                }
            }
        }
        let facts = match crate::journal::reconciliation_facts(&project.journal_dir) {
            Ok(facts) => facts,
            Err(error) => {
                row_failed = true;
                failures.push(CollectionFailure {
                    project: project.label(),
                    anchor: Some(anchor.display().to_string()),
                    component: "events",
                    reason: format!("{error:#}"),
                });
                crate::journal::ReconciliationFacts::default()
            }
        };
        let mut artifacts: Vec<ReconciledArtifact> = entries
            .iter()
            .map(|entry| reconcile_artifact(entry, &facts))
            .collect();
        artifacts.sort_by(|a, b| {
            b.timestamp
                .cmp(&a.timestamp)
                .then_with(|| a.file.cmp(&b.file))
        });
        let row = ReconciledProject {
            project: project.label(),
            anchor: anchor.display().to_string(),
            journal_dir: project.journal_dir.display().to_string(),
            artifacts,
        };
        if row_failed {
            failed += 1;
            projects.push(row);
        } else if row.artifacts.is_empty() {
            empty += 1;
        } else {
            non_empty += 1;
            projects.push(row);
        }
    }

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
            serde_json::to_string_pretty(&WorkspaceInventory {
                schema: "arc-workspace-inventory/2",
                scope: scope.view(),
                storage: storage.as_str(),
                observation: Observation {
                    started_at: observed_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    finished_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    consistency: "sequential",
                },
                collection,
                projects,
                unreachable,
            })?
        );
        return Ok(if partial { 16 } else { 0 });
    }

    println!("scope: {}", scope.text());
    println!("storage: {}", storage.as_str());
    render_collection(&collection);
    for project in &projects {
        println!("# {} ({})", project.project, project.anchor);
        for artifact in &project.artifacts {
            println!(
                "  {}  {}  {}  {}  {}",
                artifact.storage,
                artifact.timestamp,
                artifact.explanation,
                artifact.resolution.as_deref().unwrap_or("unknown"),
                artifact.file
            );
            if let Some(successor) = &artifact.superseded_by {
                println!("    superseded by {successor}");
            }
        }
    }
    for project in &unreachable {
        project.render();
    }
    Ok(if partial { 16 } else { 0 })
}

#[derive(Serialize)]
struct WorkspaceInventory {
    schema: &'static str,
    scope: BacklogScope,
    storage: &'static str,
    observation: Observation,
    collection: CollectionManifest,
    projects: Vec<ReconciledProject>,
    unreachable: Vec<UnreachableProject>,
}

#[derive(Serialize)]
struct ReconciledProject {
    project: String,
    anchor: String,
    journal_dir: String,
    /// Artifacts from the selected stores, newest first, keyed by filename
    /// rather than by topic: two files sharing a topic are two rows.
    artifacts: Vec<ReconciledArtifact>,
}

#[derive(Serialize)]
struct ReconciledArtifact {
    file: String,
    topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    timestamp: String,
    storage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution_basis: Option<String>,
    /// The recorded successor this artifact was superseded by, when a
    /// transition named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
    /// Why the artifact is where it is: `present`, `terminal`, `archived`, or
    /// `superseded`. Nothing is classified from one store's absence alone.
    explanation: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    promotions: Option<Vec<crate::journal::InventoryPromotion>>,
}

fn reconcile_artifact(
    entry: &crate::journal::ArtifactEntry,
    facts: &crate::journal::ReconciliationFacts,
) -> ReconciledArtifact {
    let consumed = facts.consumed.get(&entry.file);
    let superseded_by = facts.successors.get(&entry.file).cloned();
    let explanation = if superseded_by.is_some() {
        "superseded"
    } else if consumed.is_some() || entry.resolution.is_some() {
        "terminal"
    } else if entry.storage == "archived" {
        "archived"
    } else {
        "present"
    };
    ReconciledArtifact {
        file: entry.file.clone(),
        topic: entry.topic.clone(),
        kind: entry.kind.clone(),
        timestamp: entry.timestamp.clone(),
        storage: entry.storage.clone(),
        resolution: entry.resolution.clone(),
        resolution_basis: entry.resolution_basis.clone(),
        superseded_by,
        explanation,
        promotions: entry.promotions.clone(),
    }
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
/// One observation of the workspace backlog, shared by `workspace backlog`
/// and `workspace report` so both read the same projects the same way.
struct CollectedBacklog {
    backlog: Backlog,
    scope: ResolvedWorkspaceScope,
    selection: BacklogSelection,
    partial: bool,
}

fn collect_backlog(
    ctx: &Ctx,
    since: Option<&str>,
    show_items: bool,
    scope: WorkspaceScope,
    show_unreachable: bool,
    rank_by: RankBasis,
    include_empty: bool,
) -> Result<CollectedBacklog> {
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
            // An orphan holds work nobody can reach, so the census counts it
            // as a failed observation: an anchor that is not there is not a
            // project with no work. A journal at a vanished path that holds
            // nothing and was never bound is housekeeping, observed empty.
            if !project.is_orphan() {
                empty += 1;
                continue;
            }
            let reason = match project.anchor {
                Some(_) => "anchor does not exist",
                None => "journal name resolves to no single path",
            };
            unreachable.push(UnreachableProject {
                slug: project.slug.clone(),
                journal_dir: project.journal_dir.display().to_string(),
                anchor: project.anchor.as_ref().map(|p| p.display().to_string()),
                reason,
            });
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
            if include_empty {
                projects.push(entry);
            }
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

    let backlog = Backlog {
        schema: "arc-workspace-backlog/19",
        scope: scope.view(),
        observation: Observation {
            started_at: observed_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
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
    };
    Ok(CollectedBacklog {
        backlog,
        scope,
        selection,
        partial,
    })
}

fn workspace_backlog(
    ctx: &Ctx,
    since: Option<&str>,
    show_items: bool,
    scope: WorkspaceScope,
    show_unreachable: bool,
    rank_by: RankBasis,
    json: bool,
) -> Result<i32> {
    let CollectedBacklog {
        backlog,
        scope,
        selection,
        partial,
    } = collect_backlog(
        ctx,
        since,
        show_items,
        scope,
        show_unreachable,
        rank_by,
        false,
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&backlog)?);
        return Ok(if partial { 16 } else { 0 });
    }
    let Backlog {
        collection,
        summary,
        projects,
        unreachable,
        ..
    } = backlog;

    println!("scope: {}", scope.text());
    println!("ordering: {} (descending)", rank_by.as_str());
    render_collection(&collection);
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

/// Read each reported project's ledger once for what the backlog does not
/// carry: how long every open change has been open, and which closed changes
/// left a worktree on disk. Failures belong to this observation even when an
/// earlier read of the ledger succeeded.
fn report_ledger_facts(
    backlog: &Backlog,
) -> Result<(
    BTreeMap<String, report::LedgerFacts>,
    Vec<CollectionFailure>,
)> {
    let observed = chrono::DateTime::parse_from_rfc3339(&backlog.observation.finished_at)
        .ok()
        .map(|at| at.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);
    let reported: BTreeSet<String> = backlog
        .projects
        .iter()
        .map(|project| project.project.clone())
        .collect();
    let cfg = crate::config::load()?;
    let mut facts = BTreeMap::new();
    let mut failures = Vec::new();
    for project in crate::registry::projects(&cfg)? {
        let label = project.label();
        if !reported.contains(&label) {
            continue;
        }
        let Some(root) = &project.ledger else {
            continue;
        };
        let anchor = project.anchor.clone();
        let states = Store::open_at(root)
            .and_then(|store| store.context("ledger is missing"))
            .and_then(|store| {
                let candidates = super::candidate::load_ledger(&store)?;
                Ok((store.readable_states()?, candidates))
            });
        let (states, candidates) = match states {
            Ok(states) => states,
            Err(error) => {
                failures.push(CollectionFailure {
                    project: label,
                    anchor: anchor.as_ref().map(|path| path.display().to_string()),
                    component: "report-ledger",
                    reason: format!("{error:#}"),
                });
                continue;
            }
        };
        let mut ledger = report::LedgerFacts {
            unjudged_siblings: unjudged_siblings(&candidates),
            ..Default::default()
        };
        for (change_id, state) in states {
            match &state.closure {
                None => {
                    let days = u64::try_from((observed - state.opened_at).num_days()).unwrap_or(0);
                    if let Some(brief) = state.latest_brief() {
                        let keys = report::BriefKeys {
                            plan: brief.plan_ref.clone().zip(brief.plan_slice.clone()),
                            digest: crate::journal_exchange::body_digest(&brief.body),
                        };
                        ledger.open_briefs.insert(change_id.clone(), keys);
                    }
                    ledger.open.insert(change_id, (state.title.clone(), days));
                }
                Some(_) => {
                    // A change begun without a worktree records the main
                    // checkout, which outlives every change by design; only
                    // a separate checkout left behind is worth a look.
                    let separate = |path: &&str| {
                        let path = Path::new(path);
                        path.exists()
                            && anchor.as_ref().is_none_or(|anchor| {
                                path.canonicalize().ok() != anchor.canonicalize().ok()
                            })
                    };
                    if let Some(worktree) = state.worktree.as_deref().filter(separate) {
                        ledger
                            .closed_worktrees
                            .push((change_id, worktree.to_string()));
                    }
                }
            }
        }
        facts.insert(label, ledger);
    }
    Ok((facts, failures))
}

/// For each promoted candidate, the registrations answering the same brief
/// version that nobody has judged, as (change, promoted, unjudged).
fn unjudged_siblings(ledger: &super::candidate::Ledger) -> Vec<(String, String, Vec<String>)> {
    let mut facts: Vec<(String, String, Vec<String>)> = Vec::new();
    for selection in ledger.selections() {
        if ledger.promotion_of(&selection.event_id).is_none() {
            continue;
        }
        let Some(promoted) = ledger.registration(&selection.candidate_id) else {
            continue;
        };
        let unjudged: Vec<String> = ledger
            .siblings(promoted)
            .into_iter()
            .filter(|sibling| ledger.judgements_of(&sibling.candidate_id).is_empty())
            .filter(|sibling| {
                // A sibling that was itself promoted was judged by selection.
                !ledger.selections().any(|other| {
                    other.candidate_id == sibling.candidate_id
                        && ledger.promotion_of(&other.event_id).is_some()
                })
            })
            .map(|sibling| sibling.candidate_id.clone())
            .collect();
        let known = facts.iter().any(|(change, candidate, _)| {
            change == &selection.change_id && candidate == &promoted.candidate_id
        });
        if !unjudged.is_empty() && !known {
            facts.push((
                selection.change_id.clone(),
                promoted.candidate_id.clone(),
                unjudged,
            ));
        }
    }
    facts
}

/// The workspace backlog classified by `workspace_report`'s rules, with
/// deltas against a previous report when one is given. Every artifact that
/// left the backlog since that report is looked up in its journal, hot and
/// archived, so its reason is the recorded one.
fn workspace_report(
    ctx: &Ctx,
    scope: WorkspaceScope,
    previous: Option<&Path>,
    json: bool,
) -> Result<i32> {
    let mut collected = collect_backlog(ctx, None, true, scope, false, RankBasis::Blocking, true)?;
    let (ledgers, mut failures) = report_ledger_facts(&collected.backlog)?;
    let backlog = serde_json::to_value(&collected.backlog)?;
    let previous = match previous {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read previous report {}", path.display()))?;
            let value: serde_json::Value = serde_json::from_str(&raw)
                .with_context(|| format!("previous report {} is not JSON", path.display()))?;
            let schema = value.get("schema").and_then(serde_json::Value::as_str);
            ensure!(
                schema == Some(report::SCHEMA),
                "previous report {} has schema {:?}; expected {}",
                path.display(),
                schema.unwrap_or("none"),
                report::SCHEMA
            );
            ensure!(value.get("scope") == backlog.get("scope"),
                "previous report {} has a different workspace scope; compare the same scope or start without --previous",
                path.display());
            Some(value)
        }
        None => None,
    };
    let mut fates = BTreeMap::new();
    if let Some(previous) = &previous {
        let anchors: BTreeMap<String, String> = previous
            .get("projects")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|project| {
                Some((
                    project.get("project")?.as_str()?.to_string(),
                    project.get("anchor")?.as_str()?.to_string(),
                ))
            })
            .collect();
        let mut wanted: BTreeMap<(String, String), Vec<(String, String)>> = BTreeMap::new();
        for (project, file, dir) in report::departed(&backlog, previous) {
            let (Some(dir), Some(anchor)) = (dir, anchors.get(&project).cloned()) else {
                continue;
            };
            wanted
                .entry((dir, anchor))
                .or_default()
                .push((project, file));
        }
        for ((dir, anchor), files) in wanted {
            let dir = PathBuf::from(dir);
            let anchor = PathBuf::from(anchor);
            let observation = (|| -> Result<_> {
                let facts = crate::journal::reconciliation_facts(&dir)?;
                let mut entries = Vec::new();
                for archived in [false, true] {
                    entries.extend(crate::journal::inventory_artifacts(
                        ctx, &dir, &anchor, archived,
                    )?);
                }
                Ok((facts, entries))
            })();
            let (facts, entries) = match observation {
                Ok(observation) => observation,
                Err(error) => {
                    let projects: BTreeSet<_> = files.iter().map(|(project, _)| project).collect();
                    for project in projects {
                        failures.push(CollectionFailure {
                            project: project.clone(),
                            anchor: Some(anchor.display().to_string()),
                            component: "departure-journal",
                            reason: format!(
                                "cannot reconcile departures in {}: {error:#}",
                                dir.display()
                            ),
                        });
                    }
                    continue;
                }
            };
            for (project, file) in files {
                if let Some(entry) = entries.iter().find(|entry| entry.file == file) {
                    let reconciled = reconcile_artifact(entry, &facts);
                    fates.insert(
                        (project, file),
                        report::Fate {
                            explanation: reconciled.explanation.to_string(),
                            storage: reconciled.storage,
                            resolution: reconciled.resolution,
                            superseded_by: reconciled.superseded_by,
                        },
                    );
                }
            }
        }
    }
    for failure in failures {
        let collection = &mut collected.backlog.collection;
        if !collection
            .failures
            .iter()
            .any(|prior| prior.project == failure.project)
        {
            if let Some(project) = collected
                .backlog
                .projects
                .iter()
                .find(|project| project.project == failure.project)
            {
                collection.failed += 1;
                if project.is_empty() {
                    collection.empty -= 1;
                } else {
                    collection.non_empty -= 1;
                }
            }
        }
        collection.failures.push(failure);
        collected.partial = true;
    }
    let mut backlog = serde_json::to_value(&collected.backlog)?;
    backlog["observation"]["finished_at"] = chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        .into();
    let report = report::build(&backlog, previous.as_ref(), &fates, &ledgers);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        report.render();
    }
    Ok(if collected.partial { 16 } else { 0 })
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
    let states = store.readable_states()?;
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

/// The attention rules `arc workspace report` can emit, one line each.
pub fn report_rules_help() -> String {
    report::rules_help()
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
    let states = store.readable_states()?;
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

/// `arc workspace report`: the workspace backlog, classified by named rules.
///
/// The backlog states facts; this module applies the rules a reader needs to
/// act on them — which status an artifact is in, which section it belongs to,
/// and which facts deserve attention — so any consumer, model or not, reads the
/// same classification from the same ledger. It is a pure function of one
/// backlog observation, an optional previous report, and the recorded fate of
/// each artifact that left the backlog since that report.
mod report {
    use serde::Serialize;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};

    pub(crate) const SCHEMA: &str = "arc-workspace-report/3";

    /// A decision question open longer than this is flagged `stale-question`.
    pub(crate) const STALE_QUESTION_DAYS: u64 = 7;
    /// A handoff unresolved longer than this is flagged `stale-handoff`.
    pub(crate) const STALE_HANDOFF_DAYS: u64 = 14;
    /// A change open longer than this without a patchset is flagged
    /// `stale-no-patchset`.
    pub(crate) const STALE_NO_PATCHSET_DAYS: u64 = 7;

    /// Every attention rule the report emits. Each variant's doc line is the
    /// rule's entry in `arc workspace report --help`, and its kebab-case name
    /// is the `rule` value in the report.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
    #[serde(rename_all = "kebab-case")]
    pub(crate) enum Rule {
        /// A delivered artifact: every promotion closed, one integrated, and
        /// nothing consumed it
        DeliveredUnconsumed,
        /// A decision question open longer than 7 days
        StaleQuestion,
        /// A handoff unresolved longer than 14 days
        StaleHandoff,
        /// A claim that lapsed and can be reclaimed
        StaleClaim,
        /// A change open longer than 7 days with no patchset
        StaleNoPatchset,
        /// Two or more open changes whose in-force briefs share a plan and
        /// slice, or a body digest
        SharedPlanSlice,
        /// A closed change whose separate worktree is still on disk
        WorktreeOutlivesChange,
        /// Review owed in a project rose since the previous report
        DebtGrew,
        /// A component read failed; the collection is partial
        CollectionFailed,
        /// A registered project whose anchor is gone
        UnreachableAnchor,
        /// Registered journals whose temporary or scratch anchors are gone,
        /// folded into one fact
        UnreachableScratch,
        /// A promoted candidate whose sibling registrations, answering the
        /// same brief version, nobody has judged
        CandidatesUnjudged,
    }

    impl Rule {
        pub(crate) fn name(self) -> String {
            clap::ValueEnum::to_possible_value(&self)
                .map(|value| value.get_name().to_string())
                .unwrap_or_default()
        }
    }

    impl std::fmt::Display for Rule {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.name())
        }
    }

    impl PartialEq<&str> for Rule {
        fn eq(&self, other: &&str) -> bool {
            self.name() == *other
        }
    }

    /// The attention rules as `arc workspace report --help` lists them, one
    /// line each.
    pub(crate) fn rules_help() -> String {
        let rules: Vec<_> = <Rule as clap::ValueEnum>::value_variants()
            .iter()
            .filter_map(clap::ValueEnum::to_possible_value)
            .collect();
        let width = rules
            .iter()
            .map(|rule| rule.get_name().len())
            .max()
            .unwrap_or(0);
        let mut help = String::from("Attention rules:\n");
        for rule in rules {
            let line = rule.get_help().map(ToString::to_string).unwrap_or_default();
            help.push_str(&format!("  {:width$}  {line}\n", rule.get_name()));
        }
        help
    }

    /// What one project's ledger says that the backlog observation does not:
    /// how long each open change has been open, what its in-force brief
    /// answers, and which closed changes still have their worktree on disk.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub(crate) struct LedgerFacts {
        /// Open change id -> (title, whole days open at the observation).
        pub open: BTreeMap<String, (String, u64)>,
        /// Open change id -> the keys of its in-force brief, for each open
        /// change that has one.
        pub open_briefs: BTreeMap<String, BriefKeys>,
        /// (change id, worktree path) for each closed change whose recorded
        /// worktree still exists.
        pub closed_worktrees: Vec<(String, String)>,
        /// (destination change, promoted candidate, unjudged siblings) for
        /// each promoted candidate with a sibling nobody has judged.
        pub unjudged_siblings: Vec<(String, String, Vec<String>)>,
    }

    /// What identifies the brief a change answers: its plan file and slice
    /// when both are recorded, and the `sha256:` digest of its body.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct BriefKeys {
        pub plan: Option<(String, String)>,
        pub digest: String,
    }

    /// Scaffold headings arc prepends to artifacts. A row titled by one of them
    /// names the template, not the artifact, so the topic titles it instead.
    const SCAFFOLD_HEADINGS: &[&str] = &[
        "How to append a position",
        "Positions",
        "How it resolves",
        "Questions only a person settles",
        "The question",
    ];

    /// Where an artifact that left the backlog went, as the journal records it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Fate {
        /// `present`, `terminal`, `archived`, or `superseded`, as
        /// `workspace inventory` explains it.
        pub explanation: String,
        /// `hot` or `archived`.
        pub storage: String,
        pub resolution: Option<String>,
        pub superseded_by: Option<String>,
    }

    #[derive(Debug, Serialize)]
    pub(crate) struct Report {
        schema: &'static str,
        observation: Value,
        scope: Value,
        collection: Value,
        /// The compared report's `observation.finished_at`, or null.
        previous: Option<String>,
        tallies: BTreeMap<&'static str, Tally>,
        sections: Sections,
        attention: Vec<Attention>,
        projects: Vec<ProjectSummary>,
    }

    #[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Tally {
        value: u64,
        previous: Option<u64>,
    }

    #[derive(Debug, Serialize, Default)]
    struct Sections {
        needs_person: Vec<QuestionRow>,
        needs_agent: Vec<QuestionRow>,
        in_flight: Vec<ChangeRow>,
        review_owed: Vec<DebtRow>,
        deferred: Vec<DeferredRow>,
        work: Vec<Row>,
        proposals: Vec<Row>,
        parked: Vec<Row>,
        departed_since_previous: Vec<DepartedRow>,
    }

    /// One journal artifact, in whichever section its status places it.
    #[derive(Debug, Serialize, Clone, PartialEq)]
    pub(crate) struct Row {
        project: String,
        file: String,
        topic: String,
        kind: String,
        title: String,
        filed_at: Option<String>,
        age_days: Option<u64>,
        status: &'static str,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        flags: Vec<Rule>,
        #[serde(skip_serializing_if = "Option::is_none")]
        claimed_by: Option<String>,
        new_since_previous: Option<bool>,
        path: String,
    }

    #[derive(Debug, Serialize)]
    struct QuestionRow {
        project: String,
        file: String,
        question: String,
        text: String,
        placement: String,
        asked_at: Option<String>,
        age_days: Option<u64>,
        options: Vec<Value>,
        delivery: Option<String>,
        settle_by: String,
    }

    #[derive(Debug, Serialize)]
    struct ChangeRow {
        project: String,
        change_id: String,
        title: String,
        /// Every inbox predicate that holds, plus `no-patchset` when applicable.
        buckets: Vec<String>,
        next_actors: Vec<String>,
        /// Whole days since the change opened, when its ledger was read.
        age_days: Option<u64>,
    }

    #[derive(Debug, Serialize)]
    struct DebtRow {
        project: String,
        change_id: String,
        title: String,
        kind: String,
        age_days: Option<u64>,
        implementer: Option<String>,
    }

    #[derive(Debug, Serialize)]
    struct DeferredRow {
        project: String,
        id: String,
        summary: String,
        subject: String,
        why: String,
    }

    #[derive(Debug, Serialize, PartialEq)]
    struct DepartedRow {
        project: String,
        file: String,
        kind: String,
        title: String,
        /// `consumed`, `archived`, `superseded`, `unobserved`,
        /// or `unknown`. Never inferred from absence alone.
        reason: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        superseded_by: Option<String>,
    }

    #[derive(Debug, Serialize, PartialEq)]
    pub(crate) struct Attention {
        rule: Rule,
        project: String,
        subject: String,
        evidence: String,
    }

    #[derive(Debug, Serialize)]
    struct ProjectSummary {
        project: String,
        anchor: Option<String>,
        journal_dir: Option<String>,
        work: u64,
        proposals: u64,
        parked: u64,
        in_flight: u64,
        review_owed: u64,
        questions: u64,
    }

    /// One `shared-plan-slice` fact per group of two or more open changes
    /// sharing a plan and slice, or a brief digest. A group that shares both
    /// keys is one fact naming both.
    fn shared_briefs(project: &str, briefs: &BTreeMap<String, BriefKeys>) -> Vec<Attention> {
        let mut by_key: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for (change_id, keys) in briefs {
            if let Some((plan, slice)) = &keys.plan {
                by_key
                    .entry(format!("plan `{plan}` slice `{slice}`"))
                    .or_default()
                    .insert(change_id);
            }
            by_key
                .entry(format!("brief digest `{}`", keys.digest))
                .or_default()
                .insert(change_id);
        }
        let mut groups: BTreeMap<BTreeSet<&str>, Vec<String>> = BTreeMap::new();
        for (key, members) in by_key.into_iter().filter(|(_, members)| members.len() > 1) {
            groups.entry(members).or_default().push(key);
        }
        groups
            .into_iter()
            .map(|(members, keys)| {
                let ids = members.into_iter().collect::<Vec<_>>().join(", ");
                Attention {
                    rule: Rule::SharedPlanSlice,
                    project: project.to_string(),
                    evidence: format!("open changes {ids} share {}", keys.join(" and ")),
                    subject: ids,
                }
            })
            .collect()
    }

    fn text(value: &Value, key: &str) -> String {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    fn opt_text(value: &Value, key: &str) -> Option<String> {
        value.get(key).and_then(Value::as_str).map(str::to_string)
    }

    fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The row title: the artifact's heading without its `#` marks, unless the
    /// heading is absent, a scaffold heading, or a position block's, in which
    /// case the topic with hyphens as spaces.
    fn title(item: &Value) -> String {
        let heading = text(item, "heading");
        let heading = heading.trim_start_matches('#').trim();
        let scaffold = heading.is_empty()
            || heading == "(none)"
            || SCAFFOLD_HEADINGS.contains(&heading)
            || heading.starts_with("Position ");
        if scaffold {
            text(item, "topic").replace('-', " ")
        } else {
            heading.to_string()
        }
    }

    /// The status rule, applied in this order:
    /// `delivered` when every promotion is closed and at least one closed
    /// integrated; `abandoned-promotion` when every promotion is closed and none
    /// integrated; `claimed` when an active claim occupies the artifact;
    /// otherwise the tier's own status — `unresolved`, `proposal`, or `parked`.
    fn status(item: &Value, tier: &str) -> &'static str {
        let promotions = array(item, "promotions");
        let all_closed = !promotions.is_empty()
            && promotions
                .iter()
                .all(|promotion| promotion.get("status").and_then(Value::as_str) == Some("closed"));
        if all_closed {
            let integrated = promotions.iter().any(|promotion| {
                promotion
                    .pointer("/closure/outcome")
                    .and_then(Value::as_str)
                    == Some("integrated")
            });
            return if integrated {
                "delivered"
            } else {
                "abandoned-promotion"
            };
        }
        if item.get("availability").and_then(Value::as_str) == Some("occupied") {
            return "claimed";
        }
        match tier {
            "later" => "parked",
            "feature_requests" => "proposal",
            _ => "unresolved",
        }
    }

    fn claimed_by(item: &Value) -> Option<String> {
        array(item, "claims").iter().find_map(|claim| {
            claim
                .pointer("/owner/actor")
                .or_else(|| claim.get("actor"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    }

    fn age_days(seconds: Option<u64>) -> Option<u64> {
        seconds.map(|seconds| seconds / 86_400)
    }

    fn kind_order(kind: &str) -> u8 {
        match kind {
            "handoff" => 0,
            "plan" => 1,
            "todo" => 2,
            "discussion" => 3,
            _ => 4,
        }
    }

    fn previous_rows(previous: &Value) -> BTreeMap<(String, String), (String, String)> {
        let mut rows = BTreeMap::new();
        for section in ["work", "proposals", "parked"] {
            for row in previous
                .pointer(&format!("/sections/{section}"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                rows.insert(
                    (text(row, "project"), text(row, "file")),
                    (text(row, "kind"), text(row, "title")),
                );
            }
        }
        rows
    }

    fn previous_project_debt(previous: &Value) -> BTreeMap<String, u64> {
        array(previous, "projects")
            .iter()
            .map(|project| {
                (
                    text(project, "project"),
                    project
                        .get("review_owed")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                )
            })
            .collect()
    }

    fn failed_projects(report: &Value) -> BTreeSet<String> {
        array(report.get("collection").unwrap_or(&Value::Null), "failures")
            .iter()
            .map(|failure| text(failure, "project"))
            .collect()
    }

    /// Every artifact that left the backlog since `previous`, keyed by project
    /// and file, with the journal directory that holds it. The caller looks each
    /// one up in the journal so its reason is recorded, not inferred.
    pub(crate) fn departed(
        backlog: &Value,
        previous: &Value,
    ) -> Vec<(String, String, Option<String>)> {
        let current: BTreeSet<(String, String)> = array(backlog, "projects")
            .iter()
            .flat_map(|project| {
                let name = text(project, "project");
                ["open", "later", "feature_requests"]
                    .into_iter()
                    .flat_map(move |tier| {
                        array(project.get("items").unwrap_or(&Value::Null), tier).to_vec()
                    })
                    .map(move |item| (name.clone(), text(&item, "file")))
            })
            .collect();
        let dirs: BTreeMap<String, String> = array(previous, "projects")
            .iter()
            .filter_map(|project| {
                Some((text(project, "project"), opt_text(project, "journal_dir")?))
            })
            .collect();
        previous_rows(previous)
            .into_keys()
            .filter(|key| !current.contains(key))
            .map(|(project, file)| {
                let dir = dirs.get(&project).cloned();
                (project, file, dir)
            })
            .collect()
    }

    /// Classify one backlog observation (`arc-workspace-backlog`, collected with
    /// `--items`) into the report.
    pub(crate) fn build(
        backlog: &Value,
        previous: Option<&Value>,
        fates: &BTreeMap<(String, String), Fate>,
        ledgers: &BTreeMap<String, LedgerFacts>,
    ) -> Report {
        let known_before = previous.map(previous_rows);
        let failed_now = failed_projects(backlog);
        let failed_before = previous.map(failed_projects).unwrap_or_default();
        let mut sections = Sections::default();
        let mut attention = Vec::new();
        let mut projects = Vec::new();

        for project in array(backlog, "projects") {
            let name = text(project, "project");
            let journal_dir = opt_text(project, "journal_dir");
            let items = project.get("items").unwrap_or(&Value::Null);
            let mut summary = ProjectSummary {
                project: name.clone(),
                anchor: opt_text(project, "anchor"),
                journal_dir: journal_dir.clone(),
                work: 0,
                proposals: 0,
                parked: 0,
                in_flight: 0,
                review_owed: 0,
                questions: 0,
            };

            for tier in ["open", "later", "feature_requests"] {
                for item in array(items, tier) {
                    let status = status(item, tier);
                    let age = age_days(item.get("age_seconds").and_then(Value::as_u64));
                    let kind = text(item, "kind");
                    let mut flags = Vec::new();
                    if status == "delivered" {
                        flags.push(Rule::DeliveredUnconsumed);
                    }
                    if kind == "handoff"
                        && tier == "open"
                        && age.is_some_and(|days| days > STALE_HANDOFF_DAYS)
                    {
                        flags.push(Rule::StaleHandoff);
                    }
                    if item.get("availability").and_then(Value::as_str) == Some("reclaimable") {
                        flags.push(Rule::StaleClaim);
                    }
                    let file = text(item, "file");
                    let row = Row {
                        project: name.clone(),
                        file: file.clone(),
                        topic: text(item, "topic"),
                        kind: kind.clone(),
                        title: title(item),
                        filed_at: opt_text(item, "filed_at"),
                        age_days: age,
                        status,
                        flags: flags.clone(),
                        claimed_by: (status == "claimed").then(|| claimed_by(item)).flatten(),
                        new_since_previous: known_before
                            .as_ref()
                            .filter(|_| {
                                !failed_before.contains(&name) && !failed_now.contains(&name)
                            })
                            .map(|known| !known.contains_key(&(name.clone(), file.clone()))),
                        path: journal_dir
                            .as_ref()
                            .map(|dir| format!("{dir}/{file}"))
                            .unwrap_or_else(|| file.clone()),
                    };
                    for flag in &flags {
                        attention.push(Attention {
                            rule: *flag,
                            project: name.clone(),
                            subject: file.clone(),
                            evidence: match flag {
                                Rule::DeliveredUnconsumed => {
                                    "every promotion closed and one integrated; the artifact was never consumed".to_string()
                                }
                                Rule::StaleHandoff => format!("handoff unresolved for {} days", age.unwrap_or(0)),
                                _ => "its claim lapsed and can be reclaimed".to_string(),
                            },
                        });
                    }
                    match tier {
                        "later" => {
                            summary.parked += 1;
                            sections.parked.push(row);
                        }
                        "feature_requests" => {
                            summary.proposals += 1;
                            sections.proposals.push(row);
                        }
                        _ => {
                            summary.work += 1;
                            sections.work.push(row);
                        }
                    }
                }
            }

            for question in array(project, "open_questions") {
                if question.get("disposition").and_then(Value::as_str) != Some("open") {
                    continue;
                }
                let asked_at = opt_text(question, "asked_at");
                let observed = backlog
                    .pointer("/observation/finished_at")
                    .and_then(Value::as_str);
                let age = match (&asked_at, observed) {
                    (Some(asked), Some(observed)) => chrono::DateTime::parse_from_rfc3339(asked)
                        .ok()
                        .zip(chrono::DateTime::parse_from_rfc3339(observed).ok())
                        .map(|(asked, observed)| {
                            u64::try_from((observed - asked).num_days()).unwrap_or(0)
                        }),
                    _ => None,
                };
                if age.is_some_and(|days| days > STALE_QUESTION_DAYS) {
                    attention.push(Attention {
                        rule: Rule::StaleQuestion,
                        project: name.clone(),
                        subject: text(question, "question"),
                        evidence: format!(
                            "open for {} days on {}",
                            age.unwrap_or(0),
                            text(question, "file")
                        ),
                    });
                }
                summary.questions += 1;
                let settle_by = opt_text(question, "settle_by").unwrap_or_else(|| "person".into());
                let target = if settle_by == "person" {
                    &mut sections.needs_person
                } else {
                    &mut sections.needs_agent
                };
                target.push(QuestionRow {
                    project: name.clone(),
                    file: text(question, "file"),
                    question: text(question, "question"),
                    text: text(question, "heading"),
                    placement: text(question, "placement"),
                    asked_at,
                    age_days: age,
                    options: array(question, "options").to_vec(),
                    delivery: opt_text(question, "delivery"),
                    settle_by,
                });
            }

            let changes = project.get("changes").unwrap_or(&Value::Null);
            let empty = LedgerFacts::default();
            let ledger = ledgers.get(&name).unwrap_or(&empty);
            let mut in_flight = BTreeMap::<String, ChangeRow>::new();
            if let Some(buckets) = changes.as_object() {
                for (bucket, rows) in buckets {
                    if matches!(
                        bucket.as_str(),
                        "schema" | "debt-owed" | "debt-owed-by-kind" | "deferred"
                    ) {
                        continue;
                    }
                    for row in rows.as_array().into_iter().flatten() {
                        let change_id = text(row, "change_id");
                        let change =
                            in_flight
                                .entry(change_id.clone())
                                .or_insert_with(|| ChangeRow {
                                    project: name.clone(),
                                    age_days: ledger.open.get(&change_id).map(|(_, days)| *days),
                                    change_id,
                                    title: text(row, "title"),
                                    buckets: Vec::new(),
                                    next_actors: Vec::new(),
                                });
                        change.buckets.push(bucket.clone());
                        if let Some(actor) = opt_text(row, "next_actor") {
                            change.next_actors.push(actor);
                        }
                    }
                }
            }
            for change in array(project, "no_patchset") {
                let id = change.as_str().unwrap_or("").to_string();
                let opened = ledger.open.get(&id);
                if let Some((_, days)) = opened.filter(|(_, days)| *days > STALE_NO_PATCHSET_DAYS) {
                    attention.push(Attention {
                        rule: Rule::StaleNoPatchset,
                        project: name.clone(),
                        subject: id.clone(),
                        evidence: format!("open for {days} days with no patchset recorded"),
                    });
                }
                in_flight
                    .entry(id.clone())
                    .or_insert_with(|| ChangeRow {
                        project: name.clone(),
                        title: opened.map(|(title, _)| title.clone()).unwrap_or_default(),
                        age_days: opened.map(|(_, days)| *days),
                        change_id: id,
                        buckets: Vec::new(),
                        next_actors: Vec::new(),
                    })
                    .buckets
                    .push("no-patchset".into());
            }
            summary.in_flight = in_flight.len() as u64;
            for mut change in in_flight.into_values() {
                change.buckets.sort();
                change.buckets.dedup();
                change.next_actors.sort();
                change.next_actors.dedup();
                sections.in_flight.push(change);
            }
            attention.extend(shared_briefs(&name, &ledger.open_briefs));
            for (change_id, promoted, unjudged) in &ledger.unjudged_siblings {
                attention.push(Attention {
                    rule: Rule::CandidatesUnjudged,
                    project: name.clone(),
                    subject: change_id.clone(),
                    evidence: format!(
                        "candidate {promoted} was promoted; its sibling(s) {} {} unjudged",
                        unjudged.join(", "),
                        if unjudged.len() == 1 { "is" } else { "are" }
                    ),
                });
            }
            for (change_id, path) in &ledger.closed_worktrees {
                attention.push(Attention {
                    rule: Rule::WorktreeOutlivesChange,
                    project: name.clone(),
                    subject: change_id.clone(),
                    evidence: format!("the change is closed and its worktree is still at {path}"),
                });
            }

            let debt_titles: BTreeMap<String, String> = array(changes, "debt-owed")
                .iter()
                .map(|row| (text(row, "change_id"), text(row, "title")))
                .collect();
            for debt in array(project, "debt_owed") {
                let change_id = text(debt, "change_id");
                summary.review_owed += 1;
                sections.review_owed.push(DebtRow {
                    project: name.clone(),
                    title: debt_titles.get(&change_id).cloned().unwrap_or_default(),
                    change_id,
                    kind: text(debt, "effective_missing"),
                    age_days: debt.get("age_days").and_then(Value::as_u64),
                    implementer: debt
                        .pointer("/production/implementer/model")
                        .or_else(|| debt.pointer("/production/implementer/actor"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
            for deferred in array(changes, "deferred") {
                sections.deferred.push(DeferredRow {
                    project: name.clone(),
                    id: text(deferred, "id"),
                    summary: text(deferred, "summary"),
                    subject: text(deferred, "subject"),
                    why: text(deferred, "why"),
                });
            }

            if let Some(before) = previous.map(previous_project_debt) {
                let earlier = before.get(&name).copied().unwrap_or(0);
                if summary.review_owed > earlier
                    && !failed_before.contains(&name)
                    && !failed_now.contains(&name)
                {
                    attention.push(Attention {
                        rule: Rule::DebtGrew,
                        project: name.clone(),
                        subject: name.clone(),
                        evidence: format!(
                            "review owed rose from {earlier} to {}",
                            summary.review_owed
                        ),
                    });
                }
            }
            projects.push(summary);
        }

        // A vanished anchor is reported once, as unreachable; the collection
        // failure it also produces names the same fact.
        for failure in array(
            backlog.get("collection").unwrap_or(&Value::Null),
            "failures",
        ) {
            if text(failure, "component") == "anchor" {
                continue;
            }
            attention.push(Attention {
                rule: Rule::CollectionFailed,
                project: text(failure, "project"),
                subject: text(failure, "component"),
                evidence: text(failure, "reason"),
            });
        }
        let mut scratch = 0usize;
        for unreachable in array(backlog, "unreachable") {
            let anchor = text(unreachable, "anchor");
            if !anchor.is_empty()
                && super::is_temporary_or_scratch_anchor(std::path::Path::new(&anchor))
            {
                scratch += 1;
                continue;
            }
            attention.push(Attention {
                rule: Rule::UnreachableAnchor,
                project: text(unreachable, "slug"),
                subject: anchor,
                evidence: text(unreachable, "reason"),
            });
        }
        if scratch > 0 {
            attention.push(Attention {
                rule: Rule::UnreachableScratch,
                project: String::new(),
                subject: format!("{scratch} journals"),
                evidence: "registered journals whose temporary or scratch anchors are gone; housekeeping, not lost work".to_string(),
            });
        }

        if let Some(previous) = previous {
            let failed: BTreeSet<String> = array(
                backlog.get("collection").unwrap_or(&Value::Null),
                "failures",
            )
            .iter()
            .map(|failure| text(failure, "project"))
            .collect();
            let before = previous_rows(previous);
            for (project, file, _) in departed(backlog, previous) {
                let (kind, title) = before
                    .get(&(project.clone(), file.clone()))
                    .cloned()
                    .unwrap_or_default();
                let fate = fates.get(&(project.clone(), file.clone()));
                let (reason, outcome, superseded_by) = match fate {
                    _ if failed.contains(&project) => ("unobserved", None, None),
                    // Shelving records the resolution `unresolved` and moves the
                    // artifact to the archive; only a consumption is `consumed`.
                    Some(fate) if fate.explanation == "superseded" => (
                        "superseded",
                        fate.resolution.clone(),
                        fate.superseded_by.clone(),
                    ),
                    Some(fate)
                        if fate.storage == "archived"
                            && fate
                                .resolution
                                .as_deref()
                                .is_none_or(|outcome| outcome == "unresolved") =>
                    {
                        ("archived", fate.resolution.clone(), None)
                    }
                    Some(fate) if fate.explanation == "terminal" => {
                        ("consumed", fate.resolution.clone(), None)
                    }
                    Some(fate) if fate.explanation == "archived" => {
                        ("archived", fate.resolution.clone(), None)
                    }
                    _ => ("unknown", None, None),
                };
                sections.departed_since_previous.push(DepartedRow {
                    project,
                    file,
                    kind,
                    title,
                    reason,
                    outcome,
                    superseded_by,
                });
            }
        }

        let order = |row: &Row| {
            (
                row.project.clone(),
                kind_order(&row.kind),
                row.filed_at.clone().unwrap_or_default(),
                row.file.clone(),
            )
        };
        sections.work.sort_by_key(order);
        sections.proposals.sort_by_key(order);
        sections.parked.sort_by_key(order);
        sections
            .in_flight
            .sort_by(|a, b| (&a.project, &a.change_id).cmp(&(&b.project, &b.change_id)));
        sections
            .review_owed
            .sort_by(|a, b| (&a.project, &a.change_id).cmp(&(&b.project, &b.change_id)));
        sections.needs_person.sort_by(|a, b| {
            (&a.project, &a.asked_at, &a.question).cmp(&(&b.project, &b.asked_at, &b.question))
        });
        sections.needs_agent.sort_by(|a, b| {
            (&a.project, &a.asked_at, &a.question).cmp(&(&b.project, &b.asked_at, &b.question))
        });
        projects.sort_by(|a, b| a.project.cmp(&b.project));
        attention.sort_by_cached_key(|a| (a.rule.name(), a.project.clone(), a.subject.clone()));

        let previous_tally = |key: &str| {
            previous
                .filter(|_| failed_now.is_empty() && failed_before.is_empty())
                .and_then(|previous| {
                    previous
                        .pointer(&format!("/tallies/{key}/value"))
                        .and_then(Value::as_u64)
                })
        };
        let count = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
        let mut tallies = BTreeMap::new();
        let delivered = sections
            .work
            .iter()
            .chain(&sections.proposals)
            .chain(&sections.parked)
            .filter(|row| row.status == "delivered")
            .count();
        for (key, value) in [
            ("needs_person", count(sections.needs_person.len())),
            ("needs_agent", count(sections.needs_agent.len())),
            ("in_flight", count(sections.in_flight.len())),
            ("review_owed", count(sections.review_owed.len())),
            ("deferred", count(sections.deferred.len())),
            ("work", count(sections.work.len())),
            ("proposals", count(sections.proposals.len())),
            ("parked", count(sections.parked.len())),
            ("delivered", count(delivered)),
            ("projects", count(projects.len())),
            ("attention", count(attention.len())),
        ] {
            tallies.insert(
                key,
                Tally {
                    value,
                    previous: previous_tally(key),
                },
            );
        }

        Report {
            schema: SCHEMA,
            observation: backlog.get("observation").cloned().unwrap_or(Value::Null),
            scope: backlog.get("scope").cloned().unwrap_or(Value::Null),
            collection: backlog.get("collection").cloned().unwrap_or(Value::Null),
            previous: previous.and_then(|previous| {
                previous
                    .pointer("/observation/finished_at")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
            tallies,
            sections,
            attention,
            projects,
        }
    }

    impl Report {
        /// A compact text summary; `--json` carries every row.
        pub(crate) fn render(&self) {
            let tally = |key: &str| {
                self.tallies
                    .get(key)
                    .map_or_else(String::new, |tally| match tally.previous {
                        Some(previous) if previous != tally.value => {
                            format!("{} (was {previous})", tally.value)
                        }
                        _ => tally.value.to_string(),
                    })
            };
            println!(
                "needs a person {}  agent-answerable {}  in flight {}  review owed {}  deferred {}",
                tally("needs_person"),
                tally("needs_agent"),
                tally("in_flight"),
                tally("review_owed"),
                tally("deferred")
            );
            println!(
                "work {}  proposals {}  parked {}  delivered but unconsumed {}  across {} projects",
                tally("work"),
                tally("proposals"),
                tally("parked"),
                tally("delivered"),
                tally("projects")
            );
            if let Some(previous) = &self.previous {
                println!(
                    "since {previous}: {} new, {} departed",
                    self.sections
                        .work
                        .iter()
                        .chain(&self.sections.proposals)
                        .chain(&self.sections.parked)
                        .filter(|row| row.new_since_previous == Some(true))
                        .count(),
                    self.sections.departed_since_previous.len()
                );
            }
            for entry in &self.attention {
                println!(
                    "attention  {}  {}  {}  {}",
                    entry.rule, entry.project, entry.subject, entry.evidence
                );
            }
            println!("detail: arc workspace report --json");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        fn item(file: &str, kind: &str, heading: &str) -> Value {
            json!({
                "file": file,
                "topic": file.trim_end_matches(".md"),
                "kind": kind,
                "heading": heading,
                "filed_at": "2026-09-01T00:00:00Z",
                "age_seconds": 20 * 86_400,
                "availability": "available",
                "promotions": [],
            })
        }

        fn backlog(open: Vec<Value>, feature_requests: Vec<Value>) -> Value {
            json!({
                "schema": "arc-workspace-backlog/19",
                "observation": {"started_at": "2026-09-26T00:00:00Z", "finished_at": "2026-09-26T00:00:01Z"},
                "scope": {"mode": "global"},
                "collection": {"failures": []},
                "projects": [{
                    "project": "demo",
                    "anchor": "/code/demo",
                    "journal_dir": "/journals/demo",
                    "items": {"open": open, "later": [], "feature_requests": feature_requests},
                    "open_questions": [],
                    "changes": {},
                    "no_patchset": [],
                    "debt_owed": [],
                }],
                "unreachable": [],
            })
        }

        fn rows(report: &Report) -> Vec<&Row> {
            report
                .sections
                .work
                .iter()
                .chain(&report.sections.proposals)
                .collect()
        }

        #[test]
        fn a_delivered_plan_reads_delivered_and_is_flagged() {
            let mut plan = item("plan.md", "plan", "# Plan");
            plan["promotions"] = json!([
                {"status": "closed", "closure": {"outcome": "integrated"}},
                {"status": "closed", "closure": {"outcome": "abandoned"}},
            ]);
            let mut abandoned = item("gone.md", "plan", "# Gone");
            abandoned["promotions"] =
                json!([{"status": "closed", "closure": {"outcome": "abandoned"}}]);
            let report = build(
                &backlog(vec![plan, abandoned], vec![]),
                None,
                &BTreeMap::new(),
                &BTreeMap::new(),
            );
            let statuses: Vec<_> = rows(&report)
                .iter()
                .map(|row| (row.file.as_str(), row.status))
                .collect();
            assert!(statuses.contains(&("plan.md", "delivered")), "{statuses:?}");
            assert!(
                statuses.contains(&("gone.md", "abandoned-promotion")),
                "{statuses:?}"
            );
            assert!(report
                .attention
                .iter()
                .any(|entry| entry.rule == "delivered-unconsumed" && entry.subject == "plan.md"));
            assert_eq!(report.tallies["delivered"].value, 1);
        }

        #[test]
        fn an_unresolved_record_is_never_new_work_and_a_scaffold_heading_yields_the_topic() {
            let restored = item("restored.md", "todo", "# Restored from the archive");
            let scaffolded = item("argue-it.md", "discussion", "## How to append a position");
            let report = build(
                &backlog(vec![restored, scaffolded], vec![]),
                None,
                &BTreeMap::new(),
                &BTreeMap::new(),
            );
            for row in rows(&report) {
                assert_eq!(row.status, "unresolved", "{row:?}");
                assert_eq!(
                    row.new_since_previous, None,
                    "no previous report, no novelty claim"
                );
            }
            let titles: Vec<_> = rows(&report).iter().map(|row| row.title.as_str()).collect();
            assert!(titles.contains(&"argue it"), "{titles:?}");
            assert!(titles.contains(&"Restored from the archive"), "{titles:?}");
        }

        #[test]
        fn a_claimed_record_names_its_holder_and_a_lapsed_claim_is_flagged() {
            let mut held = item("held.md", "todo", "# Held");
            held["availability"] = json!("occupied");
            held["claims"] = json!([{"owner": {"actor": "someone"}}]);
            let mut lapsed = item("lapsed.md", "todo", "# Lapsed");
            lapsed["availability"] = json!("reclaimable");
            let report = build(
                &backlog(vec![held, lapsed], vec![]),
                None,
                &BTreeMap::new(),
                &BTreeMap::new(),
            );
            let held = rows(&report)
                .into_iter()
                .find(|row| row.file == "held.md")
                .unwrap();
            assert_eq!(held.status, "claimed");
            assert_eq!(held.claimed_by.as_deref(), Some("someone"));
            assert!(report
                .attention
                .iter()
                .any(|entry| entry.rule == "stale-claim"));
        }

        #[test]
        fn departures_carry_their_recorded_reason_and_a_failed_project_reads_unobserved() {
            let first = build(
                &backlog(
                    vec![
                        item("done.md", "todo", "# Done"),
                        item("shelved.md", "todo", "# Shelved"),
                        item("mystery.md", "todo", "# Mystery"),
                    ],
                    vec![],
                ),
                None,
                &BTreeMap::new(),
                &BTreeMap::new(),
            );
            let previous = serde_json::to_value(&first).unwrap();
            let now = backlog(vec![item("fresh.md", "todo", "# Fresh")], vec![]);
            let departed_files: Vec<_> = departed(&now, &previous)
                .into_iter()
                .map(|(_, file, dir)| (file, dir))
                .collect();
            assert!(departed_files
                .contains(&("done.md".to_string(), Some("/journals/demo".to_string()))));
            let mut fates = BTreeMap::new();
            fates.insert(
                ("demo".to_string(), "done.md".to_string()),
                Fate {
                    explanation: "terminal".into(),
                    storage: "hot".into(),
                    resolution: Some("done".into()),
                    superseded_by: None,
                },
            );
            fates.insert(
                ("demo".to_string(), "shelved.md".to_string()),
                Fate {
                    explanation: "terminal".into(),
                    storage: "archived".into(),
                    resolution: Some("unresolved".into()),
                    superseded_by: None,
                },
            );
            let report = build(&now, Some(&previous), &fates, &BTreeMap::new());
            let reason = |file: &str| {
                report
                    .sections
                    .departed_since_previous
                    .iter()
                    .find(|row| row.file == file)
                    .map(|row| (row.reason, row.outcome.clone()))
            };
            assert_eq!(
                reason("done.md"),
                Some(("consumed", Some("done".to_string())))
            );
            assert_eq!(
                reason("shelved.md"),
                Some(("archived", Some("unresolved".to_string())))
            );
            assert_eq!(
                reason("mystery.md"),
                Some(("unknown", None)),
                "absence alone establishes nothing"
            );
            let fresh = report
                .sections
                .work
                .iter()
                .find(|row| row.file == "fresh.md")
                .unwrap();
            assert_eq!(fresh.new_since_previous, Some(true));
            assert_eq!(
                report.tallies["work"],
                Tally {
                    value: 1,
                    previous: Some(3)
                }
            );

            let mut failed = now.clone();
            failed["collection"]["failures"] =
                json!([{"project": "demo", "component": "journal", "reason": "unreadable"}]);
            let report = build(&failed, Some(&previous), &fates, &BTreeMap::new());
            assert!(report
                .sections
                .departed_since_previous
                .iter()
                .all(|row| row.reason == "unobserved"));
            assert!(report
                .attention
                .iter()
                .any(|entry| entry.rule == "collection-failed"));
        }

        #[test]
        fn a_vanished_anchor_is_reported_once_and_scratch_anchors_fold_together() {
            let mut data = backlog(vec![], vec![]);
            let scratch = std::env::temp_dir().join("probe-fixture");
            data["collection"]["failures"] = json!([
                {"project": "kept", "component": "anchor", "reason": "anchor does not exist"},
                {"project": "demo", "component": "journal", "reason": "unreadable"},
            ]);
            data["unreachable"] = json!([
                {"slug": "kept", "anchor": "/code/kept", "reason": "anchor does not exist"},
                {"slug": "probe-a", "anchor": scratch.join("a").to_str().unwrap(), "reason": "anchor does not exist"},
                {"slug": "probe-b", "anchor": scratch.join("b").to_str().unwrap(), "reason": "anchor does not exist"},
            ]);
            let report = build(&data, None, &BTreeMap::new(), &BTreeMap::new());
            let rules: Vec<_> = report
                .attention
                .iter()
                .map(|entry| (entry.rule.name(), entry.project.as_str()))
                .collect();
            assert!(
                rules.contains(&("unreachable-anchor".into(), "kept")),
                "{rules:?}"
            );
            assert!(
                !rules.contains(&("collection-failed".into(), "kept")),
                "one fact, one entry: {rules:?}"
            );
            assert!(
                rules.contains(&("collection-failed".into(), "demo")),
                "{rules:?}"
            );
            let scratch = report
                .attention
                .iter()
                .find(|entry| entry.rule == "unreachable-scratch")
                .unwrap();
            assert_eq!(scratch.subject, "2 journals");
        }

        #[test]
        fn a_promoted_candidate_with_unjudged_siblings_is_flagged() {
            let data = backlog(vec![], vec![]);
            let mut facts = LedgerFacts::default();
            facts.unjudged_siblings.push((
                "answer-1".into(),
                "alice".into(),
                vec!["bob".into(), "carol".into()],
            ));
            let mut ledgers = BTreeMap::new();
            ledgers.insert("demo".to_string(), facts);
            let report = build(&data, None, &BTreeMap::new(), &ledgers);
            let flagged = report
                .attention
                .iter()
                .find(|entry| entry.rule == "candidates-unjudged")
                .expect("the rule fires");
            assert_eq!(flagged.subject, "answer-1");
            assert!(flagged.evidence.contains("bob, carol are unjudged"));
        }

        #[test]
        fn ledger_facts_flag_a_stale_no_patchset_change_and_a_surviving_worktree() {
            let mut data = backlog(vec![], vec![]);
            data["projects"][0]["no_patchset"] = json!(["idle-1", "fresh-2"]);
            let mut facts = LedgerFacts::default();
            facts.open.insert("idle-1".into(), ("Idle work".into(), 12));
            facts
                .open
                .insert("fresh-2".into(), ("Fresh work".into(), 1));
            facts
                .closed_worktrees
                .push(("done-3".into(), "/worktrees/done-3".into()));
            let mut ledgers = BTreeMap::new();
            ledgers.insert("demo".to_string(), facts);
            let report = build(&data, None, &BTreeMap::new(), &ledgers);
            let rules: Vec<_> = report
                .attention
                .iter()
                .map(|entry| (entry.rule.name(), entry.subject.as_str()))
                .collect();
            assert!(
                rules.contains(&("stale-no-patchset".into(), "idle-1")),
                "{rules:?}"
            );
            assert!(
                !rules.contains(&("stale-no-patchset".into(), "fresh-2")),
                "{rules:?}"
            );
            assert!(
                rules.contains(&("worktree-outlives-change".into(), "done-3")),
                "{rules:?}"
            );
            let idle = report
                .sections
                .in_flight
                .iter()
                .find(|row| row.change_id == "idle-1")
                .unwrap();
            assert_eq!(idle.title, "Idle work");
            assert_eq!(idle.age_days, Some(12));
        }

        #[test]
        fn debt_growth_against_the_previous_report_is_flagged() {
            let quiet = backlog(vec![], vec![]);
            let previous =
                serde_json::to_value(build(&quiet, None, &BTreeMap::new(), &BTreeMap::new()))
                    .unwrap();
            let mut owing = quiet.clone();
            owing["projects"][0]["debt_owed"] =
                json!([{"change_id": "c-1", "effective_missing": "nothing-read", "age_days": 2}]);
            let report = build(&owing, Some(&previous), &BTreeMap::new(), &BTreeMap::new());
            assert!(report
                .attention
                .iter()
                .any(|entry| entry.rule == "debt-grew" && entry.project == "demo"));
            assert_eq!(report.sections.review_owed[0].kind, "nothing-read");
        }

        #[test]
        fn incomplete_observations_do_not_prove_arrival_or_debt_growth() {
            let mut unread = backlog(vec![], vec![]);
            unread["collection"]["failures"] = json!([
                {"project": "demo", "component": "ledger", "reason": "unreadable"}
            ]);
            let previous =
                serde_json::to_value(build(&unread, None, &BTreeMap::new(), &BTreeMap::new()))
                    .unwrap();
            let mut current = backlog(vec![item("existing.md", "todo", "# Existing")], vec![]);
            current["projects"][0]["debt_owed"] = json!([
                {"change_id": "existing", "effective_missing": "nothing-read"}
            ]);
            let report = build(
                &current,
                Some(&previous),
                &BTreeMap::new(),
                &BTreeMap::new(),
            );
            assert_eq!(report.sections.work[0].new_since_previous, None);
            assert_eq!(report.tallies["work"].previous, None);
            assert!(!report.attention.iter().any(|a| a.rule == "debt-grew"));
        }

        #[test]
        fn a_stale_question_and_a_stale_handoff_are_flagged() {
            let mut data = backlog(vec![item("hand.md", "handoff", "# Hand")], vec![]);
            data["projects"][0]["open_questions"] = json!([{
                "file": "d.md", "question": "q-1", "heading": "Which?", "placement": "closing",
                "asked_at": "2026-09-10T00:00:00Z", "disposition": "open", "options": [], "delivery": "delivered"
            }]);
            let report = build(&data, None, &BTreeMap::new(), &BTreeMap::new());
            assert_eq!(report.sections.needs_person.len(), 1);
            assert_eq!(report.sections.needs_person[0].age_days, Some(16));
            let rules: Vec<_> = report
                .attention
                .iter()
                .map(|entry| entry.rule.name())
                .collect();
            assert!(rules.contains(&"stale-question".to_string()), "{rules:?}");
            assert!(rules.contains(&"stale-handoff".to_string()), "{rules:?}");
        }

        fn keys(plan: Option<(&str, &str)>, digest: &str) -> BriefKeys {
            BriefKeys {
                plan: plan.map(|(plan, slice)| (plan.to_string(), slice.to_string())),
                digest: digest.to_string(),
            }
        }

        #[test]
        fn open_changes_sharing_a_brief_are_one_fact_per_group() {
            let mut briefs = BTreeMap::new();
            briefs.insert("a-1".to_string(), keys(Some(("p.md", "s")), "sha256:x"));
            briefs.insert("b-2".to_string(), keys(Some(("p.md", "s")), "sha256:x"));
            briefs.insert("c-3".to_string(), keys(Some(("p.md", "t")), "sha256:y"));
            briefs.insert("d-4".to_string(), keys(Some(("q.md", "u")), "sha256:y"));
            briefs.insert("e-5".to_string(), keys(Some(("q.md", "v")), "sha256:z"));
            let facts = shared_briefs("demo", &briefs);
            let facts: Vec<_> = facts
                .iter()
                .map(|fact| (fact.subject.as_str(), fact.evidence.as_str()))
                .collect();
            assert_eq!(
                facts,
                vec![
                    (
                        "a-1, b-2",
                        "open changes a-1, b-2 share brief digest `sha256:x` and plan `p.md` slice `s`"
                    ),
                    ("c-3, d-4", "open changes c-3, d-4 share brief digest `sha256:y`"),
                ]
            );
        }

        #[test]
        fn a_plan_without_a_slice_shares_only_by_digest() {
            let mut briefs = BTreeMap::new();
            briefs.insert("a-1".to_string(), keys(None, "sha256:x"));
            briefs.insert("b-2".to_string(), keys(None, "sha256:y"));
            assert!(shared_briefs("demo", &briefs).is_empty());
        }

        #[test]
        fn shared_briefs_reach_the_report_as_attention() {
            let mut facts = LedgerFacts::default();
            for id in ["a-1", "b-2"] {
                facts.open.insert(id.into(), (id.into(), 1));
                facts
                    .open_briefs
                    .insert(id.into(), keys(Some(("p.md", "s")), id));
            }
            let mut ledgers = BTreeMap::new();
            ledgers.insert("demo".to_string(), facts);
            let report = build(&backlog(vec![], vec![]), None, &BTreeMap::new(), &ledgers);
            let shared: Vec<_> = report
                .attention
                .iter()
                .filter(|entry| entry.rule == Rule::SharedPlanSlice)
                .collect();
            assert_eq!(shared.len(), 1, "{:?}", report.attention);
            assert_eq!(shared[0].project, "demo");
            assert_eq!(shared[0].subject, "a-1, b-2");
        }

        #[test]
        fn the_help_lists_every_rule_the_report_emits() {
            let help = rules_help();
            for rule in <Rule as clap::ValueEnum>::value_variants() {
                let name = rule.name();
                let line = help
                    .lines()
                    .find(|line| line.split_whitespace().next() == Some(name.as_str()))
                    .unwrap_or_else(|| panic!("{name} missing from:\n{help}"));
                assert!(line.trim().len() > name.len(), "{name} has no description");
                assert_eq!(
                    serde_json::to_value(rule).unwrap(),
                    Value::String(name.clone()),
                    "the serialized rule and its help name agree"
                );
            }
            assert_eq!(
                help.lines().skip(1).count(),
                <Rule as clap::ValueEnum>::value_variants().len()
            );
            for (line, days) in [
                ("stale-question", STALE_QUESTION_DAYS),
                ("stale-handoff", STALE_HANDOFF_DAYS),
                ("stale-no-patchset", STALE_NO_PATCHSET_DAYS),
            ] {
                let line = help.lines().find(|l| l.contains(line)).unwrap();
                assert!(
                    line.contains(&format!("longer than {days} days")),
                    "{line} states the threshold the rule applies"
                );
            }
        }
    }
}
