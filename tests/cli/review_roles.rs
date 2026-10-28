use super::common::*;

const MODEL_A: &str = "gpt-6.1-sol#xhigh";
const MODEL_B: &str = "claude-opus-5-5#high";
const MODEL_C: &str = "gemini-4.1-pro#high";
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
fn a_debt_with_no_ledger_record_treats_trailers_as_declared_evidence() {
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
    // With no shipped patchset either, the range has no ledger authorship at
    // all: only commit text is left, and it cannot decide the read.
    rewrite_event(&repo, &change_id, "change-integrated", |event| {
        event["event_type"] = serde_json::json!("change-closed");
        event["outcome"] = serde_json::json!("integrated");
        event.as_object_mut().unwrap().remove("source_patchset_id");
        event.as_object_mut().unwrap().remove("source_head");
        event.as_object_mut().unwrap().remove("target_branch");
        event.as_object_mut().unwrap().remove("target_before");
    });
    // No patchset at all is what leaves this range without ledger authorship:
    // a ledger patchset is authorship even when the closure did not link it.
    let mut events: Vec<PathBuf> = fs::read_dir(event_dir(&repo, &change_id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    events.sort();
    for path in events {
        let event: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "patchset-added" {
            fs::remove_file(path).unwrap();
        }
    }

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

fn numbered_file() -> String {
    (1..=30).map(|line| format!("line {line}\n")).collect()
}

fn snapshot_as(repo: &Repo, cwd: &Path, slug: &str, actor: &str, harness: &str, model: &str) {
    stdout(
        repo.arc(cwd)
            .env("ARC_ACTOR", actor)
            .env("ARC_HARNESS", harness)
            .env("ARC_MODEL", model)
            .args(["snapshot", slug]),
    );
}

fn snapshot_solo_as(repo: &Repo, cwd: &Path, slug: &str, actor: &str, harness: &str, model: &str) {
    stdout(
        repo.arc(cwd)
            .env("ARC_ACTOR", actor)
            .env("ARC_HARNESS", harness)
            .env("ARC_MODEL", model)
            .args(["snapshot", slug, "--solo"]),
    );
}

fn integrate_with_debt(repo: &Repo, slug: &str, actor: &str, model: &str) {
    repo.arc(&repo.root)
        .env("ARC_ACTOR", actor)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", model)
        .args(["integrate", slug, "--debt", "an independent read is owed"])
        .assert()
        .success();
}

#[test]
fn a_clean_same_file_rebase_keeps_the_implementer_coordinate() {
    let repo = policy(true);
    repo.commit(
        &repo.root,
        "file.txt",
        &numbered_file(),
        "chore: numbered file",
    );
    stdout(repo.arc(&repo.root).args(["begin", "same-file"]));
    let worktree = repo.home.join(".worktrees/repo-same-file");
    let authored = numbered_file().replace("line 2\n", "author edit\n");
    repo.commit(&worktree, "file.txt", &authored, "feat: author edit");
    snapshot_as(&repo, &worktree, "same-file", "Author-A", "codex", MODEL_A);

    let target = numbered_file().replace("line 28\n", "target edit\n");
    repo.commit(&repo.root, "file.txt", &target, "chore: target edit");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "same-file"])
        .assert()
        .success();

    let rebased = status(&repo, "same-file");
    assert_eq!(
        rebased["review_subject"]["effective_author"], "Author-A",
        "{rebased}"
    );
    assert_eq!(
        rebased["latest_patchset"]["on_behalf_of"], "Author-A",
        "{rebased}"
    );
    integrate_with_debt(&repo, "same-file", REVIEWER_B, MODEL_B);
    let after = debt(&repo, "same-file");
    assert_eq!(
        after["production"]["implementer"]["actor"], "Author-A",
        "{after}"
    );
    assert_eq!(
        after["production"]["implementer"]["model"], MODEL_A,
        "{after}"
    );

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Another-Actor")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_A)
        .args(["audit", "same-file", "--verdict", "approved"])
        .assert()
        .failure();
    assert_eq!(debt(&repo, "same-file")["standing"], "owed");
}

#[test]
fn a_rebase_that_rewrites_the_implementers_lines_breaks_inheritance() {
    let repo = policy(true);
    repo.commit(
        &repo.root,
        "file.txt",
        &numbered_file(),
        "chore: numbered file",
    );
    stdout(repo.arc(&repo.root).args(["begin", "rewritten"]));
    let worktree = repo.home.join(".worktrees/repo-rewritten");
    let authored = numbered_file().replace("line 2\n", "author edit\n");
    repo.commit(&worktree, "file.txt", &authored, "feat: author edit");
    snapshot_as(&repo, &worktree, "rewritten", "Author-A", "codex", MODEL_A);

    let target = numbered_file().replace("line 2\n", "target edit\n");
    repo.commit(&repo.root, "file.txt", &target, "chore: target edit");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "rewritten"])
        .assert()
        .failure();

    let resolved = numbered_file().replace("line 2\n", "resolved edit\n");
    fs::write(worktree.join("file.txt"), &resolved).unwrap();
    git(&worktree, &["add", "file.txt"]);
    git(&worktree, &["rebase", "--continue"]);
    snapshot_as(&repo, &worktree, "rewritten", REVIEWER_B, "claude", MODEL_B);

    let rebased = status(&repo, "rewritten");
    assert_eq!(rebased["latest_patchset"]["actor"], REVIEWER_B, "{rebased}");
    assert!(
        rebased["latest_patchset"]["on_behalf_of"].is_null(),
        "{rebased}"
    );
    integrate_with_debt(&repo, "rewritten", REVIEWER_B, MODEL_B);
    let after = debt(&repo, "rewritten");
    assert_eq!(
        after["production"]["implementer"]["actor"], REVIEWER_B,
        "{after}"
    );
}

#[test]
fn an_ignored_submodule_change_breaks_patch_inheritance() {
    let repo = policy(true);
    let sub_one = repo.head(&repo.root);
    git(
        &repo.root,
        &["commit", "--allow-empty", "-m", "submodule one"],
    );
    let sub_two = repo.head(&repo.root);
    fs::write(
        repo.root.join(".gitmodules"),
        "[submodule \"nested\"]\n\tpath = nested\n\turl = https://example.invalid/nested\n\tignore = all\n",
    )
    .unwrap();
    fs::create_dir_all(repo.root.join("nested")).unwrap();
    let cacheinfo = format!("160000,{sub_one},nested");
    git(
        &repo.root,
        &["update-index", "--add", "--cacheinfo", &cacheinfo],
    );
    git(&repo.root, &["add", ".gitmodules"]);
    git(&repo.root, &["commit", "-m", "chore: nested gitlink"]);

    stdout(repo.arc(&repo.root).args(["begin", "ignored-submodule"]));
    let worktree = repo.home.join(".worktrees/repo-ignored-submodule");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    snapshot_as(
        &repo,
        &worktree,
        "ignored-submodule",
        "Author-A",
        "codex",
        MODEL_A,
    );

    let cacheinfo = format!("160000,{sub_two},nested");
    git(&worktree, &["update-index", "--cacheinfo", &cacheinfo]);
    git(&worktree, &["commit", "-m", "feat: lead moves submodule"]);
    snapshot_as(
        &repo,
        &worktree,
        "ignored-submodule",
        REVIEWER_B,
        "claude",
        MODEL_B,
    );

    let changed = status(&repo, "ignored-submodule");
    assert_eq!(changed["latest_patchset"]["actor"], REVIEWER_B, "{changed}");
    assert!(
        changed["latest_patchset"]["on_behalf_of"].is_null(),
        "{changed}"
    );
    integrate_with_debt(&repo, "ignored-submodule", REVIEWER_B, MODEL_B);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["audit", "ignored-submodule", "--verdict", "approved"])
        .assert()
        .failure();
    assert_eq!(debt(&repo, "ignored-submodule")["standing"], "owed");
}

#[test]
fn a_nearby_context_rebase_keeps_the_implementer_coordinate() {
    let repo = policy(true);
    repo.commit(
        &repo.root,
        "file.txt",
        &numbered_file(),
        "chore: numbered file",
    );
    stdout(repo.arc(&repo.root).args(["begin", "nearby-context"]));
    let worktree = repo.home.join(".worktrees/repo-nearby-context");
    let authored = numbered_file().replace("line 10\n", "author edit\n");
    repo.commit(&worktree, "file.txt", &authored, "feat: author edit");
    snapshot_as(
        &repo,
        &worktree,
        "nearby-context",
        "Author-A",
        "codex",
        MODEL_A,
    );

    let target = numbered_file().replace("line 12\n", "target edit\n");
    repo.commit(&repo.root, "file.txt", &target, "chore: target edit");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "nearby-context"])
        .assert()
        .success();

    let rebased = status(&repo, "nearby-context");
    assert_eq!(
        rebased["latest_patchset"]["on_behalf_of"], "Author-A",
        "{rebased}"
    );
    assert_eq!(
        rebased["review_subject"]["effective_author"], "Author-A",
        "{rebased}"
    );
    assert_eq!(
        rebased["latest_patchset"]["inherited_authors"][0]["model"], MODEL_A,
        "{rebased}"
    );
    integrate_with_debt(&repo, "nearby-context", REVIEWER_B, MODEL_B);
    let after = debt(&repo, "nearby-context");
    assert_eq!(
        after["production"]["implementer"]["actor"], "Author-A",
        "{after}"
    );
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Another-Actor")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_A)
        .args(["audit", "nearby-context", "--verdict", "approved"])
        .assert()
        .failure();
    assert_eq!(debt(&repo, "nearby-context")["standing"], "owed");
}

#[test]
fn a_relocated_edit_at_an_unchanged_base_belongs_to_the_snapshot_recorder() {
    let repo = policy(true);
    let block: String = (1..=10).map(|line| format!("line {line}\n")).collect();
    repo.commit(
        &repo.root,
        "file.txt",
        &format!("{block}{block}"),
        "chore: repeated blocks",
    );
    stdout(repo.arc(&repo.root).args(["begin", "relocated-edit"]));
    let worktree = repo.home.join(".worktrees/repo-relocated-edit");
    let first = format!("{}{}", block.replace("line 5\n", "changed\n"), block);
    repo.commit(
        &worktree,
        "file.txt",
        &first,
        "feat: author edits first block",
    );
    snapshot_as(
        &repo,
        &worktree,
        "relocated-edit",
        "Author-A",
        "codex",
        MODEL_A,
    );

    let second = format!("{}{}", block, block.replace("line 5\n", "changed\n"));
    repo.commit(
        &worktree,
        "file.txt",
        &second,
        "feat: lead reverts first edit and edits second block",
    );
    snapshot_as(
        &repo,
        &worktree,
        "relocated-edit",
        REVIEWER_B,
        "claude",
        MODEL_B,
    );

    let changed = status(&repo, "relocated-edit");
    assert_eq!(changed["latest_patchset"]["actor"], REVIEWER_B, "{changed}");
    assert!(
        changed["latest_patchset"]["on_behalf_of"].is_null(),
        "{changed}"
    );
    assert!(
        changed["latest_patchset"]["inherited_authors"].is_null(),
        "{changed}"
    );
    integrate_with_debt(&repo, "relocated-edit", REVIEWER_B, MODEL_B);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["audit", "relocated-edit", "--verdict", "approved"])
        .assert()
        .failure();
    assert_eq!(debt(&repo, "relocated-edit")["standing"], "owed");
}

#[test]
fn a_submodule_change_under_diff_submodule_log_belongs_to_the_recorder() {
    let repo = policy(true);
    let sub_one = repo.head(&repo.root);
    git(
        &repo.root,
        &["commit", "--allow-empty", "-m", "submodule one"],
    );
    let sub_two = repo.head(&repo.root);
    fs::write(
        repo.root.join(".gitmodules"),
        "[submodule \"nested\"]\n\tpath = nested\n\turl = https://example.invalid/nested\n\tignore = all\n",
    )
    .unwrap();
    fs::create_dir_all(repo.root.join("nested")).unwrap();
    let cacheinfo = format!("160000,{sub_one},nested");
    git(
        &repo.root,
        &["update-index", "--add", "--cacheinfo", &cacheinfo],
    );
    git(&repo.root, &["add", ".gitmodules"]);
    git(&repo.root, &["commit", "-m", "chore: nested gitlink"]);
    git(&repo.root, &["config", "diff.submodule", "log"]);

    stdout(repo.arc(&repo.root).args(["begin", "submodule-log"]));
    let worktree = repo.home.join(".worktrees/repo-submodule-log");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    snapshot_as(
        &repo,
        &worktree,
        "submodule-log",
        "Author-A",
        "codex",
        MODEL_A,
    );

    let cacheinfo = format!("160000,{sub_two},nested");
    git(&worktree, &["update-index", "--cacheinfo", &cacheinfo]);
    git(&worktree, &["commit", "-m", "feat: lead moves submodule"]);
    snapshot_as(
        &repo,
        &worktree,
        "submodule-log",
        REVIEWER_B,
        "claude",
        MODEL_B,
    );

    let changed = status(&repo, "submodule-log");
    assert_eq!(changed["latest_patchset"]["actor"], REVIEWER_B, "{changed}");
    assert!(
        changed["latest_patchset"]["on_behalf_of"].is_null(),
        "{changed}"
    );
    integrate_with_debt(&repo, "submodule-log", REVIEWER_B, MODEL_B);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["audit", "submodule-log", "--verdict", "approved"])
        .assert()
        .failure();
    assert_eq!(debt(&repo, "submodule-log")["standing"], "owed");
}

#[test]
fn non_utf8_bytes_belong_to_the_session_that_changed_them() {
    let repo = policy(true);
    fs::write(repo.root.join("file.txt"), b"original\n").unwrap();
    git(&repo.root, &["add", "file.txt"]);
    git(&repo.root, &["commit", "-m", "chore: original text"]);
    stdout(repo.arc(&repo.root).args(["begin", "non-utf8-text"]));
    let worktree = repo.home.join(".worktrees/repo-non-utf8-text");

    fs::write(worktree.join("file.txt"), [0xff, b'\n']).unwrap();
    git(&worktree, &["add", "file.txt"]);
    git(&worktree, &["commit", "-m", "feat: author edit"]);
    snapshot_as(
        &repo,
        &worktree,
        "non-utf8-text",
        "Author-A",
        "codex",
        MODEL_A,
    );

    fs::write(worktree.join("file.txt"), [0xfe, b'\n']).unwrap();
    git(&worktree, &["add", "file.txt"]);
    git(&worktree, &["commit", "-m", "feat: lead edit"]);
    snapshot_as(
        &repo,
        &worktree,
        "non-utf8-text",
        REVIEWER_B,
        "claude",
        MODEL_B,
    );

    let changed = status(&repo, "non-utf8-text");
    assert_eq!(changed["latest_patchset"]["actor"], REVIEWER_B, "{changed}");
    assert!(
        changed["latest_patchset"]["on_behalf_of"].is_null(),
        "{changed}"
    );
    integrate_with_debt(&repo, "non-utf8-text", REVIEWER_B, MODEL_B);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["audit", "non-utf8-text", "--verdict", "approved"])
        .assert()
        .failure();
    assert_eq!(debt(&repo, "non-utf8-text")["standing"], "owed");
}

#[test]
fn a_solo_contributors_model_survives_a_rebase_before_integration() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "solo-model-switch"]));
    let worktree = repo.home.join(".worktrees/repo-solo-model-switch");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    snapshot_solo_as(
        &repo,
        &worktree,
        "solo-model-switch",
        "Author-A",
        "codex",
        MODEL_A,
    );

    repo.commit(&repo.root, "target.txt", "target\n", "chore: target");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Author-A")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "solo-model-switch"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Another-Actor")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_A)
        .args(["review", "solo-model-switch", "--verdict", "approved"])
        .assert()
        .success();
    let pending = status(&repo, "solo-model-switch");
    assert_eq!(
        pending["latest_patchset"]["inherited_authors"][0]["model"], MODEL_A,
        "{pending}"
    );
    assert_eq!(
        pending["latest_patchset"]["contributors"],
        serde_json::json!(["Author-A"]),
        "{pending}"
    );
    assert_eq!(pending["has_valid_approval"], false, "{pending}");
    assert!(
        pending["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker == "no-valid-approval"),
        "{pending}"
    );
}

#[test]
fn a_legacy_integration_closure_recovers_the_shipped_patchsets_authorship() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "legacy-closure"]));
    let worktree = repo.home.join(".worktrees/repo-legacy-closure");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    snapshot_as(
        &repo,
        &worktree,
        "legacy-closure",
        "Author-A",
        "codex",
        MODEL_A,
    );
    integrate_with_debt(&repo, "legacy-closure", REVIEWER_B, MODEL_B);
    let change_id = status(&repo, "legacy-closure")["change_id"]
        .as_str()
        .unwrap()
        .to_string();
    rewrite_event(&repo, &change_id, "debt-declared", |event| {
        event.as_object_mut().unwrap().remove("production");
    });
    rewrite_event(&repo, &change_id, "change-integrated", |event| {
        event["event_type"] = serde_json::json!("change-closed");
        event["outcome"] = serde_json::json!("integrated");
        for key in [
            "source_patchset_id",
            "source_head",
            "target_branch",
            "target_before",
            "provisional_resolutions",
            "authority_grant_event_id",
            "authorization",
            "already_contained",
            "shape",
        ] {
            event.as_object_mut().unwrap().remove(key);
        }
    });
    let before = status(&repo, "legacy-closure");
    assert_eq!(
        before["closure"]["integrated_commit"], before["latest_patchset"]["head"],
        "{before}"
    );

    let output = stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "Auditor-B")
            .env("ARC_HARNESS", "claude")
            .env("ARC_MODEL", MODEL_B)
            .args(["audit", "legacy-closure", "--verdict", "approved"]),
    );
    assert!(output.contains("debt discharged"), "{output}");
    assert_eq!(debt(&repo, "legacy-closure")["standing"], "discharged");
}

#[test]
fn a_same_actor_rebase_at_a_new_model_keeps_the_original_model() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "model-switch"]));
    let worktree = repo.home.join(".worktrees/repo-model-switch");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    snapshot_as(
        &repo,
        &worktree,
        "model-switch",
        "Author-A",
        "codex",
        MODEL_A,
    );

    repo.commit(&repo.root, "target.txt", "target\n", "chore: target");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Author-A")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "model-switch"])
        .assert()
        .success();
    integrate_with_debt(&repo, "model-switch", REVIEWER_B, MODEL_B);

    let after = debt(&repo, "model-switch");
    assert_eq!(
        after["production"]["implementer"]["actor"], "Author-A",
        "{after}"
    );
    assert_eq!(
        after["production"]["implementer"]["model"], MODEL_A,
        "{after}"
    );
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Another-Actor")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_A)
        .args(["audit", "model-switch", "--verdict", "approved"])
        .assert()
        .failure();
    assert_eq!(debt(&repo, "model-switch")["standing"], "owed");
}

#[test]
fn an_explicit_contributor_set_is_the_authorship_not_the_recorder() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "explicit"]));
    let worktree = repo.home.join(".worktrees/repo-explicit");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", REVIEWER_B)
            .env("ARC_HARNESS", "claude")
            .env("ARC_MODEL", MODEL_B)
            .args(["snapshot", "explicit", "--contributors", IMPLEMENTER_A]),
    );
    integrate_with_debt(&repo, "explicit", REVIEWER_B, MODEL_B);

    let after = debt(&repo, "explicit");
    assert_eq!(
        after["production"]["implementer"]["actor"], IMPLEMENTER_A,
        "{after}"
    );
    assert_eq!(
        after["production"]["implementer"]["model"], MODEL_A,
        "{after}"
    );
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["audit", "explicit", "--verdict", "approved"])
        .assert()
        .success();
    assert_eq!(debt(&repo, "explicit")["standing"], "discharged");
}

#[test]
fn a_declared_trailer_model_never_decides_independence_without_a_ledger_model() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "no-ledger-model"]));
    let worktree = repo.home.join(".worktrees/repo-no-ledger-model");
    repo.commit(
        &worktree,
        "work.txt",
        "work\n",
        "feat: work\n\nImplemented-by: fixture:claude-opus-5-5#high",
    );
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Author-A")
            .env("ARC_HARNESS", "codex")
            .env_remove("ARC_MODEL")
            .args(["snapshot", "no-ledger-model"]),
    );
    integrate_with_debt(&repo, "no-ledger-model", REVIEWER_B, MODEL_B);
    let before = debt(&repo, "no-ledger-model");
    assert!(
        before["production"]["implementer"].get("model").is_none(),
        "{before}"
    );

    let output = repo
        .arc(&repo.root)
        .env("ARC_ACTOR", "Auditor-B")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["audit", "no-ledger-model", "--verdict", "approved"])
        .assert()
        .success()
        .stderr(predicates::str::contains("model is not recorded"))
        .stderr(predicates::str::contains("Implemented-by trailers"))
        .get_output()
        .clone();
    let output = String::from_utf8_lossy(&output.stdout);
    assert!(output.contains("debt discharged"), "{output}");
    assert_eq!(debt(&repo, "no-ledger-model")["standing"], "discharged");
}

#[test]
fn a_debt_without_a_production_summary_uses_its_patchsets_authorship() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "legacy-author"]));
    let worktree = repo.home.join(".worktrees/repo-legacy-author");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    snapshot_as(
        &repo,
        &worktree,
        "legacy-author",
        "Author-A",
        "codex",
        MODEL_A,
    );
    integrate_with_debt(&repo, "legacy-author", REVIEWER_B, MODEL_B);
    let change_id = status(&repo, "legacy-author")["change_id"]
        .as_str()
        .unwrap()
        .to_string();
    rewrite_event(&repo, &change_id, "debt-declared", |event| {
        event.as_object_mut().unwrap().remove("production");
    });

    let output = stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "Auditor-B")
            .env("ARC_HARNESS", "claude")
            .env("ARC_MODEL", MODEL_B)
            .args(["audit", "legacy-author", "--verdict", "approved"]),
    );
    assert!(output.contains("debt discharged"), "{output}");
    assert_eq!(debt(&repo, "legacy-author")["standing"], "discharged");
}

#[test]
fn integration_on_behalf_of_another_subject_displays_the_invoking_integrator() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "integrator"]));
    let worktree = repo.home.join(".worktrees/repo-integrator");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    snapshot_as(&repo, &worktree, "integrator", "Author-A", "codex", MODEL_A);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .env("ARC_ON_BEHALF_OF", "Other-C")
        .args(["integrate", "integrator", "--debt", "owed"])
        .assert()
        .success();

    let after = debt(&repo, "integrator");
    assert_eq!(
        after["production"]["integrator"]["actor"], REVIEWER_B,
        "{after}"
    );
    assert!(
        after["production"]["integrator"]
            .get("on_behalf_of")
            .is_none(),
        "{after}"
    );
    let show = stdout(repo.arc(&repo.root).args(["show", "integrator"]));
    assert!(
        show.contains(&format!("integrated by {REVIEWER_B}")),
        "{show}"
    );
}

/// An ordinary project hook that authors a commit when Git rewrites refs.
fn install_authoring_post_rewrite_hook(repo: &Repo) {
    install_post_rewrite_hook(
        repo,
        "if test \"$1\" = rebase; then\n\
         \x20 printf 'hook content\\n' > injected.txt\n\
         \x20 git add injected.txt\n\
         \x20 git commit -q -m 'feat: hook authors new content'\n\
         fi\n",
    );
}

/// A hook that amends the replayed commit and then rewrites the branch reflog
/// to claim the amended commit as the replay's own result.
fn install_forging_post_rewrite_hook(repo: &Repo) {
    install_post_rewrite_hook(
        repo,
        "if test \"$1\" = rebase; then\n\
         \x20 original=$(git rev-parse ORIG_HEAD)\n\
         \x20 ref=$(git symbolic-ref HEAD)\n\
         \x20 printf 'hook content\\n' > injected.txt\n\
         \x20 git add injected.txt\n\
         \x20 git -c core.hooksPath=/dev/null commit -q --amend --no-edit\n\
         \x20 amended=$(git rev-parse HEAD)\n\
         \x20 git update-ref \"$ref\" \"$original\"\n\
         \x20 git update-ref -m 'rebase (finish): synthetic hook record' \"$ref\" \"$amended\" \"$original\"\n\
         fi\n",
    );
}

fn install_post_rewrite_hook(repo: &Repo, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let hooks = repo.root.join(".git/hooks");
    fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("post-rewrite");
    fs::write(&hook, format!("#!/bin/sh\n{body}")).unwrap();
    let mut permissions = fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).unwrap();
}

fn commits_between(repo: &Repo, base: &str, head: &str) -> usize {
    git_out(
        &repo.root,
        &["rev-list", "--count", &format!("{base}..{head}")],
    )
    .trim()
    .parse()
    .unwrap()
}

/// The coordinate the ledger records for the one author a replay carries.
fn inherited_model(status: &serde_json::Value, actor: &str) -> Option<String> {
    status["latest_patchset"]["inherited_authors"]
        .as_array()?
        .iter()
        .find(|identity| identity["actor"] == actor)?
        .get("model")?
        .as_str()
        .map(str::to_string)
}

#[test]
fn a_post_rewrite_hook_cannot_add_a_commit_to_a_replayed_patchset() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "hooked-replay"]));
    let worktree = repo.home.join(".worktrees/repo-hooked-replay");
    repo.commit(&worktree, "work.txt", "author work\n", "feat: author work");
    snapshot_as(
        &repo,
        &worktree,
        "hooked-replay",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&repo.root, "target.txt", "target\n", "chore: target");
    install_authoring_post_rewrite_hook(&repo);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "hooked-replay"])
        .assert()
        .success();

    // arc's replay runs plumbing that fires no hook, so the hook authors
    // nothing: the head is exactly Author-A's commit on the new base.
    assert!(
        !worktree.join("injected.txt").exists(),
        "a post-rewrite hook ran during the replay"
    );
    assert_eq!(
        commits_between(&repo, "master", "arc/hooked-replay"),
        1,
        "the replay carries more than the replayed patchset"
    );
    let replayed = status(&repo, "hooked-replay");
    assert_eq!(
        replayed["latest_patchset"]["actor"], REVIEWER_B,
        "{replayed}"
    );
    assert_eq!(
        replayed["latest_patchset"]["on_behalf_of"], "Author-A",
        "{replayed}"
    );
    assert_eq!(
        inherited_model(&replayed, "Author-A").as_deref(),
        Some(MODEL_A),
        "{replayed}"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("target.txt")).unwrap(),
        "target\n",
        "the worktree did not follow the replay"
    );
}

#[test]
fn a_fallback_rebase_is_recorded_as_ordinary_work() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "fallback-replay"]));
    let worktree = repo.home.join(".worktrees/repo-fallback-replay");
    repo.commit(&worktree, "work.txt", "author work\n", "feat: author work");
    snapshot_as(
        &repo,
        &worktree,
        "fallback-replay",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&repo.root, "target.txt", "target\n", "chore: target");
    let path = path_without_git_replay(&repo);
    repo.arc(&repo.root)
        .env("PATH", path)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "fallback-replay"])
        .assert()
        .success();

    // The branch lands on the target, but the replay is Git's rebase, not
    // arc's own, so the patchset is the recorder's ordinary snapshot.
    assert_eq!(
        commits_between(&repo, "master", "arc/fallback-replay"),
        1,
        "the rebase did not land the replayed commit"
    );
    let rebased = status(&repo, "fallback-replay");
    assert_eq!(rebased["latest_patchset"]["actor"], REVIEWER_B, "{rebased}");
    assert!(
        rebased["latest_patchset"]["on_behalf_of"].is_null(),
        "{rebased}"
    );
    assert!(
        rebased["latest_patchset"]
            .get("inherited_authors")
            .is_none(),
        "{rebased}"
    );
    assert_eq!(
        rebased["review_subject"]["effective_author"], REVIEWER_B,
        "{rebased}"
    );
}

#[test]
fn a_forged_reflog_entry_cannot_claim_a_hook_commit_as_the_replay() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "forged-replay"]));
    let worktree = repo.home.join(".worktrees/repo-forged-replay");
    repo.commit(&worktree, "work.txt", "author work\n", "feat: author work");
    snapshot_as(
        &repo,
        &worktree,
        "forged-replay",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&repo.root, "target.txt", "target\n", "chore: target");
    install_forging_post_rewrite_hook(&repo);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "forged-replay"])
        .assert()
        .success();

    // Reflog text is caller-writable, so it decides nothing. The replay ran no
    // hook, and its tip is the only commit the patchset can name.
    assert!(
        !worktree.join("injected.txt").exists(),
        "a post-rewrite hook ran during the replay"
    );
    assert_eq!(
        commits_between(&repo, "master", "arc/forged-replay"),
        1,
        "the replay carries more than the replayed patchset"
    );
    let replayed = status(&repo, "forged-replay");
    assert_eq!(
        replayed["latest_patchset"]["on_behalf_of"], "Author-A",
        "{replayed}"
    );
    assert_eq!(
        inherited_model(&replayed, "Author-A").as_deref(),
        Some(MODEL_A),
        "{replayed}"
    );
}

/// A merge driver the repository configures is a program `git replay` can
/// run. It must not be able to write the tip the replay records: arc runs the
/// replay where the repository's configuration cannot reach it, so the driver
/// never runs and the only result is the one Git computed.
#[test]
fn a_merge_driver_cannot_write_the_replay_result() {
    let repo = policy(true);
    fs::write(repo.root.join("file.txt"), numbered_file()).unwrap();
    fs::write(repo.root.join(".gitattributes"), "file.txt merge=driver\n").unwrap();
    git(&repo.root, &["add", "."]);
    git(&repo.root, &["commit", "-m", "chore: base driver file"]);
    stdout(repo.arc(&repo.root).args(["begin", "driver-forgery"]));
    let worktree = repo.home.join(".worktrees/repo-driver-forgery");
    repo.commit(
        &worktree,
        "file.txt",
        &numbered_file().replace("line 2\n", "author edit\n"),
        "feat: author change",
    );
    snapshot_as(
        &repo,
        &worktree,
        "driver-forgery",
        "Author-A",
        "codex",
        MODEL_A,
    );
    let original = repo.head(&worktree);

    // The lead writes content under its own commit, then resets the branch so
    // only a forged replay tip can carry that content.
    repo.commit(
        &worktree,
        "injected.txt",
        "lead-authored injected content\n",
        "feat: lead writes injected content",
    );
    let injected_tree = git_out(&worktree, &["rev-parse", "HEAD^{tree}"]);
    git(&worktree, &["reset", "--hard", &original]);

    repo.commit(
        &repo.root,
        "file.txt",
        &numbered_file().replace("line 28\n", "target edit\n"),
        "chore: target change",
    );
    let target = repo.head(&repo.root);
    let fake = git_out(
        &repo.root,
        &[
            "commit-tree",
            &injected_tree,
            "-p",
            &target,
            "-m",
            "lead content masquerades as executor replay",
        ],
    );

    let marker = repo.home.join("merge-driver-ran");
    let driver = repo.home.join("merge-driver.sh");
    fs::write(
        &driver,
        format!(
            "#!/bin/sh\n: > '{}'\nprintf 'update refs/heads/arc/driver-forgery {fake} {original}\\n'\nexit 0\n",
            marker.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &repo.root,
        &[
            "config",
            "merge.driver.driver",
            &format!("'{}' %O %A %B", driver.display()),
        ],
    );

    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "driver-forgery"])
        .assert()
        .success();

    let head = repo.head(&worktree);
    assert_ne!(head, fake, "arc took the merge driver's printed update");
    assert!(
        !worktree.join("injected.txt").exists(),
        "the lead's content reached the replayed branch"
    );
    let merged = numbered_file()
        .replace("line 2\n", "author edit\n")
        .replace("line 28\n", "target edit\n");
    assert_eq!(
        fs::read_to_string(worktree.join("file.txt")).unwrap(),
        merged,
        "the worktree is not Git's own merge"
    );
    assert!(
        !marker.exists(),
        "a merge driver the repository configures ran during arc's replay"
    );
    let replayed = status(&repo, "driver-forgery");
    assert_eq!(
        replayed["latest_patchset"]["head"].as_str(),
        Some(head.as_str()),
        "{replayed}"
    );
    assert_eq!(
        replayed["latest_patchset"]["on_behalf_of"], "Author-A",
        "{replayed}"
    );
    assert_eq!(
        inherited_model(&replayed, "Author-A").as_deref(),
        Some(MODEL_A),
        "{replayed}"
    );
}

#[test]
fn an_inherited_reflog_action_does_not_hide_a_clean_replay() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "outer-action"]));
    let worktree = repo.home.join(".worktrees/repo-outer-action");
    repo.commit(&worktree, "work.txt", "author work\n", "feat: author work");
    snapshot_as(
        &repo,
        &worktree,
        "outer-action",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&repo.root, "target.txt", "target\n", "chore: target");
    // A wrapper's reflog label is display text, not evidence about the replay.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .env("GIT_REFLOG_ACTION", "outer-operation")
        .args(["rebase", "outer-action"])
        .assert()
        .success();

    let replayed = status(&repo, "outer-action");
    assert_eq!(
        replayed["latest_patchset"]["on_behalf_of"], "Author-A",
        "{replayed}"
    );
    assert_eq!(
        inherited_model(&replayed, "Author-A").as_deref(),
        Some(MODEL_A),
        "{replayed}"
    );

    integrate_with_debt(&repo, "outer-action", REVIEWER_B, MODEL_B);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Another-Actor")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_A)
        .args(["audit", "outer-action", "--verdict", "approved"])
        .assert()
        .failure();
    assert_eq!(debt(&repo, "outer-action")["standing"], "owed");
}

#[test]
fn a_solo_snapshot_keeps_the_model_the_patchset_recorded_for_itself() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "solo-model"]));
    let worktree = repo.home.join(".worktrees/repo-solo-model");
    repo.commit(
        &worktree,
        "work.txt",
        "author work at model A\n",
        "feat: author work\n\nImplemented-by: codex:gpt-6.1-sol#xhigh",
    );
    snapshot_as(&repo, &worktree, "solo-model", "Author-A", "codex", MODEL_A);
    repo.commit(
        &worktree,
        "work.txt",
        "author work at model B\n",
        "feat: model B work\n\nImplemented-by: claude:claude-opus-5-5#high",
    );
    snapshot_solo_as(
        &repo,
        &worktree,
        "solo-model",
        "Author-A",
        "claude",
        MODEL_B,
    );

    // The recording itself names Author-A at model B, so a read at that model
    // is the author's own whatever earlier records say.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Another-Actor")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["review", "solo-model", "--verdict", "approved"])
        .assert()
        .success();
    let pending = status(&repo, "solo-model");
    assert_eq!(pending["has_valid_approval"], false, "{pending}");
    let reason = pending["approval_rejection_reason"]
        .as_str()
        .unwrap_or_default();
    assert!(
        reason.contains("ledger implementer's model"),
        "the recorded model was lost: {pending}"
    );
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Lander")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", "claude-opus-5-5#max")
        .args(["integrate", "solo-model"])
        .assert()
        .failure();
}

#[test]
fn a_replay_keeps_the_recorded_model_of_every_contributor() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "two-contributors"]));
    let worktree = repo.home.join(".worktrees/repo-two-contributors");
    repo.commit(&worktree, "a.txt", "a\n", "feat: author A work");
    snapshot_as(
        &repo,
        &worktree,
        "two-contributors",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&worktree, "b.txt", "b\n", "feat: author B work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Author-B")
            .env("ARC_HARNESS", "claude")
            .env("ARC_MODEL", MODEL_B)
            .args([
                "snapshot",
                "two-contributors",
                "--contributors",
                "Author-A,Author-B",
            ]),
    );
    repo.commit(&repo.root, "target.txt", "target\n", "chore: target");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Author-B")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", "claude-opus-5-5#max")
        .args(["rebase", "two-contributors"])
        .assert()
        .success();

    // The replayer recorded a third model, so only B's stored coordinate can
    // still say an approval at B's model is B's own read.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Another-Actor")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["review", "two-contributors", "--verdict", "approved"])
        .assert()
        .success();
    let pending = status(&repo, "two-contributors");
    assert_eq!(pending["has_valid_approval"], false, "{pending}");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Lander")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["integrate", "two-contributors"])
        .assert()
        .failure();

    let replayed = status(&repo, "two-contributors");
    assert_eq!(
        replayed["latest_patchset"]["contributors"],
        serde_json::json!(["Author-A", "Author-B"]),
        "{replayed}"
    );
    let inherited = replayed["latest_patchset"]["inherited_authors"]
        .as_array()
        .unwrap_or_else(|| panic!("no per-contributor coordinates: {replayed}"));
    let model_of = |actor: &str| {
        inherited
            .iter()
            .find(|identity| identity["actor"] == actor)
            .and_then(|identity| identity.get("model"))
            .cloned()
    };
    assert_eq!(
        model_of("Author-A"),
        Some(serde_json::json!(MODEL_A)),
        "{replayed}"
    );
    assert_eq!(
        model_of("Author-B"),
        Some(serde_json::json!(MODEL_B)),
        "{replayed}"
    );
}

#[test]
fn a_later_brief_does_not_erase_another_contributors_recorded_model() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "contributor-brief"]));
    let worktree = repo.home.join(".worktrees/repo-contributor-brief");
    repo.commit(&worktree, "a.txt", "a\n", "feat: author A work");
    snapshot_as(
        &repo,
        &worktree,
        "contributor-brief",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&worktree, "b.txt", "b\n", "feat: author B work");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Author-B")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args([
            "snapshot",
            "contributor-brief",
            "--contributors",
            "Author-A,Author-B",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Another-Actor")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_A)
        .args(["review", "contributor-brief", "--verdict", "approved"])
        .assert()
        .success();

    let before = status(&repo, "contributor-brief");
    assert_eq!(before["has_valid_approval"], false, "{before}");
    assert!(
        before["approval_rejection_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("self-approval"),
        "{before}"
    );
    let reviewed = before["latest_patchset"]["id"].clone();

    let body = repo.home.join("contributor-brief.md");
    fs::write(&body, "Verify the existing work.\n").unwrap();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Author-A")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_C)
        .args([
            "brief",
            "contributor-brief",
            "--body-file",
            body.to_str().unwrap(),
        ])
        .assert()
        .success();

    let after = status(&repo, "contributor-brief");
    assert_eq!(after["latest_patchset"]["id"], reviewed, "{after}");
    assert_eq!(after["has_valid_approval"], false, "{after}");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Lander")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["integrate", "contributor-brief"])
        .assert()
        .failure();
}

#[test]
fn a_later_brief_does_not_erase_an_on_behalf_of_subjects_recorded_model() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "subject-brief"]));
    let worktree = repo.home.join(".worktrees/repo-subject-brief");
    repo.commit(
        &worktree,
        "work.txt",
        "author A work\n",
        "feat: author A work",
    );
    snapshot_as(
        &repo,
        &worktree,
        "subject-brief",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&worktree, "more.txt", "lead B work\n", "feat: lead B work");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Lead-B")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["snapshot", "subject-brief", "--on-behalf-of", "Author-A"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Another-Actor")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_A)
        .args(["review", "subject-brief", "--verdict", "approved"])
        .assert()
        .success();

    let before = status(&repo, "subject-brief");
    assert_eq!(before["has_valid_approval"], false, "{before}");
    let reviewed = before["latest_patchset"]["id"].clone();

    let body = repo.home.join("subject-brief.md");
    fs::write(&body, "Verify the existing work.\n").unwrap();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Author-A")
        .env("ARC_HARNESS", "codex")
        .env("ARC_MODEL", MODEL_C)
        .args([
            "brief",
            "subject-brief",
            "--body-file",
            body.to_str().unwrap(),
        ])
        .assert()
        .success();

    let after = status(&repo, "subject-brief");
    assert_eq!(after["latest_patchset"]["id"], reviewed, "{after}");
    assert_eq!(after["has_valid_approval"], false, "{after}");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Lander")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["integrate", "subject-brief"])
        .assert()
        .failure();
}

#[test]
fn a_later_authoring_snapshot_moves_the_next_patchsets_contributor_coordinate() {
    let repo = policy(true);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "authoring-moves-model"]),
    );
    let worktree = repo.home.join(".worktrees/repo-authoring-moves-model");
    repo.commit(&worktree, "a.txt", "a\n", "feat: author A work");
    snapshot_as(
        &repo,
        &worktree,
        "authoring-moves-model",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&worktree, "b.txt", "b\n", "feat: author B work");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Author-B")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args([
            "snapshot",
            "authoring-moves-model",
            "--contributors",
            "Author-A,Author-B",
        ])
        .assert()
        .success();
    repo.commit(&worktree, "c.txt", "c\n", "feat: author A moves");
    snapshot_as(
        &repo,
        &worktree,
        "authoring-moves-model",
        "Author-A",
        "codex",
        MODEL_C,
    );
    repo.commit(&worktree, "d.txt", "d\n", "feat: author B follows");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Author-B")
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args([
            "snapshot",
            "authoring-moves-model",
            "--contributors",
            "Author-A,Author-B",
        ])
        .assert()
        .success();

    integrate_with_debt(&repo, "authoring-moves-model", REVIEWER_B, MODEL_B);

    let after = debt(&repo, "authoring-moves-model");
    assert_eq!(
        after["production"]["implementer"]["actor"], "Author-A",
        "{after}"
    );
    assert_eq!(
        after["production"]["implementer"]["model"], MODEL_C,
        "{after}"
    );
}

#[test]
fn an_explicit_contributor_declaration_on_rebase_is_authoritative() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "declared-replay"]));
    let worktree = repo.home.join(".worktrees/repo-declared-replay");
    repo.commit(&worktree, "work.txt", "author work\n", "feat: author work");
    snapshot_as(
        &repo,
        &worktree,
        "declared-replay",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&repo.root, "target.txt", "target\n", "chore: target");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args([
            "rebase",
            "declared-replay",
            "--contributors",
            "Author-A,Lead-B",
        ])
        .assert()
        .success();

    let replayed = status(&repo, "declared-replay");
    assert_eq!(
        replayed["latest_patchset"]["contributors"],
        serde_json::json!(["Author-A", "Lead-B"]),
        "{replayed}"
    );
    assert_eq!(
        replayed["latest_patchset"]["on_behalf_of"], "Author-A",
        "{replayed}"
    );
    let inherited = replayed["latest_patchset"]["inherited_authors"]
        .as_array()
        .unwrap_or_else(|| panic!("no carried coordinate: {replayed}"));
    assert_eq!(inherited.len(), 1, "{replayed}");
    assert_eq!(inherited[0]["actor"], "Author-A", "{replayed}");
    assert_eq!(inherited[0]["model"], MODEL_A, "{replayed}");

    // The lead declared itself a contributor of the replayed patchset, so its
    // read is authorship and leaves the debt owed.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["review", "declared-replay", "--verdict", "approved"])
        .assert()
        .success();
    let pending = status(&repo, "declared-replay");
    assert_eq!(pending["has_valid_approval"], false, "{pending}");
    integrate_with_debt(&repo, "declared-replay", REVIEWER_B, MODEL_B);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["audit", "declared-replay", "--verdict", "approved"])
        .assert()
        .failure();
    assert_eq!(debt(&repo, "declared-replay")["standing"], "owed");
}

#[test]
fn a_solo_declaration_on_rebase_records_only_the_declared_contributor() {
    let repo = policy(true);
    stdout(repo.arc(&repo.root).args(["begin", "solo-replay"]));
    let worktree = repo.home.join(".worktrees/repo-solo-replay");
    repo.commit(&worktree, "work.txt", "author work\n", "feat: author work");
    snapshot_as(
        &repo,
        &worktree,
        "solo-replay",
        "Author-A",
        "codex",
        MODEL_A,
    );
    repo.commit(&repo.root, "target.txt", "target\n", "chore: target");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "solo-replay", "--solo"])
        .assert()
        .success();

    let replayed = status(&repo, "solo-replay");
    assert_eq!(
        replayed["latest_patchset"]["contributors"],
        serde_json::json!([REVIEWER_B]),
        "{replayed}"
    );
    assert!(
        replayed["latest_patchset"]["on_behalf_of"].is_null(),
        "{replayed}"
    );
    assert!(
        replayed["latest_patchset"]
            .get("inherited_authors")
            .is_none(),
        "{replayed}"
    );
    assert_eq!(
        replayed["review_subject"]["effective_author"], REVIEWER_B,
        "{replayed}"
    );
}

#[test]
fn a_replay_that_drops_an_empty_commit_still_inherits_the_replayed_author() {
    let repo = policy(true);
    repo.commit(&repo.root, "file.txt", "base\n", "chore: base file");
    stdout(repo.arc(&repo.root).args(["begin", "dropped-replay"]));
    let worktree = repo.home.join(".worktrees/repo-dropped-replay");
    repo.commit(&worktree, "file.txt", "target\n", "feat: author file");
    repo.commit(&worktree, "work.txt", "author work\n", "feat: author work");
    snapshot_as(
        &repo,
        &worktree,
        "dropped-replay",
        "Author-A",
        "codex",
        MODEL_A,
    );
    // The target lands the file change already, so the replay produces
    // nothing for that commit and drops it; the other one is replayed. Both
    // belong to the patchset the replay was made from.
    repo.commit(&repo.root, "file.txt", "target\n", "chore: target file");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", REVIEWER_B)
        .env("ARC_HARNESS", "claude")
        .env("ARC_MODEL", MODEL_B)
        .args(["rebase", "dropped-replay"])
        .assert()
        .success();

    let replayed = status(&repo, "dropped-replay");
    assert_eq!(
        replayed["latest_patchset"]["actor"], REVIEWER_B,
        "{replayed}"
    );
    assert_eq!(
        replayed["latest_patchset"]["on_behalf_of"], "Author-A",
        "{replayed}"
    );
    assert_eq!(
        inherited_model(&replayed, "Author-A").as_deref(),
        Some(MODEL_A),
        "{replayed}"
    );
    assert_eq!(
        replayed["review_subject"]["effective_author"], "Author-A",
        "{replayed}"
    );
}
