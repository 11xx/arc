use crate::commands::ArcAlternative;
use crate::model::{Event, Payload};
use crate::state::ChangeState;
use crate::status::{Blocker, StatusReport};
use std::fmt::Write;

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
                    if p.provenance_mismatch == Some(true) {
                        " — **PROVENANCE MISMATCH**"
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

    if !state.verifications.is_empty() {
        let _ = writeln!(w, "\n## Verifications\n");
        for v in &state.verifications {
            let _ = writeln!(
                w,
                "- {} `{}` at `{}` → {:?}{} (on {})",
                v.gate.as_deref().unwrap_or("(ad hoc)"),
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
                        "  - `{}` [{:?}] {}",
                        finding.id, finding.severity, finding.summary
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
                    let _ = writeln!(out, "  - Gate `{}` is not green at head", gate.name);
                }
            }
            Blocker::HoldActive => {
                let _ = writeln!(
                    out,
                    "  - {}",
                    report.hold.as_deref().unwrap_or("hold active")
                );
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
        Blocker::HoldActive => "hold active",
    }
}

/// One chronological log line for a ledger event:
/// `<ts>  <actor>@<harness>  <event-type>  <summary>`.
pub fn event_line(event: &Event) -> String {
    let ts = event.created_at.format("%Y-%m-%dT%H:%M:%SZ");
    let actor = match &event.on_behalf_of {
        Some(subject) => format!("{} (for {subject})", event.actor),
        None => event.actor.clone(),
    };
    let who = format!("{actor}@{}", event.harness.as_deref().unwrap_or("-"));
    let (kind, summary) = event_kind_summary(&event.payload);
    if summary.is_empty() {
        format!("{ts}  {who}  {kind}")
    } else {
        format!("{ts}  {who}  {kind}  {summary}")
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
        Payload::PatchsetAdded {
            patchset_id, head, ..
        } => (
            "patchset-added",
            format!("{patchset_id} {}", short_sha(head)),
        ),
        Payload::ClaimSet { claim_id, .. } => ("claim-set", claim_id.clone()),
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
        Payload::VerdictRecorded {
            patchset_id,
            verdict,
            ..
        } => (
            "verdict-recorded",
            format!("{} {patchset_id}", format!("{verdict:?}").to_lowercase()),
        ),
        Payload::VerificationRecorded { gate, result, .. } => (
            "verification-recorded",
            format!(
                "{} {}",
                gate.as_deref().unwrap_or("-"),
                format!("{result:?}").to_lowercase()
            ),
        ),
        Payload::HoldSet { reason } => ("hold-set", reason.clone()),
        Payload::HoldReleased { reason } => ("hold-released", reason.clone().unwrap_or_default()),
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
                    "`{}` [{:?}] {}",
                    finding.id, finding.severity, finding.summary
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
            .map(|gate| format!("gate `{}` is not green at head", gate.name))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    condition(
        &mut out,
        Blocker::HoldActive,
        "no active hold",
        report.hold.clone().unwrap_or_default(),
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
