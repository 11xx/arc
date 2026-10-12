use super::*;
use crate::operation_println as println;

pub fn export_bundle(
    ctx: &Ctx,
    reference: &str,
    output: &str,
    since: Option<&str>,
    objects_json: Option<&str>,
    custody_dir: Option<&str>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let mut files = Vec::new();
    if output != "-" {
        files.push(("--output", Path::new(output)));
    }
    if let Some(path) = objects_json {
        files.push(("--objects-json", Path::new(path)));
    }
    bundle_output::check_destinations(&files, custody_dir.map(Path::new))?;
    let mut bundle = Bundle::export(&store, &ctx.cwd, &change_id, since)?;
    // The sidecar is staged first because the bundle binds its manifest
    // digest. Nothing is published until every artifact is finished, and the
    // sidecar is published first: a failed export leaves each destination as
    // it was.
    let staged = custody_dir
        .map(|dir| {
            let events = bundle
                .events
                .iter()
                .chain(&bundle.repository_events)
                .map(crate::bundle::parse_typed_event)
                .collect::<Result<Vec<_>>>()?;
            let mut events: Vec<_> = events.into_iter().flatten().collect();
            events.extend(bundle.related_events()?);
            let wanted = crate::custody::referenced(events.iter().map(|e| &e.payload));
            crate::custody::export_sidecar(
                &crate::custody::Custody::at(&store.root),
                &change_id,
                &wanted,
                Path::new(dir),
            )
            .with_context(|| format!("cannot write custody sidecar {dir}"))
        })
        .transpose()?;
    bundle.custody = staged
        .as_ref()
        .map(|(exported, _)| exported.binding.clone());
    let (sidecar, staged) = match staged {
        Some((exported, staged)) => (Some(exported), Some(staged)),
        None => (None, None),
    };
    let objects = bundle.objects.as_deref().unwrap_or_default();
    let mut publication = bundle_output::Publication {
        sidecar: staged,
        files: Vec::new(),
        stdout: None,
    };
    if let Some(path) = objects_json {
        let mut bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "arc-bundle-objects/1",
            "objects": objects,
        }))?;
        bytes.push(b'\n');
        publication.files.push((
            format!("cannot write object list {path}"),
            bundle_output::StagedFile::stage(Path::new(path), &bytes)
                .with_context(|| format!("cannot write object list {path}"))?,
        ));
    }
    let mut counts = BTreeMap::new();
    for object in objects {
        *counts.entry(object.role.as_str()).or_insert(0usize) += 1;
    }
    let summary = counts
        .iter()
        .map(|(role, count)| format!("{role}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    let bytes = bundle.to_bytes()?;
    let mut reported = format!("events: {}", bundle.event_count);
    if let Some(prefix) = &bundle.since {
        reported.push_str(&format!(
            "\nprefix: {} ({} events)",
            prefix.sha256, prefix.event_count
        ));
    }
    reported.push_str(&format!("\nsha256: {}", bundle.events_sha256));
    reported.push_str(&format!("\nobjects: {} {summary}", objects.len()));
    if let (Some(exported), Some(dir)) = (&sidecar, custody_dir) {
        reported.push_str(&format!(
            "\ncustody: {} entries, {} available, {} unavailable ({})",
            exported.entries,
            exported.available,
            exported.unavailable,
            if exported.complete() {
                "complete"
            } else {
                "incomplete"
            }
        ));
        reported.push_str(&format!(
            "\ncustody-manifest: {}\ncustody-dir: {dir}",
            exported.binding.manifest_sha256
        ));
    }
    if output == "-" {
        publication.stdout = Some(bytes);
    } else {
        publication.files.push((
            format!("cannot write export bundle {output}"),
            bundle_output::StagedFile::stage(Path::new(output), &bytes)
                .with_context(|| format!("cannot write export bundle {output}"))?,
        ));
    }
    publication.commit()?;
    if output == "-" {
        eprintln!("{reported}\noutput: -");
    } else {
        println!("{reported}\noutput: {output}");
    }
    Ok(())
}

/// Judge the sidecar a caller supplies against the bundle that binds it.
pub(super) fn load_custody_sidecar(
    validated: &ValidatedBundle,
    dir: Option<&str>,
) -> Result<Option<crate::custody::Sidecar>> {
    let Some(dir) = dir else {
        return Ok(None);
    };
    let referenced = referenced_content(validated);
    crate::custody::load_sidecar(
        Path::new(dir),
        validated.bundle.custody.as_ref(),
        &validated.bundle.change_id,
        &referenced,
    )
    .map(Some)
    .with_context(|| format!("custody sidecar {dir} refused; nothing was imported"))
}

fn referenced_content(
    validated: &ValidatedBundle,
) -> std::collections::BTreeSet<crate::custody::Content> {
    // Repository events were judged well-formed when the bundle parsed.
    let mut repository: Vec<_> = validated
        .bundle
        .repository_events
        .iter()
        .filter_map(|value| crate::bundle::parse_typed_event(value).ok().flatten())
        .collect();
    repository.extend(
        validated
            .bundle
            .related_events()
            .expect("validated related histories"),
    );
    crate::custody::referenced(
        validated
            .events
            .iter()
            .filter_map(|event| event.typed.as_ref())
            .chain(&repository)
            .map(|event| &event.payload),
    )
}

pub fn import_bundle(
    ctx: &Ctx,
    input: &str,
    dry_run: bool,
    provenance_only: bool,
    publisher: Option<&str>,
    received_sha256: Option<&str>,
    custody_dir: Option<&str>,
) -> Result<crate::import_authorization::ImportResult> {
    let bytes = if input == "-" {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        bytes
    } else {
        std::fs::read(input).with_context(|| format!("cannot read import bundle {input}"))?
    };
    crate::import_authorization::verify_received_digest(&bytes, received_sha256)?;
    // Parsing validates every path-bearing ID, checksum, envelope, and
    // patchset field before the local store is inspected or created.
    let validated = Bundle::parse(&bytes)?;
    // The sidecar is judged in full before the store is inspected: a refusal
    // leaves no event, ref or custody file behind.
    let sidecar = load_custody_sidecar(&validated, custody_dir)?;
    import_validated_guarded(
        ctx,
        &bytes,
        &validated,
        dry_run,
        provenance_only,
        publisher,
        received_sha256,
        None,
        sidecar.as_ref(),
    )
}

/// Import a validated bundle. A bundle that binds a custody sidecar the caller
/// did not supply says what that leaves unavailable and how to install it,
/// rather than reading as if custody had arrived.
#[allow(clippy::too_many_arguments)]
pub(super) fn import_validated_guarded(
    ctx: &Ctx,
    bytes: &[u8],
    validated: &ValidatedBundle,
    dry_run: bool,
    provenance_only: bool,
    publisher: Option<&str>,
    received_sha256: Option<&str>,
    before_publish: Option<&dyn Fn() -> Result<()>>,
    sidecar: Option<&crate::custody::Sidecar>,
) -> Result<crate::import_authorization::ImportResult> {
    let mut members = Vec::new();
    let mut admitted = Vec::new();
    let mut introduced = BTreeSet::new();
    for related in &validated.bundle.related_changes {
        let mut member = Bundle::parse(&related.to_bytes()?)?;
        member.bundle.related_changes = validated
            .bundle
            .related_changes
            .iter()
            .filter(|b| b.change_id != related.change_id)
            .cloned()
            .collect();
        let mut primary = validated.bundle.clone();
        primary.related_changes.clear();
        primary.custody = None;
        member.bundle.related_changes.push(primary);
        let (preflight, output) = crate::operation_output::capture(|| {
            import_events(
                ctx,
                bytes,
                &member,
                true,
                provenance_only,
                publisher,
                received_sha256,
                None,
                None,
            )
        });
        let preflight = preflight?;
        if dry_run || preflight.exit_code != 0 {
            println!("{output}");
        }
        if preflight.exit_code != 0 {
            return Ok(preflight);
        }
        members.push(member);
    }
    if !members.is_empty() {
        let (preflight, output) = crate::operation_output::capture(|| {
            import_events(
                ctx,
                bytes,
                validated,
                true,
                provenance_only,
                publisher,
                received_sha256,
                None,
                sidecar,
            )
        });
        let preflight = preflight?;
        if preflight.exit_code != 0 {
            println!("{output}");
            return Ok(preflight);
        }
        if !dry_run {
            for member in &members {
                let mut imported = import_events(
                    ctx,
                    bytes,
                    member,
                    false,
                    provenance_only,
                    publisher,
                    received_sha256,
                    None,
                    None,
                )
                .map_err(|error| related_failure(error, &admitted))?;
                introduced.extend(imported.output.introduced_event_ids.iter().cloned());
                if imported.published {
                    admitted.push(member.bundle.change_id.clone());
                }
                if imported.exit_code != 0 {
                    imported.published |= !admitted.is_empty();
                    imported.output.introduced_event_ids = introduced.into_iter().collect();
                    return Ok(imported);
                }
            }
        }
    }
    let mut result = import_events(
        ctx,
        bytes,
        validated,
        dry_run,
        provenance_only,
        publisher,
        received_sha256,
        before_publish,
        sidecar,
    )
    .map_err(|error| related_failure(error, &admitted))?;
    result.published |= !admitted.is_empty();
    introduced.extend(result.output.introduced_event_ids.iter().cloned());
    result.output.introduced_event_ids = introduced.into_iter().collect();
    if let (0, None, Some(binding)) = (result.exit_code, sidecar, &validated.bundle.custody) {
        let custody = crate::custody::Custody::at(&Store::resolve_root(&ctx.cwd)?);
        println!(
            "custody: sidecar not supplied; {}",
            crate::custody::unsupplied_note(
                binding,
                crate::custody::unavailable_among(&custody, &referenced_content(validated))
            )
        );
    }
    Ok(result)
}

fn related_failure(error: anyhow::Error, admitted: &[String]) -> anyhow::Error {
    if admitted.is_empty() {
        error
    } else {
        crate::model::OperationFailure {
            published: true,
            message: format!(
                "related changes already admitted ({}); {error:#}",
                admitted.join(", ")
            ),
        }
        .into()
    }
}

#[allow(clippy::too_many_arguments)]
fn import_events(
    ctx: &Ctx,
    bytes: &[u8],
    validated: &ValidatedBundle,
    dry_run: bool,
    provenance_only: bool,
    publisher: Option<&str>,
    received_sha256: Option<&str>,
    before_publish: Option<&dyn Fn() -> Result<()>>,
    sidecar: Option<&crate::custody::Sidecar>,
) -> Result<crate::import_authorization::ImportResult> {
    if let Some(objects) = &validated.bundle.objects {
        let mut missing = Vec::new();
        for object in objects {
            let status = gitio::git_command()
                .args(["cat-file", "-e", "--", &object.oid])
                .current_dir(&ctx.cwd)
                .output()
                .context("failed to check bundled Git object")?;
            if !status.status.success() {
                missing.push(format!("{} ({})", object.oid, object.role));
            }
        }
        if !missing.is_empty() {
            let report = missing.join("\n");
            if !provenance_only {
                bail!("missing bundled Git objects:\n{report}\nfetch these objects or use --provenance-only; nothing was imported");
            }
            println!("warning: provenance-only import is missing Git objects:\n{report}");
        }
    }
    let root = Store::resolve_root(&ctx.cwd)?;
    if sidecar.is_some() {
        crate::custody::Custody::at(&root)
            .verify_destination()
            .context("cannot install custody; nothing was imported")?;
    }
    let local_repository_id = Store::repository_id_at(&root)?;
    let local_store = local_repository_id.as_ref().map(|repository_id| Store {
        operator_policy_path: crate::policy::operator_path(&ctx.cwd).ok(),
        root: root.clone(),
        repository_id: repository_id.clone(),
        require_declared_actor: false,
        require_declared_actor_sources: Vec::new(),
        read_only: true,
    });
    // A suffix is only meaningful against the prefix it extends, and the
    // checksum of the whole history is what ties the two together.
    verify_bundle_prefix(local_store.as_ref(), validated)?;

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

    for value in &validated.bundle.events {
        let event: Event = serde_json::from_value(value.clone())?;
        if let Payload::ChangeProvisionallyIntegrated {
            provisional_commit, ..
        } = &event.payload
        {
            if gitio::commit_exists(&ctx.cwd, provisional_commit)? {
                pins.push((
                    crate::provisional::retention_ref(&event.change_id, &event.event_id),
                    provisional_commit.clone(),
                ));
            } else {
                missing_objects.push((
                    event.event_id.clone(),
                    "provisional-merge",
                    provisional_commit.clone(),
                ));
            }
        }
    }
    // A commit a provider reported stays held here when the objects arrived:
    // the ledger cites it, and nothing else need keep it reachable.
    let mut pinned_observed = BTreeSet::new();
    for event in &validated.events {
        if let Some(typed) = &event.typed {
            for commit in crate::forge_round_trip::disclosure_commits(&typed.payload) {
                if gitio::commit_exists(&ctx.cwd, commit)? {
                    pins.push((
                        gitio::retained_ref(&validated.bundle.change_id, &event.event_id, commit),
                        commit.to_string(),
                    ));
                }
            }
        }
        let Some(observed) = event
            .typed
            .as_ref()
            .and_then(|event| crate::forge_observation::observed_commits(&event.payload))
        else {
            continue;
        };
        for (_, commit) in observed {
            if matches!(commit.len(), 40 | 64)
                && pinned_observed.insert(commit)
                && gitio::commit_exists(&ctx.cwd, commit)?
            {
                pins.push((
                    gitio::retained_ref(&validated.bundle.change_id, &event.event_id, commit),
                    commit.to_string(),
                ));
            }
        }
    }
    let mut candidate_changes = BTreeMap::new();
    let held_repository_events = local_store
        .as_ref()
        .map(Store::load_repository_events)
        .transpose()?
        .unwrap_or_default();
    let incoming_repository_events = validated
        .bundle
        .repository_events
        .iter()
        .map(crate::bundle::parse_typed_event)
        .collect::<Result<Vec<_>>>()?;
    for event in held_repository_events
        .iter()
        .chain(incoming_repository_events.iter().flatten())
    {
        if let Payload::CandidateRegistered {
            candidate_id,
            brief,
            ..
        } = &event.payload
        {
            candidate_changes.insert(candidate_id.clone(), brief.change_id.clone());
        }
    }
    for value in &validated.bundle.repository_events {
        let Some(event) = crate::bundle::parse_typed_event(value)? else {
            continue;
        };
        if let Payload::CandidateEvidenceReceived {
            candidate_id,
            evidence,
        } = &event.payload
        {
            if let Some(change_id) = candidate_changes.get(candidate_id) {
                for (_, head) in evidence.heads.each() {
                    if gitio::object_type(&ctx.cwd, &head.commit)?.as_deref() == Some("commit") {
                        pins.push((
                            gitio::retained_ref(change_id, candidate_id, &head.commit),
                            head.commit.clone(),
                        ));
                    }
                }
            }
        }
    }
    // The commits a rewrite recorded as replaced stay held here as they were
    // where the rewrite ran, for as many of them as this repository has.
    for value in &validated.bundle.repository_events {
        let Some(event) = crate::bundle::parse_typed_event(value)? else {
            continue;
        };
        for commit in crate::forge_round_trip::disclosure_commits(&event.payload) {
            if gitio::commit_exists(&ctx.cwd, commit)? {
                pins.push((
                    gitio::retained_ref(Store::REPOSITORY_SCOPE, &event.event_id, commit),
                    commit.to_string(),
                ));
            }
        }
        let Payload::HistoryRewritten { retained, .. } = &event.payload else {
            continue;
        };
        for (name, commit) in crate::bundle::retained_for(retained, &validated.bundle.change_id) {
            if gitio::commit_exists(&ctx.cwd, &commit)? {
                pins.push((name, commit));
            }
        }
    }
    let initial_policy = crate::policy::load(&ctx.cwd)?;
    let preflight = classify_import_events(&root, validated)?;
    if !preflight.conflicts.is_empty() {
        print_import_report(
            validated,
            local_repository_id.as_deref(),
            &preflight.new_events,
            &preflight.skipped_events,
            &preflight.conflicts,
            &missing_objects,
            &pins,
            dry_run,
        );
        println!("aborted: no events or refs written");
        return Ok(crate::model::OperationResult::unpublished(1));
    }
    let repository = plan_repository_events(&ctx.cwd, local_store.as_ref(), validated)?;
    let initial_authorization = authorize_import(
        ctx,
        &initial_policy.replica,
        local_store.as_ref(),
        bytes,
        validated,
        &preflight.new_events,
        &repository,
        publisher,
        received_sha256,
    )?;
    let rewrites = admitted_rewrites(
        local_store.as_ref(),
        &repository,
        initial_authorization.as_ref(),
    )?;
    validated.validate_replay(&rewrites)?;
    if let Some(store) = &local_store {
        if let Some(contest) =
            claim_contest_for_import(store, validated, &preflight.new_events, &rewrites)?
        {
            println!("{}", contest.render(&validated.bundle.change_id));
            println!("aborted: no events or refs written");
            return Ok(crate::model::OperationResult::unpublished(1));
        }
    }
    validate_import_candidate(
        local_store.as_ref(),
        validated,
        &preflight.new_events,
        &repository,
        &rewrites,
    )?;

    if dry_run {
        let plan = classify_import_events(&root, validated)?;
        let mut claim_contest = None;
        if plan.conflicts.is_empty() {
            let store = local_store.as_ref();
            if let Some(store) = store {
                claim_contest =
                    claim_contest_for_import(store, validated, &plan.new_events, &rewrites)?;
            }
            if claim_contest.is_none() {
                validate_import_candidate(
                    store,
                    validated,
                    &plan.new_events,
                    &repository,
                    &rewrites,
                )?;
            }
            // The same refusals the import makes: a preflight that reports
            // success for a bundle the real path rejects is believed, and
            // wrong. A destination with no store still checks the bundle
            // against itself.
            if claim_contest.is_none() {
                plan_repository_events(&ctx.cwd, store, validated)?;
            }
        }
        print_import_report(
            validated,
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
            return Ok(crate::model::OperationResult::unpublished(1));
        }
        if let Some(contest) = claim_contest {
            println!("{}", contest.render(&validated.bundle.change_id));
            println!("aborted: no events or refs written");
            return Ok(crate::model::OperationResult::unpublished(1));
        }
        if let Some(sidecar) = sidecar {
            let outcome = sidecar.plan(&crate::custody::Custody::at(&root));
            print_custody_outcome(&outcome, "would install");
        }
        return Ok(crate::model::OperationResult {
            exit_code: 0,
            published: false,
            output: crate::import_authorization::ImportSummary {
                introduced_event_ids: plan
                    .new_events
                    .iter()
                    .chain(repository.keys())
                    .cloned()
                    .collect(),
                authorization: initial_authorization,
            },
        });
    }

    let store = Store {
        root: root.clone(),
        repository_id: local_repository_id
            .clone()
            .unwrap_or_else(crate::ids::new_identifier),
        operator_policy_path: Some(crate::policy::operator_path(&ctx.cwd)?),
        require_declared_actor: initial_policy.policy.require_declared_actor,
        require_declared_actor_sources: initial_policy
            .sources
            .sources_for("policy.require_declared_actor=true"),
        read_only: false,
    };
    #[cfg(debug_assertions)]
    crate::import_authorization::test_rendezvous("authorized")?;
    let _replica_state = crate::replica::lock(&store)?;
    let transition = store.lock_transition(&validated.bundle.change_id)?;
    if Store::repository_id_at(&root)? != local_repository_id {
        return Err(crate::import_authorization::Refusal(
            "authorization input moved before publication: receiver repository ID".into(),
        )
        .into());
    }
    // The prefix is judged under the same lock as the replay it authorises,
    // so a concurrent import cannot move the history between the two.
    verify_bundle_prefix(Some(&store), validated)?;
    // Classification and candidate replay must happen after taking the same
    // per-change lock used by claim, release, stage, and snapshot. Otherwise a
    // local transition could land between validation and the raw appends.
    let plan = classify_import_events(&root, validated)?;
    let claim_contest = if plan.conflicts.is_empty() {
        claim_contest_for_import(&store, validated, &plan.new_events, &rewrites)?
    } else {
        None
    };
    if plan.conflicts.is_empty() && claim_contest.is_none() {
        validate_import_candidate(
            Some(&store),
            validated,
            &plan.new_events,
            &repository,
            &rewrites,
        )?;
    }

    print_import_report(
        validated,
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
        return Ok(crate::model::OperationResult::unpublished(1));
    }
    if let Some(contest) = claim_contest {
        println!("{}", contest.render(&validated.bundle.change_id));
        println!("aborted: no events or refs written");
        return Ok(crate::model::OperationResult::unpublished(1));
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
    let incoming = plan_repository_events(&ctx.cwd, Some(&store), validated)?;
    let current_policy = crate::policy::load(&ctx.cwd)?;
    let changed_input = if current_policy.replica != initial_policy.replica {
        Some("operator replica policy")
    } else if current_policy.policy.require_declared_actor
        != initial_policy.policy.require_declared_actor
    {
        Some("policy.require_declared_actor")
    } else {
        None
    };
    if let Some(changed_input) = changed_input {
        return Err(crate::import_authorization::Refusal(format!(
            "authorization configuration moved before publication: {changed_input}"
        ))
        .into());
    }
    if let Some(guard) = before_publish {
        guard()?;
    }
    let authorization = authorize_import(
        ctx,
        &current_policy.replica,
        Some(&store),
        bytes,
        validated,
        &plan.new_events,
        &incoming,
        publisher,
        received_sha256,
    )?;
    let rewrites = admitted_rewrites(Some(&store), &incoming, authorization.as_ref())?;
    validated.validate_replay(&rewrites)?;
    if let Some(contest) = claim_contest_for_import(&store, validated, &plan.new_events, &rewrites)?
    {
        println!("{}", contest.render(&validated.bundle.change_id));
        println!("aborted: no events or refs written");
        return Ok(crate::model::OperationResult::unpublished(1));
    }
    validate_import_candidate(
        Some(&store),
        validated,
        &plan.new_events,
        &incoming,
        &rewrites,
    )?;
    ctx.ensure_declared_actor(&store)?;
    let rewrites = incoming.len();
    let batch = crate::store::ImportBatch {
        schema: "arc-import-batch/1".into(),
        change_id: validated.bundle.change_id.clone(),
        events: validated
            .events
            .iter()
            .filter(|event| plan.new_events.contains(&event.event_id))
            .map(|event| event.value.clone())
            .collect(),
        repository_events: incoming
            .values()
            .map(|bytes| serde_json::from_slice(bytes))
            .collect::<Result<Vec<_>, _>>()?,
        authorization: authorization.clone(),
    };
    let published = !batch.events.is_empty() || !batch.repository_events.is_empty();
    if published {
        store.publish_import(&batch)?;
    }
    if rewrites > 0 {
        println!("repository events: {rewrites} imported");
    }
    let (pinned, unpinned) =
        super::candidate::pin_imported(&ctx.cwd, &store, &incoming.keys().cloned().collect())
            .map_err(|error| crate::model::OperationFailure {
                published,
                message: format!("candidate retention maintenance failed: {error:#}"),
            })?;
    for candidate_id in pinned {
        println!("candidate {candidate_id}: pinned");
    }
    for candidate_id in unpinned {
        println!("candidate {candidate_id}: tree not held here; pin absent");
    }
    drop(transition);
    for (name, head) in pins {
        gitio::update_ref(&ctx.cwd, &name, &head).map_err(|error| {
            crate::model::OperationFailure {
                published,
                message: format!("retention-ref maintenance failed: {error:#}"),
            }
        })?;
    }
    // Custody is reported installed only once the events are published. Its
    // bytes are verified and content-addressed, so a failure here costs
    // nothing but the installation, which repeating the import completes.
    if let Some(sidecar) = sidecar {
        let outcome = sidecar
            .install(&crate::custody::Custody::at(&root))
            .map_err(|error| crate::model::OperationFailure {
                published,
                message: format!(
                    "custody installation failed: {error:#}; {}repeat the import with \
                     --custody-dir to install it",
                    if published {
                        "the events are published; "
                    } else {
                        ""
                    }
                ),
            })?;
        print_custody_outcome(&outcome, "installed");
    }
    Ok(crate::model::OperationResult {
        exit_code: 0,
        published,
        output: crate::import_authorization::ImportSummary {
            introduced_event_ids: plan
                .new_events
                .iter()
                .chain(incoming.keys())
                .cloned()
                .collect(),
            authorization,
        },
    })
}

fn print_custody_outcome(outcome: &crate::custody::Outcome, installed: &str) {
    println!(
        "custody: {} {installed}, {} held, {} unavailable ({})",
        outcome.installed,
        outcome.held,
        outcome.unavailable,
        if outcome.complete() {
            "complete"
        } else {
            "incomplete"
        }
    );
}

#[allow(clippy::too_many_arguments)]
fn authorize_import(
    ctx: &Ctx,
    policy: &crate::import_authorization::ReplicaPolicy,
    store: Option<&Store>,
    bytes: &[u8],
    validated: &ValidatedBundle,
    new_events: &[String],
    repository: &BTreeMap<String, Vec<u8>>,
    publisher: Option<&str>,
    received_sha256: Option<&str>,
) -> Result<Option<crate::import_authorization::ImportAuthorization>> {
    let replica = store.map(crate::replica::status).transpose()?;
    let mut events: Vec<_> = validated
        .events
        .iter()
        .filter(|event| new_events.contains(&event.event_id))
        .map(|event| ("change".into(), event.value.clone()))
        .collect();
    for bytes in repository.values() {
        events.push(("repository".into(), serde_json::from_slice(bytes)?));
    }
    if let Some(store) = store {
        crate::import_authorization::validate_import_identities(store, &events)?;
    }
    let authorization = crate::import_authorization::plan_import(
        policy,
        &crate::policy::operator_path(&ctx.cwd)?,
        crate::import_authorization::ImportRequest {
            bytes,
            principal: publisher,
            received_sha256,
            source_replica_id: validated.bundle.source_replica_id.as_deref(),
            project_id: validated.bundle.project_id.as_deref(),
            receiver_id: store
                .map(|store| store.repository_id.as_str())
                .unwrap_or("uninitialized"),
            receiver_project: replica
                .as_ref()
                .and_then(|replica| replica.project_id.as_deref()),
            events: &events,
        },
    )?;
    if let Some(replica) = replica
        .as_ref()
        .filter(|replica| replica.project_id.is_some())
    {
        let members: BTreeSet<_> = replica
            .local
            .iter()
            .chain(&replica.peers)
            .map(|identity| identity.repository_id.as_str())
            .collect();
        for (_, value) in &events {
            let origin = value["repository_id"]
                .as_str()
                .context("event has no recorded origin")?;
            if !members.contains(origin) {
                return Err(crate::import_authorization::Refusal(format!(
                    "project {:?}: event {} origin {origin} is not a project member",
                    replica.project_id, value["event_id"]
                ))
                .into());
            }
        }
        if !validated
            .bundle
            .source_replica_id
            .as_deref()
            .is_some_and(|id| members.contains(id))
        {
            return Err(crate::import_authorization::Refusal(format!(
                "project {:?}: exporter {:?} is not a project member",
                replica.project_id, validated.bundle.source_replica_id
            ))
            .into());
        }
    }
    Ok(authorization)
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
    rewrites: &crate::rewrite::RewriteMap,
) -> Result<Option<crate::replica::ClaimContest>> {
    let mut incoming = validated
        .events
        .iter()
        .filter_map(|event| event.typed.clone())
        .collect::<Vec<_>>();
    if let Some(prefix) = &validated.bundle.since {
        for (_, value) in store
            .raw_events(&validated.bundle.change_id)?
            .into_iter()
            .take(prefix.event_count)
        {
            if let Some(event) = crate::bundle::parse_typed_event(&value)? {
                incoming.push(event);
            }
        }
    }
    let new_event_ids = new_event_ids.iter().cloned().collect::<BTreeSet<_>>();
    crate::replica::live_claim_contest(
        store,
        &validated.bundle.change_id,
        &incoming,
        &new_event_ids,
        rewrites,
    )
}

fn combined_repository_events(
    store: Option<&Store>,
    incoming: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<Event>> {
    let mut events = store
        .map(Store::load_repository_events)
        .transpose()?
        .unwrap_or_default();
    for bytes in incoming.values() {
        events.push(serde_json::from_slice(bytes)?);
    }
    events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    Ok(events)
}

/// Stored assertions require bound admission; introduced assertions require
/// the authorization this publication will record. Legacy transport grants
/// no rewrite capability for falsification identity.
fn admitted_rewrites(
    store: Option<&Store>,
    incoming: &BTreeMap<String, Vec<u8>>,
    authorization: Option<&crate::import_authorization::ImportAuthorization>,
) -> Result<crate::rewrite::RewriteMap> {
    let events = combined_repository_events(store, incoming)?;
    let scope = store.map(Store::privilege_scope).transpose()?;
    crate::rewrite::RewriteMap::from_permitted_events(events.iter(), |event| {
        if incoming.contains_key(&event.event_id) {
            authorization.is_some_and(|authorization| {
                authorization.events.iter().any(|receipt| {
                    receipt.event_id == event.event_id
                        && receipt.scope == "repository"
                        && receipt.origin_replica_id == event.repository_id
                        && receipt
                            .authorized_roles
                            .contains(&crate::import_authorization::Role::Rewrite)
                })
            })
        } else {
            scope.as_ref().is_some_and(|scope| {
                scope.permits(
                    &event.event_id,
                    &event.repository_id,
                    crate::import_authorization::Role::Rewrite,
                )
            })
        }
    })
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
        if store
            .map(|store| store.repository_event_conflicts(event_id, &bytes))
            .transpose()?
            .unwrap_or(false)
        {
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
    let held: BTreeSet<String> = match store {
        Some(store) => store
            .raw_repository_events_unseen(&BTreeSet::new())?
            .into_iter()
            .map(|(id, _)| id)
            .collect(),
        None => BTreeSet::new(),
    };
    let mut novel = Vec::new();
    for value in &validated.bundle.repository_events {
        if let Some(event) = crate::bundle::parse_typed_event(value)? {
            if !held.contains(&event.event_id) {
                novel.push(event.clone());
                combined.push(event);
            }
        }
    }
    // Only what the bundle adds is judged against the objects held here.
    // Evidence already held was admitted earlier, and a later contradiction
    // of it is a read-time diagnostic, never a reason to refuse an import.
    for event in &novel {
        if let Payload::CandidateEvidenceReceived { evidence, .. } = &event.payload {
            evidence.validate_admission().with_context(|| {
                format!(
                    "the bundle's received evidence event {} is refused; nothing was imported",
                    event.event_id
                )
            })?;
            evidence.validate_held_heads(cwd).with_context(|| {
                format!(
                    "the bundle's received evidence event {} is refused; nothing was imported",
                    event.event_id
                )
            })?;
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
        let extra: Vec<_> = validated
            .events
            .iter()
            .filter_map(|event| event.typed.clone())
            .collect();
        let judged = crate::provisional::retain_signature_only(
            cwd,
            store,
            &extra,
            &rewrites,
            crate::rewrite::signature_only_successors(cwd, mapping, &rewrites),
        )?;
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
    incoming.retain(|id, _| !held.contains(id));
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
    repository: &BTreeMap<String, Vec<u8>>,
    rewrites: &crate::rewrite::RewriteMap,
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
    let admission_history = if validated.bundle.since.is_none() {
        validated
            .events
            .iter()
            .filter_map(|event| event.typed.clone())
            .collect::<Vec<_>>()
    } else {
        candidate.clone()
    };
    crate::forge_observation::validate_import_admission(&admission_history, &new_events)?;
    let mut round_trip_history = store
        .map(crate::forge_round_trip::held_events)
        .transpose()?
        .unwrap_or_default();
    round_trip_history.retain(|e| e.change_id != validated.bundle.change_id);
    round_trip_history.extend(candidate.clone());
    round_trip_history.extend(combined_repository_events(store, repository)?);
    round_trip_history.extend(validated.bundle.related_events()?);
    round_trip_history.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    for pair in round_trip_history.windows(2) {
        if pair[0].event_id == pair[1].event_id {
            anyhow::ensure!(
                serde_json::to_value(&pair[0])? == serde_json::to_value(&pair[1])?,
                "conflicting related event identity"
            );
        }
    }
    round_trip_history.dedup_by(|a, b| a.event_id == b.event_id);
    let mut introduced_round_trip = new_events.clone();
    introduced_round_trip.extend(repository.keys());
    crate::forge_round_trip::validate_history(&round_trip_history, &introduced_round_trip)?;
    let mut projected = state::reduce_with_rewrites(&candidate, rewrites)
        .context("combined local and bundled known events are not replayable")?;
    let repository_events = combined_repository_events(store, repository)?;
    projected.follow_rewrites(&crate::rewrite::RewriteMap::from_events(
        repository_events.iter(),
    )?)?;
    crate::provisional::validate_resolution_history(&projected, &candidate, store)?;
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
