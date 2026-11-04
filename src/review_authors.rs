//! Author coordinates a ladder read must answer, with missing evidence kept explicit.

use crate::model::{DebtIdentity, DebtProduction};
use crate::state::{ChangeState, Patchset};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct Authors {
    pub base: String,
    pub revision: String,
    pub status: &'static str,
    pub identities: Vec<DebtIdentity>,
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

pub fn read(
    cwd: &Path,
    state: &ChangeState,
    subject: &Patchset,
    base: &str,
    revision: &str,
) -> Authors {
    let production = state
        .debt
        .as_ref()
        .and_then(|debt| debt.production.as_ref())
        .filter(|_| {
            state
                .debt_subject_patchset()
                .is_some_and(|patchset| patchset.id == subject.id)
        });
    collect(
        state,
        subject,
        base,
        revision,
        production,
        crate::trailers::range_implementers(cwd, base, revision)
            .map_err(|error| format!("{error:#}")),
    )
}

fn collect(
    state: &ChangeState,
    subject: &Patchset,
    base: &str,
    revision: &str,
    production: Option<&DebtProduction>,
    trailers: Result<Vec<String>, String>,
) -> Authors {
    let mut identities = baseline(state, subject);
    if let Some(production) = production {
        identities.push(production.implementer.clone());
    }
    let mut missing = Vec::new();
    match trailers {
        Ok(values) => identities.extend(
            values
                .iter()
                .map(|value| crate::state::identity_from_role_value(value)),
        ),
        Err(reason) => missing.push(format!(
            "Implemented-by range {base}..{revision} unread: {reason}"
        )),
    }
    let mut unique = Vec::new();
    for identity in identities {
        if !unique.contains(&identity) {
            unique.push(identity);
        }
    }
    let mut models = Vec::new();
    for identity in &unique {
        if crate::coordinate::is_person(identity.effective_actor()) {
            continue;
        }
        match identity.model.as_deref() {
            Some(model)
                if crate::coordinate::model_recorded(Some(model), identity.session.as_deref()) =>
            {
                models.push(model.to_string());
            }
            _ => missing.push(format!(
                "author model not established for {:?}",
                identity.effective_actor()
            )),
        }
    }
    models.sort();
    models.dedup();
    missing.sort();
    missing.dedup();
    Authors {
        base: base.to_string(),
        revision: revision.to_string(),
        status: if missing.is_empty() {
            "recorded"
        } else {
            "not-established"
        },
        identities: unique,
        models,
        missing,
    }
}

fn baseline(state: &ChangeState, subject: &Patchset) -> Vec<DebtIdentity> {
    state
        .authoring_coordinates(subject)
        .into_iter()
        .map(|identity| {
            let author = identity.effective_actor();
            let carried = subject
                .inherited_authors
                .iter()
                .any(|carried| carried == &identity);
            if !carried && subject.on_behalf_of.as_deref() == Some(author) {
                if let Some((harness, session, declared)) =
                    crate::coordinate::session_subject(author)
                {
                    return DebtIdentity::new(
                        identity.actor.clone(),
                        identity.on_behalf_of.clone(),
                        Some(harness.to_string()),
                        subject
                            .model
                            .clone()
                            .or_else(|| declared.map(str::to_string)),
                        Some(session.to_string()),
                    );
                }
            }
            identity
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Event;
    use serde_json::{json, Value};

    fn event(actor: &str, model: Option<&str>, payload: Value) -> Event {
        let mut value = json!({
            "schema_version": crate::model::SCHEMA_VERSION, "event_id": actor,
            "repository_id": "fixture", "change_id": "subject", "actor": actor,
            "harness": "test", "session": "fixture-session", "model": model,
            "created_at": "2026-10-10T00:00:00Z"
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn state(
        actor: &str,
        model: Option<&str>,
        contributors: &[&str],
        behalf: Option<&str>,
    ) -> ChangeState {
        crate::state::reduce(&[
            event("opener", None, json!({
                "event_type": "change-opened", "slug": "subject", "title": "Subject",
                "profile": "local", "target_branch": "master", "branch": "arc/subject",
                "base": "b0", "blocked_by": [], "tags": []
            })),
            event("Member", Some("member#medium"), json!({
                "event_type": "patchset-added", "patchset_id": "ps-01", "base": "b0", "head": "h0"
            })),
            event(actor, model, json!({
                "event_type": "patchset-added", "patchset_id": "ps-02", "base": "b0", "head": "h1",
                "contributors": contributors, "on_behalf_of": behalf
            })),
        ]).unwrap()
    }

    fn authors(
        state: &ChangeState,
        production: Option<&DebtProduction>,
        trailers: &[&str],
    ) -> Authors {
        collect(
            state,
            state.latest_patchset().unwrap(),
            "b0",
            "h1",
            production,
            Ok(trailers.iter().map(|value| value.to_string()).collect()),
        )
    }

    /// Claim: declared contributors supply the baseline, rather than a lead
    /// recorder who is absent from the declaration.
    #[test]
    fn declared_contributors_exclude_an_unlisted_recorder() {
        let state = state("Recorder", Some("recorder#high"), &["Member"], None);
        let set = authors(&state, None, &[]);
        assert_eq!(set.models, ["member#medium"]);
        assert_eq!(set.identities[0].actor, "Member");
    }

    /// Claim: without a contributor declaration, the effective author supplies
    /// the baseline. The recorder's model cannot replace an on-behalf-of author.
    #[test]
    fn the_effective_author_is_the_undeclared_set_fallback() {
        let state = state("Recorder", Some("recorder#high"), &[], Some("Member"));
        let set = authors(&state, None, &[]);
        assert_eq!(set.models, ["member#medium"]);
    }

    /// Claim: production is an unconditional union member, even when its
    /// implementer is outside the declared contributors.
    #[test]
    fn production_outside_the_contributor_declaration_is_included() {
        let state = state("Writer", Some("writer#high"), &["Member"], None);
        let mut production = state.debt_production_for(state.latest_patchset().unwrap(), None);
        production.implementer = DebtIdentity::new(
            "Producer".into(),
            None,
            Some("test".into()),
            Some("producer#max".into()),
            None,
        );
        let set = authors(&state, Some(&production), &[]);
        assert_eq!(set.models, ["member#medium", "producer#max"]);
        assert!(set
            .identities
            .iter()
            .any(|identity| identity.actor == "Producer"));
    }

    /// Claim: every Implemented-by coordinate joins the author set, across
    /// labs and efforts; the snapshot recorder is not the whole answer.
    #[test]
    fn implemented_by_coordinates_join_the_ledger_authors() {
        let state = state("Writer", Some("writer#high"), &[], None);
        let set = authors(&state, None, &["lab-a:first#high", "lab-b:second#xhigh"]);
        assert_eq!(set.models, ["first#high", "second#xhigh", "writer#high"]);
    }

    /// Claim: an agent without a recorded model prevents establishment,
    /// even when another source supplies known coordinates.
    #[test]
    fn an_unrecorded_author_model_is_named_not_established() {
        let state = state("Writer", None, &[], None);
        let set = authors(&state, None, &["lab-a:known#high"]);
        assert_eq!(set.status, "not-established");
        assert!(set
            .missing
            .iter()
            .any(|reason| reason.contains("Writer") && reason.contains("model")));
    }

    /// Claim: malformed historical model values cannot establish an author.
    #[test]
    fn a_placeholder_author_model_is_not_established() {
        let state = state("Writer", Some("unknown"), &[], None);
        assert_eq!(authors(&state, None, &[]).status, "not-established");
    }

    /// Claim: an unknown production implementer cannot disappear behind the
    /// declaration's known contributor.
    #[test]
    fn an_unrecorded_production_model_is_not_established() {
        let state = state("Writer", Some("writer#high"), &["Member"], None);
        let mut production = state.debt_production_for(state.latest_patchset().unwrap(), None);
        production.implementer = DebtIdentity::new("Producer".into(), None, None, None, None);
        let set = authors(&state, Some(&production), &[]);
        assert_eq!(set.status, "not-established");
        assert!(set.missing.iter().any(|reason| reason.contains("Producer")));
    }

    /// Claim: person identities are retained without adding agent coordinates,
    /// even if the event also kept the recorder's model metadata.
    #[test]
    fn persons_take_part_in_identity_facts_only() {
        let state = state(
            "Person <person@example.org>",
            Some("recorder#high"),
            &[],
            None,
        );
        let set = authors(&state, None, &["Other <other@example.org>"]);
        assert!(set.models.is_empty());
        assert!(set.missing.is_empty());
        assert_eq!(set.identities.len(), 2);
    }

    /// Claim: duplicates do not add floors, but distinct identities remain
    /// available for identity checks.
    #[test]
    fn repeated_coordinates_are_deduplicated_without_losing_identities() {
        let state = state("Writer", Some("writer#high"), &[], None);
        let production = state.debt_production_for(state.latest_patchset().unwrap(), None);
        let set = authors(
            &state,
            Some(&production),
            &["test:writer#high", "test:writer#high"],
        );
        assert_eq!(set.models, ["writer#high"]);
        assert_eq!(set.identities.len(), 2);
    }

    /// Claim: unreadable ranges leave Implemented-by coverage unestablished;
    /// known ledger authors alone cannot fill that missing evidence.
    #[test]
    fn an_unreadable_implemented_by_range_is_not_established() {
        let state = state("Writer", Some("writer#high"), &[], None);
        let set = collect(
            &state,
            state.latest_patchset().unwrap(),
            "b0",
            "h1",
            None,
            Err("fixture missing revision".into()),
        );
        assert_eq!(set.status, "not-established");
        assert!(set
            .missing
            .iter()
            .any(|reason| reason.contains("Implemented-by") && reason.contains("range")));
    }

    /// Claim: malformed commit coordinates leave their missing model named.
    #[test]
    fn malformed_trailer_coordinates_do_not_vanish() {
        let state = state("Writer", Some("writer#high"), &[], None);
        let set = authors(&state, None, &["not-a-coordinate"]);
        assert_eq!(set.status, "not-established");
        assert!(set
            .missing
            .iter()
            .any(|reason| reason.contains("not-a-coordinate")));
    }
    /// Claim: model metadata of a represented native session belongs to that
    /// author, rather than the invoker or the session id embedded in its name.
    #[test]
    fn a_represented_session_keeps_its_recorded_model_coordinate() {
        let state = state(
            "Recorder",
            Some("served#high"),
            &[],
            Some("codex:019f0000-0000-7000-8000-00000000000a:declared#medium"),
        );
        let set = authors(&state, None, &[]);
        assert_eq!(set.models, ["served#high"]);
    }

    /// Claim: a represented session without a model is unestablished; its
    /// native id cannot become a model coordinate.
    #[test]
    fn a_represented_session_without_a_model_is_not_established() {
        let state = state(
            "Recorder",
            None,
            &[],
            Some("codex:019f0000-0000-7000-8000-00000000000a"),
        );
        let set = authors(&state, None, &[]);
        assert_eq!(set.status, "not-established");
        assert!(set.models.is_empty());
    }

    /// Claim: mechanical replay keeps the carried author's coordinate even
    /// when its recorder uses a different model.
    #[test]
    fn replay_keeps_carried_author_coordinates() {
        let mut state = state("Replayer", Some("replayer#xhigh"), &["Member"], None);
        state.patchsets.last_mut().unwrap().inherited_authors = vec![DebtIdentity::new(
            "Member".into(),
            None,
            Some("test".into()),
            Some("retired#high".into()),
            None,
        )];
        assert_eq!(authors(&state, None, &[]).models, ["retired#high"]);
    }
}
