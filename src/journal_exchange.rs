//! The versioned file that carries selected journal artifacts between
//! replicas.
//!
//! A bundle holds artifact bodies beside the journal events recorded about
//! them, each body digested, and names the replica that exported it. A
//! receipt written into the receiving journal records the bundle digest and
//! source replica and admitted artifacts. A completed receipt makes a repeated
//! import a no-op; a partial receipt leaves the rest eligible for admission. Neither
//! carries a journal directory path: a journal location is local to the
//! machine that holds it.

use crate::ids;
use crate::journal::{artifact_references, parse_artifact_name, ArtifactReference, JournalEvent};
use crate::replica::{validate_identity, ReplicaIdentity};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

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
    /// Opaque payloads retain their fields and serialization order for checksum
    /// validation even when this reader cannot understand them.
    #[serde(deserialize_with = "deserialize_events")]
    pub(crate) events: Vec<Box<RawValue>>,
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

/// Formatting whitespace is excluded from bundle checksums; event tokens,
/// including object key order and string escapes, retain their bytes.
fn deserialize_events<'de, D>(deserializer: D) -> std::result::Result<Vec<Box<RawValue>>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<Box<RawValue>>::deserialize(deserializer)?
        .into_iter()
        .map(|raw| {
            let mut in_string = false;
            let mut escaped = false;
            let compact: String = raw
                .get()
                .chars()
                .filter(|&character| {
                    if in_string {
                        if escaped {
                            escaped = false;
                        } else if character == '\\' {
                            escaped = true;
                        } else if character == '"' {
                            in_string = false;
                        }
                        true
                    } else if character == '"' {
                        in_string = true;
                        true
                    } else {
                        !matches!(character, ' ' | '\t' | '\r' | '\n')
                    }
                })
                .collect();
            RawValue::from_string(compact).map_err(serde::de::Error::custom)
        })
        .collect()
}

impl BundledArtifact {
    pub(crate) fn understood_events(&self) -> Result<Vec<JournalEvent>> {
        self.events
            .iter()
            .map(|value| {
                let event: JournalEvent = serde_json::from_str(value.get())?;
                if !event.known() {
                    bail!("{} carries an unsupported journal event", self.file);
                }
                Ok(event)
            })
            .collect()
    }

    fn unsupported_events(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|value| {
                serde_json::from_str::<JournalEvent>(value.get())
                    .map_or(true, |event| !event.known())
            })
            .map(|raw| {
                let value: serde_json::Value =
                    serde_json::from_str(raw.get()).expect("raw journal events contain valid JSON");
                format!(
                    "unsupported event kind {:?} in schema {:?}",
                    value["event"].as_str().unwrap_or_default(),
                    value["schema"].as_str().unwrap_or_default()
                )
            })
            .collect()
    }
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

    /// Admission preserves each artifact and the closure of its references.
    pub(crate) fn held_back(&self) -> Result<BTreeMap<String, Vec<String>>> {
        let mut held: BTreeMap<String, Vec<String>> = self
            .artifacts
            .iter()
            .filter_map(|artifact| {
                let reasons = artifact.unsupported_events();
                (!reasons.is_empty()).then(|| (artifact.file.clone(), reasons))
            })
            .collect();
        loop {
            let mut changed = false;
            for artifact in &self.artifacts {
                if held.contains_key(&artifact.file) {
                    continue;
                }
                for event in artifact.understood_events()? {
                    for reference in artifact_references(&event)? {
                        if let ArtifactReference::Local(file) = reference {
                            if let Some(reasons) = held.get(&file) {
                                let reasons = reasons
                                    .iter()
                                    .map(|reason| format!("references {file}: {reason}"))
                                    .collect();
                                held.insert(artifact.file.clone(), reasons);
                                changed = true;
                                break;
                            }
                        }
                    }
                    if held.contains_key(&artifact.file) {
                        break;
                    }
                }
            }
            if !changed {
                return Ok(held);
            }
        }
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
        let Ok(event) = serde_json::from_str::<JournalEvent>(event.get()) else {
            continue;
        };
        if !event.known() {
            continue;
        }
        for reference in artifact_references(&event)? {
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
    for raw in &artifact.events {
        let event: serde_json::Value = serde_json::from_str(raw.get())?;
        if !event["event"].is_string() || !event["schema"].is_string() {
            bail!(
                "journal bundle artifact {} carries an event without a kind or schema",
                artifact.file
            );
        }
        if event["file"].as_str() != Some(artifact.file.as_str()) {
            bail!(
                "journal bundle artifact {} carries an event recorded about {:?}",
                artifact.file,
                event["file"].as_str().unwrap_or("no artifact")
            );
        }
        if !seen.insert(serde_json::to_vec(&event)?) {
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
                if offset == 0 {
                    bail!("{text} (no unique append event)");
                }
                starts.push((offset - 1, text, matching));
            }
        }
        offset += line.len();
    }
    let base = &body[..starts.first().map_or(body.len(), |(offset, _, _)| *offset)];
    let mut blocks = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, (start, heading, candidates)) in starts.iter().enumerate() {
        let end = starts
            .get(index + 1)
            .map_or(body.len(), |(offset, _, _)| *offset);
        let block = &body[*start..end];
        let digest = body_digest(block);
        let mut matching: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|(_, (_, id, ts, recorded))| {
                if seen.contains(id.as_str()) {
                    return false;
                }
                if let Some(recorded) = recorded {
                    return digest == *recorded;
                }
                // Answers without a digest bind by choice and second-resolution timestamp.
                !id.starts_with("answer:")
                    || ts.parse::<DateTime<Utc>>().is_ok_and(|ts| {
                        heading.ends_with(&format!(
                            ", {})",
                            ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                        ))
                    })
            })
            .collect();
        matching.sort_by_key(|(_, (_, _, ts, _))| ts.parse::<DateTime<Utc>>().ok());
        let Some((event, (_, id, ts, _))) = matching.first().copied() else {
            let (_, (_, id, _, _)) = candidates[0];
            bail!("{id} (block digest differs from its event or duplicate block identity)");
        };
        if matching.len() > 1
            && !matching
                .iter()
                .all(|(_, (_, id, _, _))| id.starts_with("answer:"))
        {
            bail!("{heading} (no unique append event)");
        }
        seen.insert(id.as_str());
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
    let incoming_revision = revision(&incoming.body, &incoming.understood_events()?)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_ordered_legacy_bundle_validates() {
        let bytes = br###"{
  "schema": "arc-journal-bundle/1",
  "project_id": "project-fixture",
  "source_replica": {
    "name": "origin",
    "repository_id": "repo-fixture"
  },
  "artifacts_sha256": "95057253866f506d57905839db1e8c8861165a2e8b6ed414a141ddabf349a378",
  "artifacts": [
    {
      "file": "20261010T120000Z-example-todo.md",
      "storage": "hot",
      "body": "# Example\n",
      "digest": "sha256:8ef0b048ad51a9d2ccaf9177716d7c83159a1d8c22fd37aa2b7d03c11b21b40d",
      "events": [
        {
          "schema": "journal-events/1",
          "ts": "2026-10-10T12:00:00Z",
          "harness": "codex",
          "session": "fixture-session",
          "topic": "example",
          "event": "note",
          "file": "20261010T120000Z-example-todo.md"
        }
      ]
    }
  ]
}"###;
        let bundle = JournalBundle::parse(bytes).unwrap();
        assert_eq!(
            serde_json::to_string(&bundle.artifacts[0].events[0]).unwrap(),
            r###"{"schema":"journal-events/1","ts":"2026-10-10T12:00:00Z","harness":"codex","session":"fixture-session","topic":"example","event":"note","file":"20261010T120000Z-example-todo.md"}"###
        );
        let returned = JournalBundle::parse(&bundle.to_bytes().unwrap()).unwrap();
        assert_eq!(bundle.digest().unwrap(), returned.digest().unwrap());
        assert!(bundle.held_back().unwrap().is_empty());
    }

    #[test]
    fn opaque_future_bundle_validates() {
        let bytes = br###"{
  "schema": "arc-journal-bundle/2",
  "project_id": "project-fixture",
  "source_replica": {
    "name": "origin",
    "repository_id": "repo-fixture"
  },
  "artifacts_sha256": "7911de98ac458d023ce455ca2a7e0471fa8f64b95fe7e4c7524b16bb768fff55",
  "artifacts": [
    {
      "file": "20261010T120000Z-example-todo.md",
      "storage": "hot",
      "body": "# Example\n",
      "digest": "sha256:8ef0b048ad51a9d2ccaf9177716d7c83159a1d8c22fd37aa2b7d03c11b21b40d",
      "events": [
        {
          "schema": "journal-events/99",
          "event": "future-observation",
          "file": "20261010T120000Z-example-todo.md",
          "payload": {
            "z": "space \\\" quote and \\ slash",
            "a": [
              1,
              true,
              null
            ]
          }
        }
      ]
    }
  ]
}"###;
        let bundle = JournalBundle::parse(bytes).unwrap();
        assert_eq!(
            serde_json::to_string(&bundle.artifacts[0].events[0]).unwrap(),
            r###"{"schema":"journal-events/99","event":"future-observation","file":"20261010T120000Z-example-todo.md","payload":{"z":"space \\\" quote and \\ slash","a":[1,true,null]}}"###
        );
        let returned = JournalBundle::parse(&bundle.to_bytes().unwrap()).unwrap();
        assert_eq!(bundle.digest().unwrap(), returned.digest().unwrap());
        assert_eq!(bundle.held_back().unwrap().len(), 1);
    }
}
