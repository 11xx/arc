use anyhow::{bail, Result};

/// Event ids sort in the order this process issued them, so a command that
/// records several dependent events (an opening and the facts asserted with
/// it) replays them in that order even within one millisecond. Ids issued
/// after a recorded event also continue this one sequence.
static LAST_EVENT_ID: std::sync::Mutex<Option<ulid::Ulid>> = std::sync::Mutex::new(None);

/// The largest timestamp a ULID's 48-bit time part holds.
const MAX_TIMESTAMP_MS: u64 = (1 << 48) - 1;

/// An ordered event identity. Exhaustion leaves the sequence unchanged.
pub fn new_event_id() -> Result<String> {
    next_event_id(None).map(|id| id.to_string())
}

/// A random ULID for identifiers and temporary names outside the event sequence.
pub fn new_identifier() -> String {
    ulid::Ulid::new().to_string()
}

/// An event id later than `previous` and than every id this process issued.
/// Refuses when no later id exists.
pub fn new_event_id_after(previous: ulid::Ulid) -> Result<ulid::Ulid> {
    next_event_id(Some(previous))
}

/// The smallest id after `floor` that is at least `candidate`. An exhausted
/// random part moves to the next millisecond, which still sorts after the
/// floor. None when `floor` is the last id of the last millisecond.
pub(crate) fn after_floor(candidate: ulid::Ulid, floor: ulid::Ulid) -> Option<ulid::Ulid> {
    if candidate > floor {
        return Some(candidate);
    }
    floor.increment().or_else(|| {
        let next = floor.timestamp_ms() + 1;
        (next <= MAX_TIMESTAMP_MS).then(|| ulid::Ulid::from_parts(next, 0))
    })
}

fn next_event_id(floor: Option<ulid::Ulid>) -> Result<ulid::Ulid> {
    let mut last = LAST_EVENT_ID
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let floor = match (*last, floor) {
        (Some(last), Some(floor)) => Some(last.max(floor)),
        (last, floor) => last.or(floor),
    };
    let candidate = ulid::Ulid::new();
    let id = match floor {
        Some(floor) => after_floor(candidate, floor)
            .ok_or_else(|| anyhow::anyhow!("no event id sorts after {floor}"))?,
        None => candidate,
    };
    *last = Some(id);
    Ok(id)
}

pub fn new_finding_id() -> String {
    format!("f{}", ulid::Ulid::new().to_string().to_lowercase())
}

/// A deferred finding's handle, which a later round collects it by.
pub fn new_deferral_id() -> String {
    format!("def-{}", ulid::Ulid::new().to_string().to_lowercase())
}

/// A change ID is the slug plus a short random suffix so re-used slugs
/// across a repository's lifetime never collide.
pub fn new_change_id(slug: &str) -> String {
    let u = ulid::Ulid::new().to_string().to_lowercase();
    format!("{}-{}", slug, &u[u.len() - 6..])
}

pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slug.len() > 64 {
        bail!("slug must be 1-64 characters");
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("slug may contain only [a-z0-9-]: {slug:?}");
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        bail!("slug must not start/end with '-' or contain '--': {slug:?}");
    }
    Ok(())
}

/// Change directory names come from us, but validate anything read back
/// from disk or user input before it is used as a path component.
pub fn validate_id_component(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 128 {
        bail!("invalid id length: {id:?}");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("id may contain only [A-Za-z0-9-_]: {id:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn unfloored_issuer_refuses_after_the_last_event_id() {
        const CHILD: &str = "ARC_TEST_EXHAUSTED_ISSUER";
        if std::env::var_os(CHILD).is_some() {
            let issued = new_event_id_after(ulid::Ulid::from(u128::MAX - 1)).unwrap();
            assert_eq!(issued, ulid::Ulid::from(u128::MAX));
            assert!(new_event_id().is_err());
            assert!(new_event_id_after(ulid::Ulid::new()).is_err());
            assert!(new_identifier().parse::<ulid::Ulid>().is_ok());
            return;
        }
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ids::tests::unfloored_issuer_refuses_after_the_last_event_id",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
    }

    #[test]
    fn ids_after_a_floor_stay_in_one_sequence() {
        // The current millisecond with its random part exhausted: the next
        // id moves to the following millisecond, never far ahead of the
        // clock other tests in this process read.
        let floor = ulid::Ulid::from_parts(ulid::Ulid::new().timestamp_ms(), u128::MAX >> 48);
        let after = new_event_id_after(floor).unwrap();
        assert!(after > floor);
        let next = new_event_id().unwrap();
        assert!(next > after.to_string());
    }

    #[test]
    fn an_exhausted_random_part_rolls_to_the_next_millisecond() {
        let floor = ulid::Ulid::from_parts(1_000, u128::MAX >> 48);
        let after = after_floor(ulid::Ulid::from_parts(999, 7), floor).unwrap();
        assert_eq!(after, ulid::Ulid::from_parts(1_001, 0));
    }

    #[test]
    fn a_floor_at_the_maximum_id_refuses_instead_of_wrapping() {
        let max = ulid::Ulid::from_parts(u64::MAX, u128::MAX);
        assert_eq!(after_floor(ulid::Ulid::new(), max), None);
        assert!(new_event_id_after(max).is_err());
        // A refusal leaves the sequence untouched.
        assert!(new_event_id_after(ulid::Ulid::new()).is_ok());
    }

    #[test]
    fn event_ids_sort_in_issue_order() {
        let ids = (0..10_000)
            .map(|_| new_event_id().unwrap())
            .collect::<Vec<_>>();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    use super::*;

    #[test]
    fn slug_rules() {
        assert!(validate_slug("radio-refill-fix").is_ok());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("Bad").is_err());
        assert!(validate_slug("a--b").is_err());
        assert!(validate_slug("-a").is_err());
    }

    #[test]
    fn change_ids_are_slug_prefixed_and_unique() {
        let a = new_change_id("fix-thing");
        let b = new_change_id("fix-thing");
        assert!(a.starts_with("fix-thing-"));
        assert_ne!(a, b);
        assert!(validate_id_component(&a).is_ok());
    }
}
