//! A write needs an actor. When no flag, variable, harness session or Git
//! identity names one, the write refuses rather than recording a word of
//! arc's own, and a read proceeds.

use super::absent_identity::{journal_dir, refusal, without_session};
use super::common::*;

/// `arc` with no declared actor, and no harness or session to derive one from.
pub(crate) fn without_actor(repo: &Repo, cwd: &Path) -> AssertCommand {
    let mut command = without_session(repo, cwd);
    command.env_remove("ARC_ACTOR");
    command
}

#[test]
fn a_ledger_write_with_no_establishable_actor_refuses_and_a_read_proceeds() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "existing"]));
    let change = opened_change_id(&opened);
    git(&repo.root, &["config", "--unset", "user.name"]);

    let stderr = refusal(without_actor(&repo, &repo.root).args(["begin", "no-actor"]));
    assert!(
        stderr.contains("--actor") && stderr.contains("ARC_ACTOR"),
        "{stderr}"
    );
    assert!(git_out(&repo.root, &["branch", "--list", "arc/no-actor"]).is_empty());
    let stderr = refusal(without_actor(&repo, &repo.root).args(["journal", "log", "t", "m"]));
    assert!(stderr.contains("ARC_ACTOR"), "{stderr}");
    assert!(!journal_dir(&repo).join("events.jsonl").exists());
    let stderr = refusal(without_actor(&repo, &repo.root).args(["roles", &change]));
    assert!(stderr.contains("--actor"), "{stderr}");

    for read in [vec!["list"], vec!["catchup"], vec!["log", &change]] {
        without_actor(&repo, &repo.root)
            .args(&read)
            .assert()
            .success();
    }
}

#[test]
fn a_session_that_names_the_actor_stands_in_for_an_unset_git_identity() {
    let repo = Repo::new();
    git(&repo.root, &["config", "--unset", "user.name"]);
    let mut command = repo.arc(&repo.root);
    command.env_remove("ARC_ACTOR").args(["begin", "derived"]);
    stdout(&mut command);
    let mut command = repo.arc(&repo.root);
    command.env_remove("ARC_ACTOR").args(["log", "derived"]);
    let logged = stdout(&mut command);
    assert!(logged.contains("test:session-a@test"), "{logged}");
    assert!(!logged.contains("unknown"), "{logged}");
}
