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

/// A `data_root` subdirectory that is an arc store, with its slug label.
fn workspace_stores() -> Result<Vec<(String, Store)>> {
    let data_root = crate::config::load()?.data_root.context(
        "arc workspace requires a configured data_root; per-repo git-common-dir \
         ledgers are not enumerable (set data_root in the config file or ARC_DATA_ROOT)",
    )?;
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

pub fn workspace(_ctx: &Ctx, view: WorkspaceView, json: bool) -> Result<()> {
    let stores = workspace_stores()?;
    match view {
        WorkspaceView::List => workspace_list(&stores, json),
        WorkspaceView::Inbox => workspace_inbox(&stores, json),
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
