//! Conservative authorship from completed, bound implementation runs.

use super::{run, Ctx};
use crate::gitio;
use crate::model::{DebtIdentity, Event, Payload};
use crate::state::ChangeState;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

pub(super) struct Attribution {
    pub authors: Vec<DebtIdentity>,
    pub dispatches: Vec<String>,
    pub includes_invoker: bool,
}

/// No dispatch leaves the ordinary default untouched. Incomplete evidence is
/// a diagnostic, never permission to credit an executor.
pub(super) fn derive(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    events: &[Event],
    base: &str,
    head: &str,
) -> Option<Attribution> {
    let attempt = || -> Result<Option<Attribution>> {
        let repository_events = store.load_repository_events()?;
        let rounds: Vec<_> = run::rounds(&repository_events)
            .into_iter()
            .filter(|round| match &round.subject {
                Some(run::RunSubject::Change { id }) => {
                    id == &st.change_id
                        || store.resolve_change(id).is_ok_and(|id| id == st.change_id)
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
        let start = previous.unwrap_or(base);
        if previous.is_some_and(|head| st.latest_patchset().is_some_and(|ps| ps.head != head))
            || !gitio::is_ancestor(&ctx.cwd, start, head)?
        {
            bail!("the previous patchset or base was rewritten");
        }
        let revisions = gitio::range_commits(&ctx.cwd, start, head)?;
        if revisions.is_empty() {
            return Ok(None);
        }
        let relevant = |revision: &str| {
            revisions.iter().any(|commit| commit.starts_with(revision))
                || start.starts_with(revision)
        };
        if repository_events.iter().any(|event| match &event.payload {
            Payload::HistoryRewritten { mapping, .. } => mapping
                .iter()
                .any(|(old, new)| relevant(old) || new.as_deref().is_some_and(relevant)),
            _ => false,
        }) {
            bail!("a recorded rewrite touches the commit range");
        }
        let refs: Vec<_> = revisions.iter().map(String::as_str).collect();
        let commits = gitio::read_commits(&ctx.cwd, &refs)?;
        let worktree = gitio::worktree_for_branch(&ctx.cwd, &st.branch)?
            .context("the change branch has no checked-out worktree")?;
        let worktree = worktree.canonicalize()?;
        let mut authors = Vec::new();
        let mut dispatches = BTreeSet::new();
        let mut includes_invoker = false;
        for (revision, commit) in revisions.iter().zip(commits) {
            let seconds: i64 = commit
                .committer
                .date
                .split_whitespace()
                .next()
                .context("commit has no committer time")?
                .parse()?;
            let time = DateTime::<Utc>::from_timestamp(seconds, 0)
                .context("commit has an invalid committer time")?;
            let trailers: BTreeSet<_> = crate::trailers::contribution_trailers(&commit.message)
                .into_iter()
                .filter_map(|(role, value)| (role == "Implemented-by").then_some(value))
                .collect();
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
                    bail!("commit {revision} has implementer trailers outside a matching completed interval");
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
    };
    match attempt() {
        Ok(attribution) => attribution,
        Err(error) => {
            eprintln!("attribution: recorder retained: {}; pass --contributors and --on-behalf-of, or --solo", crate::render::one_line(&format!("{error:#}")));
            None
        }
    }
}
