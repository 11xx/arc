use super::common::*;

/// Claim: printed entry instructions fit their published size ceilings.
/// Rejects a capability map or root help exceeding the reader's bounded input.
#[test]
fn entry_instructions_fit_their_size_ceilings() {
    let repo = Repo::new();
    let map = stdout(&mut repo.arc(&repo.home));
    assert!(map.lines().count() <= 80);
    assert!(map.len() <= 6_144);
    for flag in ["-h", "--help"] {
        let help = stdout(repo.arc(&repo.home).arg(flag));
        assert!(help.len() <= 12_288);
    }
}
