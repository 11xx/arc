use crate::commands::ArcAlternative;
use crate::model::{DebtCoverage, DebtIdentity, DebtMissing, DebtProduction, Event, Payload};
use crate::state::ChangeState;
use crate::status::{Blocker, BriefBaseDrift, GateStatus, StatusReport};
use std::collections::BTreeMap;
use std::fmt::Write;

/// Why a passing *gate* verification cannot be reused as evidence at its
/// revision. `None` when it can, when it did not pass — a failure explains
/// itself — or when it is not gate evidence: probe readiness is a different
/// question, answered by the probe's own baseline and final results.
fn unusable_gate_evidence_reason(
    entry: &crate::state::VerificationEntry,
    waiver: Option<&crate::state::DirtyTreeWaiver>,
) -> Option<&'static str> {
    if entry.gate.is_none()
        || entry.probe.is_some()
        || entry.green_at_head(waiver)
        || entry.result != crate::model::VerifyResult::Pass
    {
        return None;
    }
    Some(if entry.tree_moved {
        "the worktree changed while the command ran"
    } else if entry.worktree_dirty == Some(true) {
        "the worktree was dirty, so no checkout of this revision reproduces it"
    } else if entry.tested_tree.is_some() {
        "the worktree's cleanliness was not recorded"
    } else {
        "the tested tree was not recorded"
    })
}

/// What a passing gate line says about whether that gate could have failed.
///
/// A gate that has only ever passed and one watched to fail and then fixed
/// read identically otherwise, so the line says which it is. Empty for
/// anything but a pass: a failure has no discrimination to report, and neither
/// does a gate nobody has run.
///
/// Advisory. It qualifies a result; it never changes one.
pub fn discrimination_suffix(gate: &GateStatus) -> String {
    match (gate.discrimination, &gate.falsification) {
        (None, _) => String::new(),
        (Some(_), Some(falsification)) => format!(
            " (discriminating: failed at {}: {})",
            short_revision(&falsification.revision),
            falsification.predicted_reason
        ),
        (Some(_), None) => " (undiscriminated)".to_string(),
    }
}

/// Which run a gate's result comes from, when it is not one at this head.
///
/// A gate holds for a tree, so a run against another commit carrying the tree
/// being evaluated answers for this one. The line names that commit: a reader
/// checking gate output against `git log` would otherwise find no run at the
/// head at all. The tree itself is on the report's merged-tree line.
///
/// Advisory. It says where a result came from; it never changes one.
pub fn inheritance_suffix(gate: &GateStatus) -> String {
    match &gate.inherited_from {
        Some(revision) => format!(" (inherited from {})", short_revision(revision)),
        None => String::new(),
    }
}

/// What a gate's greenness is a claim about: the head's own content, or the
/// tree a merge into the target would ship when the change is behind it.
fn gate_scope(gate: &GateStatus) -> String {
    match gate.evaluated_tree.as_deref() {
        Some(tree) => format!("merged tree {}", short_revision(tree)),
        None => "head".to_string(),
    }
}

fn short_revision(revision: &str) -> &str {
    &revision[..revision.len().min(8)]
}

/// `merged tree: <short> (evaluated|unevaluated at <n> gates)`.
///
/// What a merge into the target would ship, and whether the required gates
/// have answered for it. `None` where no single tree exists to name: a textual
/// conflict, a missing branch or target, or a report built from the ledger
/// alone.
fn merged_tree_line(report: &StatusReport) -> Option<String> {
    let tree = report.merged_tree.as_deref()?;
    let evaluated = report
        .gates
        .iter()
        .filter(|gate| gate.evidence_event_id.is_some())
        .count();
    let (state, count) = if evaluated > 0 {
        ("evaluated", evaluated)
    } else {
        ("unevaluated", report.gates.len())
    };
    Some(format!(
        "merged tree: {} ({state} at {count} gate{})",
        short_revision(tree),
        if count == 1 { "" } else { "s" }
    ))
}

/// One gate's line in a human-facing gate list: the raw result, why a passing
/// result still does not count at head, which run the result came from when it
/// is not one at this head, and whether a counted pass was ever shown capable
/// of failing.
pub fn gate_line(gate: &GateStatus) -> String {
    let status = match gate.not_green_reason() {
        None => format!(
            "{}{}{}",
            gate.result,
            inheritance_suffix(gate),
            discrimination_suffix(gate)
        ),
        Some(reason) => format!(
            "{} (not green at {}: {reason}){}",
            gate.result,
            gate_scope(gate),
            inheritance_suffix(gate)
        ),
    };
    format!("{status}{}", gate_declaration_suffix(gate))
}

/// Which declarations readiness evaluated, and where they disagree with the
/// checkout or the change's own. Empty when nothing disagrees.
fn declaration_notes(report: &StatusReport) -> String {
    let mut out = String::new();
    if !report.declaration_notes.is_empty() {
        let _ = writeln!(out, "Declarations differ:");
        for note in &report.declaration_notes {
            let _ = writeln!(out, "  - {note}");
        }
    }
    out
}

fn gate_declaration_suffix(gate: &GateStatus) -> String {
    if gate.declared_by.is_empty() {
        " (declaration source unavailable)".to_string()
    } else {
        format!(" (declared by {})", gate.declared_by.join(", "))
    }
}

/// The authorization basis, as a human-readable block. Used by
/// `integrate --dry-run` to show what the merge would be recorded as resting
/// on, before anything is written.
pub fn authorization_basis(basis: &crate::model::AuthorizationBasis) -> String {
    let mut out = String::new();
    let _ = match &basis.verdict_event_id {
        Some(verdict) => writeln!(out, "    verdict: {verdict}"),
        None => writeln!(out, "    verdict: none — authorized by declared debt"),
    };
    if let Some(external) = &basis.external_verdict {
        let _ = writeln!(
            out,
            "    external verdict: {:?} by {} at {} (reference {})",
            external.verdict,
            external.decided_by,
            short_sha(&external.revision),
            external.reference
        );
    }
    let _ = match &basis.danger {
        Some(danger) => writeln!(
            out,
            "    danger: {} — {}",
            if danger.dangerous {
                "dangerous"
            } else {
                "not dangerous"
            },
            danger.explain()
        ),
        None => writeln!(out, "    danger: not recorded"),
    };
    for (gate, evidence) in &basis.gate_evidence {
        let sources = basis
            .gates
            .get(gate)
            .map(|declaration| declaration.declared_by.as_slice())
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "    gate {gate}: {evidence} (declared by {})",
            source_list(sources)
        );
    }
    for prerequisite in &basis.prerequisites {
        let _ = writeln!(
            out,
            "    prerequisite {}: closed by {}{}",
            prerequisite.change_id,
            prerequisite.closure_event_id,
            prerequisite
                .integrated_commit
                .as_deref()
                .map(|commit| format!(" at {}", short_sha(commit)))
                .unwrap_or_default()
        );
    }
    let _ = writeln!(
        out,
        "    blocking findings: {}; holds: {}",
        basis.blocking_findings.len(),
        basis.holds.len()
    );
    for (name, gate) in &basis.gates {
        let _ = writeln!(
            out,
            "    gate declaration {name}: {}{}{} (declared by {})",
            gate.command,
            gate.timeout
                .map(|timeout| format!(" (timeout {timeout}s)"))
                .unwrap_or_default(),
            if gate.profiles.is_empty() {
                String::new()
            } else {
                format!(" (profiles: {})", gate.profiles.join(", "))
            },
            source_list(&gate.declared_by)
        );
    }
    if let Some(debt) = &basis.audit_debt_event_id {
        let _ = writeln!(out, "    debt waiving review: {debt}");
    }
    let _ = writeln!(
        out,
        "    policy: forbid_self_approval={} (declared by {}), require_declared_actor={} (declared by {}), git_identity={} (declared by {})",
        basis.policy.forbid_self_approval,
        source_list_for_rule(
            &basis.policy.declared_by,
            &format!("policy.forbid_self_approval={}", basis.policy.forbid_self_approval)
        ),
        basis.policy.require_declared_actor,
        source_list_for_rule(
            &basis.policy.declared_by,
            &format!("policy.require_declared_actor={}", basis.policy.require_declared_actor)
        ),
        basis.policy.provenance_git_identity,
        source_list_for_rule(
            &basis.policy.declared_by,
            &format!("provenance.git_identity={}", basis.policy.provenance_git_identity)
        )
    );
    for (rule, sources) in &basis.policy.declared_by {
        if rule.starts_with("danger.paths[") {
            let _ = writeln!(
                out,
                "    danger rule {rule}: declared by {}",
                sources.join(", ")
            );
        }
    }
    out
}

fn source_list(sources: &[String]) -> String {
    if sources.is_empty() {
        "source unavailable".to_string()
    } else {
        sources.join(", ")
    }
}

fn source_list_for_rule(sources: &BTreeMap<String, Vec<String>>, rule: &str) -> String {
    sources
        .get(rule)
        .map(|entries| source_list(entries))
        .unwrap_or_else(|| "Arc default".to_string())
}

/// Human-readable Markdown view of one change. Suitable for terminals
/// and for dropping into a journal artifact; the ledger stays private.
pub fn markdown(
    state: &ChangeState,
    report: &StatusReport,
    alternatives: &[ArcAlternative],
) -> String {
    let mut out = String::new();
    let w = &mut out;

    let _ = writeln!(w, "# {} (`{}`)", state.title, state.change_id);
    let _ = writeln!(w);
    let _ = writeln!(w, "- State: {}", report.state);
    let _ = writeln!(w, "- Profile: {}", state.profile);
    if state.iterating {
        let _ = writeln!(
            w,
            "- Iterating: integration is not the goal until this is cleared"
        );
    }
    let _ = writeln!(
        w,
        "- Branch: `{}` → `{}`",
        state.branch, state.target_branch
    );
    let _ = writeln!(w, "- Base: `{}`", state.base);
    if let Some(wt) = &state.worktree {
        let _ = writeln!(w, "- Worktree: `{wt}`");
    }
    if let Some(file) = &state.journal_ref {
        let digest = state
            .journal_ref_digest
            .as_deref()
            .map(|digest| format!(" ({digest})"))
            .unwrap_or_default();
        let _ = writeln!(w, "- Opened from: `{file}`{digest}");
    }
    for hold in state.holds.values() {
        let _ = writeln!(
            w,
            "- **Hold active** (`{}`, {}): {}",
            hold.hold_event_id, hold.held_by, hold.reason
        );
    }
    if let Some(c) = &state.closure {
        // How an integration happened is the distinction the ledger exists to
        // hold; a human view that renders all three identically hides it from
        // the reader most likely to care.
        let how = match c.integration {
            Some(crate::state::IntegrationKind::Guarded) => " (guarded by arc)",
            Some(crate::state::IntegrationKind::Asserted) => " (asserted; arc did not guard it)",
            Some(crate::state::IntegrationKind::LegacyUnclassified) => {
                " (recorded before arc distinguished guarded from asserted)"
            }
            None => "",
        };
        let _ = writeln!(
            w,
            "- Closed: {:?}{}{how}",
            c.outcome,
            c.integrated_commit
                .as_deref()
                .map(|s| format!(" at `{s}`"))
                .unwrap_or_default()
        );
        if let (Some(branch), Some(before)) = (&c.target_branch, &c.target_before) {
            let _ = writeln!(
                w,
                "  - into `{branch}`, which stood at `{}`",
                &before[..before.len().min(8)]
            );
        }
        if let Some(reference) = &c.external_reference {
            let _ = writeln!(w, "  - external outcome reference: `{reference}`");
        }
    }
    if !state.tags.is_empty() {
        let _ = writeln!(w, "- Tags: {}", state.tags.join(", "));
    }
    if let Some(assigned) = &state.assigned_to {
        let _ = writeln!(w, "- Assigned to: {assigned}");
    }
    let _ = writeln!(w, "- Priority: {}", state.priority);
    let _ = writeln!(
        w,
        "- Worktree state: head {}; uncommitted edits {}",
        match (
            report.latest_patchset.as_ref(),
            report.head_matches_latest_patchset,
        ) {
            (None, _) => "has no recorded patchset",
            (Some(_), true) => "matches newest approved/snapshotted head",
            (Some(_), false) => "has moved past newest patchset",
        },
        match report.worktree_dirty {
            Some(true) => "present",
            Some(false) => "absent",
            None => "unknown",
        }
    );

    if let Some(line) = merged_tree_line(report) {
        let _ = writeln!(w, "- {line}");
    }
    let _ = writeln!(w, "- Next step: {}", next_step(report));

    if !report.blocker_status.blockers_ready.is_empty() {
        let _ = writeln!(w, "\n## Blocked by\n");
        for blocker in &report.blocker_status.blockers_ready {
            let _ = writeln!(
                w,
                "- {} (`{}`): {}{}{}",
                blocker.slug,
                blocker.change_id,
                blocker.status,
                if blocker.integrated { " ✓" } else { "" },
                blocker
                    .recovery
                    .as_deref()
                    .map(|recovery| format!(" — {recovery}"))
                    .unwrap_or_default()
            );
        }
    }

    if !alternatives.is_empty() {
        let _ = writeln!(w, "\n## Suggested alternatives (ready now)\n");
        for alternative in alternatives {
            let _ = writeln!(
                w,
                "- {} (`{}`): {}",
                alternative.slug, alternative.change_id, alternative.reason
            );
        }
    }

    if let Some(claim) = &report.claim {
        let _ = writeln!(w, "\n## Claim / Progress\n");
        let condition = if claim.expired {
            "EXPIRED"
        } else if claim.stale {
            "STALE"
        } else if claim.stage == "blocked-on" {
            "BLOCKED"
        } else {
            "active"
        };
        let _ = writeln!(
            w,
            "- Owner: {} via {}/{} — **{}**",
            claim.owner.actor, claim.owner.harness, claim.owner.session, condition
        );
        let _ = writeln!(
            w,
            "- Stage: `{}`{} — age {}s{}",
            claim.stage,
            claim
                .note
                .as_deref()
                .map(|note| format!(" — {note}"))
                .unwrap_or_default(),
            claim.age_seconds,
            claim
                .budget_seconds
                .map(|budget| format!(" / budget {budget}s"))
                .unwrap_or_default()
        );
        if let Some(blocker) = &claim.blocker {
            let blocker = match blocker {
                crate::model::BlockerRef::Brief { brief_event_id } => {
                    format!("brief `{brief_event_id}`")
                }
                crate::model::BlockerRef::Finding { finding_id } => {
                    format!("finding `{finding_id}`")
                }
                crate::model::BlockerRef::Change { change_id } => {
                    format!("change `{change_id}`")
                }
                crate::model::BlockerRef::External => "external".to_string(),
            };
            let _ = writeln!(w, "- Blocker: {blocker}");
        }
        let _ = writeln!(
            w,
            "- Activity: claimed {}, last {}, expires {} (TTL {}s)",
            claim.claimed_at, claim.last_activity_at, claim.expires_at, claim.ttl_seconds
        );
        if let Some(reason) = &claim.displaced_reason {
            let _ = writeln!(w, "- displaced before its budget: {reason}");
        }
    }

    if !state.patchsets.is_empty() {
        let _ = writeln!(w, "\n## Patchsets\n");
        for p in &state.patchsets {
            let _ = writeln!(w, "- `{}`: `{}` → `{}`", p.id, p.base, p.head);
            if let (Some(brief_ref), Some(version)) = (&p.brief_ref, p.brief_version) {
                let _ = writeln!(w, "  - brief: v{version} (`{}`)", brief_ref.event_id);
            }
            if p.contributors.is_empty() {
                let _ = writeln!(
                    w,
                    "  - contributors: {} (declared-by-invoker)",
                    p.effective_author()
                );
            } else {
                let _ = writeln!(w, "  - contributors: {}", p.contributors.join(", "));
            }
            if let Some(subject) = &p.on_behalf_of {
                let _ = writeln!(w, "  - snapshot by: {} (for {subject})", p.actor);
            }
            if let Some(author) = &p.author {
                let _ = writeln!(
                    w,
                    "  - author: {}{}",
                    author.name,
                    author
                        .email
                        .as_deref()
                        .map(|email| format!(" <{email}>"))
                        .unwrap_or_default()
                );
            }
            if let Some(committer) = &p.committer {
                let _ = writeln!(
                    w,
                    "  - committer: {}{}",
                    committer.name,
                    committer
                        .email
                        .as_deref()
                        .map(|email| format!(" <{email}>"))
                        .unwrap_or_default()
                );
            }
            for link in &p.journal_refs {
                let via = link
                    .via
                    .map(|via| format!(", via {}", via.as_str()))
                    .unwrap_or_default();
                let _ = writeln!(w, "  - framed by: `{}` ({}){via}", link.file, link.digest);
            }
            if let Some(thread) = &p.thread {
                let _ = writeln!(w, "  - thread: {}:{}", thread.scheme, thread.id);
            }
            if let Some(actor) = &p.claim_actor {
                let provenance_warning = if report.provenance_check_enabled
                    && p.provenance_mismatch == Some(true)
                {
                    format!(
                        " — **PROVENANCE MISMATCH**; use `--on-behalf-of` for a delegated snapshot or set `[provenance] git_identity = \"shared\"` when the project uses one committing identity (declared by {})",
                        source_list_for_rule(
                            &report.policy_sources,
                            "provenance.git_identity=per-actor"
                        )
                    )
                } else {
                    String::new()
                };
                let _ = writeln!(
                    w,
                    "  - claim actor at snapshot: {}{}",
                    actor, provenance_warning
                );
            }
        }
    }

    if let Some(brief) = state.latest_brief() {
        let version = state.briefs.len();
        let _ = writeln!(w, "\n## Brief (v{version})\n");
        if let Some(title) = &brief.title {
            let _ = writeln!(w, "### {title}\n");
        }
        if let Some(base_revision) = &brief.base_revision {
            let drift = report
                .brief
                .as_ref()
                .and_then(|brief| brief.base_drift.as_ref())
                .and_then(BriefBaseDrift::annotation)
                .unwrap_or_default();
            let _ = writeln!(w, "- Base revision: `{base_revision}`{drift}");
        }
        if let (Some(plan_ref), Some(plan_slice)) = (&brief.plan_ref, &brief.plan_slice) {
            let _ = writeln!(w, "- Plan: `{plan_ref}`");
            let _ = writeln!(w, "- Slice: `{plan_slice}`\n");
        }
        if !brief.acceptance_probes.is_empty() {
            let _ = writeln!(w, "- Acceptance probes:");
            for probe in &brief.acceptance_probes {
                let _ = writeln!(w, "  - `{}`: `{}`", probe.name, probe.command);
            }
            let _ = writeln!(w);
        }
        let _ = write!(w, "{}", brief.body);
        if !brief.body.ends_with('\n') {
            let _ = writeln!(w);
        }
    }

    if let Some(v) = &report.verdict {
        let _ = writeln!(w, "\n## Verdict\n");
        let by = match &v.on_behalf_of {
            Some(subject) => format!("{} (for {subject})", v.actor),
            None => v.actor.clone(),
        };
        let _ = writeln!(
            w,
            "- {:?} on `{}` by {} — {}",
            v.verdict,
            v.patchset_id,
            by,
            if v.valid_for_current_head {
                "valid for current head"
            } else {
                "STALE for current head"
            }
        );
        if let Some(reason) = &report.approval_rejection_reason {
            let _ = writeln!(w, "- {reason}");
        }
        if report.approval_rejection_reason.is_some() {
            if let Some(line) = review_subject_line(report) {
                let _ = writeln!(w, "- {line}");
            }
        }
        if let Some(body) = &v.body {
            let _ = writeln!(w, "\n{body}");
        }
    }

    if !report.external_verdicts.is_empty() {
        let _ = writeln!(w, "\n## External verdicts\n");
        for external in &report.external_verdicts {
            let _ = writeln!(
                w,
                "- [external] {:?} at `{}` by {} — {} (reference `{}`)",
                external.verdict,
                external.revision,
                external.decided_by,
                if external.gates_current_head {
                    "valid for current head"
                } else if external.matches_current_patchset {
                    "applies to current patchset but does not satisfy local review policy"
                } else {
                    "stale for current head"
                },
                external.reference
            );
            for finding in &external.findings {
                let _ = writeln!(
                    w,
                    "  - external finding `{}` [{}{:?}] {}",
                    finding.finding_id,
                    if finding.blocking { "blocking/" } else { "" },
                    finding.severity,
                    finding.summary
                );
                if let Some(body) = &finding.body {
                    let _ = writeln!(w, "    {body}");
                }
            }
        }
    }

    if !state.findings.is_empty() {
        let _ = writeln!(w, "\n## Findings\n");
        for f in state.findings.values() {
            let shipped = f
                .effective_status()
                .map(|s| format!("{s:?}").to_lowercase())
                .unwrap_or_else(|| {
                    if f.contested() {
                        "CONTESTED".into()
                    } else {
                        "open".into()
                    }
                });
            let status = match f.after_integration_status() {
                Some(after) => format!("{shipped} at ship; {after} after integration"),
                None => shipped,
            };
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
                    "  - disposition: {:?} by {}{}{}",
                    d.status,
                    d.actor,
                    d.commit
                        .as_deref()
                        .map(|c| format!(" (commit `{c}`)"))
                        .unwrap_or_default(),
                    d.evidence_event_id
                        .as_deref()
                        .map(|id| format!(" (evidence event `{id}`)"))
                        .unwrap_or_default()
                );
            }
            for d in &f.after_integration {
                let _ = writeln!(
                    w,
                    "  - after integration: {:?} by {}{}",
                    d.status,
                    d.actor,
                    d.commit
                        .as_deref()
                        .map(|c| format!(" (commit `{c}`)"))
                        .unwrap_or_default()
                );
            }
            for reply in &f.replies {
                let _ = writeln!(w, "  - {}: {}", reply.actor, reply.body);
            }
        }
    }

    if !report.gates.is_empty() {
        let _ = writeln!(w, "\n## Gates\n");
        for g in &report.gates {
            let _ = writeln!(
                w,
                "- {}: `{}` — {}{}{}{}",
                g.name,
                g.command,
                if g.green_at_head {
                    "green at head"
                } else {
                    "NOT green at head"
                },
                if g.attested {
                    " (attested)"
                } else if g.timed_out {
                    " (timed out)"
                } else {
                    ""
                },
                discrimination_suffix(g),
                gate_declaration_suffix(g)
            );
            if g.attested {
                let _ = writeln!(
                    w,
                    "  - attested by {} on {}",
                    g.runner.as_deref().unwrap_or("unknown runner"),
                    g.hostname.as_deref().unwrap_or("unknown host")
                );
            }
            if let Some(output_tail) = &g.output_tail {
                let marker = if output_tail.len() >= 4096 {
                    "[output truncated to final 4096 bytes]"
                } else {
                    "[output tail]"
                };
                let _ = writeln!(w, "  {marker}");
                for line in output_tail.lines() {
                    let _ = writeln!(w, "    {line}");
                }
            }
        }
    } else {
        let _ = writeln!(w, "\n## Gates\n");
        let _ = writeln!(w, "- none declared for profile {}", report.profile);
    }

    if !report.declaration_notes.is_empty() {
        let _ = writeln!(w, "\n## Declarations differ\n");
        for note in &report.declaration_notes {
            let _ = writeln!(w, "- {note}");
        }
    }

    if !report.policy_sources.is_empty() {
        let _ = writeln!(w, "\n## Policy declarations\n");
        for (rule, sources) in &report.policy_sources {
            let _ = writeln!(w, "- {rule}: declared by {}", sources.join(", "));
        }
    }

    if !report.probes.is_empty() {
        let _ = writeln!(w, "\n## Acceptance probes\n");
        for probe in &report.probes {
            let _ = writeln!(
                w,
                "- `{}` (brief v{}): baseline {} at `{}`; final {} at `{}` — {}",
                probe.name,
                probe.brief_version,
                probe.baseline_result,
                probe.baseline_revision,
                probe.final_result,
                probe.final_revision,
                if probe.discriminating_at_head {
                    "discriminating at head"
                } else {
                    "NOT discriminating at head"
                }
            );
        }
        let _ = writeln!(
            w,
            "\nBase-fail/head-pass proves behavioral discrimination, not semantic relevance; \
             the reviewer must inspect the baseline output and confirm it failed for the intended reason."
        );
    }

    if !state.verification_runs.is_empty() {
        let _ = writeln!(w, "\n## Verification runs\n");
        for run in &state.verification_runs {
            let _ = writeln!(
                w,
                "### Verification run `{}` — {}\n",
                run.run_id,
                if run.complete {
                    "complete"
                } else {
                    "incomplete"
                }
            );
            let _ = writeln!(
                w,
                "- Revision: `{}`; mode: {:?}; skip green: {}",
                run.revision, run.mode, run.skip_green
            );
            for terminal in &run.terminals {
                match terminal {
                    crate::state::VerificationRunTerminal::Recorded {
                        gate,
                        evidence_event_id,
                        result,
                    } => {
                        let _ =
                            writeln!(w, "- {gate}: observed {:?} (`{evidence_event_id}`)", result);
                    }
                    crate::state::VerificationRunTerminal::Reused {
                        gate,
                        evidence_event_id,
                        reuse_event_id,
                    } => {
                        let _ = writeln!(
                            w,
                            "- {gate}: reused `{evidence_event_id}` (`{reuse_event_id}`)"
                        );
                    }
                }
            }
            if !run.missing_gates.is_empty() {
                let _ = writeln!(w, "- Missing: {}", run.missing_gates.join(", "));
            }
        }
    }

    if !state.verifications.is_empty() {
        let _ = writeln!(w, "\n## Verifications\n");
        for v in &state.verifications {
            let label = match (&v.probe, &v.gate) {
                (Some(probe), _) => format!(
                    "probe {} {:?} (brief {})",
                    probe.name, probe.phase, probe.brief_event_id
                ),
                (None, Some(gate)) => gate.clone(),
                (None, None) => "(ad hoc)".into(),
            };
            let _ = writeln!(
                w,
                "- {} `{}` at `{}` → {:?}{} (on {})",
                label,
                v.command,
                v.revision,
                v.result,
                if v.attested {
                    " (attested)"
                } else if v.timed_out {
                    " (timed out)"
                } else {
                    ""
                },
                v.hostname
            );
            if v.attested {
                let _ = writeln!(
                    w,
                    "  - attested by {} on {}",
                    v.runner.as_deref().unwrap_or("unknown runner"),
                    v.hostname
                );
            }
            // A passing run that cannot be reused reads as `Pass` above, next
            // to a gate summary that says the same gate is not green. Saying
            // why here is what keeps the two from contradicting each other.
            if let Some(reason) = unusable_gate_evidence_reason(v, state.dirty_tree_waiver.as_ref())
            {
                let _ = writeln!(w, "  - not reusable as evidence: {reason}");
            }
        }
    }

    if !state.messages.is_empty() {
        let _ = writeln!(w, "\n## Messages\n");
        for m in &state.messages {
            let _ = writeln!(
                w,
                "- [{}/{}] {} ({})",
                m.message_type.as_str(),
                m.severity.as_str(),
                m.summary,
                m.actor
            );
            if let Some(detail) = &m.detail {
                let _ = writeln!(w, "  - {detail}");
            }
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

    if let Some(forge) = &report.forge {
        let _ = writeln!(w, "\n## Forge\n");
        let _ = writeln!(w, "- Projection: {}", forge.projection);
        if let Some(declared) = &forge.declared {
            let _ = writeln!(
                w,
                "- Declared: {} — base `{}`@`{}` ← head `{}`@`{}` (policy {})",
                declared.host,
                declared.base_repo,
                declared.base_ref,
                declared.head_repo,
                declared.head_ref,
                declared.policy
            );
        }
        if let Some(link) = &forge.link {
            let _ = writeln!(
                w,
                "- PR #{}: {} (head `{}`)",
                link.pr_number, link.url, link.head_sha
            );
            let _ = writeln!(
                w,
                "- Head match: {}",
                if forge.head_match {
                    "yes"
                } else {
                    "NO — linked head differs from approved patchset"
                }
            );
        }
        let _ = writeln!(
            w,
            "- Checks: {}{}",
            forge.checks,
            forge
                .checks_detail
                .as_deref()
                .map(|detail| format!(" — {detail}"))
                .unwrap_or_default()
        );
        if let Some(pr_state) = &forge.pr_state {
            let _ = writeln!(
                w,
                "- PR state: {}{}",
                pr_state.state,
                pr_state
                    .merge_sha
                    .as_deref()
                    .map(|sha| format!(" (merge `{sha}`)"))
                    .unwrap_or_default()
            );
        }
        let _ = writeln!(
            w,
            "- Forge ready: {}",
            if forge.forge_ready { "yes" } else { "no" }
        );
        for caveat in &forge.caveats {
            let _ = writeln!(w, "  - caveat: {caveat}");
        }
        if let Some(awaiting) = &forge.awaiting_user {
            let _ = writeln!(
                w,
                "- **Awaiting user:** open PR {} at head `{}`",
                awaiting.pr_url, awaiting.head_sha
            );
        }
    }

    if !report.review_map.is_empty() {
        let _ = writeln!(w, "\n## Review coverage\n");
        for row in &report.review_map {
            let attribution = if let Some(contributor) = &row.matched_contributor {
                format!(
                    " — non-independent: matches contributor {contributor} ({})",
                    row.contributors_source
                )
            } else if row.attribution_unknown {
                " — attribution unknown: nobody declared this identity".to_string()
            } else if row.is_author {
                " — non-independent".to_string()
            } else {
                String::new()
            };
            let _ = writeln!(
                w,
                "- {} last saw `{}`{}{}{}",
                row.reviewer,
                row.last_patchset,
                if row.covers_final {
                    " (covers the final patchset)"
                } else {
                    " (**stale**)"
                },
                attribution,
                format_args!(" [{} verdicts, {} findings]", row.verdicts, row.findings),
            );
        }
        for advisory in &report.advisories {
            let _ = writeln!(w, "- advisory ({}): {}", advisory.code, advisory.detail);
        }
    }

    if report.debt.is_some() || !report.audit_verdicts.is_empty() {
        let _ = writeln!(w, "\n## Post-integration audit\n");
        if let Some(debt) = &report.debt {
            let _ = writeln!(
                w,
                "- Owed{}: {} (declared by {})",
                if report.debt_outstanding {
                    ""
                } else {
                    " (discharged)"
                },
                debt.reason,
                debt.actor
            );
            if let Some(missing) = debt.missing {
                let _ = writeln!(w, "  - Missing: {}", missing.as_str());
            } else {
                let _ = writeln!(w, "  - Record: legacy");
            }
            if let Some(production) = &debt.production {
                let _ = writeln!(w, "  - Produced: {}", debt_production_label(production));
            }
            if let Some(coverage) = &debt.coverage {
                if coverage.is_empty() {
                    let _ = writeln!(w, "  - Coverage: none");
                } else {
                    let _ = writeln!(w, "  - Coverage:");
                    for reviewer in coverage {
                        let _ = writeln!(w, "    - {}", debt_coverage_detail(reviewer));
                    }
                }
            }
            if let Some(discharged_by) = &debt.discharged_by {
                let _ = writeln!(
                    w,
                    "  - Discharged by: {}",
                    debt_coverage_detail(discharged_by)
                );
            }
        }
        for audit in &report.audit_verdicts {
            let _ = writeln!(
                w,
                "- {:?} at `{}` by {}{}{}",
                audit.verdict,
                &audit.revision[..audit.revision.len().min(8)],
                audit.effective_author(),
                audit
                    .model
                    .as_deref()
                    .map(|model| format!(" ({model})"))
                    .unwrap_or_default(),
                audit
                    .body
                    .as_deref()
                    .map(|body| format!(" — {}", body.lines().next().unwrap_or_default()))
                    .unwrap_or_default()
            );
        }
    }

    let _ = writeln!(w, "\n## Integration\n");
    if report.integrate_ready() {
        let _ = writeln!(w, "- ready to integrate");
    } else {
        for b in report.blockers() {
            let _ = writeln!(w, "- blocker: {b:?}");
        }
    }

    out
}

/// Advisory review-coverage lines, printed after the ready/blocked verdict.
///
/// These are advisories by design. Blocking on thin coverage would refuse the
/// single-reviewer changes that make up most of the work, and an
/// orchestrator's review is a valid review unless a project's policy says
/// otherwise; the point is that nobody integrates without having been told.
/// One line naming the identities the independence check compares on the
/// current review subject. Shown beside approval rejection and in check
/// output so a lead reading the refusal sees the exact sets that were compared,
/// without re-deriving them from snapshot events.
pub fn review_subject_line(report: &StatusReport) -> Option<String> {
    let subject = report.review_subject.as_ref()?;
    Some(review_subject_detail(subject))
}

/// One identity-comparison line for a review subject carried outside a full
/// status report, such as an integrated debt row in `catchup`.
pub fn review_subject_detail(subject: &crate::status::ReviewSubject) -> String {
    let contributors = subject.contributors.join(", ");
    format!(
        "review subject: `{}` compares reviewer against contributors [{}] ({})",
        subject.patchset_id, contributors, subject.basis,
    )
}

pub fn advisories(report: &StatusReport) {
    if report.advisories.is_empty() {
        return;
    }
    println!("\nAdvisories (never blocking):");
    for advisory in &report.advisories {
        println!("  {}: {}", advisory.code, advisory.detail);
    }
}

/// Detailed refusal text for `check` and `integrate`. Exit codes remain the
/// machine contract; this text tells a human or executor how to recover.
/// The headline names every blocker, and each blocker's detail precedes the
/// next step, so a reader who keeps any one line of it still learns why.
pub fn blocker_explanation(state: &ChangeState, report: &StatusReport) -> String {
    let mut out = String::new();
    let titles = report
        .blockers()
        .iter()
        .map(|blocker| blocker_title(*blocker))
        .collect::<Vec<_>>();
    if titles.is_empty() {
        let _ = writeln!(out, "Cannot integrate {}", state.change_id);
    } else {
        let _ = writeln!(
            out,
            "Cannot integrate {}: {}",
            state.change_id,
            titles.join("; ")
        );
    }
    let _ = writeln!(out);

    for (index, blocker) in report.blockers().iter().enumerate() {
        let _ = writeln!(out, "Blocker {}: {}", index + 1, blocker_title(*blocker));
        match blocker {
            Blocker::Closed => {
                let _ = writeln!(out, "  - Change is already closed");
            }
            Blocker::BranchMissing => {
                let _ = writeln!(out, "  - Branch `{}` is missing", state.branch);
            }
            Blocker::TargetUnreadable => {
                let _ = writeln!(
                    out,
                    "  - Target branch `{}` cannot be resolved, so the gate and policy \
                     declarations the change is judged by could not be read",
                    state.target_branch
                );
            }
            Blocker::ForkBranch => {
                // The refusal names the change's branch, not the directory the
                // caller stands in. The report carries the fork; the branch
                // stands in as the slug only if a report somehow reached here
                // without it.
                let slug = report.fork.as_deref().unwrap_or(state.branch.as_str());
                let _ = writeln!(
                    out,
                    "  - {}",
                    crate::commands::fork::integrate_refusal(&state.branch, slug)
                );
            }
            Blocker::Iterating => {
                let _ = writeln!(
                    out,
                    "  - change declares it is iterating; clear it with `arc iterating {} --off`",
                    state.change_id
                );
            }
            Blocker::BlockedByChanges => {
                for dependency in report
                    .blocker_status
                    .blockers_ready
                    .iter()
                    .filter(|dependency| !dependency.integrated)
                {
                    let _ = writeln!(
                        out,
                        "  - {} (`{}`): {}{}",
                        dependency.slug,
                        dependency.change_id,
                        dependency.status,
                        dependency
                            .recovery
                            .as_deref()
                            .map(|recovery| format!(" — {recovery}"))
                            .unwrap_or_default()
                    );
                }
            }
            Blocker::NeedsRebase => {
                let _ = writeln!(
                    out,
                    "  - needs rebase: target {} moved with conflicting changes; run `arc rebase \
                     {}`, then rerun the gates and re-review",
                    state.target_branch, state.change_id
                );
            }
            Blocker::MergedTreeUnevaluated => {
                let _ = writeln!(
                    out,
                    "  - merging into {} would ship tree {}, which no required gate has run \
                     against",
                    state.target_branch,
                    report
                        .merged_tree
                        .as_deref()
                        .map(short_revision)
                        .unwrap_or("unknown")
                );
                let _ = writeln!(
                    out,
                    "  - evaluate that merge with `arc verify --against {}`",
                    state.target_branch
                );
                let _ = writeln!(
                    out,
                    "  - the result is spent as soon as {} moves again, exactly as a verdict is",
                    state.target_branch
                );
            }
            Blocker::BlockingFindings => {
                for finding in report
                    .findings
                    .iter()
                    .filter(|finding| report.open_blocking_findings.contains(&finding.id))
                {
                    let _ = writeln!(
                        out,
                        "  - `{}` [{:?}] {}{}",
                        finding.id,
                        finding.severity,
                        finding.summary,
                        finding_era(finding, report)
                    );
                }
            }
            Blocker::NoValidApproval => {
                let source = report
                    .policy_sources
                    .get("policy.forbid_self_approval=true")
                    .map(|items| format!(" (declared by {})", items.join(", ")))
                    .unwrap_or_default();
                let _ = writeln!(
                    out,
                    "  - {}{source}",
                    report
                        .approval_rejection_reason
                        .as_deref()
                        .unwrap_or("Current head has no valid approval")
                );
                if let Some(line) = review_subject_line(report) {
                    let _ = writeln!(out, "  - {line}");
                }
            }
            Blocker::GatesNotGreen => {
                for gate in report.gates.iter().filter(|gate| !gate.green_at_head) {
                    let _ = writeln!(
                        out,
                        "  - Gate `{}` is not green at {}: {}{}",
                        gate.name,
                        gate_scope(gate),
                        gate.not_green_reason().unwrap_or_default(),
                        gate_declaration_suffix(gate)
                    );
                }
                let _ = writeln!(
                    out,
                    "  Gates evaluated: those {}'s .arc/gates.toml declares, plus any {}'s adds",
                    state.target_branch, state.branch
                );
            }
            Blocker::AcceptanceProbesNotGreen => {
                for probe in report
                    .probes
                    .iter()
                    .filter(|probe| !probe.discriminating_at_head)
                {
                    if probe.undischargeable {
                        let _ = writeln!(
                            out,
                            "  - Probe `{}` cannot discharge: {}. Record a brief based on the \
                             revision the work started from",
                            probe.name,
                            undischargeable_reason(probe)
                        );
                        continue;
                    }
                    let _ = writeln!(
                        out,
                        "  - Probe `{}` needs Fail at `{}` and Pass at `{}`",
                        probe.name, probe.baseline_revision, probe.final_revision
                    );
                }
            }
            Blocker::HoldActive => {
                for hold in &report.holds {
                    let _ = writeln!(
                        out,
                        "  - hold `{}` by {}: {}",
                        hold.hold_event_id, hold.held_by, hold.reason
                    );
                }
            }
        }
        let _ = writeln!(out);
    }
    let notes = declaration_notes(report);
    if !notes.is_empty() {
        let _ = writeln!(out, "{notes}");
    }
    let _ = writeln!(out, "Next step: {}", next_step(report));
    out
}

/// `next_action` as its reader acts on it: a command with the change filled
/// in, or an instruction where no single command does it, followed by the
/// code in parentheses. Every human `Next step:` line is this; JSON carries
/// the code alone. A code without a rendering prints as itself.
pub fn next_step(report: &StatusReport) -> String {
    let code = report.next_action.as_str();
    let change = report.change_id.as_str();
    let review = format!("arc review {change} --verdict <verdict>");
    // An approval rejection is its own code: the policy's reason, which
    // already says what is wrong, so only the command follows it.
    if report.approval_rejection_reason.as_deref() == Some(code) {
        if report.verdict_contested {
            return format!("{code}: {review}");
        }
        return format!("{code}; ask a reviewer independent of the contributors to run: {review}");
    }
    let external = format!(
        "arc external verdict {change} --verdict <verdict> --decided-by <who> --reference <ref> --revision <revision>"
    );
    let step = match code.split_once(':').unwrap_or((code, "")) {
        ("none", "closed") => "nothing; the change is closed".to_string(),
        ("restore_branch", _) => match &report.latest_patchset {
            Some(patchset) => format!(
                "recreate branch {branch} at its newest patchset: git branch {branch} {}",
                patchset.head,
                branch = report.branch
            ),
            None => format!("recreate branch {}", report.branch),
        },
        ("restore_target", target) => format!(
            "restore target branch {target}; the change is judged by the declarations on it"
        ),
        ("repair_blockers", _) => {
            let withdrawals: String = dependencies_with_status(report, "wedged")
                .map(|dependency| format!(" --remove-blocked-by {dependency}"))
                .collect();
            format!(
                "withdraw the prerequisite closed without integrating, or name its replacement with --blocked-by: arc metadata {change}{withdrawals}"
            )
        }
        ("wait_for", _) => {
            let open = dependencies_with_status(report, "open").collect::<Vec<_>>();
            let waiting = if open.is_empty() {
                "the prerequisites".to_string()
            } else {
                open.join(", ")
            };
            format!("wait for {waiting} to integrate; arc blocker-status {change} reports them")
        }
        ("rebase", _) => format!("arc rebase {change}"),
        ("snapshot", _) => format!("arc snapshot {change}"),
        ("resolve_findings", _) => {
            let finding = report
                .open_blocking_findings
                .first()
                .map_or("<finding>", String::as_str);
            format!(
                "address blocking finding {finding}, then: arc resolve {change} {finding} --status resolved"
            )
        }
        ("release_hold", hold) => format!("arc release-hold {change} {hold}"),
        ("run_gate", gate) => format!("arc verify {change} --gate {gate}"),
        ("clean_worktree", gate) => format!(
            "commit or discard the worktree's uncommitted edits, then: arc verify {change} --gate {gate}"
        ),
        ("verify_against", target) => format!("arc verify {change} --against {target}"),
        ("run_probe", name) => match report.probes.iter().find(|probe| probe.name == name) {
            Some(probe) if probe.undischargeable => undischargeable_probe_step(report, probe),
            Some(probe) if probe.baseline_result != "fail" => format!(
                "with {} checked out in the change's worktree: arc verify {change} --probe {name} --probe-phase baseline",
                probe.baseline_revision
            ),
            _ => format!("arc verify {change} --probe {name}"),
        },
        ("declare_debt", _) => {
            format!("arc debt {change} --reason \"<what was read and what review is owed>\"")
        }
        ("iterating", "clear") => format!("arc iterating {change} --off"),
        ("external_changes_requested", _) => format!(
            "address the external reviewer's requested changes, then record their next decision: {external}"
        ),
        ("external_rejected", _) => format!(
            "revise what the external reviewer rejected, then record their next decision: {external}"
        ),
        ("comment-only", _) => format!(
            "address the comment-only verdict, record any new commits with arc snapshot {change}, then ask for a fresh verdict: {review}"
        ),
        ("changes-requested", _) => format!(
            "address the requested changes, record them with arc snapshot {change}, then ask for a fresh verdict: {review}"
        ),
        ("await_receiver", _) => {
            format!("send the change to its receiver, then record their decision: {external}")
        }
        ("integrate", _) => format!("arc integrate {change}"),
        ("request_review", _) => {
            format!("ask a reviewer independent of the contributors to run: {review}")
        }
        _ => return code.to_string(),
    };
    format!("{step} ({code})")
}

/// The recovery for a probe its patchset's brief cannot discharge. A patchset
/// binds the brief that is latest when it is recorded, so the brief version a
/// probe can fail at comes first and the snapshot that binds it second. Once
/// a later version exists, that snapshot is all that remains, so a recovery
/// stopped between the two resumes rather than recording another version.
fn undischargeable_probe_step(report: &StatusReport, probe: &crate::status::ProbeStatus) -> String {
    let change = report.change_id.as_str();
    let bound = probe.brief_version;
    let snapshot = format!(
        "arc snapshot {change} --contributors {}",
        patchset_contributors(report)
    );
    let later = report
        .brief
        .as_ref()
        .map(|brief| brief.version)
        .filter(|latest| *latest > bound);
    if let Some(latest) = later {
        return format!("snapshot to bind the patchset to brief v{latest}: {snapshot}");
    }
    format!(
        "as lead, record a brief based on the revision the work started from, redeclaring v{bound}'s probes, and snapshot to bind the patchset to it: arc brief {change} --body-file <file> --base <revision the work started from> --cause-note \"<why v{bound} could not discharge its probes>\" --probes-json {} && {snapshot}",
        redeclared_probes(report)
    )
}

/// The latest patchset's effective contributors as a `--contributors` value:
/// its recorded set when nonempty, otherwise its effective author. A snapshot
/// that rebinds the same head to a later brief records the same work, so it
/// declares the same hands, which is also what a live claim held by another
/// actor requires of it.
fn patchset_contributors(report: &StatusReport) -> String {
    let Some(patchset) = &report.latest_patchset else {
        return "<contributors>".to_string();
    };
    let contributors = if patchset.contributors.is_empty() {
        vec![patchset.effective_author()]
    } else {
        patchset.contributors.iter().map(String::as_str).collect()
    };
    shell_word(&contributors.join(","))
}

/// `value` as one shell word: bare when it holds nothing a shell interprets.
fn shell_word(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:@/+=,%".contains(c));
    if plain {
        value.to_string()
    } else {
        crate::context::shell_quote(value)
    }
}

/// The patchset's brief's probes as a shell-quoted `--probes-json` argument.
/// A brief version declares only the probes it is given, so a version that
/// replaces an undischargeable one restates every probe or drops it.
fn redeclared_probes(report: &StatusReport) -> String {
    let probes: Vec<crate::model::AcceptanceProbe> = report
        .probes
        .iter()
        .map(|probe| crate::model::AcceptanceProbe {
            name: probe.name.clone(),
            command: probe.command.clone(),
        })
        .collect();
    let json = serde_json::to_string(&probes).unwrap_or_default();
    crate::context::shell_quote(&json)
}

fn dependencies_with_status<'a>(
    report: &'a StatusReport,
    status: &'a str,
) -> impl Iterator<Item = &'a str> {
    report
        .blocker_status
        .blockers_ready
        .iter()
        .filter(move |dependency| dependency.status == status)
        .map(|dependency| dependency.change_id.as_str())
}

/// The required gates that do not answer for the head, and why each does not.
///
/// It reads the same gate statuses readiness is decided from, so what it lists
/// is what `check` refuses for rather than a second opinion about the same
/// evidence.
pub fn gates_owed(report: &StatusReport) -> String {
    let mut out = String::new();
    if report.gates.is_empty() {
        let _ = writeln!(
            out,
            "gates: no gates declared for profile {}; nothing was evaluated",
            report.profile
        );
        return out;
    }
    let owed: Vec<&GateStatus> = report
        .gates
        .iter()
        .filter(|gate| !gate.green_at_head)
        .collect();
    if owed.is_empty() {
        let _ = writeln!(out, "gates: every required gate is green at head");
        return out;
    }
    let _ = writeln!(out, "gates owed:");
    for gate in owed {
        let _ = writeln!(
            out,
            "  - `{}` at {}: {}{}",
            gate.name,
            gate_scope(gate),
            gate.not_green_reason().unwrap_or_default(),
            gate_declaration_suffix(gate)
        );
    }
    out
}

fn blocker_title(blocker: Blocker) -> &'static str {
    match blocker {
        Blocker::Closed => "change closed",
        Blocker::BranchMissing => "branch missing",
        Blocker::TargetUnreadable => "target declarations unreadable",
        Blocker::ForkBranch => "change sits on fork work",
        Blocker::Iterating => "change is iterating",
        Blocker::BlockedByChanges => "prerequisite changes unresolved",
        Blocker::NeedsRebase => "target branch conflicts with change",
        Blocker::MergedTreeUnevaluated => "merged tree has no gate evidence",
        Blocker::BlockingFindings => "open blocking findings",
        Blocker::NoValidApproval => "missing or stale approval",
        Blocker::GatesNotGreen => "required gates not green",
        Blocker::AcceptanceProbesNotGreen => "acceptance probes not discriminating",
        Blocker::HoldActive => "hold active",
    }
}

/// One coverage entry as `<reviewer>@<effort> [route <v>]`. A coordinate the
/// record does not carry is left out rather than filled in.
fn debt_coverage_label(coverage: &DebtCoverage) -> String {
    let mut label = coverage.reviewer.clone();
    if let Some(effort) = &coverage.effort {
        label.push('@');
        label.push_str(effort);
    }
    if let Some(route) = &coverage.route_version {
        let _ = write!(label, " [route {route}]");
    }
    label
}

/// One coverage entry with the model string it was cast under, kept whole.
fn debt_coverage_detail(coverage: &DebtCoverage) -> String {
    match coverage.model.as_deref() {
        Some(model) => format!("{} ({model})", debt_coverage_label(coverage)),
        None => format!("{} (model unrecorded)", debt_coverage_label(coverage)),
    }
}

/// One production identity as `<actor>@<effort>`, read the way every other
/// identity comparison reads one: the subject when a lead acted for somebody.
fn debt_identity_label(identity: &DebtIdentity) -> String {
    match &identity.effort {
        Some(effort) => format!("{}@{effort}", identity.effective_actor()),
        None => identity.effective_actor().to_string(),
    }
}

/// How the work was produced, as `planned by <planner> (brief v<n>),
/// implemented by <implementer>`. Unbriefed work names only its implementer.
fn debt_production_label(production: &DebtProduction) -> String {
    let mut parts = Vec::new();
    if let Some(planner) = &production.planner {
        let version = production
            .brief_version
            .map(|version| format!(" (brief v{version})"))
            .unwrap_or_default();
        parts.push(format!(
            "planned by {}{version}",
            debt_identity_label(planner)
        ));
    }
    parts.push(format!(
        "implemented by {}",
        debt_identity_label(&production.implementer)
    ));
    parts.join(", ")
}

/// One obligation as a line: what kind of deficit, who produced the work it
/// covers, and what review that work did have. A coordinate the record does
/// not carry is left out rather than filled in.
pub fn debt_line(
    missing: Option<DebtMissing>,
    production: Option<&DebtProduction>,
    coverage: Option<&[DebtCoverage]>,
) -> String {
    let mut line = format!(
        "debt: {}",
        missing.map(DebtMissing::as_str).unwrap_or("unversioned")
    );
    if let Some(production) = production {
        let _ = write!(line, "; {}", debt_production_label(production));
    }
    if let Some(coverage) = coverage.filter(|entries| !entries.is_empty()) {
        let _ = write!(
            line,
            "; coverage: {}",
            coverage
                .iter()
                .map(debt_coverage_label)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    line
}

/// One chronological log line for a ledger event:
/// `<ts>  <actor>@<harness> (<model>)  <event-type>  <summary>`, with the
/// parenthesized model omitted when no model was recorded.
pub fn event_line(event: &Event) -> String {
    let (kind, summary) = event_kind_summary(&event.payload);
    if summary.is_empty() {
        format!("{}  {kind}", event_prefix(event))
    } else {
        format!("{}  {kind}  {summary}", event_prefix(event))
    }
}

/// The `<ts>  <actor>@<harness> (<model>)` half of a log line, shared by
/// events and by the facts an event carries inside it. The model annotation is
/// present only when the event recorded one.
fn event_prefix(event: &Event) -> String {
    let ts = event.created_at.format("%Y-%m-%dT%H:%M:%SZ");
    let actor = match &event.on_behalf_of {
        Some(subject) => format!("{} (for {subject})", event.actor),
        None => event.actor.clone(),
    };
    let model = event
        .model
        .as_deref()
        .map(|model| format!(" ({model})"))
        .unwrap_or_default();
    let source = event
        .model_provenance
        .model_source
        .map(|source| format!(" source: {}", source.as_str()))
        .unwrap_or_default();
    let observation = event
        .model_provenance
        .model_observation
        .as_ref()
        .map(|obs| format!("; {}", obs.line()))
        .unwrap_or_default();
    let disagreement = event
        .model_provenance
        .model_disagreement
        .as_ref()
        .map(|mismatch| {
            format!(
                "; declared: {}, observed: {}",
                mismatch.declared, mismatch.observed
            )
        })
        .unwrap_or_default();
    let provenance = one_line(&format!("{source}{observation}{disagreement}"));
    format!(
        "{ts}  {actor}@{}{model}{provenance}",
        event.harness.as_deref().unwrap_or("-")
    )
}

/// Which patchset a finding predates, when it is not the one under review.
///
/// A blocking-findings count saturates by the second round and gates nothing
/// after that, so the useful distinction is between what was raised against
/// what is about to ship and what was carried in from an earlier round.
fn finding_era(finding: &crate::status::FindingSummary, report: &StatusReport) -> String {
    let Some(latest) = report.latest_patchset.as_ref() else {
        return String::new();
    };
    match finding.patchset_id.as_deref() {
        Some(filed_against) if filed_against == latest.id => String::new(),
        Some(filed_against) => format!(" (against {filed_against})"),
        // Filed before anything was snapshotted, so it predates every
        // patchset rather than answering the one under review.
        None => " (raised before the first patchset)".to_string(),
    }
}

/// Log lines for findings carried inside another event. A review batch files
/// findings as part of its verdict, so without these the same object renders
/// only when it was filed standalone — and the batch is the path review loops
/// use.
pub fn nested_finding_lines(event: &Event) -> Vec<String> {
    let (findings, kind) = match &event.payload {
        Payload::VerdictRecorded { findings, .. } => (findings, "finding-added"),
        Payload::AuditVerdictRecorded { findings, .. } => (findings, "audit-finding-added"),
        Payload::ExternalVerdictRecorded { findings, .. } => {
            return findings
                .iter()
                .map(|finding| {
                    format!(
                        "{}  external-finding-added  {} [{}{:?}] {}",
                        event_prefix(event),
                        finding.finding_id,
                        if finding.blocking { "blocking/" } else { "" },
                        finding.severity,
                        finding.summary
                    )
                })
                .collect();
        }
        _ => return Vec::new(),
    };
    // Each line carries the same prefix an event line does, because these are
    // the same facts recorded by the same actor at the same moment — only the
    // ledger packs them into one event.
    findings
        .iter()
        .map(|finding| {
            format!(
                "{}  {kind}  {} [{}{:?}] {}",
                event_prefix(event),
                finding.finding_id,
                if finding.blocking { "blocking/" } else { "" },
                finding.severity,
                finding.summary
            )
        })
        .collect()
}

/// Why a declared probe has no revision pair that could discharge it.
fn undischargeable_reason(probe: &crate::status::ProbeStatus) -> &'static str {
    if probe.baseline_revision.is_empty() {
        "its brief records no base revision, so there is nothing for it to fail at"
    } else {
        "its brief's base is the head under review, so no run produces both a Fail and a Pass"
    }
}

/// Flatten stored free text into one line, for surfaces whose structure a
/// stray newline would break: a log row, a Markdown bullet, a section list.
/// The ledger keeps the body verbatim; only the rendering is flattened.
pub(crate) fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The events a kept fact cites, as a suffix; empty when it cites none.
pub(crate) fn kept_citations(cites: &[String]) -> String {
    if cites.is_empty() {
        String::new()
    } else {
        format!(" (cites {})", cites.join(", "))
    }
}

/// Stable kebab event type plus a type-specific one-line summary.
pub(crate) fn event_kind_summary(payload: &Payload) -> (&'static str, String) {
    match payload {
        Payload::ChangeOpened { slug, title, .. } => ("change-opened", format!("{slug}: {title}")),
        Payload::ContextKept {
            kind, body, cites, ..
        } => (
            "context-kept",
            format!(
                "[{}] {}{}",
                kind.as_str(),
                one_line(body),
                kept_citations(cites)
            ),
        ),
        Payload::MetadataUpdated {
            add_blocked_by,
            remove_blocked_by,
            add_tags,
            remove_tags,
            assign,
            priority,
        } => {
            let mut parts = Vec::new();
            for blocker in add_blocked_by {
                parts.push(format!("+blocked-by {blocker}"));
            }
            for blocker in remove_blocked_by {
                parts.push(format!("-blocked-by {blocker}"));
            }
            for tag in add_tags {
                parts.push(format!("+{tag}"));
            }
            for tag in remove_tags {
                parts.push(format!("-{tag}"));
            }
            if let Some(assign) = assign {
                parts.push(if assign.is_empty() {
                    "unassign".into()
                } else {
                    format!("assign {assign}")
                });
            }
            if let Some(priority) = priority {
                parts.push(format!("priority {priority}"));
            }
            ("metadata-updated", parts.join(", "))
        }
        Payload::Message {
            severity, summary, ..
        } => ("message", format!("[{severity:?}] {summary}")),
        Payload::BriefRecorded { title, .. } => (
            "brief-recorded",
            title.clone().unwrap_or_else(|| "brief".into()),
        ),
        Payload::ChangelogRecorded {
            category,
            body,
            supersedes,
            no_entry_reason,
        } => {
            let recorded = match no_entry_reason {
                Some(reason) => format!("no entry: {}", first_line(reason)),
                None => format!("{}: {}", category, first_line(body)),
            };
            (
                "changelog-recorded",
                match supersedes {
                    Some(superseded) => format!("{recorded} (supersedes {superseded})"),
                    None => recorded,
                },
            )
        }
        Payload::PatchsetAdded {
            patchset_id,
            head,
            journal_refs,
            thread,
            ..
        } => {
            let mut summary = format!("{patchset_id} {}", short_sha(head));
            if !journal_refs.is_empty() {
                summary.push_str(&format!("; {} journal link(s)", journal_refs.len()));
                let vias = journal_refs
                    .iter()
                    .filter_map(|link| link.via.map(|via| via.as_str()))
                    .collect::<Vec<_>>();
                if !vias.is_empty() {
                    summary.push_str(&format!(" via {}", vias.join(", ")));
                }
            }
            if let Some(thread) = thread {
                summary.push_str(&format!("; thread {}:{}", thread.scheme, thread.id));
            }
            ("patchset-added", summary)
        }
        Payload::PatchsetAttributionAmended {
            patchset_id,
            contributors,
        } => (
            "patchset-attribution-amended",
            format!("{patchset_id}: {}", contributors.join(", ")),
        ),
        Payload::ClaimSet {
            claim_id,
            displaced,
            ..
        } => (
            "claim-set",
            match displaced {
                Some(displaced) => match &displaced.reason {
                    Some(reason) => format!(
                        "{claim_id} displaced {} before its budget: {reason}",
                        displaced.claim_id
                    ),
                    None => format!("{claim_id} displaced {}", displaced.claim_id),
                },
                None => claim_id.clone(),
            },
        ),
        Payload::ClaimReleased { claim_id } => ("claim-released", claim_id.clone()),
        Payload::StageSet { stage, note, .. } => {
            let stage = format!("{stage:?}").to_lowercase();
            match note {
                Some(note) => ("stage-set", format!("{stage} — {note}")),
                None => ("stage-set", stage),
            }
        }
        Payload::CommentAdded { body, .. } => ("comment-added", first_line(body)),
        Payload::FindingAdded {
            finding_id,
            severity,
            summary,
            ..
        } => (
            "finding-added",
            format!("{finding_id} [{severity:?}] {summary}"),
        ),
        Payload::ReplyAdded { body, .. } => ("reply-added", first_line(body)),
        Payload::DispositionRecorded {
            finding_id,
            status,
            evidence_event_id,
            ..
        } => (
            "disposition-recorded",
            disposition_summary(finding_id, status, evidence_event_id.as_deref()),
        ),
        Payload::DirtyTreeWaived { reason, revision } => (
            "dirty-tree-waived",
            format!(
                "{}: {}",
                &revision[..revision.len().min(8)],
                one_line(reason)
            ),
        ),
        Payload::AuditDebtDeclared {
            reason,
            patchset_id,
        } => (
            "audit-debt-declared",
            match patchset_id {
                Some(id) => format!("{id}: {reason}"),
                None => reason.clone(),
            },
        ),
        Payload::DebtDeclared {
            reason,
            patchset_id,
            missing,
            coverage,
            production,
        } => (
            "debt-declared",
            format!(
                "{}; missing {}; coverage: {}{}",
                match patchset_id {
                    Some(id) => format!("{id}: {reason}"),
                    None => reason.clone(),
                },
                missing.as_str(),
                if coverage.is_empty() {
                    "none".to_string()
                } else {
                    coverage
                        .iter()
                        .map(debt_coverage_detail)
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                production
                    .as_ref()
                    .map(|production| format!("; {}", debt_production_label(production)))
                    .unwrap_or_default()
            ),
        ),
        Payload::AuditVerdictRecorded {
            revision,
            verdict,
            body,
            ..
        } => {
            let mut summary = format!(
                "{} at {}",
                format!("{verdict:?}").to_lowercase(),
                &revision[..revision.len().min(8)]
            );
            if let Some(body) = body {
                summary.push_str(" — ");
                summary.push_str(body.lines().next().unwrap_or_default());
            }
            ("audit-verdict-recorded", summary)
        }
        Payload::AuditFindingAdded {
            finding_id,
            severity,
            summary,
            ..
        } => (
            "audit-finding-added",
            format!("{finding_id} [{severity:?}] {summary}"),
        ),
        Payload::AuditDispositionRecorded {
            finding_id,
            status,
            evidence_event_id,
            ..
        } => (
            "audit-disposition-recorded",
            disposition_summary(finding_id, status, evidence_event_id.as_deref()),
        ),
        Payload::PostIntegrationDispositionRecorded {
            finding_id,
            status,
            evidence_event_id,
            ..
        } => (
            "post-integration-disposition-recorded",
            disposition_summary(finding_id, status, evidence_event_id.as_deref()),
        ),
        Payload::VerdictRecorded {
            patchset_id,
            verdict,
            body,
            ..
        } => {
            let mut summary = format!("{} {patchset_id}", format!("{verdict:?}").to_lowercase());
            if let Some(body) = body {
                summary.push_str(" — ");
                summary.push_str(&first_line(body));
            }
            ("verdict-recorded", summary)
        }
        Payload::ReadyToSend {
            patchset_id,
            head,
            history,
            ..
        } => (
            "ready-to-send",
            format!(
                "{patchset_id} at {} ({history} history); nothing merged",
                short_sha(head)
            ),
        ),
        Payload::ExternalVerdictRecorded {
            revision,
            verdict,
            decided_by,
            reference,
            ..
        } => (
            "external-verdict-recorded",
            format!(
                "[external] {} at {} by {} ({})",
                format!("{verdict:?}").to_lowercase(),
                short_sha(revision),
                decided_by,
                reference
            ),
        ),
        Payload::VerificationRunStarted {
            mode,
            gates,
            skip_green,
            ..
        } => (
            "verification-run-started",
            format!("{mode:?} {} gate(s), skip-green={skip_green}", gates.len()),
        ),
        Payload::VerificationRecorded {
            gate,
            probe,
            result,
            ..
        } => (
            "verification-recorded",
            format!(
                "{} {}",
                probe
                    .as_ref()
                    .map(|probe| format!("probe:{}:{:?}", probe.name, probe.phase))
                    .or_else(|| gate.clone())
                    .unwrap_or_else(|| "-".into()),
                format!("{result:?}").to_lowercase()
            ),
        ),
        Payload::VerificationReused {
            gate,
            evidence_event_id,
            ..
        } => (
            "verification-reused",
            format!("{gate} reused {evidence_event_id}"),
        ),
        Payload::HoldSet { reason } => ("hold-set", reason.clone()),
        Payload::HoldReleased {
            hold_event_id,
            reason,
        } => (
            "hold-released",
            match (hold_event_id, reason) {
                (Some(id), Some(reason)) => format!("{id}: {reason}"),
                (Some(id), None) => id.clone(),
                (None, reason) => reason.clone().unwrap_or_default(),
            },
        ),
        Payload::ChangeIntegrated {
            integrated_commit,
            target_branch,
            already_contained,
            authorization,
            ..
        } => {
            let external = authorization
                .as_ref()
                .and_then(|authorization| authorization.external_verdict.as_ref())
                .map(|external| format!(" [external: {}]", external.reference))
                .unwrap_or_default();
            (
                "change-integrated",
                if *already_contained {
                    format!(
                        "{} into {target_branch} (already contained; no merge created){external}",
                        short_sha(integrated_commit)
                    )
                } else {
                    format!(
                        "{} into {target_branch}{external}",
                        short_sha(integrated_commit)
                    )
                },
            )
        }
        Payload::IntegrationAsserted {
            integrated_commit,
            target_branch,
            external_reference,
            ..
        } => {
            let external = external_reference
                .as_deref()
                .map(|reference| format!(" [external: {reference}]"))
                .unwrap_or_default();
            (
                "integration-asserted",
                format!(
                    "{} into {target_branch}{external}",
                    short_sha(integrated_commit)
                ),
            )
        }
        Payload::IterationScopeSet { iterating } => {
            ("iteration-scope-set", format!("iterating: {iterating}"))
        }
        Payload::HistoryRewritten {
            mapping, reason, ..
        } => (
            "history-rewritten",
            format!("{} revisions: {reason}", mapping.len()),
        ),
        Payload::HistoryRewriteWithdrawn {
            rewrite_event_id,
            reason,
        } => (
            "history-rewrite-withdrawn",
            format!("{rewrite_event_id}: {}", one_line(reason)),
        ),
        Payload::ReviewPassOpened {
            pass_id, members, ..
        } => (
            "review-pass-opened",
            format!("{pass_id}: {} members", members.len()),
        ),
        Payload::ReviewPassCompleted { pass_id, .. } => ("review-pass-completed", pass_id.clone()),
        Payload::ReviewPassAbandoned { pass_id, reason } => (
            "review-pass-abandoned",
            format!("{pass_id}: {}", one_line(reason)),
        ),
        Payload::RunDispatched {
            route,
            worktree,
            change,
            fork,
            range,
            note,
            ..
        } => {
            let mut summary = format!("route={} worktree={}", one_line(route), one_line(worktree));
            if let Some(change) = change {
                summary.push_str(&format!(" change={}", one_line(change)));
            }
            if let Some(fork) = fork {
                summary.push_str(&format!(" fork={}", one_line(fork)));
            }
            if let Some(range) = range {
                summary.push_str(&format!(" range={}", one_line(&range.as_str())));
            }
            if let Some(note) = note {
                summary.push_str(" — ");
                summary.push_str(&one_line(note));
            }
            ("run-dispatched", summary)
        }
        Payload::RunEnded {
            dispatch_event_id,
            outcome,
            reviewed_head,
            raised,
            deferred,
            collects,
            note,
        } => {
            let mut summary = format!("{} for {dispatch_event_id}", outcome.as_str());
            if let Some(head) = reviewed_head {
                summary.push_str(&format!(" reviewed={}", short_sha(head)));
            }
            for (label, count) in [
                ("raised", raised.len()),
                ("deferred", deferred.len()),
                ("collects", collects.len()),
            ] {
                if count > 0 {
                    summary.push_str(&format!(" {label}={count}"));
                }
            }
            if let Some(note) = note {
                summary.push_str(" — ");
                summary.push_str(&one_line(note));
            }
            ("run-ended", summary)
        }
        Payload::CandidateRegistered {
            candidate_id,
            tree,
            brief,
            ..
        } => (
            "candidate-registered",
            format!(
                "{candidate_id}: {} for {}",
                short_sha(tree),
                brief.change_id
            ),
        ),
        Payload::CandidateJudged {
            candidate_id,
            judgement,
            reason,
        } => {
            let judged = match judgement {
                crate::model::CandidateJudgement::Rejected => "rejected".to_string(),
                crate::model::CandidateJudgement::SupersededBy { candidate_id } => {
                    format!("superseded by {candidate_id}")
                }
            };
            (
                "candidate-judged",
                format!("{candidate_id} {judged}: {}", one_line(reason)),
            )
        }
        Payload::CandidateRetired { candidate_id } => ("candidate-retired", candidate_id.clone()),
        Payload::CandidateVerified {
            candidate_id,
            gate,
            result,
            ..
        } => (
            "candidate-verified",
            format!("{candidate_id} gate {gate}: {result:?}"),
        ),
        Payload::CandidateSelected {
            candidate_id,
            destination: change_id,
            ..
        } => (
            "candidate-selected",
            format!("{candidate_id} into {change_id}"),
        ),
        Payload::CandidatePromoted {
            candidate_id,
            destination: change_id,
            patchset_id,
            ..
        } => (
            "candidate-promoted",
            format!("{candidate_id} into {change_id} as {patchset_id}"),
        ),
        Payload::ContextRead {
            subject,
            record,
            path,
            coverage,
            ..
        } => (
            "context-read",
            format!("{} read {path} ({coverage}) as {record}", subject.label()),
        ),
        Payload::ContextDeclared {
            subject,
            relation,
            target,
            ..
        } => (
            "context-declared",
            format!("{} {} {target}", subject.label(), relation.as_str()),
        ),
        Payload::ContextCaptureReported {
            subject,
            record,
            capture,
        } => (
            "context-capture-reported",
            format!("{record} on {}: {}", subject.label(), capture.as_str()),
        ),
        Payload::ChangeClosed {
            outcome,
            integrated_commit,
            ..
        } => {
            let outcome = format!("{outcome:?}").to_lowercase();
            match integrated_commit {
                Some(commit) => (
                    "change-closed",
                    format!("{outcome} at {}", short_sha(commit)),
                ),
                None => ("change-closed", outcome),
            }
        }
        Payload::ForgeProjection { base_repo, .. } => ("forge-projection", base_repo.clone()),
        Payload::ForgeLink { pr_number, .. } => ("forge-link", format!("#{pr_number}")),
        Payload::ForgeChecks { state, .. } => ("forge-checks", format!("{state:?}").to_lowercase()),
        Payload::ForgePrState { state, .. } => {
            ("forge-pr-state", format!("{state:?}").to_lowercase())
        }
        Payload::Unknown => ("unknown", String::new()),
    }
}

fn disposition_summary(
    finding_id: &str,
    status: &crate::model::DispositionStatus,
    evidence_event_id: Option<&str>,
) -> String {
    let mut summary = format!("{finding_id} {}", format!("{status:?}").to_lowercase());
    if let Some(evidence_event_id) = evidence_event_id {
        summary.push_str(&format!(" (evidence event {evidence_event_id})"));
    }
    summary
}

pub(crate) fn short_sha(sha: &str) -> String {
    sha.chars().take(12).collect()
}

fn first_line(body: &str) -> String {
    body.lines().next().unwrap_or_default().to_string()
}

/// Full integration-readiness checklist: every gate condition, passing or
/// failing, in exit-code precedence order. Unlike `blocker_explanation`
/// (which lists only blockers), this renders the complete evaluation so a
/// reviewer sees what already passes alongside what does not.
pub fn check_explanation(state: &ChangeState, report: &StatusReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Integration readiness for {}", state.change_id);
    if let Some(line) = merged_tree_line(report) {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out);

    let blocked = |blocker: Blocker| report.blockers().contains(&blocker);
    let condition = |out: &mut String, blocker: Blocker, label: &str, detail: String| {
        if blocked(blocker) {
            let _ = writeln!(out, "  [ ] {label}");
            if !detail.is_empty() {
                for line in detail.lines() {
                    let _ = writeln!(out, "        {line}");
                }
            }
        } else {
            let _ = writeln!(out, "  [x] {label}");
        }
    };

    condition(
        &mut out,
        Blocker::BranchMissing,
        "branch present",
        format!("branch `{}` is missing", state.branch),
    );
    condition(
        &mut out,
        Blocker::TargetUnreadable,
        "target declarations readable",
        format!(
            "target `{}` cannot be resolved, so the declarations the change is judged by \
             could not be read",
            state.target_branch
        ),
    );
    condition(
        &mut out,
        Blocker::Iterating,
        "integration scope is cleared",
        format!(
            "change declares it is iterating; clear it with `arc iterating {} --off`",
            state.change_id
        ),
    );
    condition(
        &mut out,
        Blocker::BlockedByChanges,
        "prerequisites integrated",
        report
            .blocker_status
            .blockers_ready
            .iter()
            .filter(|dependency| !dependency.integrated)
            .map(|dependency| {
                format!(
                    "{} (`{}`): {}",
                    dependency.slug, dependency.change_id, dependency.status
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    condition(
        &mut out,
        Blocker::NeedsRebase,
        "rebased on target",
        format!(
            "target `{}` moved with conflicting changes",
            state.target_branch
        ),
    );
    condition(
        &mut out,
        Blocker::MergedTreeUnevaluated,
        "merged tree evaluated",
        format!(
            "no required gate has run against the tree merging into `{}` would ship; \
             run `arc verify --against {}`",
            state.target_branch, state.target_branch
        ),
    );
    condition(
        &mut out,
        Blocker::BlockingFindings,
        "no open blocking findings",
        report
            .findings
            .iter()
            .filter(|finding| report.open_blocking_findings.contains(&finding.id))
            .map(|finding| {
                format!(
                    "`{}` [{:?}] {}{}",
                    finding.id,
                    finding.severity,
                    finding.summary,
                    finding_era(finding, report)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    condition(
        &mut out,
        Blocker::NoValidApproval,
        "valid approval at head",
        report
            .approval_rejection_reason
            .clone()
            .unwrap_or_else(|| "current head has no valid approval".into()),
    );
    // No declared gate is a satisfied condition nothing was checked
    // against, and a reader must not take the tick for a pass.
    let gates_label = if report.gates.is_empty() {
        format!("no gates declared for profile {}", state.profile)
    } else {
        "required gates green".to_string()
    };
    condition(
        &mut out,
        Blocker::GatesNotGreen,
        &gates_label,
        report
            .gates
            .iter()
            .filter(|gate| !gate.green_at_head)
            .map(|gate| {
                format!(
                    "gate `{}` is not green at {}: {}{}",
                    gate.name,
                    gate_scope(gate),
                    gate.not_green_reason().unwrap_or_default(),
                    gate_declaration_suffix(gate)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    // A green gate answers whether it passes, not whether it could have
    // failed, nor which run said so. Neither question has a blocker or an exit
    // code, so the checklist is the only place a reader meets them.
    for gate in &report.gates {
        let suffix = format!(
            "{}{}",
            inheritance_suffix(gate),
            discrimination_suffix(gate)
        );
        if !suffix.is_empty() {
            let _ = writeln!(out, "        gate `{}`:{suffix}", gate.name);
        }
    }
    condition(
        &mut out,
        Blocker::AcceptanceProbesNotGreen,
        "declared acceptance probes discriminate",
        report
            .probes
            .iter()
            .filter(|probe| !probe.discriminating_at_head)
            .map(|probe| {
                if probe.undischargeable {
                    return format!(
                        "probe `{}` cannot discharge: {}",
                        probe.name,
                        undischargeable_reason(probe)
                    );
                }
                format!(
                    "probe `{}` needs fail at `{}` and pass at `{}`",
                    probe.name, probe.baseline_revision, probe.final_revision
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    condition(
        &mut out,
        Blocker::HoldActive,
        "no active hold",
        report
            .holds
            .iter()
            .map(|hold| format!("hold `{}`: {}", hold.hold_event_id, hold.reason))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let notes = declaration_notes(report);
    if !notes.is_empty() {
        let _ = writeln!(out);
        out.push_str(&notes);
    }
    let _ = writeln!(out);
    if report.integrate_ready() {
        let _ = writeln!(out, "Ready to integrate (exit 0)");
    } else {
        let code = crate::status::check_exit_code(report);
        let first = report
            .blockers()
            .first()
            .map(|blocker| blocker.as_str())
            .unwrap_or("blocked");
        let _ = writeln!(out, "Exit code: {code} ({first})");
    }
    out
}
