//! Cost view inputs and figures a review found mishandled.

use super::common::*;

// --- A non-ASCII idle gap ---------------------------------------------------

/// Claim: `--idle` refuses any value that is not a whole number with an
/// `s`, `m` or `h` unit as a usage error naming it, whatever its characters.
/// Rejects splitting a multi-byte character, which panics.
#[test]
fn a_non_ascii_idle_gap_is_a_usage_error() {
    let repo = Repo::new();
    for value in ["é", "5é", "é5m", "", "0m", "5"] {
        let output = repo
            .arc(&repo.root)
            .args(["cost", "show", "anything", "--idle", value])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{value:?}: {stderr}");
        assert!(
            stderr.contains("is not a duration such as 90s, 45m or 2h"),
            "{value:?}: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "{value:?}: {stderr}");
    }
}
