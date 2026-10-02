//! Read-only finding projections for terminal and external review tooling.

use super::*;
use crate::state::FindingState;
use clap::ValueEnum;
use serde::Serialize;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum FindingsFormat {
    Text,
    Json,
    Sarif,
}

/// Render a change's findings without changing their ledger state.
///
/// Audit findings are listed apart from the ones raised before integration.
/// Merging them would answer "what was found reviewing this change" with a
/// set that includes what nobody knew when it shipped; omitting them would
/// leave an audit's findings write-only. So both are shown, labelled.
pub fn findings(ctx: &Ctx, reference: &str, format: FindingsFormat, audit: bool) -> Result<()> {
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    let selected = if audit {
        &state.audit_findings
    } else {
        &state.findings
    };
    match format {
        FindingsFormat::Text => {
            for finding in selected.values() {
                let shipped = finding
                    .effective_status()
                    .map(|status| format!("{status:?}").to_lowercase())
                    .unwrap_or_else(|| "open".into());
                let disposition = match finding.after_integration_status() {
                    Some(after) => format!("{shipped} at ship; {after} after integration"),
                    None => shipped,
                };
                println!(
                    "{} [{}{:?}] {} — {disposition}",
                    finding.id,
                    if finding.blocking { "blocking/" } else { "" },
                    finding.severity,
                    finding.summary
                );
                if let Some(patchset_id) = &finding.patchset_id {
                    println!("  against: {patchset_id}");
                }
                if let Some(anchor) = &finding.anchor {
                    println!("  at: {}", anchor_location(anchor));
                }
                for tip in finding.after_integration_tips() {
                    println!(
                        "  after integration: {} by {}{}",
                        format!("{:?}", tip.status).to_lowercase(),
                        tip.actor,
                        tip.commit
                            .as_deref()
                            .map(|commit| format!(" (commit {commit})"))
                            .unwrap_or_default()
                    );
                }
                // The body is what a reader needs to act; without it the only
                // way to learn what a finding says is to parse raw events. Its
                // own indentation is content, so only surrounding blank lines
                // are dropped.
                if let Some(body) = finding
                    .body
                    .as_deref()
                    .map(|body| body.trim_matches('\n'))
                    .filter(|body| !body.trim().is_empty())
                {
                    for line in body.lines() {
                        println!("  | {line}");
                    }
                }
            }
            if !audit && !state.audit_findings.is_empty() {
                println!(
                    "({} audit finding(s) raised after integration; arc findings {reference} --audit)",
                    state.audit_findings.len()
                );
            }
            for external in &state.external_verdicts {
                if external.findings.is_empty() {
                    continue;
                }
                println!(
                    "[external] {:?} by {} at {} (reference {})",
                    external.verdict, external.decided_by, external.revision, external.reference
                );
                for finding in &external.findings {
                    println!(
                        "  {} [{}{:?}] {}",
                        finding.finding_id,
                        if finding.blocking { "blocking/" } else { "" },
                        finding.severity,
                        finding.summary
                    );
                    if let Some(body) = &finding.body {
                        println!("    {body}");
                    }
                }
            }
        }
        FindingsFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&FindingsJson {
                    schema: "arc-findings/3",
                    change_id: &state.change_id,
                    audit,
                    findings: selected.values().collect(),
                    external_verdicts: state.external_verdicts.iter().collect(),
                })?
            );
        }
        FindingsFormat::Sarif => {
            let mut results = selected
                .values()
                .filter(|finding| {
                    finding.effective_status().is_none()
                        && finding.after_integration_status().is_none()
                })
                .map(|finding| {
                    // SARIF exists to carry file, line, and message. The
                    // summary is a label; the body is the message.
                    let message = match finding.body.as_deref().map(str::trim) {
                        Some(body) if !body.is_empty() => {
                            format!("{}\n\n{body}", finding.summary)
                        }
                        _ => finding.summary.clone(),
                    };
                    let mut result = serde_json::json!({
                        "ruleId": finding.id,
                        "level": sarif_level(finding),
                        "message": { "text": message },
                        // The one-line summary stays addressable on its own,
                        // for a consumer that wants a label rather than the
                        // whole finding.
                        "properties": { "summary": finding.summary },
                    });
                    if let Some(anchor) = &finding.anchor {
                        // A region whose end precedes its start is not valid
                        // SARIF, so a backwards range collapses to its start.
                        let start = anchor.line_start.unwrap_or(1);
                        let end = anchor.line_end.filter(|end| *end >= start).unwrap_or(start);
                        result["locations"] = serde_json::json!([{
                            "physicalLocation": {
                                "artifactLocation": { "uri": anchor.path },
                                "region": { "startLine": start, "endLine": end }
                            }
                        }]);
                    }
                    result
                })
                .collect::<Vec<_>>();
            for external in &state.external_verdicts {
                for finding in &external.findings {
                    let body = finding.body.as_deref().map(str::trim).unwrap_or_default();
                    let message = if body.is_empty() {
                        format!(
                            "[external review by {} at {} ({})] {}",
                            external.decided_by,
                            external.revision,
                            external.reference,
                            finding.summary
                        )
                    } else {
                        format!(
                            "[external review by {} at {} ({})] {}\n\n{body}",
                            external.decided_by,
                            external.revision,
                            external.reference,
                            finding.summary
                        )
                    };
                    let mut result = serde_json::json!({
                        "ruleId": finding.finding_id,
                        "level": sarif_level_severity(finding.severity),
                        "message": { "text": message },
                        "properties": {
                            "source": "external",
                            "verdict": format!("{:?}", external.verdict).to_lowercase(),
                            "decidedBy": external.decided_by,
                            "reference": external.reference,
                            "revision": external.revision,
                            "blocking": finding.blocking,
                            "summary": finding.summary,
                        },
                    });
                    if let Some(anchor) = &finding.anchor {
                        let start = anchor.line_start.unwrap_or(1);
                        let end = anchor.line_end.filter(|end| *end >= start).unwrap_or(start);
                        result["locations"] = serde_json::json!([{
                            "physicalLocation": {
                                "artifactLocation": { "uri": anchor.path },
                                "region": { "startLine": start, "endLine": end }
                            }
                        }]);
                    }
                    results.push(result);
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "version": "2.1.0",
                    "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
                    "runs": [{ "tool": { "driver": { "name": "arc" } }, "results": results }]
                }))?
            );
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct FindingsJson<'a> {
    schema: &'static str,
    change_id: &'a str,
    /// These are post-integration audit findings, not what shipped.
    audit: bool,
    findings: Vec<&'a FindingState>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    external_verdicts: Vec<&'a crate::state::ExternalVerdictEntry>,
}

/// `path:line` when the anchor has one, `path` otherwise, with the side that
/// the lines are numbered against. A range that runs backwards is not a range,
/// so only the line the reviewer started from is shown.
fn anchor_location(anchor: &crate::model::Anchor) -> String {
    let side = format!("{:?}", anchor.side).to_lowercase();
    let path = if anchor.path.trim().is_empty() {
        "(no path)"
    } else {
        anchor.path.as_str()
    };
    match (anchor.line_start, anchor.line_end) {
        (Some(start), Some(end)) if end > start => format!("{path}:{start}-{end} ({side})"),
        (Some(start), _) => format!("{path}:{start} ({side})"),
        (None, _) => format!("{path} ({side})"),
    }
}

fn sarif_level(finding: &FindingState) -> &'static str {
    sarif_level_severity(finding.severity)
}

fn sarif_level_severity(severity: crate::model::Severity) -> &'static str {
    match severity {
        Severity::Critical | Severity::Major => "error",
        Severity::Minor => "warning",
        Severity::Note => "note",
    }
}
