use anyhow::{bail, Result};

/// Event ids sort in the order this process wrote them, so a command that
/// records several dependent events (an opening and the facts asserted with
/// it) replays them in that order even within one millisecond.
static EVENT_IDS: std::sync::Mutex<ulid::Generator> = std::sync::Mutex::new(ulid::Generator::new());

pub fn new_event_id() -> String {
    let mut generator = EVENT_IDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Overflow needs 2^80 ids in one millisecond; a fresh id then still
    // carries a later timestamp than any already issued.
    generator
        .generate()
        .unwrap_or_else(|_| ulid::Ulid::new())
        .to_string()
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
    fn event_ids_sort_in_issue_order() {
        let ids = (0..10_000).map(|_| new_event_id()).collect::<Vec<_>>();
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
