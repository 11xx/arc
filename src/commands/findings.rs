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
pub fn findings(ctx: &Ctx, reference: &str, format: FindingsFormat) -> Result<()> {
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    match format {
        FindingsFormat::Text => {
            for finding in state.findings.values() {
                let disposition = finding
                    .effective_status()
                    .map(|status| format!("{status:?}").to_lowercase())
                    .unwrap_or_else(|| "open".into());
                println!(
                    "{} [{:?}] {} — {disposition}",
                    finding.id, finding.severity, finding.summary
                );
            }
        }
        FindingsFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&FindingsJson {
                    schema: "arc-findings/1",
                    change_id: &state.change_id,
                    findings: state.findings.values().collect(),
                })?
            );
        }
        FindingsFormat::Sarif => {
            let results = state
                .findings
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
    findings: Vec<&'a FindingState>,
}

fn sarif_level(finding: &FindingState) -> &'static str {
    match finding.severity {
        Severity::Critical | Severity::Major => "error",
        Severity::Minor => "warning",
        Severity::Note => "note",
    }
}
