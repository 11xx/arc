//! Must-land-together relations, and `arc land`: several changes installed on
//! their target by one guarded transition.

use crate::landing::{self, Landings};
use crate::model::{Closure, Payload};
use crate::state::ChangeState;
use crate::store::Store;
use anyhow::{bail, Result};
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
