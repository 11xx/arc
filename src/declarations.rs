//! The gate and policy declarations a change is judged by.
//!
//! Integration answers to the branch it merges into. A change that edits
//! `.arc/gates.toml` or `.arc/policy.toml` is therefore judged by the target's
//! declarations, read at the target's current head, wherever the command is
//! typed. The gates the change's own tree adds are owed on top; the operator's
//! policy file lives outside every tree and applies in both readings.

use crate::gates::{self, GatesFile};
use crate::gitio;
use crate::policy::{self, PolicyFile};
use crate::state::ChangeState;
use anyhow::Result;
use std::fs;
use std::path::Path;

/// What readiness, verification and integration require of one change.
#[derive(Debug)]
pub struct Declarations {
    pub gates: GatesFile,
    pub policy: PolicyFile,
    /// One line per way the declarations in play disagree, so a refusal never
    /// rests on a declaration nobody was told about.
    pub notes: Vec<String>,
}

/// The declarations `state` is judged by, from any checkout of its repository.
///
/// Required gates are the target's plus those the change's branch head adds;
/// policy is the target's. A repository whose target branch cannot be read
/// has no other tree to answer to, so the declarations come from the checkout
/// at `cwd`.
pub fn for_change(cwd: &Path, state: &ChangeState) -> Result<Declarations> {
    let toplevel = gitio::toplevel(cwd)?;
    let Ok(target_head) = gitio::branch_head(cwd, &state.target_branch) else {
        return Ok(Declarations {
            gates: gates::load(&toplevel)?,
            policy: policy::load(&toplevel)?,
            notes: Vec::new(),
        });
    };
    let target = &state.target_branch;
    let mut required = gates::load_at(cwd, &target_head)?;
    let mut notes = Vec::new();
    if let Ok(change_head) = gitio::branch_head(cwd, &state.branch) {
        let own = gates::inspect_at(cwd, &change_head)?;
        let divergences = required.owe_also(
            own,
            &format!("{target}'s .arc/gates.toml"),
            &format!("{}'s .arc/gates.toml", state.branch),
        );
        notes.extend(divergences.iter().map(gates::GateDivergence::describe));
    }
    notes.extend(checkout_notes(cwd, &toplevel, target, &target_head)?);
    Ok(Declarations {
        gates: required,
        policy: policy::load_at(cwd, &target_head)?,
        notes,
    })
}

/// Where the checkout at `toplevel` declares something the target does not,
/// naming the reading readiness takes instead.
fn checkout_notes(
    cwd: &Path,
    toplevel: &Path,
    target: &str,
    target_head: &str,
) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for file in [".arc/gates.toml", ".arc/policy.toml"] {
        let here = match fs::read_to_string(toplevel.join(file)) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if here != gitio::file_at(cwd, target_head, file)? {
            notes.push(format!(
                "this checkout's {file} differs from {target}'s; readiness answers for {target}'s"
            ));
        }
    }
    if let (Ok(here), Ok(there)) = (
        gates::inspect(toplevel),
        gates::inspect_at(cwd, target_head),
    ) {
        for (name, gate) in &here.gates {
            match there.gates.get(name) {
                None => notes.push(format!(
                    "gate {name:?} is declared in this checkout and not by {target}"
                )),
                Some(other) if gate.command != other.command => notes.push(format!(
                    "gate {name:?} runs {:?} in this checkout and {:?} on {target}; {target}'s is evaluated",
                    gate.command, other.command
                )),
                Some(_) => {}
            }
        }
        for name in there
            .gates
            .keys()
            .filter(|name| !here.gates.contains_key(*name))
        {
            notes.push(format!(
                "gate {name:?} is declared by {target} and absent from this checkout; it is still required"
            ));
        }
    }
    Ok(notes)
}
