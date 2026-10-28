use super::common::*;

const MODEL_A: &str = "gpt-6.1-sol#xhigh";
const MODEL_B: &str = "claude-opus-5-5#high";
const IMPLEMENTER_A: &str = "codex:gpt-6.1-sol#xhigh";
const REVIEWER_B: &str = "Lead-B";

fn policy(forbid_self_approval: bool) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        format!("[policy]\nforbid_self_approval = {forbid_self_approval}\n"),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    repo
}

fn plan_naming_lead(repo: &Repo) -> String {
    let body = "# Roles plan\nplanned-by: {\"actor\":\"Lead-B\",\"harness\":\"claude\",\"model\":\"claude-opus-5-5#high\"}\n\nDo the work.\n";
    let output = stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "Plan-Author")
            .args(["journal", "plan", "review-role-plan", "--body-file", "-"])
            .write_stdin(body),
    );
    Path::new(output.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string()
}

fn begin_with_plan(repo: &Repo, slug: &str, plan: &str) -> PathBuf {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let brief = repo.home.join(format!("{slug}-brief.md"));
    fs::write(&brief, "Do it as planned.\n").unwrap();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args([
            "brief",
            slug,
            "--plan-ref",
            plan,
            "--plan-slice",
            "one",
            "--body-file",
            brief.to_str().unwrap(),
        ])
        .assert()
        .success();
    repo.home.join(".worktrees").join(format!("repo-{slug}"))
}

fn status(repo: &Repo, slug: &str) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]))
}

fn debt(repo: &Repo, slug: &str) -> serde_json::Value {
    status(repo, slug)["debt"].clone()
}

#[test]
fn a_planner_and_orchestrator_can_clear_work_implemented_for_another_model() {
    let repo = policy(true);
    let plan = plan_naming_lead(&repo);
    let worktree = begin_with_plan(&repo, "role-cleared", &plan);
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", REVIEWER_B)
            .env("ARC_HARNESS", "claude")
            .env("ARC_MODEL", MODEL_B)
            .args(["snapshot", "role-cleared", "--on-behalf-of", IMPLEMENTER_A]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["review", "role-cleared", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args([
            "integrate",
            "role-cleared",
            "--debt",
            "a deeper read is owed",
        ])
        .assert()
        .success();

    let before = debt(&repo, "role-cleared");
    assert_eq!(before["standing"], "owed", "{before}");
    assert_eq!(
        before["production"]["implementer"]["model"], MODEL_A,
        "{before}"
    );
    assert_eq!(
        before["production"]["orchestrator"]["actor"], REVIEWER_B,
        "{before}"
    );

    let output = stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", REVIEWER_B)
            .env("ARC_HARNESS", "claude")
            .env("ARC_MODEL", MODEL_B)
            .args(["audit", "role-cleared", "--verdict", "approved"]),
    );
    assert!(output.contains("debt discharged"), "{output}");
    let after = status(&repo, "role-cleared");
    assert_eq!(after["debt_outstanding"], false, "{after}");
    assert_eq!(
        after["audit_verdicts"][0]["held_roles"],
        serde_json::json!(["planner", "orchestrator"]),
        "{after}"
    );
}

#[test]
fn an_implementer_can_never_clear_its_own_work_even_when_trailers_disagree() {
    let repo = policy(false);
    stdout(repo.arc(&repo.root).args(["begin", "role-refused"]));
    let worktree = repo.home.join(".worktrees/repo-role-refused");
    repo.commit(
        &worktree,
        "work.txt",
        "work\n",
        "feat: work\n\nImplemented-by: codex:gpt-6.1-sol#low",
    );
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", REVIEWER_B)
            .env("ARC_HARNESS", "claude")
            .env("ARC_MODEL", MODEL_B)
            .args(["snapshot", "role-refused"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["review", "role-refused", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args([
            "integrate",
            "role-refused",
            "--debt",
            "an independent read is owed",
        ])
        .assert()
        .success();

    let output = repo
        .arc(&repo.root)
        .env("ARC_ACTOR", "Auditor-X")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["audit", "role-refused", "--verdict", "approved"])
        .assert()
        .success()
        .stderr(predicates::str::contains("independent"))
        .get_output()
        .clone();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("still owes"), "{text}");
    let after = debt(&repo, "role-refused");
    assert_eq!(after["standing"], "owed", "{after}");
    assert!(after["discharged_by"].is_null(), "{after}");
    assert_eq!(
        after["production"]["trailer_implementers"],
        serde_json::json!(["codex:gpt-6.1-sol#low"]),
        "{after}"
    );
    let show = stdout(repo.arc(&repo.root).args(["show", "role-refused"]));
    assert!(
        show.contains("Implemented-by trailers: codex:gpt-6.1-sol#low"),
        "{show}"
    );
}

#[test]
fn a_rebased_snapshot_keeps_its_author_and_lists_the_landing_session_as_integrator() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "role-rebased"]));
    let worktree = repo.home.join(".worktrees/repo-role-rebased");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Author-A")
            .env("ARC_HARNESS", "codex")
            .env("ARC_MODEL", MODEL_A)
            .args(["snapshot", "role-rebased"]),
    );
    let original = repo.head(&worktree);

    repo.commit(&repo.root, "target.txt", "target\n", "chore: target moves");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "role-rebased"])
        .assert()
        .success();
    let rebased = status(&repo, "role-rebased");
    assert_ne!(rebased["latest_patchset"]["head"], original, "{rebased}");
    assert_eq!(rebased["latest_patchset"]["actor"], REVIEWER_B, "{rebased}");
    assert_eq!(
        rebased["latest_patchset"]["on_behalf_of"], "Author-A",
        "{rebased}"
    );
    assert_eq!(
        rebased["review_subject"]["effective_author"], "Author-A",
        "{rebased}"
    );

    repo.arc(&worktree)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["review", "role-rebased", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args([
            "integrate",
            "role-rebased",
            "--debt",
            "an independent read is owed",
        ])
        .assert()
        .success();

    let after = debt(&repo, "role-rebased");
    assert_eq!(
        after["production"]["implementer"]["actor"], "Author-A",
        "{after}"
    );
    assert_eq!(
        after["production"]["integrator"]["actor"], REVIEWER_B,
        "{after}"
    );
    assert_eq!(
        status(&repo, "role-rebased")["review_subject"]["contributors"],
        serde_json::json!(["Author-A"]),
        "{after}"
    );
}

#[test]
fn trailers_without_a_production_record_are_declared_evidence_that_neither_refuses_nor_clears() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "role-declared"]));
    let worktree = repo.home.join(".worktrees/repo-role-declared");
    repo.commit(
        &worktree,
        "work.txt",
        "work\n",
        "feat: work\n\nImplemented-by: codex:gpt-6.1-sol#xhigh",
    );
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", REVIEWER_B)
            .env("ARC_HARNESS", "claude")
            .env("ARC_MODEL", MODEL_B)
            .args(["snapshot", "role-declared"]),
    );
    let change_id = status(&repo, "role-declared")["change_id"]
        .as_str()
        .unwrap()
        .to_string();
    repo.arc(&worktree)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["review", "role-declared", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args([
            "integrate",
            "role-declared",
            "--debt",
            "an independent read is owed",
        ])
        .assert()
        .success();
    rewrite_event(&repo, &change_id, "debt-declared", |event| {
        event.as_object_mut().unwrap().remove("production");
    });

    let output = repo
        .arc(&repo.root)
        .env("ARC_ACTOR", "Author-A")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_A)
        .args(["audit", "role-declared", "--verdict", "approved"])
        .assert()
        .success()
        .stderr(predicates::str::contains("declared evidence"))
        .get_output()
        .clone();
    let output = String::from_utf8_lossy(&output.stdout);
    assert!(output.contains("still owes"), "{output}");
    let after = debt(&repo, "role-declared");
    assert_eq!(after["standing"], "owed", "{after}");
    assert!(after["discharged_by"].is_null(), "{after}");
}
