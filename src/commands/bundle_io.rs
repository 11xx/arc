use super::*;

pub fn export_bundle(ctx: &Ctx, reference: &str, output: &str, since: Option<&str>) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let bundle = Bundle::export(&store, &change_id, since)?;
    let bytes = bundle.to_bytes()?;
    let mut reported = format!("events: {}", bundle.event_count);
    if let Some(prefix) = &bundle.since {
        reported.push_str(&format!(
            "\nprefix: {} ({} events)",
            prefix.sha256, prefix.event_count
        ));
    }
    reported.push_str(&format!("\nsha256: {}", bundle.events_sha256));
    if output == "-" {
        std::io::stdout().write_all(&bytes)?;
        eprintln!("{reported}\noutput: -");
    } else {
        std::fs::write(output, bytes)
            .with_context(|| format!("cannot write export bundle {output}"))?;
        println!("{reported}\noutput: {output}");
    }
    Ok(())
}

pub fn import_bundle(ctx: &Ctx, input: &str, dry_run: bool) -> Result<i32> {
    let bytes = if input == "-" {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        bytes
    } else {
        std::fs::read(input).with_context(|| format!("cannot read import bundle {input}"))?
    };
    // Parsing validates every path-bearing ID, checksum, envelope, and
    // patchset field before the local store is inspected or created.
    let validated = Bundle::parse(&bytes)?;
    let root = Store::resolve_root(&ctx.cwd)?;
    let local_repository_id = Store::repository_id_at(&root)?;
    let local_store = local_repository_id.as_ref().map(|repository_id| Store {
        root: root.clone(),
        repository_id: repository_id.clone(),
        require_declared_actor: false,
        require_declared_actor_sources: Vec::new(),
    });
    // A suffix is only meaningful against the prefix it extends, and the
    // checksum of the whole history is what ties the two together.
    verify_bundle_prefix(local_store.as_ref(), &validated)?;

    let mut missing_objects = Vec::new();
    let mut pins = Vec::new();
    for patchset in &validated.patchsets {
        if !gitio::commit_exists(&ctx.cwd, &patchset.base)? {
            missing_objects.push((patchset.event_id.clone(), "base", patchset.base.clone()));
        }
        if gitio::commit_exists(&ctx.cwd, &patchset.head)? {
            pins.push((
                gitio::retention_ref(&validated.bundle.change_id, &patchset.patchset_id),
                patchset.head.clone(),
            ));
        } else {
            missing_objects.push((patchset.event_id.clone(), "head", patchset.head.clone()));
        }
    }

    if dry_run {
        let plan = classify_import_events(&root, &validated)?;
        let mut claim_contest = None;
        if plan.conflicts.is_empty() {
            let store = local_store.as_ref();
            if let Some(store) = store {
                claim_contest = claim_contest_for_import(store, &validated, &plan.new_events)?;
            }
            if claim_contest.is_none() {
                validate_import_candidate(store, &validated, &plan.new_events)?;
            }
            // The same refusals the import makes: a preflight that reports
            // success for a bundle the real path rejects is believed, and
            // wrong. A destination with no store still checks the bundle
            // against itself.
            if claim_contest.is_none() {
                plan_repository_events(&ctx.cwd, store, &validated)?;
            }
        }
        print_import_report(
            &validated,
            local_repository_id.as_deref(),
            &plan.new_events,
            &plan.skipped_events,
            &plan.conflicts,
            &missing_objects,
            &pins,
            true,
        );
        if !plan.conflicts.is_empty() {
            println!("aborted: no events or refs written");
            return Ok(1);
        }
        if let Some(contest) = claim_contest {
            println!("{}", contest.render(&validated.bundle.change_id));
            println!("aborted: no events or refs written");
            return Ok(1);
        }
        return Ok(0);
    }

    let store = Store::discover(&ctx.cwd)?;
    let _replica_state = crate::replica::lock(&store)?;
    let transition = store.lock_transition(&validated.bundle.change_id)?;
    // The prefix is judged under the same lock as the replay it authorises,
    // so a concurrent import cannot move the history between the two.
    verify_bundle_prefix(Some(&store), &validated)?;
    // Classification and candidate replay must happen after taking the same
    // per-change lock used by claim, release, stage, and snapshot. Otherwise a
    // local transition could land between validation and the raw appends.
    let plan = classify_import_events(&root, &validated)?;
    let claim_contest = if plan.conflicts.is_empty() {
        claim_contest_for_import(&store, &validated, &plan.new_events)?
    } else {
        None
    };
    if plan.conflicts.is_empty() && claim_contest.is_none() {
        validate_import_candidate(Some(&store), &validated, &plan.new_events)?;
    }

    print_import_report(
        &validated,
        local_repository_id.as_deref(),
        &plan.new_events,
        &plan.skipped_events,
        &plan.conflicts,
        &missing_objects,
        &pins,
        false,
    );
    if !plan.conflicts.is_empty() {
        println!("aborted: no events or refs written");
        return Ok(1);
    }
    if let Some(contest) = claim_contest {
        println!("{}", contest.render(&validated.bundle.change_id));
        println!("aborted: no events or refs written");
        return Ok(1);
    }

    if local_repository_id.is_none() && store.repository_id != validated.bundle.repository_id {
        println!(
            "repository: bundle {} differs from local {} (expected for cross-machine import)",
            validated.bundle.repository_id, store.repository_id
        );
    }
    // Every repository event is checked before the first one is written, and
    // all of them before any change event: an import that discovered a
    // contradiction halfway would leave one rewrite recorded, another not, and
    // a change whose revisions resolve through half a map.
    // Held across planning and writing: the combined map is judged as a
    // whole, so two imports of different changes must not interleave between
    // the judgement and the write that makes it true.
    let _repository_events = store.lock_repository_events()?;
    let incoming = plan_repository_events(&ctx.cwd, Some(&store), &validated)?;
    let mut rewrites = 0;
    for (event_id, bytes) in &incoming {
        if store.append_raw_repository_event(event_id, bytes)? {
            rewrites += 1;
        }
    }
    for event in &validated.events {
        if plan.new_events.contains(&event.event_id) {
            store.append_raw_event(&validated.bundle.change_id, &event.event_id, &event.bytes)?;
        }
    }
    if rewrites > 0 {
        println!("repository events: {rewrites} imported");
    }
    let (pinned, unpinned) =
        super::candidate::pin_imported(&ctx.cwd, &store, &incoming.keys().cloned().collect())?;
    for candidate_id in pinned {
        println!("candidate {candidate_id}: pinned");
    }
    for candidate_id in unpinned {
        println!("candidate {candidate_id}: tree not held here; pin absent");
    }
    drop(transition);
    for (name, head) in pins {
        gitio::update_ref(&ctx.cwd, &name, &head)?;
    }
    Ok(0)
}

/// A delta bundle carries the suffix of a history. The receiver must hold
/// the prefix the bundle names: its first `event_count` events, in event-id
/// order, must checksum to `sha256`, and that same prefix followed by the
/// bundled events must checksum to the bundle's own `events_sha256`. Both
/// halves are checked before the first write, so a suffix over a different
/// history, or a tampered one, is refused with nothing imported.
fn verify_bundle_prefix(store: Option<&Store>, validated: &ValidatedBundle) -> Result<()> {
    let Some(prefix) = &validated.bundle.since else {
        return Ok(());
    };
    let Some(store) = store else {
        bail!(
            "this destination holds no store, so it cannot hold prefix checksum {}; \
             nothing was imported",
            prefix.sha256
        );
    };
    let mut held = Vec::new();
    if store
        .list_change_ids()?
        .iter()
        .any(|change_id| change_id == &validated.bundle.change_id)
    {
        held = store
            .raw_events(&validated.bundle.change_id)?
            .into_iter()
            .map(|(_, value)| value)
            .collect();
    }
    if held.len() < prefix.event_count {
        bail!(
            "this store holds {} events for change {}; the bundle extends a prefix of {}; \
             nothing was imported",
            held.len(),
            validated.bundle.change_id,
            prefix.event_count
        );
    }
    let head = &held[..prefix.event_count];
    let actual = crate::bundle::checksum(head)?;
    if actual != prefix.sha256 {
        bail!(
            "the receiving store's first {} events for change {} checksum to {actual}, \
             the bundle extends {}; nothing was imported",
            prefix.event_count,
            validated.bundle.change_id,
            prefix.sha256
        );
    }
    let mut combined = head.to_vec();
    combined.extend(validated.bundle.events.iter().cloned());
    let history = crate::bundle::checksum(&combined)?;
    if history != validated.bundle.events_sha256 {
        bail!(
            "the bundle's checksum {} does not cover prefix {} and its events; \
             nothing was imported",
            validated.bundle.events_sha256,
            prefix.sha256
        );
    }
    Ok(())
}

fn claim_contest_for_import(
    store: &Store,
    validated: &ValidatedBundle,
    new_event_ids: &[String],
) -> Result<Option<crate::replica::ClaimContest>> {
    let incoming = validated
        .events
        .iter()
        .filter_map(|event| event.typed.clone())
        .collect::<Vec<_>>();
    let new_event_ids = new_event_ids.iter().cloned().collect::<BTreeSet<_>>();
    crate::replica::live_claim_contest(
        store,
        &validated.bundle.change_id,
        &incoming,
        &new_event_ids,
    )
}

/// The repository events an import would write, refusing every contradiction
/// before the first is written — including two bundled events sharing an ID,
/// which no check against the destination can see.
fn plan_repository_events(
    cwd: &Path,
    store: Option<&Store>,
    validated: &ValidatedBundle,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut incoming: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for value in &validated.bundle.repository_events {
        let Some(event_id) = value.get("event_id").and_then(serde_json::Value::as_str) else {
            bail!("bundled repository event has no event_id");
        };
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        if store.is_some_and(|store| {
            store
                .repository_event_conflicts(event_id, &bytes)
                .unwrap_or(false)
        }) {
            bail!(
                "repository event {event_id} already exists here with different content; \
                 nothing was imported"
            );
        }
        if let Some(existing) = incoming.get(event_id) {
            if existing != &bytes {
                bail!(
                    "the bundle carries two different repository events with ID {event_id}; \
                     nothing was imported"
                );
            }
        }
        incoming.insert(event_id.to_string(), bytes);
    }
    // Two events with different IDs can still disagree about one revision, and
    // no per-event check sees that. The combined map is the thing that has to
    // hold together, so it is built before anything is written.
    let mut combined = match store {
        Some(store) => store.load_repository_events()?,
        // A destination with no store holds nothing to contradict, but the
        // bundle must still hold together on its own.
        None => Vec::new(),
    };
    let held: BTreeSet<String> = combined
        .iter()
        .map(|event| event.event_id.clone())
        .collect();
    for value in &validated.bundle.repository_events {
        if let Some(event) = crate::bundle::parse_typed_event(value)? {
            if !held.contains(&event.event_id) {
                combined.push(event);
            }
        }
    }
    combined.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    let rewrites = crate::rewrite::RewriteMap::from_events(combined.iter())
        .context("the bundle's rewrites contradict this repository's; nothing was imported")?;
    // Candidate events are judged with the ledger they join, by every rule
    // that needs no local object: a parent or an adoption may arrive in the
    // same bundle or already be here, and a tree this object store lacks is
    // not a refusal.
    super::candidate::Ledger::replay(&combined).map_err(|refusal| {
        anyhow::anyhow!(
            "the bundle's candidate events are refused: {refusal}; nothing was imported"
        )
    })?;
    crate::relations::Relations::replay(&combined).map_err(|refusal| {
        anyhow::anyhow!(
            "the bundle's candidate relations are refused: {refusal}; nothing was imported"
        )
    })?;
    let borrowed: Vec<_> = combined.iter().collect();
    let withdrawn = crate::rewrite::withdrawn_event_ids(&borrowed)?;
    // Whether a successor differs from its old commit by signature alone is
    // this repository's judgement to make, against the objects it holds. What
    // the sender recorded is a claim about the sender's objects, and taking it
    // would let a bundle move an approval onto content nobody reviewed.
    for value in &validated.bundle.repository_events {
        let Some(event) = crate::bundle::parse_typed_event(value)? else {
            continue;
        };
        let Payload::HistoryRewritten { mapping, .. } = &event.payload else {
            continue;
        };
        if held.contains(&event.event_id) || withdrawn.contains(event.event_id.as_str()) {
            continue;
        }
        let judged = crate::rewrite::signature_only_successors(cwd, mapping, &rewrites);
        let mut value = value.clone();
        if let Some(object) = value.as_object_mut() {
            object.remove(crate::store::SIGNATURE_ONLY_FIELD);
            if !judged.is_empty() {
                object.insert(
                    crate::store::SIGNATURE_ONLY_FIELD.to_string(),
                    serde_json::json!(judged),
                );
            }
        }
        let mut bytes = serde_json::to_vec_pretty(&value)?;
        bytes.push(b'\n');
        incoming.insert(event.event_id.clone(), bytes);
    }
    Ok(incoming)
}

fn classify_import_events(root: &Path, validated: &ValidatedBundle) -> Result<ImportEventPlan> {
    let mut plan = ImportEventPlan {
        new_events: Vec::new(),
        skipped_events: Vec::new(),
        conflicts: Vec::new(),
    };
    for event in &validated.events {
        match Store::raw_event_at(root, &validated.bundle.change_id, &event.event_id)? {
            None => plan.new_events.push(event.event_id.clone()),
            Some(existing) => match serde_json::from_slice::<serde_json::Value>(&existing) {
                Ok(value) if value == event.value => {
                    plan.skipped_events.push(event.event_id.clone())
                }
                _ => plan.conflicts.push(event.event_id.clone()),
            },
        }
    }
    Ok(plan)
}

/// Whether the bundle's events, combined with whatever this store already
/// holds, form a history that can exist. `None` is a destination with no store
/// yet: there is nothing local to combine with, and the bundle must still
/// stand on its own — otherwise a dry run against a fresh destination reports
/// success for an import that will fail.
fn validate_import_candidate(
    store: Option<&Store>,
    validated: &ValidatedBundle,
    new_events: &[String],
) -> Result<()> {
    let mut candidate = Vec::new();
    if let Some(store) = store {
        if store
            .list_change_ids()?
            .iter()
            .any(|change_id| change_id == &validated.bundle.change_id)
        {
            for (_, value) in store.raw_events(&validated.bundle.change_id)? {
                if let Some(event) = crate::bundle::parse_typed_event(&value)? {
                    candidate.push(event);
                }
            }
        }
    }
    let new_events = new_events.iter().collect::<BTreeSet<_>>();
    candidate.extend(
        validated
            .events
            .iter()
            .filter(|event| new_events.contains(&event.event_id))
            .filter_map(|event| event.typed.clone()),
    );
    candidate.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    state::reduce(&candidate)
        .context("combined local and bundled known events are not replayable")?;
    // Relations on the change are judged by the rules a local write is.
    crate::relations::Relations::replay(&candidate).map_err(|refusal| {
        anyhow::anyhow!("the bundle's relations on the change are refused: {refusal}")
    })?;
    // Replayability is not admissibility. A bundle legitimately carries the
    // lifecycle events a command would not append by hand, so the CLI's own
    // permission table is the wrong question here; what an import must still
    // refuse is a history that contradicts itself — a change closed twice,
    // work recorded after it closed, or post-integration work recorded before
    // it integrated.
    let mut closed_at: Option<&str> = None;
    let mut integrated = false;
    for event in &candidate {
        let terminal = matches!(
            event.payload,
            Payload::ChangeClosed { .. }
                | Payload::ChangeIntegrated { .. }
                | Payload::IntegrationAsserted { .. }
        );
        if let Some(first) = closed_at {
            if terminal {
                bail!(
                    "bundle closes {} twice: {first}, then {}",
                    validated.bundle.change_id,
                    event.event_id
                );
            }
            // What may follow a closure depends on which closure it was: the
            // audit domain records review after an integration, and a
            // changelog entry belongs to something that shipped. Neither
            // belongs after an abandonment.
            let admissible = match append_permission(&event.payload) {
                AppendPermission::AnyPhaseFact => true,
                AppendPermission::IntegratedOnlyFact | AppendPermission::OpenOrIntegratedFact => {
                    integrated
                }
                _ => false,
            };
            if !admissible {
                bail!(
                    "bundled event {} records work after {} closed at {first}",
                    event.event_id,
                    validated.bundle.change_id
                );
            }
        } else if append_permission(&event.payload) == AppendPermission::IntegratedOnlyFact {
            bail!(
                "bundled event {} records post-integration work before {} integrated",
                event.event_id,
                validated.bundle.change_id
            );
        }
        if terminal {
            closed_at = Some(&event.event_id);
            integrated = matches!(
                event.payload,
                Payload::ChangeIntegrated { .. }
                    | Payload::IntegrationAsserted { .. }
                    | Payload::ChangeClosed {
                        outcome: Closure::Integrated,
                        ..
                    }
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_import_report(
    validated: &ValidatedBundle,
    local_repository_id: Option<&str>,
    new_events: &[String],
    skipped_events: &[String],
    conflicts: &[String],
    missing_objects: &[(String, &str, String)],
    pins: &[(String, String)],
    dry_run: bool,
) {
    if let Some(prefix) = &validated.bundle.since {
        println!(
            "prefix: {} ({} events verified)",
            prefix.sha256, prefix.event_count
        );
    }
    if let Some(local) = local_repository_id {
        if local != validated.bundle.repository_id {
            println!(
                "repository: bundle {} differs from local {local} (expected for cross-machine import)",
                validated.bundle.repository_id
            );
        }
    }
    for event_id in new_events {
        println!("new: {event_id}");
    }
    for event_id in skipped_events {
        println!("skipped: {event_id}");
    }
    for event_id in conflicts {
        println!("conflict: {event_id}");
    }
    for (event_id, kind, oid) in missing_objects {
        println!("warning: event {event_id} is missing {kind} commit {oid}");
    }
    for (event_id, event_type) in &validated.unknown_event_types {
        println!("unknown event type: {event_id} {event_type} (preserved verbatim)");
    }
    for (name, head) in pins {
        if dry_run {
            println!("would pin: {name} -> {head}");
        } else {
            println!("pin: {name} -> {head}");
        }
    }
    println!(
        "summary: new={} skipped={} conflicts={} missing_objects={}",
        new_events.len(),
        skipped_events.len(),
        conflicts.len(),
        missing_objects.len()
    );
    if dry_run {
        println!("dry-run: no events or refs written");
    }
}
