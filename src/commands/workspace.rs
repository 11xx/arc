//! Cross-repository aggregation and rebase advice for a lead working across
//! many change ledgers. All read-only: no store is ever created while
//! scanning, and `restack` only prints commands — arc never rewrites branches.

use super::*;
use crate::gates::GatesFile;
use crate::policy::PolicyFile;
use anyhow::ensure;
use serde::Serialize;

pub enum WorkspaceView {
    List,
    Inbox,
    Backlog {
        since: Option<String>,
        items: bool,
        scope: WorkspaceScope,
        show_unreachable: bool,
    },
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

fn repo_states(store: &Store) -> Result<BTreeMap<String, ChangeState>> {
    let mut states = BTreeMap::new();
    let rewrites = store.rewrites()?;
    for change_id in store.list_change_ids()? {
        let events = store.load_events(&change_id)?;
        states.insert(change_id, state::reduce_following(&events, &rewrites)?);
    }
    Ok(states)
}

pub fn workspace(ctx: &Ctx, view: WorkspaceView, json: bool) -> Result<()> {
    if let WorkspaceView::Backlog {
        since,
        items,
        scope,
        show_unreachable,
    } = view
    {
        return workspace_backlog(ctx, since.as_deref(), items, scope, show_unreachable, json);
    }
    let stores = workspace_stores()?;
    match view {
        WorkspaceView::List => workspace_list(&stores, json),
        WorkspaceView::Inbox => workspace_inbox(&stores, json),
        WorkspaceView::Backlog { .. } => unreachable!("handled above"),
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

fn workspace_inbox(stores: &[(String, Store)], json: bool) -> Result<()> {
    // Workspace inbox is ledger-derived: it consults no per-repo working tree,
    // so the derived latest-patchset head stands in for the live branch head
    // and repo-local gate policy is not applied (gate buckets stay empty).
    let gates = GatesFile::default();
    let policy = PolicyFile::default();
    let mut repos = Vec::new();
    for (repo, store) in stores {
        let states = repo_states(store)?;
        let mut inbox = crate::inbox::Inbox::new(None);
        for state in states.values() {
            if state.is_closed() {
                continue;
            }
            let report = status::build_as_of(
                state,
                &gates,
                &policy,
                dependency_status(state, &states),
                changes_blocked_by(&state.change_id, &states),
                chrono::Utc::now(),
                // Workspace aggregation declares no per-repo policy, so no
                // danger list is in play and there is nothing to resolve.
                None,
            )?;
            inbox.absorb(state, &report);
        }
        inbox.sort_by_priority();
        repos.push(RepoInbox {
            repo: repo.clone(),
            inbox,
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
    summary: BacklogSummary,
    projects: Vec<ProjectBacklog>,
    unreachable: Vec<UnreachableProject>,
}

#[derive(Serialize)]
struct BacklogSummary {
    projects: usize,
    needs_review: usize,
    no_patchset: usize,
    debt_owed: usize,
    /// The debt count split by what each obligation says is missing, in
    /// severity order. A workspace total says how much is owed and nothing
    /// about what any of it owes.
    debt_owed_by_kind: Vec<crate::inbox::DebtKindCount>,
    open_items: usize,
    later_items: usize,
    feature_requests: usize,
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
                    .map(|debt| debt.missing),
            ),
            open_items: projects.iter().map(|project| project.open_items).sum(),
            later_items: projects.iter().map(|project| project.later_items).sum(),
            feature_requests: projects
                .iter()
                .map(|project| project.feature_requests)
                .sum(),
            unreachable: unreachable.len(),
        }
    }

    fn render(&self) {
        println!(
            "summary: {} projects; {} needs-review; {} debt-owed ({}); {} no-patchset; journal {} open, {} later, {} feature-request; {} unreachable",
            self.projects,
            self.needs_review,
            self.debt_owed,
            crate::inbox::DebtKindCount::render(&self.debt_owed_by_kind),
            self.no_patchset,
            self.open_items,
            self.later_items,
            self.feature_requests,
            self.unreachable,
        );
    }
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

#[derive(Serialize)]
struct ProjectBacklog {
    project: String,
    anchor: String,
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
}

#[derive(Serialize)]
struct BacklogItems {
    open: Vec<crate::journal::ArtifactEntry>,
    later: Vec<crate::journal::ArtifactEntry>,
    feature_requests: Vec<crate::journal::ArtifactEntry>,
}

impl ProjectBacklog {
    /// Whether anything here is waiting on a person rather than on work.
    fn blocked(&self) -> usize {
        self.needs_review.len() + self.debt_owed.len()
    }

    fn is_empty(&self) -> bool {
        self.blocked() == 0
            && self.no_patchset.is_empty()
            && self.open_items == 0
            && self.later_items == 0
            && self.feature_requests == 0
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
    json: bool,
) -> Result<()> {
    let cfg = crate::config::load()?;
    let scope = ResolvedWorkspaceScope::resolve(scope)?;
    let cutoff = match since {
        Some(raw) => Some(
            crate::journal::parse_since(raw)
                .with_context(|| format!("cannot read --since {raw:?}"))?,
        ),
        None => None,
    };
    let mut projects = Vec::new();
    let mut unreachable = Vec::new();

    for project in crate::registry::projects(&cfg)? {
        if !scope.includes(project.anchor.as_deref()) {
            continue;
        }
        if !project.reachable {
            // An orphan holds work nobody can reach; a merely empty journal at
            // a vanished path is housekeeping, not a finding.
            if project.is_orphan() {
                unreachable.push(UnreachableProject {
                    slug: project.slug.clone(),
                    journal_dir: project.journal_dir.display().to_string(),
                    anchor: project.anchor.as_ref().map(|p| p.display().to_string()),
                    reason: match project.anchor {
                        Some(_) => "anchor does not exist",
                        None => "journal name resolves to no single path",
                    },
                });
            }
            continue;
        }
        let anchor = project
            .anchor
            .clone()
            .expect("a reachable project has an anchor");

        let queues = match &project.ledger {
            Some(root) => ledger_queues(root)?,
            None => LedgerQueues::default(),
        };

        let open_queue = crate::journal::collect_open_in(ctx, &project.journal_dir, &anchor, None)?;
        // Under --since the counts mean "filed since", not "outstanding": a
        // delta that reported the whole queue beside a delta heading would read
        // as a full report and be believed as one.
        let (open_items, later_items, feature_requests) = match cutoff {
            Some(cutoff) => open_queue.tier_counts_since(cutoff),
            None => open_queue.tier_counts(),
        };
        let backlog_items = if show_items {
            let (open, later, feature_requests) = match cutoff {
                Some(cutoff) => open_queue.tiers_since(cutoff),
                None => {
                    let (open, later, feature_requests) = open_queue.tiers();
                    (
                        open.iter().collect(),
                        later.iter().collect(),
                        feature_requests.iter().collect(),
                    )
                }
            };
            Some(BacklogItems {
                open: open.into_iter().cloned().collect(),
                later: later.into_iter().cloned().collect(),
                feature_requests: feature_requests.into_iter().cloned().collect(),
            })
        } else {
            None
        };
        let entry = ProjectBacklog {
            project: project.label(),
            anchor: anchor.display().to_string(),
            needs_review: queues.needs_review,
            no_patchset: queues.no_patchset,
            shared_surfaces: queues.shared_surfaces,
            debt_owed: queues.debt_owed,
            open_items,
            later_items,
            feature_requests,
            // Age is a property of the whole queue, so it would contradict
            // counts that mean "filed since". A delta reports arrivals only.
            oldest_open_days: cutoff
                .is_none()
                .then(|| open_queue.oldest_open_days())
                .flatten(),
            items: backlog_items,
        };
        if !entry.is_empty() {
            projects.push(entry);
        }
    }
    projects.sort_by(|a, b| {
        b.blocked()
            .cmp(&a.blocked())
            .then_with(|| b.open_items.cmp(&a.open_items))
            .then_with(|| a.project.cmp(&b.project))
    });
    let summary = BacklogSummary::derive(&projects, &unreachable);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Backlog {
                schema: "arc-workspace-backlog/11",
                scope: scope.view(),
                summary,
                projects,
                unreachable,
            })?
        );
        return Ok(());
    }

    println!("scope: {}", scope.text());
    if let Some(raw) = since {
        println!("since {raw}: journal counts are what was filed since, not what is outstanding");
    }
    summary.render();
    if projects.is_empty() && unreachable.is_empty() {
        println!("nothing outstanding in this workspace scope");
        return Ok(());
    }
    for project in &projects {
        println!("# {} ({})", project.project, project.anchor);
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
                "  debt-owed     {}  {}d, {}, {}{}",
                change.change_id,
                change.age_days,
                crate::render::debt_line(
                    change.missing,
                    change.production.as_ref(),
                    change.coverage.as_deref()
                ),
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
    Ok(())
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
