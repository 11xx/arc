//! Change-worktree context and explicit harness bootstrap helpers.
//!
//! Explicit change references and `ARC_HARNESS`/`ARC_SESSION` remain
//! authoritative. This module only infers an omitted reference from the
//! current Git branch or recorded worktree, and exposes identity detection
//! through the opt-in `arc env` command.

use crate::commands::{self, Ctx, StatusOutput};
use crate::gitio;
use crate::journal;
use crate::state::{self, ChangeState};
use crate::store::Store;
use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Environment variables recognized by `arc env`, in precedence order.
/// Detection is deliberately not part of normal command identity resolution.
const HARNESS_ENV: [(&str, &str); 3] = [
    ("CLAUDE_SESSION_ID", "claude"),
    ("CODEX_THREAD_ID", "codex"),
    ("OPENCODE_SESSION", "opencode"),
];

/// Resolve an explicit change reference or infer one from the current branch
/// and, as a fallback, the current directory's recorded change worktree.
pub fn resolve_change_or_infer(
    store: &Store,
    cwd: &Path,
    maybe_arg: Option<&str>,
) -> Result<String> {
    if let Some(reference) = maybe_arg {
        return store.resolve_change(reference);
    }
    infer_change(store, cwd)?.ok_or_else(|| {
        anyhow::anyhow!(
            "cannot infer a change from {}; candidates: (none); pass CHANGE explicitly",
            cwd.display()
        )
    })
}

fn infer_change(store: &Store, cwd: &Path) -> Result<Option<String>> {
    let open = open_changes(store)?;
    if let Some(branch) = gitio::current_branch(cwd)? {
        let matches = open
            .iter()
            .filter(|state| state.branch == branch)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [state] => return Ok(Some(state.change_id.clone())),
            [] => {}
            _ => ambiguous(cwd, &matches)?,
        }
    }

    let cwd = canonical_or_owned(cwd);
    let matches = open
        .iter()
        .filter(|state| {
            state
                .worktree
                .as_deref()
                .map(PathBuf::from)
                .map(|path| canonical_or_owned(&path))
                .is_some_and(|worktree| cwd.starts_with(worktree))
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [state] => Ok(Some(state.change_id.clone())),
        [] => Ok(None),
        _ => ambiguous(&cwd, &matches),
    }
}

fn open_changes(store: &Store) -> Result<Vec<ChangeState>> {
    let states = store
        .list_change_ids()?
        .into_iter()
        .map(|id| state::reduce(&store.load_events(&id)?))
        .collect::<Result<Vec<_>>>()?;
    Ok(states
        .into_iter()
        .filter(|state| !state.is_closed())
        .collect())
}

fn canonical_or_owned(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn ambiguous<T>(cwd: &Path, matches: &[&ChangeState]) -> Result<T> {
    bail!(
        "cannot infer a unique change from {}; candidates: {}; pass CHANGE explicitly",
        cwd.display(),
        matches
            .iter()
            .map(|state| state.change_id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub fn print_env() -> i32 {
    for (variable, harness) in HARNESS_ENV {
        if let Some(session) = std::env::var_os(variable) {
            let session = session.to_string_lossy();
            if session.is_empty() {
                continue;
            }
            println!(
                "export ARC_HARNESS={} ARC_SESSION={}",
                shell_quote(harness),
                shell_quote(&session)
            );
            return 0;
        }
    }
    println!("# export ARC_HARNESS=<claude|codex|opencode> ARC_SESSION=<session-id>");
    1
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Serialize)]
struct ResumeOutput {
    schema: &'static str,
    status: StatusOutput,
    journal: journal::ContextJournal,
}

pub fn resume(
    ctx: &Ctx,
    reference: Option<&str>,
    json: bool,
    get: Option<&str>,
    fields: Option<&str>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = resolve_change_or_infer(&store, &ctx.cwd, reference)?;
    let (_, state) = ctx.load_state(&store, &change_id)?;
    let status = commands::status_output(ctx, &store, &state)?;
    let journal = journal::context_for_change(ctx, &state.slug)?;

    if json || get.is_some() || fields.is_some() {
        commands::print_projected(
            serde_json::to_value(ResumeOutput {
                schema: "arc-resume/1",
                status,
                journal,
            })?,
            get,
            fields,
        )?;
        return Ok(());
    }

    println!("# {} (`{}`)", state.title, state.change_id);
    if let Some(brief) = state.latest_brief() {
        println!("\n## Brief (v{})\n", state.briefs.len());
        print!("{}", brief.body);
        if !brief.body.ends_with('\n') {
            println!();
        }
    }
    println!("\n## Claim / Stage\n");
    match &status.report.claim {
        Some(claim) => println!(
            "- {} via {}/{}: `{}`{}",
            claim.owner.actor,
            claim.owner.harness,
            claim.owner.session,
            claim.stage,
            claim
                .note
                .as_deref()
                .map(|note| format!(" — {note}"))
                .unwrap_or_default()
        ),
        None => println!("- (unclaimed)"),
    }
    println!("\n## Open Findings\n");
    let findings = status
        .report
        .findings
        .iter()
        .filter(|finding| {
            !matches!(
                finding.status.as_str(),
                "resolved" | "acceptedrisk" | "obsolete"
            )
        })
        .collect::<Vec<_>>();
    if findings.is_empty() {
        println!("- (none)");
    } else {
        for finding in findings {
            println!(
                "- `{}` [{}] {}",
                finding.id, finding.status, finding.summary
            );
        }
    }
    println!("\n## Gates at Head\n");
    if status.report.gates.is_empty() {
        println!("- (none)");
    } else {
        for gate in &status.report.gates {
            println!("- {}: {}", gate.name, gate.result);
        }
    }
    println!("\nNext action: {}", status.report.next_action);
    journal.render_markdown();
    Ok(())
}

pub fn prompt(ctx: &Ctx, reference: Option<&str>) -> Result<()> {
    let store = match reference {
        Some(_) => ctx.store()?,
        None => match ctx.store() {
            Ok(store) => store,
            Err(error) if not_inside_git_repository(&error) => return Ok(()),
            Err(error) => return Err(error),
        },
    };
    let change_id = match reference {
        Some(reference) => store.resolve_change(reference)?,
        None => match infer_change(&store, &ctx.cwd)? {
            Some(change_id) => change_id,
            None => return Ok(()),
        },
    };
    let (_, state) = ctx.load_state(&store, &change_id)?;
    if reference.is_none() && !cwd_is_in_recorded_worktree(&state, &ctx.cwd) {
        return Ok(());
    }
    let report = ctx.report(&store, &state)?;
    let patchset = state.latest_patchset().map_or("ps-00", |p| p.id.as_str());
    let stage = report
        .claim
        .as_ref()
        .map_or("-", |claim| claim.stage.as_str());
    let verdict = report
        .verdict
        .as_ref()
        .map_or("unreviewed", |verdict| match verdict.verdict {
            crate::model::Verdict::Approved => "approved",
            crate::model::Verdict::ChangesRequested => "changes-requested",
            crate::model::Verdict::CommentOnly => "comment-only",
        });
    let gates = if report.gates.is_empty() {
        "none"
    } else if report.gates.iter().all(|gate| gate.green_at_head) {
        "green"
    } else {
        "red"
    };
    println!(
        "{} {} {} {} gates:{}{}",
        state.slug,
        patchset,
        stage,
        verdict,
        gates,
        if state.hold.is_some() { " [hold]" } else { "" }
    );
    Ok(())
}

fn not_inside_git_repository(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.to_string() == "not inside a Git repository (and no path override is set)"
    })
}

fn cwd_is_in_recorded_worktree(state: &ChangeState, cwd: &Path) -> bool {
    let cwd = canonical_or_owned(cwd);
    state
        .worktree
        .as_deref()
        .map(PathBuf::from)
        .map(|path| canonical_or_owned(&path))
        .is_some_and(|worktree| cwd.starts_with(worktree))
}
