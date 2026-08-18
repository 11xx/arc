//! Patchset diffs with unresolved finding anchors.

use super::*;
use crate::state::Patchset;

/// What to render, as the CLI expresses it. One struct rather than eight
/// positional arguments, because the selectors are mutually exclusive and a
/// caller reading the call site should see which one it chose.
pub struct DiffArgs {
    pub patchset: Option<String>,
    pub stat: bool,
    pub findings: bool,
    pub between: Option<Vec<String>>,
    pub since_approved: bool,
    pub integrated: bool,
    pub base: Option<String>,
    pub paths: Vec<String>,
}

/// Render one recorded patchset through Git, optionally followed by the
/// unresolved findings whose anchors are checked against that patchset head.
pub fn diff(ctx: &Ctx, reference: &str, args: DiffArgs) -> Result<()> {
    let DiffArgs {
        patchset,
        stat,
        findings,
        between,
        since_approved,
        integrated,
        base,
        paths,
    } = args;
    let store = ctx.store()?;
    let (change_id, state) = ctx.load_state(&store, reference)?;
    if integrated {
        return diff_integrated(ctx, &state, &change_id, stat, base, paths);
    }
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

/// The exact range an integration recorded: from where the target stood
/// before to the commit that landed. This is what an audit reviews — not a
/// patchset range, which describes the work rather than what reached the
/// target.
fn diff_integrated(
    ctx: &Ctx,
    state: &ChangeState,
    change_id: &str,
    stat: bool,
    base: Option<String>,
    paths: Vec<String>,
) -> Result<()> {
    let closure = state
        .closure
        .as_ref()
        .with_context(|| format!("{change_id} is not closed; nothing integrated to render"))?;
    let head = closure.integrated_commit.as_deref().with_context(|| {
        format!("{change_id} closed without an integration commit; nothing to render")
    })?;
    // A closure written before arc recorded the range knows what landed but
    // not what it landed onto. Guessing a base would misreport the range an
    // audit is about, so the caller supplies it.
    let base = match (base, closure.target_before.as_deref()) {
        (Some(base), _) => gitio::rev_parse(&ctx.cwd, &base)?,
        (None, Some(before)) => before.to_string(),
        (None, None) => bail!(
            "{change_id} recorded no integration base; pass --base <rev> to name what {head} \
             landed onto"
        ),
    };
    let mut args = vec!["diff".to_string()];
    if stat {
        args.push("--stat".to_string());
    }
    args.extend([base, head.to_string()]);
    if !paths.is_empty() {
        args.push("--".to_string());
        args.extend(paths);
    }
    gitio::git_inherit(&ctx.cwd, &args)?;
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
