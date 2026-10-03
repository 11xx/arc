//! Style resolution is shared by configuration inspection and CLI consumers.
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const CONFIG_SCHEMA: &str = "arc-config/1";

const KEYS: [(&str, &str); 4] = [
    ("change.checkout", "in-place"),
    ("change.branch_prefix", "arc/"),
    ("integration.shape", "ff-only"),
    ("integration.cleanup", "false"),
];

#[derive(Debug, Serialize)]
pub struct Candidate {
    value: Value,
    source: String,
}

#[derive(Debug, Serialize)]
pub struct Resolved {
    pub value: Value,
    source: String,
    overridden: Vec<Candidate>,
    allowed: Option<Allowed>,
}

#[derive(Debug, Serialize)]
struct Allowed {
    values: Vec<String>,
    sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkout {
    InPlace,
    Worktree,
}

impl Checkout {
    fn as_str(self) -> &'static str {
        match self {
            Self::InPlace => "in-place",
            Self::Worktree => "worktree",
        }
    }
}

#[derive(Debug, Default)]
pub struct StyleFlags {
    pub shape: Option<crate::integration::Shape>,
    pub checkout: Option<Checkout>,
    pub branch_prefix: Option<String>,
    pub cleanup: Option<bool>,
}

#[derive(Debug)]
pub struct Style {
    pub keys: BTreeMap<String, Resolved>,
}

impl Style {
    pub fn checkout(&self) -> Checkout {
        match self.keys["change.checkout"].value.as_str() {
            Some("in-place") => Checkout::InPlace,
            Some("worktree") => Checkout::Worktree,
            _ => unreachable!("resolved change.checkout is validated"),
        }
    }

    pub fn branch_prefix(&self) -> &str {
        self.keys["change.branch_prefix"]
            .value
            .as_str()
            .expect("resolved prefix is a string")
    }

    pub fn shape(&self) -> crate::integration::Shape {
        match self.keys["integration.shape"].value.as_str() {
            Some("no-ff") => crate::integration::Shape::NoFf,
            Some("ff-only") => crate::integration::Shape::FfOnly,
            _ => unreachable!("resolved integration.shape is validated"),
        }
    }
}

fn optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn field<'a>(value: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.').try_fold(value, |v, part| v.get(part))
}

fn validate_value(key: &str, value: &toml::Value, origin: &str) -> Result<Value> {
    let valid = match key {
        "change.checkout" => matches!(value.as_str(), Some("in-place" | "worktree")),
        "change.branch_prefix" => value.as_str().is_some(),
        "integration.shape" => matches!(value.as_str(), Some("ff-only" | "no-ff")),
        "integration.cleanup" => value.as_bool().is_some(),
        _ => false,
    };
    if !valid {
        bail!("invalid {origin} key {key}: {value}");
    }
    Ok(serde_json::to_value(value)?)
}

fn parse(text: &str, origin: &str, user: bool) -> Result<toml::Value> {
    let value: toml::Value = toml::from_str(text).with_context(|| format!("malformed {origin}"))?;
    let defaults = if user {
        Some(&value)
    } else {
        value.get("defaults")
    };
    if let Some(defaults) = defaults {
        if !defaults.is_table() {
            bail!("invalid {origin} key defaults: expected a table");
        }
        for table in ["change", "integration"] {
            if defaults.get(table).is_some_and(|v| !v.is_table()) {
                bail!("invalid {origin} key {table}: expected a table");
            }
        }
        for (key, _) in KEYS {
            if let Some(raw) = field(defaults, key) {
                validate_value(key, raw, origin)?;
            }
        }
    }
    for key in [
        "integration.shapes",
        "integration.history",
        "contribution.history",
    ] {
        if let Some(raw) = field(&value, key) {
            if user {
                bail!("{origin} key {key} is a constraint; user configuration holds style only");
            }
            if key == "integration.shapes" {
                if !raw.as_array().is_some_and(|a| {
                    a.iter()
                        .all(|v| matches!(v.as_str(), Some("ff-only" | "no-ff")))
                }) {
                    bail!("invalid {origin} key {key}: {raw}");
                }
            } else if !matches!(raw.as_str(), Some("squash" | "preserve")) {
                bail!("invalid {origin} key {key}: {raw}");
            }
        }
    }
    Ok(value)
}

pub fn validate_user(text: &str, origin: &str) -> Result<()> {
    parse(text, origin, true)?;
    Ok(())
}

pub fn validate_policy(text: &str, origin: &str) -> Result<()> {
    parse(text, origin, false)?;
    Ok(())
}

/// Resolve a target's committed defaults beneath operator policy and flags.
/// Outside Git, only the user file and built-in values participate.
pub fn resolved_style(cwd: &Path, revision: &str, flags: StyleFlags) -> Result<Style> {
    let path = crate::config::ai_home()?.join("arc/config.toml");
    let mut layers = Vec::new();
    if let Some(text) = optional(&path)? {
        let source = format!("user:{}", path.display());
        layers.push((source.clone(), parse(&text, &source, true)?, true));
    }
    if crate::gitio::common_dir(cwd).is_ok() {
        if let Some(text) = crate::gitio::file_at(cwd, revision, ".arc/policy.toml")? {
            let source = format!("project:.arc/policy.toml at {revision}");
            layers.push((source.clone(), parse(&text, &source, false)?, false));
        }
        let path = crate::policy::operator_path(cwd)?;
        if let Some(text) = optional(&path)? {
            let source = format!("operator:{}", path.display());
            layers.push((source.clone(), parse(&text, &source, false)?, false));
        }
    }
    resolve(layers, flags)
}

fn resolve(layers: Vec<(String, toml::Value, bool)>, flags: StyleFlags) -> Result<Style> {
    let mut keys = BTreeMap::new();
    let mut constraints = Vec::new();
    for (source, value, user) in &layers {
        if !user {
            if let Some(raw) = field(value, "integration.shapes") {
                let allowed = raw
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect::<BTreeSet<_>>();
                constraints.push((source.clone(), allowed));
            }
        }
    }
    let allowed = constraints
        .iter()
        .map(|(_, values)| values.clone())
        .reduce(|a, b| a.intersection(&b).cloned().collect());
    let constraint_sources = constraints
        .iter()
        .map(|(source, values)| format!("{source}={values:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    if allowed.as_ref().is_some_and(BTreeSet::is_empty) {
        bail!("integration.shapes has an empty intersection: {constraint_sources}");
    }
    for (key, default) in KEYS {
        let mut candidates = vec![Candidate {
            value: if key == "integration.cleanup" {
                json!(false)
            } else {
                json!(default)
            },
            source: "builtin".into(),
        }];
        for (source, layer, user) in &layers {
            let defaults = if *user {
                Some(layer)
            } else {
                layer.get("defaults")
            };
            if let Some(raw) = defaults.and_then(|v| field(v, key)) {
                candidates.push(Candidate {
                    value: validate_value(key, raw, source)?,
                    source: source.clone(),
                });
            }
        }
        let flag = match key {
            "change.checkout" => flags.checkout.map(|v| json!(v.as_str())),
            "change.branch_prefix" => flags.branch_prefix.as_ref().map(|v| json!(v)),
            "integration.cleanup" => flags.cleanup.map(|v| json!(v)),
            "integration.shape" => flags.shape.map(|shape| {
                json!(match shape {
                    crate::integration::Shape::FfOnly => "ff-only",
                    crate::integration::Shape::NoFf => "no-ff",
                })
            }),
            _ => unreachable!("known style key"),
        };
        if let Some(value) = flag {
            candidates.push(Candidate {
                value,
                source: "flag".into(),
            });
        }
        let winner = candidates.pop().unwrap();
        let allowed = if key == "integration.shape" {
            allowed
                .as_ref()
                .map(|a| a.iter().cloned().collect::<Vec<_>>())
        } else {
            None
        };
        if allowed
            .as_ref()
            .is_some_and(|a| !a.iter().any(|v| Some(v.as_str()) == winner.value.as_str()))
        {
            bail!(
                "{key}={} from {} is outside allowed values: {constraint_sources}",
                winner.value,
                winner.source
            );
        }
        candidates.reverse();
        keys.insert(
            key.into(),
            Resolved {
                value: winner.value,
                source: winner.source,
                overridden: candidates,
                allowed: allowed.map(|values| Allowed {
                    values,
                    sources: constraints
                        .iter()
                        .map(|(source, _)| source.clone())
                        .collect(),
                }),
            },
        );
    }
    Ok(Style { keys })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_style_key_resolves_each_layer_and_preserves_sources() {
        let texts = [
            ("user:config.toml", "[change]\ncheckout='in-place'\nbranch_prefix='user/'\n[integration]\nshape='no-ff'\ncleanup=true", true),
            ("project:target", "[defaults.change]\ncheckout='worktree'\nbranch_prefix='project/'\n[defaults.integration]\nshape='ff-only'\ncleanup=false", false),
            ("operator:policy.toml", "[defaults.change]\ncheckout='in-place'\nbranch_prefix='operator/'\n[defaults.integration]\nshape='no-ff'\ncleanup=true", false),
        ];
        let builtin = resolve(Vec::new(), StyleFlags::default()).unwrap();
        assert_eq!(builtin.keys["change.checkout"].value, "in-place");
        assert_eq!(builtin.keys["change.branch_prefix"].value, "arc/");
        assert_eq!(builtin.keys["integration.cleanup"].value, false);
        for n in 1..=3 {
            let layers = texts[..n]
                .iter()
                .map(|(source, text, user)| {
                    (
                        source.to_string(),
                        parse(text, source, *user).unwrap(),
                        *user,
                    )
                })
                .collect();
            let resolved = resolve(layers, StyleFlags::default()).unwrap();
            for (key, _) in KEYS {
                assert_eq!(resolved.keys[key].source, texts[n - 1].0);
                assert_eq!(resolved.keys[key].overridden.len(), n);
                assert_eq!(
                    resolved.keys[key].overridden.last().unwrap().source,
                    "builtin"
                );
            }
            assert_eq!(
                resolved.keys["change.branch_prefix"].value,
                ["user/", "project/", "operator/"][n - 1]
            );
            assert_eq!(resolved.keys["integration.cleanup"].value, n != 2);
            assert_eq!(
                resolved.keys["change.checkout"].value,
                if n == 2 { "worktree" } else { "in-place" }
            );
            assert_eq!(
                resolved.keys["integration.shape"].value,
                if n == 2 { "ff-only" } else { "no-ff" }
            );
        }
    }

    #[test]
    fn shape_constraints_intersect_and_bind_every_winning_source() {
        let make = |source: &str, text: &str| {
            (
                source.to_string(),
                parse(text, source, false).unwrap(),
                false,
            )
        };
        let project = "[integration]\nshapes=['ff-only','no-ff']";
        let operator = "[integration]\nshapes=['no-ff']";
        let resolved = resolve(
            vec![make("project:p", project), make("operator:o", operator)],
            StyleFlags {
                shape: Some(crate::integration::Shape::NoFf),
                ..Default::default()
            },
        )
        .unwrap();
        let key = &resolved.keys["integration.shape"];
        assert_eq!(key.source, "flag");
        assert_eq!(key.allowed.as_ref().unwrap().values, ["no-ff"]);
        assert_eq!(
            key.allowed.as_ref().unwrap().sources,
            ["project:p", "operator:o"]
        );
        for flag in [None, Some(crate::integration::Shape::FfOnly)] {
            let error = resolve(
                vec![make("project:p", project), make("operator:o", operator)],
                StyleFlags {
                    shape: flag,
                    ..Default::default()
                },
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("project:p") && error.contains("operator:o"),
                "{error}"
            );
            assert!(
                error.contains(if flag.is_some() { "flag" } else { "builtin" }),
                "{error}"
            );
        }
        let error = resolve(
            vec![
                make("project:p", "[integration]\nshapes=['ff-only']"),
                make("operator:o", operator),
            ],
            StyleFlags::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("empty intersection")
                && error.contains("project:p")
                && error.contains("operator:o")
        );
    }

    #[test]
    fn every_style_flag_overrides_layers() {
        let style = resolve(
            vec![("user:config.toml".into(), parse("[change]\ncheckout='worktree'\nbranch_prefix='user/'\n[integration]\ncleanup=false\nshape='ff-only'", "user", true).unwrap(), true)],
            StyleFlags {
                checkout: Some(Checkout::InPlace),
                branch_prefix: Some("flag-".into()),
                cleanup: Some(true),
                shape: Some(crate::integration::Shape::NoFf),
            },
        ).unwrap();
        assert_eq!(style.checkout(), Checkout::InPlace);
        assert_eq!(style.branch_prefix(), "flag-");
        assert_eq!(style.keys["integration.cleanup"].value, true);
        assert!(matches!(style.shape(), crate::integration::Shape::NoFf));
        for (key, _) in KEYS {
            assert_eq!(style.keys[key].source, "flag");
            assert_eq!(style.keys[key].overridden.len(), 2);
        }
    }

    #[test]
    fn invalid_keys_name_the_file_and_key_in_every_layer() {
        for (key, value) in [
            ("change.checkout", "'bad'"),
            ("change.branch_prefix", "false"),
            ("integration.shape", "'bad'"),
            ("integration.cleanup", "'yes'"),
        ] {
            let (table, name) = key.split_once('.').unwrap();
            for user in [true, false] {
                let prefix = if user { "" } else { "defaults." };
                let text = format!("[{prefix}{table}]\n{name}={value}");
                let error = parse(&text, "file.toml", user).unwrap_err().to_string();
                assert!(
                    error.contains("file.toml") && error.contains(key),
                    "{error}"
                );
            }
        }
        for (key, value) in [
            ("shapes", "['bad']"),
            ("shapes", "false"),
            ("history", "'bad'"),
        ] {
            let error = parse(
                &format!("[integration]\n{key}={value}"),
                "policy.toml",
                false,
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("policy.toml") && error.contains(&format!("integration.{key}")),
                "{error}"
            );
        }
    }
}
