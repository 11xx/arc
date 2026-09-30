//! `arc context`: record read records, declarations, and capture reports on
//! a change or a candidate.
//!
//! A change subject's relations are events on its own ledger; a candidate
//! subject's are repository events, as the candidate's own are. Every write
//! holds the lock of the ledger it joins from the read to the append, so the
//! rules are judged against the ledger the event joins.

use crate::gitio;
use crate::model::{
    CaptureState, DeclaredRelation, DeclaredTarget, Event, InferredBlob, Payload, ReadArtifact,
    ReadCoverage, RelationSubject, CONTENT_MATCHES_REVISION,
};
use crate::relations::{self, line_range, Relations};
use crate::store::{Store, TransitionLock};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use super::Ctx;

/// The tool record documents `--from-tapes` reads: `tapes events --json`, and
/// the `.json` file of a `tapes export` bundle, which carries the same events.
pub const TAPES_SCHEMAS: [&str; 2] = ["tapes-events/9", "tapes-session/14"];

/// A subject resolved to where its relations live and which change's claims
/// its episodes are.
struct Subject {
    subject: RelationSubject,
    /// The change whose claims are this subject's episodes: the change
    /// itself, or a candidate's brief change.
    claims_change: String,
}

impl Subject {
    fn ledger_scope(&self) -> &str {
        match &self.subject {
            RelationSubject::Change { change_id } => change_id,
            RelationSubject::Candidate { .. } => Store::REPOSITORY_SCOPE,
        }
    }

    fn lock(&self, store: &Store) -> Result<TransitionLock> {
        match &self.subject {
            RelationSubject::Change { change_id } => store.lock_transition(change_id),
            RelationSubject::Candidate { .. } => store.lock_repository_events(),
        }
    }

    fn events(&self, store: &Store) -> Result<Vec<Event>> {
        match &self.subject {
            RelationSubject::Change { change_id } => store.load_events(change_id),
            RelationSubject::Candidate { .. } => store.load_repository_events(),
        }
    }

    fn append(&self, store: &Store, event: &Event) -> Result<()> {
        match &self.subject {
            RelationSubject::Change { .. } => store.append_event(event),
            RelationSubject::Candidate { .. } => store.append_repository_event(event),
        }
    }
}

/// A registered candidate id names a candidate; anything else resolves as a
/// change reference. An exact candidate id that is also an exact change id
/// is ambiguous and refused.
fn resolve_subject(store: &Store, raw: &str) -> Result<Subject> {
    let ledger = super::candidate::load_ledger(store)?;
    let change = store.resolve_change(raw);
    if let Some(registration) = ledger.registration(raw) {
        if matches!(&change, Ok(change_id) if change_id == raw) {
            bail!("ambiguous-subject: {raw:?} is both a candidate id and a change id");
        }
        return Ok(Subject {
            subject: RelationSubject::Candidate {
                candidate_id: raw.to_string(),
            },
            claims_change: registration.brief.change_id.clone(),
        });
    }
    match change {
        Ok(change_id) => Ok(Subject {
            subject: RelationSubject::Change {
                change_id: change_id.clone(),
            },
            claims_change: change_id,
        }),
        Err(error) => bail!("unknown-subject: {raw:?} names no candidate or change: {error:#}"),
    }
}

/// Every relation on the subject's ledger.
fn load_relations(store: &Store, subject: &Subject) -> Result<Relations> {
    Relations::replay(&subject.events(store)?)
        .with_context(|| format!("the relations on {} do not replay", subject.subject.label()))
}

/// The relations recorded on one change's ledger.
pub fn change_relations(events: &[Event]) -> Result<Relations> {
    Relations::replay(events).context("the change's relations do not replay")
}

/// The relations recorded on the repository ledger: every candidate's.
pub fn repository_relations(store: &Store) -> Result<Relations> {
    Relations::replay(&store.load_repository_events()?)
        .context("the repository's relations do not replay")
}

/// An event sorting after every event already on the ledger, so a replay
/// meets a read before anything that cites or reports on it.
fn next_event(
    ctx: &Ctx,
    store: &Store,
    subject: &Subject,
    ledger: &[Event],
    appended: &[Event],
    payload: Payload,
) -> Result<Event> {
    let mut event = ctx.event(store, subject.ledger_scope(), payload);
    let latest = ledger
        .iter()
        .chain(appended)
        .map(|event| event.event_id.as_str())
        .max();
    if let Some(latest) = latest {
        if event.event_id.as_str() <= latest {
            event.event_id = super::event_id_after(latest)?;
        }
    }
    Ok(event)
}

pub struct ReadArgs {
    pub subject: String,
    pub episode: String,
    pub record: String,
    pub path: String,
    pub digest: String,
    pub coverage: ReadCoverage,
    pub at: Option<String>,
}

/// One read as a tool recorded it, before arc infers anything.
struct ToolRead {
    record: String,
    path: String,
    digest: String,
    coverage: ReadCoverage,
}

pub fn read(ctx: &Ctx, args: ReadArgs) -> Result<()> {
    let store = ctx.store()?;
    let subject = resolve_subject(&store, &args.subject)?;
    let _lock = subject.lock(&store)?;
    let events = subject.events(&store)?;
    let mut ledger = load_relations(&store, &subject)?;
    check_episode(&store, &subject, &args.episode)?;
    if !relations::is_digest(&args.digest) {
        bail!(
            "malformed-digest: {:?} is not `sha256:` and 64 hex digits",
            args.digest
        );
    }
    let at = resolve_at(ctx, args.at.as_deref())?;
    let tool_read = ToolRead {
        record: args.record,
        path: args.path,
        digest: args.digest.to_ascii_lowercase(),
        coverage: args.coverage,
    };
    let event = read_event(
        ctx,
        &store,
        &subject,
        &events,
        &[],
        &args.episode,
        tool_read,
        at,
        None,
    )?;
    ledger.record(&event)?;
    ctx.ensure_declared_actor(&store)?;
    subject.append(&store, &event)?;
    print_read(&event);
    Ok(())
}

pub struct TapesArgs {
    pub subject: String,
    pub episode: String,
    pub file: PathBuf,
    pub at: Option<String>,
}

pub fn read_from_tapes(ctx: &Ctx, args: TapesArgs) -> Result<()> {
    let bytes = std::fs::read(&args.file)
        .with_context(|| format!("cannot read {}", args.file.display()))?;
    let document: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not JSON", args.file.display()))?;
    let schema = document["schema"].as_str().unwrap_or_default();
    if !TAPES_SCHEMAS.contains(&schema) {
        bail!(
            "unsupported-tapes-schema: {} is {schema:?}; accepted: {}",
            args.file.display(),
            TAPES_SCHEMAS.join(", ")
        );
    }
    let Some(tool_events) = document["events"].as_array() else {
        bail!(
            "unsupported-tapes-schema: {} carries no `events` array",
            args.file.display()
        );
    };
    let (found, skipped) = tapes_reads(tool_events);

    let store = ctx.store()?;
    let subject = resolve_subject(&store, &args.subject)?;
    let _lock = subject.lock(&store)?;
    let events = subject.events(&store)?;
    let mut ledger = load_relations(&store, &subject)?;
    check_episode(&store, &subject, &args.episode)?;
    let at = resolve_at(ctx, args.at.as_deref())?;
    let source = match document["session"]["id"].as_str() {
        Some(session) => format!("{schema} session {session}"),
        None => schema.to_string(),
    };

    let mut skips = skipped;
    let mut appended = Vec::new();
    for tool_read in found {
        if ledger.read(&subject.subject, &tool_read.record).is_some() {
            skips.push((tool_read.record, "already recorded".to_string()));
            continue;
        }
        let event = read_event(
            ctx,
            &store,
            &subject,
            &events,
            &appended,
            &args.episode,
            tool_read,
            at.clone(),
            Some(source.clone()),
        )?;
        ledger.record(&event)?;
        appended.push(event);
    }
    if !appended.is_empty() {
        ctx.ensure_declared_actor(&store)?;
    }
    for event in &appended {
        subject.append(&store, event)?;
        print_read(event);
    }
    for (event_id, reason) in &skips {
        println!("skipped {event_id}: {reason}");
    }
    println!(
        "reads: {} recorded, {} skipped",
        appended.len(),
        skips.len()
    );
    Ok(())
}

/// Each tool call carrying a `read` member, as a read or as a skip with its
/// reason. A call that failed, or whose paired result failed, is never a
/// read.
fn tapes_reads(events: &[Value]) -> (Vec<ToolRead>, Vec<(String, String)>) {
    let failed = |status: &Value| matches!(status.as_str(), Some("error" | "failed"));
    let mut reads = Vec::new();
    let mut skips = Vec::new();
    for event in events {
        let read = &event["read"];
        if !read.is_object() {
            continue;
        }
        let event_id = event["event_id"]
            .as_str()
            .or_else(|| event["native_id"].as_str())
            .unwrap_or("<unidentified event>")
            .to_string();
        let result = event["pair"]["ordinal"].as_u64().and_then(|ordinal| {
            events
                .iter()
                .find(|other| other["ordinal"].as_u64() == Some(ordinal))
        });
        let skip = if event["event_id"].as_str().is_none() {
            Some("no stable event id")
        } else if read["succeeded"] == Value::Bool(false)
            || failed(&event["status"])
            || result.is_some_and(|result| failed(&result["status"]))
        {
            Some("the call failed")
        } else if read["path"].as_str().is_none_or(|path| path.is_empty()) {
            Some("no path")
        } else if read["sha256"]
            .as_str()
            .is_none_or(|d| !relations::is_digest(d))
        {
            Some("no digest")
        } else {
            None
        };
        if let Some(reason) = skip {
            skips.push((event_id, reason.to_string()));
            continue;
        }
        let lines = &read["lines"];
        let coverage = match (lines["start"].as_u64(), lines["end"].as_u64()) {
            (Some(from), Some(to)) if from >= 1 && to >= from => ReadCoverage::Lines { from, to },
            _ if read["whole"] == Value::Bool(true) => ReadCoverage::Whole,
            _ => ReadCoverage::Unknown,
        };
        reads.push(ToolRead {
            record: event_id,
            path: read["path"].as_str().unwrap_or_default().to_string(),
            digest: read["sha256"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase(),
            coverage,
        });
    }
    (reads, skips)
}

#[allow(clippy::too_many_arguments)]
fn read_event(
    ctx: &Ctx,
    store: &Store,
    subject: &Subject,
    ledger: &[Event],
    appended: &[Event],
    episode: &str,
    tool_read: ToolRead,
    at: Option<String>,
    source: Option<String>,
) -> Result<Event> {
    let blob = match &at {
        Some(revision) => infer_blob(ctx, revision, &tool_read)?,
        None => None,
    };
    let artifact = journal_artifact(ctx, &tool_read.path);
    next_event(
        ctx,
        store,
        subject,
        ledger,
        appended,
        Payload::ContextRead {
            subject: subject.subject.clone(),
            episode: episode.to_string(),
            record: tool_read.record,
            path: tool_read.path,
            digest: tool_read.digest,
            coverage: tool_read.coverage,
            compared_at: at,
            blob,
            artifact,
            source,
        },
    )
}

fn print_read(event: &Event) {
    let Payload::ContextRead {
        record,
        path,
        digest,
        coverage,
        compared_at,
        blob,
        artifact,
        ..
    } = &event.payload
    else {
        return;
    };
    println!("read: {record}");
    println!("  path: {path} ({coverage})");
    println!("  digest: {digest}");
    match (blob, compared_at) {
        (Some(blob), _) => println!(
            "  blob: {} at {} (inferred: {})",
            blob.blob, blob.revision, blob.inference
        ),
        (None, Some(revision)) => println!(
            "  blob: none (the returned bytes do not match {revision}; the read stands on its digest)"
        ),
        (None, None) => {}
    }
    if let Some(artifact) = artifact {
        println!(
            "  artifact: {} ({})",
            artifact.file,
            artifact_match(digest, artifact)
        );
    }
    println!("  event: {}", event.event_id);
}

/// How a read's digest compares with the artifact body it names.
pub fn artifact_match(digest: &str, artifact: &ReadArtifact) -> &'static str {
    match &artifact.body_digest {
        Some(body) if body == digest => "body matches",
        Some(_) => "body differs",
        None => "body unreadable when recorded",
    }
}

fn check_episode(store: &Store, subject: &Subject, episode: &str) -> Result<()> {
    let claims = super::candidate::claims_on(store, &subject.claims_change)?;
    if !claims.contains(episode) {
        bail!(
            "unknown-episode: {episode} is not a claim recorded on change {}",
            subject.claims_change
        );
    }
    Ok(())
}

fn resolve_at(ctx: &Ctx, at: Option<&str>) -> Result<Option<String>> {
    match at {
        None => Ok(None),
        Some(rev) if rev.is_empty() || rev.starts_with('-') => {
            bail!("unknown-revision: {rev:?} names no commit")
        }
        Some(rev) => gitio::rev_parse(&ctx.cwd, rev)
            .map(Some)
            .map_err(|_| anyhow::anyhow!("unknown-revision: {rev:?} names no commit")),
    }
}

/// The blob at `revision` whose bytes, over the read's range, equal the
/// returned bytes. A path the revision does not hold, or bytes that differ,
/// infer nothing.
fn infer_blob(ctx: &Ctx, revision: &str, read: &ToolRead) -> Result<Option<InferredBlob>> {
    let Some(path) = repository_path(&ctx.cwd, &read.path)? else {
        return Ok(None);
    };
    let Some(blob) = gitio::blob_oid(&ctx.cwd, revision, &path) else {
        return Ok(None);
    };
    let bytes = gitio::blob_bytes(&ctx.cwd, &blob)?;
    let covered = match read.coverage {
        ReadCoverage::Whole | ReadCoverage::Unknown => Some(bytes.as_slice()),
        ReadCoverage::Lines { from, to } => line_range(&bytes, from, to),
    };
    let matches = covered.is_some_and(|covered| {
        format!("sha256:{}", hex::encode(Sha256::digest(covered))) == read.digest
    });
    Ok(matches.then(|| InferredBlob {
        revision: revision.to_string(),
        path,
        blob,
        inference: CONTENT_MATCHES_REVISION.to_string(),
    }))
}

/// A read path as a path inside the repository: relative paths are taken
/// from the current directory, and an absolute path must lie inside one of
/// the repository's worktrees. Anything else names no repository path.
pub(crate) fn repository_path(cwd: &Path, raw: &str) -> Result<Option<String>> {
    let path = Path::new(raw);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let absolute = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let mut roots = vec![gitio::toplevel(cwd)?];
    roots.extend(
        gitio::worktree_inventory(cwd)?
            .into_iter()
            .map(|entry| entry.path),
    );
    let relative = roots
        .into_iter()
        .map(|root| std::fs::canonicalize(&root).unwrap_or(root))
        .filter_map(|root| absolute.strip_prefix(&root).ok().map(Path::to_path_buf))
        .min_by_key(|relative| relative.components().count());
    Ok(relative
        .filter(|relative| relative.components().count() > 0)
        .and_then(|relative| relative.to_str().map(|text| text.replace('\\', "/"))))
}

/// The journal artifact a path names, when the path is a file directly in
/// a journal's hot directory or its cold archive under an artifact name. An
/// artifact of this project's journal is named by its file name; one of
/// another project's journal by the qualified `<journal-dir>::<file>`, the
/// hot directory as it resolves on disk.
fn journal_artifact(ctx: &Ctx, raw: &str) -> Option<ReadArtifact> {
    let path = Path::new(raw);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.cwd.join(path)
    };
    let name = absolute.file_name()?.to_str()?.to_string();
    crate::journal::parse_artifact_name(&name)?;
    let canonical = |dir: &Path| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let parent = canonical(absolute.parent()?);
    let own = crate::journal::resolve_dir(&ctx.cwd)
        .ok()
        .map(|hot| canonical(&hot));
    let hot = match &own {
        Some(own) if *own == parent || canonical(&crate::journal::archive_dir(own)) == parent => {
            return Some(ReadArtifact {
                body_digest: crate::journal::artifact_digest(ctx, &name).ok(),
                file: name,
            });
        }
        _ => archived_from(&parent).unwrap_or(parent),
    };
    let reference = format!(
        "{}{}{name}",
        hot.to_str()?,
        crate::journal::REFERENCE_SEPARATOR
    );
    crate::journal::locate_artifact(ctx, &reference).ok()?;
    Some(ReadArtifact {
        body_digest: crate::journal::artifact_digest(ctx, &reference).ok(),
        file: reference,
    })
}

/// The hot journal a cold archive directory belongs to, when `dir` is one.
fn archived_from(dir: &Path) -> Option<PathBuf> {
    let hot = PathBuf::from(dir.to_str()?.strip_suffix("-archive")?);
    (crate::journal::archive_dir(&hot) == dir && hot.is_dir()).then_some(hot)
}

pub struct DeclareArgs {
    pub subject: String,
    pub relation: DeclaredRelation,
    pub path: Option<String>,
    pub at: Option<String>,
    pub artifact: Option<String>,
    pub citation: Option<String>,
}

pub fn declare(ctx: &Ctx, args: DeclareArgs) -> Result<()> {
    let store = ctx.store()?;
    let subject = resolve_subject(&store, &args.subject)?;
    let _lock = subject.lock(&store)?;
    let events = subject.events(&store)?;
    let mut ledger = load_relations(&store, &subject)?;
    let target = match (args.path, args.artifact) {
        (Some(path), None) => DeclaredTarget::Path {
            path,
            at: resolve_at(ctx, args.at.as_deref())?,
        },
        (None, Some(file)) => {
            let name =
                crate::journal::split_qualified(&file).map_or(file.as_str(), |(_, name)| name);
            if crate::journal::parse_artifact_name(name).is_none() {
                bail!(
                    "unknown-artifact: {file:?} is not a journal artifact name \
                     (<timestamp>-<topic>-<kind>.md)"
                );
            }
            DeclaredTarget::Artifact { file }
        }
        _ => bail!("name exactly one of --path or --artifact"),
    };
    if let Some(citation) = &args.citation {
        ledger.check_citation(&subject.subject, citation)?;
    }
    let event = next_event(
        ctx,
        &store,
        &subject,
        &events,
        &[],
        Payload::ContextDeclared {
            subject: subject.subject.clone(),
            relation: args.relation,
            target: target.clone(),
            citation: args.citation.clone(),
        },
    )?;
    ledger.record(&event)?;
    ctx.ensure_declared_actor(&store)?;
    subject.append(&store, &event)?;
    println!(
        "declared: {} {} {target}",
        subject.subject.label(),
        args.relation.as_str()
    );
    if let Some(citation) = &args.citation {
        println!("  citation: {citation}");
    }
    println!("  event: {}", event.event_id);
    Ok(())
}

pub fn capture(
    ctx: &Ctx,
    record: String,
    subject: Option<String>,
    capture: CaptureState,
) -> Result<()> {
    let store = ctx.store()?;
    let subject = match subject {
        Some(raw) => resolve_subject(&store, &raw)?,
        None => subject_holding(&store, &record)?,
    };
    let _lock = subject.lock(&store)?;
    let events = subject.events(&store)?;
    let mut ledger = load_relations(&store, &subject)?;
    ledger.check_capture(&subject.subject, &record)?;
    let event = next_event(
        ctx,
        &store,
        &subject,
        &events,
        &[],
        Payload::ContextCaptureReported {
            subject: subject.subject.clone(),
            record: record.clone(),
            capture,
        },
    )?;
    ledger.record(&event)?;
    ctx.ensure_declared_actor(&store)?;
    subject.append(&store, &event)?;
    println!(
        "capture: {record} on {} {}",
        subject.subject.label(),
        capture.as_str()
    );
    println!("  event: {}", event.event_id);
    Ok(())
}

/// The one subject holding a read of `record`, across every change and
/// candidate. None, or more than one, refuses.
fn subject_holding(store: &Store, record: &str) -> Result<Subject> {
    let mut holders = Vec::new();
    for change_id in store.list_change_ids()? {
        let relations = change_relations(&store.load_events(&change_id)?)?;
        let subject = RelationSubject::Change {
            change_id: change_id.clone(),
        };
        if relations.read(&subject, record).is_some() {
            holders.push(Subject {
                subject,
                claims_change: change_id,
            });
        }
    }
    let candidates = super::candidate::load_ledger(store)?;
    for read in repository_relations(store)?.reads() {
        if read.record != record {
            continue;
        }
        if let RelationSubject::Candidate { candidate_id } = &read.subject {
            holders.push(Subject {
                subject: read.subject.clone(),
                claims_change: candidates
                    .registration(candidate_id)
                    .map(|registration| registration.brief.change_id.clone())
                    .unwrap_or_default(),
            });
        }
    }
    match holders.len() {
        0 => bail!("unknown-record: tool record {record} is not recorded as a read anywhere"),
        1 => Ok(holders.remove(0)),
        _ => bail!(
            "ambiguous-record: tool record {record} is recorded on {}; name one with --subject",
            holders
                .iter()
                .map(|holder| holder.subject.label())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
