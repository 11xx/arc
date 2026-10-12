//! A routing assessment as arc reads it: `agent-routing assess --json`.
//!
//! Arc validates the shape and keeps the answer. It never computes a family,
//! a tier or a floor, and it never stores the numbers the assessment carries:
//! what survives is the independence verdict, the qualifier names, the route
//! version and whether the reviewer it describes is the auditor that recorded
//! it.

use crate::model::{
    model_effort, validate_route_version, AuditorBinding, Independence, ReviewQualifier,
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

/// The schema this reader accepts.
pub const SCHEMA: &str = "agent-routing-assessment/1";

/// What arc keeps of one assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub independence: Independence,
    /// Empty exactly when the reviewer is eligible.
    pub qualifiers: Vec<ReviewQualifier>,
    pub route_version: Option<String>,
    reviewer: Reviewer,
}

/// The reviewer an assessment describes, as far as arc compares it with an
/// auditor. Never stored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
struct Reviewer {
    /// Every model string routing accepts for the reviewer.
    #[serde(default)]
    identities: Vec<String>,
    #[serde(default)]
    effort: Option<String>,
}

impl Assessment {
    /// Whether this assessment describes the auditor whose audit event
    /// recorded `model`, a model with an optional `#effort` suffix.
    pub fn bind(&self, model: Option<&str>) -> AuditorBinding {
        if self.reviewer.identities.is_empty() {
            return AuditorBinding::IdentitiesUnstated;
        }
        let Some(model) = model else {
            return AuditorBinding::ModelUnrecorded;
        };
        let (name, _) = model.split_once('#').unwrap_or((model, ""));
        if !self
            .reviewer
            .identities
            .iter()
            .any(|identity| identity == name)
        {
            return AuditorBinding::ModelMismatch;
        }
        match (model_effort(model), self.reviewer.effort.as_deref()) {
            (Some(recorded), Some(assessed)) if recorded != assessed => {
                AuditorBinding::EffortMismatch
            }
            _ => AuditorBinding::Bound,
        }
    }
}

/// Only the fields arc keeps are named; the authors and every tier number are
/// ignored.
#[derive(Deserialize)]
struct Raw {
    schema: String,
    independence: Independence,
    #[serde(default)]
    qualifiers: Vec<String>,
    #[serde(default)]
    route_version: Option<String>,
    #[serde(default)]
    reviewer: Reviewer,
}

pub fn read(path: &Path) -> Result<Assessment> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read assessment {}", path.display()))?;
    parse(&text).with_context(|| format!("assessment {}", path.display()))
}

pub fn parse(text: &str) -> Result<Assessment> {
    let raw: Raw = serde_json::from_str(text).context("not an agent-routing assessment")?;
    if raw.schema != SCHEMA {
        bail!("schema {:?} is not {SCHEMA}", raw.schema);
    }
    let mut qualifiers = Vec::new();
    for name in &raw.qualifiers {
        let qualifier =
            <ReviewQualifier as clap::ValueEnum>::from_str(name, false).map_err(|_| {
                anyhow::anyhow!(
                    "unknown qualifier {name:?}; the closed set is {}",
                    ReviewQualifier::NAMES
                )
            })?;
        qualifiers.push(qualifier);
    }
    let qualifiers = ReviewQualifier::normalized(qualifiers);
    match (raw.independence, qualifiers.is_empty()) {
        (Independence::Eligible, false) => {
            bail!("an eligible assessment names no qualifiers, but it lists some")
        }
        (Independence::Ineligible, true) => {
            bail!("an ineligible assessment must name at least one qualifier")
        }
        _ => {}
    }
    if let Some(version) = &raw.route_version {
        validate_route_version(version)?;
    }
    Ok(Assessment {
        independence: raw.independence,
        qualifiers,
        route_version: raw.route_version,
        reviewer: raw.reviewer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(independence: &str, qualifiers: &str) -> String {
        format!(
            r#"{{"schema":"agent-routing-assessment/1","independence":"{independence}",
            "qualifiers":{qualifiers},"route_version":"2026.10.12",
            "reviewer":{{"model":"m","review_tier":2}},"authors":[{{"author_floor":3}}]}}"#
        )
    }

    #[test]
    fn keeps_only_the_answer() {
        let kept = parse(&text(
            "ineligible",
            r#"["same-family","below-floor","same-family"]"#,
        ))
        .unwrap();
        assert_eq!(kept.independence, Independence::Ineligible);
        assert_eq!(
            kept.qualifiers,
            [ReviewQualifier::SameFamily, ReviewQualifier::BelowFloor]
        );
        assert_eq!(kept.route_version.as_deref(), Some("2026.10.12"));
    }

    fn bound_to(identities: &str, effort: &str) -> Assessment {
        parse(&text("eligible", "[]").replace(
            r#""reviewer":{"model":"m","review_tier":2}"#,
            &format!(r#""reviewer":{{"identities":{identities},"effort":{effort}}}"#),
        ))
        .unwrap()
    }

    #[test]
    fn binds_a_model_among_the_identities_at_the_effort_both_name() {
        let assessed = bound_to(r#"["claude-opus-5-5","opus-5.5"]"#, r#""xhigh""#);
        assert_eq!(assessed.bind(Some("opus-5.5#xhigh")), AuditorBinding::Bound);
        assert_eq!(assessed.bind(Some("opus-5.5")), AuditorBinding::Bound);
        assert_eq!(
            assessed.bind(Some("opus-5.5#low")),
            AuditorBinding::EffortMismatch
        );
        assert_eq!(
            assessed.bind(Some("sonnet-5.5#xhigh")),
            AuditorBinding::ModelMismatch
        );
        assert_eq!(assessed.bind(None), AuditorBinding::ModelUnrecorded);
        let effortless = bound_to(r#"["opus-5.5"]"#, "null");
        assert_eq!(effortless.bind(Some("opus-5.5#low")), AuditorBinding::Bound);
    }

    #[test]
    fn an_assessment_without_identities_binds_nothing() {
        assert_eq!(
            parse(&text("eligible", "[]")).unwrap().bind(Some("m")),
            AuditorBinding::IdentitiesUnstated
        );
        assert_eq!(
            bound_to("[]", "null").bind(None),
            AuditorBinding::IdentitiesUnstated
        );
    }

    #[test]
    fn refuses_what_the_shape_does_not_allow() {
        assert!(parse(&text("ineligible", r#"["nope"]"#))
            .unwrap_err()
            .to_string()
            .contains("nope"));
        assert!(parse(&text("ineligible", "[]")).is_err());
        assert!(parse(&text("eligible", r#"["same-model"]"#)).is_err());
        assert!(parse(&text("eligible", "[]")).is_ok());
        assert!(parse("{}").is_err());
        assert!(parse(&text("eligible", "[]").replace("/1", "/2")).is_err());
        assert!(parse(&text("eligible", "[]").replace("2026.10.12", "a b")).is_err());
    }
}
