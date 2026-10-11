//! Partial ledger observations for cost queries. Published import batches
//! keep their admission checks; a failed record never replaces readable peers.

use crate::model::{Event, Payload};
use crate::store::{ImportBatch, Store};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) struct CostRead<'a> {
    store: &'a Store,
    batches: Vec<ImportBatch>,
    pub gaps: Vec<Value>,
}

fn record_id(scope: &str, path: &Path) -> String {
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    match stem.filter(|stem| crate::ids::validate_id_component(stem).is_ok()) {
        Some(stem) => format!("{scope}:{stem}"),
        None => format!("{scope}:unidentified-record"),
    }
}

fn paths(directory: &Path, scope: &str, gaps: &mut Vec<Value>) -> Vec<std::path::PathBuf> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => {
            gaps.push(json!({"record": scope, "reason": "ledger directory unavailable"}));
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry)
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json") =>
            {
                out.push(entry.path())
            }
            Ok(_) => {}
            Err(_) => gaps.push(json!({"record": scope, "reason": "ledger entry unavailable"})),
        }
    }
    out.sort();
    out
}

fn regular(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn may_assign(value: &Value) -> bool {
    !matches!(
        value["event_type"].as_str(),
        Some("cost-facts-retained" | "run-ended")
    )
}

impl<'a> CostRead<'a> {
    pub fn new(store: &'a Store) -> Self {
        let mut gaps = Vec::new();
        let mut batches = Vec::new();
        for path in paths(&store.root.join("imports"), "import", &mut gaps) {
            let batch = regular(&path).then(|| crate::store::read_import_batch(&path));
            match batch {
                Some(Ok(batch)) => batches.push(batch),
                _ => gaps.push(json!({"record": record_id("import", &path), "reason": "published import unreadable"})),
            }
        }
        Self {
            store,
            batches,
            gaps,
        }
    }

    pub fn change_ids(&mut self) -> Vec<String> {
        let mut ids: BTreeSet<String> = self
            .batches
            .iter()
            .map(|batch| batch.change_id.clone())
            .collect();
        match std::fs::read_dir(self.store.root.join("changes")) {
            Ok(entries) => {
                for entry in entries {
                    if let Ok(entry) = entry {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        match entry.file_type() {
                            Ok(kind)
                                if kind.is_dir()
                                    && crate::ids::validate_id_component(&name).is_ok() =>
                            {
                                ids.insert(name);
                            }
                            Ok(_) => {}
                            Err(_) => self.gaps.push(
                                json!({"record": "changes", "reason": "ledger entry unavailable"}),
                            ),
                        }
                    } else {
                        self.gaps.push(
                            json!({"record": "changes", "reason": "ledger entry unavailable"}),
                        );
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => self
                .gaps
                .push(json!({"record": "changes", "reason": "ledger directory unavailable"})),
        }
        ids.into_iter().collect()
    }

    pub fn events(&mut self, change: &str) -> Vec<Event> {
        let directory = if change == Store::REPOSITORY_SCOPE {
            self.store.root.join("repository/events")
        } else {
            self.store.root.join("changes").join(change).join("events")
        };
        let mut values: BTreeMap<String, Value> = BTreeMap::new();
        let mut conflicts = BTreeSet::new();
        let mut assignment = BTreeMap::new();
        let mut incoming = Vec::new();
        for path in paths(&directory, change, &mut self.gaps) {
            let value = regular(&path)
                .then(|| {
                    std::fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                })
                .flatten();
            match value {
                Some(value) if value["event_id"].as_str() == path.file_stem().and_then(|stem| stem.to_str()) => incoming.push(value),
                _ => self.gaps.push(json!({"record": record_id(change, &path), "reason": "ledger record unreadable"})),
            }
        }
        for batch in &self.batches {
            if change == Store::REPOSITORY_SCOPE {
                incoming.extend(batch.repository_events.iter().cloned());
            } else if batch.change_id == change {
                incoming.extend(batch.events.iter().cloned());
            }
        }
        for value in incoming {
            let Some(id) = value["event_id"]
                .as_str()
                .filter(|id| crate::ids::validate_id_component(id).is_ok())
            else {
                self.gaps
                    .push(json!({"record": change, "reason": "ledger identity unreadable"}));
                continue;
            };
            let unknown_assignment = assignment.entry(id.to_string()).or_insert(false);
            *unknown_assignment |= may_assign(&value);
            if value["change_id"] != change || values.get(id).is_some_and(|held| *held != value) {
                conflicts.insert(id.to_string());
            } else {
                values.insert(id.to_string(), value);
            }
        }
        for id in &conflicts {
            self.gaps.push(
                json!({"record": format!("{change}:{id}"), "reason": "ledger identity conflict", "assignment": assignment[id]}),
            );
        }
        let mut events = Vec::new();
        for (id, value) in values {
            if conflicts.contains(&id) {
                continue;
            }
            match serde_json::from_value::<Event>(value) {
                Ok(event) if !matches!(event.payload, Payload::Unknown) => events.push(event),
                _ => self.gaps.push(json!({"record": format!("{change}:{id}"), "reason": "ledger record unsupported or corrupt", "assignment": assignment[&id]})),
            }
        }
        events
    }
}
