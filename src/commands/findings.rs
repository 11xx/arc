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
                let disposition = finding
                    .effective_status()
                    .map(|status| format!("{status:?}").to_lowercase())
                    .unwrap_or_else(|| "open".into());
                println!(
                    "{} [{:?}] {} — {disposition}",
                    finding.id, finding.severity, finding.summary
                );
            }
            if !audit && !state.audit_findings.is_empty() {
                println!(
                    "({} audit finding(s) raised after integration; arc findings {reference} --audit)",
                    state.audit_findings.len()
                );
            }
        }
        FindingsFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&FindingsJson {
                    schema: "arc-findings/1",
                    change_id: &state.change_id,
                    audit,
                    findings: selected.values().collect(),
                })?
            );
        }
        FindingsFormat::Sarif => {
            let results = selected
                .values()
                .filter(|finding| finding.effective_status().is_none())
                .map(|finding| {
                    let mut result = serde_json::json!({
                        "ruleId": finding.id,
                        "level": sarif_level(finding),
                        "message": { "text": finding.summary },
                    });
                    if let Some(anchor) = &finding.anchor {
                        result["locations"] = serde_json::json!([{
                            "physicalLocation": {
                                "artifactLocation": { "uri": anchor.path },
                                "region": {
                                    "startLine": anchor.line_start.unwrap_or(1),
                                    "endLine": anchor.line_end.or(anchor.line_start).unwrap_or(1),
                                }
                            }
                        }]);
                    }
                    result
                })
                .collect::<Vec<_>>();
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
}

fn sarif_level(finding: &FindingState) -> &'static str {
    match finding.severity {
        Severity::Critical | Severity::Major => "error",
        Severity::Minor => "warning",
        Severity::Note => "note",
    }
}
