//! Review receipts: `arc-review-receipt/1`.
//!
//! A delegated reader that runs read-only cannot write its own verdict. The
//! lead pins what it will read with `arc receipt template`, the reader fills
//! the template in, and `arc review --receipt` or `arc audit --receipt`
//! records each item as the ordinary verdict or audit it reports, on the
//! reader's behalf, through the guards a hand-recorded read passes.

use super::*;
use crate::state::{FindingState, Patchset};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;

pub const SCHEMA: &str = "arc-review-receipt/1";
pub const RESULT_SCHEMA: &str = "arc-receipt-result/1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    Verdict,
    Audit,
}

impl ItemKind {
    fn as_str(self) -> &'static str {
        match self {
            ItemKind::Verdict => "verdict",
            ItemKind::Audit => "audit",
        }
    }

    fn command(self) -> &'static str {
        match self {
            ItemKind::Verdict => "arc review --receipt",
            ItemKind::Audit => "arc audit --receipt",
        }
    }
}

/// What a reader had seen on the change when the template was issued: the
/// verdict tips and every audit and external verdict, the findings its
/// history leaves open, and the tips of every finding's dispositions. An
/// event of those kinds recorded later appears here and not in the item's
/// copy, which is what makes the item stale.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontier {
    pub verdicts: Vec<String>,
    pub findings: Vec<String>,
    pub dispositions: Vec<String>,
}

impl Frontier {
    fn of(st: &ChangeState) -> Frontier {
        let mut verdicts: BTreeSet<String> = st
            .verdict_tips()
            .iter()
            .map(|verdict| verdict.event_id.clone())
            .collect();
        verdicts.extend(st.audit_verdicts.iter().map(|a| a.event_id.clone()));
        verdicts.extend(st.external_verdicts.iter().map(|e| e.event_id.clone()));
        let mut findings = BTreeSet::new();
        let mut dispositions = BTreeSet::new();
        for finding in st.findings.values() {
            if !finding.currently_released() {
                findings.insert(finding.id.clone());
            }
            dispositions.extend(disposition_tips(finding));
        }
        for finding in st.audit_findings.values() {
            if !finding.released() {
                findings.insert(finding.id.clone());
            }
            dispositions.extend(disposition_tips(finding));
        }
        Frontier {
            verdicts: verdicts.into_iter().collect(),
            findings: findings.into_iter().collect(),
            dispositions: dispositions.into_iter().collect(),
        }
    }

    /// The ids in `self` that `observed` does not name.
    fn unobserved(&self, observed: &Frontier) -> Vec<String> {
        let named: BTreeSet<&str> = observed
            .verdicts
            .iter()
            .chain(&observed.findings)
            .chain(&observed.dispositions)
            .map(String::as_str)
            .collect();
        let mut unnamed: Vec<String> = self
            .verdicts
            .iter()
            .chain(&self.findings)
            .chain(&self.dispositions)
            .filter(|id| !named.contains(id.as_str()))
            .cloned()
            .collect();
        unnamed.sort();
        unnamed.dedup();
        unnamed
    }
}

/// The tips of every disposition a finding holds, before and after
/// integration, whatever their gating eligibility: a disposition nobody
/// observed is new whether or not it gates.
fn disposition_tips(finding: &FindingState) -> Vec<String> {
    let entries: Vec<_> = finding
        .dispositions
        .iter()
        .chain(&finding.after_integration)
        .collect();
    let superseded: BTreeSet<&str> = entries
        .iter()
        .flat_map(|entry| entry.supersedes.iter().map(String::as_str))
        .collect();
    entries
        .iter()
        .filter(|entry| !superseded.contains(entry.event_id.as_str()))
        .map(|entry| entry.event_id.clone())
        .collect()
}

/// What a verdict item was issued against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerdictSubject {
    /// The store that issued the template.
    pub origin: String,
    pub change: String,
    pub patchset: String,
    pub base: String,
    pub head: String,
    /// `sha256:` over the body of the brief the patchset was bound to, or
    /// null for a patchset bound to none.
    pub brief_digest: Option<String>,
    pub frontier: Frontier,
}

/// What an audit item was issued against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSubject {
    pub origin: String,
    pub change: String,
    /// The integration closure's event.
    pub closure: String,
    /// The integrated revision.
    pub revision: String,
    /// The patchset the integration shipped, when the closure recorded one.
    pub patchset: Option<String>,
    /// The debts outstanding when the template was issued.
    pub debts: Vec<String>,
    pub frontier: Frontier,
}

#[derive(Clone, Debug)]
enum Subject {
    Verdict(VerdictSubject),
    Audit(AuditSubject),
}

impl Subject {
    fn change(&self) -> &str {
        match self {
            Subject::Verdict(subject) => &subject.change,
            Subject::Audit(subject) => &subject.change,
        }
    }

    fn origin(&self) -> &str {
        match self {
            Subject::Verdict(subject) => &subject.origin,
            Subject::Audit(subject) => &subject.origin,
        }
    }

    fn frontier(&self) -> &Frontier {
        match self {
            Subject::Verdict(subject) => &subject.frontier,
            Subject::Audit(subject) => &subject.frontier,
        }
    }
}

fn brief_digest(st: &ChangeState, patchset: &Patchset) -> Option<String> {
    let reference = patchset.brief_ref.as_ref()?;
    st.briefs
        .iter()
        .find(|brief| brief.event_id == reference.event_id)
        .map(|brief| {
            format!(
                "sha256:{}",
                hex::encode(Sha256::digest(brief.body.as_bytes()))
            )
        })
}

fn verdict_subject(origin: &str, st: &ChangeState) -> Result<VerdictSubject> {
    if let Some(closure) = &st.closure {
        bail!(
            "change {} is {}; a verdict item needs an open change (use --audit for an \
             integrated one)",
            st.change_id,
            serde_json::to_value(closure.outcome)?
                .as_str()
                .unwrap_or("closed")
        );
    }
    let patchset = st.latest_patchset().with_context(|| {
        format!(
            "change {} has no patchset to read; run `arc snapshot` first",
            st.change_id
        )
    })?;
    Ok(VerdictSubject {
        origin: origin.to_string(),
        change: st.change_id.clone(),
        patchset: patchset.id.clone(),
        base: patchset.base.clone(),
        head: patchset.head.clone(),
        brief_digest: brief_digest(st, patchset),
        frontier: Frontier::of(st),
    })
}

fn audit_subject(origin: &str, st: &ChangeState) -> Result<AuditSubject> {
    let closure = st
        .closure
        .as_ref()
        .filter(|closure| closure.outcome == Closure::Integrated)
        .with_context(|| {
            format!(
                "change {} has not integrated; an audit item reads an integrated revision \
                 (use --verdict for an open change)",
                st.change_id
            )
        })?;
    let revision = closure.integrated_commit.clone().with_context(|| {
        format!(
            "change {} closed without an integration commit; nothing to audit",
            st.change_id
        )
    })?;
    Ok(AuditSubject {
        origin: origin.to_string(),
        change: st.change_id.clone(),
        closure: closure.event_id.clone(),
        revision,
        patchset: closure.source_patchset_id.clone(),
        debts: outstanding_debts(st),
        frontier: Frontier::of(st),
    })
}

fn outstanding_debts(st: &ChangeState) -> Vec<String> {
    st.debt
        .as_ref()
        .filter(|_| st.debt_outstanding())
        .map(|debt| vec![debt.event_id.clone()])
        .unwrap_or_default()
}

/// JSON with every object's keys sorted, so equal content has one spelling.
fn canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&map[key], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical(value, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn canonical_string(value: &serde_json::Value) -> String {
    let mut out = String::new();
    canonical(value, &mut out);
    out
}

/// An item's key: its kind, its subject's canonical content and the
/// template's issue time, hashed. Stable across retries of one template; a
/// template issued later keys its items afresh.
fn item_key(kind: ItemKind, subject: &serde_json::Value, issued_at: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(kind.as_str().as_bytes());
    hash.update(b"\n");
    hash.update(canonical_string(subject).as_bytes());
    hash.update(b"\n");
    hash.update(issued_at.as_bytes());
    format!("rk-{}", &hex::encode(hash.finalize())[..32])
}

/// Print an `arc-review-receipt/1` template with one item per change.
pub fn template(ctx: &Ctx, kind: ItemKind, references: &[String]) -> Result<()> {
    let store = ctx.store()?;
    let issued_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for reference in references {
        let change_id = store.resolve_change(reference)?;
        if !seen.insert(change_id.clone()) {
            bail!("change {change_id} is named twice; a template holds one item per change");
        }
        let st = store.state(&change_id)?;
        let subject = match kind {
            ItemKind::Verdict => serde_json::to_value(verdict_subject(&store.repository_id, &st)?)?,
            ItemKind::Audit => serde_json::to_value(audit_subject(&store.repository_id, &st)?)?,
        };
        items.push(serde_json::json!({
            "key": item_key(kind, &subject, &issued_at),
            "kind": kind.as_str(),
            "subject": subject,
            "verdict": null,
            "causes": [],
            "body": null,
            "findings": [],
            "dispositions": [],
            "assessment": null,
        }));
    }
    let template = serde_json::json!({
        "schema": SCHEMA,
        "issued_at": issued_at,
        "reviewer": {"harness": null, "session": null, "model": null},
        "items": items,
    });
    println!("{}", serde_json::to_string_pretty(&template)?);
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnvelope {
    schema: String,
    #[serde(default)]
    issued_at: Option<String>,
    #[serde(default)]
    reviewer: Option<RawReviewer>,
    items: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReviewer {
    #[serde(default)]
    harness: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    key: String,
    kind: ItemKind,
    subject: serde_json::Value,
    #[serde(default)]
    verdict: Option<Verdict>,
    #[serde(default)]
    causes: Vec<ReviewCause>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    findings: Vec<RawFinding>,
    #[serde(default)]
    dispositions: Vec<RawDisposition>,
    #[serde(default)]
    assessment: Option<serde_json::Value>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawFinding {
    severity: Severity,
    blocking: bool,
    summary: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ReceiptDisposition {
    Fixed,
    StillOpen,
    NotADefect,
}

impl ReceiptDisposition {
    /// The ledger status a receipt's disposition records. `not-a-defect` is
    /// the reader disputing the finding, which releases no blocker.
    fn status(self) -> DispositionStatus {
        match self {
            ReceiptDisposition::Fixed => DispositionStatus::Resolved,
            ReceiptDisposition::StillOpen => DispositionStatus::StillOpen,
            ReceiptDisposition::NotADefect => DispositionStatus::Disputed,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDisposition {
    finding: String,
    disposition: ReceiptDisposition,
    #[serde(default)]
    evidence: Option<String>,
}

/// Why an item was refused, by a stable code a caller can branch on.
#[derive(Debug)]
pub(super) struct ItemRefusal {
    code: &'static str,
    message: String,
    events: Vec<String>,
}

impl std::fmt::Display for ItemRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ItemRefusal {}

fn refuse(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    ItemRefusal {
        code,
        message: message.into(),
        events: Vec::new(),
    }
    .into()
}

/// What a receipt item's read carries into the one event it publishes.
pub(super) struct Admitted {
    pub dispositions: Vec<InlineDisposition>,
    pub reference: ReceiptReference,
}

pub(super) enum Admission {
    /// An identical item already published this event.
    Published(String),
    /// The item is current; publish it with these.
    Fresh(Admitted),
}

/// The checks a receipt item passes under the locks its read takes.
pub(super) struct ItemGuard {
    kind: ItemKind,
    key: String,
    issued_at: String,
    subject_value: serde_json::Value,
    subject: Subject,
    item_digest: String,
    receipt_digest: String,
    dispositions: Vec<RawDisposition>,
}

impl ItemGuard {
    /// Recognize an identical published item first, then revalidate the
    /// subject, the key and the frontier against `st`, then resolve the
    /// item's dispositions against the findings on the subject.
    pub(super) fn admit(
        &self,
        store: &Store,
        events: &[Event],
        st: &ChangeState,
    ) -> Result<Admission> {
        for event in events {
            let recorded = match &event.payload {
                Payload::VerdictRecorded {
                    receipt: Some(receipt),
                    ..
                }
                | Payload::AuditVerdictRecorded {
                    receipt: Some(receipt),
                    ..
                } => receipt,
                _ => continue,
            };
            if recorded.key != self.key {
                continue;
            }
            if recorded.item_digest == self.item_digest {
                return Ok(Admission::Published(event.event_id.clone()));
            }
            return Err(refuse(
                "key-conflict",
                format!(
                    "item {} was recorded as {} with item digest {}; this item's digest is {}. \
                     A changed reading needs a fresh template",
                    self.key, event.event_id, recorded.item_digest, self.item_digest
                ),
            ));
        }
        self.check_subject(store, st)?;
        let derived = item_key(self.kind, &self.subject_value, &self.issued_at);
        if derived != self.key {
            return Err(refuse(
                "invalid",
                format!(
                    "key {} does not derive from the item's subject and the receipt's issued_at \
                     ({derived}); copy the template's key and subject unchanged",
                    self.key
                ),
            ));
        }
        let unobserved = Frontier::of(st).unobserved(self.subject.frontier());
        if !unobserved.is_empty() {
            return Err(ItemRefusal {
                code: "stale-frontier",
                message: format!(
                    "recorded on {} after the template and not observed by this item: {}; issue \
                     a fresh template",
                    st.change_id,
                    unobserved.join(", ")
                ),
                events: unobserved,
            }
            .into());
        }
        let dispositions = self.resolve_dispositions(events, st)?;
        Ok(Admission::Fresh(Admitted {
            dispositions,
            reference: ReceiptReference {
                key: self.key.clone(),
                item_digest: self.item_digest.clone(),
                receipt_digest: self.receipt_digest.clone(),
            },
        }))
    }

    fn check_subject(&self, store: &Store, st: &ChangeState) -> Result<()> {
        if self.subject.origin() != store.repository_id {
            return Err(refuse(
                "subject-mismatch",
                format!(
                    "the template was issued by store {}, not this one ({})",
                    self.subject.origin(),
                    store.repository_id
                ),
            ));
        }
        if self.subject.change() != st.change_id {
            return Err(refuse(
                "subject-mismatch",
                format!(
                    "the subject names change {}, not {}",
                    self.subject.change(),
                    st.change_id
                ),
            ));
        }
        match &self.subject {
            Subject::Verdict(subject) => check_verdict_subject(subject, st),
            Subject::Audit(subject) => check_audit_subject(subject, st),
        }
    }

    fn resolve_dispositions(
        &self,
        events: &[Event],
        st: &ChangeState,
    ) -> Result<Vec<InlineDisposition>> {
        let mut named = BTreeSet::new();
        let mut resolved = Vec::new();
        for raw in &self.dispositions {
            let finding_id = self.finding_on_subject(&raw.finding, events, st)?;
            if !named.insert(finding_id.clone()) {
                return Err(refuse(
                    "contradiction",
                    format!("finding {finding_id} takes two dispositions in one item"),
                ));
            }
            let finding = match self.kind {
                ItemKind::Verdict => &st.findings[&finding_id],
                ItemKind::Audit => &st.audit_findings[&finding_id],
            };
            resolved.push(InlineDisposition {
                finding_id,
                status: raw.disposition.status(),
                evidence: raw.evidence.clone(),
                supersedes: finding
                    .tips()
                    .iter()
                    .map(|tip| tip.event_id.clone())
                    .collect(),
            });
        }
        Ok(resolved)
    }

    /// The finding id `reference` names on the subject, by exact finding id
    /// or exact finding event id: a verdict item disposes the change's review
    /// findings, an audit item its audit findings.
    fn finding_on_subject(
        &self,
        reference: &str,
        events: &[Event],
        st: &ChangeState,
    ) -> Result<String> {
        let (own, other, own_name, other_name) = match self.kind {
            ItemKind::Verdict => (&st.findings, &st.audit_findings, "review", "audit"),
            ItemKind::Audit => (&st.audit_findings, &st.findings, "audit", "review"),
        };
        let by_event = events
            .iter()
            .find(|event| event.event_id == reference)
            .and_then(|event| match &event.payload {
                Payload::FindingAdded { finding_id, .. }
                | Payload::AuditFindingAdded { finding_id, .. } => Some(finding_id.as_str()),
                _ => None,
            });
        let id = by_event.unwrap_or(reference);
        if own.contains_key(id) {
            return Ok(id.to_string());
        }
        let why = if other.contains_key(id) {
            format!(
                "is {other_name} finding on {}; a {} item disposes only {own_name} findings{}",
                st.change_id,
                self.kind.as_str(),
                match self.kind {
                    ItemKind::Audit => {
                        "; record a review finding's post-integration disposition with `arc resolve`"
                    }
                    ItemKind::Verdict => "",
                }
            )
        } else {
            format!("names no finding on {}", st.change_id)
        };
        Err(refuse(
            "contradiction",
            format!("disposition of {reference:?} {why}"),
        ))
    }
}

fn check_verdict_subject(subject: &VerdictSubject, st: &ChangeState) -> Result<()> {
    let Some(patchset) = st.patchsets.iter().find(|p| p.id == subject.patchset) else {
        return Err(refuse(
            "subject-mismatch",
            format!("{} has no patchset {}", st.change_id, subject.patchset),
        ));
    };
    if patchset.head != subject.head {
        return Err(refuse(
            "subject-mismatch",
            format!(
                "patchset {} records head {}; the item names head {}",
                patchset.id, patchset.head, subject.head
            ),
        ));
    }
    let sharing: Vec<&str> = st
        .patchsets
        .iter()
        .filter(|p| p.head == subject.head)
        .map(|p| p.id.as_str())
        .collect();
    if sharing.len() > 1 {
        return Err(refuse(
            "ambiguous-head",
            format!(
                "head {} is recorded by patchsets {}; issue a fresh template for the patchset \
                 being read",
                subject.head,
                sharing.join(", ")
            ),
        ));
    }
    if patchset.base != subject.base {
        return Err(refuse(
            "subject-mismatch",
            format!(
                "patchset {} records base {}; the item names base {}",
                patchset.id, patchset.base, subject.base
            ),
        ));
    }
    let digest = brief_digest(st, patchset);
    if digest != subject.brief_digest {
        return Err(refuse(
            "subject-mismatch",
            format!(
                "patchset {} is bound to brief digest {}; the item names {}",
                patchset.id,
                digest.as_deref().unwrap_or("none"),
                subject.brief_digest.as_deref().unwrap_or("none")
            ),
        ));
    }
    Ok(())
}

fn check_audit_subject(subject: &AuditSubject, st: &ChangeState) -> Result<()> {
    let Some(closure) = st
        .closure
        .as_ref()
        .filter(|closure| closure.outcome == Closure::Integrated)
    else {
        return Err(refuse(
            "subject-mismatch",
            format!("{} has not integrated", st.change_id),
        ));
    };
    let mismatches = [
        (
            "closure",
            Some(closure.event_id.as_str()),
            Some(subject.closure.as_str()),
        ),
        (
            "integrated revision",
            closure.integrated_commit.as_deref(),
            Some(subject.revision.as_str()),
        ),
        (
            "source patchset",
            closure.source_patchset_id.as_deref(),
            subject.patchset.as_deref(),
        ),
    ];
    for (name, recorded, named) in mismatches {
        if recorded != named {
            return Err(refuse(
                "subject-mismatch",
                format!(
                    "{} records {name} {}; the item names {}",
                    st.change_id,
                    recorded.unwrap_or("none"),
                    named.unwrap_or("none")
                ),
            ));
        }
    }
    let debts = outstanding_debts(st);
    if debts != subject.debts {
        return Err(refuse(
            "subject-mismatch",
            format!(
                "{} owes debts [{}]; the item was issued against [{}]",
                st.change_id,
                debts.join(", "),
                subject.debts.join(", ")
            ),
        ));
    }
    Ok(())
}

/// The model a receipt's assessment binds to: the one the reader's session
/// store observed where it disagrees with the receipt's declaration, so a
/// declared model the store contradicts binds nothing it did not earn.
pub(super) fn binding_model(ctx: &Ctx, receipt: bool) -> Option<String> {
    let attribution = ctx.resolve_model();
    match attribution.provenance.model_disagreement {
        Some(disagreement) if receipt => Some(disagreement.observed),
        _ => attribution.model,
    }
}

/// Whether `session` has the shape of a complete native session id for
/// `harness`. A prefix, a delegate handle and an unknown harness's id are
/// refused: arc cannot tell a whole id from part of one where it does not
/// know the shape.
fn full_native_session(harness: &str, session: &str) -> bool {
    match harness {
        "claude" | "codex" | "pi" => {
            let groups: Vec<&str> = session.split('-').collect();
            groups.len() == 5
                && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, len)| {
                    group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit())
                })
        }
        "opencode" => session.strip_prefix("ses_").is_some_and(|rest| {
            rest.len() >= 16 && rest.bytes().all(|b| b.is_ascii_alphanumeric())
        }),
        _ => false,
    }
}

/// The `--on-behalf-of` subject the receipt's reviewer is recorded as.
fn reviewer_subject(reviewer: Option<&RawReviewer>) -> Result<String> {
    let reviewer = reviewer.context("the receipt names no reviewer")?;
    let filled = |name: &str, value: &Option<String>| -> Result<String> {
        match value.as_deref().map(str::trim) {
            Some(value) if !value.is_empty() => Ok(value.to_string()),
            _ => bail!("reviewer.{name} is not filled in"),
        }
    };
    let harness = filled("harness", &reviewer.harness)?;
    let session = filled("session", &reviewer.session)?;
    let model = filled("model", &reviewer.model)?;
    if crate::coordinate::is_placeholder(&harness) || !crate::model::plain_session_id(&harness) {
        bail!("reviewer.harness {harness:?} is not a harness name");
    }
    if !full_native_session(&harness, &session) {
        bail!(
            "reviewer.session {session:?} is not a full native {harness} session id; a prefix \
             or a delegate handle names no session arc can corroborate (claude, codex and pi \
             ids are UUIDs, opencode ids start ses_)"
        );
    }
    if let Some(defect) = crate::coordinate::model_defect(&model, Some(&session)) {
        bail!("reviewer.model {model:?}: {}", defect.reason());
    }
    if !crate::model::portable_coordinate(&model) || crate::model::model_effort(&model).is_none() {
        bail!("reviewer.model {model:?} must be model#effort");
    }
    let subject = format!("{harness}:{session}:{model}");
    if crate::coordinate::session_subject(&subject).is_none() {
        bail!("reviewer {subject:?} is not a harness:session:model subject");
    }
    Ok(subject)
}

/// The invocation acting for the receipt's reviewer: the invoker writes, the
/// reviewer is the represented subject, and the reviewer's model is the
/// one the receipt declares, corroborated against the reviewer's session.
fn reviewer_ctx(ctx: &Ctx, subject: String) -> Result<Ctx> {
    if ctx.on_behalf_of.is_some() {
        bail!("--on-behalf-of conflicts with --receipt: the receipt names its reviewer");
    }
    if ctx.model_source == Some(ModelSource::Flag) {
        bail!("--model conflicts with --receipt: the receipt names its reviewer's model");
    }
    let mut represented = ctx.with_cwd(ctx.cwd.clone());
    represented.on_behalf_of = Some(subject);
    represented.model = None;
    represented.model_source = None;
    represented.model_attribution = std::rc::Rc::new(std::cell::OnceCell::new());
    represented.ensure_fresh_identity()?;
    Ok(represented)
}

/// A parsed item, ready for its read.
struct Item {
    raw: RawItem,
    guard: ItemGuard,
    assessed: Option<crate::assessment::Assessment>,
}

/// Parse and check what an item says on its own, before any lock: shape,
/// contradictions within it, and its assessment.
fn prepare(
    kind: ItemKind,
    value: &serde_json::Value,
    issued_at: &str,
    receipt_digest: &str,
    receipt_dir: &Path,
) -> Result<Item> {
    if value.get("extent").is_some() {
        return Err(refuse(
            "extent-not-accepted",
            "the item carries an extent, which this build does not yet accept; drop it to record \
             a whole-subject read",
        ));
    }
    let raw: RawItem = serde_json::from_value(value.clone())
        .map_err(|error| refuse("invalid", format!("not an {SCHEMA} item: {error}")))?;
    if raw.kind != kind {
        return Err(refuse(
            "invalid",
            "the item's kind differs from the receipt's",
        ));
    }
    let subject = match kind {
        ItemKind::Verdict => serde_json::from_value(raw.subject.clone()).map(Subject::Verdict),
        ItemKind::Audit => serde_json::from_value(raw.subject.clone()).map(Subject::Audit),
    }
    .map_err(|error| {
        refuse(
            "invalid",
            format!("not a {} subject: {error}", kind.as_str()),
        )
    })?;
    let Some(verdict) = raw.verdict else {
        return Err(refuse("invalid", "the item's verdict is not filled in"));
    };
    let blocking: Vec<String> = raw
        .findings
        .iter()
        .enumerate()
        .filter(|(_, finding)| finding.blocking)
        .map(|(index, finding)| {
            format!(
                "findings[{index}] {:?} (severity {}, blocking: true)",
                finding.summary,
                serde_json::to_value(finding.severity)
                    .ok()
                    .and_then(|value| value.as_str().map(String::from))
                    .unwrap_or_default()
            )
        })
        .collect();
    if verdict == Verdict::Approved && !blocking.is_empty() {
        return Err(refuse(
            "contradiction",
            format!(
                "approved with blocking findings: {}; an approval cannot carry a blocker, and \
                 arc does not clear the flag",
                blocking.join("; ")
            ),
        ));
    }
    match (kind, verdict) {
        (ItemKind::Verdict, Verdict::ChangesRequested) if raw.causes.is_empty() => {
            return Err(refuse(
                "contradiction",
                "changes-requested without causes; name the root cause of the requested rework",
            ));
        }
        (ItemKind::Verdict, Verdict::Approved | Verdict::CommentOnly) if !raw.causes.is_empty() => {
            return Err(refuse(
                "contradiction",
                "causes are only valid with changes-requested",
            ));
        }
        (ItemKind::Audit, _) if !raw.causes.is_empty() => {
            return Err(refuse("invalid", "an audit records no causes"));
        }
        _ => {}
    }
    for (index, finding) in raw.findings.iter().enumerate() {
        if finding.line.is_some() && finding.path.is_none() {
            return Err(refuse(
                "invalid",
                format!("findings[{index}] names a line without a path"),
            ));
        }
        if kind == ItemKind::Audit && finding.path.is_some() {
            return Err(refuse(
                "invalid",
                format!(
                    "findings[{index}] names a path; audit findings cannot carry an anchor, \
                     which resolves through a patchset diff"
                ),
            ));
        }
    }
    let mut digest_input = canonical_string(value);
    let assessed = match &raw.assessment {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(path)) => {
            let path = receipt_dir.join(path);
            let bytes = fs::read(&path).map_err(|error| {
                refuse(
                    "invalid",
                    format!("cannot read assessment {}: {error}", path.display()),
                )
            })?;
            digest_input.push_str("\nassessment ");
            digest_input.push_str(&hex::encode(Sha256::digest(&bytes)));
            let text = String::from_utf8(bytes).map_err(|_| {
                refuse(
                    "invalid",
                    format!("assessment {} is not UTF-8", path.display()),
                )
            })?;
            Some(crate::assessment::parse(&text).map_err(|error| {
                refuse(
                    "invalid",
                    format!("assessment {}: {error:#}", path.display()),
                )
            })?)
        }
        Some(object @ serde_json::Value::Object(_)) => Some(
            crate::assessment::parse(&object.to_string())
                .map_err(|error| refuse("invalid", format!("assessment: {error:#}")))?,
        ),
        Some(_) => {
            return Err(refuse(
                "invalid",
                "assessment must be an agent-routing-assessment object or a path",
            ))
        }
    };
    let item_digest = hex::encode(Sha256::digest(digest_input.as_bytes()));
    let guard = ItemGuard {
        kind,
        key: raw.key.clone(),
        issued_at: issued_at.to_string(),
        subject_value: raw.subject.clone(),
        subject,
        item_digest,
        receipt_digest: receipt_digest.to_string(),
        dispositions: raw.dispositions.clone(),
    };
    Ok(Item {
        raw,
        guard,
        assessed,
    })
}

/// One item's outcome, as `arc-receipt-result/1` reports it.
#[derive(Serialize)]
struct ItemResult {
    index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    change: Option<String>,
    /// `recorded`, `already-recorded` or `refused`.
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unobserved: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    findings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    binding: Option<AuditorBinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    uncovered_authors: Vec<String>,
    /// Why a recorded read clears nothing, when it does not: a findings read
    /// under the clearance policy, or what keeps an audit from discharging.
    #[serde(skip_serializing_if = "Option::is_none")]
    bar: Option<Bar>,
    #[serde(skip_serializing_if = "Option::is_none")]
    debt_discharged: Option<bool>,
    #[serde(skip)]
    output: String,
}

#[derive(Serialize)]
struct Bar {
    code: String,
    reason: String,
}

impl ItemResult {
    fn refused(index: usize, error: &anyhow::Error) -> ItemResult {
        let (code, reason, unobserved) = match error.downcast_ref::<ItemRefusal>() {
            Some(refusal) => (
                refusal.code,
                refusal.message.clone(),
                refusal.events.clone(),
            ),
            None => ("refused", format!("{error:#}"), Vec::new()),
        };
        ItemResult {
            index,
            key: None,
            change: None,
            outcome: "refused",
            event: None,
            item_digest: None,
            code: Some(code),
            reason: Some(reason),
            unobserved,
            findings: Vec::new(),
            binding: None,
            uncovered_authors: Vec::new(),
            bar: None,
            debt_discharged: None,
            output: String::new(),
        }
    }
}

/// Facts about a published read that say whether it clears.
fn describe(result: &mut ItemResult, kind: ItemKind, st: &ChangeState, event_id: &str) {
    match kind {
        ItemKind::Verdict => {
            let Some(verdict) = st.verdicts.iter().find(|v| v.event_id == event_id) else {
                return;
            };
            result.findings = st
                .findings
                .values()
                .filter(|finding| finding.origin_event == event_id)
                .map(|finding| finding.id.clone())
                .collect();
            if let Some(assessment) = &verdict.assessment {
                result.binding = assessment.binding;
                result.uncovered_authors = assessment.uncovered_authors.clone();
            }
            if st.review_clearance.is_assessed() && verdict.verdict != Verdict::CommentOnly {
                if let Some(why) = crate::state::clearance_gap(
                    verdict.effective_author(),
                    verdict.model.as_deref(),
                    verdict.assessment.as_ref(),
                ) {
                    result.bar = Some(Bar {
                        code: format!("findings-read:{}", why.as_str()),
                        reason: format!(
                            "is a findings read: it {}; an approval from it gates no \
                             integration and discharges no debt",
                            why.reason()
                        ),
                    });
                }
            }
        }
        ItemKind::Audit => {
            let Some(audit) = st.audit_verdicts.iter().find(|a| a.event_id == event_id) else {
                return;
            };
            result.findings = st
                .audit_findings
                .values()
                .filter(|finding| finding.origin_event == event_id)
                .map(|finding| finding.id.clone())
                .collect();
            if let Some(assessment) = &audit.assessment {
                result.binding = assessment.binding;
                result.uncovered_authors = assessment.uncovered_authors.clone();
            }
            if let Some(debt) = &st.debt {
                let discharger = debt.discharged_by_event_id.as_deref();
                result.debt_discharged = Some(discharger == Some(event_id));
                if discharger != Some(event_id) {
                    if let Some(bar) = st.audit_discharge_bar(event_id) {
                        result.bar = Some(Bar {
                            code: bar.as_str().to_string(),
                            reason: bar.reason(),
                        });
                    }
                }
            }
        }
    }
}

/// Record every item of the receipt at `path` as `kind` reads. Returns the
/// exit status: 0 when every item was recorded or already recorded, 1 when
/// any was refused. An error refuses the whole receipt and writes nothing.
pub fn ingest(ctx: &Ctx, kind: ItemKind, path: &Path, json: bool) -> Result<i32> {
    let bytes =
        fs::read(path).with_context(|| format!("cannot read receipt {}", path.display()))?;
    let envelope: RawEnvelope = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not an {SCHEMA} receipt", path.display()))?;
    if envelope.schema != SCHEMA {
        bail!("receipt schema {:?} is not {SCHEMA}", envelope.schema);
    }
    let issued_at = envelope
        .issued_at
        .as_deref()
        .filter(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
        .context("the receipt's issued_at is missing or not RFC 3339; keep the template's")?
        .to_string();
    let subject = reviewer_subject(envelope.reviewer.as_ref())?;
    if envelope.items.is_empty() {
        bail!("the receipt holds no items");
    }
    let mut kinds = BTreeSet::new();
    for (index, item) in envelope.items.iter().enumerate() {
        let named = item
            .get("kind")
            .cloned()
            .and_then(|value| serde_json::from_value::<ItemKind>(value).ok())
            .with_context(|| format!("items[{index}] names no kind (verdict or audit)"))?;
        kinds.insert(named.as_str());
        if named != kind {
            if kinds.len() > 1 {
                bail!(
                    "the receipt mixes verdict and audit items; split it and record each part \
                     with its own command"
                );
            }
            bail!(
                "the receipt holds {} items; record it with `{}`",
                named.as_str(),
                named.command()
            );
        }
    }
    let represented = reviewer_ctx(ctx, subject.clone())?;
    let attribution = represented.resolve_model();
    let store = ctx.store()?;
    let receipt_digest = hex::encode(Sha256::digest(&bytes));
    let content = crate::custody::Content {
        sha256: receipt_digest.clone(),
        byte_length: bytes.len() as u64,
    };
    crate::custody::Custody::at(&store.root)
        .install(&content, path)
        .with_context(|| format!("cannot keep receipt {} in custody", path.display()))?;
    let receipt_dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();

    let mut results = Vec::new();
    for (index, value) in envelope.items.iter().enumerate() {
        let item = match prepare(kind, value, &issued_at, &receipt_digest, &receipt_dir) {
            Ok(item) => item,
            Err(error) => {
                let mut result = ItemResult::refused(index, &error);
                result.key = value.get("key").and_then(|k| k.as_str()).map(String::from);
                results.push(result);
                continue;
            }
        };
        let change = item.guard.subject.change().to_string();
        let item_digest = item.guard.item_digest.clone();
        let key = item.guard.key.clone();
        let (recorded, output) =
            crate::operation_output::capture(|| record(&represented, kind, &change, item));
        let mut result = match recorded {
            Ok(recorded) => {
                let mut result = ItemResult {
                    index,
                    key: None,
                    change: None,
                    outcome: if recorded.existing {
                        "already-recorded"
                    } else {
                        "recorded"
                    },
                    event: Some(recorded.event_id.clone()),
                    item_digest: None,
                    code: None,
                    reason: None,
                    unobserved: Vec::new(),
                    findings: Vec::new(),
                    binding: None,
                    uncovered_authors: Vec::new(),
                    bar: None,
                    debt_discharged: None,
                    output: String::new(),
                };
                if let Ok(st) = store.state(&change) {
                    describe(&mut result, kind, &st, &recorded.event_id);
                }
                result
            }
            Err(error) => ItemResult::refused(index, &error),
        };
        result.key = Some(key);
        result.change = Some(change);
        result.item_digest = Some(item_digest);
        result.output = output;
        results.push(result);
        if index + 1 < envelope.items.len() {
            // The test suite interrupts a batch here, between two items.
            super::pause_until_released("ARC_RECEIPT_PAUSE")?;
        }
    }

    let refused = results.iter().filter(|r| r.outcome == "refused").count();
    if json {
        let value = serde_json::json!({
            "schema": RESULT_SCHEMA,
            "kind": kind.as_str(),
            "receipt_digest": receipt_digest,
            "reviewer": {
                "subject": subject,
                "model": attribution.model,
                "observed_model": attribution
                    .provenance
                    .model_disagreement
                    .as_ref()
                    .map(|d| d.observed.clone()),
            },
            "refused": refused,
            "items": results,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("receipt: {receipt_digest} ({} items)", results.len());
        println!("reviewer: {subject}");
        for result in &results {
            println!(
                "item {} {} {}: {}",
                result.index,
                result.key.as_deref().unwrap_or("(no key)"),
                result.change.as_deref().unwrap_or("-"),
                result.outcome
            );
            for line in result.output.lines() {
                println!("  {line}");
            }
            if let (Some(code), Some(reason)) = (result.code, &result.reason) {
                println!("  refused ({code}): {reason}");
            }
            if let Some(bar) = &result.bar {
                println!("  this read {} ({})", bar.reason, bar.code);
            }
        }
        if refused > 0 {
            println!("refused: {refused} of {} items", results.len());
        }
    }
    if let Some(disagreement) = &attribution.provenance.model_disagreement {
        eprintln!(
            "warning: the reviewer's session store records model {}, the receipt declares {}; \
             each read records the disagreement, and an assessment binds the observed model",
            disagreement.observed, disagreement.declared
        );
    }
    Ok(if refused > 0 { 1 } else { 0 })
}

/// Record one item's read through the command a hand-recorded read uses.
fn record(
    represented: &Ctx,
    kind: ItemKind,
    change: &str,
    item: Item,
) -> Result<super::review::Recorded> {
    let Item {
        raw,
        guard,
        assessed,
    } = item;
    let verdict = raw.verdict.expect("prepare refuses an unfilled verdict");
    match kind {
        ItemKind::Verdict => {
            let Subject::Verdict(subject) = &guard.subject else {
                unreachable!("a verdict item carries a verdict subject");
            };
            let findings = raw
                .findings
                .into_iter()
                .map(|finding| FindingInput {
                    forge_sources: Vec::new(),
                    blocking: finding.blocking,
                    severity: finding.severity,
                    summary: finding.summary,
                    body: finding.body,
                    anchor: finding.path.map(|path| AnchorInput {
                        path,
                        side: Side::Head,
                        line_start: finding.line,
                        line_end: None,
                        context: None,
                    }),
                })
                .collect();
            super::review::record_verdict(
                represented,
                change,
                super::review::ReviewArgs {
                    forge_source: Vec::new(),
                    verdict,
                    relation: VerdictRelationKind::Supersedes,
                    body: raw.body,
                    provisional: None,
                    patchset: Some(subject.patchset.clone()),
                    causes: raw.causes,
                    findings_json: None,
                    snapshot_first: false,
                    contributors: None,
                    solo: false,
                    route_version: None,
                    assessment: None,
                    assessed,
                    findings,
                },
                Some(&guard),
            )
        }
        ItemKind::Audit => {
            let findings = raw
                .findings
                .into_iter()
                .map(|finding| InlineFinding {
                    forge_sources: Vec::new(),
                    finding_id: ids::new_finding_id(),
                    blocking: finding.blocking,
                    severity: finding.severity,
                    summary: finding.summary,
                    body: finding.body,
                    anchor: None,
                })
                .collect();
            super::audit::record_audit(
                represented,
                change,
                AuditArgs {
                    verdict,
                    body: raw.body,
                    findings_json: None,
                    route_version: None,
                    assessment: None,
                    assessed,
                    findings,
                    extent: None,
                },
                Some(&guard),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let value = serde_json::json!({"b": [{"z": 1, "a": 2}], "a": "x"});
        assert_eq!(canonical_string(&value), r#"{"a":"x","b":[{"a":2,"z":1}]}"#);
    }

    #[test]
    fn a_full_native_session_is_whole_for_its_harness() {
        let uuid = "01a12b77-6c25-7c01-b325-6e5e44523cc4";
        assert!(full_native_session("codex", uuid));
        assert!(full_native_session("claude", uuid));
        assert!(!full_native_session("codex", "01a12b77"));
        assert!(!full_native_session("codex", &uuid[..30]));
        assert!(!full_native_session(
            "codex",
            "series-opus-3b-receipts-20261103T182958Z-da75"
        ));
        assert!(full_native_session("opencode", "ses_6c1a2b3c4d5e6f7a8b9c"));
        assert!(!full_native_session("opencode", "ses_6c1a"));
        assert!(!full_native_session("hermes", uuid));
    }
}
