//! Conservative authorship from completed, bound implementation runs.

use super::{run, Ctx};
use crate::gitio;
use crate::model::{DebtIdentity, Event, Payload};
use crate::rewrite::RewriteMap;
use crate::state::ChangeState;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

#[derive(Clone)]
pub(super) struct Attribution {
    pub authors: Vec<DebtIdentity>,
    pub dispatches: Vec<String>,
    pub includes_invoker: bool,
}

/// No dispatch leaves the ordinary default untouched. Incomplete evidence is
/// a diagnostic, never permission to credit an executor.
pub(super) fn from_runs(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    events: &[Event],
    base: &str,
    head: &str,
) -> Result<Option<Attribution>> {
    let repository_events = store.load_repository_events()?;
    let rounds: Vec<_> = run::rounds(&repository_events)
        .into_iter()
        .filter(|round| match &round.subject {
            Some(run::RunSubject::Change { id }) => {
                id == &st.change_id || store.resolve_change(id).is_ok_and(|id| id == st.change_id)
            }
            _ => false,
        })
        .collect();
    if rounds.is_empty() {
        return Ok(None);
    }
    let previous = events.iter().rev().find_map(|event| match &event.payload {
        Payload::PatchsetAdded { head, .. } => Some(head.as_str()),
        _ => None,
    });
    let recorded_start = previous.unwrap_or(base);
    let start = previous
        .and_then(|_| st.latest_patchset().map(|ps| ps.head.as_str()))
        .unwrap_or(base);
    if !gitio::is_ancestor(&ctx.cwd, start, head)? {
        bail!("the previous patchset or base was rewritten; use arc rebase to preserve attribution during replay");
    }
    let revisions = gitio::range_commits(&ctx.cwd, start, head)?;
    let rewrites: Vec<_> = repository_events
        .iter()
        .filter(|event| matches!(event.payload, Payload::HistoryRewritten { .. }))
        .collect();
    let mut relevant: BTreeSet<_> = revisions.iter().cloned().collect();
    relevant.insert(start.to_string());
    relevant.insert(recorded_start.to_string());
    let mut connected = relevant.clone();
    expand_rewrite_range(&mut connected, &rewrites);
    let preserving: Vec<_> = rewrites
        .iter()
        .copied()
        .filter(|event| {
            let Payload::HistoryRewritten { mapping, .. } = &event.payload else {
                unreachable!();
            };
            mapping.iter().any(|(old, new)| {
                names_revision(&connected, old)
                    || new
                        .as_deref()
                        .is_some_and(|new| names_revision(&connected, new))
            }) && preserves_run_evidence(ctx, event, &rounds)
        })
        .collect();
    let carried = RewriteMap::from_events(preserving.iter().copied())?;
    if carried.current(recorded_start)? != start {
        bail!("the previous patchset or base was rewritten; use arc rebase to preserve attribution during replay");
    }
    if revisions.is_empty() {
        return Ok(None);
    }
    expand_rewrite_range(&mut relevant, &preserving);
    let preserving_ids: BTreeSet<_> = preserving.iter().map(|event| &event.event_id).collect();
    let refusing: Vec<_> = rewrites
        .into_iter()
        .filter(|event| !preserving_ids.contains(&event.event_id))
        .collect();
    if refusing.iter().any(|event| match &event.payload {
        Payload::HistoryRewritten { mapping, .. } => {
            mapping.keys().any(|old| names_revision(&relevant, old))
        }
        _ => false,
    }) {
        bail!("a recorded rewrite touches the commit range");
    }
    let incoming_rewrite_at = refusing
        .iter()
        .filter_map(|event| {
            let Payload::HistoryRewritten { mapping, .. } = &event.payload else {
                return None;
            };
            mapping
                .values()
                .flatten()
                .any(|new| names_revision(&relevant, new))
                .then_some(event.created_at)
        })
        .max();
    let refs: Vec<_> = revisions.iter().map(String::as_str).collect();
    let commits = gitio::read_commits(&ctx.cwd, &refs)?;
    let worktree = gitio::worktree_for_branch(&ctx.cwd, &st.branch)?
        .context("the change branch has no checked-out worktree")?;
    let worktree = worktree.canonicalize()?;
    let mut authors = Vec::new();
    let mut dispatches = BTreeSet::new();
    let mut includes_invoker = false;
    for (revision, commit) in revisions.iter().zip(commits) {
        let time = committer_time(&commit)?;
        let trailers = implementers(&commit.message);
        let mut covered = Vec::new();
        for round in &rounds {
            let dispatch = round.dispatch;
            if dispatch.created_at > time
                || round
                    .ending
                    .as_ref()
                    .is_some_and(|end| end.event.created_at < time)
            {
                continue;
            }
            let binding = round
                .binding
                .as_ref()
                .with_context(|| format!("dispatch {} has no bind", dispatch.event_id))?;
            if !binding.conflicts.is_empty() {
                bail!("dispatch {} has conflicting bindings", dispatch.event_id);
            }
            if binding.role.as_deref() == Some("reviewer") {
                continue;
            }
            if incoming_rewrite_at.is_some_and(|at| at >= dispatch.created_at) {
                bail!(
                    "a recorded rewrite touches the commit range during dispatch {}",
                    dispatch.event_id
                );
            }
            let ending = round
                .ending
                .as_ref()
                .with_context(|| format!("dispatch {} has not ended", dispatch.event_id))?;
            if binding.event.created_at < dispatch.created_at
                || ending.event.created_at < dispatch.created_at
                || dispatch.repository_id != store.repository_id
                || binding.event.repository_id != store.repository_id
                || ending.event.repository_id != store.repository_id
            {
                bail!(
                    "dispatch {} has inconsistent lifecycle evidence",
                    dispatch.event_id
                );
            }
            let Payload::RunDispatched {
                worktree: dispatched_worktree,
                ..
            } = &dispatch.payload
            else {
                unreachable!();
            };
            if std::path::Path::new(dispatched_worktree)
                .canonicalize()
                .ok()
                .as_ref()
                != Some(&worktree)
            {
                bail!(
                    "dispatch {} names a different or unavailable worktree",
                    dispatch.event_id
                );
            }
            let actor = format!("{}:{}", binding.executor_harness, binding.executor_session);
            if crate::coordinate::actor_defect(&actor).is_some() {
                bail!(
                    "dispatch {} has an invalid executor identity",
                    dispatch.event_id
                );
            }
            let is_invoker = actor == ctx.actor
                || (ctx.harness.as_ref() == Some(binding.executor_harness)
                    && ctx.session.as_ref() == Some(binding.executor_session));
            if is_invoker {
                covered.push((None, dispatch.event_id.as_str()));
                continue;
            }
            let subject = dispatch.on_behalf_of.as_deref().with_context(|| {
                format!(
                    "dispatch {} has no on-behalf-of coordinate",
                    dispatch.event_id
                )
            })?;
            if crate::coordinate::coordinate_defect(subject, Some(binding.executor_session))
                .is_some()
                || !subject.starts_with(&format!("{}:", binding.executor_harness))
                || subject.contains('<')
            {
                bail!(
                    "dispatch {} has no matching executor coordinate",
                    dispatch.event_id
                );
            }
            let model = subject.split_once(':').unwrap().1.to_string();
            covered.push((
                Some(DebtIdentity::new(
                    actor,
                    Some(subject.to_string()),
                    Some(binding.executor_harness.clone()),
                    Some(model),
                    Some(binding.executor_session.clone()),
                )),
                dispatch.event_id.as_str(),
            ));
        }
        let distinct: BTreeSet<_> = covered
            .iter()
            .map(|(author, _)| author.as_ref().map(|author| (&author.actor, &author.model)))
            .collect();
        if distinct.len() > 1 {
            bail!("commit {revision} is covered by different executors");
        }
        if let Some((Some(author), _)) = covered.first() {
            if trailers != BTreeSet::from([author.effective_actor().to_string()]) {
                bail!("commit {revision} lacks matching Implemented-by corroboration");
            }
            if !authors.contains(author) {
                if authors
                    .iter()
                    .any(|held: &DebtIdentity| held.actor == author.actor)
                {
                    bail!(
                        "executor {} has different author coordinates in the commit range",
                        author.actor
                    );
                }
                authors.push(author.clone());
            }
            dispatches.extend(
                covered
                    .iter()
                    .map(|(_, id)| format!("{id} ({})", author.actor)),
            );
        } else {
            let invoker = ctx.resolve_model();
            let coordinate = ctx
                .harness
                .as_ref()
                .zip(invoker.model.as_ref())
                .map(|(harness, model)| format!("{harness}:{model}"));
            if !trailers.is_empty()
                && trailers
                    .iter()
                    .any(|value| Some(value) != coordinate.as_ref())
            {
                bail!("commit {revision} has implementer trailers outside a matching completed interval; use arc rebase to preserve attribution during replay");
            }
            includes_invoker = true;
        }
    }
    if authors.is_empty() {
        bail!("no commit is covered by a corroborated executor interval");
    }
    Ok(Some(Attribution {
        authors,
        dispatches: dispatches.into_iter().collect(),
        includes_invoker,
    }))
}

fn names_revision(revisions: &BTreeSet<String>, revision: &str) -> bool {
    let revision = crate::rewrite::canonical_revision(revision);
    revisions.contains(&revision)
        || revisions.iter().any(|held| {
            held.len() != revision.len()
                && held.len().min(revision.len()) >= crate::rewrite::ABBREVIATION_FLOOR
                && (held.starts_with(&revision) || revision.starts_with(held))
        })
}

/// Keep intermediate revisions visible so a permitted link cannot hide an
/// earlier rewrite that changed the evidence.
fn expand_rewrite_range(revisions: &mut BTreeSet<String>, events: &[&Event]) {
    loop {
        let before = revisions.len();
        for event in events {
            let Payload::HistoryRewritten { mapping, .. } = &event.payload else {
                unreachable!();
            };
            for (old, new) in mapping {
                if names_revision(revisions, old)
                    || new
                        .as_deref()
                        .is_some_and(|new| names_revision(revisions, new))
                {
                    revisions.insert(crate::rewrite::canonical_revision(old));
                    revisions.extend(
                        new.iter()
                            .map(|new| crate::rewrite::canonical_revision(new)),
                    );
                }
            }
        }
        if before == revisions.len() {
            break;
        }
    }
}

/// Recorded per-commit judgments prove complete role additions and unchanged
/// headers. Read both sides to require the same implementer set, and refuse
/// a rewrite recorded by a session bound to produce a recreated commit.
fn preserves_run_evidence(ctx: &Ctx, event: &Event, rounds: &[run::Round<'_>]) -> bool {
    let Payload::HistoryRewritten {
        mapping,
        signature_only,
        role_additions,
        ..
    } = &event.payload
    else {
        return false;
    };
    if mapping.is_empty()
        || mapping.iter().any(|(old, new)| {
            new.is_none() || (!signature_only.contains(old) && !role_additions.contains(old))
        })
    {
        return false;
    }
    let revisions: Vec<_> = mapping
        .iter()
        .flat_map(|(old, new)| [old.as_str(), new.as_deref().unwrap()])
        .collect();
    let Ok(commits) = gitio::read_commits(&ctx.cwd, &revisions) else {
        return false;
    };
    commits.chunks_exact(2).all(|pair| {
        let [before, after] = pair else {
            unreachable!();
        };
        if implementers(&before.message) != implementers(&after.message) {
            return false;
        }
        let Ok(time) = committer_time(before) else {
            return false;
        };
        !rounds.iter().any(|round| {
            let Some(binding) = &round.binding else {
                return false;
            };
            binding.role.as_deref() != Some("reviewer")
                && round.dispatch.created_at <= time
                && round
                    .ending
                    .as_ref()
                    .is_none_or(|end| end.event.created_at >= time)
                && (event.actor
                    == format!("{}:{}", binding.executor_harness, binding.executor_session)
                    || (event.harness.as_ref() == Some(binding.executor_harness)
                        && event.session.as_ref() == Some(binding.executor_session)))
        })
    })
}

fn implementers(message: &[u8]) -> BTreeSet<String> {
    crate::trailers::contribution_trailers(message)
        .into_iter()
        .filter_map(|(role, value)| (role == "Implemented-by").then_some(value))
        .collect()
}

fn committer_time(commit: &gitio::RawCommit) -> Result<DateTime<Utc>> {
    let seconds = commit
        .committer
        .date
        .split_whitespace()
        .next()
        .context("commit has no committer time")?
        .parse()?;
    DateTime::<Utc>::from_timestamp(seconds, 0).context("commit has an invalid committer time")
}

/// Report incomplete evidence once when recording an ordinary snapshot.
pub(super) fn derive(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    events: &[Event],
    base: &str,
    head: &str,
) -> Option<Attribution> {
    match from_runs(ctx, store, st, events, base, head) {
        Ok(attribution) => attribution,
        Err(error) => {
            eprintln!("attribution: recorder retained: {}; pass --contributors and --on-behalf-of, or --solo", crate::render::one_line(&format!("{error:#}")));
            None
        }
    }
}
