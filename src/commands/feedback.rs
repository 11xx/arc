//! File, deduplicate, and list tool and workflow feedback reports across
//! registered projects from any directory.

use crate::commands::Ctx;
use crate::config;
use crate::journal::{
    feedback_file, feedback_list, FeedbackListRow, FeedbackOutcome, FeedbackRoute, FeedbackWrite,
    SourceArgs,
};
use crate::registry::{self, Project};
use anyhow::{bail, Result};
use clap::Args;
use serde::Serialize;
use std::collections::HashMap;

pub const FEEDBACK_SCHEMA: &str = "arc-feedback/1";
pub const FEEDBACK_LIST_SCHEMA: &str = "arc-feedback-list/1";

#[derive(Args)]
pub struct FeedbackArgs {
    /// Registered project name (label or slug) from the workspace registry
    pub project: String,
    /// Body source: a file path, or '-' for stdin
    #[arg(long)]
    pub body_file: Option<String>,
    /// Cross-session dedup key (kebab-case slug). If an open feedback report
    /// with this key exists in the target project's journal, append an
    /// evidence block to that report instead of creating a duplicate
    #[arg(long, value_name = "SLUG")]
    pub key: Option<String>,
    /// Heading for a new feedback report, or title for the appended evidence
    /// block when `--key` matches an open report
    #[arg(long)]
    pub title: Option<String>,
    /// Routing classification based on cause: local (Arc-managed tool journal,
    /// default on creation), upstream (external issue or PR draft), operator
    /// (operator attention or policy decision), or delegate (delegatable
    /// follow-up). Omitted when appending via `--key`, preserves the existing
    /// report's route
    #[arg(long, value_enum)]
    pub route: Option<FeedbackRoute>,
    /// Scaffold template around the body (.arc/templates/<name>.md or a
    /// built-in; defaults to `feedback` on creation)
    #[arg(long, conflicts_with = "no_scaffold")]
    pub scaffold: Option<String>,
    /// Record the body alone, without the `feedback` default scaffold
    #[arg(long)]
    pub no_scaffold: bool,
    #[command(flatten)]
    pub source: SourceArgs,
    /// List open feedback reports (key, file, route, evidence count, title) in
    /// the target project's journal instead of filing one
    #[arg(
        long,
        conflicts_with_all = [
            "body_file",
            "key",
            "title",
            "route",
            "scaffold",
            "no_scaffold",
            "source",
            "item_key"
        ]
    )]
    pub list: bool,
    /// Emit structured JSON (`arc-feedback-list/1` with `--list`,
    /// `arc-feedback/1` when filing)
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct FeedbackListOutput {
    schema: &'static str,
    project: String,
    slug: String,
    journal_dir: String,
    items: Vec<FeedbackListRow>,
}

#[derive(Serialize)]
struct FeedbackWriteOutput {
    schema: &'static str,
    action: &'static str,
    project: String,
    slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    existing: Option<String>,
}

fn resolve_project(name: &str) -> Result<Project> {
    let cfg = config::load()?;
    let active: Vec<Project> = registry::journal_projects(&cfg)?
        .into_iter()
        .filter(|project| project.retired.is_none())
        .collect();
    let matches: Vec<&Project> = active
        .iter()
        .filter(|project| project.slug == name || project.label() == name)
        .collect();
    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => {
            if active.is_empty() {
                bail!(
                    "unknown project {name:?}; no projects are registered in the workspace registry"
                );
            }
            let mut label_counts: HashMap<String, usize> = HashMap::new();
            for project in &active {
                *label_counts.entry(project.label()).or_default() += 1;
            }
            let mut registered: Vec<String> = active
                .iter()
                .map(|project| {
                    let label = project.label();
                    if label_counts.get(&label).copied().unwrap_or(0) > 1 {
                        format!("{label} ({})", project.slug)
                    } else {
                        label
                    }
                })
                .collect();
            registered.sort();
            registered.dedup();
            bail!(
                "unknown project {name:?}; registered projects: {}",
                registered.join(", ")
            );
        }
        _ => bail!(
            "project name {name:?} matched multiple registered projects: {}",
            matches
                .iter()
                .map(|project| format!("{} ({})", project.label(), project.slug))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn run(ctx: &Ctx, args: FeedbackArgs) -> Result<i32> {
    let project = resolve_project(&args.project)?;
    let label = project.label();
    if !project.journal_dir.is_absolute() {
        bail!(
            "project {label:?} resolves to relative journal path {} which cannot be anchored without an existing absolute project root",
            project.journal_dir.display()
        );
    }
    if args.list {
        let items = feedback_list(&project.journal_dir)?;
        if args.json {
            let output = FeedbackListOutput {
                schema: FEEDBACK_LIST_SCHEMA,
                project: label,
                slug: project.slug,
                journal_dir: project.journal_dir.display().to_string(),
                items,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("project: {label} ({})", project.journal_dir.display());
            println!("open feedback (newest first):");
            if items.is_empty() {
                println!("  (none)");
            } else {
                for item in &items {
                    let route = item
                        .route
                        .as_deref()
                        .map(|route| format!("  [route: {route}]"))
                        .unwrap_or_default();
                    println!(
                        "  {}  {}{route}  [evidence: {}]  {}",
                        item.key, item.file, item.evidence_count, item.title
                    );
                }
            }
        }
        return Ok(0);
    }

    let write = FeedbackWrite {
        dir: &project.journal_dir,
        anchor: project.anchor.as_deref(),
        body_file: args.body_file.as_deref(),
        key: args.key.as_deref(),
        title: args.title.as_deref(),
        route: args.route,
        scaffold: args.scaffold.as_deref(),
        no_scaffold: args.no_scaffold,
        source: &args.source,
    };
    let outcome = feedback_file(ctx, &write)?;
    match outcome {
        FeedbackOutcome::Created {
            file,
            path,
            key,
            route,
        } => {
            if args.json {
                let output = FeedbackWriteOutput {
                    schema: FEEDBACK_SCHEMA,
                    action: "created",
                    project: label,
                    slug: project.slug,
                    file: Some(file),
                    path: Some(path.display().to_string()),
                    key: Some(key),
                    route: Some(route),
                    evidence_id: None,
                    existing: None,
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("{}", path.display());
            }
        }
        FeedbackOutcome::Appended {
            file,
            path,
            key,
            route,
            evidence_id,
        } => {
            if args.json {
                let output = FeedbackWriteOutput {
                    schema: FEEDBACK_SCHEMA,
                    action: "appended",
                    project: label,
                    slug: project.slug,
                    file: Some(file),
                    path: Some(path.display().to_string()),
                    key: Some(key),
                    route,
                    evidence_id: Some(evidence_id),
                    existing: None,
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!(
                    "appended evidence {evidence_id} to existing feedback {file} (key: {key})\n{}",
                    path.display()
                );
            }
        }
        FeedbackOutcome::Existing { summary } => {
            if args.json {
                let output = FeedbackWriteOutput {
                    schema: FEEDBACK_SCHEMA,
                    action: "existing",
                    project: label,
                    slug: project.slug,
                    file: None,
                    path: None,
                    key: None,
                    route: None,
                    evidence_id: None,
                    existing: Some(summary),
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("{summary}");
            }
        }
    }
    Ok(0)
}
