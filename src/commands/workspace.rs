//! Cross-repository aggregation and rebase advice for a lead working across
//! many change ledgers. All read-only: no store is ever created while
//! scanning, and `restack` only prints commands — arc never rewrites branches.

use super::*;
use crate::gates::GatesFile;
use crate::policy::PolicyFile;
use serde::Serialize;

pub enum WorkspaceView {
    List,
    Inbox,
    Backlog { since: Option<String>, items: bool },
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
    for change_id in store.list_change_ids()? {
        let events = store.load_events(&change_id)?;
        states.insert(change_id, state::reduce(&events)?);
    }
    Ok(states)
}

pub fn workspace(ctx: &Ctx, view: WorkspaceView, json: bool) -> Result<()> {
    if let WorkspaceView::Backlog { since, items } = view {
        return workspace_backlog(ctx, since.as_deref(), items, json);
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
    projects: Vec<ProjectBacklog>,
    unreachable: Vec<UnreachableProject>,
}

#[derive(Serialize)]
struct ProjectBacklog {
    project: String,
    anchor: String,
    /// Changes whose next step is a verdict rather than more work.
    needs_review: Vec<String>,
    debt_owed: Vec<String>,
    open_items: usize,
    later_items: usize,
    feature_requests: usize,
    /// The primary tier's oldest entry, in days. A one-item queue never looks
    /// like a backlog from inside the project; across projects it is visible.
    oldest_open_days: Option<u64>,
    /// Every artifact behind the counts above, when `--items` asked for
    /// them. The same artifacts the counts are taken over, so the two cannot
    /// disagree. The debt_owed rename is carried by
    /// arc-workspace-backlog/4.
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

/// One backlog across every project the registry knows, ledger and journal
/// together.
///
/// Ranked by what is blocked, never by comparing items across projects: arc
/// records no priority that spans repositories, and inventing one here would
/// be a routing opinion rather than a derived fact.
fn workspace_backlog(ctx: &Ctx, since: Option<&str>, show_items: bool, json: bool) -> Result<()> {
    let cfg = crate::config::load()?;
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

        let (needs_review, debt_owed) = match &project.ledger {
            Some(root) => ledger_queues(root)?,
            None => (Vec::new(), Vec::new()),
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
            needs_review,
            debt_owed,
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

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Backlog {
                schema: "arc-workspace-backlog/4",
                projects,
                unreachable,
            })?
        );
        return Ok(());
    }

    if let Some(raw) = since {
        println!("since {raw}: journal counts are what was filed since, not what is outstanding");
    }
    if projects.is_empty() && unreachable.is_empty() {
        println!("nothing outstanding across every known project");
        return Ok(());
    }
    for project in &projects {
        println!("# {} ({})", project.project, project.anchor);
        for change in &project.needs_review {
            println!("  needs-review  {change}");
        }
        for change in &project.debt_owed {
            println!("  debt-owed    {change}");
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
        println!("unreachable:");
        for project in &unreachable {
            println!("  {}  {}", project.slug, project.reason);
            println!("    journal: {}", project.journal_dir);
            println!("    adopt from the project's new location: arc journal rebind <dir>");
        }
    }
    Ok(())
}

/// The two ledger buckets a lead reads across projects: what awaits a verdict,
/// and what shipped owing one.
fn ledger_queues(root: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let Some(store) = Store::open_at(root)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let states = repo_states(&store)?;
    let mut needs_review = Vec::new();
    let mut debt_owed = Vec::new();
    for state in states.values() {
        // Audit debt outlives integration, so it is asked of every change;
        // a review verdict is only owed while the change is still open.
        if state.debt_outstanding() {
            debt_owed.push(state.change_id.clone());
        }
        if !state.is_closed() && crate::inbox::needs_review(state) {
            needs_review.push(state.change_id.clone());
        }
    }
    needs_review.sort();
    debt_owed.sort();
    Ok((needs_review, debt_owed))
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
