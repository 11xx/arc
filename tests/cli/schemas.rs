//! The guide is the register of every versioned surface arc emits, and a
//! version bumped in code without the register being edited leaves it
//! describing a format nothing writes.

use super::common::{stdout, Repo};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn staleness_inbox_help_matches_schema_constant() {
    let repo = Repo::new();
    let help = stdout(repo.arc(&repo.root).args(["inbox", "--help"]));
    let schemas = schema_constants();
    let (_, schema) = schemas
        .iter()
        .find(|(name, _)| name == "INBOX_SCHEMA")
        .unwrap();
    assert!(
        help.contains(schema),
        "inbox help must name {schema}: {help}"
    );
}

#[test]
fn staleness_guide_names_rebase_snapshot_journal_defaults() {
    let repo = Repo::new();
    let guide = crate::common::instruction_topics(&repo, &["rules-evidence"]);
    let flat = guide.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("snapshot (`snapshot`, `done`, `rebase`,"),
        "{flat}"
    );
}

/// A floor on the schema constants found, so a scan that stopped matching
/// fails rather than passing over an empty set.
const MINIMUM_SCHEMAS: usize = 10;

/// The constants are the authority: each one's value must appear in the
/// guide verbatim.
#[test]
fn every_schema_constant_is_registered_in_the_guide() {
    let repo = Repo::new();
    let guide =
        crate::common::instruction_topics(&repo, &["schemas", "assurance", "debt-obligations"]);

    let schemas = schema_constants();
    assert!(
        schemas.len() >= MINIMUM_SCHEMAS,
        "found only {} schema constants in src/; the scan is no longer matching",
        schemas.len()
    );

    let missing: Vec<&(String, String)> = schemas
        .iter()
        .filter(|(_, value)| !guide.contains(&format!("`{value}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "{} schema constant(s) name a version the guide's SCHEMAS section does \
         not list; update the line for each:\n{}",
        missing.len(),
        missing
            .iter()
            .map(|(name, value)| format!("{name} = {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every `…SCHEMA: &str = "…"` in the crate, as constant name and value. The
/// source is scanned rather than a list kept here, so a constant added or
/// bumped is covered without anyone remembering to say so twice.
fn schema_constants() -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut sources = Vec::new();
    collect_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );
    sources.sort();
    for source in &sources {
        let text = std::fs::read_to_string(source).unwrap();
        for line in text.lines() {
            let line = line.trim();
            let Some((declaration, rest)) = line.split_once(": &str = \"") else {
                continue;
            };
            let declaration = declaration.strip_prefix("pub ").unwrap_or(declaration);
            let Some(name) = declaration.strip_prefix("const ") else {
                continue;
            };
            let name = name.trim();
            if !name.contains("SCHEMA") {
                continue;
            }
            let Some((value, _)) = rest.split_once('"') else {
                continue;
            };
            found.push((name.to_string(), value.to_string()));
        }
    }
    found
}

fn collect_sources(at: &Path, sources: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(at).unwrap().filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, sources);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            sources.push(path);
        }
    }
}

#[test]
fn model_provenance_versions_are_registered() {
    let output = Command::new(env!("CARGO_BIN_EXE_arc"))
        .args(["guide", "schemas"])
        .output()
        .unwrap();
    let guide = String::from_utf8_lossy(&output.stdout);
    let schemas = schema_constants();
    for name in [
        "CHANGE_STATE_SCHEMA",
        "BUNDLE_SCHEMA",
        "EVENT_SCHEMA",
        "JOURNAL_SCHEMA",
    ] {
        let found: Vec<&(String, String)> = schemas
            .iter()
            .filter(|(constant, _)| constant == name)
            .collect();
        assert!(!found.is_empty(), "{name} is not a schema constant");
        for (_, schema) in found {
            assert!(guide.contains(&format!("`{schema}`")), "{name}: {schema}");
        }
    }
    assert!(guide.contains("store format 8"));
}

/// The guide's rule is one version per release, counted from the last one:
/// a surface already bumped for the unreleased version keeps that number, so
/// no schema in the tree may be more than one past the same schema in the
/// latest release. Current values are read from the constants and inline
/// records, not restated here.
#[test]
fn no_schema_advances_more_than_once_past_the_last_release() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tags = Command::new("git")
        .current_dir(root)
        .args(["tag", "--list", "v20*", "--sort=-v:refname"])
        .output();
    let Ok(tags) = tags else {
        return;
    };
    if !tags.status.success() {
        return;
    }
    let tags = String::from_utf8_lossy(&tags.stdout);
    let Some(tag) = tags.lines().next() else {
        return;
    };
    let head = grep_schemas(root, None);
    let released = grep_schemas(root, Some(tag));
    let mut released_max: BTreeMap<&str, u32> = BTreeMap::new();
    for (name, version) in &released {
        let slot = released_max.entry(name.as_str()).or_default();
        *slot = (*slot).max(*version);
    }
    for (name, version) in head {
        let Some(released) = released_max.get(name.as_str()) else {
            // Introduced after the release: there is no released baseline for
            // it to advance past.
            continue;
        };
        assert!(
            version <= released + 1,
            "{name}/{version} advances more than one version past {tag}'s {name}/{released};              a surface already bumped for the unreleased version keeps its number"
        );
    }
}

/// Every `arc-<name>/<n>` recorded in the tree, or in the tagged release.
fn grep_schemas(root: &Path, tag: Option<&str>) -> Vec<(String, u32)> {
    let mut args = vec!["grep", "-h", "-o", "-E", "arc-[a-z0-9-]+/[0-9]+"];
    if let Some(tag) = tag {
        args.push(tag);
    }
    args.extend(["--", "src"]);
    let output = Command::new("git")
        .current_dir(root)
        .args(&args)
        .output()
        .unwrap();
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (name, version) = line.rsplit_once('/')?;
            Some((name.to_string(), version.parse().ok()?))
        })
        .collect()
}
