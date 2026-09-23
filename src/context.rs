//! Change-worktree context and explicit harness bootstrap helpers.
//!
//! Explicit change references and identity always remain authoritative. This
//! module infers an omitted reference from the current Git branch or recorded
//! worktree, and provides identity detection as an opt-in fallback enabled by
//! `[identity] detect`.

use crate::commands::{self, Ctx, StatusOutput};
use crate::gitio;
use crate::journal;
use crate::model::SessionResolution;
use crate::session_store;
use crate::state::ChangeState;
use crate::status::BriefBaseDrift;
use crate::store::Store;
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variables recognized for opt-in identity detection, in
/// precedence order. Explicit identity always wins over detected values.
///
/// Claude has two spellings: `CLAUDE_CODE_SESSION_ID` is what Claude Code
/// exports into its tool shells, and `CLAUDE_SESSION_ID` is the hand-set
/// form. The hand-set one comes first so a deliberately exported id beats
/// the ambient one.
const HARNESS_ENV: [(&str, &str); 5] = [
    ("CLAUDE_SESSION_ID", "claude"),
    ("CLAUDE_CODE_SESSION_ID", "claude"),
    ("CODEX_THREAD_ID", "codex"),
    ("OPENCODE_SESSION", "opencode"),
    ("PI_SESSION_ID", "pi"),
];

/// A harness whose session id reaches its tool children only through the
/// prompt. The v1 CLI exports `OPENCODE_SESSION` and is handled by the env
/// ladder; the v2 beta (`opencode2`) exports none, so it is recognized by the
/// witnesses it cannot help carrying: `OPENCODE_TERMINAL` in every tool-shell
/// environment, and its own process name in the PPID chain. One label covers
/// both versions — v2 is the same project — and the session id stays unset,
/// which is the honest report rather than a guessed one.
const OPENCODE_COMMS: [&str; 2] = ["opencode", "opencode2"];

/// How far up the PPID chain ancestry detection looks. A harness sits within
/// a few steps of its tool children; the cap keeps a pathological chain
/// bounded rather than trusting it to terminate.
const ANCESTRY_DEPTH: usize = 32;

/// Whether the process ancestry carries an OpenCode marker. Reads `/proc`
/// directly, so this is a Linux witness: anywhere else it finds nothing.
fn detect_opencode_ancestry() -> bool {
    let mut pid = std::process::id();
    for _ in 0..ANCESTRY_DEPTH {
        let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
            return false;
        };
        if OPENCODE_COMMS.contains(&comm.trim()) {
            return true;
        }
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            return false;
        };
        let ppid = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .and_then(|rest| rest.trim().parse::<u32>().ok());
        match ppid {
            Some(parent) if parent != pid && parent != 0 => pid = parent,
            _ => return false,
        }
    }
    false
}

/// Whether OpenCode is detectable without a session variable: positive env
/// evidence first, then ancestry. `OPENCODE_TERMINAL` names a terminal, not a
/// session, and may be absent in headless runs — that is why ancestry exists
/// beside it rather than instead of it.
fn detect_opencode_harness() -> bool {
    if std::env::var("OPENCODE_TERMINAL").is_ok_and(|value| value == "1") {
        return true;
    }
    detect_opencode_ancestry()
}

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

/// The open change this checkout belongs to, matched first by branch and then
/// by recorded worktree. `None` is an answer: a caller may legitimately stand
/// outside every open change, and only a caller that requires one turns that
/// into an error.
pub(crate) fn infer_change(store: &Store, cwd: &Path) -> Result<Option<String>> {
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
        .map(|id| store.state(&id))
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

/// A session id detected from the environment, with what the harness's own
/// store said about it.
pub struct DetectedSession {
    pub id: String,
    /// The store's answer for `id`. Most events carry nothing else about who
    /// wrote them, so whether the store knows the id is part of the identity
    /// report rather than a fact inferred later from a missing model.
    pub resolution: SessionResolution,
}

pub struct DetectedIdentity {
    pub harness: String,
    /// A harness recognized without its cooperation carries no session id;
    /// `None` reports that honestly where an empty string would fabricate a
    /// record.
    pub session: Option<DetectedSession>,
    pub model: Option<String>,
}

pub fn detect_identity() -> Option<DetectedIdentity> {
    for (variable, harness) in HARNESS_ENV {
        if let Some(session) = std::env::var_os(variable) {
            let session = session.to_string_lossy().into_owned();
            if session.is_empty() {
                continue;
            }
            let (resolution, model) = match harness {
                "claude" | "codex" | "pi" => {
                    let transcript = session_store::transcript_path(harness, &session);
                    let resolution = match transcript {
                        Some(_) => SessionResolution::Corroborated,
                        None => SessionResolution::Uncorroborated,
                    };
                    let model = transcript
                        .as_deref()
                        .and_then(|path| detect_model(harness, path));
                    (resolution, model)
                }
                "opencode" => detect_opencode_record(&session),
                _ => (SessionResolution::Uncorroborated, None),
            };
            return Some(DetectedIdentity {
                harness: harness.to_string(),
                session: Some(DetectedSession {
                    id: session,
                    resolution,
                }),
                model,
            });
        }
    }
    if detect_opencode_harness() {
        return Some(DetectedIdentity {
            harness: "opencode".to_string(),
            session: None,
            model: None,
        });
    }
    None
}

pub fn print_env() -> i32 {
    let Some(identity) = detect_identity() else {
        println!(
            "# export ARC_HARNESS=<claude|codex|opencode|pi> ARC_SESSION=<session-id> \
             ARC_MODEL=<model[#effort]>"
        );
        return 1;
    };
    let Some(session) = identity.session else {
        // The harness resolved without its cooperation, so the session id was
        // never reachable. The export line is real and eval-able; the comment
        // carries the report a full-detection run would not need.
        println!("export ARC_HARNESS={}", shell_quote(&identity.harness));
        println!(
            "# export ARC_SESSION=<session-id>  # unavailable: {} does not \
             export a session variable; set it by hand",
            identity.harness
        );
        return 0;
    };
    match identity.model {
        Some(model) => println!(
            "export ARC_HARNESS={} ARC_SESSION={} ARC_MODEL={}",
            shell_quote(&identity.harness),
            shell_quote(&session.id),
            shell_quote(&model)
        ),
        None => println!(
            "export ARC_HARNESS={} ARC_SESSION={}",
            shell_quote(&identity.harness),
            shell_quote(&session.id)
        ),
    }
    println!(
        "# {}",
        session_resolution_line(&identity.harness, session.resolution)
    );
    0
}

/// What `arc env` says about a detected session's backing. The store is
/// asked once, at detection time, so this is a fact the report carries rather
/// than something a caller infers from an absent model.
fn session_resolution_line(harness: &str, resolution: SessionResolution) -> String {
    match resolution {
        SessionResolution::Corroborated => format!(
            "session corroborated: the {harness} session store resolved a recording for this id"
        ),
        SessionResolution::Uncorroborated => format!(
            "session uncorroborated: the {harness} session store resolved no recording for this id"
        ),
    }
}

/// Best-effort model detection for `arc env`: read the resolved transcript
/// and extract the model (plus effort, where the store records one).
/// Detection is a convenience layered on the explicit `ARC_MODEL` contract —
/// every failure mode here is a silent omission, never an error; whether the
/// store backs the session is reported separately.
fn detect_model(harness: &str, transcript: &Path) -> Option<String> {
    match harness {
        "claude" => detect_claude_model(transcript),
        "codex" => detect_codex_model(transcript),
        "pi" => detect_pi_model(transcript),
        _ => None,
    }
}

/// `<claude store>/<cwd-slug>/<session>.jsonl`: assistant messages carry
/// `message.model` beside the turn's `perTurnEffort` (or the session-wide
/// `effort`); the newest one wins, combined as `model#effort` when both
/// exist. Entries Claude Code writes itself for API errors carry the model
/// `<synthetic>` and no effort, so they are skipped.
fn detect_claude_model(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines().rev() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(model) = value["message"]["model"].as_str() else {
            continue;
        };
        if model == "<synthetic>" {
            continue;
        }
        return Some(
            match value["perTurnEffort"]
                .as_str()
                .or_else(|| value["effort"].as_str())
            {
                Some(effort) => format!("{model}#{effort}"),
                None => model.to_string(),
            },
        );
    }
    None
}

/// `$CODEX_HOME/sessions/<y>/<m>/<d>/rollout-*<session>.jsonl` (falling back
/// to `~/.codex`): the newest `turn_context` payload carries `model` and
/// `effort`; combined as `model#effort` when both exist. Older effort fields
/// remain readable for compatibility.
fn detect_codex_model(file: &Path) -> Option<String> {
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

/// The OpenCode store's answer for one session: whether it holds a row for
/// the id, and the model that row records when it parses. Both stable and
/// preview store names are checked. `sqlite3` is an optional best-effort
/// reader: its absence resolves no row without making `arc env` fail.
fn detect_opencode_record(session: &str) -> (SessionResolution, Option<String>) {
    let session = session.replace('\'', "''");
    let query = format!("SELECT model FROM session WHERE id = '{session}' LIMIT 1;");
    for path in session_store::opencode_databases().into_iter().flatten() {
        if !path.is_file() {
            continue;
        }
        let output = match Command::new("sqlite3")
            .arg("-noheader")
            .arg(&path)
            .arg(&query)
            .output()
        {
            Ok(output) => output,
            Err(_) => continue,
        };
        if !output.status.success() {
            continue;
        }
        let Ok(raw) = String::from_utf8(output.stdout) else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() {
            // A successful query returning nothing is this database holding
            // no row for the id, not an answer about the session.
            continue;
        }
        return (SessionResolution::Corroborated, parse_opencode_model(raw));
    }
    (SessionResolution::Uncorroborated, None)
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
fn detect_pi_model(file: &Path) -> Option<String> {
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

pub(crate) fn shell_quote(value: &str) -> String {
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
    let journal = journal::context_for_change(
        ctx,
        &state.slug,
        state.from_fork.as_ref().map(|fork| fork.slug.as_str()),
    )?;

    if json || get.is_some() || fields.is_some() {
        commands::print_projected(
            serde_json::to_value(ResumeOutput {
                schema: "arc-resume/5",
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
            let drift = status
                .report
                .brief
                .as_ref()
                .and_then(|brief| brief.base_drift.as_ref())
                .and_then(BriefBaseDrift::annotation)
                .unwrap_or_default();
            println!("- Base revision: `{base_revision}`{drift}\n");
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
    if let Some(fork) = &status.report.from_fork {
        println!("\n## From Fork\n");
        println!("- Fork: {} (`{}`)", fork.slug, fork.branch);
        println!("- Source base: `{}`", fork.base);
        println!("- Source head: `{}`", fork.head);
        println!("- Source tree: `{}`", fork.tree);
        println!(
            "- The link records where the work came from. It is not review coverage: \
             this change's own patchset, gates, and verdict are what integration reads."
        );
    }
    // Before findings: a rejected approach is worth more to a cold session
    // than an open defect, because nothing else will stop it being re-tried.
    println!("\n## Kept Context\n");
    if status.report.kept.is_empty() {
        println!("- (none kept)");
    } else {
        for kept in &status.report.kept {
            // Flattened: a stored newline must not break the bullet list or
            // inject a heading into the section that follows.
            println!(
                "- **{}** — {}{}",
                kept.kind.as_str(),
                crate::render::one_line(&kept.body),
                kept.evidence
                    .as_deref()
                    .map(|evidence| format!(" _(evidence: {})_", crate::render::one_line(evidence)))
                    .unwrap_or_default()
            );
        }
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

/// Branches and worktrees no open change and no active fork own. This is
/// where work disappears: nothing in the ledger or the journal names such a
/// ref, so every queue reports empty while it holds commits or edits.
///
/// The scan reads refs, `git worktree list`, and each candidate's index stat
/// data only. No tree is walked and no file content is read, because this
/// runs on every catchup and a check that reads trees is a check somebody
/// turns off.
pub(crate) fn unowned_surface(ctx: &Ctx) -> Result<crate::inbox::Unowned> {
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let forks = crate::commands::fork::list_entries(ctx).unwrap_or_default();
    let Ok(Some(target)) = gitio::primary_worktree_branch(&ctx.cwd) else {
        return Ok(crate::inbox::Unowned::default());
    };

    let mut owned_branches: BTreeSet<String> = BTreeSet::new();
    let mut owned_worktrees: BTreeSet<PathBuf> = BTreeSet::new();
    for state in states.values().filter(|state| !state.is_closed()) {
        owned_branches.insert(state.branch.clone());
        if let Some(worktree) = state.worktree.as_deref() {
            owned_worktrees.insert(canonical_or_owned(Path::new(worktree)));
        }
    }
    for fork in forks.iter().filter(|fork| fork.retired.is_none()) {
        owned_branches.insert(fork.branch.clone());
        if let Some(worktree) = fork.worktree.as_deref() {
            owned_worktrees.insert(canonical_or_owned(Path::new(worktree)));
        }
    }
    owned_branches.insert(target.clone());

    let now = chrono::Utc::now();
    let mut unowned = crate::inbox::Unowned::default();
    for tip in gitio::branch_tips(&ctx.cwd).unwrap_or_default() {
        if owned_branches.contains(&tip.name) {
            continue;
        }
        let worktree = gitio::worktree_for_branch(&ctx.cwd, &tip.name)
            .ok()
            .flatten()
            .map(|path| path.display().to_string());
        let merged = gitio::is_ancestor(&ctx.cwd, &tip.name, &target).unwrap_or(false);
        let (behind, ahead) = gitio::divergence(&ctx.cwd, &target, &tip.name)
            .map(|(behind, ahead)| (Some(behind), Some(ahead)))
            .unwrap_or((None, None));
        let age_days = tip
            .committed_at
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
            .map(|committed| (now - committed).num_days().max(0) as u64);
        let action = if merged {
            format!("git branch -d {}", tip.name)
        } else if worktree.is_some() {
            format!("arc begin <slug> --adopt {}", tip.name)
        } else {
            format!("arc fork adopt <slug> --branch {}", tip.name)
        };
        let row = crate::inbox::UnownedBranch {
            name: tip.name,
            age_days,
            ahead,
            behind,
            worktree,
            action,
        };
        if merged {
            unowned.merged_branches.push(row);
        } else {
            unowned.branches.push(row);
        }
    }

    let primary = gitio::primary_worktree(&ctx.cwd).ok();
    for entry in gitio::worktree_inventory(&ctx.cwd).unwrap_or_default() {
        if entry.prunable || primary.as_deref() == Some(entry.path.as_path()) {
            continue;
        }
        if owned_worktrees.contains(&canonical_or_owned(&entry.path)) {
            continue;
        }
        let (dirty_files, untracked_files) = match gitio::worktree_dirt(&entry.path) {
            Ok((dirty, untracked)) => (Some(dirty), Some(untracked)),
            Err(_) => (None, None),
        };
        let action = match &entry.branch {
            Some(branch) => format!("arc begin <slug> --adopt {branch}"),
            None => format!("git worktree remove {}", entry.path.display()),
        };
        unowned.worktrees.push(crate::inbox::UnownedWorktree {
            path: entry.path.display().to_string(),
            branch: entry.branch,
            dirty_files,
            untracked_files,
            action,
        });
    }
    unowned.branches.sort_by(|a, b| a.name.cmp(&b.name));
    unowned.merged_branches.sort_by(|a, b| a.name.cmp(&b.name));
    unowned.worktrees.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(unowned)
}
