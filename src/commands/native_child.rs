//! Positive identification of a native child a binding names: a session one
//! harness session spawned inside its own harness, such as a Claude subagent
//! or a Codex `thread_spawn` thread.
//!
//! A Claude subagent has no session of its own. Its recording sits under its
//! parent's, and tapes names it `<parent>::<agent>`. Any other harness's child
//! is an ordinary session whose own recording names the parent. Either way the
//! pinned tapes reader must find exactly one recording that names both, or the
//! binding is refused with a fixed reason.

use crate::model::{native_session_key, MIRROR_AGENT_TYPE};

/// What a positively identified child is.
pub(crate) enum NativeChild {
    /// A child with consumption of its own, measured from its own recording.
    Measured,
    /// A delegated-agents mirror row: it carries no consumption, and the
    /// agent-delegate run it mirrors names this handle when its brief does.
    Mirror(Option<String>),
}

/// The session key a cost read measures a bound child under: `<parent>::<agent>`
/// for a Claude subagent, and the child's own session id elsewhere.
pub(crate) fn child_key(harness: &str, parent: &str, child: &str) -> String {
    if harness == "claude" {
        format!("{parent}::{child}")
    } else {
        child.to_string()
    }
}

fn backends(harness: &str) -> Vec<Box<dyn agent_tapes_core::backend::Backend>> {
    agent_tapes_core::backend::backends()
        .into_iter()
        .filter(|backend| backend.harness() == harness)
        .collect()
}

/// Identify `child` as `parent`'s native child in `harness`, or say why the
/// pinned reader cannot. Reasons are fixed and never echo a value.
pub(crate) fn identify(
    harness: &str,
    parent: &str,
    child: &str,
) -> std::result::Result<NativeChild, &'static str> {
    let backends = backends(harness);
    if backends.is_empty() {
        return Err("the pinned reader has no session store for this harness");
    }
    if harness == "claude" {
        claude_child(&backends, parent, child)
    } else {
        spawned_child(&backends, harness, parent, child)
    }
}

fn claude_child(
    backends: &[Box<dyn agent_tapes_core::backend::Backend>],
    parent: &str,
    child: &str,
) -> std::result::Result<NativeChild, &'static str> {
    let resolved = agent_tapes_core::resolve_session(backends, parent)
        .ok()
        .filter(|resolved| resolved.session.id == parent)
        .ok_or("the parent session does not resolve to exactly one recording")?;
    let lineage = agent_tapes_core::lineage_with_backends(
        backends,
        agent_tapes_core::Selection::IdInHarness {
            id: parent,
            harness: "claude",
        },
    )
    .map_err(|_| "the parent session's children cannot be read")?;
    let named: Vec<_> = lineage
        .lineage
        .children
        .iter()
        .filter(|relative| relative.reference == child)
        .collect();
    let relative = match named.as_slice() {
        [] => return Err("the parent session records no child with this agent id"),
        [one] => *one,
        _ => return Err("the parent session records this agent id more than once"),
    };
    if !relative.resolved {
        return Err("the child's recording is not in the store");
    }
    // The child's own records must name this parent and this agent alone.
    let backend = &backends[resolved.backend_index];
    let mut brief = None;
    backend
        .stream_child_transcript(&resolved.session, child, &mut |turn| {
            if brief.is_none() && turn.role == agent_tapes_core::model::Role::User {
                brief = Some(turn.text);
            }
            Ok(())
        })
        .map_err(|_| "the child's recording does not name this parent and agent")?;
    if relative.role.as_deref() == Some(MIRROR_AGENT_TYPE) {
        return Ok(NativeChild::Mirror(
            brief.as_deref().and_then(mirrored_handle),
        ));
    }
    Ok(NativeChild::Measured)
}

/// The agent-delegate handle a mirror row's brief names: the plugin writes a
/// heading line, a blank line, and a JSON object carrying `handle`.
fn mirrored_handle(brief: &str) -> Option<String> {
    let (_, body) = brief.split_once("\n\n")?;
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    value["handle"]
        .as_str()
        .filter(|handle| native_session_key(handle))
        .map(str::to_string)
}

fn spawned_child(
    backends: &[Box<dyn agent_tapes_core::backend::Backend>],
    harness: &str,
    parent: &str,
    child: &str,
) -> std::result::Result<NativeChild, &'static str> {
    agent_tapes_core::resolve_session(backends, child)
        .ok()
        .filter(|resolved| resolved.session.id == child)
        .ok_or("the child session does not resolve to exactly one recording")?;
    let lineage = agent_tapes_core::lineage_with_backends(
        backends,
        agent_tapes_core::Selection::IdInHarness { id: child, harness },
    )
    .map_err(|_| "the child session's lineage cannot be read")?;
    match lineage.lineage.parent {
        Some(named) if named.native_id == parent => Ok(NativeChild::Measured),
        Some(_) => Err("the child's recording names another parent"),
        None => Err("the child's recording names no parent"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mirror_brief_names_its_handle() {
        let brief = "Mirror delegated run lane · model#low · change-abc\n\n{\"handle\":\"lane-20260101T000000Z-ab12\",\"title\":\"lane\"}";
        assert_eq!(
            mirrored_handle(brief).as_deref(),
            Some("lane-20260101T000000Z-ab12")
        );
        assert_eq!(mirrored_handle("no body"), None);
        assert_eq!(mirrored_handle("heading\n\n{\"handle\":\"/a/path\"}"), None);
    }
}
