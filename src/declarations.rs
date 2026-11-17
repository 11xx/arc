//! The gate and policy declarations a change is judged by.
//!
//! Integration answers to the branch it merges into. A change that edits
//! `.arc/gates.toml` or `.arc/policy.toml` is therefore judged by the target's
//! declarations, read at the target's current head while the change is open
//! and at the target revision its closure recorded once it is closed,
//! wherever the command is typed. The gates the change's own tree adds are
//! owed on top; the operator's files live outside every tree and apply in
//! every reading.

use crate::gates::{self, GatesFile};
use crate::gitio;
use crate::policy::{self, PolicyFile};
use crate::state::ChangeState;
use anyhow::Result;
use std::fs;
use std::path::Path;

/// What readiness, verification and integration require of one change.
#[derive(Debug)]
pub struct Declarations {
    pub review_context: Option<crate::review_context::ReviewContext>,
    pub gates: GatesFile,
    pub policy: PolicyFile,
    /// One line per way the declarations in play disagree, so a refusal never
    /// rests on a declaration nobody was told about.
    pub notes: Vec<String>,
    /// The target branch could not be resolved. Nothing then says what the
    /// change owes, so the gates and policy here are a display fallback and
    /// no decision may rest on them.
    pub target_unreadable: bool,
}

/// The declarations `state` is judged by, from any checkout of its repository.
///
/// Required gates are the target's plus those the change's head adds; policy
/// is the target's. An open change's heads are its branch and target where
/// they stand. A closed change's are the head it is judged at and the target
/// revision its closure recorded, else the target's current head; neither its
/// branch nor this checkout is read, so declarations edited after closure do
/// not change what it answered to. When the target cannot be resolved the
/// result says so and carries only what the change's own head declares, for
/// display: a change is never judged by declarations it could have written.
pub fn for_change(cwd: &Path, state: &ChangeState) -> Result<Declarations> {
    for_change_at(cwd, state, None)
}

/// Read destination declarations at an explicit revision when supplied.
/// The branch name still identifies the destination in diagnostics.
pub(crate) fn for_change_at(
    cwd: &Path,
    state: &ChangeState,
    target_revision: Option<&str>,
) -> Result<Declarations> {
    let toplevel = gitio::toplevel(cwd)?;
    let change_head = if state.is_closed() {
        state.closed_head().map(str::to_string)
    } else {
        gitio::branch_head(cwd, &state.branch).ok()
    };
    let target_head = authorizing_revision(cwd, state, target_revision);
    let Ok(target_head) = target_head else {
        let own = change_head
            .and_then(|head| gates::inspect_at(cwd, &head).ok())
            .unwrap_or_default();
        return Ok(Declarations {
            gates: own,
            policy: PolicyFile::default(),
            notes: Vec::new(),
            target_unreadable: true,
            review_context: crate::review_context::load(cwd, state, target_revision),
        });
    };
    let target = &state.target_branch;
    let mut required = gates::load_at(cwd, &target_head)?;
    required.suspend_budgets(crate::gate_suspension::in_force(
        &state.budget_suspensions,
        chrono::Utc::now(),
    ));
    let mut notes = Vec::new();
    if let Some(change_head) = change_head {
        crate::gate_raise::apply(cwd, state, &target_head, &change_head, &mut required)?;
        let own = gates::inspect_at(cwd, &change_head)?;
        for (name, gate) in &mut required.gates {
            gate.head_removes = !own.gates.contains_key(name);
        }
        if required.confine != own.confine {
            notes.push(format!("confinement differs: {target} declares {:?}; {} declares {:?}; destination writable roots apply", required.confine, state.branch, own.confine));
        }
        let divergences = required.owe_also(
            own,
            &format!("{target}'s .arc/gates.toml"),
            &format!("{}'s .arc/gates.toml", state.branch),
        );
        notes.extend(divergences.iter().map(gates::GateDivergence::describe));
        if let Err(error) =
            crate::gate_raise::advise(cwd, state, &target_head, &change_head, &mut required)
        {
            notes.push(format!("raise allowance unavailable: {error:#}"));
        }
    }
    if !state.is_closed() {
        notes.extend(checkout_notes(cwd, &toplevel, target, &target_head)?);
    }
    Ok(Declarations {
        gates: required,
        policy: policy::load_at(cwd, &target_head)?,
        notes,
        target_unreadable: false,
        review_context: crate::review_context::load(cwd, state, target_revision),
    })
}

/// Where the checkout at `toplevel` declares something the target does not,
/// naming the reading readiness takes instead.
fn checkout_notes(
    cwd: &Path,
    toplevel: &Path,
    target: &str,
    target_head: &str,
) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for file in [".arc/gates.toml", ".arc/policy.toml"] {
        let here = match fs::read_to_string(toplevel.join(file)) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if here != gitio::file_at(cwd, target_head, file)? {
            notes.push(format!(
                "this checkout's {file} differs from {target}'s; readiness answers for {target}'s"
            ));
        }
    }
    if let (Ok(here), Ok(there)) = (
        gates::inspect(toplevel),
        gates::inspect_at(cwd, target_head),
    ) {
        for (name, gate) in &here.gates {
            match there.gates.get(name) {
                None => notes.push(format!(
                    "gate {name:?} is declared in this checkout and not by {target}"
                )),
                Some(other) if gate.command != other.command => notes.push(format!(
                    "gate {name:?} runs {:?} in this checkout and {:?} on {target}; {target}'s is evaluated",
                    gate.command, other.command
                )),
                Some(_) => {}
            }
        }
        for name in there
            .gates
            .keys()
            .filter(|name| !here.gates.contains_key(*name))
        {
            notes.push(format!(
                "gate {name:?} is declared by {target} and absent from this checkout; it is still required"
            ));
        }
    }
    Ok(notes)
}

/// Integration authority is frozen at a recorded closure's destination basis.
pub(crate) fn authorizing_revision(
    cwd: &Path,
    state: &ChangeState,
    target_revision: Option<&str>,
) -> Result<String> {
    match target_revision.map(str::to_string).or_else(|| {
        state
            .closure
            .as_ref()
            .and_then(|closure| closure.target_before.clone())
    }) {
        Some(recorded) => Ok(recorded),
        None => gitio::branch_head(cwd, &state.target_branch),
    }
}

/// The ladder authorizing this integration, from the same revision as policy.
pub(crate) fn authorizing_ladder(
    cwd: &Path,
    state: &ChangeState,
    target_revision: Option<&str>,
) -> Result<Option<crate::review_ladder::Ladder>> {
    crate::review_ladder::load_at(cwd, &authorizing_revision(cwd, state, target_revision)?)
}

/// The branch whose registry governs reads after integration.
pub(crate) fn accepted_target(state: &ChangeState) -> &str {
    state
        .closure
        .as_ref()
        .and_then(|closure| closure.target_branch.as_deref())
        .unwrap_or(&state.target_branch)
}

/// The accepted registry follows the destination branch even after closure.
pub(crate) fn accepted_ladder(
    cwd: &Path,
    target_branch: &str,
) -> Result<Option<crate::review_ladder::Ladder>> {
    crate::review_ladder::load_at(cwd, &gitio::branch_head(cwd, target_branch)?)
}

#[cfg(test)]
mod ladder_tests {
    use super::*;
    use crate::model::Event;
    use serde_json::{json, Value};

    fn event(id: &str, payload: Value) -> Event {
        let mut value = json!({
            "schema_version": crate::model::SCHEMA_VERSION,
            "event_id": id, "repository_id": "fixture", "change_id": "registry",
            "actor": "fixture", "created_at": "2026-10-10T00:00:00Z"
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let mut options = vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.org",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ];
        options.extend(args);
        gitio::git(cwd, &options).unwrap()
    }

    fn commit_gap(cwd: &Path, id: &str) -> String {
        fs::create_dir_all(cwd.join(".arc")).unwrap();
        fs::write(cwd.join(crate::review_ladder::FILE), format!(
            "schema = \"arc-review-ladder/1\"\n[[version]]\nid = \"{id}\"\neffective_from = \"2026-10-07T00:00:00Z\"\nsource = \"fixture gap\"\ngap = true\n"
        )).unwrap();
        git(cwd, &["add", ".arc/review-ladder.toml"]);
        git(cwd, &["commit", "-m", "test: capability gap"]);
        git(cwd, &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// Claim: authorization shares policy's closure basis, while accepted
    /// registry reads follow the target head. Rejects either reading using
    /// the candidate, or freezing later registry reads at closure.
    #[test]
    fn authorization_and_accepted_registry_use_distinct_target_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "-b", "master"]);
        let before = commit_gap(cwd, "before");
        let opened = event(
            "open",
            json!({
                "event_type": "change-opened", "slug": "registry", "title": "Registry",
                "profile": "local", "target_branch": "master", "branch": "arc/registry",
                "base": before, "blocked_by": [], "tags": []
            }),
        );
        let open = crate::state::reduce(std::slice::from_ref(&opened)).unwrap();
        assert_eq!(
            authorizing_ladder(cwd, &open, None)
                .unwrap()
                .unwrap()
                .version[0]
                .id,
            "before"
        );
        let after = commit_gap(cwd, "after");
        let integrated = event(
            "integrated",
            json!({
                "event_type": "change-integrated", "integrated_commit": after,
                "source_patchset_id": "ps-01", "source_head": before,
                "target_branch": "master", "target_before": before
            }),
        );
        let closed = crate::state::reduce(&[opened, integrated]).unwrap();
        assert_eq!(
            authorizing_ladder(cwd, &closed, None)
                .unwrap()
                .unwrap()
                .version[0]
                .id,
            "before"
        );
        assert_eq!(
            accepted_ladder(cwd, &closed.target_branch)
                .unwrap()
                .unwrap()
                .version[0]
                .id,
            "after"
        );
        assert_eq!(
            authorizing_ladder(cwd, &closed, Some(&after))
                .unwrap()
                .unwrap()
                .version[0]
                .id,
            "after"
        );
        let selected = for_change_at(cwd, &closed, Some(&before)).unwrap();
        assert_eq!(
            selected
                .review_context
                .as_ref()
                .map(|context| context.authorizing.version_ids.clone()),
            Some(vec!["before".to_string()])
        );
        assert_eq!(
            selected
                .review_context
                .as_ref()
                .map(|context| context.accepted.version_ids.clone()),
            Some(vec!["after".to_string()])
        );
    }
}
