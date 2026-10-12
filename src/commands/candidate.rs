//! `arc candidate`: register alternative answers to a brief, show and list
//! them, judge them, and retire their pins.
//!
//! Every write holds the repository-events lock from the ledger read to the
//! append, so the rules are judged against the ledger the event joins.

pub use crate::candidate::Ledger;
use crate::candidate::{self, contract_label, Received, Registration};
use crate::custody::{self, Content};
use crate::forge_observation::RetainedContentRef;
use crate::gitio;
use crate::ids;
use crate::model::{
    CandidateBriefRef, CandidateJudgement, CaptureState, DeclaredRelation, DeclaredTarget,
    InferredBlob, Payload, ReadArtifact, ReadCoverage, RelationSubject, VerifyResult,
};
use crate::received_evidence::{
    self, Applicability, Command, Environment, Omission, Provenance, ReceivedEvidence,
};
use crate::relations::Relations;
use crate::rewrite::{Fate, RewriteMap};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use super::Ctx;

pub const CANDIDATE_SCHEMA: &str = "arc-candidate/1";

pub struct RegisterArgs {
    pub tree: String,
    pub brief: String,
    pub producers: Vec<String>,
    pub parents: Vec<String>,
    pub adopts: Vec<String>,
    pub episodes: Vec<String>,
    pub id: Option<String>,
    pub evidence_file: Option<String>,
    pub custody_dir: Option<String>,
}

/// The candidate ledger as the repository holds it.
pub fn load_ledger(store: &Store) -> Result<Ledger> {
    let events = store.load_repository_events()?;
    Ledger::replay(&events).context("the repository's candidate events do not replay")
}

pub fn register(ctx: &Ctx, args: RegisterArgs) -> Result<()> {
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let ledger = load_ledger(&store)?;

    let (brief, change_slug) = resolve_brief(ctx, &store, &args.brief)?;
    let explicit_id = args.id.is_some();
    let candidate_id = match args.id {
        Some(id) => {
            ids::validate_id_component(&id)?;
            id
        }
        None => ids::new_change_id(&format!("{change_slug}-candidate")),
    };
    let Some(tree) = gitio::resolve_tree(&ctx.cwd, &args.tree)? else {
        bail!(
            "unknown-tree: {:?} names no tree or commit this object store holds",
            args.tree
        );
    };
    let claims = claims_on(&store, &brief.change_id)?;
    for episode in &args.episodes {
        if !claims.contains(episode) {
            bail!(
                "unknown-episode: {episode} is not a claim recorded on change {}",
                brief.change_id
            );
        }
    }

    let received = args
        .evidence_file
        .as_deref()
        .map(|path| read_evidence(&ctx.cwd, path))
        .transpose()?;
    let materialization = match (&received, args.custody_dir.as_deref()) {
        (Some(evidence), Some(dir)) => {
            let wanted = evidence.receipts.iter().map(Content::from).collect();
            Some(
                custody::plan_materialization(std::path::Path::new(dir), &wanted).with_context(
                    || format!("custody directory {dir} refused; nothing was recorded"),
                )?,
            )
        }
        _ => None,
    };

    let mut event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::CandidateRegistered {
            candidate_id: candidate_id.clone(),
            tree: tree.clone(),
            brief: brief.clone(),
            producers: distinct(args.producers),
            parents: distinct(args.parents),
            adopts: distinct(args.adopts),
            episodes: distinct(args.episodes),
            requested_evidence_sha256: received
                .as_ref()
                .map(ReceivedEvidence::sha256)
                .transpose()?,
        },
    )?;
    let mut evidence_event = received
        .map(|evidence| {
            ctx.event(
                &store,
                Store::REPOSITORY_SCOPE,
                Payload::CandidateEvidenceReceived {
                    candidate_id: candidate_id.clone(),
                    evidence,
                },
            )
        })
        .transpose()?;
    // An explicit identity makes a partial publication recoverable. Only the
    // same registration and declarant can finish it.
    let held_events = store.load_repository_events()?;
    let existing = held_events.iter().find(|held| matches!(
        &held.payload, Payload::CandidateRegistered { candidate_id: id, .. } if id == &candidate_id
    ));
    let declarant = |event: &crate::model::Event| {
        event
            .on_behalf_of
            .clone()
            .unwrap_or_else(|| event.actor.clone())
    };
    let mut ledger = ledger;
    if let Some(held) = existing {
        let Payload::CandidateRegistered {
            requested_evidence_sha256,
            ..
        } = &held.payload
        else {
            unreachable!();
        };
        if evidence_event.is_some() && requested_evidence_sha256.is_none() {
            bail!(
                "evidence-not-requested: {candidate_id} has no original requested-evidence intent"
            );
        }
        if !explicit_id
            || evidence_event.is_none()
            || ledger.retirement(&candidate_id).is_some()
            || serde_json::to_value(&held.payload)? != serde_json::to_value(&event.payload)?
            || declarant(held) != declarant(&event)
        {
            bail!("duplicate-candidate: {candidate_id} is not a matching active evidence retry");
        }
    } else {
        ledger.record(&event)?;
    }
    let held_evidence = ledger.received_by(&candidate_id);
    let evidence_id = held_evidence.map(|received| received.event_id.clone());
    if let Some(requested) = &evidence_event {
        if let Some(held) = held_evidence {
            let Payload::CandidateEvidenceReceived { evidence, .. } = &requested.payload else {
                unreachable!();
            };
            if &held.evidence != evidence || held.declarant != declarant(requested) {
                bail!("evidence-already-received: {candidate_id} holds different evidence or declarant");
            }
        } else {
            ledger.record(requested)?;
        }
    }
    ctx.ensure_declared_actor(&store)?;

    // Pinned before it is recorded: an interrupted registration leaves a pin
    // `arc doctor` names, never a record of content Git may collect.
    let reference = candidate::candidate_ref(&candidate_id);
    gitio::update_ref(&ctx.cwd, &reference, &tree)?;
    // Verified bytes land before the event that cites them.
    if let Some(materialization) = &materialization {
        materialization.publish(&custody::Custody::at(&store.root))?;
    }
    if let Some(evidence) = ledger.received_by(&candidate_id) {
        for (_, head) in evidence.evidence.heads.each() {
            if gitio::object_type(&ctx.cwd, &head.commit)?.as_deref() == Some("commit") {
                gitio::update_ref(
                    &ctx.cwd,
                    &gitio::retained_ref(&brief.change_id, &candidate_id, &head.commit),
                    &head.commit,
                )?;
            }
        }
    }
    #[cfg(debug_assertions)]
    registration_rendezvous("pinned")?;
    if existing.is_none() {
        store.append_local_repository_event(&mut event)?;
    }
    if let Some(evidence) = evidence_event.as_mut().filter(|_| evidence_id.is_none()) {
        (|| {
            #[cfg(debug_assertions)]
            registration_rendezvous("registered")?;
            store.append_local_repository_event(evidence)
        })()
        .with_context(|| {
            format!(
                "candidate {candidate_id} is registered (event {}) but its received \
                     evidence was not recorded",
                existing
                    .map(|held| &held.event_id)
                    .unwrap_or(&event.event_id)
            )
        })?;
    }
    println!("candidate: {candidate_id}");
    println!("tree: {tree}");
    println!("brief: {}", contract_label(&brief));
    println!("pinned: {reference}");
    println!(
        "event: {}",
        existing
            .map(|held| &held.event_id)
            .unwrap_or(&event.event_id)
    );
    if let Some(evidence) = &evidence_event {
        println!(
            "received evidence: {} (external)",
            evidence_id.as_ref().unwrap_or(&evidence.event_id)
        );
    }
    Ok(())
}

#[cfg(debug_assertions)]
fn registration_rendezvous(phase: &str) -> Result<()> {
    use std::io::{Read, Write};
    if std::env::var("ARC_TEST_REGISTRATION_RENDEZVOUS").as_deref() != Ok(phase) {
        return Ok(());
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "arc test: registration {phase}")?;
    stderr.flush()?;
    drop(stderr);
    let mut release = [0];
    std::io::stdin().read_exact(&mut release)?;
    anyhow::ensure!(release == *b"+", "invalid registration rendezvous release");
    Ok(())
}

/// A received-evidence document read from `path` and judged by every rule
/// that needs only the document and the object store: its own consistency,
/// and the tree of each commit this repository holds.
fn read_evidence(cwd: &std::path::Path, path: &str) -> Result<ReceivedEvidence> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read evidence file {path}"))?;
    let evidence: ReceivedEvidence = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "malformed-evidence: {path} is not an {} document",
            received_evidence::SCHEMA
        )
    })?;
    evidence
        .validate()
        .map_err(|error| anyhow::anyhow!("malformed-evidence: {error}"))?;
    evidence.validate_held_heads(cwd)?;
    Ok(evidence)
}

/// Names in the order given, each once: a flag repeated names one member.
fn distinct(names: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    names
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

/// A brief reference as `<change>[@<brief-event>]`, resolved to one recorded
/// brief version and the digest of its body. The latest version answers when
/// none is named.
fn resolve_brief(ctx: &Ctx, store: &Store, raw: &str) -> Result<(CandidateBriefRef, String)> {
    let (change, version) = match raw.split_once('@') {
        Some((change, version)) => (change, Some(version)),
        None => (raw, None),
    };
    let change_id = store
        .resolve_change(change)
        .map_err(|error| anyhow::anyhow!("unknown-brief: {raw:?} names no change: {error:#}"))?;
    let (_, state) = ctx.load_state(store, &change_id)?;
    let brief = match version {
        None => state.briefs.last(),
        Some(version) => {
            let matches: Vec<_> = state
                .briefs
                .iter()
                .filter(|brief| brief.event_id == version || brief.event_id.starts_with(version))
                .collect();
            match matches.as_slice() {
                [one] => Some(*one),
                [] => None,
                _ => match matches
                    .iter()
                    .copied()
                    .find(|brief| brief.event_id == version)
                {
                    Some(exact) => Some(exact),
                    None => bail!(
                        "unknown-brief: {version:?} is ambiguous on change {change_id}: matches {}",
                        matches
                            .iter()
                            .map(|brief| brief.event_id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
            }
        }
    };
    let Some(brief) = brief else {
        bail!("unknown-brief: {raw:?} resolves to no brief recorded on change {change_id}");
    };
    Ok((
        CandidateBriefRef {
            change_id: change_id.clone(),
            brief_event_id: brief.event_id.clone(),
            digest: format!(
                "sha256:{}",
                hex::encode(Sha256::digest(brief.body.as_bytes()))
            ),
        },
        state.slug.clone(),
    ))
}

/// Every claim recorded on a change: the episodes its work ran under.
pub(crate) fn claims_on(store: &Store, change_id: &str) -> Result<BTreeSet<String>> {
    Ok(store
        .load_events(change_id)?
        .into_iter()
        .filter_map(|event| match event.payload {
            Payload::ClaimSet { claim_id, .. } => Some(claim_id),
            _ => None,
        })
        .collect())
}

pub fn judge(
    ctx: &Ctx,
    candidate_id: String,
    superseded_by: Option<String>,
    reason: String,
) -> Result<()> {
    if reason.trim().is_empty() {
        bail!("--reason must not be empty");
    }
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let mut ledger = load_ledger(&store)?;
    let judgement = match superseded_by {
        Some(other) => CandidateJudgement::SupersededBy {
            candidate_id: other,
        },
        None => CandidateJudgement::Rejected,
    };
    ledger.check_judgement(&candidate_id, &judgement)?;
    let mut event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::CandidateJudged {
            candidate_id: candidate_id.clone(),
            judgement: judgement.clone(),
            reason,
        },
    )?;
    ledger.record(&event)?;
    store.append_local_repository_event(&mut event)?;
    match judgement {
        CandidateJudgement::Rejected => println!("judged: {candidate_id} rejected"),
        CandidateJudgement::SupersededBy {
            candidate_id: other,
        } => {
            println!("judged: {candidate_id} superseded by {other}")
        }
    }
    println!("event: {}", event.event_id);
    Ok(())
}

pub fn retire(ctx: &Ctx, candidate_id: String) -> Result<()> {
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let mut ledger = load_ledger(&store)?;
    if ledger.registration(&candidate_id).is_none() {
        bail!("unknown-candidate: no candidate {candidate_id} is registered");
    }
    if let Some(retired) = ledger.retirement(&candidate_id) {
        println!(
            "candidate {candidate_id} is already retired (event {})",
            retired.event_id
        );
        return Ok(());
    }
    let roots = ledger.roots_reaching(&candidate_id);
    if !roots.is_empty() {
        bail!(
            "rooted-candidate: {candidate_id} is reached by {}; its pin stays",
            roots
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let mut event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::CandidateRetired {
            candidate_id: candidate_id.clone(),
        },
    )?;
    ledger.record(&event)?;
    ctx.ensure_declared_actor(&store)?;
    // The pin goes first: an interrupted retirement leaves an unpinned
    // registration that running `retire` again records.
    let reference = candidate::candidate_ref(&candidate_id);
    let held = gitio::ref_value(&ctx.cwd, &reference)?;
    if held.is_some() {
        gitio::delete_ref(&ctx.cwd, &reference)?;
    }
    store.append_local_repository_event(&mut event)?;
    println!("retired: {candidate_id}");
    match held {
        Some(_) => println!("unpinned: {reference}"),
        None => println!("unpinned: {reference} was already absent"),
    }
    println!("event: {}", event.event_id);
    Ok(())
}

#[derive(Serialize)]
struct CandidateDocument {
    schema: &'static str,
    candidates: Vec<CandidateView>,
    shared_trees: Vec<SharedTreeView>,
}

#[derive(Serialize)]
struct CandidateView {
    candidate_id: String,
    tree: String,
    brief: CandidateBriefRef,
    producers: Vec<String>,
    parents: Vec<String>,
    adopts: Vec<String>,
    episodes: Vec<String>,
    event_id: String,
    registered_by: String,
    registered_at: DateTime<Utc>,
    judgements: Vec<JudgementView>,
    retired: Option<RetirementView>,
    pin: PinView,
    relations: RelationsView,
    /// What the candidate received from an external execution, when it did.
    received_evidence: Option<ReceivedView>,
    evaluations: Vec<EvaluationView>,
    selections: Vec<SelectionView>,
    /// Promotion refs of this candidate no `candidate-promoted` event
    /// records.
    interrupted_promotions: Vec<InterruptedView>,
}

/// Evidence a candidate received from outside this ledger. It is the
/// sender's claim, labelled by its origin; it is no evaluation and no gate
/// reads it as one.
#[derive(Serialize)]
struct ReceivedView {
    origin: &'static str,
    event_id: String,
    recorded_by: String,
    recorded_at: DateTime<Utc>,
    schema: String,
    run_handle: String,
    packet: String,
    heads: HeadsView,
    /// Claims this repository's own objects contradict. Empty while every
    /// named commit is absent or agrees.
    diagnostics: Vec<DiagnosticView>,
    /// Whether the tree the run reports having tested is the registered tree.
    /// A claim about a tree, never an evaluation of it.
    tested_tree_is_registered_tree: bool,
    provenance: Vec<Provenance>,
    receipts: Vec<ReceiptView>,
    commands: Vec<Command>,
    environment: Option<Environment>,
    applicability: Applicability,
    omissions: Vec<Omission>,
}

/// One claimed head the objects held here contradict. Object IDs and kinds
/// only.
#[derive(Serialize)]
pub(crate) struct DiagnosticView {
    pub(crate) code: &'static str,
    role: &'static str,
    commit: String,
    pub(crate) detail: String,
}

/// The diagnostics of one evidence document. A repository that cannot be
/// read reports the one it cannot judge, without the cause.
pub(crate) fn held_diagnostics(
    cwd: &std::path::Path,
    evidence: &ReceivedEvidence,
) -> Vec<DiagnosticView> {
    match evidence.head_mismatches(cwd) {
        Ok(mismatches) => mismatches
            .into_iter()
            .map(|mismatch| DiagnosticView {
                code: mismatch.code(),
                role: mismatch.role.as_str(),
                commit: mismatch.commit.clone(),
                detail: mismatch.detail(),
            })
            .collect(),
        Err(_) => vec![DiagnosticView {
            code: "evidence-heads-unverifiable",
            role: "all",
            commit: String::new(),
            detail: "the held objects could not be read to judge the claimed heads".to_string(),
        }],
    }
}

#[derive(Serialize)]
struct HeadsView {
    source: HeadView,
    tested: HeadView,
    report: HeadView,
}

/// A head as the run reported it, beside what a recorded rewrite makes of
/// the commit here: itself when none touched it, its successor, or null
/// when a rewrite dropped it.
#[derive(Serialize)]
struct HeadView {
    commit: String,
    tree: String,
    resolved_commit: Option<String>,
}

#[derive(Serialize)]
struct ReceiptView {
    #[serde(flatten)]
    reference: RetainedContentRef,
    /// `available`, `unavailable` or `corrupt`, read from custody now.
    availability: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct EvaluationView {
    event_id: String,
    gate: String,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
    result: VerifyResult,
    recorded_by: String,
    recorded_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct SelectionView {
    event_id: String,
    destination: String,
    /// `promoted`, `superseded`, or `unpromoted`.
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    patchset_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    promotion_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
    /// The revision the promotion moved the destination to.
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    target: String,
    reuse: crate::policy::EvaluationReuse,
    evaluations: Vec<crate::model::SelectedEvaluation>,
    reads: Vec<crate::model::SelectedRead>,
    contributors: Vec<String>,
    selector: String,
    rationale: String,
    recorded_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct InterruptedView {
    reference: String,
    selection: String,
    commit: String,
}

/// A candidate's read records and declarations. A read carries its record,
/// a declaration its declarant's claim; neither stands in for the other.
#[derive(Serialize)]
struct RelationsView {
    reads: Vec<ReadView>,
    declarations: Vec<DeclarationView>,
}

#[derive(Serialize)]
struct ReadView {
    record: String,
    episode: String,
    path: String,
    digest: String,
    coverage: ReadCoverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    compared_at: Option<String>,
    /// Inferred: the returned bytes equal this blob's at `compared_at`.
    #[serde(skip_serializing_if = "Option::is_none")]
    blob: Option<InferredBlob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<ReadArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    /// The standing capture report for the recording, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    capture: Option<CaptureState>,
    /// Unless the latest capture report is `pinned`.
    at_risk: bool,
    recorded_by: String,
    event_id: String,
    recorded_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct DeclarationView {
    relation: DeclaredRelation,
    target: DeclaredTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    citation: Option<String>,
    declarant: String,
    event_id: String,
    recorded_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct JudgementView {
    #[serde(flatten)]
    judgement: CandidateJudgement,
    reason: String,
    declarant: String,
    event_id: String,
    recorded_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct RetirementView {
    event_id: String,
    declarant: String,
    recorded_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct PinView {
    reference: String,
    /// Whether the ref holds the registered tree.
    present: bool,
    /// What the ref holds, when it exists.
    value: Option<String>,
}

#[derive(Serialize)]
struct SharedTreeView {
    tree: String,
    candidates: Vec<String>,
}

pub fn show(ctx: &Ctx, candidate_id: &str, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let ledger = load_ledger(&store)?;
    let Some(registration) = ledger.registration(candidate_id) else {
        bail!("unknown-candidate: no candidate {candidate_id} is registered");
    };
    emit(ctx, &ledger, vec![registration], json)
}

pub fn list(ctx: &Ctx, brief: Option<&str>, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let ledger = load_ledger(&store)?;
    // A bundle carries candidates for changes this store may not hold, so an
    // exact id the store cannot resolve still filters.
    let change = brief.map(|reference| {
        store
            .resolve_change(reference)
            .unwrap_or_else(|_| reference.to_string())
    });
    let selected = ledger
        .registrations()
        .filter(|registration| {
            change
                .as_deref()
                .is_none_or(|change| registration.brief.change_id == change)
        })
        .collect();
    emit(ctx, &ledger, selected, json)
}

fn emit(ctx: &Ctx, ledger: &Ledger, selected: Vec<&Registration>, json: bool) -> Result<()> {
    let relations = super::relations::repository_relations(&ctx.store()?)?;
    let pins: BTreeMap<String, String> =
        gitio::list_refs(&ctx.cwd, candidate::CANDIDATE_REF_PREFIX)?
            .into_iter()
            .map(|(name, value)| {
                (
                    name.trim_start_matches(candidate::CANDIDATE_REF_PREFIX)
                        .to_string(),
                    value,
                )
            })
            .collect();
    let trees: BTreeSet<&str> = selected.iter().map(|r| r.tree.as_str()).collect();
    let shared_trees = ledger
        .shared_trees()
        .into_iter()
        .filter(|(tree, _)| trees.contains(tree))
        .map(|(tree, candidates)| SharedTreeView {
            tree: tree.to_string(),
            candidates: candidates.into_iter().map(str::to_string).collect(),
        })
        .collect::<Vec<_>>();
    let interrupted = super::selection::interrupted_promotions(&ctx.cwd, ledger)?;
    let store = ctx.store()?;
    let custody = custody::Custody::at(&store.root);
    // A rewrite map is read only for evidence that names commits to resolve.
    let rewrites = if selected
        .iter()
        .any(|registration| ledger.received_by(&registration.candidate_id).is_some())
    {
        Some(RewriteMap::load(&store).context("the recorded rewrites do not load")?)
    } else {
        None
    };
    let candidates = selected
        .into_iter()
        .map(|registration| {
            view(
                ledger,
                &relations,
                registration,
                &pins,
                &interrupted,
                (rewrites.as_ref(), &custody, ctx.cwd.as_path()),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let document = CandidateDocument {
        schema: CANDIDATE_SCHEMA,
        candidates,
        shared_trees,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
        render(&document);
    }
    Ok(())
}

fn view(
    ledger: &Ledger,
    relations: &Relations,
    registration: &Registration,
    pins: &BTreeMap<String, String>,
    interrupted: &[super::selection::Interrupted],
    (rewrites, custody, cwd): (Option<&RewriteMap>, &custody::Custody, &std::path::Path),
) -> Result<CandidateView> {
    let value = pins.get(&registration.candidate_id).cloned();
    let id = registration.candidate_id.as_str();
    let received_evidence = ledger
        .received_by(id)
        .map(|received| received_view(received, registration, (rewrites, custody, cwd)))
        .transpose()?;
    Ok(CandidateView {
        candidate_id: registration.candidate_id.clone(),
        tree: registration.tree.clone(),
        brief: registration.brief.clone(),
        producers: registration.producers.clone(),
        parents: registration.parents.clone(),
        adopts: registration.adopts.clone(),
        episodes: registration.episodes.clone(),
        event_id: registration.event_id.clone(),
        registered_by: registration.declarant.clone(),
        registered_at: registration.recorded_at,
        judgements: ledger
            .judgements_of(&registration.candidate_id)
            .into_iter()
            .map(|judgement| JudgementView {
                judgement: judgement.judgement.clone(),
                reason: judgement.reason.clone(),
                declarant: judgement.declarant.clone(),
                event_id: judgement.event_id.clone(),
                recorded_at: judgement.recorded_at,
            })
            .collect(),
        retired: ledger
            .retirement(&registration.candidate_id)
            .map(|retired| RetirementView {
                event_id: retired.event_id.clone(),
                declarant: retired.declarant.clone(),
                recorded_at: retired.recorded_at,
            }),
        pin: PinView {
            reference: candidate::candidate_ref(&registration.candidate_id),
            present: value.as_deref() == Some(registration.tree.as_str()),
            value,
        },
        relations: relations_view(relations, &registration.candidate_id),
        received_evidence,
        evaluations: ledger
            .evaluations_of(id)
            .into_iter()
            .map(|evaluation| EvaluationView {
                event_id: evaluation.event_id.clone(),
                gate: evaluation.gate.clone(),
                command: evaluation.command.clone(),
                environment: evaluation
                    .environment
                    .as_ref()
                    .map(|environment| environment.identity.clone()),
                result: evaluation.result,
                recorded_by: evaluation.declarant.clone(),
                recorded_at: evaluation.recorded_at,
            })
            .collect(),
        selections: ledger
            .selections()
            .filter(|selection| selection.candidate_id == id)
            .map(|selection| {
                let promotion = ledger.promotion_of(&selection.event_id);
                let superseded_by = ledger
                    .superseded_by(&selection.event_id)
                    .map(|later| later.event_id.clone());
                SelectionView {
                    event_id: selection.event_id.clone(),
                    destination: selection.change_id.clone(),
                    status: match (promotion, &superseded_by) {
                        (Some(_), _) => "promoted",
                        (None, Some(_)) => "superseded",
                        (None, None) => "unpromoted",
                    },
                    patchset_id: promotion.map(|promotion| promotion.patchset_id.clone()),
                    promotion_event_id: promotion.map(|promotion| promotion.event_id.clone()),
                    superseded_by,
                    revision: promotion.map(|promotion| promotion.revision.clone()),
                    target: selection.target.clone(),
                    reuse: selection.reuse,
                    evaluations: selection.evaluations.clone(),
                    reads: selection.reads.clone(),
                    contributors: selection.contributors.clone(),
                    selector: selection.selector.clone(),
                    rationale: selection.rationale.clone(),
                    recorded_at: selection.recorded_at,
                }
            })
            .collect(),
        interrupted_promotions: interrupted
            .iter()
            .filter(|interrupted| interrupted.candidate_id == id)
            .map(|interrupted| InterruptedView {
                reference: interrupted.reference.clone(),
                selection: interrupted.selection.clone(),
                commit: interrupted.commit.clone(),
            })
            .collect(),
    })
}

fn received_view(
    received: &Received,
    registration: &Registration,
    (rewrites, custody, cwd): (Option<&RewriteMap>, &custody::Custody, &std::path::Path),
) -> Result<ReceivedView> {
    let evidence = &received.evidence;
    let head = |head: &received_evidence::Head| -> Result<HeadView> {
        let resolved_commit = match rewrites.map(|map| map.fate(&head.commit)).transpose()? {
            Some(Some(Fate::Dropped)) => None,
            Some(Some(Fate::Rewritten(successor))) => Some(successor),
            Some(None) | None => Some(head.commit.clone()),
        };
        Ok(HeadView {
            commit: head.commit.clone(),
            tree: head.tree.clone(),
            resolved_commit,
        })
    };
    Ok(ReceivedView {
        origin: "external",
        event_id: received.event_id.clone(),
        recorded_by: received.declarant.clone(),
        recorded_at: received.recorded_at,
        schema: evidence.schema.clone(),
        run_handle: evidence.run_handle.clone(),
        packet: evidence.packet.clone(),
        heads: HeadsView {
            source: head(&evidence.heads.source)?,
            tested: head(&evidence.heads.tested)?,
            report: head(&evidence.heads.report)?,
        },
        diagnostics: held_diagnostics(cwd, evidence),
        tested_tree_is_registered_tree: evidence.heads.tested.tree == registration.tree,
        provenance: evidence.provenance.clone(),
        receipts: evidence
            .receipts
            .iter()
            .map(|reference| {
                let availability = custody.availability(&Content::from(reference));
                ReceiptView {
                    reference: reference.clone(),
                    availability: availability.label(),
                    detail: match &availability {
                        custody::Availability::Corrupt(reason) => Some(reason.clone()),
                        _ => None,
                    },
                }
            })
            .collect(),
        commands: evidence.commands.clone(),
        environment: evidence.environment.clone(),
        applicability: evidence.applicability.clone(),
        omissions: evidence.omissions.clone(),
    })
}

fn relations_view(relations: &Relations, candidate_id: &str) -> RelationsView {
    let subject = RelationSubject::Candidate {
        candidate_id: candidate_id.to_string(),
    };
    RelationsView {
        reads: relations
            .reads_of(&subject)
            .map(|read| {
                let capture = relations.capture_of(read).map(|capture| capture.capture);
                ReadView {
                    record: read.record.clone(),
                    episode: read.episode.clone(),
                    path: read.path.clone(),
                    digest: read.digest.clone(),
                    coverage: read.coverage,
                    compared_at: read.compared_at.clone(),
                    blob: read.blob.clone(),
                    artifact: read.artifact.clone(),
                    source: read.source.clone(),
                    capture,
                    at_risk: capture != Some(CaptureState::Pinned),
                    recorded_by: read.declarant.clone(),
                    event_id: read.event_id.clone(),
                    recorded_at: read.recorded_at,
                }
            })
            .collect(),
        declarations: relations
            .declarations_of(&subject)
            .map(|declaration| DeclarationView {
                relation: declaration.relation,
                target: declaration.target.clone(),
                citation: declaration.citation.clone(),
                declarant: declaration.declarant.clone(),
                event_id: declaration.event_id.clone(),
                recorded_at: declaration.recorded_at,
            })
            .collect(),
    }
}

fn render(document: &CandidateDocument) {
    if document.candidates.is_empty() {
        println!("no candidates");
        return;
    }
    for candidate in &document.candidates {
        println!("candidate {}", candidate.candidate_id);
        println!("  tree: {}", candidate.tree);
        println!(
            "  brief: {} ({})",
            contract_label(&candidate.brief),
            candidate.brief.digest
        );
        println!("  producers: {}", candidate.producers.join(", "));
        for (label, names) in [
            ("parents", &candidate.parents),
            ("adopts", &candidate.adopts),
            ("episodes", &candidate.episodes),
        ] {
            if !names.is_empty() {
                println!("  {label}: {}", names.join(", "));
            }
        }
        println!(
            "  registered: {} by {} (event {})",
            candidate.registered_at.to_rfc3339(),
            candidate.registered_by,
            candidate.event_id
        );
        for judgement in &candidate.judgements {
            let judged = match &judgement.judgement {
                CandidateJudgement::Rejected => "rejected".to_string(),
                CandidateJudgement::SupersededBy { candidate_id } => {
                    format!("superseded by {candidate_id}")
                }
            };
            println!(
                "  judged: {judged} by {}: {}",
                judgement.declarant, judgement.reason
            );
        }
        if let Some(retired) = &candidate.retired {
            println!(
                "  retired: by {} (event {})",
                retired.declarant, retired.event_id
            );
        }
        let pin = match (&candidate.pin.value, candidate.pin.present) {
            (_, true) => "present".to_string(),
            (Some(other), false) => format!("holds {other}, not the registered tree"),
            (None, false) => "absent".to_string(),
        };
        println!("  pin: {} {pin}", candidate.pin.reference);
        for read in &candidate.relations.reads {
            let blob = match (&read.blob, &read.compared_at) {
                (Some(blob), _) => format!(
                    "; blob {} at {} (inferred: {})",
                    blob.blob, blob.revision, blob.inference
                ),
                (None, Some(revision)) => format!("; no blob: bytes differ from {revision}"),
                (None, None) => String::new(),
            };
            let capture = match read.capture {
                Some(CaptureState::Pinned) => "recording pinned",
                Some(CaptureState::Unpinned) => "at risk: recording reported unpinned",
                None => "at risk: no capture report pins the recording",
            };
            println!(
                "  read [recorded]: `{}` of {} ({}), {}, episode {}{blob}; {capture}",
                read.record, read.path, read.coverage, read.digest, read.episode
            );
        }
        if let Some(received) = &candidate.received_evidence {
            render_received(received);
        }
        for evaluation in &candidate.evaluations {
            let result = match evaluation.result {
                VerifyResult::Pass => "pass",
                _ => "fail",
            };
            let environment = evaluation
                .environment
                .as_deref()
                .map(|identity| format!(" in {identity}"))
                .unwrap_or_default();
            println!(
                "  evaluation {}: gate {} {result}{environment}",
                evaluation.event_id, evaluation.gate
            );
        }
        for selection in &candidate.selections {
            let status = match (&selection.patchset_id, &selection.superseded_by) {
                (Some(patchset), _) => format!("promoted as {patchset}"),
                (None, Some(later)) => format!("superseded by {later}"),
                (None, None) => "not promoted".to_string(),
            };
            println!(
                "  selection {} into {} by {}: {status}",
                selection.event_id, selection.destination, selection.selector
            );
        }
        for interrupted in &candidate.interrupted_promotions {
            println!(
                "  interrupted promotion: {} holds {}; `arc candidate promote {}` completes or \
                 discards it",
                interrupted.reference, interrupted.commit, interrupted.selection
            );
        }
        for declaration in &candidate.relations.declarations {
            println!(
                "  {} [declared by {}]: {}{}",
                declaration.relation.as_str(),
                declaration.declarant,
                declaration.target,
                declaration
                    .citation
                    .as_deref()
                    .map(|citation| format!(", citing read `{citation}`"))
                    .unwrap_or_default()
            );
        }
    }
    for shared in &document.shared_trees {
        println!(
            "shared tree {}: {}",
            shared.tree,
            shared.candidates.join(", ")
        );
    }
}

fn render_received(received: &ReceivedView) {
    println!(
        "  received evidence [{}]: recorded {} by {} (event {}); a claim, not an evaluation",
        received.origin,
        received.recorded_at.to_rfc3339(),
        received.recorded_by,
        received.event_id
    );
    println!(
        "    run handle (private): {}; packet: {}",
        received.run_handle, received.packet
    );
    for (role, head) in [
        ("source", &received.heads.source),
        ("tested", &received.heads.tested),
        ("report", &received.heads.report),
    ] {
        let resolved = match &head.resolved_commit {
            Some(commit) if *commit == head.commit => String::new(),
            Some(commit) => format!("; resolves here to {commit}"),
            None => "; dropped by a recorded rewrite".to_string(),
        };
        println!(
            "    {role} head: {} (tree {}){resolved}",
            head.commit, head.tree
        );
    }
    for diagnostic in &received.diagnostics {
        println!("    diagnostic {}: {}", diagnostic.code, diagnostic.detail);
    }
    println!(
        "    applies to: {} head; the tested tree is {} the registered tree",
        received.applicability.applies_to.as_str(),
        if received.tested_tree_is_registered_tree {
            "also"
        } else {
            "not"
        }
    );
    if let Some(note) = &received.applicability.note {
        println!("    applicability note: {note}");
    }
    for entry in &received.provenance {
        let mut seen = Vec::new();
        if let Some(native) = &entry.native {
            seen.push(format!(
                "native {}/{} [{} {}]",
                native.provider, native.identity, native.source.kind, native.source.reference
            ));
        }
        if let Some(model) = &entry.model {
            seen.push(format!(
                "model {}{} [{} {}]",
                model.name,
                model
                    .effort
                    .as_deref()
                    .map(|effort| format!(" {effort}"))
                    .unwrap_or_default(),
                model.source.kind,
                model.source.reference
            ));
        }
        for (label, account) in [
            ("forge account", &entry.forge_account),
            ("review sender", &entry.review_sender),
        ] {
            if let Some(account) = account {
                seen.push(format!(
                    "{label} {} at {} ({})",
                    account.account, account.host, account.provider
                ));
            }
        }
        for (label, identity) in [
            ("git author", &entry.git_author),
            ("git committer", &entry.git_committer),
        ] {
            if let Some(identity) = identity {
                seen.push(format!("{label} {} <{}>", identity.name, identity.email));
            }
        }
        println!(
            "    provenance {}: {}",
            entry.role.as_str(),
            seen.join("; ")
        );
    }
    for (index, receipt) in received.receipts.iter().enumerate() {
        let reference = &receipt.reference;
        println!(
            "    receipt {index}: {} {}, sha256 {}, {} bytes, {}, reported {}; {}{}",
            reference.provider,
            reference.handle,
            reference.sha256,
            reference.byte_length,
            reference.media_type,
            reference.capture.as_str(),
            receipt.availability,
            receipt
                .detail
                .as_deref()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        );
    }
    for (index, command) in received.commands.iter().enumerate() {
        println!(
            "    command {index}: `{}` reported {}{}{}",
            command.command,
            command.result.as_str(),
            command
                .exit_code
                .map(|code| format!(" (exit {code})"))
                .unwrap_or_default(),
            command
                .receipt
                .map(|receipt| format!(", receipt {receipt}"))
                .unwrap_or_default()
        );
    }
    if let Some(environment) = &received.environment {
        println!(
            "    environment: {}{}",
            environment.identity,
            environment
                .detail
                .as_deref()
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default()
        );
    }
    for omission in &received.omissions {
        println!("    omitted: {}: {}", omission.what, omission.reason);
    }
}

/// Pin each registration an import brought whose tree this object store
/// holds; one whose tree is absent stays unpinned, which is no refusal.
/// Returns what was pinned and what stayed absent.
pub fn pin_imported(
    cwd: &std::path::Path,
    store: &Store,
    imported: &BTreeSet<String>,
) -> Result<(Vec<String>, Vec<String>)> {
    let ledger = load_ledger(store)?;
    let mut pinned = Vec::new();
    let mut absent = Vec::new();
    for registration in ledger.registrations() {
        if !imported.contains(&registration.event_id)
            || ledger.retirement(&registration.candidate_id).is_some()
        {
            continue;
        }
        let reference = candidate::candidate_ref(&registration.candidate_id);
        if gitio::ref_value(cwd, &reference)?.is_some() {
            continue;
        }
        if gitio::resolve_tree(cwd, &registration.tree)?.as_deref()
            == Some(registration.tree.as_str())
        {
            gitio::update_ref(cwd, &reference, &registration.tree)?;
            pinned.push(registration.candidate_id.clone());
        } else {
            absent.push(registration.candidate_id.clone());
        }
    }
    Ok((pinned, absent))
}
