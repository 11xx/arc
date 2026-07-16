use crate::state::ChangeState;
use crate::status::StatusReport;
use std::fmt::Write;

/// Human-readable Markdown view of one change. Suitable for terminals
/// and for dropping into a /thread artifact; the ledger stays private.
pub fn markdown(state: &ChangeState, report: &StatusReport) -> String {
    let mut out = String::new();
    let w = &mut out;

    let _ = writeln!(w, "# {} (`{}`)", state.title, state.change_id);
    let _ = writeln!(w);
    let _ = writeln!(w, "- State: {}", report.state);
    let _ = writeln!(w, "- Profile: {}", state.profile);
    let _ = writeln!(
        w,
        "- Branch: `{}` → `{}`",
        state.branch, state.target_branch
    );
    let _ = writeln!(w, "- Base: `{}`", state.base);
    if let Some(wt) = &state.worktree {
        let _ = writeln!(w, "- Worktree: `{wt}`");
    }
    if let Some(hold) = &state.hold {
        let _ = writeln!(w, "- **Hold active:** {hold}");
    }
    if let Some(c) = &state.closure {
        let _ = writeln!(
            w,
            "- Closed: {:?}{}",
            c.outcome,
            c.integrated_commit
                .as_deref()
                .map(|s| format!(" at `{s}`"))
                .unwrap_or_default()
        );
    }

    if !state.patchsets.is_empty() {
        let _ = writeln!(w, "\n## Patchsets\n");
        for p in &state.patchsets {
            let _ = writeln!(w, "- `{}`: `{}` → `{}`", p.id, p.base, p.head);
        }
    }

    if let Some(v) = &report.verdict {
        let _ = writeln!(w, "\n## Verdict\n");
        let _ = writeln!(
            w,
            "- {:?} on `{}` by {} — {}",
            v.verdict,
            v.patchset_id,
            v.actor,
            if v.valid_for_current_head {
                "valid for current head"
            } else {
                "STALE for current head"
            }
        );
    }

    if !state.findings.is_empty() {
        let _ = writeln!(w, "\n## Findings\n");
        for f in state.findings.values() {
            let status = f
                .effective_status()
                .map(|s| format!("{s:?}").to_lowercase())
                .unwrap_or_else(|| {
                    if f.contested() {
                        "CONTESTED".into()
                    } else {
                        "open".into()
                    }
                });
            let _ = writeln!(
                w,
                "- `{}` [{}{:?}] {} — {}",
                f.id,
                if f.blocking { "blocking/" } else { "" },
                f.severity,
                f.summary,
                status
            );
            if let Some(a) = &f.anchor {
                let _ = writeln!(
                    w,
                    "  - `{}` ({:?}{})",
                    a.path,
                    a.side,
                    a.line_start
                        .map(|s| format!(", lines {}-{}", s, a.line_end.unwrap_or(s)))
                        .unwrap_or_default()
                );
            }
            for d in &f.dispositions {
                let _ = writeln!(
                    w,
                    "  - disposition: {:?} by {}{}",
                    d.status,
                    d.actor,
                    d.commit
                        .as_deref()
                        .map(|c| format!(" (commit `{c}`)"))
                        .unwrap_or_default()
                );
            }
        }
    }

    if !report.gates.is_empty() {
        let _ = writeln!(w, "\n## Gates\n");
        for g in &report.gates {
            let _ = writeln!(
                w,
                "- {}: `{}` — {}",
                g.name,
                g.command,
                if g.green_at_head {
                    "green at head"
                } else {
                    "NOT green at head"
                }
            );
        }
    }

    if !state.verifications.is_empty() {
        let _ = writeln!(w, "\n## Verifications\n");
        for v in &state.verifications {
            let _ = writeln!(
                w,
                "- {} `{}` at `{}` → {:?} (on {})",
                v.gate.as_deref().unwrap_or("(ad hoc)"),
                v.command,
                v.revision,
                v.result,
                v.hostname
            );
        }
    }

    if !state.comments.is_empty() {
        let _ = writeln!(w, "\n## Comments\n");
        for c in &state.comments {
            let _ = writeln!(w, "- {} (`{}`): {}", c.actor, c.event_id, c.body);
            for (_, actor, body) in &c.replies {
                let _ = writeln!(w, "  - {actor}: {body}");
            }
        }
    }

    let _ = writeln!(w, "\n## Integration\n");
    if report.integrate_ready {
        let _ = writeln!(w, "- ready to integrate");
    } else {
        for b in &report.blockers {
            let _ = writeln!(w, "- blocker: {b:?}");
        }
    }

    out
}
