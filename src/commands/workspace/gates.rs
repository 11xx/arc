//! Registry-scoped gate costs without journal or change-state projection.

use super::*;
use crate::commands::gate_reports;

const WORKSPACE_GATES_SCHEMA: &str = "arc-workspace-gates/1";

#[derive(Serialize)]
struct Output {
    schema: &'static str,
    scope: BacklogScope,
    collection: CollectionManifest,
    projects: Vec<ProjectGates>,
    unreachable: Vec<UnreachableProject>,
    retired: Vec<RetiredProject>,
}

#[derive(Serialize)]
struct ProjectGates {
    project: String,
    anchor: String,
    declares_gates: Option<bool>,
    report: Option<gate_reports::GateReport>,
}

pub(super) fn report(scope: WorkspaceScope, show_retired: bool, json: bool) -> Result<i32> {
    let cfg = crate::config::load()?;
    let scope = ResolvedWorkspaceScope::resolve(scope)?;
    let mut output = Output {
        schema: WORKSPACE_GATES_SCHEMA,
        scope: scope.view(),
        collection: CollectionManifest {
            discovered: 0,
            selected: 0,
            skipped: 0,
            empty: 0,
            non_empty: 0,
            failed: 0,
            retired: 0,
            failures: Vec::new(),
        },
        projects: Vec::new(),
        unreachable: Vec::new(),
        retired: Vec::new(),
    };
    let registered = crate::registry::projects(&cfg)?;
    let mut reports = observe_projects(
        &registered,
        |project| observed_in(&scope, project),
        |project, anchor| project_report(&cfg, project, anchor),
    );
    for (index, project) in registered.into_iter().enumerate() {
        let c = &mut output.collection;
        if let Some(retirement) = &project.retired {
            if scope.includes(project.anchor.as_deref()) {
                c.retired += 1;
                if show_retired {
                    output
                        .retired
                        .push(RetiredProject::of(&project, retirement));
                }
            }
            continue;
        }
        c.discovered += 1;
        if !scope.includes(project.anchor.as_deref()) {
            c.skipped += 1;
            continue;
        }
        c.selected += 1;
        if !project.reachable {
            if !project.is_orphan() {
                c.empty += 1;
                continue;
            }
            let reason = if project.anchor.is_some() {
                "anchor does not exist"
            } else {
                "journal name resolves to no single path"
            };
            output.unreachable.push(UnreachableProject {
                slug: project.slug.clone(),
                journal_dir: project.journal_dir.display().to_string(),
                anchor: project.anchor.as_ref().map(|p| p.display().to_string()),
                reason,
            });
            c.failed += 1;
            c.failures.push(CollectionFailure {
                project: project.label(),
                anchor: project.anchor.as_ref().map(|p| p.display().to_string()),
                component: "anchor",
                reason: reason.into(),
            });
            continue;
        }
        let anchor = project
            .anchor
            .as_deref()
            .expect("reachable project has an anchor");
        let result = reports
            .remove(&index)
            .expect("every selected project is read");
        match result {
            Ok(report) => {
                let declares_gates = !report.gates.is_empty();
                if report.failures.is_empty() {
                    if declares_gates {
                        c.non_empty += 1;
                    } else {
                        c.empty += 1;
                    }
                } else {
                    c.failed += 1;
                    for failure in &report.failures {
                        c.failures.push(CollectionFailure {
                            project: project.label(),
                            anchor: Some(anchor.display().to_string()),
                            component: "ledger",
                            reason: serde_json::to_string(failure)?,
                        });
                    }
                }
                output.projects.push(ProjectGates {
                    project: project.label(),
                    anchor: anchor.display().to_string(),
                    declares_gates: Some(declares_gates),
                    report: Some(report),
                });
            }
            Err(error) => {
                c.failed += 1;
                c.failures.push(CollectionFailure {
                    project: project.label(),
                    anchor: Some(anchor.display().to_string()),
                    component: "gates",
                    reason: format!("{error:#}"),
                });
                output.projects.push(ProjectGates {
                    project: project.label(),
                    anchor: anchor.display().to_string(),
                    declares_gates: None,
                    report: None,
                });
            }
        }
    }
    output.projects.sort_by(|a, b| {
        let paid = |project: &ProjectGates| {
            project
                .report
                .as_ref()
                .map(|report| report.landing.last_30_days.measured_ms)
        };
        paid(b)
            .cmp(&paid(a))
            .then_with(|| a.project.cmp(&b.project))
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        for project in &output.projects {
            println!(
                "{}: {}",
                project.project,
                match project.declares_gates {
                    Some(true) => "gates declared",
                    Some(false) => "no gates declared",
                    None => "unobserved",
                }
            );
            if let Some(report) = &project.report {
                gate_reports::render(report);
            }
        }
        for retired in &output.retired {
            retired.render();
        }
        render_collection(&output.collection);
    }
    for failure in &output.collection.failures {
        eprintln!(
            "cannot read {} [{}]: {}",
            failure.project, failure.component, failure.reason
        );
    }
    Ok(if output.collection.failed == 0 { 0 } else { 16 })
}

fn project_report(
    cfg: &crate::config::Config,
    project: &crate::registry::Project,
    anchor: &Path,
) -> Result<gate_reports::GateReport> {
    // A failed registry ledger lookup must not turn unreadable evidence into no runs.
    let ledger = match &project.ledger {
        Some(root) => Some(root.clone()),
        None => crate::registry::ledger_at(cfg, anchor)?,
    };
    let store = ledger.as_deref().map(Store::open_at).transpose()?.flatten();
    gate_reports::collect(anchor, store.as_ref())
}
