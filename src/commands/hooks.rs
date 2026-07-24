//! Opt-in Git hook pack and commit↔change linkage. arc never installs hooks
//! silently: `hooks install` is an explicit action, hook scripts are marked so
//! `uninstall` only removes arc's own, and every hook probes for `arc` on PATH
//! before running it and unconditionally exits 0 so it can never block a commit.

use super::*;

/// Marker line identifying an arc-authored hook script.
const MARKER: &str = "# arc-managed hook";
const HOOKS: [&str; 2] = ["post-commit", "prepare-commit-msg"];

fn hook_script(name: &str) -> String {
    format!(
        "#!/bin/sh\n{MARKER}\ncommand -v arc >/dev/null 2>&1 || exit 0\narc hook-run {name} \"$@\"\nexit 0\n"
    )
}

fn is_arc_hook(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.contains(MARKER))
        .unwrap_or(false)
}

pub fn install(ctx: &Ctx, force: bool) -> Result<()> {
    let dir = gitio::git_path(&ctx.cwd, "hooks")?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create hooks dir {}", dir.display()))?;
    for name in HOOKS {
        let path = dir.join(name);
        if path.exists() && !is_arc_hook(&path) {
            if !force {
                bail!(
                    "{name} hook already exists and is not arc-managed; \
                     rerun with --force to replace it (the current hook is saved as {name}.pre-arc)"
                );
            }
            let saved = dir.join(format!("{name}.pre-arc"));
            std::fs::rename(&path, &saved)
                .with_context(|| format!("cannot save existing hook to {}", saved.display()))?;
            println!("saved existing {name} to {name}.pre-arc");
        }
        std::fs::write(&path, hook_script(name))
            .with_context(|| format!("cannot write {}", path.display()))?;
        make_executable(&path)?;
        println!("installed: {name}");
    }
    Ok(())
}

pub fn uninstall(ctx: &Ctx) -> Result<()> {
    let dir = gitio::git_path(&ctx.cwd, "hooks")?;
    for name in HOOKS {
        let path = dir.join(name);
        if path.exists() && is_arc_hook(&path) {
            std::fs::remove_file(&path)
                .with_context(|| format!("cannot remove {}", path.display()))?;
            println!("removed: {name}");
        }
    }
    Ok(())
}

pub fn status(ctx: &Ctx) -> Result<()> {
    let dir = gitio::git_path(&ctx.cwd, "hooks")?;
    for name in HOOKS {
        let path = dir.join(name);
        let state = if !path.exists() {
            "absent"
        } else if is_arc_hook(&path) {
            "arc-managed"
        } else {
            "foreign"
        };
        println!("{name}: {state}");
    }
    Ok(())
}

fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("cannot chmod {}", path.display()))
}

/// Dispatch a hook. Hooks are advisory and must never block a commit, so any
/// internal failure is swallowed and the process still exits 0.
pub fn hook_run(ctx: &Ctx, name: &str, args: &[String]) -> i32 {
    let result = match name {
        "post-commit" => post_commit(ctx),
        "prepare-commit-msg" => prepare_commit_msg(ctx, args),
        _ => Ok(()),
    };
    if let Err(error) = result {
        eprintln!("arc hook {name}: {error:#}");
    }
    0
}

/// The open change whose branch is currently checked out, if any; falls back
/// to a closed change on the branch so post-commit can warn about it.
fn change_for_branch(ctx: &Ctx, store: &Store) -> Result<Option<ChangeState>> {
    let Some(branch) = gitio::current_branch(&ctx.cwd)? else {
        return Ok(None);
    };
    let mut closed_match = None;
    for change_id in store.list_change_ids()? {
        let state = state::reduce(&store.load_events(&change_id)?)?;
        if state.branch != branch {
            continue;
        }
        if state.is_closed() {
            closed_match = Some(state);
        } else {
            return Ok(Some(state));
        }
    }
    Ok(closed_match)
}

fn post_commit(ctx: &Ctx) -> Result<()> {
    let store = ctx.store()?;
    let Some(state) = change_for_branch(ctx, &store)? else {
        return Ok(());
    };
    if state.is_closed() {
        println!(
            "arc: this branch's change {} is closed; new commits are not tracked",
            state.change_id
        );
        return Ok(());
    }
    // A new commit moves the head past any approved snapshot, so an approval
    // bound to that exact head no longer holds.
    let head = gitio::branch_head(&ctx.cwd, &state.branch)?;
    if let Some(verdict) = state.latest_verdict() {
        if verdict.verdict == Verdict::Approved {
            if let Some(patchset) = state
                .patchsets
                .iter()
                .find(|patchset| patchset.id == verdict.patchset_id)
            {
                if patchset.head != head {
                    let short: String = patchset.head.chars().take(12).collect();
                    println!(
                        "arc: approval on {} is now stale ({} approved at {short})",
                        state.change_id, patchset.id
                    );
                }
            }
        }
    }
    Ok(())
}

fn prepare_commit_msg(ctx: &Ctx, args: &[String]) -> Result<()> {
    let Some(path) = args.first() else {
        return Ok(());
    };
    let store = ctx.store()?;
    let Some(state) = change_for_branch(ctx, &store)? else {
        return Ok(());
    };
    if state.is_closed() {
        return Ok(());
    }
    let Ok(body) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let trailer = format!("Arc-Change: {}", state.change_id);
    if body.lines().any(|line| line.trim() == trailer) {
        return Ok(());
    }
    let mut updated = body;
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&trailer);
    updated.push('\n');
    std::fs::write(path, updated).with_context(|| format!("cannot write {path}"))?;
    Ok(())
}

/// Changes whose recorded revisions match a commit (unique-prefix accepted):
/// any patchset head, or the integration/closure commit. Ledger-only — no
/// commit-trailer scanning.
pub fn query_commit(ctx: &Ctx, sha: &str) -> Result<()> {
    let store = ctx.store()?;
    let mut matched = Vec::new();
    for change_id in store.list_change_ids()? {
        let state = state::reduce(&store.load_events(&change_id)?)?;
        let mut revisions: Vec<&str> = state
            .patchsets
            .iter()
            .map(|patchset| patchset.head.as_str())
            .collect();
        if let Some(closure) = &state.closure {
            if let Some(commit) = &closure.integrated_commit {
                revisions.push(commit);
            }
        }
        if revisions.iter().any(|rev| rev.starts_with(sha)) {
            matched.push(state);
        }
    }
    for state in matched {
        println!(
            "{}  [{}] {}",
            state.change_id,
            change_status(&state),
            state.title
        );
    }
    Ok(())
}
