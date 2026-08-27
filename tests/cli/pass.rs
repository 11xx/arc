use super::common::*;

fn pass_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("pass: "))
        .expect("pass output should contain a pass id")
        .to_string()
}

fn event_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("pass output should contain an event id")
        .to_string()
}

#[test]
fn pass_lists_exact_members_and_updates_verdict_coverage() {
    let repo = Repo::new();
    let (change_a, _worktree_a, _) = change_with_patchset(&repo, "pass-a");
    let (change_b, worktree_b, _) = change_with_patchset(&repo, "pass-b");
    let (change_c, _worktree_c, _) = change_with_patchset(&repo, "pass-c");
    let member_a = format!("{change_a}:ps-01");
    let member_b = format!("{change_b}:ps-01");
    let member_c = format!("{change_c}:ps-01");

    let opened = stdout(repo.arc(&repo.root).args([
        "pass",
        "open",
        "--member",
        &member_a,
        "--member",
        &member_b,
        "--member",
        &member_c,
        "--note",
        "one combined pass",
    ]));
    let pass = pass_id(&opened);
    let before = stdout(repo.arc(&repo.root).args(["pass", "list"]));
    assert!(before.contains(&member_a), "{before}");
    assert!(before.contains(&member_b), "{before}");
    assert!(before.contains(&member_c), "{before}");
    assert_eq!(before.matches("not covered").count(), 3, "{before}");

    repo.arc(&worktree_b)
        .args(["review", "pass-b", "--verdict", "approved"])
        .assert()
        .success();

    let after = stdout(repo.arc(&repo.root).args(["pass", "list"]));
    assert!(after.contains(&format!("{member_b} — covered")), "{after}");
    assert!(
        after.contains(&format!("{member_a} — not covered")),
        "{after}"
    );
    assert!(
        after.contains(&format!("{member_c} — not covered")),
        "{after}"
    );
    assert!(after.contains(&format!("pass {pass} [open]")), "{after}");
}

#[test]
fn pass_end_refusals_name_the_existing_or_missing_pass() {
    let repo = Repo::new();
    let (change_id, _worktree, _) = change_with_patchset(&repo, "pass-end");
    let member = format!("{change_id}:ps-01");
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["pass", "open", "--member", &member]),
    );
    let pass = pass_id(&opened);
    let opened_event = event_id(&opened);

    let not_a_pass = repo
        .arc(&repo.root)
        .args(["pass", "complete", &opened_event])
        .output()
        .unwrap();
    assert!(!not_a_pass.status.success());
    let not_a_pass_error = String::from_utf8_lossy(&not_a_pass.stderr);
    assert!(
        not_a_pass_error.contains("is not a review pass"),
        "{not_a_pass_error}"
    );

    let completed = stdout(repo.arc(&repo.root).args(["pass", "complete", &pass]));
    let ending_event = event_id(&completed);

    let second = repo
        .arc(&repo.root)
        .args(["pass", "complete", &pass])
        .output()
        .unwrap();
    assert!(!second.status.success());
    let second_error = String::from_utf8_lossy(&second.stderr);
    assert!(
        second_error.contains("already ended as completed"),
        "{second_error}"
    );
    assert!(second_error.contains(&ending_event), "{second_error}");

    let missing = repo
        .arc(&repo.root)
        .args([
            "pass",
            "abandon",
            "01J00000000000000000000000",
            "--reason",
            "x",
        ])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let missing_error = String::from_utf8_lossy(&missing.stderr);
    assert!(
        missing_error.contains("no review pass has id"),
        "{missing_error}"
    );
    assert_ne!(second_error, missing_error);
}

#[test]
fn pass_open_rejects_a_phantom_patchset_by_member() {
    let repo = Repo::new();
    let (change_id, _worktree, _) = change_with_patchset(&repo, "pass-phantom");
    let member = format!("{change_id}:ps-does-not-exist");
    repo.arc(&repo.root)
        .args(["pass", "open", "--member", &member])
        .assert()
        .failure()
        .stderr(predicates::str::contains(&member))
        .stderr(predicates::str::contains("has no patchset"));
}

#[test]
fn abandoned_pass_json_keeps_reason_and_event_ids() {
    let repo = Repo::new();
    let (change_id, _worktree, _) = change_with_patchset(&repo, "pass-json");
    let member = format!("{change_id}:ps-01");
    let opened = stdout(repo.arc(&repo.root).args([
        "pass",
        "open",
        "--member",
        &member,
        "--note",
        "reviewer opened the batch",
    ]));
    let pass = pass_id(&opened);
    let opened_event = event_id(&opened);
    let abandoned = stdout(repo.arc(&repo.root).args([
        "pass",
        "abandon",
        &pass,
        "--reason",
        "reviewer process stopped",
    ]));
    let ending_event = event_id(&abandoned);

    let rows = json_stdout(repo.arc(&repo.root).args(["pass", "list", "--json"]));
    let row = &rows[0];
    assert_eq!(row["pass_id"], pass);
    assert_eq!(row["opened_event_id"], opened_event);
    assert_eq!(row["ending_event_id"], ending_event);
    assert_eq!(row["state"], "abandoned");
    assert_eq!(row["note"], "reviewer opened the batch");
    assert_eq!(row["reason"], "reviewer process stopped");
    assert_eq!(row["members"][0]["member"], member);
    assert_eq!(row["members"][0]["covered"], false);
    for field in [
        "pass_id",
        "opened_event_id",
        "opened_at",
        "opened_by",
        "members",
        "note",
        "state",
        "ending_event_id",
        "ending_at",
        "ending_by",
        "ending_note",
        "reason",
    ] {
        assert!(
            row.as_object().unwrap().contains_key(field),
            "{field}: {row}"
        );
    }
}

#[test]
fn open_pass_does_not_change_an_unrelated_check_decision() {
    let repo = Repo::new();
    let (change_id, _worktree, _) = change_with_patchset(&repo, "pass-unrelated");
    let before = repo
        .arc(&repo.root)
        .args(["check", "pass-unrelated"])
        .output()
        .unwrap();

    let member = format!("{change_id}:ps-01");
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["pass", "open", "--member", &member]),
    );
    let pass = pass_id(&opened);

    let after = repo
        .arc(&repo.root)
        .args(["check", "pass-unrelated"])
        .output()
        .unwrap();
    assert_eq!(before.status.code(), after.status.code());
    assert_ne!(before.stdout, after.stdout);
    assert!(
        String::from_utf8_lossy(&after.stdout).contains(&format!("pass {pass}")),
        "{}",
        String::from_utf8_lossy(&after.stdout)
    );
    assert_eq!(before.stderr, after.stderr);
}
