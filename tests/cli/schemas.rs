//! The guide is the register of every versioned surface arc emits, and a
//! version bumped in code without the register being edited leaves it
//! describing a format nothing writes.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A floor on the schema constants found, so a scan that stopped matching
/// fails rather than passing over an empty set.
const MINIMUM_SCHEMAS: usize = 10;

/// The constants are the authority: each one's value must appear in the
/// guide verbatim.
#[test]
fn every_schema_constant_is_registered_in_the_guide() {
    let output = Command::new(env!("CARGO_BIN_EXE_arc")).output().unwrap();
    let guide = String::from_utf8_lossy(&output.stdout);

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
    let output = Command::new(env!("CARGO_BIN_EXE_arc")).output().unwrap();
    let guide = String::from_utf8_lossy(&output.stdout);
    for schema in [
        "arc-state/3",
        "arc-bundle/6",
        "arc-replica-event/3",
        "arc-replica-bundle/3",
        "journal-events/1",
    ] {
        assert!(guide.contains(&format!("`{schema}`")), "{schema}");
    }
    assert!(guide.contains("store format 7"));
}
