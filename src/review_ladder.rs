//! Dated capability tables held by a repository.

use anyhow::{bail, Context, Result};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub const SCHEMA: &str = "arc-review-ladder/1";
pub const FILE: &str = ".arc/review-ladder.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ladder {
    pub schema: String,
    pub version: Vec<Version>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub correction: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub id: String,
    pub effective_from: String,
    pub source: String,
    pub gap: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Vec<Model>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub model: String,
    pub identities: Vec<String>,
    pub family: String,
    pub superseded: bool,
    pub efforts: Vec<Effort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effort {
    pub effort: String,
    pub rank: u64,
    pub floor: u64,
}

pub fn parse(text: &str) -> Result<Ladder> {
    let ladder: Ladder = if text.trim_start().starts_with('{') {
        serde_json::from_str(text).context("invalid review ladder JSON")?
    } else {
        toml::from_str(text).context("invalid review ladder TOML")?
    };
    ladder.validate()?;
    Ok(ladder)
}

/// A missing committed file means no ladder; an invalid one is an error.
pub fn load_at(cwd: &Path, revision: &str) -> Result<Option<Ladder>> {
    crate::gitio::file_at(cwd, revision, FILE)?
        .map(|text| {
            let ladder: Ladder =
                toml::from_str(&text).with_context(|| format!("invalid {FILE} at {revision}"))?;
            ladder
                .validate()
                .with_context(|| format!("invalid {FILE} at {revision}"))?;
            Ok(ladder)
        })
        .transpose()
}

impl Ladder {
    fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            bail!("schema: expected {SCHEMA}, got {:?}", self.schema);
        }
        if !self.correction.is_empty() {
            bail!("corrections-unsupported: [[correction]] requires correction authorization");
        }
        if self.version.is_empty() {
            bail!("empty-versions: a review ladder needs at least one version");
        }
        let mut ids = BTreeSet::new();
        let mut previous = None;
        for version in &self.version {
            nonempty(&version.id, "version.id")?;
            nonempty(&version.source, "version.source")?;
            if !ids.insert(&version.id) {
                bail!("duplicate-id: {:?}", version.id);
            }
            let effective = DateTime::parse_from_rfc3339(&version.effective_from)
                .with_context(|| format!("effective_from: version {:?}", version.id))?;
            if previous.is_some_and(|time| effective <= time) {
                bail!(
                    "version-order: {:?} must start after the preceding version",
                    version.id
                );
            }
            previous = Some(effective);
            version.validate()?;
        }
        Ok(())
    }
}

impl Version {
    fn validate(&self) -> Result<()> {
        if self.gap {
            if self.model.is_some() {
                bail!("gap-with-models: {:?} must omit its model table", self.id);
            }
            return Ok(());
        }
        let models = self
            .model
            .as_deref()
            .filter(|models| !models.is_empty())
            .with_context(|| {
                format!(
                    "empty-models: {:?} needs a full model table or gap = true",
                    self.id
                )
            })?;
        let mut model_ids = BTreeSet::new();
        let mut identities = std::collections::BTreeMap::new();
        for model in models {
            nonempty(&model.model, "model.model")?;
            nonempty(&model.family, "model.family")?;
            if !model_ids.insert(&model.model) {
                bail!("duplicate-model: {:?} in {:?}", model.model, self.id);
            }
            let mut own_identities = BTreeSet::new();
            for identity in &model.identities {
                nonempty(identity, "model.identities")?;
                if !own_identities.insert(identity) {
                    bail!("duplicate-identity: {identity:?} in {:?}", self.id);
                }
            }
            own_identities.insert(&model.model);
            for identity in own_identities {
                if let Some(other) = identities.insert(identity, &model.model) {
                    bail!(
                        "duplicate-identity: {identity:?} names {other:?} and {:?} in {:?}",
                        model.model,
                        self.id
                    );
                }
            }
            if model.efforts.is_empty() {
                bail!("empty-efforts: {:?} in {:?}", model.model, self.id);
            }
            let mut efforts = BTreeSet::new();
            for row in &model.efforts {
                nonempty(&row.effort, "effort")?;
                if !efforts.insert(&row.effort) {
                    bail!(
                        "duplicate-effort: {:?}#{:?} in {:?}",
                        model.model,
                        row.effort,
                        self.id
                    );
                }
                if row.rank == 0 || row.floor == 0 {
                    bail!(
                        "positive-rank-floor: {:?}#{:?} in {:?}",
                        model.model,
                        row.effort,
                        self.id
                    );
                }
            }
        }
        for model in models {
            for row in &model.efforts {
                if !models.iter().any(|reader| {
                    reader.family != model.family
                        && reader.efforts.iter().any(|effort| effort.rank >= row.floor)
                }) {
                    bail!("exclusive-top: {:?}#{:?} floor {} has no reader in a different family in {:?}",
                        model.model, row.effort, row.floor, self.id);
                }
            }
        }
        Ok(())
    }
}

fn nonempty(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("empty-field: {field}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn table() -> Value {
        json!({
            "schema": SCHEMA,
            "version": [{
                "id": "v2026-10-07",
                "effective_from": "2026-10-07T00:00:00Z",
                "source": "fixture capability judgment",
                "gap": false,
                "model": [
                    {"model": "reader", "identities": ["reader", "reader-alias"],
                     "family": "lab-a", "superseded": false,
                     "efforts": [{"effort": "high", "rank": 2, "floor": 2}]},
                    {"model": "author", "identities": ["author"],
                     "family": "lab-b", "superseded": true,
                     "efforts": [{"effort": "high", "rank": 2, "floor": 2}]}
                ]
            }]
        })
    }

    fn rejects(value: Value, reason: &str) {
        let error = parse(&value.to_string()).expect_err(reason).to_string();
        assert!(error.contains(reason), "{error}; expected {reason}");
    }

    /// Claim: either encoding carries the same table, including retired rows.
    /// Rejects a producer-specific JSON shape or loss during TOML storage.
    #[test]
    fn json_and_toml_round_trip_the_shared_shape() {
        let json = parse(&table().to_string()).unwrap();
        let toml = toml::to_string_pretty(&json).unwrap();
        assert_eq!(parse(&toml).unwrap(), json);
        assert!(json.version[0].model.as_ref().unwrap()[1].superseded);
        assert_eq!(FILE, ".arc/review-ladder.toml");
    }

    /// Claim: every author floor has a reader in another family.
    /// Rejects an exclusive top even when its own family meets its floor.
    #[test]
    fn a_floor_no_other_family_can_meet_is_refused() {
        let mut value = table();
        value["version"][0]["model"][0]["efforts"][0]["floor"] = json!(3);
        rejects(value, "exclusive-top");
    }

    /// Claim: a gap has no model table, even when the supplied table is empty.
    /// Rejects each ambiguous representation of missing capability history.
    #[test]
    fn a_gap_with_model_rows_is_refused() {
        let mut value = table();
        value["version"][0]["gap"] = json!(true);
        rejects(value, "gap-with-models");
    }

    #[test]
    fn a_gap_with_an_empty_model_table_is_refused() {
        let mut value = table();
        value["version"][0]["gap"] = json!(true);
        value["version"][0]["model"] = json!([]);
        rejects(value, "gap-with-models");
    }

    #[test]
    fn a_gap_without_a_model_table_is_accepted() {
        let mut value = table();
        value["version"][0]["gap"] = json!(true);
        value["version"][0].as_object_mut().unwrap().remove("model");
        assert!(parse(&value.to_string()).is_ok());
    }

    /// Claim: corrections have a named refusal until their authorization exists.
    /// Rejects silently dropping or activating a correction during import.
    #[test]
    fn corrections_are_refused_by_name() {
        let mut value = table();
        value["correction"] = json!([{"id": "correction-one", "replaces": "v2026-10-07"}]);
        rejects(value, "corrections-unsupported");
    }

    /// Claim: each id names one dated version; duplicate ids refuse.
    #[test]
    fn duplicate_version_ids_are_refused() {
        let mut value = table();
        let mut next = value["version"][0].clone();
        next["effective_from"] = json!("2026-10-08T00:00:00Z");
        value["version"].as_array_mut().unwrap().push(next);
        rejects(value, "duplicate-id");
    }

    /// Claim: an effective date is an RFC 3339 instant, not arbitrary text.
    #[test]
    fn invalid_effective_instants_are_refused() {
        let mut value = table();
        value["version"][0]["effective_from"] = json!("not-a-time");
        rejects(value, "effective_from");
    }

    /// Claim: ordinary versions increase by instant; backdating refuses.
    #[test]
    fn backdated_versions_are_refused() {
        let mut value = table();
        let mut next = value["version"][0].clone();
        next["id"] = json!("next");
        next["effective_from"] = json!("2026-10-06T00:00:00Z");
        value["version"].as_array_mut().unwrap().push(next);
        rejects(value, "version-order");
    }

    /// Claim: different RFC 3339 spellings of one instant cannot create two versions.
    #[test]
    fn equal_effective_instants_are_refused() {
        let mut value = table();
        let mut next = value["version"][0].clone();
        next["id"] = json!("next");
        next["effective_from"] = json!("2026-10-06T21:00:00-03:00");
        value["version"].as_array_mut().unwrap().push(next);
        rejects(value, "version-order");
    }

    /// Claim: aliases resolve to one model; shared aliases refuse.
    #[test]
    fn model_identity_collisions_are_refused() {
        let mut value = table();
        value["version"][0]["model"][1]["identities"] = json!(["reader-alias"]);
        rejects(value, "duplicate-identity");
    }

    /// Claim: aliases cannot shadow another row's canonical id.
    #[test]
    fn an_alias_cannot_shadow_an_unlisted_canonical_id() {
        let mut value = table();
        value["version"][0]["model"][0]["identities"] = json!(["reader-alias"]);
        value["version"][0]["model"][1]["identities"] = json!(["reader"]);
        rejects(value, "duplicate-identity");
    }

    /// Claim: a model's effort list contains at least one capability row.
    #[test]
    fn an_empty_effort_list_is_refused() {
        let mut value = table();
        value["version"][0]["model"][1]["efforts"] = json!([]);
        rejects(value, "empty-efforts");
    }

    /// Claim: rank is positive; zero cannot qualify as a strength.
    #[test]
    fn a_zero_rank_is_refused() {
        let mut value = table();
        value["version"][0]["model"][1]["efforts"][0]["rank"] = json!(0);
        rejects(value, "positive-rank-floor");
    }

    /// Claim: floor is positive; zero cannot erase an author's requirement.
    #[test]
    fn a_zero_floor_is_refused() {
        let mut value = table();
        value["version"][0]["model"][1]["efforts"][0]["floor"] = json!(0);
        rejects(value, "positive-rank-floor");
    }

    /// Claim: an effort has one rank and floor; duplicate rows refuse.
    #[test]
    fn duplicate_effort_rows_are_refused() {
        let mut value = table();
        let row = value["version"][0]["model"][1]["efforts"][0].clone();
        value["version"][0]["model"][1]["efforts"]
            .as_array_mut()
            .unwrap()
            .push(row);
        rejects(value, "duplicate-effort");
    }

    /// Claim: a ladder declares the shared reader schema, not an unrecognized version.
    #[test]
    fn an_unknown_schema_is_refused() {
        let mut value = table();
        value["schema"] = json!("arc-review-ladder/2");
        rejects(value, "schema");
    }

    /// Claim: a registry needs at least one historical version.
    #[test]
    fn an_empty_registry_is_refused() {
        let mut value = table();
        value["version"] = json!([]);
        rejects(value, "empty-versions");
    }
}
