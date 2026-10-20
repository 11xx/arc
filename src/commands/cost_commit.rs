//! The cost view of a commit, and the local Git note that caches its text.
//!
//! A commit has no cost of its own. It resolves to the change that records it,
//! and the change's `arc-cost/1` document is embedded unchanged: every figure
//! is shared by all of the change's commits and none is split among them. Each
//! role trailer on the commit is mapped to the sessions the ledger records for
//! that role, and a trailer no recorded session answers says so.
//!
//! A note holds the text form of this view under `refs/notes/arc-cost`. It is
//! a cache of one read, never a source: the ledger and its retained facts stay
//! the truth, and a refresh replaces the note with a newer read. arc never
//! pushes the notes ref or adds it to a refspec.

use super::cost::{emit, read_cost, report_text, write_totals, Acting, CostView, TAPES_READER};
use super::*;
use crate::model::portable_cost_metadata;
use crate::state::ChangeState;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const COMMIT_SCHEMA: &str = "arc-cost-commit/1";

/// The notes ref's short name: `git notes --ref=arc-cost`.
const NOTES: &str = "arc-cost";

/// Where a commit stands among its change's commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The 1-based position among the current patchset's commits.
    Commit(usize),
    /// The commit that integrated the change, outside the patchset's range.
    Integration,
    /// A commit of a patchset the change has since replaced.
    Earlier,
}

/// The change that records a commit.
struct Located {
    state: ChangeState,
    /// The commits of the change's current patchset, oldest first.
    commits: Vec<String>,
    place: Place,
}

/// The commits `base..head` of a change's current patchset, oldest first.
fn patchset_commits(ctx: &Ctx, state: &ChangeState) -> Vec<String> {
    let Some((base, head)) = patchset_range(state) else {
        return Vec::new();
    };
    gitio::git(
        &ctx.cwd,
        &["rev-list", "--reverse", &format!("{base}..{head}")],
    )
    .map(|listed| listed.lines().map(str::to_string).collect())
    .unwrap_or_default()
}

fn integrated_commit(state: &ChangeState) -> Option<&str> {
    state
        .closure
        .as_ref()
        .and_then(|closure| closure.integrated_commit.as_deref())
}

/// Every commit a change holds a note for: its current patchset's commits and
/// the commit that integrated it.
fn change_commits(ctx: &Ctx, state: &ChangeState) -> Vec<String> {
    let mut commits = patchset_commits(ctx, state);
    if let Some(integrated) = integrated_commit(state) {
        if !commits.iter().any(|commit| commit == integrated) {
            commits.push(integrated.to_string());
        }
    }
    commits
}

fn place_in(state: &ChangeState, commits: &[String], commit: &str) -> Place {
    match commits.iter().position(|held| held == commit) {
        Some(index) => Place::Commit(index + 1),
        None if integrated_commit(state) == Some(commit) => Place::Integration,
        None => Place::Earlier,
    }
}

/// The current patchset range of a change: its base and head.
fn patchset_range(state: &ChangeState) -> Option<(&str, &str)> {
    let patchset = state.patchsets.last()?;
    let base = patchset.merge_base.as_ref().unwrap_or(&patchset.base);
    Some((base.as_str(), patchset.head.as_str()))
}

/// Every readable change's state and the commit graph its patchset ranges
/// span, read once for every commit a command locates, with the changes whose
/// state could not be read: any of them might record a commit.
struct Changes {
    states: Vec<ChangeState>,
    unreadable: Vec<String>,
    /// Absent when the one-pass graph read failed: each range is then walked
    /// on its own, as `rev-list` answers it.
    graph: Option<gitio::CommitGraph>,
}

impl Changes {
    fn read(ctx: &Ctx) -> Result<Self> {
        let store = ctx.store()?;
        let read = store.read()?;
        let mut states = Vec::new();
        let mut unreadable = Vec::new();
        for change_id in read.change_ids()? {
            match read.state(&change_id) {
                Ok(state) => states.push(state),
                Err(error) => {
                    eprintln!("warning: unreadable change {change_id}: {error:#}");
                    unreadable.push(change_id);
                }
            }
        }
        let revisions = states
            .iter()
            .filter_map(patchset_range)
            .flat_map(|(base, head)| [base, head]);
        let graph = gitio::CommitGraph::reachable_from(&ctx.cwd, revisions).ok();
        Ok(Self {
            states,
            unreadable,
            graph,
        })
    }

    /// How far a lookup over these changes is authoritative: absent when every
    /// change was read, otherwise its status and the unreadable changes.
    fn lookup(&self, matched: bool) -> Option<Value> {
        if self.unreadable.is_empty() {
            return None;
        }
        Some(json!({
            "status": if matched { "may-not-be-unique" } else { "unresolved" },
            "unreadable": self.unreadable,
            "statement": if matched {
                "a change whose state could not be read might also record this commit, so the match may not be unique"
            } else {
                "no readable change records this commit, and a change whose state could not be read might"
            },
        }))
    }

    /// The answer for a commit no readable change records: a no-match only
    /// when every change was read.
    fn no_match(&self) -> String {
        if self.unreadable.is_empty() {
            "no change records this commit".to_string()
        } else {
            format!(
                "no readable change records this commit; unreadable changes might: {}",
                self.unreadable.join(", ")
            )
        }
    }

    /// The changes whose current patchset range `base..head` holds `commit`.
    /// A range whose ends name no commit holds none, as `rev-list` lists none
    /// for it.
    fn ranges_holding(&self, ctx: &Ctx, commit: &str) -> Vec<&ChangeState> {
        let Some(graph) = &self.graph else {
            return self
                .states
                .iter()
                .filter(|state| {
                    patchset_commits(ctx, state)
                        .iter()
                        .any(|held| held == commit)
                })
                .collect();
        };
        let descendants = graph.descendants(commit);
        self.states
            .iter()
            .filter(|state| {
                patchset_range(state).is_some_and(|(base, head)| {
                    match (graph.tips.get(base), graph.tips.get(head)) {
                        (Some(base), Some(head)) => {
                            descendants.contains(head) && !descendants.contains(base)
                        }
                        _ => false,
                    }
                })
            })
            .collect()
    }
}

/// The change that records `commit`, as `query_commit` finds it: a patchset
/// head or the integrated commit decides first, and otherwise a commit inside
/// a change's current patchset range belongs to that change.
fn locate(ctx: &Ctx, changes: &Changes, commit: &str) -> Result<Option<Located>> {
    let exact: Vec<&ChangeState> = changes
        .states
        .iter()
        .filter(|state| {
            state
                .patchsets
                .iter()
                .any(|patchset| patchset.head == commit)
                || integrated_commit(state) == Some(commit)
        })
        .collect();
    let mut found = if exact.is_empty() {
        changes.ranges_holding(ctx, commit)
    } else {
        exact
    };
    if found.len() > 1 {
        let ids: Vec<&str> = found.iter().map(|state| state.change_id.as_str()).collect();
        bail!(
            "commit {commit} belongs to several changes ({}); name one with `arc cost show <change>`",
            ids.join(", ")
        );
    }
    Ok(found.pop().map(|state| {
        let commits = patchset_commits(ctx, state);
        let place = place_in(state, &commits, commit);
        Located {
            state: state.clone(),
            commits,
            place,
        }
    }))
}

fn subject(message: &[u8]) -> String {
    String::from_utf8_lossy(message)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The role a session's own events give it that answers a trailer, beside the
/// executors bound to the change's runs in that role.
fn acting_role(trailer: &str) -> &'static str {
    match trailer {
        "Reviewed-by" => "reviewer",
        _ => "orchestrator",
    }
}

/// One session behind a trailer, with its whole figure as the change view
/// shares it and the status word the change's table gives its row.
fn session_entry(report: &Value, change_key: &str, key: &str, kind: &str) -> Value {
    let row = report["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["key"] == key);
    let Some(row) = row else {
        return json!({"key": key, "kind": kind, "status": "unmeasured", "measurement": "unmeasured"});
    };
    let status = report["table"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["key"] == key)
        .map_or(Value::Null, |row| row["status"].clone());
    let item = report["work"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["key"] == change_key);
    let listed = |field: &Value| {
        field
            .as_array()
            .is_some_and(|keys| keys.iter().any(|held| held == key))
    };
    let divided = item.is_some_and(|item| listed(&item["direct"]["shares"]));
    let shared = divided || item.is_some_and(|item| listed(&item["shared"]));
    let mut entry = json!({
        "key": key,
        "kind": row["kind"],
        "per_run": row["per_run"],
        "shared": shared,
        "status": status,
        "measurement": row["measurement"]["status"],
        "source": row["measurement"]["source"],
        "totals": row["totals"],
    });
    if divided {
        if let Some(share) = row["split"]["shares"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|share| share["work"] == change_key)
        {
            entry["share"] = share["totals"].clone();
        }
    }
    entry
}

/// The sessions the ledger records behind one role trailer.
fn trailer_row(trailer: &str, value: &str, cost: &CostView, change_key: &str) -> Value {
    let model = crate::trailers::coordinate_model(value);
    let mut keys: BTreeMap<String, &str> = BTreeMap::new();
    let mut unmeasured: BTreeMap<String, String> = BTreeMap::new();
    let mut sources: BTreeMap<String, &str> = BTreeMap::new();
    let mut unestablished: Vec<Value> = Vec::new();
    if let Some(model) = model {
        let executor_role = match trailer {
            "Implemented-by" => Some("implementer"),
            "Reviewed-by" => Some("reviewer"),
            _ => None,
        };
        for run in cost
            .runs
            .iter()
            .filter(|run| run.work == change_key && Some(run.role) == executor_role)
        {
            if run.model.as_deref() == Some(model) {
                for session in &run.sessions {
                    keys.insert(session.clone(), "executor");
                }
            }
        }
        if trailer != "Implemented-by" {
            let role = acting_role(trailer);
            let harness = value.trim().split_once(':').map(|(harness, _)| harness);
            for acting in cost
                .acting
                .iter()
                .filter(|acting| acting.roles.contains(role))
            {
                // An executor its binding already matched is listed once.
                if keys.contains_key(&acting.key) {
                    continue;
                }
                match acting_model(acting, role) {
                    Some((models, source)) if models.contains(model) => {
                        keys.insert(acting.key.clone(), acting.kind);
                        sources.insert(acting.key.clone(), source);
                        if let Some(reason) = &acting.unmeasured {
                            unmeasured.insert(acting.key.clone(), reason.clone());
                        }
                    }
                    None if harness
                        .is_some_and(|harness| acting.key.starts_with(&format!("{harness}:"))) =>
                    {
                        unestablished.push(json!({"key": acting.key, "reason": UNESTABLISHED}));
                    }
                    _ => {}
                }
            }
        }
    }
    let sessions: Vec<Value> = keys
        .iter()
        .map(|(key, kind)| {
            let mut entry = session_entry(&cost.report, change_key, key, kind);
            if let Some(reason) = unmeasured.get(key) {
                entry["reason"] = json!(reason);
            }
            if let Some(source) = sources.get(key) {
                entry["model_source"] = json!(source);
            }
            entry
        })
        .collect();
    let mut row = json!({
        "trailer": trailer,
        "value": value,
        "model": model,
        "status": if sessions.is_empty() { "no-session-recorded" } else { "recorded" },
        "sessions": sessions,
    });
    if !unestablished.is_empty() {
        row["unestablished"] = json!(unestablished);
    }
    row
}

/// Why a session holding a trailer's role by its events is not matched to it.
const UNESTABLISHED: &str = "no model of its own is established";

/// The models that answer for a session in `role`, with their source: those
/// its own events, or the subject recorded on its behalf, declared in the
/// role, else the one its recording says served it. The invoker's model on
/// an event recorded on its behalf is never its own.
fn acting_model<'a>(acting: &'a Acting, role: &str) -> Option<(BTreeSet<&'a str>, &'static str)> {
    match acting.models.get(role).filter(|models| !models.is_empty()) {
        Some(models) => Some((models.iter().map(String::as_str).collect(), "events")),
        None => acting
            .served
            .as_deref()
            .map(|served| (BTreeSet::from([served]), "recording")),
    }
}

fn shared_statement(commits: usize) -> String {
    format!("figures are shared by the change's {commits} commits; no figure is per commit")
}

/// The view of `commit`, one of `located`'s change's commits, over the
/// change's own cost read.
fn view_of(ctx: &Ctx, commit: &str, located: &Located, cost: &CostView) -> Result<Value> {
    let change_key = format!("change:{}", located.state.change_id);
    let revisions: Vec<&str> = std::iter::once(commit)
        .chain(located.commits.iter().map(String::as_str))
        .collect();
    let read = gitio::read_commits(&ctx.cwd, &revisions)?;
    let roles: Vec<Value> = crate::trailers::contribution_trailers(&read[0].message)
        .iter()
        .map(|(trailer, value)| trailer_row(trailer, value, cost, &change_key))
        .collect();
    let commits: Vec<Value> = located
        .commits
        .iter()
        .zip(&read[1..])
        .map(|(sha, held)| json!({"sha": sha, "subject": subject(&held.message)}))
        .collect();
    let of = located.commits.len();
    let position = match located.place {
        Place::Commit(index) => json!({"index": index, "of": of}),
        Place::Integration => json!({"index": null, "of": of, "role": "integration"}),
        Place::Earlier => json!({"index": null, "of": of, "role": "earlier-patchset"}),
    };
    Ok(json!({
        "schema": COMMIT_SCHEMA,
        "commit": commit,
        "subject": subject(&read[0].message),
        "change": {
            "id": located.state.change_id,
            "title": located.state.title,
            "status": change_status(&located.state),
            "integrated_commit": integrated_commit(&located.state),
        },
        "position": position,
        "commits": commits,
        "statement": shared_statement(of),
        "roles": roles,
        "cost": cost.report.clone(),
    }))
}

/// The view of any commit: the change's figures, or the answer that no
/// change records it.
fn commit_view(ctx: &Ctx, revision: &str, prices: Option<&std::path::Path>) -> Result<Value> {
    let commit = gitio::rev_parse(&ctx.cwd, revision)
        .with_context(|| format!("{revision} is not a commit"))?;
    let changes = Changes::read(ctx)?;
    let mut view = match locate(ctx, &changes, &commit)? {
        Some(located) => {
            let cost = read_cost(ctx, &located.state.change_id, prices)?;
            view_of(ctx, &commit, &located, &cost)?
        }
        None => json!({
            "schema": COMMIT_SCHEMA,
            "commit": commit,
            "change": null,
            "answer": changes.no_match(),
        }),
    };
    if let Some(lookup) = changes.lookup(!view["change"].is_null()) {
        view["lookup"] = lookup;
    }
    Ok(view)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// What a note may say of a trailer value: one that is not portable text is
/// named and withheld.
fn trailer_value(value: &str, detail: bool) -> String {
    if detail || portable_cost_metadata(value) {
        value.to_string()
    } else {
        "(value withheld: not portable)".to_string()
    }
}

/// The text form of a commit view. Without `detail`, no free-form text of the
/// commit or the change (subjects, title) appears: a note rides with its
/// commit and carries only what the ledger's figures admit for transport.
fn view_text(view: &Value, detail: bool) -> String {
    let mut out = String::new();
    let sha = view["commit"].as_str().unwrap_or_default();
    let free = |value: &Value| match value.as_str() {
        Some(text) if detail => format!(" {text}"),
        _ => String::new(),
    };
    emit!(out, "commit {}{}", short(sha), free(&view["subject"]));
    if view["lookup"]["status"] == "may-not-be-unique" {
        emit!(
            out,
            "this match may not be unique: unreadable changes might also record the commit: {}",
            view["lookup"]["unreadable"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if view["change"].is_null() {
        emit!(out, "{}", view["answer"].as_str().unwrap_or_default());
        return out;
    }
    let change = &view["change"];
    emit!(
        out,
        "change {} [{}]{}",
        change["id"].as_str().unwrap_or_default(),
        change["status"].as_str().unwrap_or_default(),
        free(&change["title"])
    );
    let of = view["position"]["of"].as_u64().unwrap_or_default();
    match (
        view["position"]["index"].as_u64(),
        view["position"]["role"].as_str(),
    ) {
        (Some(index), _) => emit!(out, "commit {index} of {of}"),
        (None, Some("integration")) => {
            emit!(out, "the integration commit of the change's {of} commits")
        }
        _ => emit!(
            out,
            "a commit of an earlier patchset; the change now holds {of} commits"
        ),
    }
    for (index, held) in view["commits"].as_array().into_iter().flatten().enumerate() {
        let sibling = held["sha"].as_str().unwrap_or_default();
        emit!(
            out,
            "  {} {}{}{}",
            index + 1,
            short(sibling),
            free(&held["subject"]),
            if sibling == sha {
                "  (this commit)"
            } else {
                ""
            }
        );
    }
    emit!(out, "{}", view["statement"].as_str().unwrap_or_default());
    emit!(out, "roles");
    for role in view["roles"].as_array().into_iter().flatten() {
        emit!(
            out,
            "  {} {}",
            role["trailer"].as_str().unwrap_or_default(),
            trailer_value(role["value"].as_str().unwrap_or_default(), detail)
        );
        let sessions = role["sessions"].as_array().cloned().unwrap_or_default();
        let unestablished = role["unestablished"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if sessions.is_empty() && unestablished.is_empty() {
            emit!(out, "    no session recorded");
        }
        for unmatched in &unestablished {
            let key = &unmatched["key"];
            let kind = view["cost"]["sessions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| &row["key"] == key)
                .and_then(|row| row["kind"].as_str())
                .unwrap_or("participant");
            emit!(
                out,
                "    {kind} {}  unmatched: {}",
                key.as_str().unwrap_or_default(),
                unmatched["reason"].as_str().unwrap_or_default()
            );
        }
        for session in &sessions {
            emit!(
                out,
                "    {} {}  {}{}",
                session["kind"].as_str().unwrap_or_default(),
                session["key"].as_str().unwrap_or_default(),
                session["status"].as_str().unwrap_or_default(),
                session["reason"]
                    .as_str()
                    .map_or(String::new(), |reason| format!(": {reason}"))
            );
            if session["model_source"] == "recording" {
                emit!(out, "      model from its recording");
            }
            if session["totals"].is_object() {
                write_totals(&mut out, "      ", "whole", &session["totals"]);
            }
            if session["share"].is_object() {
                write_totals(&mut out, "      ", "share of the change", &session["share"]);
            }
        }
    }
    emit!(out, "cost");
    out.push_str(&report_text(&view["cost"]));
    out
}

/// Print the cost view of a commit.
pub fn show_commit_cost(
    ctx: &Ctx,
    revision: &str,
    prices: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    let view = commit_view(ctx, revision, prices)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print!("{}", view_text(&view, true));
    }
    Ok(())
}

/// A note's text may carry no path separator and no control character, the
/// same bar retained cost facts pass for transport.
fn check_portable(text: &str) -> Result<()> {
    if text.lines().all(portable_cost_metadata) {
        Ok(())
    } else {
        bail!("note text is not portable: a line carries a path separator or a control character")
    }
}

/// What a note holds past its opening line, which names when it was read.
fn note_body(view: &Value) -> String {
    let mut body = String::new();
    emit!(
        body,
        "a dollar figure is a harness-calculated estimate on the accounting basis shown beside it, never a charge; the ledger and its retained facts are the source, and this note is a cache of one read"
    );
    body.push_str(&view_text(view, false));
    body
}

fn note_text(view: &Value) -> Result<String> {
    let text = format!(
        "arc cost as of {} reader {TAPES_READER}\n{}",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        note_body(view)
    );
    check_portable(&text)?;
    Ok(text)
}

/// The note's text past its opening line.
fn beyond_opening(text: &str) -> &str {
    text.split_once('\n')
        .map_or("", |(_, rest)| rest)
        .trim_end()
}

/// Write the note on each commit a change holds, or, with `all`, refresh every
/// noted commit whose text a newer read changes.
pub fn note_cost(ctx: &Ctx, target: Option<&str>, all: bool) -> Result<()> {
    let store = ctx.store()?;
    let mut states: BTreeMap<String, ChangeState> = BTreeMap::new();
    // Each commit to note, whether a refresh may leave it as it is, and
    // whether a commit lookup found its change.
    let mut jobs: BTreeMap<String, (String, bool, bool)> = BTreeMap::new();
    // Every change is read once however many commits are located.
    let held = std::cell::OnceCell::new();
    let changes = || -> Result<&Changes> {
        if let Some(changes) = held.get() {
            return Ok(changes);
        }
        let read = Changes::read(ctx)?;
        Ok(held.get_or_init(|| read))
    };
    if let Some(target) = target {
        let mut looked_up = false;
        let state = match store.resolve_change(target) {
            Ok(change_id) => store.read()?.state(&change_id)?,
            Err(_) => {
                let commit = gitio::rev_parse(&ctx.cwd, target)
                    .with_context(|| format!("{target} is neither a change nor a commit"))?;
                let changes = changes()?;
                looked_up = true;
                match locate(ctx, changes, &commit)? {
                    Some(located) => located.state,
                    None => bail!("{}", changes.no_match()),
                }
            }
        };
        for commit in change_commits(ctx, &state) {
            jobs.insert(commit, (state.change_id.clone(), false, looked_up));
        }
        states.insert(state.change_id.clone(), state);
    }
    if all {
        for commit in gitio::noted_commits(&ctx.cwd, NOTES)? {
            if jobs.contains_key(&commit) {
                continue;
            }
            let changes = changes()?;
            match locate(ctx, changes, &commit)? {
                Some(located) => {
                    let change_id = located.state.change_id.clone();
                    states.entry(change_id.clone()).or_insert(located.state);
                    jobs.insert(commit, (change_id, true, true));
                }
                None if changes.unreadable.is_empty() => println!(
                    "no change records {}: its note is left as it is",
                    short(&commit)
                ),
                None => println!(
                    "no readable change records {}: its note is left as it is; unreadable changes might: {}",
                    short(&commit),
                    changes.unreadable.join(", ")
                ),
            }
        }
    }
    let mut costs: BTreeMap<String, CostView> = BTreeMap::new();
    let mut refused = 0usize;
    for (commit, (change_id, refresh, looked_up)) in &jobs {
        if !costs.contains_key(change_id) {
            costs.insert(change_id.clone(), read_cost(ctx, change_id, None)?);
        }
        let state = &states[change_id];
        let commits = patchset_commits(ctx, state);
        let located = Located {
            place: place_in(state, &commits, commit),
            state: state.clone(),
            commits,
        };
        let mut view = view_of(ctx, commit, &located, &costs[change_id])?;
        if let Some(lookup) = held
            .get()
            .filter(|_| *looked_up)
            .and_then(|changes| changes.lookup(true))
        {
            view["lookup"] = lookup;
        }
        let text = match note_text(&view) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("{}: {error}", short(commit));
                refused += 1;
                continue;
            }
        };
        let unchanged = *refresh
            && gitio::read_note(&ctx.cwd, NOTES, commit)?
                .is_some_and(|held| beyond_opening(&held) == beyond_opening(&text));
        if unchanged {
            println!("unchanged: {}", short(commit));
        } else {
            gitio::set_note(&ctx.cwd, NOTES, commit, &text)?;
            println!("written: {}", short(commit));
        }
    }
    if refused > 0 {
        bail!("{refused} notes refused: their text is not portable");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_text_with_a_path_separator_or_control_character_is_refused() {
        assert!(check_portable("a line\nanother line\n").is_ok());
        for line in ["a/b", "a\\b", "~", "a\u{7}b"] {
            assert!(
                check_portable(&format!("fine\n{line}\n")).is_err(),
                "{line:?}"
            );
        }
    }

    #[test]
    fn a_trailer_value_that_is_not_portable_text_is_withheld_from_a_note() {
        assert_eq!(trailer_value("pi:model#high", false), "pi:model#high");
        assert_eq!(
            trailer_value("pi:vendor/model#high", false),
            "(value withheld: not portable)"
        );
        assert_eq!(
            trailer_value("pi:vendor/model#high", true),
            "pi:vendor/model#high"
        );
    }
}
