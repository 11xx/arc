//! A routing assessment as arc reads it: `agent-routing assess --json`.
//!
//! Arc validates the shape and keeps the answer. It never computes a family,
//! a tier or a floor, and it never stores the numbers the assessment carries:
//! what survives is the independence verdict, the qualifier names and the
//! route version.

use crate::model::{validate_route_version, Independence, ReviewQualifier};
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
}

/// Only the fields arc keeps are named; the reviewer, the authors and every
/// tier number are ignored.
#[derive(Deserialize)]
struct Raw {
    schema: String,
    independence: Independence,
    #[serde(default)]
    qualifiers: Vec<String>,
    #[serde(default)]
    route_version: Option<String>,
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
