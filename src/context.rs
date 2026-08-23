//! Change-worktree context and explicit harness bootstrap helpers.
//!
//! Explicit change references and identity always remain authoritative. This
//! module infers an omitted reference from the current Git branch or recorded
//! worktree, and provides identity detection as an opt-in fallback enabled by
//! `[identity] detect`.

use crate::commands::{self, Ctx, StatusOutput};
use crate::gitio;
use crate::journal;
use crate::session_store;
use crate::state::{self, ChangeState};
use crate::store::Store;
use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variables recognized for opt-in identity detection, in
/// precedence order. Explicit identity always wins over detected values.
const HARNESS_ENV: [(&str, &str); 4] = [
    ("CLAUDE_SESSION_ID", "claude"),
    ("CODEX_THREAD_ID", "codex"),
    ("OPENCODE_SESSION", "opencode"),
    ("PI_SESSION_ID", "pi"),
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
    if let Some(change_id) = infer_change(store, cwd)? {
        return Ok(change_id);
    }
    bail!(
        "cannot infer a change from {}; candidates: (none); pass CHANGE explicitly\n{}",
        cwd.display(),
        no_candidate_tip(store)?
    )
}

/// A caller standing outside every recorded worktree has asked a question arc
/// can answer — it just needs to be told which change. Naming the open
/// changes and where each is checked out turns a dead end into the next
/// command; with nothing open at all, the backlog is the honest next step.
fn no_candidate_tip(store: &Store) -> Result<String> {
    let open = open_changes(store)?;
    if open.is_empty() {
        return Ok(
            "tip: no change is open here; `arc catchup` shows the ledger queue and the \
             journal backlog"
                .to_string(),
        );
    }
    let mut tip = String::from("tip: name one of these, or cd to its worktree:");
    for state in &open {
        tip.push_str(&format!(
            "\n  {}  {}",
            state.change_id,
            state.worktree.as_deref().unwrap_or("(no worktree)")
        ));
    }
    Ok(tip)
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

pub struct DetectedIdentity {
    pub harness: String,
    pub session: String,
    pub model: Option<String>,
}

pub fn detect_identity() -> Option<DetectedIdentity> {
    for (variable, harness) in HARNESS_ENV {
        if let Some(session) = std::env::var_os(variable) {
            let session = session.to_string_lossy();
            if session.is_empty() {
                continue;
            }
            let model = detect_model(harness, &session);
            return Some(DetectedIdentity {
                harness: harness.to_string(),
                session: session.into_owned(),
                model,
            });
        }
    }
    None
}

pub fn print_env() -> i32 {
    if let Some(identity) = detect_identity() {
        match identity.model {
            Some(model) => println!(
                "export ARC_HARNESS={} ARC_SESSION={} ARC_MODEL={}",
                shell_quote(&identity.harness),
                shell_quote(&identity.session),
                shell_quote(&model)
            ),
            None => println!(
                "export ARC_HARNESS={} ARC_SESSION={}",
                shell_quote(&identity.harness),
                shell_quote(&identity.session)
            ),
        }
        return 0;
    }
    println!(
        "# export ARC_HARNESS=<claude|codex|opencode|pi> ARC_SESSION=<session-id> \
         ARC_MODEL=<model[#effort]>"
    );
    1
}

/// Best-effort model detection for `arc env`: read the harness's own session
/// store and extract the model (plus effort, where the store records one).
/// Detection is a convenience layered on the explicit `ARC_MODEL` contract —
/// every failure mode is a silent omission, never an error.
fn detect_model(harness: &str, session: &str) -> Option<String> {
    match harness {
        "claude" => detect_claude_model(session),
        "codex" => detect_codex_model(session),
        "opencode" => detect_opencode_model(session),
        "pi" => detect_pi_model(session),
        _ => None,
    }
}

/// `~/.claude/projects/<cwd-slug>/<session>.jsonl`: assistant messages carry
/// `message.model`; the newest one wins.
fn detect_claude_model(session: &str) -> Option<String> {
    let path = session_store::transcript_path("claude", session)?;
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines().rev() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(model) = value["message"]["model"].as_str() {
            return Some(model.to_string());
        }
    }
    None
}

/// `$CODEX_HOME/sessions/<y>/<m>/<d>/rollout-*<session>.jsonl` (falling back
/// to `~/.codex`): the newest `turn_context` payload carries `model` and
/// `effort`; combined as `model#effort` when both exist. Older effort fields
/// remain readable for compatibility.
fn detect_codex_model(session: &str) -> Option<String> {
    let file = session_store::transcript_path("codex", session)?;
    let text = std::fs::read_to_string(file).ok()?;
    let mut model: Option<String> = None;
    let mut effort: Option<String> = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["type"] != "turn_context" {
            continue;
        }
        let payload = &value["payload"];
        if let Some(m) = payload["model"].as_str() {
            model = Some(m.to_string());
        }
        if let Some(e) = payload["effort"]
            .as_str()
            .or_else(|| payload["reasoning_effort"].as_str())
            .or_else(|| payload["collaboration_mode"]["settings"]["reasoning_effort"].as_str())
        {
            effort = Some(e.to_string());
        }
    }
    let model = model?;
    Some(match effort {
        Some(effort) => format!("{model}#{effort}"),
        None => model,
    })
}

/// OpenCode stores the selected model as JSON in its SQLite session row. Both
/// stable and preview store names are checked. `sqlite3` is an optional
/// best-effort reader: its absence leaves ARC_MODEL unset without making
/// `arc env` fail.
fn detect_opencode_model(session: &str) -> Option<String> {
    let session = session.replace('\'', "''");
    let query = format!("SELECT model FROM session WHERE id = '{session}' LIMIT 1;");
    for path in session_store::opencode_databases()? {
        if !path.is_file() {
            continue;
        }
        let output = Command::new("sqlite3")
            .arg("-noheader")
            .arg(&path)
            .arg(&query)
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }
        let raw = String::from_utf8(output.stdout).ok()?;
        if let Some(model) = parse_opencode_model(raw.trim()) {
            return Some(model);
        }
    }
    None
}

fn parse_opencode_model(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let model = value["id"].as_str()?.trim();
    if model.is_empty() {
        return None;
    }
    Some(
        match value["variant"].as_str().filter(|value| !value.is_empty()) {
            Some(effort) => format!("{model}#{effort}"),
            None => model.to_string(),
        },
    )
}

/// Pi session JSONL carries model and thinking-level changes. The current Pi
/// runtime bridge exposes its native ID as `PI_SESSION_ID`; custom agent and
/// session roots are honored before the default store.
fn detect_pi_model(session: &str) -> Option<String> {
    let file = session_store::transcript_path("pi", session)?;
    let text = std::fs::read_to_string(file).ok()?;
    let mut model: Option<String> = None;
    let mut effort: Option<String> = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value["type"].as_str() {
            Some("model_change") => {
                if let Some(value) = value["modelId"].as_str() {
                    model = Some(value.to_string());
                }
            }
            Some("thinking_level_change") => {
                if let Some(value) = value["thinkingLevel"].as_str() {
                    effort = Some(value.to_string());
                }
            }
            _ => {}
        }
    }
    let model = model?;
    Some(match effort {
        Some(effort) => format!("{model}#{effort}"),
        None => model,
    })
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
        if let Some(base_revision) = &brief.base_revision {
            println!("- Base revision: `{base_revision}`\n");
        }
        if !brief.acceptance_probes.is_empty() {
            println!("- Acceptance probes:");
            for probe in &brief.acceptance_probes {
                println!("  - `{}`: `{}`", probe.name, probe.command);
            }
            println!();
        }
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
    println!("\n## Worktree\n");
    println!(
        "- Branch head: {}",
        match (
            status.report.latest_patchset.as_ref(),
            status.report.head_matches_latest_patchset,
        ) {
            (None, _) => "no patchset recorded",
            (Some(_), true) => "matches the newest approved/snapshotted head",
            (Some(_), false) => "has moved past the newest patchset",
        }
    );
    println!(
        "- Uncommitted edits: {}",
        match status.report.worktree_dirty {
            Some(true) => "present",
            Some(false) => "absent",
            None => "unknown",
        }
    );
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
            println!("- {}: {}", gate.name, crate::render::gate_line(gate));
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
        if state.holds.is_empty() {
            ""
        } else {
            " [hold]"
        }
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
