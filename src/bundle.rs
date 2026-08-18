use crate::ids;
use crate::model::{ClaimStage, Event, Payload, StageBudget};
use crate::state;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const BUNDLE_SCHEMA: &str = "arc-bundle/2";

#[derive(Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub schema: String,
    pub repository_id: String,
    pub change_id: String,
    pub event_count: usize,
    pub events_sha256: String,
    /// The store format the exporting build wrote. An importer that would skip
    /// event types it does not know refuses instead: skipping a lifecycle
    /// event means reading a closed change as open, and then closing it a
    /// second way.
    #[serde(default = "legacy_store_format")]
    pub store_format: u32,
    pub events: Vec<Value>,
    /// Repository-scoped events, carried so a change's recorded revisions stay
    /// followable after transport. A rewrite is not change-scoped, but a
    /// bundle that drops it hands the receiver revisions nothing can resolve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repository_events: Vec<Value>,
}

/// Bundles written before the format was carried came from a build that wrote
/// store format 1.
fn legacy_store_format() -> u32 {
    1
}

impl Bundle {
    pub fn export(store: &Store, change_id: &str) -> Result<Self> {
        ids::validate_id_component(change_id)?;
        let raw = store.raw_events(change_id)?;
        if raw.is_empty() {
            bail!("change {change_id:?} has no events");
        }

        let mut events = Vec::with_capacity(raw.len());
        let mut origin_repository_id: Option<String> = None;
        for (file_event_id, value) in raw {
            let envelope = event_envelope(&value)?;
            if envelope.event_id != file_event_id {
                bail!(
                    "event file {file_event_id:?} contains event_id {:?}",
                    envelope.event_id
                );
            }
            if envelope.change_id != change_id {
                bail!(
                    "event {} belongs to change {:?}, not {change_id:?}",
                    envelope.event_id,
                    envelope.change_id
                );
            }
            // The opening event identifies the originating store. Later
            // events may legitimately come from other stores after transfer.
            if origin_repository_id.is_none() {
                origin_repository_id = Some(envelope.repository_id.to_string());
            }
            events.push(value);
        }
        events.sort_by(|a, b| {
            event_id(a)
                .expect("validated event")
                .cmp(event_id(b).expect("validated event"))
        });

        let events_sha256 = checksum(&events)?;
        // The checksum covers the change's events, which is what the receiver
        // replays. Repository events travel beside them: they are context for
        // resolving recorded revisions, not part of this change's history.
        let repository_events = store
            .raw_repository_events_unseen(&std::collections::BTreeSet::new())?
            .into_iter()
            .map(|(_, value)| value)
            .collect();
        Ok(Bundle {
            schema: BUNDLE_SCHEMA.to_string(),
            store_format: crate::model::SCHEMA_VERSION,
            repository_id: origin_repository_id.expect("non-empty event list"),
            change_id: change_id.to_string(),
            event_count: events.len(),
            events_sha256,
            events,
            repository_events,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<ValidatedBundle> {
        let bundle: Bundle =
            serde_json::from_slice(bytes).context("malformed arc export bundle")?;
        if bundle.store_format > crate::model::SCHEMA_VERSION {
            bail!(
                "bundle was written by a newer arc (store format {}, this build understands \
                 {}); upgrade arc rather than importing history it would not fully read",
                bundle.store_format,
                crate::model::SCHEMA_VERSION
            );
        }
        if bundle.schema != BUNDLE_SCHEMA {
            bail!(
                "unsupported bundle schema {:?}; expected {BUNDLE_SCHEMA:?}",
                bundle.schema
            );
        }
        ids::validate_id_component(&bundle.repository_id)?;
        ids::validate_id_component(&bundle.change_id)?;
        if bundle.event_count != bundle.events.len() {
            bail!(
                "event_count {} does not match {} bundled events",
                bundle.event_count,
                bundle.events.len()
            );
        }
        let actual_checksum = checksum(&bundle.events)?;
        if bundle.events_sha256 != actual_checksum {
            bail!(
                "events checksum mismatch: bundle says {}, computed {actual_checksum}",
                bundle.events_sha256
            );
        }

        let mut events = Vec::with_capacity(bundle.events.len());
        let mut patchsets = Vec::new();
        let mut unknown_event_types = Vec::new();
        let mut prior_event_id: Option<String> = None;
        let mut patchset_ids = HashSet::new();
        for value in &bundle.events {
            let envelope = event_envelope(value)?;
            if prior_event_id.is_none() && envelope.repository_id != bundle.repository_id {
                bail!(
                    "first event {} has repository_id {:?}, expected bundle origin {:?}",
                    envelope.event_id,
                    envelope.repository_id,
                    bundle.repository_id
                );
            }
            if envelope.change_id != bundle.change_id {
                bail!(
                    "event {} belongs to change {:?}, expected {:?}",
                    envelope.event_id,
                    envelope.change_id,
                    bundle.change_id
                );
            }
            if prior_event_id
                .as_deref()
                .is_some_and(|previous| previous >= envelope.event_id)
            {
                bail!("events must be strictly ordered by ascending event_id");
            }
            prior_event_id = Some(envelope.event_id.to_string());

            let typed = parse_typed_event(value)?;

            if envelope.event_type == "patchset-added" {
                let object = value.as_object().expect("validated event object");
                let patchset_id = string_field(object, "patchset_id", envelope.event_id)?;
                ids::validate_id_component(patchset_id)?;
                if !patchset_ids.insert(patchset_id.to_string()) {
                    bail!("duplicate patchset_id {patchset_id:?}");
                }
                let base = string_field(object, "base", envelope.event_id)?;
                let head = string_field(object, "head", envelope.event_id)?;
                if base.is_empty() || head.is_empty() {
                    bail!(
                        "patchset event {} has an empty object ID",
                        envelope.event_id
                    );
                }
                patchsets.push(PatchsetObject {
                    event_id: envelope.event_id.to_string(),
                    patchset_id: patchset_id.to_string(),
                    base: base.to_string(),
                    head: head.to_string(),
                });
            } else if typed.is_none() {
                unknown_event_types.push((
                    envelope.event_id.to_string(),
                    envelope.event_type.to_string(),
                ));
            }

            events.push(ValidatedEvent {
                event_id: envelope.event_id.to_string(),
                value: value.clone(),
                bytes: event_file_bytes(value)?,
                typed,
            });
        }

        let typed_events = events
            .iter()
            .filter_map(|event| event.typed.clone())
            .collect::<Vec<_>>();
        state::reduce(&typed_events).context("bundled known events are not replayable")?;

        Ok(ValidatedBundle {
            bundle,
            events,
            patchsets,
            unknown_event_types,
        })
    }
}

pub struct ValidatedBundle {
    pub bundle: Bundle,
    pub events: Vec<ValidatedEvent>,
    pub patchsets: Vec<PatchsetObject>,
    pub unknown_event_types: Vec<(String, String)>,
}

pub struct ValidatedEvent {
    pub event_id: String,
    pub value: Value,
    pub bytes: Vec<u8>,
    pub typed: Option<Event>,
}

pub struct PatchsetObject {
    pub event_id: String,
    pub patchset_id: String,
    pub base: String,
    pub head: String,
}

struct EventEnvelope<'a> {
    event_id: &'a str,
    repository_id: &'a str,
    change_id: &'a str,
    event_type: &'a str,
}

fn event_envelope(value: &Value) -> Result<EventEnvelope<'_>> {
    let object = value.as_object().context("event must be a JSON object")?;
    let event_id = object
        .get("event_id")
        .and_then(Value::as_str)
        .context("event must contain a string event_id")?;
    let repository_id = object
        .get("repository_id")
        .and_then(Value::as_str)
        .context("event must contain a string repository_id")?;
    let change_id = object
        .get("change_id")
        .and_then(Value::as_str)
        .context("event must contain a string change_id")?;
    let event_type = object
        .get("event_type")
        .and_then(Value::as_str)
        .context("event must contain a string event_type")?;
    ids::validate_id_component(event_id)?;
    ids::validate_id_component(repository_id)?;
    ids::validate_id_component(change_id)?;
    Ok(EventEnvelope {
        event_id,
        repository_id,
        change_id,
        event_type,
    })
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    event_id: &str,
) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("event {event_id} must contain a string {field}"))
}

/// Decode a complete event envelope while leaving future payload tags opaque.
/// Every imported event must satisfy this build's envelope; only the payload
/// tag may be unknown. Import uses the same helper for bundled and already-local
/// raw history.
pub fn parse_typed_event(value: &Value) -> Result<Option<Event>> {
    let envelope = event_envelope(value)?;
    let event: Event = serde_json::from_value(value.clone())
        .with_context(|| format!("event {} is malformed", envelope.event_id))?;
    if matches!(&event.payload, Payload::Unknown) {
        return Ok(None);
    }
    match &event.payload {
        Payload::ClaimSet {
            claim_id,
            ttl_seconds,
            stage_budgets,
            displaced,
        } => {
            validate_claim_identity(&event)?;
            ids::validate_id_component(claim_id)?;
            if let Some(displaced) = displaced {
                ids::validate_id_component(&displaced.claim_id)?;
            }
            if *ttl_seconds == 0 {
                bail!("claim event {} has a zero TTL", event.event_id);
            }
            for key in [
                StageBudget::Launch,
                StageBudget::Started,
                StageBudget::SpecRead,
                StageBudget::Implementing,
                StageBudget::Verifying,
            ] {
                if !stage_budgets.get(&key).is_some_and(|seconds| *seconds > 0) {
                    bail!(
                        "claim event {} lacks a positive {} budget",
                        event.event_id,
                        key.as_str()
                    );
                }
            }
        }
        Payload::ClaimReleased { claim_id } => {
            validate_claim_identity(&event)?;
            ids::validate_id_component(claim_id)?;
        }
        Payload::StageSet {
            claim_id,
            stage,
            note,
            ..
        } => {
            validate_claim_identity(&event)?;
            ids::validate_id_component(claim_id)?;
            if *stage == ClaimStage::Snapshotted {
                bail!(
                    "stage event {} cannot set snapshotted explicitly",
                    event.event_id
                );
            }
            if *stage == ClaimStage::BlockedOn
                && !note
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|note| !note.is_empty())
            {
                bail!("blocked-on event {} requires a note", event.event_id);
            }
        }
        _ => {}
    }
    Ok(Some(event))
}

fn validate_claim_identity(event: &Event) -> Result<()> {
    for (field, value) in [
        ("actor", Some(event.actor.as_str())),
        ("harness", event.harness.as_deref()),
        ("session", event.session.as_deref()),
    ] {
        let value = value.with_context(|| {
            format!(
                "{} event {} has no {field}",
                event_type(&event.payload),
                event.event_id
            )
        })?;
        if value.trim().is_empty() || value != value.trim() {
            bail!(
                "{} event {} has a non-canonical {field}",
                event_type(&event.payload),
                event.event_id
            );
        }
    }
    Ok(())
}

fn event_type(payload: &Payload) -> &'static str {
    match payload {
        Payload::BriefRecorded { .. } => "brief",
        Payload::ChangelogRecorded { .. } => "changelog",
        Payload::ClaimSet { .. } => "claim",
        Payload::ClaimReleased { .. } => "claim-release",
        Payload::StageSet { .. } => "stage",
        _ => "event",
    }
}

fn event_id(value: &Value) -> Option<&str> {
    value.get("event_id").and_then(Value::as_str)
}

pub fn checksum(events: &[Value]) -> Result<String> {
    let mut digest = Sha256::new();
    for event in events {
        digest.update(serde_json::to_vec(event)?);
        digest.update(b"\n");
    }
    Ok(hex::encode(digest.finalize()))
}

fn event_file_bytes(event: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(event)?;
    bytes.push(b'\n');
    Ok(bytes)
}
