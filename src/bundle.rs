use crate::ids;
use crate::model::{ClaimStage, Event, Payload, StageBudget};
use crate::state;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

pub const BUNDLE_SCHEMA: &str = "arc-bundle/6";

/// The bundle versions import accepts: the one export writes and the one
/// before it, whose every bundle is also a valid bundle of the current shape.
pub const READABLE_BUNDLE_SCHEMAS: &[&str] = &[BUNDLE_SCHEMA, "arc-bundle/5"];

/// The prefix a delta bundle extends: the checksum of the exported history
/// before its suffix, and how many events that prefix covers. A bundle's
/// `events_sha256` is the checksum of the whole history, so it doubles as the
/// token a later `--since` names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePrefix {
    pub sha256: String,
    pub event_count: usize,
}

/// One Git object and the purpose for which the exported events cite it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleObject {
    pub oid: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub schema: String,
    pub repository_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_replica_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub change_id: String,
    pub event_count: usize,
    pub events_sha256: String,
    /// The prefix this bundle carries only the suffix after, when the caller
    /// named one with `--since`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<BundlePrefix>,
    /// The store format the exporting build wrote. An importer that would skip
    /// event types it does not know refuses instead: skipping a lifecycle
    /// event means reading a closed change as open, and then closing it a
    /// second way.
    #[serde(default = "legacy_store_format")]
    pub store_format: u32,
    pub events: Vec<Value>,
    /// None identifies a bundle whose object availability is advisory.
    /// Some, including an empty list, requires availability before import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objects: Option<Vec<BundleObject>>,
    /// Repository-scoped events, carried so a change's recorded revisions stay
    /// followable after transport. A rewrite is not change-scoped, but a
    /// bundle that drops it hands the receiver revisions nothing can resolve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repository_events: Vec<Value>,
    /// The digest of the custody sidecar's manifest, present only when the
    /// export wrote a sidecar. Import judges the sidecar against it, so a
    /// replaced or foreign sidecar is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custody: Option<crate::custody::Binding>,
    /// Full related change histories required by source references and adoption.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_changes: Vec<Bundle>,
    // Validated at parse, not at write: an import that discovered a malformed
    // one halfway through would already have written change events.
}

/// Bundles written before the format was carried came from a build that wrote
/// store format 1.
fn legacy_store_format() -> u32 {
    1
}

impl Bundle {
    /// Export one change. `since` names the checksum of a prefix the caller
    /// holds; the bundle then carries only the events after it, and its own
    /// `events_sha256` remains the checksum of the whole history so the
    /// receiver can verify the suffix against that prefix.
    pub fn export(store: &Store, cwd: &Path, change_id: &str, since: Option<&str>) -> Result<Self> {
        let mut bundle = Self::export_one(store, cwd, change_id, since)?;
        let repository = store.load_repository_events()?;
        let mut pending = vec![change_id.to_string()];
        let mut seen = BTreeSet::new();
        let mut related = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let events = store.load_events(&id)?;
            for event in events.iter().chain(&repository) {
                if event.change_id == Store::REPOSITORY_SCOPE {
                    pending.extend(crate::forge_round_trip::source_changes(&event.payload));
                }
                if event.change_id == id {
                    pending.extend(crate::forge_round_trip::source_changes(&event.payload));
                    if let Payload::ChangeClosed {
                        superseded_by: Some(successor),
                        ..
                    } = &event.payload
                    {
                        pending.push(successor.clone());
                    }
                }
                if let Payload::CandidateSelected {
                    candidate_id,
                    destination,
                    ..
                } = &event.payload
                {
                    if destination != &id {
                        continue;
                    }
                    let mut candidates = vec![candidate_id.clone()];
                    let mut visited = BTreeSet::new();
                    while let Some(candidate) = candidates.pop() {
                        if !visited.insert(candidate.clone()) {
                            continue;
                        }
                        for registered in &repository {
                            match &registered.payload {
                                Payload::CandidateRegistered {
                                    candidate_id,
                                    brief,
                                    parents,
                                    adopts,
                                    ..
                                } if candidate_id == &candidate => {
                                    pending.push(brief.change_id.clone());
                                    candidates.extend(parents.iter().chain(adopts).cloned());
                                }
                                Payload::ContextDeclared {
                                    subject:
                                        crate::model::RelationSubject::Candidate { candidate_id },
                                    ..
                                } if candidate_id == &candidate => {
                                    pending.extend(crate::forge_round_trip::source_changes(
                                        &registered.payload,
                                    ));
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            if id != change_id {
                related.insert(id);
            }
        }
        for id in related {
            bundle
                .related_changes
                .push(Self::export_one(store, cwd, &id, None)?);
        }
        let mut objects: BTreeSet<_> = bundle
            .objects
            .take()
            .unwrap_or_default()
            .into_iter()
            .collect();
        for related in &bundle.related_changes {
            objects.extend(related.objects.clone().unwrap_or_default());
        }
        bundle.objects = Some(objects.into_iter().collect());
        Ok(bundle)
    }

    fn export_one(store: &Store, cwd: &Path, change_id: &str, since: Option<&str>) -> Result<Self> {
        ids::validate_id_component(change_id)?;
        let (raw, repository_events) = {
            let _cursor = store.lock_event_cursor()?;
            let reservation = store.oldest_event_reservation()?;
            let mut raw = store.raw_events(change_id)?;
            let mut repository =
                store.raw_repository_events_unseen(&std::collections::BTreeSet::new())?;
            if let Some(reserved) = &reservation {
                raw.retain(|(id, _)| id < reserved);
                repository.retain(|(id, _)| id < reserved);
                if raw.is_empty() {
                    bail!(
                        "change {change_id} export waits below provisional reservation {reserved}"
                    );
                }
            }
            (
                raw,
                repository
                    .into_iter()
                    .map(|(_, value)| value)
                    .collect::<Vec<_>>(),
            )
        };
        if raw.is_empty() {
            bail!("change {change_id:?} has no events");
        }

        let mut all = Vec::with_capacity(raw.len());
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
            all.push(value);
        }
        all.sort_by(|a, b| {
            event_id(a)
                .expect("validated event")
                .cmp(event_id(b).expect("validated event"))
        });
        // The opening event identifies the originating store. Later events
        // may legitimately come from other stores after transfer, and a
        // suffix bundle begins at neither the opener nor its own start.
        let opening = all
            .iter()
            .find(|value| value["event_type"] == "change-opened")
            .context("change has no opening event")?;
        let repository_id = event_envelope(opening)?.repository_id.to_string();
        // The checksum always covers the complete history at export: for a
        // full bundle that is its events, and for a delta it is the prefix
        // plus the suffix, which is what doubles as the next `--since` token
        // and what the receiver verifies the suffix against.
        let history_sha256 = checksum(&all)?;
        // Repository facts travel even when the change's ledger has not
        // advanced: a withdrawal cannot depend on another change event.
        let (events, since) = match since {
            None => (all, None),
            Some(prefix_sha256) => {
                let (prefix_event_count, suffix) =
                    if prefix_sha256 == history_sha256 && !repository_events.is_empty() {
                        (all.len(), Vec::new())
                    } else {
                        split_at_prefix(&all, prefix_sha256)?
                    };
                (
                    suffix,
                    Some(BundlePrefix {
                        sha256: prefix_sha256.to_string(),
                        event_count: prefix_event_count,
                    }),
                )
            }
        };

        let objects = exported_objects(cwd, change_id, &events, &repository_events)?;
        let events_sha256 = history_sha256;
        // The checksum covers the change's events, which is what the receiver
        // replays. Repository events travel beside them: they are context for
        // resolving recorded revisions, not part of this change's history.
        let replica = crate::replica::status(store)?;
        Ok(Bundle {
            schema: BUNDLE_SCHEMA.to_string(),
            store_format: crate::model::SCHEMA_VERSION,
            source_replica_id: Some(store.repository_id.clone()),
            project_id: replica.project_id,
            repository_id,
            change_id: change_id.to_string(),
            event_count: events.len(),
            events_sha256,
            since,
            events,
            objects: Some(objects),
            repository_events,
            custody: None,
            related_changes: Vec::new(),
        })
    }

    pub fn related_events(&self) -> Result<Vec<Event>> {
        self.related_changes
            .iter()
            .flat_map(|b| b.events.iter().chain(&b.repository_events))
            .map(parse_typed_event)
            .collect::<Result<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<ValidatedBundle> {
        let bundle: Bundle =
            serde_json::from_slice(bytes).context("malformed arc export bundle")?;
        for (key, id) in [
            ("source_replica_id", bundle.source_replica_id.as_deref()),
            ("project_id", bundle.project_id.as_deref()),
        ] {
            if let Some(id) = id {
                ids::validate_id_component(id).with_context(|| format!("invalid bundle {key}"))?;
            }
        }
        if bundle.store_format > crate::model::SCHEMA_VERSION {
            bail!(
                "bundle was written by a newer arc (store format {}, this build understands \
                 {}); upgrade arc rather than importing history it would not fully read",
                bundle.store_format,
                crate::model::SCHEMA_VERSION
            );
        }
        if !READABLE_BUNDLE_SCHEMAS.contains(&bundle.schema.as_str()) {
            bail!(
                "unsupported bundle schema {:?}; expected one of {READABLE_BUNDLE_SCHEMAS:?}",
                bundle.schema
            );
        }
        if let Some(objects) = &bundle.objects {
            for object in objects {
                if !valid_object_id(&object.oid) || object.role.is_empty() {
                    bail!(
                        "invalid bundled object {:?} with role {:?}",
                        object.oid,
                        object.role
                    );
                }
            }
        }
        if let Some(custody) = &bundle.custody {
            custody
                .validate()
                .context("invalid bundle custody binding")?;
        }
        ids::validate_id_component(&bundle.repository_id)?;
        ids::validate_id_component(&bundle.change_id)?;
        // Repository events are validated here, before anything is written:
        // an import that found a malformed one halfway through would already
        // have appended change events, and a stored malformed envelope breaks
        // every later read of the rewrite map.
        for value in &bundle.repository_events {
            let envelope = event_envelope(value)
                .context("bundled repository event is not a well-formed event")?;
            ids::validate_id_component(envelope.event_id)?;
            if envelope.change_id != Store::REPOSITORY_SCOPE {
                bail!(
                    "bundled repository event {} names change {:?}, not the repository scope",
                    envelope.event_id,
                    envelope.change_id
                );
            }
            let typed = parse_typed_event(value).with_context(|| {
                format!(
                    "bundled repository event {} is malformed",
                    envelope.event_id
                )
            })?;
            // A bundle carries the payload directly, so the rules the
            // recording path enforces have to be enforced here too. Otherwise
            // an imported map says something no recorded map could — a
            // rewrite to the zero object, where "dropped" is spelled as no
            // successor at all.
            if let Some(crate::model::Payload::HistoryRewritten {
                mapping, retained, ..
            }) = typed.as_ref().map(|event| &event.payload)
            {
                crate::rewrite::validate_mapping(mapping).with_context(|| {
                    format!(
                        "bundled repository event {} records a mapping it cannot mean",
                        envelope.event_id
                    )
                })?;
                // An import writes refs from what this names, so a name that
                // is no retention ref is refused before anything is written.
                for (name, commit) in retained {
                    if !name.starts_with(crate::gitio::RETAINED_PREFIX) || !valid_object_id(commit)
                    {
                        bail!(
                            "bundled repository event {} retains {name:?} at {commit:?}, which \
                             is not a retention ref holding a commit",
                            envelope.event_id
                        );
                    }
                }
            }
        }
        if bundle.event_count != bundle.events.len() {
            bail!(
                "event_count {} does not match {} bundled events",
                bundle.event_count,
                bundle.events.len()
            );
        }
        if !valid_checksum(&bundle.events_sha256) {
            bail!(
                "events checksum {:?} is not a sha256 digest",
                bundle.events_sha256
            );
        }
        match &bundle.since {
            None => {
                let actual_checksum = checksum(&bundle.events)?;
                if bundle.events_sha256 != actual_checksum {
                    bail!(
                        "events checksum mismatch: bundle says {}, computed {actual_checksum}",
                        bundle.events_sha256
                    );
                }
            }
            Some(prefix) => {
                if !valid_checksum(&prefix.sha256) {
                    bail!(
                        "bundle names prefix checksum {:?}, which is not a sha256 digest",
                        prefix.sha256
                    );
                }
                if prefix.event_count == 0 {
                    bail!("bundle names an empty prefix to extend");
                }
                // A delta bundle's checksum covers the prefix and the suffix
                // together, so it is verified against the receiving store's
                // prefix rather than against the suffix alone.
                if bundle.events.is_empty() && bundle.repository_events.is_empty() {
                    bail!("a delta bundle must carry the events after its prefix");
                }
            }
        }

        let mut events = Vec::with_capacity(bundle.events.len());
        let mut patchsets = Vec::new();
        let mut unknown_event_types = Vec::new();
        let mut prior_event_id: Option<String> = None;
        let mut patchset_ids = HashSet::new();
        for value in &bundle.events {
            let envelope = event_envelope(value)?;
            if prior_event_id.is_none()
                && bundle.since.is_none()
                && envelope.repository_id != bundle.repository_id
            {
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
                typed,
            });
        }

        let mut related_ids = BTreeSet::from([bundle.change_id.clone()]);
        for related in &bundle.related_changes {
            anyhow::ensure!(
                related.related_changes.is_empty()
                    && related.since.is_none()
                    && related.custody.is_none(),
                "related change must be a flat full history sharing the root custody manifest"
            );
            anyhow::ensure!(
                related.source_replica_id == bundle.source_replica_id
                    && related.project_id == bundle.project_id,
                "related change identity disagrees with exporting bundle"
            );
            anyhow::ensure!(
                related_ids.insert(related.change_id.clone()),
                "duplicate related change"
            );
            Self::parse(&related.to_bytes()?)?;
        }
        Ok(ValidatedBundle {
            bundle,
            events,
            patchsets,
            unknown_event_types,
        })
    }
}

fn valid_object_id(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The closure covers the transported events, including repository facts,
/// rather than the receiving state or the prefix of a delta.
fn exported_objects(
    cwd: &Path,
    change_id: &str,
    events: &[Value],
    repository_events: &[Value],
) -> Result<Vec<BundleObject>> {
    let mut objects = BTreeSet::new();
    for value in events.iter().chain(repository_events) {
        let Some(event) = parse_typed_event(value)? else {
            continue;
        };
        for commit in crate::forge_round_trip::disclosure_commits(&event.payload) {
            if crate::gitio::commit_exists(cwd, commit)? {
                insert_object(cwd, &mut objects, commit, "disclosure-commit")?;
            }
        }
        // A commit a provider reported is a claim about the provider. It
        // travels only where this repository holds it, so an exporter that
        // never fetched it does not oblige the receiver to.
        if let Some(observed) = crate::forge_observation::observed_commits(&event.payload) {
            for (_, revision) in observed {
                if let Ok(oid) = crate::gitio::rev_parse(cwd, revision) {
                    insert_object(cwd, &mut objects, &oid, "observed-commit")?;
                }
            }
            continue;
        }
        // The commits and trees an external run names are its claims. Like a
        // provider's, they travel only where this repository holds them.
        if let Payload::CandidateEvidenceReceived { evidence, .. } = &event.payload {
            for (_, head) in evidence.heads.each() {
                if crate::gitio::commit_exists(cwd, &head.commit)? {
                    insert_object(cwd, &mut objects, &head.commit, "received-commit")?;
                }
                if crate::gitio::resolve_tree(cwd, &head.tree)?.as_deref() == Some(&head.tree) {
                    insert_object(cwd, &mut objects, &head.tree, "received-tree")?;
                }
            }
            continue;
        }
        let payload = serde_json::to_value(&event.payload)?;
        let event_type = payload["event_type"].as_str().unwrap_or_default();
        collect_payload_objects(cwd, &payload, event_type, &mut objects)?;
        if let Payload::VerificationRecorded {
            revision,
            against_target,
            tested_tree,
            ..
        } = &event.payload
        {
            if let Some(tree) = tested_tree {
                insert_object(cwd, &mut objects, tree, "retention-commit")?;
            }
            let name = crate::gitio::tree_retention_ref(change_id, &event.event_id);
            if let Some(oid) = crate::gitio::ref_value(cwd, &name)? {
                insert_object(cwd, &mut objects, &oid, "retention-commit")?;
            }
            if against_target.is_some() {
                // The event names the merge even when its retention ref was released.
                insert_object(cwd, &mut objects, revision, "synthesized-merge")?;
                insert_object(cwd, &mut objects, revision, "retention-commit")?;
                let name = crate::gitio::merge_retention_ref(change_id, revision);
                if let Some(oid) = crate::gitio::ref_value(cwd, &name)? {
                    insert_object(cwd, &mut objects, &oid, "retention-commit")?;
                }
            }
        }
        if let Payload::VerificationRunStarted { revision, .. }
        | Payload::VerificationReused { revision, .. } = &event.payload
        {
            let name = crate::gitio::merge_retention_ref(change_id, revision);
            if let Some(oid) = crate::gitio::ref_value(cwd, &name)? {
                insert_object(cwd, &mut objects, revision, "synthesized-merge")?;
                insert_object(cwd, &mut objects, &oid, "retention-commit")?;
            }
        }
        if let Payload::HistoryRewritten {
            mapping,
            refs,
            retained,
            ..
        } = &event.payload
        {
            for (old, new) in mapping {
                insert_object(cwd, &mut objects, old, "rewrite-source")?;
                if let Some(new) = new {
                    insert_object(cwd, &mut objects, new, "rewrite-successor")?;
                }
            }
            for moved in refs.values() {
                insert_object(cwd, &mut objects, &moved.old, "rewrite-source")?;
                insert_object(cwd, &mut objects, &moved.new, "rewrite-successor")?;
            }
            for (_, commit) in retained_for(retained, change_id) {
                insert_object(cwd, &mut objects, &commit, "retention-commit")?;
            }
        }
    }
    Ok(objects.into_iter().collect())
}

/// The retention refs a rewrite event names for one change, each as the name
/// this repository gives it and the commit it holds.
///
/// The name is rebuilt from the change, the subject and the commit rather than
/// taken as written: an import writes these refs, and a name that merely
/// begins like a retention ref could end anywhere.
pub fn retained_for(
    retained: &std::collections::BTreeMap<String, String>,
    change_id: &str,
) -> Vec<(String, String)> {
    let prefix = crate::gitio::retained_prefix(change_id);
    retained
        .iter()
        .filter_map(|(name, commit)| {
            let (subject, named) = name.strip_prefix(&prefix)?.rsplit_once('/')?;
            if named != commit
                || !valid_object_id(commit)
                || ids::validate_id_component(subject).is_err()
            {
                return None;
            }
            Some((
                crate::gitio::retained_ref(change_id, subject, commit),
                commit.clone(),
            ))
        })
        .collect()
}

fn insert_object(
    cwd: &Path,
    objects: &mut BTreeSet<BundleObject>,
    revision: &str,
    role: &str,
) -> Result<()> {
    let oid = if valid_object_id(revision) {
        revision.to_owned()
    } else {
        crate::gitio::rev_parse(cwd, revision)
            .with_context(|| format!("cannot identify exported {role} object {revision:?}"))?
    };
    objects.insert(BundleObject {
        oid,
        role: role.to_owned(),
    });
    Ok(())
}

/// Only the typed payload is traversed: unknown events and arbitrary message
/// metadata have no object-field contract. Nested anchors, file requirements,
/// read evidence and authorization carry the same Git-object field names.
fn collect_payload_objects(
    cwd: &Path,
    value: &Value,
    event_type: &str,
    objects: &mut BTreeSet<BundleObject>,
) -> Result<()> {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_payload_objects(cwd, value, event_type, objects)?;
            }
        }
        Value::Object(fields) => {
            for (field, value) in fields {
                if matches!(field.as_str(), "metadata" | "mapping" | "refs") {
                    continue;
                }
                let role = match field.as_str() {
                    "base" if event_type == "patchset-added" => Some("patchset-base"),
                    "head" if event_type == "patchset-added" => Some("patchset-head"),
                    "commit" => Some("disposition-commit"),
                    "tree" | "tested_tree"
                        if matches!(
                            event_type,
                            "verification-recorded" | "verification-reused" | "candidate-verified"
                        ) =>
                    {
                        Some("evaluated-tree")
                    }
                    "tree" => Some("content-tree"),
                    "blob" => Some("cited-blob"),
                    "provisional_commit" => Some("provisional-merge"),
                    "evaluated_tree" => Some("evaluated-tree"),
                    "staging_before" => Some("staging-base"),
                    "integrated_commit" | "merge_sha" => Some("integration-commit"),
                    "against_target" | "target_before" | "target_revision" => Some("target-commit"),
                    "target" if event_type == "candidate-selected" => Some("target-commit"),
                    "base" | "head" | "merge_base" | "base_revision" | "revision"
                    | "source_head" | "reviewed_head" | "head_sha" | "pr_head" | "compared_at"
                    | "at" => Some("cited-commit"),
                    _ => None,
                };
                if let (Some(role), Some(oid)) = (role, value.as_str()) {
                    insert_object(cwd, objects, oid, role)?;
                }
                collect_payload_objects(cwd, value, event_type, objects)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub struct ValidatedBundle {
    pub bundle: Bundle,
    pub events: Vec<ValidatedEvent>,
    pub patchsets: Vec<PatchsetObject>,
    pub unknown_event_types: Vec<(String, String)>,
}

impl ValidatedBundle {
    /// A full history replays with the receiver's admitted identity proof.
    /// Delta histories require the receiver's prefix and replay at import.
    pub fn validate_replay(&self, rewrites: &crate::rewrite::RewriteMap) -> Result<()> {
        if self.bundle.since.is_none() {
            let events = self
                .events
                .iter()
                .filter_map(|event| event.typed.clone())
                .collect::<Vec<_>>();
            let replay = if rewrites.is_empty() {
                state::reduce(&events)
            } else {
                state::reduce_with_rewrites(&events, rewrites)
            };
            replay.context("bundled known events are not replayable")?;
        }
        Ok(())
    }
}

pub struct ValidatedEvent {
    pub event_id: String,
    pub value: Value,
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

pub(crate) fn event_id(value: &Value) -> Option<&str> {
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

fn valid_checksum(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// The count of events an earlier export digested, and the suffix after them.
///
/// Every export prints the checksum of the history it represents, so that
/// checksum names a prefix of every later export of the same change. A
/// checksum no prefix produces means the caller holds a different history.
fn split_at_prefix(events: &[Value], prefix_sha256: &str) -> Result<(usize, Vec<Value>)> {
    if !valid_checksum(prefix_sha256) {
        bail!(
            "--since takes the 64-character events checksum an earlier export printed, \
             not {prefix_sha256:?}"
        );
    }
    let mut digest = Sha256::new();
    let mut matched = None;
    for (index, event) in events.iter().enumerate() {
        digest.update(serde_json::to_vec(event)?);
        digest.update(b"\n");
        if matched.is_none() && hex::encode(digest.clone().finalize()) == prefix_sha256 {
            matched = Some(index + 1);
        }
    }
    let Some(event_count) = matched else {
        bail!(
            "no prefix of this change's history has checksum {prefix_sha256}; \
             export the change in full"
        );
    };
    if event_count == events.len() {
        bail!(
            "this change's history already ends at checksum {prefix_sha256}; \
             there is nothing to export"
        );
    }
    Ok((event_count, events[event_count..].to_vec()))
}

#[cfg(test)]
mod checksum_tests {
    use super::*;

    #[test]
    fn struct_ordered_events_use_sorted_key_checksum_and_prefix() {
        let mut events: Vec<Value> = serde_json::from_str(
            r#"[{"schema_version":7,"event_id":"event-1","repository_id":"repo-1","payload":{"event_type":"opened","title":"Example"}}]"#,
        ).unwrap();
        // SHA-256 of compact, sorted-key event JSON followed by a newline.
        let expected = "2d361771d797e82bd92ac1cd8dd1a952132fbdbada8d77a0c330917ac63cec37";
        assert_eq!(checksum(&events).unwrap(), expected);
        events.push(serde_json::json!({"event_id": "event-2"}));
        let (count, suffix) = split_at_prefix(&events, expected).unwrap();
        assert_eq!(count, 1);
        assert_eq!(suffix, events[1..]);
    }
}
