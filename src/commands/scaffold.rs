//! Scaffold templates for briefs and journal artifacts. A repo-local
//! `.arc/templates/<name>.md` always wins over the compiled-in defaults, so
//! projects can teach their own conventions without forking the binary.

use super::Ctx;
use crate::gitio;
use anyhow::{bail, Context, Result};

/// Built-in brief scaffolds. They encode the delegation-canon fences and the
/// sandbox facts an arc-driving executor must respect.
const SCAFFOLD_SOL_LOW: &str = include_str!("scaffolds/sol-low.md");
const SCAFFOLD_SOL_HIGH: &str = include_str!("scaffolds/sol-high.md");
const SCAFFOLD_REVIEWER: &str = include_str!("scaffolds/reviewer.md");
/// Built-in journal scaffolds. They seed the discussion conventions (position
/// headings, stance lines, resolution vocabulary) at artifact birth.
const SCAFFOLD_DISCUSSION: &str = include_str!("scaffolds/discussion.md");

/// Resolve a scaffold template: a repo-local `.arc/templates/<name>.md` wins,
/// otherwise a compiled-in default (`sol-low`, `sol-high`, `reviewer`,
/// `discussion`).
pub(crate) fn resolve(ctx: &Ctx, name: &str) -> Result<String> {
    let repo_template = gitio::toplevel(&ctx.cwd).ok().map(|top| {
        top.join(".arc")
            .join("templates")
            .join(format!("{name}.md"))
    });
    if let Some(path) = repo_template {
        if path.is_file() {
            return std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read scaffold {}", path.display()));
        }
    }
    match name {
        "sol-low" => Ok(SCAFFOLD_SOL_LOW.to_string()),
        "sol-high" => Ok(SCAFFOLD_SOL_HIGH.to_string()),
        "reviewer" => Ok(SCAFFOLD_REVIEWER.to_string()),
        "discussion" => Ok(SCAFFOLD_DISCUSSION.to_string()),
        other => bail!(
            "unknown scaffold {other:?}; provide .arc/templates/{other}.md or use \
             a built-in (sol-low, sol-high, reviewer, discussion)"
        ),
    }
}

/// Every compiled-in scaffold, with the one line each exists to serve.
pub(crate) const BUILT_IN: [(&str, &str); 4] = [
    ("sol-low", "brief: a fully specified executor task"),
    (
        "sol-high",
        "brief: a task needing judgment inside stated bounds",
    ),
    (
        "reviewer",
        "brief: an adversarial review with named pressure points",
    ),
    (
        "discussion",
        "journal: position headings, stance lines, resolution",
    ),
];

/// The scaffold a kind prepends when the caller names none.
pub(crate) fn default_for_kind(kind: &str) -> Option<&'static str> {
    // Only a discussion has one. A kind whose artifacts have no conventions
    // to seed is better off with the body the caller wrote and nothing else.
    (kind == "discussion").then_some("discussion")
}

/// Names resolvable here: the built-ins, plus any `.arc/templates/<name>.md`,
/// with a repo template shadowing a built-in of the same name.
pub(crate) fn available(ctx: &Ctx) -> Vec<(String, bool)> {
    let mut names: Vec<(String, bool)> = BUILT_IN
        .iter()
        .map(|(name, _)| ((*name).to_string(), false))
        .collect();
    let dir = gitio::toplevel(&ctx.cwd)
        .ok()
        .map(|top| top.join(".arc").join("templates"));
    let Some(dir) = dir else {
        return names;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return names;
    };
    let mut local: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".md")
                .map(str::to_string)
        })
        .collect();
    local.sort();
    for name in local {
        match names.iter_mut().find(|(known, _)| *known == name) {
            // A repo template of a built-in's name wins, and saying so is the
            // point: the body under that name is not the one documented here.
            Some(entry) => entry.1 = true,
            None => names.push((name, true)),
        }
    }
    names
}

/// Prepend a scaffold template to a body being recorded, mirroring the brief
/// semantics: the template comes first (newline-terminated), a blank line
/// separates it from the body, and a scaffold with no body records the
/// template alone. An empty template yields the body verbatim.
pub(crate) fn prepended(template: &str, body: &str) -> String {
    if template.is_empty() {
        return body.to_string();
    }
    let mut out = template.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
    }
    out
}
