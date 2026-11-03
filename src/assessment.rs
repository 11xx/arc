//! A routing assessment as arc reads it: `agent-routing assess --json`.
//!
//! Arc validates the shape and keeps the answer. It never computes a family,
//! a tier or a floor, and it never stores the numbers the assessment carries:
//! what survives is the independence verdict, the findings and clearance
//! answers where the schema carries them, the qualifier names, the route
//! version, whether the reviewer it describes is the auditor that recorded
//! it, and which author coordinates of a range its authors leave out.

use crate::model::{
    model_effort, validate_route_version, AuditorBinding, Independence, ReviewQualifier,
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

/// The schema whose answer is `independence` alone.
pub const SCHEMA: &str = "agent-routing-assessment/1";
/// The schema that adds `findings` and `clears` beside `independence`, which
/// keeps its `/1` meaning.
pub const SCHEMA_V2: &str = "agent-routing-assessment/2";

/// What arc keeps of one assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub independence: Independence,
    /// Whether the reviewer may record findings against the authors; absent
    /// on a `/1` assessment.
    pub findings: Option<Independence>,
    /// Whether the reviewer's read clears the authors' work; absent on a
    /// `/1` assessment.
    pub clears: Option<bool>,
    /// Empty when the reviewer is eligible and, on `/2`, clears.
    pub qualifiers: Vec<ReviewQualifier>,
    pub route_version: Option<String>,
    reviewer: Reviewer,
    authors: Vec<Author>,
}

/// An author an assessment was made against, as far as arc compares it with
/// the coordinates a range names. Never stored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
struct Author {
    /// Every model string routing accepts for the author.
    #[serde(default)]
    identities: Vec<String>,
    /// The effort the author was assessed at. Absent, routing read the
    /// model's highest floor, which covers the model at any effort.
    #[serde(default)]
    effort: Option<String>,
}

impl Author {
    /// Whether this entry answers for `coordinate`, a `model[#effort]`: the
    /// model is among its identities, and its effort is the coordinate's or
    /// unnamed. A coordinate without an effort is covered only by an entry
    /// that names none.
    fn covers(&self, coordinate: &str) -> bool {
        let (model, _) = coordinate.split_once('#').unwrap_or((coordinate, ""));
        self.identities.iter().any(|identity| identity == model)
            && match self.effort.as_deref() {
                None => true,
                Some(assessed) => model_effort(coordinate) == Some(assessed),
            }
    }
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
    /// The `model[#effort]` coordinates that no author of this assessment
    /// answers for, in the order given and without repeats.
    pub fn uncovered<'a>(&self, coordinates: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut uncovered = Vec::<String>::new();
        for coordinate in coordinates {
            if !self.authors.iter().any(|author| author.covers(coordinate))
                && !uncovered.iter().any(|seen| seen == coordinate)
            {
                uncovered.push(coordinate.to_string());
            }
        }
        uncovered
    }

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

/// Only the fields arc keeps are named; every tier number is ignored, and an
/// author is read for the model strings it answers to.
#[derive(Deserialize)]
struct Raw {
    schema: String,
    independence: Independence,
    #[serde(default)]
    findings: Option<Independence>,
    #[serde(default)]
    clears: Option<bool>,
    #[serde(default)]
    qualifiers: Vec<String>,
    #[serde(default)]
    route_version: Option<String>,
    #[serde(default)]
    reviewer: Reviewer,
    #[serde(default)]
    authors: Vec<Author>,
}

pub fn read(path: &Path) -> Result<Assessment> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read assessment {}", path.display()))?;
    parse(&text).with_context(|| format!("assessment {}", path.display()))
}

pub fn parse(text: &str) -> Result<Assessment> {
    let raw: Raw = serde_json::from_str(text).context("not an agent-routing assessment")?;
    let second = match raw.schema.as_str() {
        SCHEMA => false,
        SCHEMA_V2 => true,
        other => bail!("schema {other:?} is neither {SCHEMA} nor {SCHEMA_V2}"),
    };
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
    let (findings, clears) = if second {
        let findings = raw
            .findings
            .context("an agent-routing-assessment/2 names findings: eligible or ineligible")?;
        let clears = raw
            .clears
            .context("an agent-routing-assessment/2 names clears: true or false")?;
        check_second(
            raw.independence,
            findings,
            clears,
            &qualifiers,
            &raw.reviewer,
        )?;
        (Some(findings), Some(clears))
    } else {
        if raw.findings.is_some() || raw.clears.is_some() {
            bail!("findings and clears belong to {SCHEMA_V2}, not {SCHEMA}");
        }
        match (raw.independence, qualifiers.is_empty()) {
            (Independence::Eligible, false) => {
                bail!("an eligible assessment names no qualifiers, but it lists some")
            }
            (Independence::Ineligible, true) => {
                bail!("an ineligible assessment must name at least one qualifier")
            }
            _ => {}
        }
        (None, None)
    };
    if let Some(version) = &raw.route_version {
        validate_route_version(version)?;
    }
    Ok(Assessment {
        independence: raw.independence,
        findings,
        clears,
        qualifiers,
        route_version: raw.route_version,
        reviewer: raw.reviewer,
        authors: raw.authors,
    })
}

/// The answers a `/2` assessment may give together. A read that clears is
/// eligible on every answer, names no qualifier, and names the effort its
/// reviewer was assessed at; every refusal names why.
fn check_second(
    independence: Independence,
    findings: Independence,
    clears: bool,
    qualifiers: &[ReviewQualifier],
    reviewer: &Reviewer,
) -> Result<()> {
    if clears {
        if independence == Independence::Ineligible || findings == Independence::Ineligible {
            bail!("an assessment that clears is eligible for independence and findings");
        }
        if !qualifiers.is_empty() {
            bail!("an assessment that clears names no qualifiers, but it lists some");
        }
        if reviewer.effort.is_none() {
            bail!("an assessment that clears names the effort its reviewer was assessed at");
        }
    } else if qualifiers.is_empty() {
        bail!("an assessment that does not clear must name at least one qualifier");
    }
    if independence == Independence::Ineligible && qualifiers.is_empty() {
        bail!("an ineligible assessment must name at least one qualifier");
    }
    Ok(())
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

    fn second(independence: &str, findings: &str, clears: bool, qualifiers: &str) -> String {
        format!(
            r#"{{"schema":"agent-routing-assessment/2","independence":"{independence}",
            "findings":"{findings}","clears":{clears},"qualifiers":{qualifiers},
            "reviewer":{{"identities":["m"],"effort":"high"}},"authors":[]}}"#
        )
    }

    /// Claim: `/2` keeps `independence` with its `/1` meaning beside
    /// `findings` and `clears`, and `/1` leaves both absent. Rejects a reader
    /// that drops the clearance answer or invents one for `/1`.
    #[test]
    fn the_second_schema_keeps_findings_and_clears_beside_independence() {
        let findings_only = parse(&second(
            "eligible",
            "eligible",
            false,
            r#"["below-clearance"]"#,
        ))
        .unwrap();
        assert_eq!(findings_only.independence, Independence::Eligible);
        assert_eq!(findings_only.findings, Some(Independence::Eligible));
        assert_eq!(findings_only.clears, Some(false));
        assert_eq!(findings_only.qualifiers, [ReviewQualifier::BelowClearance]);
        let clearing = parse(&second("eligible", "eligible", true, "[]")).unwrap();
        assert_eq!(clearing.clears, Some(true));
        let first = parse(&text("eligible", "[]")).unwrap();
        assert_eq!((first.findings, first.clears), (None, None));
    }

    /// Claim: a `/2` answer that contradicts itself is refused, never read as
    /// clearing. Rejects a clearing read that is ineligible, qualified, or
    /// names no reviewer effort, and a refusal that says no reason.
    #[test]
    fn the_second_schema_refuses_contradictory_answers() {
        for refused in [
            second("ineligible", "eligible", true, r#"["same-family"]"#),
            second("eligible", "ineligible", true, "[]"),
            second("eligible", "eligible", true, r#"["below-clearance"]"#),
            second("eligible", "eligible", false, "[]"),
            second("eligible", "eligible", true, "[]").replace(r#","effort":"high""#, ""),
            second("eligible", "eligible", true, "[]").replace(r#""clears":true,"#, ""),
            second("eligible", "eligible", true, "[]").replace(r#""findings":"eligible","#, ""),
            text("eligible", "[]").replace(r#""qualifiers""#, r#""clears":true,"qualifiers""#),
        ] {
            assert!(parse(&refused).is_err(), "{refused}");
        }
    }

    fn against(identities: &str, effort: &str) -> Assessment {
        parse(&text("eligible", "[]").replace(
            r#""authors":[{"author_floor":3}]"#,
            &format!(r#""authors":[{{"identities":{identities},"effort":{effort}}}]"#),
        ))
        .unwrap()
    }

    /// Claim: an author entry covers a coordinate at its own effort, or at
    /// any effort when it names none. Rejects coverage by model name alone.
    #[test]
    fn an_author_entry_covers_its_model_at_its_effort() {
        let low = against(r#"["gpt-6-astra"]"#, r#""low""#);
        assert!(low.uncovered(["gpt-6-astra#low"]).is_empty());
        assert_eq!(low.uncovered(["gpt-6-astra#high"]), ["gpt-6-astra#high"]);
        assert_eq!(low.uncovered(["gpt-6-astra"]), ["gpt-6-astra"]);
        let any = against(r#"["gpt-6-astra"]"#, "null");
        assert!(any
            .uncovered(["gpt-6-astra#high", "gpt-6-astra"])
            .is_empty());
        assert_eq!(any.uncovered(["other#high"]), ["other#high"]);
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
