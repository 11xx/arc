//! The dated model-match exception, shared by recording and read projections.

use crate::{
    model::DebtProduction,
    policy::TiebreakReviewer,
    state::{ChangeState, Patchset},
};
use chrono::{DateTime, Utc};

pub struct Reader<'a> {
    pub actor: &'a str,
    pub target: Option<&'a str>,
    pub assumed: bool,
    pub session: Option<&'a str>,
    pub model: Option<&'a str>,
    pub observed: Option<&'a str>,
    pub at: DateTime<Utc>,
    pub after_integration: bool,
}

/// Only the model-match test is waived. A recorded election is evaluated
/// against its read's immutable coordinates, time and ledger authorship;
/// subsequent policy edits cannot grant or revoke that read's exception.
pub fn tiebreak<'a>(
    state: &ChangeState,
    subject: &Patchset,
    production: Option<&DebtProduction>,
    reader: &Reader<'_>,
    entries: &'a [TiebreakReviewer],
) -> Option<&'a TiebreakReviewer> {
    let session = session_of(reader.actor, reader.session);
    if reader.target != Some(crate::declarations::accepted_target(state))
        || reader.assumed
        || crate::coordinate::is_person(reader.actor)
        || subject.contributor_match(reader.actor).is_some()
        || state
            .authoring_identities(subject, production)
            .iter()
            .any(|author| {
                author.effective_actor() == reader.actor
                    || session.is_some_and(|session| {
                        session_of(author.effective_actor(), author.session.as_deref())
                            == Some(session)
                    })
            })
    {
        return None;
    }
    let recorded = reader.model?;
    if reader.observed.is_some_and(|observed| observed != recorded) {
        return None;
    }
    let authors = state.authoring_model_values(subject, production);
    let registry = state
        .review_context
        .as_ref()
        .map(|context| context.registry(reader.after_integration));
    let version = registry
        .and_then(|registry| registry.ladder.as_ref())
        .and_then(|ladder| ladder.version_at(reader.at));
    if !mixed_authorship(&authors, recorded, version) {
        return None;
    }
    entries.iter().find(|entry| {
        entry.validate().is_ok()
            && entry.model == recorded
            && reader.at >= entry.since
            && entry.until.is_none_or(|until| reader.at < until)
    })
}

fn session_of<'a>(actor: &'a str, fallback: Option<&'a str>) -> Option<&'a str> {
    crate::coordinate::session_subject(actor)
        .map(|(_, id, _)| id)
        .or(fallback)
}

fn author_model(value: &str) -> Option<&str> {
    let value = value
        .split_once(':')
        .map_or(value, |(_, model)| model)
        .trim();
    if crate::coordinate::model_defect(value, None).is_some()
        || value.contains(':')
        || value.matches('#').count() > 1
        || value.chars().any(char::is_whitespace)
    {
        return None;
    }
    value.split('#').next()
}

fn canonical_model<'a>(
    name: &'a str,
    version: Option<&'a crate::review_ladder::Version>,
) -> &'a str {
    version
        .and_then(|version| version.model_named(name))
        .map_or(name, |model| model.model.as_str())
}

fn mixed_authorship(
    authors: &[String],
    reader: &str,
    version: Option<&crate::review_ladder::Version>,
) -> bool {
    let reader = canonical_model(reader.split('#').next().unwrap_or(reader).trim(), version);
    authors
        .iter()
        .filter_map(|author| author_model(author))
        .any(|author| canonical_model(author, version) != reader)
}

impl ChangeState {
    pub fn verdict_tiebreak<'a>(
        &self,
        verdict: &'a crate::state::VerdictEntry,
    ) -> Option<&'a TiebreakReviewer> {
        let subject = self
            .patchsets
            .iter()
            .find(|subject| subject.id == verdict.patchset_id)?;
        tiebreak(
            self,
            subject,
            self.debt.as_ref().and_then(|debt| debt.production.as_ref()),
            &Reader {
                actor: verdict.effective_author(),
                target: verdict.tiebreak_target.as_deref(),
                assumed: verdict.author_assumed(),
                session: verdict.session.as_deref(),
                model: verdict.model.as_deref(),
                observed: verdict.observed_model.as_deref(),
                at: verdict.created_at,
                after_integration: crate::review_context::after_integration(
                    self,
                    &verdict.event_id,
                    false,
                ),
            },
            std::slice::from_ref(verdict.tiebreak_reviewer.as_ref()?),
        )
    }

    pub fn audit_tiebreak<'a>(
        &self,
        audit: &'a crate::state::AuditVerdictEntry,
    ) -> Option<&'a TiebreakReviewer> {
        tiebreak(
            self,
            self.debt_subject_patchset()
                .or_else(|| self.latest_patchset())?,
            self.debt.as_ref().and_then(|debt| debt.production.as_ref()),
            &Reader {
                actor: audit.effective_author(),
                target: audit.tiebreak_target.as_deref(),
                assumed: audit.author_assumed(),
                session: audit.session.as_deref(),
                model: audit.model.as_deref(),
                observed: audit.observed_model.as_deref(),
                at: audit.created_at,
                after_integration: true,
            },
            std::slice::from_ref(audit.tiebreak_reviewer.as_ref()?),
        )
    }
}

/// The election is an operator remedy only for mixed-model work.
pub fn remedy(authors: &[String], reader: &str) -> &'static str {
    if mixed_authorship(authors, reader, None) {
        "; for mixed-model work, elect a dated model#effort in [policy] tiebreak_reviewers on the target branch or in operator policy"
    } else {
        ""
    }
}
