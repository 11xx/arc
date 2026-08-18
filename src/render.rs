use crate::commands::ArcAlternative;
use crate::model::{Event, Payload};
use crate::state::ChangeState;
use crate::status::{Blocker, GateStatus, StatusReport};
use std::fmt::Write;

/// Why a passing *gate* verification cannot be reused as evidence at its
/// revision. `None` when it can, when it did not pass — a failure explains
/// itself — or when it is not gate evidence: probe readiness is a different
/// question, answered by the probe's own baseline and final results.
fn unusable_gate_evidence_reason(entry: &crate::state::VerificationEntry) -> Option<&'static str> {
    if entry.gate.is_none()
        || entry.probe.is_some()
        || entry.green_at_head()
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

/// One gate's line in a human-facing gate list: the raw result, plus why a
/// passing result still does not count at head.
pub fn gate_line(gate: &GateStatus) -> String {
    match gate.not_green_reason() {
        None => gate.result.clone(),
        Some(reason) => format!("{} (not green at head: {reason})", gate.result),
    }
}

/// The authorization basis, as a human-readable block. Used by
/// `integrate --dry-run` to show what the merge would be recorded as resting
/// on, before anything is written.
pub fn authorization_basis(basis: &crate::model::AuthorizationBasis) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "    verdict: {}", basis.verdict_event_id);
    for (gate, evidence) in &basis.gate_evidence {
        let _ = writeln!(out, "    gate {gate}: {evidence}");
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
            "    gate declaration {name}: {}{}{}",
            gate.command,
            gate.timeout
                .map(|timeout| format!(" (timeout {timeout}s)"))
                .unwrap_or_default(),
            if gate.profiles.is_empty() {
                String::new()
            } else {
                format!(" (profiles: {})", gate.profiles.join(", "))
            }
        );
    }
    if let Some(debt) = &basis.audit_debt_event_id {
        let _ = writeln!(out, "    audit debt waiving review: {debt}");
    }
    let _ = write!(
        out,
        "    policy: forbid_self_approval={}, require_declared_actor={}, git_identity={}",
        basis.policy.forbid_self_approval,
        basis.policy.require_declared_actor,
        basis.policy.provenance_git_identity
    );
    out
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
    let _ = writeln!(
        w,
        "- Branch: `{}` → `{}`",
        state.branch, state.target_branch
    );
    let _ = writeln!(w, "- Base: `{}`", state.base);
    if let Some(wt) = &state.worktree {
        let _ = writeln!(w, "- Worktree: `{wt}`");
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
    }

    if !state.patchsets.is_empty() {
        let _ = writeln!(w, "\n## Patchsets\n");
        for p in &state.patchsets {
            let _ = writeln!(w, "- `{}`: `{}` → `{}`", p.id, p.base, p.head);
            if let (Some(brief_ref), Some(version)) = (&p.brief_ref, p.brief_version) {
                let _ = writeln!(w, "  - brief: v{version} (`{}`)", brief_ref.event_id);
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
            if let Some(actor) = &p.claim_actor {
                let _ = writeln!(
                    w,
                    "  - claim actor at snapshot: {}{}",
                    actor,
                    if report.provenance_check_enabled && p.provenance_mismatch == Some(true) {
                        " — **PROVENANCE MISMATCH**; use `--on-behalf-of` for a delegated snapshot or set `[provenance] git_identity = \"shared\"` when the project uses one committing identity"
                    } else {
                        ""
                    }
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
            let _ = writeln!(w, "- Base revision: `{base_revision}`");
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
        if let Some(body) = &v.body {
            let _ = writeln!(w, "\n{body}");
        }
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
                "- {}: `{}` — {}{}",
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
                }
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
            if let Some(reason) = unusable_gate_evidence_reason(v) {
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
            let attribution = if row.attribution_unknown {
                " — attribution unknown, no `--on-behalf-of` recorded"
            } else if row.is_author {
                " — same identity as the patchset author"
            } else {
                ""
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

    if report.audit_debt.is_some() || !report.audit_verdicts.is_empty() {
        let _ = writeln!(w, "\n## Post-integration audit\n");
        if let Some(debt) = &report.audit_debt {
            let _ = writeln!(
                w,
                "- Owed{}: {} (declared by {})",
                if report.audit_debt_outstanding {
                    ""
                } else {
                    " (discharged)"
                },
                debt.reason,
                debt.actor
            );
        }
        for audit in &report.audit_verdicts {
            let _ = writeln!(
                w,
                "- {:?} at `{}` by {}{}",
                audit.verdict,
                &audit.revision[..audit.revision.len().min(8)],
                audit.effective_author(),
                audit
                    .body
                    .as_deref()
                    .map(|body| format!(" — {}", body.lines().next().unwrap_or_default()))
                    .unwrap_or_default()
            );
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

/// Advisory review-coverage lines, printed after the ready/blocked verdict.
///
/// These are advisories by design. Blocking on thin coverage would refuse the
/// single-reviewer changes that make up most of the work, and an
/// orchestrator's review is a valid review unless a project's policy says
/// otherwise; the point is that nobody integrates without having been told.
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
pub fn blocker_explanation(state: &ChangeState, report: &StatusReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Cannot integrate {}", state.change_id);
    let _ = writeln!(out);

    for (index, blocker) in report.blockers.iter().enumerate() {
        let _ = writeln!(out, "Blocker {}: {}", index + 1, blocker_title(*blocker));
        match blocker {
            Blocker::Closed => {
                let _ = writeln!(out, "  - Change is already closed");
            }
            Blocker::BranchMissing => {
                let _ = writeln!(out, "  - Branch `{}` is missing", state.branch);
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
                    "  - needs rebase: target {} moved with conflicting changes; rebase, rerun gates, re-review",
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
                let _ = writeln!(
                    out,
                    "  - {}",
                    report
                        .approval_rejection_reason
                        .as_deref()
                        .unwrap_or("Current head has no valid approval")
                );
            }
            Blocker::GatesNotGreen => {
                for gate in report.gates.iter().filter(|gate| !gate.green_at_head) {
                    let _ = writeln!(
                        out,
                        "  - Gate `{}` is not green at head: {}",
                        gate.name,
                        gate.not_green_reason().unwrap_or_default()
                    );
                }
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
    let _ = writeln!(out, "Next step: {}", report.next_action);
    out
}

fn blocker_title(blocker: Blocker) -> &'static str {
    match blocker {
        Blocker::Closed => "change closed",
        Blocker::BranchMissing => "branch missing",
        Blocker::BlockedByChanges => "prerequisite changes unresolved",
        Blocker::NeedsRebase => "target branch conflicts with change",
        Blocker::BlockingFindings => "open blocking findings",
        Blocker::NoValidApproval => "missing or stale approval",
        Blocker::GatesNotGreen => "required gates not green",
        Blocker::AcceptanceProbesNotGreen => "acceptance probes not discriminating",
        Blocker::HoldActive => "hold active",
    }
}

/// One chronological log line for a ledger event:
/// `<ts>  <actor>@<harness>  <event-type>  <summary>`.
pub fn event_line(event: &Event) -> String {
    let (kind, summary) = event_kind_summary(&event.payload);
    if summary.is_empty() {
        format!("{}  {kind}", event_prefix(event))
    } else {
        format!("{}  {kind}  {summary}", event_prefix(event))
    }
}

/// The `<ts>  <actor>@<harness>` half of a log line, shared by events and by
/// the facts an event carries inside it.
fn event_prefix(event: &Event) -> String {
    let ts = event.created_at.format("%Y-%m-%dT%H:%M:%SZ");
    let actor = match &event.on_behalf_of {
        Some(subject) => format!("{} (for {subject})", event.actor),
        None => event.actor.clone(),
    };
    format!("{ts}  {actor}@{}", event.harness.as_deref().unwrap_or("-"))
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

/// Stable kebab event type plus a type-specific one-line summary.
fn event_kind_summary(payload: &Payload) -> (&'static str, String) {
    match payload {
        Payload::ChangeOpened { slug, title, .. } => ("change-opened", format!("{slug}: {title}")),
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
        Payload::ChangelogRecorded { category, body } => (
            "changelog-recorded",
            format!("{}: {}", category, first_line(body)),
        ),
        Payload::PatchsetAdded {
            patchset_id, head, ..
        } => (
            "patchset-added",
            format!("{patchset_id} {}", short_sha(head)),
        ),
        Payload::ClaimSet {
            claim_id,
            displaced,
            ..
        } => (
            "claim-set",
            match displaced {
                Some(displaced) => format!("{claim_id} displaced {}", displaced.claim_id),
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
            finding_id, status, ..
        } => (
            "disposition-recorded",
            format!("{finding_id} {}", format!("{status:?}").to_lowercase()),
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
            finding_id, status, ..
        } => (
            "audit-disposition-recorded",
            format!("{finding_id} {}", format!("{status:?}").to_lowercase()),
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
            ..
        } => (
            "change-integrated",
            format!("{} into {target_branch}", short_sha(integrated_commit)),
        ),
        Payload::IntegrationAsserted {
            integrated_commit,
            target_branch,
            ..
        } => (
            "integration-asserted",
            format!("{} into {target_branch}", short_sha(integrated_commit)),
        ),
        Payload::HistoryRewritten {
            mapping, reason, ..
        } => (
            "history-rewritten",
            format!("{} revisions: {reason}", mapping.len()),
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

fn short_sha(sha: &str) -> String {
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
    let _ = writeln!(out);

    let blocked = |blocker: Blocker| report.blockers.contains(&blocker);
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
    condition(
        &mut out,
        Blocker::GatesNotGreen,
        "required gates green",
        report
            .gates
            .iter()
            .filter(|gate| !gate.green_at_head)
            .map(|gate| {
                format!(
                    "gate `{}` is not green at head: {}",
                    gate.name,
                    gate.not_green_reason().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
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

    let _ = writeln!(out);
    if report.integrate_ready {
        let _ = writeln!(out, "Ready to integrate (exit 0)");
    } else {
        let code = crate::status::check_exit_code(report);
        let first = report
            .blockers
            .first()
            .map(|blocker| blocker.as_str())
            .unwrap_or("blocked");
        let _ = writeln!(out, "Exit code: {code} ({first})");
    }
    out
}
