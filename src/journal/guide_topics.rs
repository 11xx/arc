//! Read-only projection of journal pins into scoped guide topics.

use super::{pins, read_events, resolve_dir, valid_topic, Amendments, ArtifactLocation};
use crate::config;
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use std::path::Path;

pub(super) fn validate_name(name: &str) -> Result<()> {
    ensure!(
        valid_topic(name),
        "guide topic must be kebab-case: {name:?}"
    );
    ensure!(
        !crate::guide::reserved_topic(name),
        "guide topic {name:?} shadows a built-in topic or command route"
    );
    Ok(())
}

pub(crate) struct GuideTopic {
    pub(crate) name: String,
    pub(crate) file: String,
    pub(crate) title: String,
    pub(crate) scope: &'static str,
    pub(crate) body: String,
    edited_at: Option<DateTime<Utc>>,
}

impl GuideTopic {
    pub(crate) fn age(&self, now: DateTime<Utc>) -> String {
        self.edited_at
            .map(|at| {
                let seconds = now.signed_duration_since(at).num_seconds().max(0) as u64;
                format!("{} ago", super::format_age(seconds))
            })
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// Project topics precede operator topics for lookup. Each journal's events
/// and each topic's body are read once for the entire guide request.
pub(crate) fn load(cwd: &Path) -> Vec<GuideTopic> {
    let mut topics = Vec::new();
    let global = config::global_journal_dir().ok();
    if let Ok(project) = resolve_dir(cwd) {
        if global
            .as_ref()
            .is_none_or(|global| config::resolved(global) != config::resolved(&project))
        {
            load_scope(&project, "project", &mut topics);
        }
    }
    if let Some(global) = global {
        load_scope(&global, "operator", &mut topics);
    }
    topics
}

fn load_scope(dir: &Path, scope: &'static str, topics: &mut Vec<GuideTopic>) {
    let result = (|| -> Result<()> {
        let events = read_events(dir)?;
        let mut scoped = Vec::new();
        for (file, pin) in pins::pins(&events) {
            let Some(name) = pin.guide.as_deref() else {
                continue;
            };
            if let Err(error) = validate_name(name) {
                eprintln!("warning: {scope} guide {file}: {error}");
                continue;
            }
            let location = ArtifactLocation {
                hot: dir.to_path_buf(),
                file: file.to_string(),
                foreign: false,
            };
            let body = match super::read_located_body_in(&location, &events) {
                Ok(body) => body,
                Err(error) => {
                    eprintln!("warning: {scope} guide {file}: {error}");
                    continue;
                }
            };
            let title = Amendments::collect(&events, file)
                .title()
                .map(str::to_string)
                .or_else(|| {
                    body.lines()
                        .find(|line| line.starts_with('#'))
                        .map(|line| line.trim_start_matches('#').trim().to_string())
                })
                .unwrap_or_else(|| file.to_string());
            let edited_at = super::artifact_body_path(dir, file)
                .and_then(|path| std::fs::metadata(path).ok())
                .and_then(|metadata| metadata.modified().ok())
                .map(DateTime::<Utc>::from);
            scoped.push(GuideTopic {
                name: name.to_string(),
                file: file.to_string(),
                title,
                scope,
                body,
                edited_at,
            });
        }
        scoped.sort_by(|left, right| left.name.cmp(&right.name).then(left.file.cmp(&right.file)));
        topics.extend(scoped);
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("warning: cannot read {scope} guide topics: {error}");
    }
}
