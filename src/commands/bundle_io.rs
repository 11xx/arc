use super::*;

pub fn export_bundle(ctx: &Ctx, reference: &str, output: &str) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let bundle = Bundle::export(&store, &change_id)?;
    let bytes = bundle.to_bytes()?;
    if output == "-" {
        std::io::stdout().write_all(&bytes)?;
        eprintln!("events: {}", bundle.event_count);
        eprintln!("sha256: {}", bundle.events_sha256);
        eprintln!("output: -");
    } else {
        std::fs::write(output, bytes)
            .with_context(|| format!("cannot write export bundle {output}"))?;
        println!("events: {}", bundle.event_count);
        println!("sha256: {}", bundle.events_sha256);
        println!("output: {output}");
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
        if plan.conflicts.is_empty() {
            let store = local_repository_id.as_ref().map(|repository_id| Store {
                root: root.clone(),
                repository_id: repository_id.clone(),
                require_declared_actor: false,
            });
            validate_import_candidate(store.as_ref(), &validated, &plan.new_events)?;
            // The same refusals the import makes: a preflight that reports
            // success for a bundle the real path rejects is believed, and
            // wrong. A destination with no store still checks the bundle
            // against itself.
            plan_repository_events(store.as_ref(), &validated)?;
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
        return Ok(0);
    }

    let store = Store::discover(&ctx.cwd)?;
    let transition = store.lock_transition(&validated.bundle.change_id)?;
    // Classification and candidate replay must happen after taking the same
    // per-change lock used by claim, release, stage, and snapshot. Otherwise a
    // local transition could land between validation and the raw appends.
    let plan = classify_import_events(&root, &validated)?;
    if plan.conflicts.is_empty() {
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
    let incoming = plan_repository_events(&store, &validated)?;
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
    drop(transition);
    for (name, head) in pins {
        gitio::update_ref(&ctx.cwd, &name, &head)?;
    }
    Ok(0)
}

/// The repository events an import would write, refusing every contradiction
/// before the first is written — including two bundled events sharing an ID,
/// which no check against the destination can see.
fn plan_repository_events(
    store: &Store,
    validated: &ValidatedBundle,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut incoming: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for value in &validated.bundle.repository_events {
        let Some(event_id) = value.get("event_id").and_then(serde_json::Value::as_str) else {
            bail!("bundled repository event has no event_id");
        };
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        if store.repository_event_conflicts(event_id, &bytes)? {
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
    // Replayability is not admissibility. A bundle legitimately carries the
    // lifecycle events a command would not append by hand, so the CLI's own
    // permission table is the wrong question here; what an import must still
    // refuse is a history that contradicts itself — a change closed twice, or
    // work recorded after it closed.
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
