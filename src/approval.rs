//! Whether a change's approval counts, as one pure function over facts the
//! status report has already read.
//!
//! [`decide`] reads no Git, no store, and no clock. Everything it needs is in
//! [`ApprovalFacts`], so the same facts always yield the same validity, the
//! same rejection reason, and the same advisories, and a reader is told the
//! reason the gate acted on rather than a second reading of the ledger.

use crate::model::{DangerRule, ExternalVerdict, Verdict};
use crate::state::{ExternalVerdictEntry, Patchset, VerdictEntry};
use crate::status::{
    Advisory, DangerScope, ExternalVerdictStatus, ReviewerCoverage, VerdictStatus,
};

pub const SELF_APPROVAL_REASON: &str = "approval rejected by policy: self-approval";
/// A verdict graph with several tips has no authority to report, so the
/// change reads as unreviewed unless the blocker says which it is: nobody
/// reviewed, or several reviewers each replaced the same verdict.
pub const CONTESTED_VERDICT_REASON: &str =
    "verdicts replace the same earlier verdict, so none is authoritative; record a verdict \
     superseding all of them";
/// An identity arc assumed, from `git config user.name` or the harness
/// session, is nobody's claim, so a verdict recorded under one cannot be the
/// second party independence needs. The reviewer is the identity this
/// refuses, because it is the one the caller can still declare.
pub const UNDECLARED_APPROVAL_REASON: &str =
    "approval rejected by policy: arc assumed the reviewing identity rather than anyone \
     declaring it, so independence is unproven (pass --actor or set ARC_ACTOR)";

/// The facts an approval's validity follows from.
pub struct ApprovalFacts<'a> {
    /// The newest patchset: the one an approval must name to count.
    pub latest_patchset: Option<&'a Patchset>,
    /// The branch head, when the branch resolves.
    pub current_head: Option<&'a str>,
    /// The branch head is the newest patchset's head.
    pub head_matches: bool,
    /// The branch head is the commit the newest patchset's approval stands
    /// on: its recorded head followed only through signature-only rewrites.
    pub approved_head_matches: bool,
    /// The sole authoritative local verdict, when the verdict graph has one
    /// tip.
    pub latest_verdict: Option<&'a VerdictEntry>,
    /// How many tips the local verdict graph has.
    pub verdict_tips: usize,
    /// Every external verdict, oldest first.
    pub external_verdicts: &'a [ExternalVerdictEntry],
    /// The declared debt names the newest patchset.
    pub debt_waives_latest_patchset: bool,
    pub forbid_self_approval: bool,
    /// Where `forbid_self_approval = true` was declared, joined for a reader.
    pub forbid_self_approval_sources: String,
    pub danger: &'a DangerScope,
    /// Each reviewer's coverage of the change.
    pub review_map: &'a [ReviewerCoverage],
    /// Why the approval covering the newest patchset is owed corroboration,
    /// while it is.
    pub provisional_approval_reason: Option<&'a str>,
    /// The brief's author, when every verdict on the newest patchset came
    /// from it.
    pub reviewed_only_by_brief_author: Option<&'a str>,
    /// A review obligation recorded at integration is not discharged.
    pub debt_outstanding: bool,
}

/// What the facts decide about the change's approval.
pub struct ApprovalOutcome {
    /// The local verdict, judged against the current head.
    pub verdict: Option<VerdictStatus>,
    /// Every external verdict, newest first, judged against the current head.
    pub external_verdicts: Vec<ExternalVerdictStatus>,
    /// The newest external verdict naming the current head, when the head is
    /// the newest patchset's.
    pub current_external_verdict: Option<ExternalVerdict>,
    /// A changes-requested or comment-only local verdict on the newest
    /// patchset.
    pub local_verdict_refuses_this_head: bool,
    /// A non-approving external verdict on the current head.
    pub external_refuses_this_head: bool,
    /// Some verdict approves the current head under policy.
    pub approval_valid: bool,
    /// The declared debt stands in for a verdict nobody recorded, or one only
    /// the self-approval rule rejects.
    pub waiver_satisfies_approval: bool,
    /// The waiver is load-bearing: it satisfies the gate, and it rescued a
    /// self-approval or stood in for a verdict that was never recorded.
    pub approval_waived_by_debt: bool,
    /// Why no approval counts, when a verdict or the verdict graph says why.
    pub approval_rejection_reason: Option<String>,
    pub verdict_contested: bool,
    pub advisories: Vec<Advisory>,
}

/// Why an approval fails the self-approval guard, or `None` when it stands.
///
/// Independence is a relation between two identities, and only a declared one
/// is a claim somebody made. A reviewer matching a contributor on the work is
/// self-approval. A reviewing identity arc invented from `git config
/// user.name` names nobody in particular, so it cannot be the second party
/// either — two such identities that happen to differ do not show that two
/// people acted.
///
/// An *authoring* identity arc invented is a different case. It is what the
/// ledger holds, and a reviewer that declared a different name has claimed to
/// be somebody else, so an approval under that declared name is independent of
/// it. `arc audit` reads the shipped work the same way, and a debt is
/// discharged on the same terms: the surfaces before and after integration
/// answer independence identically.
///
/// Provenance recorded before arc kept it is *unknown*, not assumed, and is
/// compared by name as it always was — otherwise upgrading arc would strand
/// every existing ledger that uses this policy.
fn self_approval_rejection(
    patchset: &Patchset,
    verdict_author: &str,
    verdict_assumed: bool,
) -> Option<String> {
    // Naming the contributor is the more specific fact, so it is the one
    // reported when both hold.
    if let Some(contributor) = patchset.contributor_match(verdict_author) {
        return Some(format!(
            "{SELF_APPROVAL_REASON}: reviewer matches contributor {contributor}"
        ));
    }
    verdict_assumed.then(|| UNDECLARED_APPROVAL_REASON.to_string())
}

fn short_revision(revision: &str) -> &str {
    &revision[..revision.len().min(8)]
}

pub fn decide(facts: &ApprovalFacts) -> ApprovalOutcome {
    let requires_independent_review = facts.forbid_self_approval && facts.danger.dangerous;
    // An external approval is a record of someone else's decision that arc
    // cannot verify, so on a dangerous path it only stands beside a local
    // approval and never supplies one, whatever policy says of self-approval.
    // A repository that declares no danger paths has none; its uniform gate
    // refuses an external approval alone only where independence is owed.
    let external_approval_needs_local = requires_independent_review
        || (facts.danger.dangerous && facts.danger.rule != DangerRule::NotDeclared);
    let debt_waives_current_head = facts.head_matches && facts.debt_waives_latest_patchset;
    let names_latest_patchset = |patchset_id: &str| {
        facts
            .latest_patchset
            .filter(|patchset| patchset.id == patchset_id)
    };

    // Self-approval compares the reviewer with every contributor on the
    // patchset, so a lead may review work recorded for an executor unless the
    // lead is also in the declared contributor set. Computed once, so the
    // validity below and the reason a reader is given cannot disagree.
    let self_rejection = facts.latest_verdict.and_then(|verdict| {
        names_latest_patchset(&verdict.patchset_id).and_then(|patchset| {
            self_approval_rejection(
                patchset,
                verdict.effective_author(),
                verdict.author_assumed(),
            )
        })
    });
    // A declared debt converts the absent or policy-rejected review into a
    // recorded obligation. The requirement is carried forward where a query
    // can find it when no independent reviewer is reachable.
    let would_reject_self_approval = requires_independent_review && self_rejection.is_some();
    let rejected_self_approval = would_reject_self_approval && !debt_waives_current_head;

    let verdict = facts.latest_verdict.map(|v| VerdictStatus {
        verdict: v.verdict,
        patchset_id: v.patchset_id.clone(),
        body: v.body.clone(),
        provisional: v.provisional.clone(),
        actor: v.actor.clone(),
        on_behalf_of: v.on_behalf_of.clone(),
        author_assumed: v.author_assumed(),
        valid_for_current_head: v.verdict == Verdict::Approved
            && names_latest_patchset(&v.patchset_id).is_some()
            && facts.approved_head_matches
            && !rejected_self_approval,
    });
    let local_approval_valid = verdict
        .as_ref()
        .is_some_and(|verdict| verdict.valid_for_current_head);
    // The waiver only authorizes anything when it is what let the approval
    // stand. Recording it otherwise would claim a merge rested on a waiver
    // that changed nothing. Only an approval can be waived into validity: a
    // waiver declared beside a changes-requested or comment-only verdict
    // authorized nothing, and saying otherwise would report an approval that
    // does not exist.
    let waiver_authorized_approval = facts
        .latest_verdict
        .is_some_and(|v| v.verdict == Verdict::Approved)
        && would_reject_self_approval
        && debt_waives_current_head;
    let local_verdict_refuses_this_head = facts.latest_verdict.is_some_and(|verdict| {
        verdict.verdict != Verdict::Approved
            && names_latest_patchset(&verdict.patchset_id).is_some()
    });

    let latest_for_revision = |external: &ExternalVerdictEntry| {
        facts
            .external_verdicts
            .iter()
            .rev()
            .find(|latest| latest.revision == external.revision)
            .is_some_and(|latest| latest.event_id == external.event_id)
    };
    let matches_current_patchset = |external: &ExternalVerdictEntry| {
        latest_for_revision(external)
            && facts.head_matches
            && facts.current_head == Some(external.revision.as_str())
    };
    let external_verdicts: Vec<ExternalVerdictStatus> = facts
        .external_verdicts
        .iter()
        .rev()
        .map(|external| {
            let matches_current_patchset = matches_current_patchset(external);
            ExternalVerdictStatus {
                source: "external",
                event_id: external.event_id.clone(),
                revision: external.revision.clone(),
                verdict: external.verdict,
                decided_by: external.decided_by.clone(),
                reference: external.reference.clone(),
                recorded_by: external.recorded_by.clone(),
                created_at: external.created_at,
                matches_current_patchset,
                gates_current_head: matches_current_patchset
                    && external.verdict == ExternalVerdict::Approved
                    && !local_verdict_refuses_this_head
                    && !external_approval_needs_local,
                findings: external.findings.clone(),
            }
        })
        .collect();
    // The newest external verdict naming the head, when the head is the
    // newest patchset's: exactly the one entry that matches it.
    let current_external_verdict = facts
        .external_verdicts
        .iter()
        .rev()
        .find(|external| matches_current_patchset(external));
    let external_refuses_this_head = current_external_verdict
        .is_some_and(|external| external.verdict != ExternalVerdict::Approved);
    let external_approval_valid = current_external_verdict.is_some_and(|external| {
        external.verdict == ExternalVerdict::Approved
            && !local_verdict_refuses_this_head
            && !external_approval_needs_local
    });
    let approval_valid = !external_refuses_this_head
        && !local_verdict_refuses_this_head
        && (external_approval_valid || local_approval_valid);

    // A waiver stands in for a verdict nobody recorded. It does not stand over
    // one that refused: a reviewer who read this patchset and asked for changes
    // has said something a waiver has no business overriding, and letting the
    // author waive past it would make the mechanism a way to ignore review
    // rather than a way to defer it.
    //
    // So the waiver satisfies this gate exactly when the gate is unmet for want
    // of a verdict — none recorded, or one that only policy's self-approval rule
    // rejects. The obligation itself is untouched and stays where
    // `arc query --debt` finds it.
    let waiver_satisfies_approval =
        debt_waives_current_head && !local_verdict_refuses_this_head && !external_refuses_this_head;
    // True whenever the waiver is load-bearing: it rescued a self-approval, or
    // it stood in for a verdict that was never recorded. Reporting it only in
    // the first case would let the second merge look independently approved.
    // An approval that stands without the waiver leaves it authorizing
    // nothing, and a refusal at the head leaves nothing standing for it to
    // have rescued.
    let approval_waived_by_debt =
        waiver_satisfies_approval && (waiver_authorized_approval || !approval_valid);

    let local_approval_rejection_reason = verdict.as_ref().and_then(|verdict| {
        if verdict.valid_for_current_head
            || verdict.verdict != Verdict::Approved
            || debt_waives_current_head
            || !requires_independent_review
        {
            return None;
        }
        self_rejection.clone()
    });
    let external_approval_rejection_reason =
        current_external_verdict.and_then(|external| match external.verdict {
            ExternalVerdict::ChangesRequested => Some(format!(
                "external verdict requests changes: {} at {} (reference {})",
                external.decided_by,
                short_revision(&external.revision),
                external.reference
            )),
            ExternalVerdict::Rejected => Some(format!(
                "external verdict rejected by {} at {} (reference {})",
                external.decided_by,
                short_revision(&external.revision),
                external.reference
            )),
            ExternalVerdict::Approved
                if !external_approval_needs_local
                    || local_approval_valid
                    || debt_waives_current_head =>
            {
                None
            }
            ExternalVerdict::Approved if requires_independent_review => Some(format!(
                "external approval by {} at {} (reference {}) cannot satisfy the independent-review rule on this dangerous patchset; Arc cannot verify the external identity (declared by {})",
                external.decided_by,
                short_revision(&external.revision),
                external.reference,
                if facts.forbid_self_approval_sources.is_empty() {
                    "source unavailable"
                } else {
                    facts.forbid_self_approval_sources.as_str()
                }
            )),
            ExternalVerdict::Approved => Some(format!(
                "external approval by {} at {} (reference {}) cannot approve this dangerous patchset alone; Arc cannot verify the external identity, so a local approval must stand beside it",
                external.decided_by,
                short_revision(&external.revision),
                external.reference
            )),
        });
    // A contested verdict graph has no single authority, so no latest verdict
    // is reported and the change reads as unreviewed. Without this the
    // blocker would say nobody reviewed it, which is the opposite of what
    // happened: two reviewers did, and each replaced the same verdict.
    let verdict_contested = facts.verdict_tips > 1;
    let approval_rejection_reason = external_approval_rejection_reason
        .or(local_approval_rejection_reason)
        .or_else(|| {
            (verdict_contested && !approval_valid)
                .then(|| format!("{} {CONTESTED_VERDICT_REASON}", facts.verdict_tips))
        });

    ApprovalOutcome {
        verdict,
        external_verdicts,
        current_external_verdict: current_external_verdict.map(|external| external.verdict),
        local_verdict_refuses_this_head,
        external_refuses_this_head,
        approval_valid,
        waiver_satisfies_approval,
        approval_waived_by_debt,
        approval_rejection_reason,
        verdict_contested,
        advisories: advisories(facts),
    }
}

/// Advisories for `arc check`. Never blockers: a change with one reviewer is
/// normal, and refusing it would make the tool unusable for the
/// single-operator case it is most often run in.
fn advisories(facts: &ApprovalFacts) -> Vec<Advisory> {
    let Some(final_patchset) = facts.latest_patchset else {
        return Vec::new();
    };
    let review_map = facts.review_map;
    let danger = facts.danger;
    let mut warnings = Vec::new();
    for row in review_map {
        if !row.covers_final {
            warnings.push(Advisory {
                code: "reviewer-behind-final-patchset",
                detail: format!(
                    "{} last saw {}; integrating {}",
                    row.reviewer, row.last_patchset, final_patchset.id
                ),
            });
        }
    }
    // An unproven reviewer is still not the author, so a provisional verdict
    // satisfies independence and is reported on its own axis. Collapsing the
    // two would make "nobody independent read this" and "somebody read it
    // whose judgment is not yet trusted" the same state, which is the
    // conflation this advisory exists to end.
    if let Some(reason) = facts.provisional_approval_reason {
        warnings.push(Advisory {
            code: "provisional-approval",
            detail: format!(
                "the verdict covering {} is owed corroboration: {reason}. Discharge it with an \
                 independent review of this patchset, or `arc audit` after it lands",
                final_patchset.id
            ),
        });
    }
    let independent = review_map
        .iter()
        .any(|row| row.covers_final && !row.is_author && !row.attribution_unknown);
    let matched = review_map
        .iter()
        .filter(|row| row.covers_final)
        .filter_map(|row| {
            row.matched_contributor
                .as_deref()
                .map(|contributor| format!("{} matches contributor {contributor}", row.reviewer))
        })
        .collect::<Vec<_>>();
    if !independent && !danger.dangerous {
        let detail = if matched.is_empty() {
            format!(
                "no independent reviewer covers {}, and none is required: {}",
                final_patchset.id,
                danger.explain()
            )
        } else {
            format!(
                "no independent reviewer covers {}; {}; none is required: {}",
                final_patchset.id,
                matched.join(", "),
                danger.explain()
            )
        };
        warnings.push(Advisory {
            code: "self-verdict-permitted",
            detail,
        });
    }
    if !independent && danger.dangerous {
        let unknown = review_map
            .iter()
            .any(|row| row.covers_final && row.attribution_unknown);
        warnings.push(if unknown {
            Advisory {
                code: "reviewer-attribution-unknown",
                detail: format!(
                    "no reviewer of {} is distinguishable from its author; \
                     record --on-behalf-of to make attribution legible{}",
                    final_patchset.id,
                    if matched.is_empty() {
                        String::new()
                    } else {
                        format!("; {}", matched.join(", "))
                    }
                ),
            }
        } else {
            let detail = if matched.is_empty() {
                format!("no independent reviewer covers {}", final_patchset.id)
            } else {
                format!(
                    "no independent reviewer covers {}; {}",
                    final_patchset.id,
                    matched.join(", ")
                )
            };
            Advisory {
                code: "no-independent-reviewer",
                detail,
            }
        });
    }
    // The review map makes brief-author-only review visible after the fact.
    // Saying it before integration is the point of an advisory: arc reports
    // that the identity which briefed the work is the only one that approved
    // it, and infers nothing about whether that was independent.
    if let Some(author) = facts.reviewed_only_by_brief_author {
        warnings.push(Advisory {
            code: "brief-author-only-review",
            detail: format!(
                "every verdict on {} came from {author}, who wrote the brief",
                final_patchset.id
            ),
        });
    }
    if facts.debt_outstanding {
        warnings.push(Advisory {
            code: "debt-outstanding",
            detail: "a review obligation was recorded at integration and is not discharged"
                .to_string(),
        });
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ActorSource;
    use chrono::DateTime;

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OLD_HEAD: &str = "2222222222222222222222222222222222222222";

    fn patchset() -> Patchset {
        Patchset {
            id: "ps-2".into(),
            actor: "author".into(),
            model: None,
            harness: None,
            session: None,
            session_link: None,
            actor_source: Some(ActorSource::Flag),
            on_behalf_of: None,
            base: OLD_HEAD.into(),
            head: HEAD.into(),
            approved_head: HEAD.into(),
            merge_base: None,
            brief_ref: None,
            brief_version: None,
            author: None,
            committer: None,
            contributors: vec!["author".into()],
            claim_id: None,
            claim_actor: None,
            journal_refs: Vec::new(),
            thread: None,
            candidate: None,
            provenance_mismatch: None,
            created_at: DateTime::UNIX_EPOCH,
        }
    }

    fn verdict(verdict: Verdict, actor: &str) -> VerdictEntry {
        VerdictEntry {
            event_id: format!("verdict-{actor}"),
            patchset_id: "ps-2".into(),
            verdict,
            causes: Vec::new(),
            body: None,
            model: None,
            provisional: None,
            route_version: None,
            actor: actor.into(),
            on_behalf_of: None,
            actor_source: Some(ActorSource::Flag),
            relation: None,
            created_at: DateTime::UNIX_EPOCH,
        }
    }

    fn external(event_id: &str, revision: &str, verdict: ExternalVerdict) -> ExternalVerdictEntry {
        ExternalVerdictEntry {
            source: "external",
            event_id: event_id.into(),
            revision: revision.into(),
            verdict,
            decided_by: "receiver".into(),
            reference: "ref-1".into(),
            findings: Vec::new(),
            recorded_by: "lead".into(),
            created_at: DateTime::UNIX_EPOCH,
        }
    }

    fn dangerous() -> DangerScope {
        DangerScope {
            dangerous: true,
            rule: DangerRule::DeclaredPath,
            paths: vec!["src/approval.rs".into()],
        }
    }

    fn untouched() -> DangerScope {
        DangerScope {
            dangerous: false,
            rule: DangerRule::Untouched,
            paths: Vec::new(),
        }
    }

    /// A dangerous change under `forbid_self_approval`, at the newest
    /// patchset's head, with no verdict, waiver, or external decision.
    fn facts<'a>(patchset: &'a Patchset, danger: &'a DangerScope) -> ApprovalFacts<'a> {
        ApprovalFacts {
            latest_patchset: Some(patchset),
            current_head: Some(HEAD),
            head_matches: true,
            approved_head_matches: true,
            latest_verdict: None,
            verdict_tips: 0,
            external_verdicts: &[],
            debt_waives_latest_patchset: false,
            forbid_self_approval: true,
            forbid_self_approval_sources: ".arc/policy.toml".into(),
            danger,
            review_map: &[],
            provisional_approval_reason: None,
            reviewed_only_by_brief_author: None,
            debt_outstanding: false,
        }
    }

    #[test]
    fn independent_approval_of_the_head_is_valid() {
        let (patchset, danger) = (patchset(), dangerous());
        let approval = verdict(Verdict::Approved, "reviewer");
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            ..facts(&patchset, &danger)
        });
        assert!(outcome.approval_valid);
        assert!(outcome.verdict.unwrap().valid_for_current_head);
        assert!(!outcome.approval_waived_by_debt);
        assert!(!outcome.waiver_satisfies_approval);
        assert_eq!(outcome.approval_rejection_reason, None);
    }

    #[test]
    fn self_approval_is_rejected_with_the_contributor_named() {
        let (patchset, danger) = (patchset(), dangerous());
        let approval = verdict(Verdict::Approved, "author");
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            ..facts(&patchset, &danger)
        });
        assert!(!outcome.approval_valid);
        assert_eq!(
            outcome.approval_rejection_reason.as_deref(),
            Some(format!("{SELF_APPROVAL_REASON}: reviewer matches contributor author").as_str())
        );

        // Where independence is not required, the same verdict stands.
        let untouched = untouched();
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            ..facts(&patchset, &untouched)
        });
        assert!(outcome.approval_valid);
        assert_eq!(outcome.approval_rejection_reason, None);
    }

    #[test]
    fn an_assumed_reviewing_identity_is_rejected_as_undeclared() {
        let (patchset, danger) = (patchset(), dangerous());
        let approval = VerdictEntry {
            actor_source: Some(ActorSource::GitFallback),
            ..verdict(Verdict::Approved, "somebody")
        };
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            ..facts(&patchset, &danger)
        });
        assert!(!outcome.approval_valid);
        assert_eq!(
            outcome.approval_rejection_reason.as_deref(),
            Some(UNDECLARED_APPROVAL_REASON)
        );
    }

    #[test]
    fn an_approval_left_behind_by_the_head_is_stale() {
        let (patchset, danger) = (patchset(), dangerous());
        let approval = verdict(Verdict::Approved, "reviewer");
        // A rewrite that changed content moved the head off the approved one.
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            approved_head_matches: false,
            ..facts(&patchset, &danger)
        });
        assert!(!outcome.approval_valid);
        assert!(!outcome.verdict.unwrap().valid_for_current_head);
        assert_eq!(outcome.approval_rejection_reason, None);

        // An approval of an earlier patchset covers nothing here.
        let earlier = VerdictEntry {
            patchset_id: "ps-1".into(),
            ..verdict(Verdict::Approved, "reviewer")
        };
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&earlier),
            verdict_tips: 1,
            ..facts(&patchset, &danger)
        });
        assert!(!outcome.approval_valid);
        assert!(!outcome.local_verdict_refuses_this_head);
    }

    #[test]
    fn a_waiver_authorizes_a_self_approval_and_stands_in_for_a_missing_one() {
        let (patchset, danger) = (patchset(), dangerous());
        let approval = verdict(Verdict::Approved, "author");
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            debt_waives_latest_patchset: true,
            ..facts(&patchset, &danger)
        });
        assert!(outcome.approval_valid);
        assert!(outcome.approval_waived_by_debt);
        assert!(outcome.waiver_satisfies_approval);
        assert_eq!(outcome.approval_rejection_reason, None);

        let outcome = decide(&ApprovalFacts {
            debt_waives_latest_patchset: true,
            ..facts(&patchset, &danger)
        });
        assert!(!outcome.approval_valid);
        assert!(outcome.approval_waived_by_debt);
        assert!(outcome.waiver_satisfies_approval);

        // A waiver names a patchset, so a head that moved past it is unwaived.
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            debt_waives_latest_patchset: true,
            head_matches: false,
            ..facts(&patchset, &danger)
        });
        assert!(!outcome.approval_waived_by_debt);
        assert!(!outcome.waiver_satisfies_approval);
    }

    #[test]
    fn a_local_refusal_of_this_patchset_is_not_waived() {
        let (patchset, danger) = (patchset(), dangerous());
        let refusal = verdict(Verdict::ChangesRequested, "reviewer");
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&refusal),
            verdict_tips: 1,
            debt_waives_latest_patchset: true,
            ..facts(&patchset, &danger)
        });
        assert!(outcome.local_verdict_refuses_this_head);
        assert!(!outcome.approval_valid);
        assert!(!outcome.approval_waived_by_debt);
        assert!(!outcome.waiver_satisfies_approval);
    }

    #[test]
    fn an_external_refusal_leaves_a_waived_self_approval_nothing_to_rescue() {
        let (patchset, danger) = (patchset(), dangerous());
        let approval = verdict(Verdict::Approved, "author");
        let refusals = [external("ext-1", HEAD, ExternalVerdict::ChangesRequested)];
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            external_verdicts: &refusals,
            debt_waives_latest_patchset: true,
            ..facts(&patchset, &danger)
        });
        assert!(outcome.external_refuses_this_head);
        assert!(!outcome.approval_valid);
        assert!(!outcome.approval_waived_by_debt);
        assert!(!outcome.waiver_satisfies_approval);
        assert_eq!(
            outcome.approval_rejection_reason.as_deref(),
            Some("external verdict requests changes: receiver at 11111111 (reference ref-1)")
        );
    }

    #[test]
    fn an_external_verdict_refusing_the_head_overrides_a_local_approval() {
        let (patchset, danger) = (patchset(), untouched());
        let approval = verdict(Verdict::Approved, "reviewer");
        let externals = [
            external("ext-1", HEAD, ExternalVerdict::Approved),
            external("ext-2", HEAD, ExternalVerdict::ChangesRequested),
            external("ext-3", OLD_HEAD, ExternalVerdict::Approved),
        ];
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            external_verdicts: &externals,
            debt_waives_latest_patchset: true,
            ..facts(&patchset, &danger)
        });
        assert!(outcome.external_refuses_this_head);
        assert_eq!(
            outcome.current_external_verdict,
            Some(ExternalVerdict::ChangesRequested)
        );
        assert!(!outcome.approval_valid);
        assert!(!outcome.waiver_satisfies_approval);
        assert_eq!(
            outcome.approval_rejection_reason.as_deref(),
            Some("external verdict requests changes: receiver at 11111111 (reference ref-1)")
        );
        // Newest first; only the newest decision on the head matches it.
        let matching: Vec<(&str, bool)> = outcome
            .external_verdicts
            .iter()
            .map(|status| (status.event_id.as_str(), status.matches_current_patchset))
            .collect();
        assert_eq!(
            matching,
            [("ext-3", false), ("ext-2", true), ("ext-1", false)]
        );
    }

    #[test]
    fn an_external_approval_cannot_satisfy_independent_review() {
        let (patchset, danger) = (patchset(), dangerous());
        let externals = [external("ext-1", HEAD, ExternalVerdict::Approved)];
        let outcome = decide(&ApprovalFacts {
            external_verdicts: &externals,
            ..facts(&patchset, &danger)
        });
        assert!(!outcome.approval_valid);
        assert!(!outcome.external_verdicts[0].gates_current_head);
        let reason = outcome.approval_rejection_reason.unwrap();
        assert!(
            reason.contains("cannot satisfy the independent-review rule")
                && reason.ends_with("(declared by .arc/policy.toml)"),
            "{reason}"
        );

        // Outside the danger scope it is an approval like any other.
        let untouched = untouched();
        let outcome = decide(&ApprovalFacts {
            external_verdicts: &externals,
            ..facts(&patchset, &untouched)
        });
        assert!(outcome.approval_valid);
        assert!(outcome.external_verdicts[0].gates_current_head);
    }

    #[test]
    fn an_external_approval_never_counts_alone_on_a_dangerous_path() {
        let (patchset, danger) = (patchset(), dangerous());
        let externals = [external("ext-1", HEAD, ExternalVerdict::Approved)];
        let permissive = || ApprovalFacts {
            external_verdicts: &externals,
            forbid_self_approval: false,
            forbid_self_approval_sources: String::new(),
            ..facts(&patchset, &danger)
        };
        let outcome = decide(&permissive());
        assert!(!outcome.approval_valid);
        assert!(!outcome.external_verdicts[0].gates_current_head);
        let reason = outcome.approval_rejection_reason.unwrap();
        assert!(
            reason.contains("cannot approve this dangerous patchset alone")
                && !reason.contains("independent-review"),
            "{reason}"
        );

        // Beside a local approval it stands, and the local verdict gates.
        let approval = verdict(Verdict::Approved, "author");
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            ..permissive()
        });
        assert!(outcome.approval_valid);
        assert!(outcome.verdict.unwrap().valid_for_current_head);
        assert!(!outcome.external_verdicts[0].gates_current_head);
        assert_eq!(outcome.approval_rejection_reason, None);

        // A non-approving external verdict still refuses beside it.
        let refusals = [external("ext-2", HEAD, ExternalVerdict::ChangesRequested)];
        let outcome = decide(&ApprovalFacts {
            latest_verdict: Some(&approval),
            verdict_tips: 1,
            external_verdicts: &refusals,
            ..permissive()
        });
        assert!(outcome.external_refuses_this_head);
        assert!(!outcome.approval_valid);

        // A repository that declares no danger paths has none to guard.
        let undeclared = DangerScope {
            dangerous: true,
            rule: DangerRule::NotDeclared,
            paths: Vec::new(),
        };
        let outcome = decide(&ApprovalFacts {
            danger: &undeclared,
            ..permissive()
        });
        assert!(outcome.approval_valid);
        assert!(outcome.external_verdicts[0].gates_current_head);
    }

    #[test]
    fn a_waiver_beside_an_approval_that_stands_without_it_authorizes_nothing() {
        let (patchset, danger) = (patchset(), untouched());
        let externals = [external("ext-1", HEAD, ExternalVerdict::Approved)];
        let outcome = decide(&ApprovalFacts {
            external_verdicts: &externals,
            debt_waives_latest_patchset: true,
            ..facts(&patchset, &danger)
        });
        assert!(outcome.approval_valid);
        assert!(!outcome.approval_waived_by_debt);
        assert_eq!(outcome.approval_rejection_reason, None);

        // On a dangerous path the same external approval stands on nothing,
        // so the waiver is what the change rests on.
        let danger = dangerous();
        let outcome = decide(&ApprovalFacts {
            external_verdicts: &externals,
            debt_waives_latest_patchset: true,
            forbid_self_approval: false,
            ..facts(&patchset, &danger)
        });
        assert!(!outcome.approval_valid);
        assert!(outcome.approval_waived_by_debt);
        assert!(outcome.waiver_satisfies_approval);
        assert_eq!(outcome.approval_rejection_reason, None);
    }

    #[test]
    fn contested_verdicts_say_so_rather_than_reading_unreviewed() {
        let (patchset, danger) = (patchset(), dangerous());
        let outcome = decide(&ApprovalFacts {
            verdict_tips: 2,
            ..facts(&patchset, &danger)
        });
        assert!(outcome.verdict_contested);
        assert!(outcome.verdict.is_none());
        assert!(!outcome.approval_valid);
        assert_eq!(
            outcome.approval_rejection_reason,
            Some(format!("2 {CONTESTED_VERDICT_REASON}"))
        );
    }

    #[test]
    fn advisories_follow_the_review_map_and_the_danger_scope() {
        let patchset = patchset();
        let self_review = [ReviewerCoverage {
            reviewer: "author".into(),
            last_patchset: "ps-2".into(),
            verdicts: 1,
            findings: 0,
            covers_final: true,
            is_author: true,
            matched_contributor: Some("author".into()),
            contributors_source: "declared",
            attribution_unknown: false,
        }];
        let codes = |danger: &DangerScope| {
            decide(&ApprovalFacts {
                review_map: &self_review,
                reviewed_only_by_brief_author: Some("author"),
                debt_outstanding: true,
                ..facts(&patchset, danger)
            })
            .advisories
            .into_iter()
            .map(|advisory| advisory.code)
            .collect::<Vec<_>>()
        };
        assert_eq!(
            codes(&untouched()),
            [
                "self-verdict-permitted",
                "brief-author-only-review",
                "debt-outstanding"
            ]
        );
        assert_eq!(
            codes(&dangerous()),
            [
                "no-independent-reviewer",
                "brief-author-only-review",
                "debt-outstanding"
            ]
        );

        let danger = dangerous();
        let without_patchset = decide(&ApprovalFacts {
            latest_patchset: None,
            debt_outstanding: true,
            ..facts(&patchset, &danger)
        });
        assert!(without_patchset.advisories.is_empty());
    }
}
