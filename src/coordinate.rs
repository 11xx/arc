//! What a write may record where a model coordinate or an identity belongs.
//!
//! Readers are lenient: [`crate::trailers::value_is_readable`] accepts any
//! `x:y` and any `name <addr>`, so historical events, imported bundles and
//! old commit messages reduce exactly as they were written. A fresh write is
//! held to more. A placeholder or a session id is not a coordinate, and a
//! value nobody can establish is omitted rather than recorded as one.

use crate::trailers::value_is_readable;

/// Spellings that stand in for a value nobody established. A segment equal to
/// one of these, trimmed and compared without case, names nothing.
const PLACEHOLDERS: [&str; 12] = [
    "unknown",
    "unset",
    "none",
    "null",
    "n/a",
    "na",
    "tbd",
    "undefined",
    "?",
    "-",
    "<synthetic>",
    "synthetic",
];

/// Why a value is not something a write may record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Defect {
    /// Neither `harness:model[#effort]` nor `Name <email>` on one line.
    Unreadable,
    /// A harness, model or effort segment, or the whole value, is a
    /// placeholder.
    Placeholder,
    /// The model segment is a session id.
    SessionId,
    /// A `Name <address>` whose name carries a `:` or whose address carries
    /// no `@`.
    NotAnAddress,
}

impl Defect {
    /// Why the value cannot be recorded, as a clause a refusal or a report
    /// places after the value it describes.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Defect::Unreadable => "expected harness:model[#effort] or Name <email> on one line",
            Defect::Placeholder => "a placeholder is not a coordinate",
            Defect::SessionId => "a session id is not a model",
            Defect::NotAnAddress => "Name <email> needs a name without ':' and an address with '@'",
        }
    }
}

/// Whether a value, trimmed and compared without case, is a placeholder.
pub(crate) fn is_placeholder(value: &str) -> bool {
    let value = value.trim();
    PLACEHOLDERS
        .iter()
        .any(|placeholder| placeholder.eq_ignore_ascii_case(value))
}

/// Whether a model segment is a session id: UUID-shaped (8-4-4-4-12 hex), or
/// equal to `session` when the caller knows the acting one.
pub(crate) fn is_session_id(segment: &str, session: Option<&str>) -> bool {
    let segment = segment.trim();
    if session.is_some_and(|session| !session.trim().is_empty() && session.trim() == segment) {
        return true;
    }
    let groups: Vec<&str> = segment.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// What is wrong with a declared `model[#effort]`, if anything.
pub(crate) fn model_defect(value: &str, session: Option<&str>) -> Option<Defect> {
    let (model, effort) = match value.split_once('#') {
        Some((model, effort)) => (model, Some(effort)),
        None => (value, None),
    };
    if is_placeholder(model) || effort.is_some_and(is_placeholder) {
        return Some(Defect::Placeholder);
    }
    is_session_id(model, session).then_some(Defect::SessionId)
}

/// What keeps a role value from being a fresh coordinate; `None` when it is
/// one.
///
/// A fresh value is a human `Name <address>`, whose name carries no `:` and
/// whose address carries an `@`, or an agent `harness:model[#effort]` whose
/// segments are no placeholder and whose model is no session id.
/// `claude:<synthetic>` reads as a person `claude:` at `synthetic`, which is
/// how the lenient reader accepts it; it is an agent form with a placeholder
/// model.
pub(crate) fn coordinate_defect(value: &str, session: Option<&str>) -> Option<Defect> {
    if !value_is_readable(value) {
        return Some(Defect::Unreadable);
    }
    let value = value.trim();
    if is_human(value) {
        return None;
    }
    let Some((harness, rest)) = value.split_once(':') else {
        return Some(Defect::NotAnAddress);
    };
    if is_placeholder(harness) {
        return Some(Defect::Placeholder);
    }
    model_defect(rest, session)
}

/// `Name <address>`: a name with no `:`, and an address with an `@`.
fn is_human(value: &str) -> bool {
    let Some((name, address)) = value.trim().rsplit_once('<') else {
        return false;
    };
    let Some(address) = address.strip_suffix('>') else {
        return false;
    };
    !name.trim().is_empty() && !name.contains(':') && address.contains('@')
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "019f0000-0000-7000-8000-00000000000a";

    #[test]
    fn placeholders_are_matched_trimmed_and_without_case() {
        for value in [
            "unknown",
            " Unknown ",
            "UNSET",
            "none",
            "Null",
            "n/a",
            "N/A",
            "na",
            "TBD",
            "undefined",
            "?",
            "-",
            "<synthetic>",
            "<SYNTHETIC>",
            "synthetic",
        ] {
            assert!(is_placeholder(value), "{value:?}");
        }
        for value in ["unknown-harness", "gpt-6.1-sol", "nan", "--", "", "tester"] {
            assert!(!is_placeholder(value), "{value:?}");
        }
    }

    #[test]
    fn session_ids_are_uuid_shaped_or_the_acting_session() {
        assert!(is_session_id(UUID, None));
        assert!(is_session_id(&UUID.to_uppercase(), None));
        assert!(is_session_id("session-a", Some("session-a")));
        for value in [
            "019f0000-0000-7000-8000-00000000000",
            "019f0000-0000-7000-8000-00000000000g",
            "019f00000000700080000000000000000a",
            "gpt-6.1-sol",
        ] {
            assert!(!is_session_id(value, None), "{value:?}");
        }
        assert!(!is_session_id("gpt-6.1-sol", Some("session-a")));
        assert!(!is_session_id("gpt-6.1-sol", Some(" ")));
    }

    #[test]
    fn declared_models_refuse_placeholders_and_session_ids() {
        assert_eq!(model_defect("unknown", None), Some(Defect::Placeholder));
        assert_eq!(model_defect("None#high", None), Some(Defect::Placeholder));
        assert_eq!(
            model_defect("gpt-6#unknown", None),
            Some(Defect::Placeholder)
        );
        assert_eq!(model_defect(UUID, None), Some(Defect::SessionId));
        assert_eq!(
            model_defect("session-a#low", Some("session-a")),
            Some(Defect::SessionId)
        );
        for value in [
            "gpt-6.1-sol#xhigh",
            "claude-haiku-5-5#high",
            "unknown-harness#low",
        ] {
            assert_eq!(model_defect(value, Some("session-a")), None, "{value:?}");
        }
    }

    #[test]
    fn role_values_are_fresh_coordinates_only() {
        let uuid_coordinate = format!("codex:{UUID}");
        let table: [(&str, Option<Defect>); 14] = [
            ("codex:gpt-6.1-sol#xhigh", None),
            ("claude:claude-haiku-5-5#high", None),
            ("Ada Lovelace <ada@example.org>", None),
            ("codex:unknown", Some(Defect::Placeholder)),
            ("codex:gpt-6#none", Some(Defect::Placeholder)),
            ("unknown:gpt-6", Some(Defect::Placeholder)),
            ("pi:N/A#high", Some(Defect::Placeholder)),
            ("claude:<synthetic>", Some(Defect::Placeholder)),
            (&uuid_coordinate, Some(Defect::SessionId)),
            ("Ada <ada>", Some(Defect::NotAnAddress)),
            ("codex:", Some(Defect::Unreadable)),
            ("unknown", Some(Defect::Unreadable)),
            ("codex:gpt\n#high", Some(Defect::Unreadable)),
            ("", Some(Defect::Unreadable)),
        ];
        for (value, defect) in table {
            assert_eq!(coordinate_defect(value, None), defect, "{value:?}");
        }
        assert_eq!(
            coordinate_defect("test:session-a", Some("session-a")),
            Some(Defect::SessionId)
        );
    }

    #[test]
    fn the_lenient_reader_still_accepts_what_freshness_refuses() {
        for value in [
            "codex:unknown",
            &format!("codex:{UUID}"),
            "claude:<synthetic>",
            "Ada <ada>",
        ] {
            assert!(value_is_readable(value), "{value:?}");
            assert!(coordinate_defect(value, None).is_some(), "{value:?}");
        }
    }
}
