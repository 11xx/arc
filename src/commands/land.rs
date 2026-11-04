//! Must-land-together relations, and `arc land`: several changes installed on
//! their target by one guarded transition.

use crate::gitio;
use crate::landing::{self, Landings};
use crate::model::{Closure, Payload};
use crate::state::ChangeState;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;

use super::Ctx;

/// Declare or withdraw must-land-together relations between one change and
/// others. Each relation is one repository event, written under the graph
/// lock and then the repository-events lock, so both ends appear in one
/// append and a landing holding the graph lock sees the frontier unmoved.
pub fn lands_with(
    ctx: &Ctx,
    reference: &str,
    declare: Vec<String>,
    withdraw: Vec<String>,
    reason: Option<String>,
) -> Result<()> {
    let reason = reason.map(|reason| reason.trim().to_string());
    if !withdraw.is_empty() && reason.as_deref().is_none_or(str::is_empty) {
        bail!("--remove-lands-with needs --reason: say why the changes may now land apart");
    }
    if withdraw.is_empty() && reason.is_some() {
        bail!("--reason explains a withdrawn relation; pass it with --remove-lands-with");
    }
    let store = ctx.store()?;
    ctx.ensure_declared_actor(&store)?;
    let _graph = store.lock_graph()?;
    let _repository_events = store.lock_repository_events()?;
    let change_id = store.resolve_change(reference)?;
    let state = store.state(&change_id)?;
    let mut landings = Landings::from_events(store.repository_events()?.iter());
    let mut payloads = Vec::new();
    for other in declare {
        let other = store.resolve_change(&other)?;
        if other == change_id {
            bail!("a change cannot land with itself");
        }
        if landings.related(&change_id, &other) {
            bail!("{change_id} already lands with {other}");
        }
        landing::ensure_relatable(&state, &store.state(&other)?)?;
        payloads.push(Payload::LandsWithDeclared {
            changes: landing::pair(&change_id, &other),
        });
    }
    for other in withdraw {
        let other = store.resolve_change(&other)?;
        if !landings.related(&change_id, &other) {
            bail!("{change_id} does not land with {other}; nothing to withdraw");
        }
        payloads.push(Payload::LandsWithWithdrawn {
            changes: landing::pair(&change_id, &other),
            reason: reason.clone().unwrap_or_default(),
        });
    }
    if payloads.is_empty() {
        bail!("provide --lands-with or --remove-lands-with");
    }
    for payload in payloads {
        let mut event = ctx.event(&store, Store::REPOSITORY_SCOPE, payload)?;
        store.append_local_repository_event(&mut event)?;
        println!("event: {}", event.event_id);
        landings = Landings::from_events(
            store
                .repository_events()?
                .iter()
                .chain(std::iter::once(&*event)),
        );
    }
    let partners = landings.partners(&change_id);
    if partners.is_empty() {
        println!("{change_id} lands alone");
    } else {
        println!("{change_id} lands with: {}", partners.join(", "));
    }
    Ok(())
}

/// One member of a must-land-together component, as a report shows it.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentMember {
    pub change_id: String,
    pub slug: String,
    /// `open`, `integrated`, `abandoned`, `superseded`, or `missing`.
    pub status: String,
}

/// A change's must-land-together component and whether it lets the change
/// integrate alone.
#[derive(Debug, Clone, Serialize)]
pub struct LandsWithReport {
    /// The other members, sorted.
    pub members: Vec<ComponentMember>,
    /// Another member is still open: they integrate only together.
    pub open_members: Vec<String>,
    /// A member closed without integrating: the relation must be withdrawn.
    pub wedged_members: Vec<String>,
}

impl LandsWithReport {
    pub fn blocks(&self) -> bool {
        !self.open_members.is_empty() || !self.wedged_members.is_empty()
    }

    /// The command that clears the block, with the change filled in.
    pub fn remedy(&self, change_id: &str) -> String {
        if let Some(wedged) = self.wedged_members.first() {
            return format!("arc metadata {change_id} --remove-lands-with {wedged} --reason <why>");
        }
        let mut all = vec![change_id.to_string()];
        all.extend(self.open_members.iter().cloned());
        all.sort();
        format!("arc land {}", all.join(" "))
    }
}

/// The component report for `state`, read against every change's state.
pub fn lands_with_report(
    state: &ChangeState,
    states: &BTreeMap<String, ChangeState>,
) -> Option<LandsWithReport> {
    if state.lands_with.is_empty() {
        return None;
    }
    let mut report = LandsWithReport {
        members: Vec::new(),
        open_members: Vec::new(),
        wedged_members: Vec::new(),
    };
    for member in &state.lands_with {
        let (slug, status) = match states.get(member) {
            None => (member.clone(), "missing"),
            Some(other) => (
                other.slug.clone(),
                match other.closure.as_ref().map(|closure| closure.outcome) {
                    None => "open",
                    Some(Closure::Integrated) => "integrated",
                    Some(Closure::Abandoned) => "abandoned",
                    Some(Closure::Superseded) => "superseded",
                },
            ),
        };
        match status {
            "open" => report.open_members.push(member.clone()),
            "integrated" => {}
            _ => report.wedged_members.push(member.clone()),
        }
        report.members.push(ComponentMember {
            change_id: member.clone(),
            slug,
            status: status.to_string(),
        });
    }
    Some(report)
}

/// Refuse a terminal transition on a change a landing attempt holds.
pub fn refuse_pending(state: &ChangeState) -> Result<()> {
    if let Some(attempt) = &state.pending_landing {
        bail!(
            "{} is a member of pending landing {attempt}; finish it with `arc land --recover \
             {attempt}` before any other terminal transition",
            state.change_id
        );
    }
    Ok(())
}

/// Refuse integrating `state` alone while its component says otherwise.
pub fn refuse_alone(state: &ChangeState, states: &BTreeMap<String, ChangeState>) -> Result<()> {
    refuse_pending(state)?;
    if let Some(report) = lands_with_report(state, states).filter(LandsWithReport::blocks) {
        bail!("{}", describe_block(&state.change_id, &report));
    }
    Ok(())
}

/// One line naming why the component blocks and what clears it.
pub fn describe_block(change_id: &str, report: &LandsWithReport) -> String {
    if !report.wedged_members.is_empty() {
        format!(
            "{change_id} must land with {}, which closed without integrating; withdraw the \
             relation: {}",
            report.wedged_members.join(", "),
            report.remedy(change_id)
        )
    } else {
        format!(
            "{change_id} must land with {}, still open; land them together: {}",
            report.open_members.join(", "),
            report.remedy(change_id)
        )
    }
}

/// Everything `arc land` was asked for.
pub struct LandArgs {
    pub changes: Vec<String>,
    pub target: Option<String>,
    pub debt: Option<String>,
    pub dry_run: bool,
    pub json: bool,
    pub recover: Option<String>,
    pub abandon: Option<String>,
    pub reason: Option<String>,
    /// How many attempts a landing may make, its first included.
    pub max_attempts: u32,
}

pub const PLAN_SCHEMA: &str = "arc-landing-plan/1";

/// A refusal with the exit code of the blocker it answers to.
struct Refusal {
    code: i32,
    message: String,
}

fn refuse<T>(code: i32, message: impl Into<String>) -> Result<std::result::Result<T, Refusal>> {
    Ok(Err(Refusal {
        code,
        message: message.into(),
    }))
}

/// One member as the plan pins it. Commit compares a fresh reading of every
/// field with this one, and any difference refuses naming the field.
#[derive(Debug, Clone, Serialize)]
struct MemberPin {
    change_id: String,
    slug: String,
    patchset_id: String,
    base: String,
    head: String,
    /// Whether the landing replays the member onto a base other than the
    /// one its patchset records.
    replayed: bool,
    /// The approval of the exact patchset a replayed member carries, which
    /// read its edit and not the base it lands on.
    #[serde(skip_serializing_if = "Option::is_none")]
    replayed_approval: Option<String>,
    /// Where the change's branch stands now.
    branch_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brief_digest: Option<String>,
    /// Every verdict recorded on the patchset, by event.
    verdicts: Vec<String>,
    external_verdicts: Vec<String>,
    approval: ApprovalPin,
    holds: Vec<String>,
    blocking_findings: Vec<String>,
    /// The latest debt declared on the change, by event.
    #[serde(skip_serializing_if = "Option::is_none")]
    debt: Option<String>,
    /// The debt this landing would declare in place of an absent approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    planned_debt: Option<PlannedDebt>,
    lands_with: Vec<String>,
    prerequisites: Vec<PrerequisitePin>,
    /// The member's required gates and their declaration identities.
    gates: BTreeMap<String, String>,
    policy: serde_json::Value,
    danger: crate::model::DangerScope,
    iterating: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ApprovalPin {
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    external_verdict_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    debt_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PlannedDebt {
    kind: crate::model::DebtMissing,
    reason: String,
}

#[derive(Debug, Clone, Serialize)]
struct PrerequisitePin {
    change_id: String,
    /// `landing` for an earlier member of this landing, else the closure
    /// event that satisfied it.
    satisfied_by: String,
}

#[derive(Debug, Clone, Serialize)]
struct GateUnion {
    identity: String,
    binds: &'static str,
    command: String,
    required_by: Vec<String>,
}

/// What a landing would do, decided before anything is written.
#[derive(Debug, Clone, Serialize)]
struct Plan {
    schema: &'static str,
    /// `stack` when every member stands on the one before it, the first on
    /// the target; `composed` when arc replays a member onto another base.
    shape: Shape,
    target_branch: String,
    target_before: String,
    /// The revision the landing installs: the top member's head for a
    /// stack, the composed head once a composed landing has replayed.
    target_after: Option<String>,
    tree: Option<String>,
    members: Vec<MemberPin>,
    components: Vec<Vec<String>>,
    gates: BTreeMap<String, GateUnion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Shape {
    Stack,
    Composed,
}

impl Plan {
    fn after(&self) -> &str {
        self.target_after
            .as_deref()
            .expect("a landing about to move its target knows its head")
    }

    fn installed_tree(&self) -> &str {
        self.tree
            .as_deref()
            .expect("a landing about to move its target knows its tree")
    }
}

/// A plan with the readings authorization is built from.
struct Planned {
    plan: Plan,
    states: Vec<ChangeState>,
    reports: Vec<crate::status::StatusReport>,
    declarations: Vec<crate::integration::Declarations>,
    /// Every gate a member requires, under its one shared declaration.
    gates: Vec<(String, crate::gates::Gate)>,
}

fn brief_digest(state: &ChangeState) -> Option<String> {
    use sha2::{Digest, Sha256};
    state.latest_brief().map(|brief| {
        format!(
            "sha256:{}",
            hex::encode(Sha256::digest(brief.body.as_bytes()))
        )
    })
}

/// Blockers a landing answers itself: gates run once at the top head, an
/// earlier member satisfies a prerequisite, and the landing holds the whole
/// component. Every other blocker refuses.
fn landing_answers(blocker: crate::blockers::Blocker, replayed: bool) -> bool {
    use crate::blockers::Blocker;
    matches!(
        blocker,
        Blocker::GatesNotGreen
            | Blocker::MergedTreeUnevaluated
            | Blocker::BlockedByChanges
            | Blocker::LandsWith
    ) || (replayed && blocker == Blocker::NeedsRebase)
}

/// The members in dependency order: each after every member it is
/// `blocked_by`, ties kept in the order given. `None` when the prerequisites
/// among them form a cycle.
fn dependency_order(members: &[ChangeState]) -> Option<Vec<usize>> {
    let ids: Vec<&str> = members.iter().map(|m| m.change_id.as_str()).collect();
    let mut placed: Vec<usize> = Vec::new();
    while placed.len() < members.len() {
        let next = (0..members.len()).find(|index| {
            !placed.contains(index)
                && members[*index].blocked_by.iter().all(|prerequisite| {
                    ids.iter()
                        .position(|id| id == prerequisite)
                        .is_none_or(|position| placed.contains(&position))
                })
        })?;
        placed.push(next);
    }
    Some(placed)
}

/// Plan a stack landing of `references`. Reads only; a refusal carries the
/// exit code of the blocker it answers to.
fn plan(
    ctx: &Ctx,
    store: &Store,
    references: &[String],
    target: Option<&str>,
    debt: Option<&str>,
    attempt: Option<&str>,
) -> Result<std::result::Result<Planned, Refusal>> {
    use crate::blockers::Blocker;
    if references.is_empty() {
        bail!("name the changes to land, in landing order");
    }
    let mut ids = Vec::new();
    for reference in references {
        let id = store.resolve_change(reference)?;
        if ids.contains(&id) {
            bail!("{id} is named twice; each member lands once");
        }
        ids.push(id);
    }
    let states = store.readable_states()?;
    let mut members = Vec::new();
    for id in &ids {
        let state = states
            .get(id)
            .cloned()
            .with_context(|| format!("{id} cannot be read"))?;
        members.push(state);
    }
    let Some(order) = dependency_order(&members) else {
        return refuse(
            Blocker::BlockedByChanges.exit_code(),
            format!(
                "the prerequisites among {} form a cycle, so no member can land first",
                ids.join(", ")
            ),
        );
    };
    let members: Vec<ChangeState> = order.iter().map(|index| members[*index].clone()).collect();
    let ids: Vec<String> = order.iter().map(|index| ids[*index].clone()).collect();
    let explicit_target = target;
    let target = match target {
        Some(target) => target.to_string(),
        None => members[0].target_branch.clone(),
    };
    for member in &members {
        if member.is_closed() {
            return refuse(
                Blocker::Closed.exit_code(),
                format!("{} is already closed", member.change_id),
            );
        }
        if let Some(pending) = member
            .pending_landing
            .as_deref()
            .filter(|pending| Some(*pending) != attempt)
        {
            return refuse(
                Blocker::LandingPending.exit_code(),
                format!(
                    "{} is a member of pending landing {pending}; finish it with `arc land \
                     --recover {pending}`",
                    member.change_id
                ),
            );
        }
        if explicit_target.is_none() && member.target_branch != target {
            return refuse(
                1,
                format!(
                    "{} targets {}, not {target}; one landing moves one target",
                    member.change_id, member.target_branch
                ),
            );
        }
    }
    let Ok(target_before) = gitio::branch_head(&ctx.cwd, &target) else {
        return refuse(
            Blocker::TargetUnreadable.exit_code(),
            format!("target branch {target:?} cannot be resolved"),
        );
    };

    // A member stands where it lands when its recorded base is the revision
    // before it: the target for the first, the previous member's landed head
    // for the rest. Every other member is replayed onto that revision, and
    // every member after a replayed one lands on a commit nobody recorded.
    let mut replayed = Vec::new();
    let mut tip = Some(target_before.clone());
    let mut commits = 0usize;
    for member in &members {
        let Some(patchset) = member.latest_patchset() else {
            return refuse(
                Blocker::NoValidApproval.exit_code(),
                format!("{} has no patchset; snapshot it first", member.change_id),
            );
        };
        if gitio::branch_head(&ctx.cwd, &member.branch).ok().as_deref() != Some(&patchset.head) {
            return refuse(
                Blocker::NoValidApproval.exit_code(),
                format!(
                    "{}'s branch moved from its recorded patchset {}; snapshot it first",
                    member.change_id, patchset.id
                ),
            );
        }
        let own: usize = gitio::git(
            &ctx.cwd,
            &[
                "rev-list",
                "--count",
                &format!("{}..{}", patchset.base, patchset.head),
            ],
        )?
        .trim()
        .parse()?;
        if own == 0 {
            return refuse(
                Blocker::NoValidApproval.exit_code(),
                format!(
                    "{}'s patchset {} has no commits to land",
                    member.change_id, patchset.id
                ),
            );
        }
        commits += own;
        let replays = tip.as_deref() != Some(patchset.base.as_str());
        replayed.push(replays);
        tip = (!replays).then(|| patchset.head.clone());
    }
    let shape = if replayed.contains(&true) {
        Shape::Composed
    } else {
        Shape::Stack
    };
    let target_after = tip;
    let tree = target_after
        .as_deref()
        .map(|after| gitio::commit_tree(&ctx.cwd, after))
        .transpose()?;

    // Must-land-together components travel whole.
    let mut components: Vec<Vec<String>> = Vec::new();
    for member in &members {
        let Some(report) = lands_with_report(member, &states) else {
            continue;
        };
        if !report.wedged_members.is_empty() {
            return refuse(
                Blocker::LandsWith.exit_code(),
                describe_block(&member.change_id, &report),
            );
        }
        let missing: Vec<&String> = report
            .open_members
            .iter()
            .filter(|other| !ids.contains(other))
            .collect();
        if !missing.is_empty() {
            let mut all = ids.clone();
            all.extend(missing.iter().map(|other| (*other).clone()));
            return refuse(
                Blocker::LandsWith.exit_code(),
                format!(
                    "{} must land with {}, which this landing leaves out: arc land {}",
                    member.change_id,
                    missing
                        .iter()
                        .map(|other| other.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    all.join(" ")
                ),
            );
        }
        let mut component: Vec<String> = member.lands_with.clone();
        component.push(member.change_id.clone());
        component.sort();
        if !components.contains(&component) {
            components.push(component);
        }
    }

    // The destination governs every member: its policy at the revision the
    // landing starts from.
    let policy = crate::policy::load_at(&ctx.cwd, &target_before)?;
    super::gatekeeping::ensure_destination_actor(ctx, &policy)?;
    if policy.contribution.is_some() {
        return refuse(
            1,
            format!("{target} receives contributions one change at a time; integrate each"),
        );
    }
    let style = crate::style::resolved_style_with_policy(&policy, Default::default())?;
    if style.shape() != crate::integration::Shape::FfOnly {
        return refuse(
            1,
            format!(
                "{target} integrates with a merge commit; a landing fast-forwards it, so \
                 integrate each member"
            ),
        );
    }
    if policy.integration_history == crate::policy::History::Squash {
        let count = commits;
        if count > 1 {
            return refuse(
                1,
                format!(
                    "integration history is squash and the landing has {count} commits since \
                     {}; squash each member and land one commit",
                    crate::render::short_sha(&target_before)
                ),
            );
        }
    }

    let mut pins = Vec::new();
    let mut reports = Vec::new();
    let mut projected_states = Vec::new();
    let mut declarations_used = Vec::new();
    let mut union: BTreeMap<String, GateUnion> = BTreeMap::new();
    let mut gate_declarations: BTreeMap<String, crate::gates::Gate> = BTreeMap::new();
    let top_index = members.len() - 1;
    let mut top_gates: BTreeMap<String, String> = BTreeMap::new();
    for (index, member) in members.iter().enumerate() {
        let mut selected = member.clone();
        selected.target_branch = target.clone();
        ctx.ensure_target_declares_actor(&selected)?;
        let mut report = ctx.report_for_integration(
            store,
            &selected,
            &target,
            crate::integration::Shape::FfOnly,
        )?;
        let mut planned_debt = None;
        if report.blockers().contains(&Blocker::NoValidApproval) {
            if let Some(reason) = debt {
                let debt_plan = super::audit::plan_debt(
                    ctx,
                    store,
                    &selected,
                    reason,
                    None,
                    &super::audit::IneligibleInputs::default(),
                )?;
                if let Payload::DebtDeclared { missing, .. } = debt_plan.payload() {
                    planned_debt = Some(PlannedDebt {
                        kind: *missing,
                        reason: reason.trim().to_string(),
                    });
                }
                selected = debt_plan.projected(store, &selected)?;
                selected.target_branch = target.clone();
                report = ctx.report_for_integration(
                    store,
                    &selected,
                    &target,
                    crate::integration::Shape::FfOnly,
                )?;
            }
        }
        if member.iterating {
            return refuse(
                Blocker::Iterating.exit_code(),
                format!(
                    "{} declares it is iterating; clear it with arc iterating {} --off",
                    member.change_id, member.change_id
                ),
            );
        }
        // Prerequisites: an earlier member satisfies one inside this
        // landing; any other must already be integrated.
        let mut prerequisites = Vec::new();
        for prerequisite in &member.blocked_by {
            if let Some(position) = ids.iter().position(|id| id == prerequisite) {
                if position >= index {
                    return refuse(
                        Blocker::BlockedByChanges.exit_code(),
                        format!(
                            "{} needs {prerequisite} first; name {prerequisite} before it",
                            member.change_id
                        ),
                    );
                }
                prerequisites.push(PrerequisitePin {
                    change_id: prerequisite.clone(),
                    satisfied_by: "landing".into(),
                });
                continue;
            }
            let ready = report
                .blocker_status
                .blockers_ready
                .iter()
                .find(|dependency| &dependency.change_id == prerequisite);
            match ready {
                Some(dependency) if dependency.integrated => prerequisites.push(PrerequisitePin {
                    change_id: prerequisite.clone(),
                    satisfied_by: states
                        .get(prerequisite)
                        .and_then(|state| state.closure.as_ref())
                        .map(|closure| closure.event_id.clone())
                        .unwrap_or_else(|| dependency.status.clone()),
                }),
                _ => {
                    return refuse(
                        Blocker::BlockedByChanges.exit_code(),
                        format!(
                            "{} needs {prerequisite}, which has not integrated and is not in \
                             this landing",
                            member.change_id
                        ),
                    )
                }
            }
        }
        // Its own attempt holds a member it is committing.
        let held_by_this_attempt =
            attempt.is_some() && member.pending_landing.as_deref() == attempt;
        if let Some(blocker) = report.blockers().iter().find(|blocker| {
            !landing_answers(**blocker, replayed[index])
                && !(held_by_this_attempt && **blocker == Blocker::LandingPending)
        }) {
            let mut explanation = crate::render::blocker_explanation(&selected, &report);
            if explanation.ends_with('\n') {
                explanation.pop();
            }
            return refuse(blocker.exit_code(), explanation);
        }
        let patchset = selected
            .latest_patchset()
            .context("a planned member has a patchset")?
            .clone();
        let approval = crate::integration::approval(&selected, &report, &patchset.id);
        // An approval that read this exact patchset, standing on its own
        // rather than through a waiver, read the edit at its recorded base.
        // Replayed onto another base, the context it lands in went unread.
        let replayed_approval =
            (replayed[index] && planned_debt.is_none() && approval.audit_debt_event_id.is_none())
                .then(|| {
                    approval.verdict_event_id.clone().or_else(|| {
                        approval
                            .external_verdict
                            .as_ref()
                            .map(|external| external.event_id.clone())
                    })
                })
                .flatten();
        if let Some(read) = &replayed_approval {
            let Some(reason) = debt else {
                return refuse(
                    Blocker::NoValidApproval.exit_code(),
                    format!(
                        "{}'s approval {read} read it on {}, and this landing replays it onto \
                         another base its approval did not read (replayed-unread); land with \
                         --debt <why>, or land it where it stands",
                        member.change_id,
                        crate::render::short_sha(&patchset.base)
                    ),
                );
            };
            planned_debt = Some(PlannedDebt {
                kind: crate::model::DebtMissing::ReplayedUnread,
                reason: reason.trim().to_string(),
            });
        }
        let declarations = ctx.declarations(&selected)?;
        let required: Vec<(String, String, bool, String, crate::gates::Gate)> = declarations
            .gates
            .required_for_event(&selected.profile, crate::model::GateEvent::Integration)
            .into_iter()
            .map(|(name, gate)| {
                (
                    name.clone(),
                    crate::gates::declaration_identity(gate),
                    gate.binds.is_tree(),
                    gate.command.clone(),
                    (*gate).clone(),
                )
            })
            .collect();
        let mut gates = BTreeMap::new();
        for (name, identity, binds_tree, command, declared) in required {
            let name = &name;
            match union.get_mut(name) {
                Some(existing) if existing.identity != identity => {
                    return refuse(
                        Blocker::GatesNotGreen.exit_code(),
                        format!(
                            "gate {name:?} is declared one way for {} and another for {}; \
                             one landing runs one declaration of each gate",
                            existing.required_by.join(", "),
                            member.change_id
                        ),
                    );
                }
                Some(existing) => existing.required_by.push(member.change_id.clone()),
                None => {
                    gate_declarations.insert(name.clone(), declared);
                    union.insert(
                        name.clone(),
                        GateUnion {
                            identity: identity.clone(),
                            binds: if binds_tree { "tree" } else { "commit" },
                            command: command.clone(),
                            required_by: vec![member.change_id.clone()],
                        },
                    );
                }
            }
            gates.insert(name.clone(), identity);
        }
        if index == top_index {
            top_gates = gates.clone();
        }
        let normalized = super::gatekeeping::normalized_declarations(declarations, &selected);
        pins.push(MemberPin {
            change_id: member.change_id.clone(),
            slug: member.slug.clone(),
            patchset_id: patchset.id.clone(),
            base: patchset.base.clone(),
            head: patchset.head.clone(),
            replayed: replayed[index],
            replayed_approval,
            branch_head: gitio::branch_head(&ctx.cwd, &member.branch).ok(),
            brief_digest: brief_digest(member),
            verdicts: member
                .verdicts
                .iter()
                .filter(|verdict| verdict.patchset_id == patchset.id)
                .map(|verdict| verdict.event_id.clone())
                .collect(),
            external_verdicts: member
                .external_verdicts
                .iter()
                .map(|verdict| verdict.event_id.clone())
                .collect(),
            approval: ApprovalPin {
                verdict_event_id: approval.verdict_event_id.clone(),
                external_verdict_event_id: approval
                    .external_verdict
                    .as_ref()
                    .map(|external| external.event_id.clone()),
                debt_event_id: planned_debt
                    .is_none()
                    .then(|| approval.audit_debt_event_id.clone())
                    .flatten(),
            },
            holds: member.holds.keys().cloned().collect(),
            blocking_findings: report.open_blocking_findings.clone(),
            debt: member.debt.as_ref().map(|debt| debt.event_id.clone()),
            planned_debt,
            lands_with: member.lands_with.clone(),
            prerequisites,
            gates,
            policy: serde_json::to_value(&normalized.policy)?,
            danger: report.danger.clone(),
            iterating: member.iterating,
        });
        reports.push(report);
        projected_states.push(selected);
        declarations_used.push(normalized);
    }
    if union.is_empty() {
        return refuse(
            Blocker::GatesNotGreen.exit_code(),
            "no required gates declared for this landing; declare an integration gate before landing",
        );
    }
    // A stack runs one gate set at its top head: every gate a member
    // requires must be one the top head requires, under the same
    // declaration. A composed landing runs the union at its composed head.
    for (name, gate) in union.iter().filter(|_| shape == Shape::Stack) {
        if top_gates.get(name) != Some(&gate.identity) {
            return refuse(
                Blocker::GatesNotGreen.exit_code(),
                format!(
                    "{} requires gate {name:?}, which the top head {} does not declare the \
                     same way; the landing runs one gate set at the top head",
                    gate.required_by.join(", "),
                    crate::render::short_sha(target_after.as_deref().unwrap_or_default())
                ),
            );
        }
    }
    Ok(Ok(Planned {
        plan: Plan {
            schema: PLAN_SCHEMA,
            shape,
            target_branch: target,
            target_before,
            target_after,
            tree,
            members: pins,
            components,
            gates: union,
        },
        states: projected_states,
        reports,
        declarations: declarations_used,
        gates: gate_declarations.into_iter().collect(),
    }))
}

/// The facts a landing pins, read straight from the ledger and Git with no
/// judgement over them, so a moved one is named even where the fresh plan
/// would refuse for it.
fn observe(
    ctx: &Ctx,
    store: &Store,
    members: &[String],
    target: &str,
) -> Result<serde_json::Value> {
    let states = store.readable_states()?;
    let mut observed = serde_json::Map::new();
    observed.insert(
        "target_before".into(),
        serde_json::json!(gitio::branch_head(&ctx.cwd, target).ok()),
    );
    for id in members {
        let Some(state) = states.get(id) else {
            observed.insert(id.clone(), serde_json::Value::Null);
            continue;
        };
        let patchset = state.latest_patchset();
        let mut findings: Vec<String> = state
            .open_blocking_findings()
            .iter()
            .map(|finding| finding.id.clone())
            .collect();
        findings.sort();
        observed.insert(
            id.clone(),
            serde_json::json!({
                "lifecycle": state.closure.as_ref().map(|closure| format!("{:?}", closure.outcome)),
                "pending_landing": state.pending_landing,
                "patchset_id": patchset.map(|patchset| &patchset.id),
                "base": patchset.map(|patchset| &patchset.base),
                "head": patchset.map(|patchset| &patchset.head),
                "branch_head": gitio::branch_head(&ctx.cwd, &state.branch).ok(),
                "brief_digest": brief_digest(state),
                "verdicts": state.verdicts.iter().map(|verdict| &verdict.event_id).collect::<Vec<_>>(),
                "external_verdicts": state
                    .external_verdicts
                    .iter()
                    .map(|verdict| &verdict.event_id)
                    .collect::<Vec<_>>(),
                "holds": state.holds.keys().collect::<Vec<_>>(),
                "blocking_findings": findings,
                "debt": state.debt.as_ref().map(|debt| &debt.event_id),
                "lands_with": state.lands_with,
                "blocked_by": state.blocked_by,
                "iterating": state.iterating,
            }),
        );
    }
    Ok(serde_json::Value::Object(observed))
}

/// The first observed fact that differs between two readings.
fn moved_observation(pinned: &serde_json::Value, fresh: &serde_json::Value) -> Option<String> {
    if pinned["target_before"] != fresh["target_before"] {
        return Some("target_before".into());
    }
    let members = pinned.as_object()?;
    for (id, old) in members.iter().filter(|(key, _)| *key != "target_before") {
        let new = &fresh[id.as_str()];
        let (Some(old_fields), Some(new_fields)) = (old.as_object(), new.as_object()) else {
            if old != new {
                return Some(id.clone());
            }
            continue;
        };
        for (key, value) in old_fields {
            // Its own attempt holds a member once prepared.
            if key == "pending_landing" {
                continue;
            }
            if new_fields.get(key) != Some(value) {
                return Some(format!("{id}.{key}"));
            }
        }
    }
    None
}

/// The first field where two readings of a plan differ, named by member.
fn moved_field(pinned: &Plan, fresh: &Plan) -> Result<Option<String>> {
    let pinned = serde_json::to_value(pinned)?;
    let fresh = serde_json::to_value(fresh)?;
    for key in [
        "target_branch",
        "target_before",
        "target_after",
        "tree",
        "components",
        "gates",
    ] {
        if pinned[key] != fresh[key] {
            return Ok(Some(key.to_string()));
        }
    }
    let (Some(old), Some(new)) = (pinned["members"].as_array(), fresh["members"].as_array()) else {
        return Ok(Some("members".into()));
    };
    if old.len() != new.len() {
        return Ok(Some("members".into()));
    }
    for (old, new) in old.iter().zip(new) {
        let (Some(old_fields), Some(new_fields)) = (old.as_object(), new.as_object()) else {
            continue;
        };
        let mut keys: Vec<&String> = old_fields.keys().chain(new_fields.keys()).collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            if old_fields.get(key) != new_fields.get(key) {
                return Ok(Some(format!(
                    "{}.{key}",
                    old["change_id"].as_str().unwrap_or("member")
                )));
            }
        }
    }
    Ok(None)
}

fn print_plan(plan: &Plan, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
    }
    match (&plan.target_after, &plan.tree) {
        (Some(after), Some(tree)) => println!(
            "land onto {} at {} -> {} (tree {})",
            plan.target_branch,
            crate::render::short_sha(&plan.target_before),
            crate::render::short_sha(after),
            crate::render::short_sha(tree)
        ),
        _ => println!(
            "land onto {} at {}, composed by replaying the members marked replayed",
            plan.target_branch,
            crate::render::short_sha(&plan.target_before),
        ),
    }
    for (index, member) in plan.members.iter().enumerate() {
        let basis = match (&member.approval, &member.planned_debt) {
            (_, Some(debt)) => format!("debt {} would be declared", debt.kind.as_str()),
            (approval, None) => approval
                .verdict_event_id
                .as_deref()
                .map(|id| format!("verdict {id}"))
                .or_else(|| {
                    approval
                        .external_verdict_event_id
                        .as_deref()
                        .map(|id| format!("external verdict {id}"))
                })
                .or_else(|| {
                    approval
                        .debt_event_id
                        .as_deref()
                        .map(|id| format!("debt {id}"))
                })
                .unwrap_or_else(|| "no approval".into()),
        };
        println!(
            "  {}. {} ({}) {} {}..{}{}: {basis}",
            index + 1,
            member.slug,
            member.change_id,
            member.patchset_id,
            crate::render::short_sha(&member.base),
            crate::render::short_sha(&member.head),
            if member.replayed { ", replayed" } else { "" },
        );
    }
    for component in &plan.components {
        println!("  lands together: {}", component.join(", "));
    }
    for (name, gate) in &plan.gates {
        println!(
            "  gate {name} (binds {}): required by {}",
            gate.binds,
            gate.required_by.join(", ")
        );
    }
    Ok(())
}

/// Stop a test run at a named point, as a crash there would.
fn crash_point(phase: &str) {
    #[cfg(debug_assertions)]
    if std::env::var("ARC_TEST_LANDING_CRASH").as_deref() == Ok(phase) {
        eprintln!("arc test: landing stopped at {phase}");
        std::process::exit(86);
    }
    let _ = phase;
}

/// Run `arc land`.
pub fn land(ctx: &Ctx, args: LandArgs) -> Result<i32> {
    if let Some(attempt) = args.recover.as_deref() {
        if !args.changes.is_empty() || args.abandon.is_some() || args.debt.is_some() {
            bail!("--recover takes only the attempt it finishes");
        }
        return recover(ctx, attempt);
    }
    if let Some(attempt) = args.abandon.as_deref() {
        let reason = args
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .context("--abandon needs --reason: say why the attempt will not finish")?;
        if !args.changes.is_empty() || args.debt.is_some() {
            bail!("--abandon takes only the attempt it abandons");
        }
        return abandon_command(ctx, attempt, reason);
    }
    if args.reason.is_some() {
        bail!("--reason explains --abandon");
    }
    if args.json && !args.dry_run {
        bail!("--json prints the plan; pass it with --dry-run");
    }
    if args.max_attempts == 0 {
        bail!("--max-attempts counts the first attempt, so it is at least 1");
    }
    let store = ctx.store()?;
    ctx.ensure_declared_actor(&store)?;
    if let Some(refusal) = crate::replica::integration_refusal(&store)? {
        eprintln!("Cannot land: {refusal}");
        return Ok(crate::replica::INTEGRATION_AUTHORITY_EXIT_CODE);
    }
    if args.dry_run {
        return match plan(
            ctx,
            &store,
            &args.changes,
            args.target.as_deref(),
            args.debt.as_deref(),
            None,
        )? {
            Ok(planned) => {
                print_plan(&planned.plan, args.json)?;
                Ok(0)
            }
            Err(refusal) => {
                eprintln!("Cannot land: {}", refusal.message);
                Ok(refusal.code)
            }
        };
    }
    let mut first = Attempted::default();
    let code = run_attempt(ctx, &store, &args, &args.changes, None, &mut first)?;
    if code == 0 || !first.gate_failed {
        return Ok(code);
    }
    split(ctx, &store, &args, first, code)
}

/// A must-land-together component, or a member that lands alone, as a
/// split moves it: whole, its members in landing order.
type Node = Vec<String>;

/// The plan's members contracted into must-land-together nodes, ordered so
/// every prerequisite inside the landing comes before its dependent, ties in
/// landing order. `Err` holds the nodes a prerequisite cycle runs through:
/// no part of such a landing can land before another.
fn contracted(plan: &Plan) -> std::result::Result<Vec<Node>, Vec<Node>> {
    let order: Vec<&String> = plan.members.iter().map(|m| &m.change_id).collect();
    let mut nodes: Vec<Node> = Vec::new();
    for id in &order {
        if nodes.iter().any(|node| node.contains(id)) {
            continue;
        }
        let component = plan
            .components
            .iter()
            .find(|component| component.contains(id));
        nodes.push(
            order
                .iter()
                .filter(|other| {
                    *other == id || component.is_some_and(|component| component.contains(other))
                })
                .map(|other| (*other).clone())
                .collect(),
        );
    }
    let node_of = |id: &str| nodes.iter().position(|node| node.iter().any(|m| m == id));
    let mut needs = vec![std::collections::BTreeSet::new(); nodes.len()];
    for member in &plan.members {
        let Some(dependent) = node_of(&member.change_id) else {
            continue;
        };
        for prerequisite in &member.prerequisites {
            if let Some(first) =
                node_of(&prerequisite.change_id).filter(|first| *first != dependent)
            {
                needs[dependent].insert(first);
            }
        }
    }
    let mut placed: Vec<usize> = Vec::new();
    while placed.len() < nodes.len() {
        let next = (0..nodes.len()).find(|index| {
            !placed.contains(index) && needs[*index].iter().all(|need| placed.contains(need))
        });
        match next {
            Some(next) => placed.push(next),
            None => {
                return Err((0..nodes.len())
                    .filter(|index| !placed.contains(index))
                    .map(|index| nodes[index].clone())
                    .collect())
            }
        }
    }
    Ok(placed
        .into_iter()
        .map(|index| nodes[index].clone())
        .collect())
}

/// One line of a split's report.
struct SplitRow {
    attempt: Option<String>,
    parent: Option<String>,
    members: Vec<String>,
    outcome: String,
}

struct SplitRun {
    used: u32,
    rows: Vec<SplitRow>,
}

/// Split a landing whose gates failed: a prefix child and a suffix child
/// of its contracted dependency order, each a new attempt naming its
/// parent, the suffix only once the prefix landed, and a failed child split
/// again while `--max-attempts` allows. The failed parent closes nothing.
fn split(ctx: &Ctx, store: &Store, args: &LandArgs, first: Attempted, code: i32) -> Result<i32> {
    let plan = first.plan.context("an attempt that gated has a plan")?;
    let parent = first.attempt.context("an attempt that gated has an id")?;
    let nodes = match contracted(&plan) {
        Ok(nodes) => nodes,
        Err(cycle) => {
            eprintln!(
                "landing {parent} is unsplittable: its must-land-together components and \
                 prerequisites form a cycle through {}, so no part of it can land before \
                 another",
                cycle
                    .iter()
                    .map(|node| node.join("+"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Ok(code);
        }
    };
    if nodes.len() < 2 {
        eprintln!(
            "landing {parent} is one must-land-together component; there is nothing to split"
        );
        return Ok(code);
    }
    let budget = 2 * nodes.len() - 1;
    if args.max_attempts <= 1 {
        eprintln!(
            "split: `--max-attempts N` retries halves of its {} components in dependency \
             order, each half on what landed before it; finding every failing component \
             takes at most {budget} attempts",
            nodes.len()
        );
        return Ok(code);
    }
    let mut run = SplitRun {
        used: 1,
        rows: vec![SplitRow {
            attempt: Some(parent.clone()),
            parent: None,
            members: nodes.concat(),
            outcome: "a gate failed; split".into(),
        }],
    };
    let landed = split_nodes(ctx, store, args, &nodes, &parent, &mut run)?;
    println!(
        "landing attempts ({} of at most {}):",
        run.used, args.max_attempts
    );
    for row in &run.rows {
        println!(
            "  {}{}: {}: {}",
            row.attempt.as_deref().unwrap_or("(none)"),
            row.parent
                .as_deref()
                .map(|parent| format!(" from {parent}"))
                .unwrap_or_default(),
            row.members.join(" "),
            row.outcome
        );
    }
    Ok(if landed { 0 } else { code })
}

/// Land `nodes` as a prefix child and then a suffix child of `parent`.
/// Whether every member of `nodes` landed.
fn split_nodes(
    ctx: &Ctx,
    store: &Store,
    args: &LandArgs,
    nodes: &[Node],
    parent: &str,
    run: &mut SplitRun,
) -> Result<bool> {
    let (prefix, suffix) = nodes.split_at(nodes.len().div_ceil(2));
    if !split_child(ctx, store, args, prefix, parent, run)? {
        if !suffix.is_empty() {
            run.rows.push(SplitRow {
                attempt: None,
                parent: Some(parent.to_string()),
                members: suffix.concat(),
                outcome: "blocked: the prefix before it did not land".into(),
            });
        }
        return Ok(false);
    }
    if suffix.is_empty() {
        return Ok(true);
    }
    split_child(ctx, store, args, suffix, parent, run)
}

/// One child attempt of a split, split again when its gates fail and the
/// budget allows. Whether every member of `nodes` landed.
fn split_child(
    ctx: &Ctx,
    store: &Store,
    args: &LandArgs,
    nodes: &[Node],
    parent: &str,
    run: &mut SplitRun,
) -> Result<bool> {
    let members = nodes.concat();
    if run.used >= args.max_attempts {
        run.rows.push(SplitRow {
            attempt: None,
            parent: Some(parent.to_string()),
            members,
            outcome: format!(
                "not attempted: --max-attempts {} is spent",
                args.max_attempts
            ),
        });
        return Ok(false);
    }
    run.used += 1;
    let mut attempted = Attempted::default();
    let code = run_attempt(ctx, store, args, &members, Some(parent), &mut attempted)?;
    let landed = code == 0;
    run.rows.push(SplitRow {
        attempt: attempted.attempt.clone(),
        parent: Some(parent.to_string()),
        members,
        outcome: if landed {
            "landed".into()
        } else if attempted.gate_failed && nodes.len() > 1 {
            "a gate failed; split".into()
        } else if attempted.gate_failed {
            "a gate failed".into()
        } else {
            format!("refused (exit {code})")
        },
    });
    if landed {
        return Ok(true);
    }
    match (&attempted.attempt, attempted.gate_failed && nodes.len() > 1) {
        (Some(attempt), true) => split_nodes(ctx, store, args, nodes, attempt, run),
        _ => Ok(false),
    }
}

/// What one attempt did, beside its exit code.
#[derive(Default)]
struct Attempted {
    attempt: Option<String>,
    /// A required gate failed: the one outcome splitting answers.
    gate_failed: bool,
    plan: Option<Plan>,
}

/// One attempt at landing `changes`, from plan to completion. A split
/// child names the attempt it was split from.
fn run_attempt(
    ctx: &Ctx,
    store: &Store,
    args: &LandArgs,
    changes: &[String],
    parent: Option<&str>,
    report: &mut Attempted,
) -> Result<i32> {
    let started_at = chrono::Utc::now();
    let planned = match plan(
        ctx,
        store,
        changes,
        args.target.as_deref(),
        args.debt.as_deref(),
        None,
    )? {
        Ok(planned) => planned,
        Err(refusal) => {
            eprintln!("Cannot land: {}", refusal.message);
            return Ok(refusal.code);
        }
    };
    report.plan = Some(planned.plan.clone());
    let mut pinned = planned.plan;
    let gate_declarations = planned.gates;
    let member_ids: Vec<String> = pinned
        .members
        .iter()
        .map(|member| member.change_id.clone())
        .collect();
    let observed = observe(ctx, store, &member_ids, &pinned.target_branch)?;

    // Intent, durable before anything moves.
    let attempt = crate::ids::new_identifier();
    report.attempt = Some(attempt.clone());
    let pins: Vec<crate::model::LandingPin> = pinned
        .members
        .iter()
        .map(|member| crate::model::LandingPin {
            change_id: member.change_id.clone(),
            patchset_id: member.patchset_id.clone(),
            base: member.base.clone(),
            head: member.head.clone(),
            brief_digest: member.brief_digest.clone(),
        })
        .collect();
    let mut intent = landing::Intent {
        schema: landing::INTENT_SCHEMA.into(),
        attempt: attempt.clone(),
        phase: landing::Phase::Prepared,
        target_branch: pinned.target_branch.clone(),
        target_before: pinned.target_before.clone(),
        target_after: pinned.target_after.clone(),
        members: pins.clone(),
        composed: Vec::new(),
        parent: parent.map(str::to_string),
        completion: None,
    };
    store.write_landing_intent(&intent)?;
    {
        let _repository_events = store.lock_repository_events()?;
        let mut prepared = ctx.event(
            store,
            Store::REPOSITORY_SCOPE,
            Payload::LandingPrepared {
                attempt: attempt.clone(),
                target_branch: pinned.target_branch.clone(),
                target_before: pinned.target_before.clone(),
                members: pins,
                parent: parent.map(str::to_string),
            },
        )?;
        store.append_local_repository_event(&mut prepared)?;
    }
    println!("landing: {attempt}");
    crash_point("prepared");

    let top = pinned
        .members
        .last()
        .context("a plan has members")?
        .change_id
        .clone();
    // A composed attempt replays first: the head it gates is the one it
    // installs.
    let composed_evidence = if pinned.shape == Shape::Composed {
        if let Err(refusal) = compose(ctx, store, &mut pinned, &mut intent)? {
            abandon(ctx, store, &attempt, &refusal.message)?;
            eprintln!("Cannot land: {}; nothing closed", refusal.message);
            return Ok(refusal.code);
        }
        crash_point("composed");
        // The gates every member requires run once at the composed head, in
        // a checkout the attempt owns, with no ledger or change lock held.
        let gated = gate_composed(ctx, store, &pinned, &gate_declarations, &top, &attempt);
        match gated {
            Ok((0, evidence)) => Some(evidence),
            Ok(_) => {
                report.gate_failed = true;
                abandon(
                    ctx,
                    store,
                    &attempt,
                    "a required gate failed at the composed head",
                )?;
                eprintln!(
                    "Cannot land: a required gate failed at the composed head; nothing closed"
                );
                return Ok(crate::blockers::Blocker::GatesNotGreen.exit_code());
            }
            Err(error) => {
                abandon(
                    ctx,
                    store,
                    &attempt,
                    &format!("gates did not run: {error:#}"),
                )?;
                return Err(error);
            }
        }
    } else {
        // One gate run at the top head, with no ledger or change lock held:
        // a gate command may itself run arc.
        let gated = super::gatekeeping::verify_landing_stack(
            ctx,
            &top,
            super::gatekeeping::VerifyArgs {
                event: Some(crate::model::GateEvent::Integration),
                all: true,
                parallel: false,
                rerun: false,
                gate: None,
                command: None,
                probe: None,
                brief_version: None,
                probe_phase: None,
                attest: false,
                after_integration: false,
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
                bypass: None,
                landing: Some(attempt.clone()),
            },
            &pinned.target_branch,
            &gate_declarations,
        );
        let gate_code = match gated {
            Ok(code) => code,
            Err(error) => {
                abandon(
                    ctx,
                    store,
                    &attempt,
                    &format!("gates did not run: {error:#}"),
                )?;
                return Err(error);
            }
        };
        if gate_code != 0 {
            report.gate_failed = true;
            abandon(
                ctx,
                store,
                &attempt,
                "a required gate failed at the top head",
            )?;
            eprintln!("Cannot land: a required gate failed at the top head; nothing closed");
            return Ok(crate::blockers::Blocker::GatesNotGreen.exit_code());
        }
        None
    };
    crash_point("gated");
    super::pause_until_released("ARC_LAND_PAUSE")?;

    commit(
        ctx,
        store,
        args,
        pinned,
        &observed,
        intent,
        started_at,
        composed_evidence,
    )
}

/// Replay every member the plan marks replayed, in landing order, onto the
/// head the members before it left, and pin the result. Each member's
/// original commits and the commits they landed as are recorded in the
/// intent alone, so nothing outside the attempt resolves a trial commit. A
/// conflict stops the attempt, naming the member and the rebase that gives
/// it a reviewable patchset on the target.
fn compose(
    ctx: &Ctx,
    store: &Store,
    plan: &mut Plan,
    intent: &mut landing::Intent,
) -> Result<std::result::Result<(), Refusal>> {
    let mut tip = plan.target_before.clone();
    let mut composed = Vec::new();
    for member in &plan.members {
        let landed = if member.replayed {
            match gitio::replay_range(&ctx.cwd, &member.base, &member.head, &tip)? {
                gitio::RangeReplay::Replayed { tip } => tip,
                gitio::RangeReplay::Conflict { detail } => {
                    return refuse(
                        crate::blockers::Blocker::NeedsRebase.exit_code(),
                        format!(
                            "{} conflicts when replayed onto {}{}; give it a patchset on \
                             the target with `arc rebase {}`, have it reviewed, and land again",
                            member.change_id,
                            crate::render::short_sha(&tip),
                            if detail.is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", detail.lines().next().unwrap_or_default())
                            },
                            member.change_id
                        ),
                    );
                }
            }
        } else {
            member.head.clone()
        };
        let original = gitio::git(
            &ctx.cwd,
            &[
                "rev-list",
                "--reverse",
                &format!("{}..{}", member.base, member.head),
            ],
        )?;
        let replayed = gitio::git(
            &ctx.cwd,
            &["rev-list", "--reverse", &format!("{tip}..{landed}")],
        )?;
        let original: Vec<&str> = original.lines().collect();
        let replayed: Vec<&str> = replayed.lines().collect();
        if original.len() != replayed.len() {
            return refuse(
                crate::blockers::Blocker::NeedsRebase.exit_code(),
                format!(
                    "{} replayed onto {} as {} commits from {}; give it a patchset on the \
                     target with `arc rebase {}`",
                    member.change_id,
                    crate::render::short_sha(&tip),
                    replayed.len(),
                    original.len(),
                    member.change_id
                ),
            );
        }
        composed.push(landing::ComposedMember {
            change_id: member.change_id.clone(),
            parent: tip.clone(),
            tip: landed.clone(),
            commits: original
                .iter()
                .zip(&replayed)
                .map(|(from, to)| (from.to_string(), to.to_string()))
                .collect(),
        });
        tip = landed;
    }
    gitio::update_ref(&ctx.cwd, &landing::pin_ref(&intent.attempt), &tip)?;
    plan.tree = Some(gitio::commit_tree(&ctx.cwd, &tip)?);
    plan.target_after = Some(tip.clone());
    intent.target_after = Some(tip.clone());
    intent.composed = composed;
    store.write_landing_intent(intent)?;
    println!("composed: {tip}");
    Ok(Ok(()))
}

/// The checkout a composed attempt gates in: a leased slot when the
/// repository declares `[slots]`, otherwise a worktree of its own under the
/// worktrees directory. Either is released or removed when it drops.
enum GateCheckout {
    Slot(crate::slots::HeldSlot, std::path::PathBuf),
    Scratch(super::gatekeeping::ScratchWorktree),
}

impl GateCheckout {
    fn path(&self) -> &std::path::Path {
        match self {
            GateCheckout::Slot(held, _) => &held.path,
            GateCheckout::Scratch(scratch) => &scratch.path,
        }
    }
}

impl Drop for GateCheckout {
    fn drop(&mut self) {
        if let GateCheckout::Slot(held, cwd) = self {
            match crate::slots::release_held(held, cwd) {
                Ok(None) => {}
                Ok(Some(kept)) => eprintln!("warning: {kept}"),
                Err(error) => eprintln!(
                    "warning: {} keeps its lease ({error:#}); once resolved, run `arc slots \
                     release {} --token {}`",
                    held.name, held.name, held.token
                ),
            }
        }
    }
}

fn gate_checkout(ctx: &Ctx, store: &Store, attempt: &str, commit: &str) -> Result<GateCheckout> {
    let toplevel = gitio::toplevel(&ctx.cwd)?;
    if let Some(declaration) = crate::warm_worktree::slot_declaration(&toplevel, true)? {
        let pool = crate::slots::Pool::declared(&ctx.cwd, &declaration)?;
        let branch = format!("landing/{attempt}");
        let leased = crate::slots::lease(
            &ctx.cwd,
            pool,
            &declaration,
            &crate::slots::LeaseRequest {
                change_id: Some(attempt),
                branch: Some(&branch),
                target: commit,
                actor: Some(&ctx.actor),
                session: ctx.session.as_deref(),
            },
        )?;
        let path = crate::config::resolved(&leased.path);
        let states = store.readable_states()?;
        let open_change = states
            .values()
            .filter(|state| state.closure.is_none())
            .find(|state| {
                state.worktree.as_deref().is_some_and(|worktree| {
                    crate::config::resolved(std::path::Path::new(worktree)) == path
                })
            })
            .map(|state| state.change_id.clone());
        let held = leased.hold_detached(&ctx.cwd, &declaration, commit, open_change.as_deref())?;
        println!("slot: {} at {}", held.name, held.path.display());
        return Ok(GateCheckout::Slot(held, ctx.cwd.clone()));
    }
    let path = super::lifecycle::worktree_path_for(&ctx.cwd, &format!("landing-{attempt}"))?;
    Ok(GateCheckout::Scratch(
        super::gatekeeping::ScratchWorktree::create(&ctx.cwd, path, commit)?,
    ))
}

/// Run the gates every member requires, once, at the composed head,
/// recorded on the top member.
fn gate_composed(
    ctx: &Ctx,
    store: &Store,
    plan: &Plan,
    gates: &[(String, crate::gates::Gate)],
    top: &str,
    attempt: &str,
) -> Result<(i32, BTreeMap<String, String>)> {
    let commit = plan.after().to_string();
    let tree = plan.installed_tree().to_string();
    let checkout = gate_checkout(ctx, store, attempt, &commit)?;
    let mut state = store.state(top)?;
    state.target_branch = plan.target_branch.clone();
    super::gatekeeping::verify_landing_head(
        ctx,
        store,
        super::gatekeeping::LandingGateRun {
            change_id: top,
            state: &state,
            gates,
            commit: &commit,
            tree: &tree,
            checkout: checkout.path(),
            attempt,
        },
    )
}

/// Revalidate under the commit guards, authorize, move the target, and
/// publish the completion.
#[allow(clippy::too_many_arguments)]
fn commit(
    ctx: &Ctx,
    store: &Store,
    args: &LandArgs,
    pinned: Plan,
    observed: &serde_json::Value,
    mut intent: landing::Intent,
    started_at: chrono::DateTime<chrono::Utc>,
    composed_evidence: Option<BTreeMap<String, String>>,
) -> Result<i32> {
    let attempt = intent.attempt.clone();
    let _authority = crate::replica::lock(store)?;
    if let Some(refusal) = crate::replica::integration_refusal(store)? {
        abandon(
            ctx,
            store,
            &attempt,
            &format!("integration authority: {refusal}"),
        )?;
        eprintln!("Cannot land: {refusal}");
        return Ok(crate::replica::INTEGRATION_AUTHORITY_EXIT_CODE);
    }
    let _target = store.lock_target(&pinned.target_branch)?;
    let _graph = store.lock_graph()?;
    let mut sorted: Vec<&String> = pinned
        .members
        .iter()
        .map(|member| &member.change_id)
        .collect();
    sorted.sort();
    let mut _transitions = Vec::new();
    for change_id in sorted {
        _transitions.push(store.lock_transition(change_id)?);
    }
    let _repository_events = store.lock_repository_events()?;

    let references: Vec<String> = pinned
        .members
        .iter()
        .map(|member| member.change_id.clone())
        .collect();
    let now = observe(ctx, store, &references, &pinned.target_branch)?;
    if let Some(field) = moved_observation(observed, &now) {
        abandon_locked(ctx, store, &attempt, &format!("stale-{field}"))?;
        bail!("{field} moved while landing {attempt} was gated; nothing closed, land again");
    }
    let mut fresh = match plan(
        ctx,
        store,
        &references,
        Some(&pinned.target_branch),
        args.debt.as_deref(),
        Some(&attempt),
    )? {
        Ok(fresh) => fresh,
        Err(refusal) => {
            abandon_locked(ctx, store, &attempt, &format!("stale: {}", refusal.message))?;
            eprintln!("Cannot land: {}; nothing closed", refusal.message);
            return Ok(refusal.code);
        }
    };
    // A plan does not replay; the head this attempt composed is its own.
    if fresh.plan.shape == Shape::Composed && pinned.shape == Shape::Composed {
        fresh.plan.target_after = pinned.target_after.clone();
        fresh.plan.tree = pinned.tree.clone();
    }
    if let Some(field) = moved_field(&pinned, &fresh.plan)? {
        abandon_locked(ctx, store, &attempt, &format!("stale-{field}"))?;
        bail!("{field} moved while landing {attempt} was gated; nothing closed, land again");
    }
    #[cfg(debug_assertions)]
    crate::import_authorization::test_rendezvous("landing-revalidated")?;
    let plan = fresh.plan;
    let top_index = plan.members.len() - 1;
    let top_report = &fresh.reports[top_index];
    let mut gate_evidence = composed_evidence.clone().unwrap_or_default();
    for gate in top_report
        .gates
        .iter()
        .filter(|_| composed_evidence.is_none())
    {
        if !plan.gates.contains_key(&gate.name) {
            continue;
        }
        match (&gate.evidence_event_id, gate.green_at_head) {
            (Some(evidence), true) => {
                gate_evidence.insert(gate.name.clone(), evidence.clone());
            }
            _ => {
                abandon_locked(
                    ctx,
                    store,
                    &attempt,
                    "a required gate is not green at the top head",
                )?;
                eprintln!(
                    "Cannot land: gate {} is not green at the top head; nothing closed",
                    gate.name
                );
                return Ok(crate::blockers::Blocker::GatesNotGreen.exit_code());
            }
        }
    }
    if let Some(missing) = plan
        .gates
        .keys()
        .find(|name| !gate_evidence.contains_key(*name))
    {
        abandon_locked(
            ctx,
            store,
            &attempt,
            "a required gate has no evidence at the top head",
        )?;
        eprintln!("Cannot land: gate {missing} has no evidence at the top head; nothing closed");
        return Ok(crate::blockers::Blocker::GatesNotGreen.exit_code());
    }

    // The checkout the target is installed in, when one holds it.
    let checkout = gitio::worktree_for_branch(&ctx.cwd, &plan.target_branch)?;
    if let Some(path) = &checkout {
        let checkout = crate::integration::TargetCheckout {
            path: path.clone(),
            switch_from: None,
        };
        let refusal = (|| -> Result<()> {
            if gitio::head(path)? != plan.target_before {
                bail!("{} is not at {}", path.display(), plan.target_before);
            }
            super::gatekeeping::checkout_tracked_dirt(path)?;
            super::gatekeeping::checkout_writes(
                &checkout,
                &plan.target_before,
                Some(plan.installed_tree()),
            )
        })();
        if let Err(error) = refusal {
            abandon_locked(ctx, store, &attempt, &format!("target checkout: {error:#}"))?;
            return Err(error);
        }
    }
    for state in &fresh.states {
        if let Some(worktree) = state.worktree.as_deref() {
            crate::journal::promote_worktree_spool(ctx, std::path::Path::new(worktree));
        }
    }

    // Authorize: the exact completion and its basis are saved before the
    // target moves.
    // Planned against the change as its ledger holds it, not the projection
    // that already reads the planned declaration.
    let debts: Vec<Option<super::audit::DebtPlan>> = plan
        .members
        .iter()
        .map(|member| {
            member
                .planned_debt
                .as_ref()
                .map(|debt| {
                    let mut recorded = store.state(&member.change_id)?;
                    recorded.target_branch = plan.target_branch.clone();
                    match (debt.kind, &member.replayed_approval) {
                        (crate::model::DebtMissing::ReplayedUnread, Some(approval)) => {
                            let landed = intent
                                .composed
                                .iter()
                                .find(|composed| composed.change_id == member.change_id)
                                .context("a replayed member has a composed record")?;
                            super::audit::plan_replayed_debt(
                                ctx,
                                store,
                                &recorded,
                                &debt.reason,
                                crate::model::ReplayedRead {
                                    approval: approval.clone(),
                                    base: member.base.clone(),
                                    parent: landed.parent.clone(),
                                },
                            )
                        }
                        _ => super::audit::plan_debt(
                            ctx,
                            store,
                            &recorded,
                            &debt.reason,
                            None,
                            &super::audit::IneligibleInputs::default(),
                        ),
                    }
                })
                .transpose()
                .map(|plan| plan.filter(|plan| plan.restated_event_id().is_none()))
        })
        .collect::<Result<_>>()?;
    let reserved = debts.iter().filter(|debt| debt.is_some()).count() + plan.members.len() + 1;
    let lander = ctx
        .event(
            store,
            Store::REPOSITORY_SCOPE,
            Payload::LandingAbandoned {
                attempt: attempt.clone(),
                reason: String::new(),
            },
        )?
        .into_event();
    // Each member's own landed range: where its first commit was placed and
    // its last landed commit.
    let landed_range = |index: usize| -> (String, String) {
        let member = &plan.members[index];
        intent
            .composed
            .iter()
            .find(|composed| composed.change_id == member.change_id)
            .map(|composed| (composed.parent.clone(), composed.tip.clone()))
            .unwrap_or_else(|| (member.base.clone(), member.head.clone()))
    };
    let built = |ids: Vec<String>| -> Result<landing::Intent> {
        let mut ids = ids.into_iter();
        let mut debt_ids = Vec::new();
        for debt in &debts {
            debt_ids.push(debt.as_ref().map(|_| ids.next().expect("reserved")));
        }
        let closure_ids: Vec<String> = plan
            .members
            .iter()
            .map(|_| ids.next().expect("reserved"))
            .collect();
        let completion_id = ids.next().expect("reserved");
        let mut landed = Vec::new();
        for (index, member) in plan.members.iter().enumerate() {
            let state = &fresh.states[index];
            let report = &fresh.reports[index];
            let approval = crate::integration::approval(state, report, &member.patchset_id);
            let mut prerequisites = super::gatekeeping::prerequisite_closures(store, state)?;
            for prerequisite in &member.prerequisites {
                if prerequisite.satisfied_by != "landing" {
                    continue;
                }
                let position = plan
                    .members
                    .iter()
                    .position(|other| other.change_id == prerequisite.change_id)
                    .context("a landing prerequisite is a member")?;
                prerequisites.push(crate::model::PrerequisiteClosure {
                    change_id: prerequisite.change_id.clone(),
                    closure_event_id: closure_ids[position].clone(),
                    integrated_commit: Some(landed_range(position).1),
                    landing: Some(attempt.clone()),
                });
            }
            let selected_verdict_provenance = approval
                .verdict_event_id
                .as_ref()
                .and_then(|id| {
                    state
                        .verdicts
                        .iter()
                        .find(|verdict| &verdict.event_id == id)
                })
                .map(
                    |verdict| crate::import_authorization::SelectedVerdictProvenance {
                        origin_replica_id: verdict.origin_replica_id.clone(),
                        admission_provenance: state
                            .privilege_scope
                            .as_ref()
                            .map(|scope| scope.provenance(&verdict.event_id))
                            .unwrap_or("legacy-unknown")
                            .into(),
                        receipt: state
                            .privilege_scope
                            .as_ref()
                            .and_then(|scope| scope.admission_for(&verdict.event_id))
                            .cloned(),
                    },
                );
            let debt = match (&debts[index], &debt_ids[index]) {
                (Some(plan_debt), Some(event_id)) => match plan_debt.payload() {
                    Payload::DebtDeclared {
                        reason,
                        patchset_id,
                        missing,
                        coverage,
                        production,
                        ineligible,
                        replayed,
                        ..
                    } => Some(crate::model::LandedDebt {
                        event_id: event_id.clone(),
                        reason: reason.clone(),
                        patchset_id: patchset_id
                            .clone()
                            .unwrap_or_else(|| member.patchset_id.clone()),
                        missing: *missing,
                        coverage: coverage.clone(),
                        production: production.clone(),
                        ineligible: ineligible.clone(),
                        replayed: replayed.clone(),
                    }),
                    _ => None,
                },
                _ => None,
            };
            let declarations = fresh.declarations[index].clone();
            let (range_base, integrated_commit) = landed_range(index);
            landed.push(crate::model::LandedMember {
                change_id: member.change_id.clone(),
                closure_event_id: closure_ids[index].clone(),
                source_patchset_id: member.patchset_id.clone(),
                source_head: member.head.clone(),
                range_base,
                integrated_commit,
                authorization: crate::model::AuthorizationBasis {
                    selected_verdict_provenance,
                    verdict_event_id: approval.verdict_event_id,
                    external_verdict: approval.external_verdict,
                    verdict_provisional: approval.verdict_provisional,
                    gate_evidence: member
                        .gates
                        .keys()
                        .filter_map(|name| {
                            gate_evidence
                                .get(name)
                                .map(|evidence| (name.clone(), evidence.clone()))
                        })
                        .collect(),
                    prerequisites,
                    provisional_prerequisites: Vec::new(),
                    blocking_findings: report.open_blocking_findings.clone(),
                    holds: report
                        .holds
                        .iter()
                        .map(|hold| hold.hold_event_id.clone())
                        .collect(),
                    gates: declarations.gates,
                    policy: declarations.policy,
                    danger: Some(report.danger.clone()),
                    audit_debt_event_id: debt
                        .as_ref()
                        .map(|debt| debt.event_id.clone())
                        .or(approval.audit_debt_event_id),
                },
                debt,
            });
        }
        let mut completion = lander.clone();
        completion.event_id = completion_id;
        completion.created_at = chrono::Utc::now();
        completion.payload = Payload::LandingCompleted {
            attempt: attempt.clone(),
            target_branch: plan.target_branch.clone(),
            target_before: plan.target_before.clone(),
            target_after: plan.after().to_string(),
            tree: plan.installed_tree().to_string(),
            gate_evidence: gate_evidence.clone(),
            members: landed,
            started_at: Some(started_at),
        };
        let mut authorized = intent.clone();
        authorized.phase = landing::Phase::Authorized;
        authorized.completion = Some(completion);
        Ok(authorized)
    };
    intent = store.reserve_landing_ids(reserved, built)?;
    crash_point("authorized");

    // Apply: the target and the applied ref move in one transaction.
    let target_ref = format!("refs/heads/{}", plan.target_branch);
    let applied = landing::applied_ref(&attempt);
    if let Err(error) = gitio::update_refs(
        &ctx.cwd,
        &[
            gitio::RefUpdate {
                name: target_ref.clone(),
                old: plan.target_before.clone(),
                new: plan.after().to_string(),
            },
            gitio::RefUpdate {
                name: applied.clone(),
                old: gitio::absent_oid(plan.after()),
                new: plan.after().to_string(),
            },
        ],
    ) {
        abandon_locked(
            ctx,
            store,
            &attempt,
            &format!("target did not move: {error:#}"),
        )?;
        return Err(error.context("the target did not move; nothing closed"));
    }
    crash_point("moved");
    if let Err(error) = reconcile_checkout(
        &ctx.cwd,
        checkout.as_deref(),
        &plan.target_branch,
        &plan.target_before,
        plan.after(),
    ) {
        let rollback = gitio::update_refs(
            &ctx.cwd,
            &[
                gitio::RefUpdate {
                    name: target_ref,
                    old: plan.after().to_string(),
                    new: plan.target_before.clone(),
                },
                gitio::RefUpdate {
                    name: applied,
                    old: plan.after().to_string(),
                    new: gitio::absent_oid(plan.after()),
                },
            ],
        );
        return match rollback {
            Ok(()) => {
                abandon_locked(
                    ctx,
                    store,
                    &attempt,
                    &format!("checkout update failed: {error:#}"),
                )?;
                Err(error.context(
                    "the target checkout could not be updated; the target was \
                                   restored and nothing closed",
                ))
            }
            Err(rollback) => Err(error.context(format!(
                "the target checkout could not be updated and restoring the target failed \
                 ({rollback:#}); run `arc land --recover {attempt}`"
            ))),
        };
    }
    intent.phase = landing::Phase::Applied;
    store.write_landing_intent(&intent)?;
    crash_point("applied");
    publish(store, &mut intent)?;
    drop(_repository_events);
    drop(_transitions);
    drop(_graph);
    drop(_target);
    finish(ctx, store, &intent)?;
    Ok(0)
}

/// Bring the target checkout to `after`, where it still holds `before`, and
/// confirm it. A checkout that holds neither is left alone and refused.
fn reconcile_checkout(
    cwd: &std::path::Path,
    checkout: Option<&std::path::Path>,
    target: &str,
    before: &str,
    after: &str,
) -> Result<()> {
    let Some(path) = checkout else {
        return Ok(());
    };
    if gitio::current_branch(path)?.as_deref() != Some(target) {
        bail!("{} no longer holds {target}", path.display());
    }
    // Stat information alone must not read as a modified file.
    let _ = gitio::git(path, &["update-index", "-q", "--refresh"]);
    let index = gitio::git(path, &["write-tree"])?;
    let before_tree = gitio::commit_tree(cwd, before)?;
    let after_tree = gitio::commit_tree(cwd, after)?;
    let worktree_matches_index = gitio::git(path, &["diff-files", "--quiet"]).is_ok();
    if index == before_tree && worktree_matches_index {
        gitio::git(path, &["read-tree", "-u", "-m", before, after])?;
    } else if !(index == after_tree && worktree_matches_index) {
        bail!(
            "{} matches neither {} nor {}; reconcile it by hand",
            path.display(),
            crate::render::short_sha(before),
            crate::render::short_sha(after)
        );
    }
    if gitio::head(path)? != after
        || gitio::git(path, &["write-tree"])? != after_tree
        || gitio::dirt(path)?.tracked
    {
        bail!("{} did not come to {after}", path.display());
    }
    Ok(())
}

/// Append the saved completion and record that it is published.
fn publish(store: &Store, intent: &mut landing::Intent) -> Result<()> {
    let completion = intent
        .completion
        .as_ref()
        .context("an applied landing intent holds its completion")?;
    store.publish_reserved_repository_event(completion)?;
    intent.phase = landing::Phase::Published;
    store.write_landing_intent(intent)
}

/// Cleanup after publication: the applied ref, the intent, and each member's
/// retention pins.
fn finish(ctx: &Ctx, store: &Store, intent: &landing::Intent) -> Result<()> {
    let completion = intent
        .completion
        .as_ref()
        .context("a published landing intent holds its completion")?;
    println!(
        "landed: {} into {} ({} members)",
        crate::render::short_sha(intent.target_after.as_deref().unwrap_or_default()),
        intent.target_branch,
        intent.members.len()
    );
    println!("event: {}", completion.event_id);
    for member in &intent.members {
        if let Err(error) =
            super::gatekeeping::release_retention_refs(ctx, &member.change_id, Some(&member.head))
        {
            eprintln!(
                "warning: {} landed; retention cleanup failed: {error:#}",
                member.change_id
            );
        }
    }
    let applied = landing::applied_ref(&intent.attempt);
    if gitio::ref_value(&ctx.cwd, &applied)?.is_some() {
        gitio::delete_ref(&ctx.cwd, &applied)?;
    }
    release_pin(&ctx.cwd, &intent.attempt)?;
    store.clear_landing_intent(&intent.attempt)
}

/// Drop the pin on a composed attempt's head: the target holds it once the
/// attempt completed, and nothing should once it was abandoned.
fn release_pin(cwd: &std::path::Path, attempt: &str) -> Result<()> {
    let pin = landing::pin_ref(attempt);
    if gitio::ref_value(cwd, &pin)?.is_some() {
        gitio::delete_ref(cwd, &pin)?;
    }
    Ok(())
}

/// Record that the attempt closes nothing. The caller holds no
/// repository-events lock.
fn abandon(ctx: &Ctx, store: &Store, attempt: &str, reason: &str) -> Result<()> {
    let _repository_events = store.lock_repository_events()?;
    abandon_locked(ctx, store, attempt, reason)
}

/// `abandon`, for a caller holding the repository-events lock.
fn abandon_locked(ctx: &Ctx, store: &Store, attempt: &str, reason: &str) -> Result<()> {
    let landings = Landings::from_events(store.repository_events()?.iter());
    if landings.completion(attempt).is_none() && landings.abandonment(attempt).is_none() {
        let mut event = ctx.event(
            store,
            Store::REPOSITORY_SCOPE,
            Payload::LandingAbandoned {
                attempt: attempt.to_string(),
                reason: reason.to_string(),
            },
        )?;
        store.append_local_repository_event(&mut event)?;
        println!("landing {attempt} abandoned: {reason}");
    }
    release_pin(&ctx.cwd, attempt)?;
    store.clear_landing_intent(attempt)
}

/// The attempt's target and members, from its intent or its prepared event.
fn attempt_facts(store: &Store, attempt: &str) -> Result<(String, String, String, Vec<String>)> {
    if let Some(intent) = store.landing_intent(attempt)? {
        return Ok((
            intent.target_branch,
            intent.target_before,
            intent.target_after.unwrap_or_default(),
            intent
                .members
                .into_iter()
                .map(|member| member.change_id)
                .collect(),
        ));
    }
    let landings = Landings::from_events(store.repository_events()?.iter());
    match landings.prepared(attempt).map(|event| &event.payload) {
        Some(Payload::LandingPrepared {
            target_branch,
            target_before,
            members,
            ..
        }) => Ok((
            target_branch.clone(),
            target_before.clone(),
            members
                .last()
                .map(|member| member.head.clone())
                .unwrap_or_default(),
            members
                .iter()
                .map(|member| member.change_id.clone())
                .collect(),
        )),
        _ => bail!("no landing attempt {attempt} is recorded here"),
    }
}

/// Take the commit guards an attempt's recovery or abandonment needs.
fn attempt_locks(
    store: &Store,
    target: &str,
    members: &[String],
) -> Result<Vec<crate::store::TransitionLock>> {
    let mut locks = vec![store.lock_target(target)?, store.lock_graph()?];
    let mut sorted: Vec<&String> = members.iter().collect();
    sorted.sort();
    for member in sorted {
        locks.push(store.lock_transition(member)?);
    }
    locks.push(store.lock_repository_events()?);
    Ok(locks)
}

/// `arc land --recover <attempt>`: publish a saved, authorized completion
/// whose applied ref exists, abandon an attempt that never moved the target,
/// and report anything else without closing anything. A finished attempt
/// frees the slot it leased to gate in.
fn recover(ctx: &Ctx, attempt: &str) -> Result<i32> {
    let code = finish_attempt(ctx, attempt)?;
    if code == 0 {
        super::slots::release_landing_slots(ctx, attempt);
    }
    Ok(code)
}

fn finish_attempt(ctx: &Ctx, attempt: &str) -> Result<i32> {
    let store = ctx.store()?;
    ctx.ensure_declared_actor(&store)?;
    let _authority = crate::replica::lock(&store)?;
    let (target, before, after, members) = attempt_facts(&store, attempt)?;
    let locks = attempt_locks(&store, &target, &members)?;
    let landings = Landings::from_events(store.repository_events()?.iter());
    if let Some(completion) = landings.completion(attempt) {
        println!(
            "landing {attempt} is complete: event {}",
            completion.event_id
        );
        let applied = landing::applied_ref(attempt);
        if gitio::ref_value(&ctx.cwd, &applied)?.is_some() {
            gitio::delete_ref(&ctx.cwd, &applied)?;
        }
        release_pin(&ctx.cwd, attempt)?;
        store.clear_landing_intent(attempt)?;
        return Ok(0);
    }
    if let Some(abandoned) = landings.abandonment(attempt) {
        if let Payload::LandingAbandoned { reason, .. } = &abandoned.payload {
            println!("landing {attempt} was abandoned: {reason}");
        }
        release_pin(&ctx.cwd, attempt)?;
        store.clear_landing_intent(attempt)?;
        return Ok(0);
    }
    let intent = store.landing_intent(attempt)?;
    let applied = gitio::ref_value(&ctx.cwd, &landing::applied_ref(attempt))?;
    let target_now = gitio::branch_head(&ctx.cwd, &target).ok();
    let authorized = intent
        .as_ref()
        .filter(|intent| intent.phase >= landing::Phase::Authorized && intent.completion.is_some());
    match (authorized, applied.as_deref(), target_now.as_deref()) {
        (Some(intent), Some(applied), _) if Some(applied) == intent.target_after.as_deref() => {
            let mut intent = intent.clone();
            let checkout = gitio::worktree_for_branch(&ctx.cwd, &target)?;
            if let Err(error) =
                reconcile_checkout(&ctx.cwd, checkout.as_deref(), &target, &before, &after)
            {
                eprintln!(
                    "Cannot recover landing {attempt}: {error:#}; it stays pending until the \
                     checkout holds either side"
                );
                return Ok(1);
            }
            if intent.phase < landing::Phase::Applied {
                intent.phase = landing::Phase::Applied;
                store.write_landing_intent(&intent)?;
            }
            publish(&store, &mut intent)?;
            drop(locks);
            finish(ctx, &store, &intent)?;
            Ok(0)
        }
        (_, None, Some(now)) if now == before => {
            abandon_locked(ctx, &store, attempt, "recovered: the target never moved")?;
            Ok(0)
        }
        _ => {
            eprintln!(
                "Cannot recover landing {attempt}: {target} is at {}, the attempt started at {} \
                 and {}; arc did not authorize that movement, so nothing closes. Inspect the \
                 target, then `arc land --abandon {attempt} --reason <why>`",
                target_now
                    .as_deref()
                    .map(crate::render::short_sha)
                    .unwrap_or_else(|| "nothing".into()),
                crate::render::short_sha(&before),
                if applied.is_some() {
                    "its applied ref has no authorized completion".to_string()
                } else {
                    "its applied ref is absent".to_string()
                }
            );
            Ok(1)
        }
    }
}

/// `arc land --abandon <attempt> --reason <why>`.
fn abandon_command(ctx: &Ctx, attempt: &str, reason: &str) -> Result<i32> {
    let store = ctx.store()?;
    ctx.ensure_declared_actor(&store)?;
    let _authority = crate::replica::lock(&store)?;
    let (target, _, _, members) = attempt_facts(&store, attempt)?;
    let locks = attempt_locks(&store, &target, &members)?;
    let landings = Landings::from_events(store.repository_events()?.iter());
    if landings.completion(attempt).is_some() {
        bail!("landing {attempt} completed; there is nothing to abandon");
    }
    if gitio::ref_value(&ctx.cwd, &landing::applied_ref(attempt))?.is_some() {
        bail!("landing {attempt} moved its target; finish it with `arc land --recover {attempt}`");
    }
    abandon_locked(ctx, &store, attempt, reason)?;
    drop(locks);
    super::slots::release_landing_slots(ctx, attempt);
    Ok(0)
}
