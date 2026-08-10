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
            if let Some(repository_id) = &local_repository_id {
                let store = Store {
                    root: root.clone(),
                    repository_id: repository_id.clone(),
                    toplevel: None,
                };
                validate_import_candidate(&store, &validated, &plan.new_events)?;
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
        return Ok(0);
    }

    let store = Store::discover(&ctx.cwd)?;
    let transition = store.lock_transition(&validated.bundle.change_id)?;
    // Classification and candidate replay must happen after taking the same
    // per-change lock used by claim, release, stage, and snapshot. Otherwise a
    // local transition could land between validation and the raw appends.
    let plan = classify_import_events(&root, &validated)?;
    if plan.conflicts.is_empty() {
        validate_import_candidate(&store, &validated, &plan.new_events)?;
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
    for event in &validated.events {
        if plan.new_events.contains(&event.event_id) {
            store.append_raw_event(&validated.bundle.change_id, &event.event_id, &event.bytes)?;
        }
    }
    drop(transition);
    for (name, head) in pins {
        gitio::update_ref(&ctx.cwd, &name, &head)?;
    }
    Ok(0)
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

fn validate_import_candidate(
    store: &Store,
    validated: &ValidatedBundle,
    new_events: &[String],
) -> Result<()> {
    let mut candidate = Vec::new();
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
