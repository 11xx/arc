use super::common::*;

#[test]
fn implementer_role_refuses_lead_owned_commands_without_appending_events() {
    let repo = Repo::new();
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["begin", "role-guard", "--no-worktree"]),
    );
    let change_id = opened_change_id(&opened);
    let initial_events = event_count(&repo, &change_id);
    let refused = [
        (
            "review",
            "reviewer or lead",
            vec!["review", "role-guard", "--verdict", "approved"],
        ),
        (
            "resolve",
            "reviewer or lead",
            vec![
                "resolve",
                "role-guard",
                "finding-id",
                "--status",
                "resolved",
            ],
        ),
        (
            "hold",
            "reviewer or lead",
            vec!["hold", "role-guard", "--reason", "pause"],
        ),
        (
            "release-hold",
            "reviewer or lead",
            vec!["release-hold", "role-guard", "01HOLD"],
        ),
        ("close", "lead", vec!["close", "role-guard", "--abandoned"]),
        ("integrate", "lead", vec!["integrate", "role-guard"]),
    ];

    for (name, required, args) in refused {
        repo.arc(&repo.root)
            .env("ARC_ROLE", "implementer")
            .args(args)
            .assert()
            .code(9)
            .stderr(format!(
                "role refusal: implementer may not {name} (requires {required})\n"
            ));
        assert_eq!(
            event_count(&repo, &change_id),
            initial_events,
            "{name} refusal must not append an event"
        );
    }
}

#[test]
fn reviewer_role_can_review_and_resolve_but_cannot_close_or_integrate() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "reviewer-role");
    let finding = stdout(repo.arc(&worktree).args([
        "finding",
        "reviewer-role",
        "--summary",
        "reviewer can resolve this",
    ]));
    let finding_id = finding
        .lines()
        .find_map(|line| line.strip_prefix("finding: "))
        .unwrap();

    repo.arc(&worktree)
        .env("ARC_ROLE", "reviewer")
        .args(["review", "reviewer-role", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ROLE", "reviewer")
        .args([
            "resolve",
            "reviewer-role",
            finding_id,
            "--status",
            "resolved",
        ])
        .assert()
        .success();

    let allowed_events = event_count(&repo, &change_id);
    for (name, args) in [
        ("integrate", vec!["integrate", "reviewer-role"]),
        ("close", vec!["close", "reviewer-role", "--abandoned"]),
    ] {
        repo.arc(&repo.root)
            .env("ARC_ROLE", "reviewer")
            .args(args)
            .assert()
            .code(9)
            .stderr(format!(
                "role refusal: reviewer may not {name} (requires lead)\n"
            ));
        assert_eq!(event_count(&repo, &change_id), allowed_events);
    }
}

#[test]
fn invalid_role_is_a_usage_error() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .env("ARC_ROLE", " executor ")
        .args(["config"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(
            "invalid execution role \"executor\"; expected implementer, reviewer, or lead",
        ));
}

#[test]
fn role_flag_and_environment_binding_are_equivalent() {
    let repo = Repo::new();
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["begin", "role-binding", "--no-worktree"]),
    );
    let change_id = opened_change_id(&opened);
    let initial_events = event_count(&repo, &change_id);

    let from_env = repo
        .arc(&repo.root)
        .env("ARC_ROLE", " implementer ")
        .args(["hold", "role-binding", "--reason", "env"])
        .output()
        .unwrap();
    let from_flag = repo
        .arc(&repo.root)
        .args([
            "--role",
            "implementer",
            "hold",
            "role-binding",
            "--reason",
            "flag",
        ])
        .output()
        .unwrap();

    assert_eq!(from_env.status.code(), Some(9));
    assert_eq!(from_flag.status.code(), Some(9));
    assert_eq!(from_env.stderr, from_flag.stderr);
    assert_eq!(
        String::from_utf8_lossy(&from_env.stderr),
        "role refusal: implementer may not hold (requires reviewer or lead)\n"
    );
    assert_eq!(event_count(&repo, &change_id), initial_events);
}

#[test]
fn lead_and_unset_roles_retain_full_access() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "explicit-lead", "--no-worktree"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "unset-role", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .env("ARC_ROLE", "lead")
        .args(["hold", "explicit-lead", "--reason", "lead probe"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["hold", "unset-role", "--reason", "unset probe"])
        .assert()
        .success();
}
