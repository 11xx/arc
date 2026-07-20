//! Patchset diffs with unresolved finding anchors.

use super::*;
use crate::state::Patchset;

/// Render one recorded patchset through Git, optionally followed by the
/// unresolved findings whose anchors are checked against that patchset head.
#[allow(clippy::too_many_arguments)]
pub fn diff(
    ctx: &Ctx,
    reference: &str,
    patchset: Option<String>,
    stat: bool,
    findings: bool,
    between: Option<Vec<String>>,
    since_approved: bool,
    paths: Vec<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, state) = ctx.load_state(&store, reference)?;
    let patchset = match patchset {
        Some(id) => state
            .patchsets
            .iter()
            .find(|patchset| patchset.id == id)
            .with_context(|| format!("unknown patchset {id:?}"))?,
        None => state.latest_patchset().with_context(|| {
            format!("no snapshot recorded for {change_id}; run `arc snapshot {change_id}` first")
        })?,
    };

    let (left, right) = if let Some(ids) = between {
        let left = lookup_patchset(&state, &ids[0])?;
        let right = lookup_patchset(&state, &ids[1])?;
        (left.head.clone(), right.head.clone())
    } else if since_approved {
        let verdict = state
            .verdicts
            .iter()
            .rev()
            .find(|verdict| verdict.verdict == Verdict::Approved)
            .context(
                "no approved patchset recorded; run `arc review <change> --verdict approved` first",
            )?;
        let approved = lookup_patchset(&state, &verdict.patchset_id)?;
        (approved.head.clone(), patchset.head.clone())
    } else {
        (patchset.base.clone(), patchset.head.clone())
    };
    let mut args = vec!["diff".to_string()];
    if stat {
        args.push("--stat".to_string());
    }
    args.extend([left, right]);
    if !paths.is_empty() {
        args.push("--".to_string());
        args.extend(paths);
    }
    gitio::git_inherit(&ctx.cwd, &args)?;

    if findings {
        render_findings(ctx, &state, &patchset.id, &patchset.head);
    }
    Ok(())
}

fn lookup_patchset<'a>(state: &'a ChangeState, id: &str) -> Result<&'a Patchset> {
    state
        .patchsets
        .iter()
        .find(|patchset| patchset.id == id)
        .with_context(|| format!("unknown patchset {id:?}"))
}

fn render_findings(ctx: &Ctx, state: &ChangeState, patchset_id: &str, head: &str) {
    let findings = state
        .findings
        .values()
        .filter(|finding| finding.effective_status().is_none())
        .collect::<Vec<_>>();
    if findings.is_empty() {
        return;
    }

    println!("\n## Open findings at {patchset_id}");
    for finding in findings {
        println!("- [{:?}] {}", finding.severity, finding.summary);
        if let Some(anchor) = &finding.anchor {
            let current = gitio::blob_oid(&ctx.cwd, head, &anchor.path);
            let marker = if anchor
                .blob
                .as_ref()
                .is_some_and(|blob| current.as_ref() == Some(blob))
            {
                "[anchored]"
            } else {
                "[drifted]"
            };
            let lines = anchor
                .line_start
                .map(|start| format!(":{start}-{}", anchor.line_end.unwrap_or(start)))
                .unwrap_or_default();
            println!("  - {marker} {}{lines}", anchor.path);
        }
    }
}
