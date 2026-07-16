use crate::ids;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const BUNDLE_SCHEMA: &str = "arc-bundle/1";

#[derive(Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub schema: String,
    pub repository_id: String,
    pub change_id: String,
    pub event_count: usize,
    pub events_sha256: String,
    pub events: Vec<Value>,
}

impl Bundle {
    pub fn export(store: &Store, change_id: &str) -> Result<Self> {
        ids::validate_id_component(change_id)?;
        let raw = store.raw_events(change_id)?;
        if raw.is_empty() {
            bail!("change {change_id:?} has no events");
        }

        let mut events = Vec::with_capacity(raw.len());
        let mut source_repository_id: Option<String> = None;
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
            match &source_repository_id {
                Some(expected) if expected != envelope.repository_id => bail!(
                    "event {} has repository_id {:?}, expected {:?}",
                    envelope.event_id,
                    envelope.repository_id,
                    expected
                ),
                None => source_repository_id = Some(envelope.repository_id.to_string()),
                _ => {}
            }
            events.push(value);
        }
        events.sort_by(|a, b| {
            event_id(a)
                .expect("validated event")
                .cmp(event_id(b).expect("validated event"))
        });

        let events_sha256 = checksum(&events)?;
        Ok(Bundle {
            schema: BUNDLE_SCHEMA.to_string(),
            repository_id: source_repository_id.expect("non-empty event list"),
            change_id: change_id.to_string(),
            event_count: events.len(),
            events_sha256,
            events,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

struct EventEnvelope<'a> {
    event_id: &'a str,
    repository_id: &'a str,
    change_id: &'a str,
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
    ids::validate_id_component(event_id)?;
    ids::validate_id_component(repository_id)?;
    ids::validate_id_component(change_id)?;
    Ok(EventEnvelope {
        event_id,
        repository_id,
        change_id,
    })
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
