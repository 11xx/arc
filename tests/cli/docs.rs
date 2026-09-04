//! The CLI is the whole of what a session needs to act correctly, so a rule
//! that lives in `docs/` and nowhere else is a rule the acting session never
//! reads. This holds the two surfaces together: every sentence in `docs/` that
//! names an exit code, or that states a refusal or a guarantee, must also be
//! reachable from `arc` and `arc <verb> --help`.
//!
//! The comparison is on whole sentences, modulo whitespace, so the CLI carries
//! the fact in the words the long form uses rather than an approximation of
//! them. A fact the CLI is missing is added to the guide or to the command's
//! help — the docs page is the specification here, not the thing to weaken.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Sentence openings that state a refusal or a guarantee about arc itself.
const CLAIM_OPENINGS: [&str; 3] = ["arc refuses", "arc records", "arc never"];

/// Abbreviations whose period ends no sentence.
const ABBREVIATIONS: [&str; 5] = ["e.g.", "i.e.", "etc.", "cf.", "vs."];

/// A floor on how much the assertion actually covers. Extraction that silently
/// stops matching would otherwise leave a passing test that checks nothing.
const MINIMUM_FACTS: usize = 25;

#[test]
fn docs_facts_are_reachable_from_the_cli() {
    let corpus = normalize(&cli_corpus());
    let mut facts = Vec::new();
    for page in pages() {
        let text = std::fs::read_to_string(&page).unwrap();
        for sentence in sentences(&text) {
            if is_fact(&sentence) {
                facts.push((page.clone(), sentence));
            }
        }
    }

    assert!(
        facts.len() >= MINIMUM_FACTS,
        "extracted only {} facts from docs/; extraction is no longer matching",
        facts.len()
    );

    let missing: Vec<String> = facts
        .iter()
        .filter(|(_, fact)| !corpus.contains(&normalize(fact)))
        .map(|(page, fact)| format!("{}: {fact}", page.file_name().unwrap().to_string_lossy()))
        .collect();

    assert!(
        missing.is_empty(),
        "{} of {} documented facts are absent from `arc` and every `--help`; \
         add each to the guide or to that command's help:\n{}",
        missing.len(),
        facts.len(),
        missing.join("\n")
    );
}

/// Every Markdown page of the long form, `QUICKSTART` included.
fn pages() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    let mut pages: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    pages.sort();
    assert!(!pages.is_empty(), "no docs pages found");
    pages
}

/// The guide plus the help of every command and subcommand, walked from
/// `arc --help` so a command added later is covered without being listed here.
fn cli_corpus() -> String {
    let mut corpus = run(&[]);
    let mut seen = BTreeSet::new();
    let mut queue = vec![Vec::<String>::new()];
    while let Some(path) = queue.pop() {
        let refs: Vec<&str> = path.iter().map(String::as_str).collect();
        let help = run(&[refs.as_slice(), &["--help"]].concat());
        corpus.push_str(&help);
        for sub in subcommands(&help) {
            let mut next = path.clone();
            next.push(sub);
            if seen.insert(next.clone()) {
                queue.push(next);
            }
        }
    }
    corpus
}

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_arc"))
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The command names in a help page's `Commands:` block. Clap lists each at a
/// fixed indent and wraps its description deeper, so the two never collide.
fn subcommands(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in help.lines() {
        if line.starts_with("Commands:") {
            inside = true;
            continue;
        }
        if inside {
            if line.trim().is_empty() {
                break;
            }
            let Some(rest) = line.strip_prefix("  ") else {
                continue;
            };
            if rest.starts_with(' ') {
                continue;
            }
            let name = rest.split_whitespace().next().unwrap_or_default();
            // `help` prints the same pages back, and a placeholder is not a
            // command anybody can run.
            if name != "help" && !name.starts_with('<') {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Prose sentences, with fenced code, headings, and table rows left out: a
/// documented fact is a claim in prose, and a code block is a transcript.
fn sentences(page: &str) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    let mut fenced = false;
    let mut current = String::new();
    for line in page.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        let trimmed = line.trim();
        let breaks = fenced
            || trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with('|')
            || trimmed.starts_with("- ")
            || trimmed.starts_with("> ");
        if breaks {
            units.push(std::mem::take(&mut current));
            if fenced || trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('|')
            {
                continue;
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(trimmed.trim_start_matches(['-', '>']).trim());
    }
    units.push(current);
    units.iter().flat_map(|unit| split(unit)).collect()
}

/// Split one prose unit into sentences at a period that ends one: followed by
/// whitespace or the end, and not the period of an abbreviation.
fn split(unit: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = unit.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'.' {
            continue;
        }
        let ends = bytes
            .get(index + 1)
            .is_none_or(|next| next.is_ascii_whitespace());
        if !ends {
            continue;
        }
        let sentence = unit[start..=index].trim();
        if ABBREVIATIONS
            .iter()
            .any(|abbreviation| sentence.ends_with(abbreviation))
        {
            continue;
        }
        if !sentence.is_empty() {
            out.push(sentence.to_string());
        }
        start = index + 1;
    }
    let tail = unit[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// Whether a sentence states a fact the CLI must carry: a numbered exit code,
/// or a refusal or guarantee about arc itself.
fn is_fact(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    if CLAIM_OPENINGS
        .iter()
        .any(|opening| lower.starts_with(opening))
    {
        return true;
    }
    names_exit_code(&lower)
}

/// `exit <n>` or `exits <n>`, the two spellings the long form uses.
fn names_exit_code(lower: &str) -> bool {
    let mut haystack = lower;
    while let Some(at) = haystack.find("exit") {
        let rest = &haystack[at + "exit".len()..];
        let rest = rest.strip_prefix('s').unwrap_or(rest);
        if rest
            .strip_prefix(' ')
            .and_then(|rest| rest.chars().next())
            .is_some_and(|first| first.is_ascii_digit())
        {
            return true;
        }
        haystack = &haystack[at + "exit".len()..];
    }
    false
}

/// Whitespace is a rendering decision on both sides: the long form wraps at a
/// column and help output wraps at another, so comparison ignores it.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A floor on the schema constants found, so a scan that stopped matching
/// fails rather than passing over an empty set.
const MINIMUM_SCHEMAS: usize = 10;

/// `docs/schemas.md` is the register of every versioned surface arc emits, and
/// a version bumped in code without the row being edited leaves the register
/// describing a format nothing writes. The constants are the authority: each
/// one's value must appear in the page verbatim.
#[test]
fn every_schema_constant_is_registered_in_the_docs() {
    let page = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("schemas.md"),
    )
    .unwrap();

    let schemas = schema_constants();
    assert!(
        schemas.len() >= MINIMUM_SCHEMAS,
        "found only {} schema constants in src/; the scan is no longer matching",
        schemas.len()
    );

    let missing: Vec<&(String, String)> = schemas
        .iter()
        .filter(|(_, value)| !page.contains(&format!("`{value}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "{} schema constant(s) name a version docs/schemas.md does not list; \
         update the row for each:\n{}",
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
