//! The versioned file that carries selected journal artifacts between
//! replicas.
//!
//! A bundle holds artifact bodies beside the journal events recorded about
//! them, each body digested, and names the replica that exported it. A
//! receipt written into the receiving journal records the bundle digest and
//! source replica, which is what makes a repeated import a no-op. Neither
//! carries a journal directory path: a journal location is local to the
//! machine that holds it.

use crate::ids;
use crate::journal::{artifact_references, parse_artifact_name, ArtifactReference, JournalEvent};
use crate::replica::{validate_identity, ReplicaIdentity};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) const BUNDLE_SCHEMA: &str = "arc-journal-bundle/2";
pub(crate) const IMPORT_RECEIPT_SCHEMA: &str = "arc-journal-exchange-import/1";
pub(crate) const HOT_STORAGE: &str = "hot";
pub(crate) const COLD_STORAGE: &str = "cold";

/// One artifact and the events the exporting journal recorded about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundledArtifact {
    pub(crate) file: String,
    /// Where the exporting journal held the body: `hot` or `cold`.
    pub(crate) storage: String,
    pub(crate) body: String,
    /// `sha256:` over the body, the same spelling the journal records for a
    /// body digest.
    pub(crate) digest: String,
    pub(crate) events: Vec<JournalEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JournalBundle {
    pub(crate) schema: String,
    pub(crate) project_id: String,
    pub(crate) source_replica: ReplicaIdentity,
    /// Checksum over the artifacts as carried, so a bundle altered after it
    /// was written is refused rather than imported.
    pub(crate) artifacts_sha256: String,
    pub(crate) artifacts: Vec<BundledArtifact>,
}

/// One artifact a receipt says arrived, with the digest it arrived at and how
/// many of its events were new here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportedArtifact {
    pub(crate) file: String,
    pub(crate) storage: String,
    pub(crate) digest: String,
    pub(crate) events: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportReceipt {
    pub(crate) schema: String,
    pub(crate) bundle_sha256: String,
    pub(crate) source_replica: ReplicaIdentity,
    pub(crate) imported_by: ReplicaIdentity,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) artifacts: Vec<ImportedArtifact>,
}

impl JournalBundle {
    /// Assemble a bundle from the artifacts an export selected, sorted so the
    /// same selection produces the same bytes.
    pub(crate) fn new(
        project_id: String,
        source_replica: ReplicaIdentity,
        mut artifacts: Vec<BundledArtifact>,
    ) -> Result<Self> {
        artifacts.sort_by(|left, right| left.file.cmp(&right.file));
        let bundle = Self {
            schema: BUNDLE_SCHEMA.to_string(),
            project_id,
            source_replica,
            artifacts_sha256: artifacts_digest(&artifacts)?,
            artifacts,
        };
        bundle.validate()?;
        Ok(bundle)
    }

    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// The identity a receipt keys an import by: a digest of the parsed
    /// bundle, so reformatting the file does not change what it is.
    pub(crate) fn digest(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(self)?)))
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        let bundle: JournalBundle =
            serde_json::from_slice(bytes).context("malformed journal bundle")?;
        if bundle.schema != BUNDLE_SCHEMA && bundle.schema != "arc-journal-bundle/1" {
            bail!(
                "unsupported journal bundle schema {:?}; expected {BUNDLE_SCHEMA:?}",
                bundle.schema
            );
        }
        bundle.validate()?;
        Ok(bundle)
    }

    fn validate(&self) -> Result<()> {
        ids::validate_id_component(&self.project_id)?;
        validate_identity(&self.source_replica)?;
        if self.artifacts.is_empty() {
            bail!("journal bundle carries no artifacts");
        }
        let mut files = BTreeSet::new();
        for artifact in &self.artifacts {
            validate_artifact(artifact)?;
            if !files.insert(artifact.file.as_str()) {
                bail!("journal bundle carries {} more than once", artifact.file);
            }
        }
        if artifacts_digest(&self.artifacts)? != self.artifacts_sha256 {
            bail!("journal bundle artifact checksum does not match");
        }
        for artifact in &self.artifacts {
            validate_references(artifact, &files)?;
        }
        Ok(())
    }
}

/// Every artifact reference one bundled event makes must resolve inside the
/// bundle, so an import cannot install a record that points at a journal it
/// does not hold.
fn validate_references(artifact: &BundledArtifact, files: &BTreeSet<&str>) -> Result<()> {
    for event in &artifact.events {
        for reference in artifact_references(event)? {
            match reference {
                ArtifactReference::Local(referenced) => {
                    if !files.contains(referenced.as_str()) {
                        bail!(
                            "journal bundle artifact {} references {referenced}, which the \
                             bundle does not carry",
                            artifact.file
                        );
                    }
                }
                ArtifactReference::ForeignProject { file, project } => bail!(
                    "journal bundle artifact {} records a decision in {file} from project \
                     {project}, which a bundle for one logical project cannot carry",
                    artifact.file
                ),
            }
        }
    }
    Ok(())
}

fn validate_artifact(artifact: &BundledArtifact) -> Result<()> {
    if artifact.file.contains(['/', '\\']) || parse_artifact_name(&artifact.file).is_none() {
        bail!(
            "journal bundle artifact {:?} is not a journal artifact name \
             (<timestamp>-<topic>-<kind>.md)",
            artifact.file
        );
    }
    if artifact.storage != HOT_STORAGE && artifact.storage != COLD_STORAGE {
        bail!(
            "journal bundle artifact {} names unknown storage {:?}",
            artifact.file,
            artifact.storage
        );
    }
    if artifact.digest != body_digest(&artifact.body) {
        bail!(
            "journal bundle artifact {} does not match its digest",
            artifact.file
        );
    }
    let mut seen = BTreeSet::new();
    for event in &artifact.events {
        if !event.known() {
            bail!(
                "journal bundle artifact {} carries an event that is not a journal record",
                artifact.file
            );
        }
        if event.file.as_deref() != Some(artifact.file.as_str()) {
            bail!(
                "journal bundle artifact {} carries an event recorded about {:?}",
                artifact.file,
                event.file.as_deref().unwrap_or("no artifact")
            );
        }
        if !seen.insert(serde_json::to_vec(event)?) {
            bail!(
                "journal bundle artifact {} carries a duplicate event",
                artifact.file
            );
        }
    }
    Ok(())
}

/// `sha256:` over one artifact body, the spelling the journal already uses
/// for a body digest.
pub(crate) fn body_digest(body: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(body.as_bytes())))
}

fn artifacts_digest(artifacts: &[BundledArtifact]) -> Result<String> {
    let mut hasher = Sha256::new();
    for artifact in artifacts {
        hasher.update(serde_json::to_vec(artifact)?);
        hasher.update(b"\n");
    }
    Ok(hex::encode(hasher.finalize()))
}

struct RevisionBlock<'a> {
    id: String,
    timestamp: DateTime<Utc>,
    body: &'a str,
    event: Vec<u8>,
}

struct Revision<'a> {
    base: &'a str,
    blocks: Vec<RevisionBlock<'a>>,
}

/// Only sections identified by recorded append events participate in a merge.
/// The base and every shared section must retain their exact bytes.
fn revision<'a>(body: &'a str, events: &[JournalEvent]) -> Result<Revision<'a>> {
    let descriptors: Vec<_> = events
        .iter()
        .filter_map(|event| event.exchange_block().map(|block| (event, block)))
        .collect();
    let mut starts = Vec::new();
    let mut offset = 0;
    let mut fence: Option<(char, usize)> = None;
    for line in body.split_inclusive('\n') {
        let text = line.trim_end_matches(['\r', '\n']);
        let trimmed = text.trim_start_matches(' ');
        if text.len() - trimmed.len() <= 3 {
            let character = trimmed.chars().next().unwrap_or(' ');
            let width = trimmed.chars().take_while(|c| *c == character).count();
            if let Some((opened, length)) = fence {
                if character == opened && width >= length && trimmed[width..].trim().is_empty() {
                    fence = None;
                }
                offset += line.len();
                continue;
            }
            if matches!(character, '`' | '~') && width >= 3 {
                fence = Some((character, width));
                offset += line.len();
                continue;
            }
        }
        if fence.is_none() {
            let matching: Vec<_> = descriptors
                .iter()
                .filter(|(_, (heading, _, _, _))| text.starts_with(heading))
                .collect();
            if !matching.is_empty() {
                if matching.len() != 1 || offset == 0 {
                    bail!("{text} (no unique append event)");
                }
                starts.push((offset - 1, matching[0]));
            }
        }
        offset += line.len();
    }
    let base = &body[..starts.first().map_or(body.len(), |(offset, _)| *offset)];
    let mut blocks = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, (start, (event, (_, id, ts, digest)))) in starts.iter().enumerate() {
        if !seen.insert(id.as_str()) {
            bail!("{id} (duplicate block identity)");
        }
        let end = starts
            .get(index + 1)
            .map_or(body.len(), |(offset, _)| *offset);
        let block = &body[*start..end];
        if digest.is_some_and(|digest| body_digest(block) != digest) {
            bail!("{id} (block digest differs from its event)");
        }
        blocks.push(RevisionBlock {
            id: id.clone(),
            timestamp: ts
                .parse()
                .with_context(|| format!("{id} (invalid event timestamp)"))?,
            body: block,
            event: serde_json::to_vec(event)?,
        });
    }
    for (_, (_, id, _, _)) in &descriptors {
        if !seen.contains(id.as_str()) {
            bail!("{id} (event has no appended block)");
        }
    }
    Ok(Revision { base, blocks })
}

pub(crate) fn merge_revision(
    held_body: &str,
    held_events: &[JournalEvent],
    incoming: &BundledArtifact,
) -> Result<String> {
    let held = revision(held_body, held_events)?;
    let incoming_revision = revision(&incoming.body, &incoming.events)?;
    if held.base != incoming_revision.base {
        bail!("base");
    }
    let mut union = std::collections::BTreeMap::new();
    for block in &held.blocks {
        union.insert(block.id.as_str(), block);
    }
    for block in &incoming_revision.blocks {
        if let Some(local) = union.get(block.id.as_str()) {
            if local.body != block.body || local.event != block.event {
                bail!("{}", block.id);
            }
        } else {
            union.insert(block.id.as_str(), block);
        }
    }
    let prefix = |left: &Revision<'_>, right: &Revision<'_>| {
        left.blocks.len() <= right.blocks.len()
            && left
                .blocks
                .iter()
                .zip(&right.blocks)
                .all(|(left, right)| left.id == right.id)
    };
    if prefix(&held, &incoming_revision) {
        return Ok(incoming.body.clone());
    }
    if prefix(&incoming_revision, &held) {
        return Ok(held_body.to_string());
    }
    let mut blocks: Vec<_> = union.into_values().collect();
    blocks.sort_by(|left, right| (left.timestamp, &left.id).cmp(&(right.timestamp, &right.id)));
    let mut merged = held.base.to_string();
    for block in blocks {
        merged.push_str(block.body);
    }
    Ok(merged)
}
