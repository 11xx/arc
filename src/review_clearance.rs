//! Capability judgments under the target-owned dated ladder.

use crate::{
    model::{AuditAssessment, ReaderModel},
    review_authors::Authors,
    review_ladder::Ladder,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Judgment time and the recorder's selected ladder version. Absent time is legacy evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Basis {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis_version: Option<String>,
}

pub fn record_basis(
    ctx: &crate::commands::Ctx,
    state: &crate::state::ChangeState,
    audit: bool,
    recorded_at: DateTime<Utc>,
    receipt: Option<&crate::commands::receipt::ItemGuard>,
) -> anyhow::Result<Basis> {
    let (at, source) = match receipt {
        Some(receipt) => receipt.basis_time(ctx, recorded_at)?,
        None => (recorded_at, "recorded"),
    };
    let version = state
        .review_context
        .as_ref()
        .and_then(|context| context.registry(audit || state.is_closed()).ladder.as_ref())
        .and_then(|ladder| ladder.version_at(at))
        .map(|version| version.id.clone());
    Ok(Basis {
        basis_time: Some(at),
        basis_source: Some(source.to_string()),
        basis_version: version,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Disagreement {
    pub assessment_clears: bool,
    pub ladder_clears: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Judgment {
    pub kind: &'static str,
    pub findings_eligible: Option<bool>,
    pub clears: Option<bool>,
    pub basis_time: DateTime<Utc>,
    pub basis_source: String,
    pub basis_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conservative_bounds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessment_disagreement: Option<Disagreement>,
}

pub struct Facts<'a> {
    pub tiebreak: bool,
    pub ladder: Option<&'a Ladder>,
    pub unavailable: Option<&'a str>,
    pub authors: Option<&'a Authors>,
    pub reader: &'a str,
    pub models: &'a [ReaderModel],
    pub at: DateTime<Utc>,
    pub basis: Option<&'a Basis>,
    pub assessment: Option<&'a AuditAssessment>,
}

pub fn coordinates(recorded: Option<&str>, observed: Option<&str>) -> Vec<ReaderModel> {
    let mut models: Vec<ReaderModel> = recorded
        .map(|model| ReaderModel {
            model: model.to_string(),
            declared: None,
        })
        .into_iter()
        .collect();
    if let (Some(recorded), Some(observed)) = (recorded, observed) {
        if recorded != observed {
            models.push(ReaderModel {
                model: observed.to_string(),
                declared: Some(recorded.to_string()),
            });
        }
    }
    models
}

pub fn judge(facts: Facts<'_>) -> Judgment {
    let mut judgment = Judgment {
        kind: "not-established",
        findings_eligible: None,
        clears: None,
        basis_time: facts
            .basis
            .and_then(|basis| basis.basis_time)
            .unwrap_or(facts.at),
        basis_source: facts
            .basis
            .filter(|basis| basis.basis_time.is_some())
            .and_then(|basis| basis.basis_source.clone())
            .unwrap_or_else(|| "inferred".to_string()),
        basis_version: None,
        reason: Some("not-established"),
        details: Vec::new(),
        conservative_bounds: Vec::new(),
        assessment_disagreement: None,
    };
    evaluate(&facts, &mut judgment);
    if let Some(answer) = facts.assessment.and_then(|assessment| assessment.clears) {
        if judgment.clears.is_some_and(|clears| clears != answer)
            || (answer && judgment.clears.is_none())
        {
            judgment.assessment_disagreement = Some(Disagreement {
                assessment_clears: answer,
                ladder_clears: judgment.clears,
            });
        }
    }
    judgment
}

pub fn enforce_coordinate(
    ctx: &crate::commands::Ctx,
    state: &crate::state::ChangeState,
    audit: bool,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let reader = ctx.on_behalf_of.as_deref().unwrap_or(&ctx.actor);
    if crate::coordinate::is_person(reader) {
        return Ok(());
    }
    let Some(context) = state.review_context.as_ref() else {
        return Ok(());
    };
    let source = context.registry(audit || state.is_closed());
    if source.coordinate_required(at) {
        let model = ctx.resolve_model().model;
        if !crate::coordinate::model_recorded(model.as_deref(), ctx.session.as_deref())
            || model
                .as_deref()
                .and_then(crate::model::model_effort)
                .is_none()
        {
            anyhow::bail!("review-clearance-coordinate-required: record the reviewer model with effort using --model <model#effort>, or a session whose store establishes it");
        }
    }
    Ok(())
}

fn unknown(judgment: &mut Judgment, detail: impl Into<String>) {
    judgment.kind = "not-established";
    judgment.findings_eligible = None;
    judgment.clears = None;
    judgment.reason = Some("not-established");
    judgment.details.push(detail.into());
}

fn findings(judgment: &mut Judgment, eligible: bool, reason: &'static str, detail: String) {
    judgment.kind = "findings";
    judgment.findings_eligible = Some(eligible);
    judgment.clears = Some(false);
    judgment.reason = Some(reason);
    judgment.details.push(detail);
}

fn evaluate(facts: &Facts<'_>, judgment: &mut Judgment) {
    if let Some(error) = facts.unavailable {
        unknown(judgment, format!("ladder registry unavailable: {error}"));
        return;
    }
    let Some(ladder) = facts.ladder else {
        unknown(judgment, "ladder registry missing");
        return;
    };
    let Some(authors) = facts
        .authors
        .filter(|authors| !authors.identities.is_empty())
    else {
        unknown(judgment, "author set not established: no subject authors");
        return;
    };
    let version = ladder.version_at(judgment.basis_time);
    judgment.basis_version = version.map(|version| version.id.clone());
    let self_review = authors
        .identities
        .iter()
        .any(|identity| identity.effective_actor() == facts.reader);
    if crate::coordinate::is_person(facts.reader) {
        if authors
            .missing
            .iter()
            .any(|missing| missing.starts_with("Implemented-by range"))
        {
            unknown(
                judgment,
                "author identity set not established: Implemented-by range unread",
            );
            return;
        }
        if self_review {
            findings(
                judgment,
                true,
                "self-review",
                format!("reader {:?} is a subject author", facts.reader),
            );
            return;
        }
        judgment.kind = "clearing";
        judgment.findings_eligible = Some(true);
        judgment.clears = Some(true);
        judgment.reason = None;
        return;
    }
    if !authors.missing.is_empty() || authors.status != "recorded" {
        unknown(
            judgment,
            format!("author set not established: {}", authors.missing.join("; ")),
        );
        return;
    }
    let Some(version) = version else {
        unknown(
            judgment,
            format!(
                "basis version missing at {}",
                judgment.basis_time.to_rfc3339()
            ),
        );
        return;
    };
    if version.gap {
        unknown(judgment, format!("ladder-gap: {}", version.id));
        return;
    }
    if facts.models.is_empty() {
        unknown(judgment, "reviewer model not recorded");
        return;
    }
    let readers: Vec<_> = facts
        .models
        .iter()
        .filter_map(|model| resolve(version, &model.model, "reviewer", judgment))
        .collect();
    let authors: Vec<_> = authors
        .models
        .iter()
        .filter_map(|coordinate| resolve(version, coordinate, "author", judgment))
        .collect();
    if !judgment.details.is_empty() {
        return;
    }
    for reader in &readers {
        for author in &authors {
            if reader.model.model == author.model.model {
                if let (Some(read_effort), Some(author_effort)) = (reader.effort, author.effort) {
                    if read_effort != author_effort {
                        let (Some(read_order), Some(author_order)) =
                            (effort_order(read_effort), effort_order(author_effort))
                        else {
                            unknown(
                                judgment,
                                format!(
                                    "same-model effort order not established: {}#{} against #{}",
                                    reader.model.model, read_effort, author_effort
                                ),
                            );
                            return;
                        };
                        if read_order < author_order {
                            findings(
                                judgment,
                                false,
                                "same-model-lower-effort",
                                format!(
                                    "reviewer {}#{} is below author effort {}",
                                    reader.model.model, read_effort, author_effort
                                ),
                            );
                            return;
                        }
                    }
                }
            }
        }
    }
    for reader in &readers {
        for author in &authors {
            if reader.rank < author.floor {
                findings(
                    judgment,
                    false,
                    "below-floor",
                    format!(
                        "reviewer {} rank {} is below author {} floor {}",
                        reader.model.model, reader.rank, author.model.model, author.floor
                    ),
                );
                return;
            }
        }
    }
    for reader in &readers {
        let mixed = authors
            .iter()
            .any(|author| author.model.model != reader.model.model);
        for author in &authors {
            if !(facts.tiebreak && mixed) && reader.model.family == author.model.family {
                findings(
                    judgment,
                    true,
                    "same-family",
                    format!(
                        "reviewer {} and author {} share family {}",
                        reader.model.model, author.model.model, reader.model.family
                    ),
                );
                return;
            }
        }
    }
    if self_review {
        findings(
            judgment,
            true,
            "self-review",
            format!("reader {:?} is a subject author", facts.reader),
        );
        return;
    }
    judgment.kind = "clearing";
    judgment.findings_eligible = Some(true);
    judgment.clears = Some(true);
    judgment.reason = None;
}

struct Coordinate<'a> {
    model: &'a crate::review_ladder::Model,
    effort: Option<&'a str>,
    rank: u64,
    floor: u64,
}

fn resolve<'a>(
    version: &'a crate::review_ladder::Version,
    coordinate: &'a str,
    who: &str,
    judgment: &mut Judgment,
) -> Option<Coordinate<'a>> {
    if crate::coordinate::model_defect(coordinate, None).is_some() {
        unknown(
            judgment,
            format!("{who} coordinate not established: {coordinate:?}"),
        );
        return None;
    }
    let (name, effort) = coordinate
        .split_once('#')
        .map_or((coordinate, None), |(name, effort)| (name, Some(effort)));
    let model = version.model_named(name);
    let Some(model) = model else {
        unknown(
            judgment,
            format!("unknown {who} coordinate {coordinate:?} in {}", version.id),
        );
        return None;
    };
    let (rank, floor) = if let Some(effort) = effort {
        let Some(row) = model.efforts.iter().find(|row| row.effort == effort) else {
            unknown(
                judgment,
                format!("unknown {who} coordinate {coordinate:?} in {}", version.id),
            );
            return None;
        };
        (row.rank, row.floor)
    } else {
        let rank = model.efforts.iter().map(|row| row.rank).min().unwrap_or(0);
        let floor = model
            .efforts
            .iter()
            .map(|row| row.floor)
            .max()
            .unwrap_or(u64::MAX);
        judgment.conservative_bounds.push(if who == "reviewer" {
            format!("{who} {name} has no effort; minimum rank {rank}")
        } else {
            format!("{who} {name} has no effort; maximum floor {floor}")
        });
        (rank, floor)
    };
    Some(Coordinate {
        model,
        effort,
        rank,
        floor,
    })
}

fn effort_order(effort: &str) -> Option<u8> {
    match effort {
        "none" => Some(0),
        "minimal" => Some(1),
        "low" => Some(2),
        "medium" => Some(3),
        "high" => Some(4),
        "xhigh" => Some(5),
        "max" => Some(6),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuditorBinding, DebtIdentity, Independence, ReviewQualifier};
    use serde_json::{json, Value};

    fn table() -> Value {
        json!({"schema": "arc-review-ladder/1", "version": [{
            "id": "v1", "effective_from": "2026-10-07T00:00:00Z", "source": "fixture judgment", "gap": false,
            "model": [
                {"model":"writer","identities":["writer","writer-alias"],"family":"lab-a","superseded":false,
                 "efforts":[{"effort":"xhigh","rank":2,"floor":3},{"effort":"medium","rank":4,"floor":1}]},
                {"model":"reader","identities":["reader"],"family":"lab-b","superseded":true,
                 "efforts":[{"effort":"medium","rank":1,"floor":1},{"effort":"high","rank":4,"floor":2}]},
                {"model":"peer","identities":["peer"],"family":"lab-a","superseded":false,
                 "efforts":[{"effort":"high","rank":4,"floor":2}]},
                {"model":"bounded","identities":["bounded"],"family":"lab-c","superseded":false,
                 "efforts":[{"effort":"high","rank":2,"floor":2}]}
            ]
        }]})
    }

    fn ladder() -> Ladder {
        crate::review_ladder::parse(&table().to_string()).unwrap()
    }
    fn authors(model: &str) -> Authors {
        Authors {
            base: "base".into(),
            revision: "head".into(),
            status: "recorded",
            identities: vec![DebtIdentity::new(
                "Author".into(),
                None,
                Some("test".into()),
                Some(model.into()),
                None,
            )],
            models: vec![model.into()],
            missing: vec![],
        }
    }
    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }
    fn read(
        table: &Ladder,
        authors: &Authors,
        model: Option<&str>,
        assessment: Option<&AuditAssessment>,
    ) -> Judgment {
        judge(Facts {
            tiebreak: false,
            ladder: Some(table),
            unavailable: None,
            authors: Some(authors),
            reader: "Reader",
            models: &ReaderModel::judged(model, None),
            basis: None,
            at: at("2026-10-10T00:00:00Z"),
            assessment,
        })
    }
    fn assessment(clears: bool) -> AuditAssessment {
        AuditAssessment {
            independence: Independence::Eligible,
            findings: Some(Independence::Eligible),
            clears: Some(clears),
            binding: Some(AuditorBinding::Bound),
            qualifiers: if clears {
                vec![]
            } else {
                vec![ReviewQualifier::BelowFloor]
            },
            route_version: None,
            uncovered_authors: vec![],
        }
    }

    /// Cross-family capability needs no routing seat or assessment.
    #[test]
    fn a_known_cross_family_reader_clears_without_an_assessment() {
        let j = read(
            &ladder(),
            &authors("writer#xhigh"),
            Some("reader#high"),
            None,
        );
        assert_eq!((j.findings_eligible, j.clears), (Some(true), Some(true)));
        assert_eq!(j.basis_version.as_deref(), Some("v1"));
    }
    /// A sufficiently strong reader in the author's lab finds but cannot clear.
    #[test]
    fn same_family_is_findings_eligible_but_not_clearing() {
        let j = read(&ladder(), &authors("writer#xhigh"), Some("peer#high"), None);
        assert_eq!((j.findings_eligible, j.clears), (Some(true), Some(false)));
        assert_eq!(j.reason, Some("same-family"));
    }
    /// Every author floor must be met; a known weak coordinate is not missing evidence.
    #[test]
    fn a_known_rank_below_floor_is_ineligible() {
        let j = read(
            &ladder(),
            &authors("writer#xhigh"),
            Some("bounded#high"),
            None,
        );
        assert_eq!((j.findings_eligible, j.clears), (Some(false), Some(false)));
        assert_eq!(j.reason, Some("below-floor"));
    }
    /// Row ordering and equal rank numbers cannot authorize a lower-effort same-model read.
    #[test]
    fn lower_effort_of_the_same_model_is_explicitly_ineligible() {
        let j = read(
            &ladder(),
            &authors("writer-alias#xhigh"),
            Some("writer#medium"),
            None,
        );
        assert_eq!(j.findings_eligible, Some(false));
        assert_eq!(j.reason, Some("same-model-lower-effort"));
    }
    #[test]
    fn an_effort_outside_the_row_is_an_unknown_reviewer_coordinate() {
        let j = read(
            &ladder(),
            &authors("writer#xhigh"),
            Some("reader#max"),
            None,
        );
        assert_eq!(j.kind, "not-established");
        assert!(j
            .details
            .iter()
            .any(|d| d.contains("reviewer") && d.contains("reader#max")));
    }
    #[test]
    fn an_effort_outside_the_author_row_is_not_established() {
        let j = read(&ladder(), &authors("writer#max"), Some("reader#high"), None);
        assert!(j
            .details
            .iter()
            .any(|d| d.contains("author") && d.contains("writer#max")));
    }
    #[test]
    fn an_unrecorded_reviewer_names_its_missing_model() {
        let j = read(&ladder(), &authors("writer#xhigh"), None, None);
        assert!(j.details.iter().any(|d| d.contains("reviewer model")));
    }
    #[test]
    fn an_unestablished_author_set_never_clears() {
        let mut a = authors("writer#xhigh");
        a.status = "not-established";
        a.missing = vec!["author model not established for Author".into()];
        let j = read(&ladder(), &a, Some("reader#high"), None);
        assert!(j.details.iter().any(|d| d.contains("author model")));
    }
    #[test]
    fn a_gap_names_the_missing_capability_basis() {
        let table = crate::review_ladder::parse(
            &json!({"schema":"arc-review-ladder/1","version":[{
                "id":"gap","effective_from":"2026-01-01T00:00:00Z","source":"fixture gap","gap":true
            }]})
            .to_string(),
        )
        .unwrap();
        let j = read(&table, &authors("writer#xhigh"), Some("reader#high"), None);
        assert!(j.details.iter().any(|d| d.contains("ladder-gap")));
    }
    #[test]
    fn an_effortless_reviewer_uses_its_minimum_rank() {
        let j = read(&ladder(), &authors("writer#xhigh"), Some("reader"), None);
        assert_eq!(j.reason, Some("below-floor"));
        assert!(j
            .conservative_bounds
            .iter()
            .any(|d| d.contains("minimum rank 1")));
    }
    #[test]
    fn an_effortless_author_uses_its_maximum_floor() {
        let j = read(&ladder(), &authors("writer"), Some("bounded#high"), None);
        assert_eq!(j.reason, Some("below-floor"));
        assert!(j
            .conservative_bounds
            .iter()
            .any(|d| d.contains("maximum floor 3")));
    }
    #[test]
    fn a_version_begins_at_its_effective_instant() {
        let mut value = table();
        let mut next = value["version"][0].clone();
        next["id"] = json!("v2");
        next["effective_from"] = json!("2026-11-01T00:00:00Z");
        next["model"][1]["efforts"][1]["rank"] = json!(1);
        next["model"][3]["efforts"][0]["rank"] = json!(4);
        value["version"].as_array_mut().unwrap().push(next);
        let table = crate::review_ladder::parse(&value.to_string()).unwrap();
        let a = authors("writer#xhigh");
        let models = ReaderModel::judged(Some("reader#high"), None);
        let run = |time| {
            judge(Facts {
                tiebreak: false,
                ladder: Some(&table),
                unavailable: None,
                authors: Some(&a),
                reader: "Reader",
                models: &models,
                basis: None,
                at: at(time),
                assessment: None,
            })
        };
        assert_eq!(run("2026-10-31T23:59:59.999999999Z").clears, Some(true));
        let j = run("2026-11-01T00:00:00Z");
        assert_eq!(j.basis_version.as_deref(), Some("v2"));
        assert_eq!(j.clears, Some(false));
    }
    #[test]
    fn the_ladder_reports_both_assessment_disagreement_directions() {
        let table = ladder();
        let a = authors("writer#xhigh");
        let yes = read(&table, &a, Some("reader#high"), Some(&assessment(false)));
        assert_eq!(yes.clears, Some(true));
        assert!(!yes.assessment_disagreement.unwrap().assessment_clears);
        let no = read(&table, &a, Some("peer#high"), Some(&assessment(true)));
        assert_eq!(no.clears, Some(false));
        assert!(no.assessment_disagreement.unwrap().assessment_clears);
    }
    #[test]
    fn matching_an_author_identity_never_clears_even_across_models() {
        let a = authors("writer#xhigh");
        let models = ReaderModel::judged(Some("reader#high"), None);
        let table = ladder();
        let j = judge(Facts {
            tiebreak: false,
            ladder: Some(&table),
            unavailable: None,
            authors: Some(&a),
            reader: "Author",
            models: &models,
            basis: None,
            at: at("2026-10-10T00:00:00Z"),
            assessment: None,
        });
        assert_eq!(j.reason, Some("self-review"));
        assert_eq!(j.clears, Some(false));
    }
    #[test]
    fn an_observed_weaker_model_cannot_borrow_the_declared_rank() {
        let a = authors("writer#xhigh");
        let table = ladder();
        let models = ReaderModel::judged(Some("reader#high"), Some("bounded#high"));
        let j = judge(Facts {
            tiebreak: false,
            ladder: Some(&table),
            unavailable: None,
            authors: Some(&a),
            reader: "Reader",
            models: &models,
            basis: None,
            at: at("2026-10-10T00:00:00Z"),
            assessment: None,
        });
        assert_eq!(j.reason, Some("below-floor"));
        assert_eq!(j.clears, Some(false));
    }
}
