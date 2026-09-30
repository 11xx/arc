//! `arc candidate`: register alternative answers to a brief, show and list
//! them, judge them, and retire their pins.
//!
//! Every write holds the repository-events lock from the ledger read to the
//! append, so the rules are judged against the ledger the event joins.

pub use crate::candidate::Ledger;
use crate::candidate::{self, contract_label, Registration};
use crate::gitio;
use crate::ids;
use crate::model::{
    CandidateBriefRef, CandidateJudgement, CaptureState, DeclaredRelation, DeclaredTarget,
    InferredBlob, Payload, ReadArtifact, ReadCoverage, RelationSubject,
};
use crate::relations::Relations;
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

    let event = ctx.event(
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
        },
    );
    // Judged by the same replay a bundle's copy is, so a registration typed
    // here can never be one an import would refuse.
    let mut ledger = ledger;
    ledger.record(&event)?;
    ctx.ensure_declared_actor(&store)?;

    // Pinned before it is recorded: an interrupted registration leaves a pin
    // `arc doctor` names, never a record of content Git may collect.
    let reference = candidate::candidate_ref(&candidate_id);
    gitio::update_ref(&ctx.cwd, &reference, &tree)?;
    store.append_repository_event(&event)?;
    println!("candidate: {candidate_id}");
    println!("tree: {tree}");
    println!("brief: {}", contract_label(&brief));
    println!("pinned: {reference}");
    println!("event: {}", event.event_id);
    Ok(())
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
    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::CandidateJudged {
            candidate_id: candidate_id.clone(),
            judgement: judgement.clone(),
            reason,
        },
    );
    ledger.record(&event)?;
    store.append_repository_event(&event)?;
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
    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::CandidateRetired {
            candidate_id: candidate_id.clone(),
        },
    );
    ledger.record(&event)?;
    ctx.ensure_declared_actor(&store)?;
    // The pin goes first: an interrupted retirement leaves an unpinned
    // registration that running `retire` again records.
    let reference = candidate::candidate_ref(&candidate_id);
    let held = gitio::ref_value(&ctx.cwd, &reference)?;
    if held.is_some() {
        gitio::delete_ref(&ctx.cwd, &reference)?;
    }
    store.append_repository_event(&event)?;
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
    let candidates = selected
        .into_iter()
        .map(|registration| view(ledger, &relations, registration, &pins))
        .collect::<Vec<_>>();
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
) -> CandidateView {
    let value = pins.get(&registration.candidate_id).cloned();
    CandidateView {
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
    }
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
