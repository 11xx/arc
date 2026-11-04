//! The dated model-match exception, shared by recording and read projections.

use crate::{
    model::DebtProduction,
    policy::TiebreakReviewer,
    state::{ChangeState, Patchset},
};
use chrono::{DateTime, Utc};

pub struct Reader<'a> {
    pub actor: &'a str,
    pub assumed: bool,
    pub session: Option<&'a str>,
    pub model: Option<&'a str>,
    pub observed: Option<&'a str>,
    pub at: DateTime<Utc>,
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
    if reader.assumed
        || crate::coordinate::is_person(reader.actor)
        || subject.contributor_match(reader.actor).is_some()
        || state
            .authoring_identities(subject, production)
            .iter()
            .any(|author| {
                author.effective_actor() == reader.actor
                    || reader
                        .session
                        .is_some_and(|session| author.session.as_deref() == Some(session))
            })
    {
        return None;
    }
    let recorded = reader.model?;
    if reader.observed.is_some_and(|observed| observed != recorded) {
        return None;
    }
    let authors = state.authoring_model_values(subject, production);
    // Different efforts of one model do not establish mixed authorship.
    if !authors.iter().any(|author| {
        crate::trailers::naming_model(std::slice::from_ref(author), recorded).is_none()
    }) {
        return None;
    }
    entries.iter().find(|entry| {
        entry.validate().is_ok()
            && entry.model == recorded
            && reader.at >= entry.since
            && entry.until.is_none_or(|until| reader.at < until)
    })
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
                assumed: verdict.author_assumed(),
                session: verdict.session.as_deref(),
                model: verdict.model.as_deref(),
                observed: verdict.observed_model.as_deref(),
                at: verdict.created_at,
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
                assumed: audit.author_assumed(),
                session: audit.session.as_deref(),
                model: audit.model.as_deref(),
                observed: audit.observed_model.as_deref(),
                at: audit.created_at,
            },
            std::slice::from_ref(audit.tiebreak_reviewer.as_ref()?),
        )
    }
}

/// The election is an operator remedy only for mixed-model work.
pub fn remedy(authors: &[String], reader: &str) -> &'static str {
    if authors
        .iter()
        .any(|author| crate::trailers::naming_model(std::slice::from_ref(author), reader).is_none())
    {
        "; for mixed-model work, elect a dated model#effort in [policy] tiebreak_reviewers on the target branch or in operator policy"
    } else {
        ""
    }
}
