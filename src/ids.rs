use anyhow::{bail, Result};

pub fn new_event_id() -> String {
    ulid::Ulid::new().to_string()
}

pub fn new_finding_id() -> String {
    format!("f{}", ulid::Ulid::new().to_string().to_lowercase())
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
