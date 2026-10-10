//! Preparation of a transported change under the receiver's local authority.
use super::accept_checkout::Allocation;
use super::accept_effects::{self, Effects};
use super::*;
use crate::operation_println as println;
use crate::rewrite::RewriteMap;

pub const ACCEPT_SCHEMA: &str = "arc-accept/1";

pub struct AcceptArgs {
    pub input: String,
    pub branch: String,
    pub publisher: String,
    pub received_sha256: String,
    pub worktree: Option<PathBuf>,
    pub key: Option<String>,
    pub contributors: Option<Vec<String>>,
    pub solo: bool,
    pub dry_run: bool,
    pub json: bool,
}

#[derive(Serialize)]
pub struct AcceptanceStep {
    name: String,
    kind: &'static str,
    effects: Effects,
    argv: Vec<String>,
    cwd: PathBuf,
    outcome: String,
    exit_code: Option<i32>,
    event_ids: Vec<String>,
}

#[derive(Serialize)]
pub struct AcceptanceResult {
    schema: &'static str,
    change_id: Option<String>,
    source_replica_id: Option<String>,
    publisher: String,
    received_sha256: String,
    delivered_head: Option<String>,
    prepared_head: Option<String>,
    worktree: Option<PathBuf>,
    preparation_complete: bool,
    steps: Vec<AcceptanceStep>,
    stopped_at: Option<String>,
    readiness: Option<StatusReport>,
    local_verdict_owed: bool,
    verdict_tips: Vec<serde_json::Value>,
    next_steps: Vec<String>,
}

#[derive(Debug)]
struct Refusal {
    code: i32,
    detail: String,
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}
impl std::error::Error for Refusal {}
fn refuse<T>(code: i32, detail: impl Into<String>) -> Result<T> {
    Err(Refusal {
        code,
        detail: detail.into(),
    }
    .into())
}
fn exit_code(error: &anyhow::Error) -> i32 {
    if let Some(refusal) = error.downcast_ref::<Refusal>() {
        refusal.code
    } else if error
        .downcast_ref::<crate::import_authorization::Refusal>()
        .is_some()
    {
        18
    } else {
        1
    }
}

struct AcceptancePlan {
    bytes: Vec<u8>,
    bundle: ValidatedBundle,
    store: Store,
    state: ChangeState,
    delivered_head: String,
    head: String,
    base: String,
    target: String,
    brief_version: Option<usize>,
    probes: Vec<AcceptanceProbe>,
    contributors: Vec<String>,
    checkout: PathBuf,
    create_checkout: bool,
    configuration: String,
    invocation: PathBuf,
    git_configuration: String,
    checkout_admin: Option<PathBuf>,
    allocation: Option<Allocation>,
    populate_checkout: bool,
}

fn configuration(ctx: &Ctx, state: &ChangeState) -> Result<String> {
    let declarations = ctx.declarations(state)?;
    Ok(format!(
        "{:?}\n{:?}\n{:?}\n{:?}",
        declarations,
        crate::policy::load(&ctx.cwd)?,
        config::load()?,
        gitio::git(&ctx.cwd, &["config", "--list", "--show-origin"])?
    ))
}

fn authority(store: &Store) -> Result<()> {
    let replica = crate::replica::status(store)?;
    if replica.local.is_none() || replica.peers.is_empty() {
        return refuse(
            17,
            "accept requires a paired receiver holding integration authority",
        );
    }
    if let Some(refusal) = crate::replica::integration_refusal(store)? {
        return refuse(17, refusal);
    }
    Ok(())
}

fn command(words: impl IntoIterator<Item = impl Into<String>>) -> Vec<String> {
    std::iter::once("arc".to_string())
        .chain(words.into_iter().map(Into::into))
        .collect()
}

pub(super) fn event_ids(store: &Store, change: Option<&str>) -> Result<BTreeSet<String>> {
    let mut ids: BTreeSet<_> = store
        .load_repository_events()?
        .into_iter()
        .map(|event| event.event_id)
        .collect();
    if let Some(change) = change.filter(|change| {
        store
            .list_change_ids()
            .is_ok_and(|ids| ids.iter().any(|id| id == change))
    }) {
        ids.extend(
            store
                .load_events(change)?
                .into_iter()
                .map(|event| event.event_id),
        );
    }
    Ok(ids)
}

fn step<T>(
    result: &mut AcceptanceResult,
    ctx: &Ctx,
    name: &str,
    argv: Vec<String>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    result.stopped_at = Some(name.to_string());
    let before =
        accept_effects::observation(ctx, result.change_id.as_deref(), result.worktree.as_deref());
    let (mut value, output) = if before.errors.is_empty() {
        crate::operation_output::capture(operation)
    } else {
        (refuse(1, before.errors.join("\n")), String::new())
    };
    let after =
        accept_effects::observation(ctx, result.change_id.as_deref(), result.worktree.as_deref());
    let (effects, event_ids) = accept_effects::effects(ctx, &before, after, &argv);
    if value.is_ok() && !effects.observation_errors.is_empty() {
        value = refuse(1, effects.observation_errors.join("\n"));
    }
    let code = value.as_ref().err().map(exit_code).unwrap_or(0);
    let outcome = match &value {
        Ok(_) => {
            if output.is_empty() {
                "completed".into()
            } else {
                output
            }
        }
        Err(error) => format!("{output}{error:#}"),
    };
    result.steps.push(AcceptanceStep {
        name: name.into(),
        kind: if argv.is_empty() {
            "internal"
        } else {
            "command"
        },
        effects,
        argv,
        cwd: ctx.cwd.clone(),
        outcome,
        exit_code: Some(code),
        event_ids,
    });
    value
}

fn plan_acceptance(
    ctx: &Ctx,
    args: &AcceptArgs,
    result: &mut AcceptanceResult,
) -> Result<AcceptancePlan> {
    let (bytes, bundle) = step(
        result,
        ctx,
        "import-preview",
        command([
            "import",
            &args.input,
            "--publisher",
            &args.publisher,
            "--received-sha256",
            &args.received_sha256,
            "--dry-run",
        ]),
        || {
            let bytes = if args.input == "-" {
                let mut bytes = Vec::new();
                std::io::stdin().read_to_end(&mut bytes)?;
                bytes
            } else {
                std::fs::read(&args.input)
                    .with_context(|| format!("cannot read delivery {}", args.input))?
            };
            crate::import_authorization::verify_received_digest(
                &bytes,
                Some(&args.received_sha256),
            )?;
            let validated = Bundle::parse(&bytes)?;
            for object in validated.bundle.objects.as_deref().unwrap_or_default() {
                if gitio::git(&ctx.cwd, &["cat-file", "-e", &object.oid]).is_err() {
                    return refuse(6, format!("missing {} object {}; fetch the delivery's Git objects before accepting", object.role, object.oid));
                }
            }
            let preview = bundle_io::import_validated(
                ctx,
                &bytes,
                &validated,
                true,
                false,
                Some(&args.publisher),
                Some(&args.received_sha256),
            )?;
            if preview.exit_code != 0 {
                return refuse(
                    preview.exit_code,
                    "import preview refused; nothing was imported",
                );
            }
            Ok((bytes, validated))
        },
    )?;
    result.change_id = Some(bundle.bundle.change_id.clone());
    result.delivered_head = bundle
        .patchsets
        .last()
        .map(|patchset| patchset.head.clone());
    result.source_replica_id = Some(
        bundle
            .bundle
            .source_replica_id
            .clone()
            .unwrap_or_else(|| bundle.bundle.repository_id.clone()),
    );
    step(
        result,
        ctx,
        "preflight",
        {
            let mut argv = acceptance_argv(args);
            argv.push("--dry-run".into());
            argv
        },
        || {
            let store = Store::discover_for_reading(&ctx.cwd)?;
            authority(&store)?;
            let mut events = if store.list_change_ids()?.contains(&bundle.bundle.change_id) {
                store.load_events(&bundle.bundle.change_id)?
            } else {
                Vec::new()
            };
            for incoming in &bundle.events {
                if !events
                    .iter()
                    .any(|event| event.event_id == incoming.event_id)
                {
                    events.push(
                        incoming
                            .typed
                            .clone()
                            .context("accept cannot prepare an opaque event")?,
                    );
                }
            }
            events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
            let mut repository = store.load_repository_events()?;
            for value in &bundle.bundle.repository_events {
                let event: Event = serde_json::from_value(value.clone())?;
                if !repository
                    .iter()
                    .any(|held| held.event_id == event.event_id)
                {
                    repository.push(event);
                }
            }
            repository.sort_by(|a, b| a.event_id.cmp(&b.event_id));
            let rewrites = RewriteMap::from_events(repository.iter())?;
            let mut state = state::reduce_following(&events, &rewrites)?;
            crate::replica::localize_change(&store.repository_id, &events, &mut state);
            ensure_append_allowed(
                &state,
                &Payload::ChangeCheckoutBound {
                    branch: state.branch.clone(),
                    worktree: state.worktree.clone().unwrap_or_default(),
                    worktree_created: false,
                    worktree_owner_token: None,
                },
            )?;
            if args.branch != state.branch {
                return refuse(1, format!("local branch {:?} differs from delivery branch {:?}; fetch the recorded branch without an alias", args.branch, state.branch));
            }
            let delivered = bundle
                .patchsets
                .last()
                .context("delivery has no patchset; worker must snapshot before exporting")?;
            let patchset = state
                .patchsets
                .iter()
                .find(|patchset| patchset.id == delivered.patchset_id)
                .context("delivered patchset is unavailable")?;
            let delivered_head = delivered.head.clone();
            let expected = rewrites.approved_successor(&delivered_head)?;
            let head = match gitio::branch_head(&ctx.cwd, &args.branch) {
            Ok(head) => head,
            Err(_) => return refuse(6, format!("required branch {:?} is unavailable; fetch that branch and its Git objects before accepting", args.branch)),
        };
            let pending = store.rewrite_intent()?;
            let recovery = if let Some(intent) = &pending {
                validate_recovery(ctx, &store, intent, &repository, &state, &expected, &head)?;
                true
            } else {
                false
            };
            if head != expected && !recovery {
                if rewrites.current(&delivered_head)? == head {
                    return refuse(
                        11,
                        format!(
                            "delivered ancestry changed through a non-signature rewrite; {}; {}",
                            publishing_remedy(&state),
                            replay(&state, &args.branch)
                        ),
                    );
                }
                return refuse(1, format!("branch {:?} head {head} differs from delivered head {delivered_head} and its recorded signature-only successor {expected}; {}", args.branch, publishing_remedy(&state)));
            }
            let target = match gitio::branch_head(&ctx.cwd, &state.target_branch) {
                Ok(head) => head,
                Err(_) => {
                    return refuse(
                        6,
                        format!(
                            "required target {:?} is unavailable; fetch the authoritative target",
                            state.target_branch
                        ),
                    )
                }
            };
            let base = patchset.base.clone();
            for (role, revision) in [("patchset base", &base), ("patchset head", &head)] {
                if !gitio::commit_exists(&ctx.cwd, revision)? {
                    return refuse(6, format!("required {role} object {revision} is unavailable; fetch the delivery objects"));
                }
            }
            if base != target {
                return refuse(11, format!("delivered base {base} differs from authoritative {} at {target}; prerequisites {:?} require canonical integration and explicit content replay. {}", state.target_branch, state.blocked_by, replay(&state, &args.branch)));
            }
            let brief_version = patchset.brief_version;
            let probes = if let Some(version) = brief_version {
                let brief = &state.briefs[version - 1];
                if !brief.acceptance_probes.is_empty() {
                    let baseline = brief.base_revision.as_deref().context("bound brief has no baseline; record a brief with --base and snapshot/review before delivery")?;
                    if !gitio::commit_exists(&ctx.cwd, baseline)? {
                        return refuse(6, format!("brief v{version} baseline object {baseline} is unavailable; fetch it before accepting"));
                    }
                }
                brief.acceptance_probes.clone()
            } else {
                Vec::new()
            };
            let declared =
                review::contributor_declaration(ctx, args.contributors.clone(), args.solo)?;
            let contributors = match declared {
            Some(contributors) => contributors,
            None if !patchset.contributors.is_empty() => patchset.contributors.clone(),
            None => return refuse(1, "delivered patchset has insufficient attribution; supply --contributors ACTOR,... or --solo"),
        };
            let inventory = gitio::worktree_inventory(&ctx.cwd)?;
            let matching = inventory
                .iter()
                .find(|entry| entry.branch.as_deref() == Some(&args.branch));
            let checkout = match &args.worktree {
                Some(path) => config::resolved(&if path.is_absolute() {
                    path.clone()
                } else {
                    ctx.cwd.join(path)
                }),
                None => match matching {
                    Some(entry) => config::resolved(&entry.path),
                    None => {
                        let root = config::load()?.worktrees_dir;
                        config::resolved(
                            &if root.is_absolute() {
                                root
                            } else {
                                ctx.cwd.join(root)
                            }
                            .join(format!("accept-{}", state.change_id)),
                        )
                    }
                },
            };
            if !config::admits_path(&ctx.repository(), &checkout) {
                return refuse(
                    1,
                    format!("worktree {} is outside sandbox bounds", checkout.display()),
                );
            }
            let create_checkout = match std::fs::symlink_metadata(&checkout) {
                Ok(_) => false,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("cannot inspect checkout path {}", checkout.display())
                    })
                }
            };
            let allocation = if create_checkout {
                None
            } else {
                Allocation::load(&checkout)?
            };
            if let Some(allocation) = &allocation {
                allocation.validate(ctx, &store, &state.change_id, &state.branch, &head)?;
            }
            let populate_checkout = create_checkout
                || allocation
                    .as_ref()
                    .map(Allocation::unpopulated)
                    .transpose()?
                    .unwrap_or(false);
            if let Some(allocation) = &allocation {
                if !populate_checkout
                    && !allocation.administrative_dir.join("index").try_exists()?
                {
                    return refuse(
                        1,
                        "unpopulated owned allocation contains unrelated content; keeping it",
                    );
                }
            }
            if create_checkout {
                if let Some(entry) = matching {
                    return refuse(
                        1,
                        format!(
                            "branch {:?} is already registered at {}; select that checkout",
                            args.branch,
                            entry.path.display()
                        ),
                    );
                }
            } else {
                let registered = inventory.iter().any(|entry| {
                    config::resolved(&entry.path) == checkout
                        && entry.branch.as_deref() == Some(&args.branch)
                        && !entry.prunable
                });
                if !registered
                    || gitio::common_dir(&checkout).ok() != Some(gitio::common_dir(&ctx.cwd)?)
                    || gitio::branch_head(&checkout, &args.branch)? != head
                    || gitio::head(&checkout)? != head
                {
                    return refuse(1, format!("occupied path {} is not a matching registered checkout of {:?} in this repository", checkout.display(), args.branch));
                }
                if !populate_checkout && !gitio::is_clean(&checkout)? {
                    return refuse(
                        1,
                        format!(
                            "checkout {} is dirty; commit or preserve its content before accepting",
                            checkout.display()
                        ),
                    );
                }
            }
            let configuration = configuration(ctx, &state)?;
            Ok(AcceptancePlan {
                bytes,
                bundle,
                store,
                state,
                delivered_head,
                head,
                base,
                target,
                brief_version,
                probes,
                contributors,
                checkout: checkout.clone(),
                create_checkout,
                allocation,
                populate_checkout,
                configuration,
                invocation: ctx.cwd.clone(),
                git_configuration: gitio::git(&ctx.cwd, &["config", "--list"])?,
                checkout_admin: if create_checkout {
                    None
                } else {
                    Some(gitio::worktree_admin_dir(&checkout)?)
                },
            })
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_recovery(
    ctx: &Ctx,
    store: &Store,
    intent: &crate::rewrite::RewriteIntent,
    repository: &[Event],
    state: &ChangeState,
    expected: &str,
    head: &str,
) -> Result<()> {
    let branch_ref = format!("refs/heads/{}", state.branch);
    let moved = intent
        .refs
        .get(&branch_ref)
        .context("pending rewrite has no change branch move")?;
    if intent.branch != state.branch
        || moved.old != expected
        || (head != moved.old && head != moved.new)
    {
        return refuse(1, "pending rewrite does not prepare this delivered branch; inspect arc rewrite recovery before accepting");
    }
    if intent
        .refs
        .keys()
        .any(|reference| reference.starts_with("refs/heads/") && reference != &branch_ref)
    {
        return refuse(
            1,
            "pending signing rewrite moves another branch; exclusive acceptance refuses recovery",
        );
    }
    let mut combined = repository.to_vec();
    combined.push(ctx.event(
        store,
        Store::REPOSITORY_SCOPE,
        Payload::HistoryRewritten {
            mapping: intent.mapping.clone(),
            reason: intent.reason.clone(),
            tool: intent.tool.clone(),
            signature_only: Vec::new(),
            refs: intent.refs.clone(),
            retained: intent.retained.clone(),
        },
    )?);
    let mapped = RewriteMap::from_events(combined.iter())?;
    if crate::rewrite::signature_only_successors(&ctx.cwd, &intent.mapping, &mapped).len()
        != intent.mapping.len()
    {
        return refuse(
            11,
            format!(
                "pending rewrite changes content or provisional ancestry; {}",
                replay(state, &state.branch)
            ),
        );
    }
    for reference in gitio::local_refs(&ctx.cwd)?.iter().filter(|reference| {
        reference.name.starts_with("refs/heads/") && reference.name != branch_ref
    }) {
        for original in intent.mapping.keys() {
            if gitio::is_ancestor(&ctx.cwd, original, &reference.commit)? {
                return refuse(1, format!("pending signing range includes shared commit {original} reachable from {}; scoped signing refuses", reference.name));
            }
        }
    }
    Ok(())
}

fn contributor_args(state: &ChangeState) -> String {
    state
        .latest_patchset()
        .filter(|patchset| !patchset.contributors.is_empty())
        .map(|patchset| {
            format!(
                " --contributors {}",
                crate::context::shell_quote(&patchset.contributors.join(","))
            )
        })
        .unwrap_or_else(|| " --contributors ACTOR,...".into())
}

fn publishing_remedy(state: &ChangeState) -> String {
    let change = crate::context::shell_quote(&state.change_id);
    let attribution = contributor_args(state);
    format!("record a patchset for the branch head on the publishing replica with arc snapshot {change}{attribution} when the publisher only rebased, preserving the implementing actors as contributors; export again, and accept the new bundle")
}

fn replay(state: &ChangeState, branch: &str) -> String {
    let change = crate::context::shell_quote(&state.change_id);
    let branch = crate::context::shell_quote(branch);
    let target = crate::context::shell_quote(&state.target_branch);
    let attribution = contributor_args(state);
    format!("# Replay in the delivery's owning checkout after transporting the authoritative target to {target}.\ngit switch {branch}\narc rebase {change}{attribution}\narc snapshot {change} --base {target}{attribution}\n# Read the replayed patchset and record approval only after fresh review.\narc review {change} --verdict approved")
}

fn acceptance_argv(args: &AcceptArgs) -> Vec<String> {
    let mut argv = command([
        "accept",
        &args.input,
        "--branch",
        &args.branch,
        "--publisher",
        &args.publisher,
        "--received-sha256",
        &args.received_sha256,
    ]);
    if let Some(worktree) = &args.worktree {
        argv.extend(["--worktree".into(), worktree.display().to_string()]);
    }
    if let Some(key) = &args.key {
        argv.extend(["--key".into(), key.clone()]);
    }
    if let Some(contributors) = &args.contributors {
        argv.extend(["--contributors".into(), contributors.join(",")]);
    }
    if args.solo {
        argv.push("--solo".into());
    }
    if args.json {
        argv.push("--json".into());
    }
    argv
}

fn revalidate(ctx: &Ctx, plan: &AcceptancePlan, expected: &str) -> Result<()> {
    authority(&plan.store)?;
    let head = gitio::branch_head(&ctx.cwd, &plan.state.branch)?;
    if head != expected {
        return refuse(
            1,
            format!(
                "change branch moved: expected {expected}, observed {head}; {}; {}",
                publishing_remedy(&plan.state),
                replay(&plan.state, &plan.state.branch)
            ),
        );
    }
    let target = gitio::branch_head(&ctx.cwd, &plan.state.target_branch)?;
    if target != plan.target {
        return refuse(
            1,
            format!(
                "target moved: expected {}, observed {target}; {}",
                plan.target,
                replay(&plan.state, &plan.state.branch)
            ),
        );
    }
    if configuration(&ctx.with_cwd(plan.invocation.clone()), &plan.state)? != plan.configuration
        || gitio::git(&ctx.cwd, &["config", "--list"])? != plan.git_configuration
    {
        return refuse(1, "acceptance policy, declarations, signing configuration, or worktree configuration moved; inspect configuration and retry the same delivery");
    }
    if config::resolved(&ctx.cwd) == plan.checkout {
        validate_checkout(ctx, plan, expected, true)?;
    }
    Ok(())
}

fn validate_checkout(
    ctx: &Ctx,
    plan: &AcceptancePlan,
    expected: &str,
    require_clean: bool,
) -> Result<()> {
    let inventory = gitio::worktree_inventory(&plan.invocation)?;
    let registered = inventory.iter().any(|entry| {
        config::resolved(&entry.path) == plan.checkout
            && entry.branch.as_deref() == Some(&plan.state.branch)
            && !entry.prunable
    });
    if !registered
        || gitio::common_dir(&plan.checkout)? != gitio::common_dir(&plan.invocation)?
        || gitio::current_branch(&plan.checkout)?.as_deref() != Some(&plan.state.branch)
        || gitio::head(&plan.checkout)? != expected
    {
        return refuse(
            1,
            format!(
                "checkout {} moved repository, branch, registration, or HEAD identity",
                plan.checkout.display()
            ),
        );
    }
    if plan.checkout_admin.as_ref().is_some_and(|admin| {
        !gitio::worktree_admin_matches(&plan.invocation, &plan.checkout, admin)
    }) {
        return refuse(
            1,
            "checkout administrative identity moved; inspect it before retrying",
        );
    }
    if require_clean && !gitio::is_clean(&ctx.cwd)? {
        return refuse(
            1,
            format!(
                "checkout {} has uncommitted content; preserve it and retry",
                ctx.cwd.display()
            ),
        );
    }
    Ok(())
}

fn check_execution(
    store: &Store,
    change: &str,
    before: &BTreeSet<String>,
    gate: &str,
) -> Result<()> {
    let events = store.load_events(change)?;
    let evidence = events
        .iter()
        .filter(|event| !before.contains(&event.event_id))
        .find(|event| matches!(event.payload, Payload::VerificationRecorded { .. }))
        .context("local gate produced no observed verification evidence")?;
    if let Payload::VerificationRecorded {
        exit_code,
        timed_out,
        tree_moved,
        revision,
        ..
    } = &evidence.payload
    {
        if exit_code.is_none() || matches!(exit_code, Some(126 | 127)) || *timed_out || *tree_moved
        {
            return refuse(1, format!("gate {gate} at {revision} could not execute or be observed; evidence {} remains recorded; inspect it before retrying", evidence.event_id));
        }
    }
    Ok(())
}

fn verify_args() -> VerifyArgs {
    VerifyArgs {
        all: false,
        parallel: false,
        skip_green: false,
        gate: None,
        command: None,
        probe: None,
        brief_version: None,
        probe_phase: None,
        attest: false,
        result: None,
        tested_revision: None,
        execution_host: None,
        runner: None,
        environment: None,
        note: None,
        waive_dirty: None,
        falsified_by: None,
        predicted: None,
        against: None,
    }
}

fn snapshot_argv(plan: &AcceptancePlan) -> Vec<String> {
    if (plan.brief_version.is_none() && !plan.state.briefs.is_empty())
        || plan.contributors.iter().any(|actor| actor.contains(','))
    {
        return Vec::new();
    }
    let mut argv = command([
        "snapshot",
        &plan.state.change_id,
        "--base",
        &plan.base,
        "--contributors",
        &plan.contributors.join(","),
    ]);
    if let Some(version) = plan.brief_version {
        argv.extend(["--brief-version".into(), version.to_string()]);
    }
    argv
}

fn execute_acceptance(
    ctx: &Ctx,
    args: &AcceptArgs,
    plan: &AcceptancePlan,
    result: &mut AcceptanceResult,
) -> Result<i32> {
    result.delivered_head = Some(plan.delivered_head.clone());
    result.prepared_head = Some(plan.head.clone());
    result.worktree = Some(plan.checkout.clone());
    let mut sign_argv = command(["rewrite", "sign"]);
    if let Some(key) = &args.key {
        sign_argv.extend(["--key".into(), key.clone()]);
    }
    let checkout_argv = vec![
        "git".into(),
        "worktree".into(),
        "add".into(),
        "--no-checkout".into(),
        plan.checkout.display().to_string(),
        plan.state.branch.clone(),
    ];
    let object_format = gitio::git(&ctx.cwd, &["rev-parse", "--show-object-format"])?;
    let null_oid = match object_format.as_str() {
        "sha1" => "0".repeat(40),
        "sha256" => "0".repeat(64),
        _ => {
            return refuse(
                1,
                format!("unsupported Git object format {object_format:?}"),
            )
        }
    };
    let population_argv = vec![
        "git".into(),
        "read-tree".into(),
        "--reset".into(),
        "-u".into(),
        "HEAD".into(),
    ];
    let hook_argv = vec![
        "git".into(),
        "hook".into(),
        "run".into(),
        "--ignore-missing".into(),
        "post-checkout".into(),
        "--".into(),
        null_oid,
        plan.head.clone(),
        "1".into(),
    ];
    let mut operations = vec![(
        "import".to_string(),
        command([
            "import",
            &args.input,
            "--publisher",
            &args.publisher,
            "--received-sha256",
            &args.received_sha256,
        ]),
        ctx.cwd.clone(),
    )];
    if plan.create_checkout {
        operations.push((
            "checkout-allocation".into(),
            checkout_argv.clone(),
            ctx.cwd.clone(),
        ));
    }
    operations.push(("checkout-binding".into(), Vec::new(), ctx.cwd.clone()));
    if plan.populate_checkout {
        operations.push((
            "checkout-population".into(),
            population_argv.clone(),
            plan.checkout.clone(),
        ));
        operations.push((
            "checkout-hook".into(),
            hook_argv.clone(),
            plan.checkout.clone(),
        ));
    }
    operations.push(("sign".into(), sign_argv.clone(), plan.checkout.clone()));
    operations.push((
        "snapshot".into(),
        snapshot_argv(plan),
        plan.checkout.clone(),
    ));
    let gates = ctx.declarations(&plan.state)?.gates;
    for (name, _) in gates.required_for(&plan.state.profile) {
        operations.push((
            format!("gate:{name}"),
            command(["verify", &plan.state.change_id, "--gate", name]),
            plan.checkout.clone(),
        ));
    }
    let target_tree_needed = !gitio::is_ancestor(&ctx.cwd, &plan.target, &plan.head)?;
    if target_tree_needed {
        operations.push((
            "target-tree".into(),
            command([
                "verify",
                &plan.state.change_id,
                "--against",
                &plan.state.target_branch,
                "--skip-green",
            ]),
            plan.checkout.clone(),
        ));
    }
    for probe in &plan.probes {
        for phase in ["baseline", "final"] {
            operations.push((
                format!("probe:{}:{phase}", probe.name),
                command([
                    "verify",
                    &plan.state.change_id,
                    "--probe",
                    &probe.name,
                    "--brief-version",
                    &plan.brief_version.unwrap().to_string(),
                    "--probe-phase",
                    phase,
                ]),
                plan.checkout.clone(),
            ));
        }
    }
    operations.push((
        "readiness".into(),
        command(["check", &plan.state.change_id, "--json"]),
        plan.checkout.clone(),
    ));
    if args.dry_run {
        for (name, argv, cwd) in operations {
            result.steps.push(AcceptanceStep {
                name,
                kind: if argv.is_empty() {
                    "internal"
                } else {
                    "command"
                },
                effects: Effects::default(),
                argv,
                cwd,
                outcome: "planned; signing and verification outcomes are conditional".into(),
                exit_code: None,
                event_ids: Vec::new(),
            });
        }
        result.stopped_at = None;
        result.next_steps.push("Run the same acceptance command without --dry-run; signing and verification remain conditional.".into());
        return Ok(0);
    }
    step(result, ctx, "import", operations[0].1.clone(), || {
        revalidate(ctx, plan, &plan.head)?;
        let imported = bundle_io::import_validated_guarded(
            ctx,
            &plan.bytes,
            &plan.bundle,
            false,
            false,
            Some(&args.publisher),
            Some(&args.received_sha256),
            Some(&|| revalidate(ctx, plan, &plan.head)),
        )?;
        if imported.exit_code != 0 {
            return refuse(
                imported.exit_code,
                "import refused; publication result retained in transcript",
            );
        }
        Ok(())
    })?;
    if plan.create_checkout {
        step(result, ctx, "checkout-allocation", checkout_argv, || {
            let store = ctx.store()?;
            let _replica = crate::replica::lock(&store)?;
            let _transition = store.lock_transition(&plan.state.change_id)?;
            revalidate(ctx, plan, &plan.head)?;
            if std::fs::symlink_metadata(&plan.checkout).is_ok() {
                return refuse(
                    1,
                    format!("allocated path {} became occupied", plan.checkout.display()),
                );
            }
            gitio::add_worktree(&ctx.cwd, &plan.checkout, &plan.state.branch)
        })?;
    }
    step(result, ctx, "checkout-binding", Vec::new(), || {
        let store = ctx.store()?;
        let _replica = crate::replica::lock(&store)?;
        let _transition = store.lock_transition(&plan.state.change_id)?;
        revalidate(ctx, plan, &plan.head)?;
        let current = store.state(&plan.state.change_id)?;
        if !plan.create_checkout {
            validate_checkout(
                &ctx.with_cwd(plan.checkout.clone()),
                plan,
                &plan.head,
                !plan.populate_checkout,
            )?;
            if let Some(allocation) = &plan.allocation {
                allocation.validate(
                    ctx,
                    &store,
                    &plan.state.change_id,
                    &plan.state.branch,
                    &plan.head,
                )?;
            }
            if plan.populate_checkout
                && plan
                    .allocation
                    .as_ref()
                    .is_some_and(|allocation| !allocation.unpopulated().unwrap_or(false))
            {
                return refuse(
                    1,
                    "owned allocation gained content or an index; preserve it before retrying",
                );
            }
        }
        if current
            .worktree
            .as_deref()
            .is_some_and(|path| config::resolved(Path::new(path)) == plan.checkout)
        {
            return Ok(());
        }
        let (created, token) = if let Some(allocation) = &plan.allocation {
            allocation.ensure_owner(ctx, &store)?;
            (true, Some(allocation.owner_token.clone()))
        } else if plan.create_checkout {
            let token = ulid::Ulid::new().to_string();
            let allocation = Allocation::record(
                ctx,
                &store,
                &plan.state.change_id,
                &plan.state.branch,
                &plan.checkout,
                &plan.head,
                token.clone(),
            )?;
            allocation.ensure_owner(ctx, &store)?;
            (true, Some(token))
        } else {
            (false, None)
        };
        let event = ctx.event(
            &store,
            &plan.state.change_id,
            Payload::ChangeCheckoutBound {
                branch: plan.state.branch.clone(),
                worktree: plan.checkout.display().to_string(),
                worktree_created: created,
                worktree_owner_token: token,
            },
        )?;
        ensure_append_allowed(&current, &event.payload)?;
        store.append_event(&event)?;
        println!(
            "bound checkout: {} at {}; locally allocated: {created}",
            plan.state.branch,
            plan.checkout.display()
        );
        Ok(())
    })?;
    let run = ctx.with_cwd(plan.checkout.clone());
    if plan.populate_checkout {
        step(
            result,
            &run,
            "checkout-population",
            population_argv.clone(),
            || {
                revalidate(ctx, plan, &plan.head)?;
                validate_checkout(&run, plan, &plan.head, false)?;
                let allocation = Allocation::load(&plan.checkout)?
                    .context("checkout population lacks local allocation proof")?;
                allocation.validate(
                    ctx,
                    &plan.store,
                    &plan.state.change_id,
                    &plan.state.branch,
                    &plan.head,
                )?;
                if !allocation.unpopulated()? {
                    return refuse(
                        1,
                        "owned allocation gained content or an index before population; keeping it",
                    );
                }
                gitio::git(
                    &run.cwd,
                    &population_argv[1..]
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                )
                .map(drop)
            },
        )?;
        step(result, &run, "checkout-hook", hook_argv.clone(), || {
            revalidate(&run, plan, &plan.head)?;
            gitio::git(
                &run.cwd,
                &hook_argv[1..]
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )?;
            revalidate(&run, plan, &plan.head)
        })?;
    }

    if plan.create_checkout || plan.allocation.is_some() {
        step(
            result,
            &run,
            "checkout-recovery-complete",
            Vec::new(),
            || {
                revalidate(&run, plan, &plan.head)?;
                let store = run.store()?;
                let _replica = crate::replica::lock(&store)?;
                let _transition = store.lock_transition(&plan.state.change_id)?;
                if let Some(allocation) = Allocation::load(&plan.checkout)? {
                    allocation.clear(ctx, &store)?;
                }
                Ok(())
            },
        )?;
    }
    step(result, &run, "sign", sign_argv, || {
        let _replica = crate::replica::lock(&plan.store)?;
        revalidate(&run, plan, &plan.head)?;
        let code = rewrite::sign(
            &run,
            SignArgs {
                key: args.key.clone(),
                from: None,
                include_shared: false,
                dry_run: false,
                no_sign: false,
                retag: false,
            },
        )?;
        if code != 0 {
            return refuse(
                code,
                "scoped signing refused; retry arc rewrite sign in the bound checkout",
            );
        }
        Ok(())
    })?;
    let prepared = gitio::branch_head(&ctx.cwd, &plan.state.branch)?;
    result.prepared_head = Some(prepared.clone());
    step(result, &run, "signature-confirmation", Vec::new(), || {
        if plan
            .store
            .rewrites()?
            .approved_successor(&plan.delivered_head)?
            != prepared
        {
            return refuse(
                11,
                format!(
                    "signing changed more than signatures and translated parents; {}; {}",
                    publishing_remedy(&plan.state),
                    replay(&plan.state, &plan.state.branch)
                ),
            );
        }
        let text = gitio::git(&run.cwd, &["cat-file", "commit", &prepared])?;
        if !text
            .split("\n\n")
            .next()
            .unwrap_or_default()
            .lines()
            .any(|line| line.starts_with("gpgsig "))
        {
            return refuse(1, format!("prepared head {prepared} has no signature"));
        }
        revalidate(&run, plan, &prepared)
    })?;
    step(result, &run, "snapshot", snapshot_argv(plan), || {
        let store = run.store()?;
        let _replica = crate::replica::lock(&store)?;
        let transition = store.lock_transition(&plan.state.change_id)?;
        revalidate(&run, plan, &prepared)?;
        review::snapshot_holding(
            &run,
            &store,
            &plan.state.change_id,
            &transition,
            Some(plan.base.clone()),
            plan.brief_version,
            review::SnapshotRequest::delivered(plan.contributors.clone()),
        )?;
        Ok(())
    })?;
    if gates.required_for(&plan.state.profile).is_empty() {
        result.steps.push(AcceptanceStep {
            name: "head-gates".into(),
            kind: "internal",
            effects: Effects::default(),
            argv: Vec::new(),
            cwd: run.cwd.clone(),
            outcome: format!(
                "no gates declared for profile {}; nothing was evaluated",
                plan.state.profile
            ),
            exit_code: Some(0),
            event_ids: Vec::new(),
        });
    }
    for (name, _) in gates.required_for(&plan.state.profile) {
        step(
            result,
            &run,
            &format!("gate:{name}"),
            command(["verify", &plan.state.change_id, "--gate", name]),
            || {
                revalidate(&run, plan, &prepared)?;
                let mut verify = verify_args();
                verify.gate = Some(name.clone());
                let before = event_ids(&plan.store, Some(&plan.state.change_id))?;
                let code = gatekeeping::verify(&run, &plan.state.change_id, verify)?;
                check_execution(&plan.store, &plan.state.change_id, &before, name)?;
                revalidate(&run, plan, &prepared)?;
                if code != 0 {
                    return refuse(5, format!("required local gate {name} failed; recorded evidence remains; run arc verify {} --gate {}", crate::context::shell_quote(&plan.state.change_id), crate::context::shell_quote(name)));
                }
                revalidate(&run, plan, &prepared)
            },
        )?;
    }
    if target_tree_needed {
        step(
            result,
            &run,
            "target-tree",
            command([
                "verify",
                &plan.state.change_id,
                "--against",
                &plan.state.target_branch,
                "--skip-green",
            ]),
            || {
                revalidate(&run, plan, &prepared)?;
                let merged = gitio::merge_outcome(&run.cwd, &plan.target, &prepared)?;
                let tree = match merged.tree {
                    Some(tree) => tree,
                    None => {
                        return refuse(
                            11,
                            format!(
                                "target {} conflicts; {}",
                                plan.state.target_branch,
                                replay(&plan.state, &plan.state.branch)
                            ),
                        )
                    }
                };
                if tree != gitio::commit_tree(&run.cwd, &prepared)?
                    && !gates.required_for(&plan.state.profile).is_empty()
                {
                    let mut verify = verify_args();
                    verify.against = Some(plan.state.target_branch.clone());
                    verify.skip_green = true;
                    let code = gatekeeping::verify(&run, &plan.state.change_id, verify)?;
                    if code != 0 {
                        return refuse(5, "required target-tree gates failed; inspect recorded evidence and rerun arc verify --against with --skip-green");
                    }
                }
                revalidate(&run, plan, &prepared)
            },
        )?;
    }
    for probe in &plan.probes {
        for (phase, label) in [
            (ProbePhase::Baseline, "baseline"),
            (ProbePhase::Final, "final"),
        ] {
            step(
                result,
                &run,
                &format!("probe:{}:{label}", probe.name),
                command([
                    "verify",
                    &plan.state.change_id,
                    "--probe",
                    &probe.name,
                    "--brief-version",
                    &plan.brief_version.unwrap().to_string(),
                    "--probe-phase",
                    label,
                ]),
                || {
                    revalidate(&run, plan, &prepared)?;
                    let before = event_ids(&plan.store, Some(&plan.state.change_id))?;
                    let mut verify = verify_args();
                    verify.probe = Some(probe.name.clone());
                    verify.brief_version = plan.brief_version;
                    verify.probe_phase = Some(phase);
                    let code = gatekeeping::verify(&run, &plan.state.change_id, verify)?;
                    let events = plan.store.load_events(&plan.state.change_id)?;
                    let evidence = events
                        .iter()
                        .filter(|event| !before.contains(&event.event_id))
                        .find_map(|event| match &event.payload {
                            Payload::VerificationRecorded {
                                exit_code,
                                timed_out,
                                tree_moved,
                                revision,
                                ..
                            } => Some((exit_code, timed_out, tree_moved, revision)),
                            _ => None,
                        })
                        .context("probe execution produced no observed evidence")?;
                    if evidence.0.is_none()
                        || matches!(evidence.0, Some(126 | 127))
                        || *evidence.1
                        || *evidence.2
                    {
                        return refuse(1, format!("probe {:?}, brief v{}, {label} at {} could not execute or be observed; inspect recorded evidence and rerun arc verify {} --probe {} --brief-version {} --probe-phase {label}", probe.name, plan.brief_version.unwrap(), evidence.3, crate::context::shell_quote(&plan.state.change_id), crate::context::shell_quote(&probe.name), plan.brief_version.unwrap()));
                    }
                    if code != 0 {
                        return refuse(12, format!("probe {:?}, brief v{}, {label} at {} failed discrimination; inspect recorded evidence before rerunning arc verify {} --probe {} --brief-version {} --probe-phase {label}", probe.name, plan.brief_version.unwrap(), evidence.3, crate::context::shell_quote(&plan.state.change_id), crate::context::shell_quote(&probe.name), plan.brief_version.unwrap()));
                    }
                    revalidate(&run, plan, &prepared)
                },
            )?;
        }
    }
    let report = step(
        result,
        &run,
        "readiness",
        command(["check", &plan.state.change_id, "--json"]),
        || {
            revalidate(&run, plan, &prepared)?;
            let current = run.store()?.state(&plan.state.change_id)?;
            let shape = crate::style::resolved_style(
                &run.cwd,
                &current.target_branch,
                crate::style::StyleFlags::default(),
            )?
            .shape();
            run.report_for_integration(&run.store()?, &current, &current.target_branch, shape)
        },
    )?;
    let code = status::check_exit_code(&report);
    let current = plan.store.state(&plan.state.change_id)?;
    result.verdict_tips = current.eligible_verdict_tips().iter().map(|verdict| serde_json::json!({"event_id": verdict.event_id, "origin_replica_id": verdict.origin_replica_id})).collect();
    result.local_verdict_owed = !report.has_valid_approval;
    result.next_steps.push(render::next_step(&report));
    result.readiness = Some(report);
    result.preparation_complete = true;
    result.stopped_at = if code == 0 {
        None
    } else {
        Some("readiness".into())
    };
    if let Some(step) = result.steps.last_mut() {
        step.exit_code = Some(code);
    }
    Ok(code)
}

fn render_acceptance(result: &AcceptanceResult, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
    } else {
        println!(
            "accept: {}",
            result.change_id.as_deref().unwrap_or("unresolved delivery")
        );
        for step in &result.steps {
            println!(
                "{} [{}]: {}",
                step.name,
                step.exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "conditional".into()),
                step.outcome.trim_end()
            );
            if !step.argv.is_empty() {
                println!(
                    "  command: {}",
                    step.argv
                        .iter()
                        .map(|arg| crate::context::shell_quote(arg))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                println!(
                    "    cwd: {}",
                    crate::context::shell_quote(&step.cwd.display().to_string())
                );
            }
            println!("  operation: {}", step.kind);
            for moved in &step.effects.refs {
                println!(
                    "  ref moved: {} {} -> {}",
                    moved.name,
                    moved.before.as_deref().unwrap_or("absent"),
                    moved.after.as_deref().unwrap_or("absent")
                );
            }
            for object in &step.effects.objects {
                println!("  {} written: {}", object.kind, object.oid);
            }
            for checkout in &step.effects.worktrees_added {
                println!(
                    "  checkout allocated: {} ({:?})",
                    checkout.path.display(),
                    checkout.branch
                );
            }
            for checkout in &step.effects.worktrees_removed {
                println!("  checkout removed: {}", checkout.path.display());
            }
            for owner in &step.effects.owner_records {
                println!(
                    "  owner record written: {} change={} token={}",
                    owner.path.display(),
                    owner.change_id,
                    owner.token
                );
            }
            if let Some(binding) = &step.effects.pending_binding {
                println!(
                    "  {}: {} owner token={} recovery={}",
                    if binding.binding_event_id.is_none() {
                        "unpublished binding"
                    } else {
                        "binding awaiting population"
                    },
                    binding.allocation.worktree.display(),
                    binding.allocation.owner_token,
                    binding.allocation.path().display()
                );
            }
            if let Some(pending) = &step.effects.pending_rewrite {
                println!(
                    "  pending rewrite: {} (recorded event: {})",
                    pending.path.display(),
                    pending.recorded_event_id.as_deref().unwrap_or("unrecorded")
                );
                for (old, new) in &pending.intent.mapping {
                    println!("    map: {old} -> {}", new.as_deref().unwrap_or("dropped"));
                }
                for (name, moved) in &pending.intent.refs {
                    println!(
                        "    planned ref: {name} {} -> {}; current {}",
                        moved.old,
                        moved.new,
                        pending
                            .ref_states
                            .get(name)
                            .and_then(|value| value.as_deref())
                            .unwrap_or("absent or unobserved")
                    );
                }
                for (name, commit) in &pending.intent.retained {
                    println!(
                        "    planned retention: {name} {commit}; current {}",
                        pending
                            .ref_states
                            .get(name)
                            .and_then(|value| value.as_deref())
                            .unwrap_or("absent or unobserved")
                    );
                }
                println!(
                    "  continue: {}",
                    pending
                        .continuation
                        .argv
                        .iter()
                        .map(|arg| crate::context::shell_quote(arg))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                println!(
                    "    cwd: {} (restore ledger publication access before continuing)",
                    crate::context::shell_quote(&pending.continuation.cwd.display().to_string())
                );
            }
            for error in &step.effects.observation_errors {
                println!("  observation incomplete: {error}");
            }
            if !step.event_ids.is_empty() {
                println!("  recorded: {}", step.event_ids.join(", "));
            }
        }
        println!(
            "preparation complete: {}; local verdict owed: {}",
            result.preparation_complete, result.local_verdict_owed
        );
        for next in &result.next_steps {
            println!("next: {next}");
        }
    }
    Ok(())
}

pub fn accept_role_refusal(
    publisher: &str,
    received_sha256: &str,
    json: bool,
    role: &str,
) -> Result<i32> {
    let result = AcceptanceResult {
        schema: ACCEPT_SCHEMA,
        change_id: None,
        source_replica_id: None,
        publisher: publisher.into(),
        received_sha256: received_sha256.into(),
        delivered_head: None,
        prepared_head: None,
        worktree: None,
        preparation_complete: false,
        steps: Vec::new(),
        stopped_at: Some("preflight".into()),
        readiness: None,
        local_verdict_owed: true,
        verdict_tips: Vec::new(),
        next_steps: vec![format!(
            "role refusal: {role} may not accept (requires lead)"
        )],
    };
    render_acceptance(&result, json)?;
    Ok(9)
}

pub fn accept(ctx: &Ctx, args: AcceptArgs) -> Result<i32> {
    let mut result = AcceptanceResult {
        schema: ACCEPT_SCHEMA,
        change_id: None,
        source_replica_id: None,
        publisher: args.publisher.clone(),
        received_sha256: args.received_sha256.clone(),
        delivered_head: None,
        prepared_head: None,
        worktree: None,
        preparation_complete: false,
        steps: Vec::new(),
        stopped_at: None,
        readiness: None,
        local_verdict_owed: true,
        verdict_tips: Vec::new(),
        next_steps: Vec::new(),
    };
    let operation = plan_acceptance(ctx, &args, &mut result)
        .and_then(|plan| execute_acceptance(ctx, &args, &plan, &mut result));
    let code = match operation {
        Ok(code) => code,
        Err(error) => {
            if result.prepared_head.is_some() {
                result.prepared_head = gitio::branch_head(&ctx.cwd, &args.branch).ok();
            }
            result.next_steps.push(format!("{error:#}"));
            if !args.dry_run
                && result
                    .steps
                    .iter()
                    .any(|step| !step.event_ids.is_empty() || step.effects.retained())
            {
                result.next_steps.push(format!(
                    "Earlier effects remain recorded. Inspect the transcript, then retry: {}",
                    acceptance_argv(&args)
                        .iter()
                        .map(|word| crate::context::shell_quote(word))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            exit_code(&error)
        }
    };
    render_acceptance(&result, args.json)?;
    Ok(code)
}
