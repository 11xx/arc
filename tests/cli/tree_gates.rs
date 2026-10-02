use super::common::*;

/// A change whose gate pairs a file the target owns with one the change adds.
///
/// Each side is correct alone and their merge is not, which is the shape no
/// textual merge can notice: the files never overlap, so Git reports success
/// and the result violates what the gate asserts.
fn paired_change(repo: &Repo) -> (String, PathBuf) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.paired]\ncommand = 'test \"$(cat a.txt)\" = \"$(cat b.txt)\"'\n",
    )
    .unwrap();
    fs::write(repo.root.join("a.txt"), "one\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(
        &repo.root,
        &["commit", "-m", "test: base with the paired gate"],
    );

    let begun = stdout(repo.arc(&repo.root).args(["begin", "paired"]));
    let change_id = begun
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let worktree = repo.home.join(".worktrees/repo-paired");
    repo.commit(
        &worktree,
        "b.txt",
        "one\n",
        "test: add b.txt paired with a.txt",
    );
    repo.arc(&worktree)
        .args(["verify", "paired", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["snapshot", "paired"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "paired", "--verdict", "approved"])
        .assert()
        .success();
    (change_id, worktree)
}

fn scratch_worktree(repo: &Repo) -> PathBuf {
    repo.home.join(".worktrees/repo-paired-against")
}

fn status(repo: &Repo) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["status", "paired"]))
}

fn blockers(repo: &Repo) -> Vec<String> {
    json_stdout_any_status(repo.arc(&repo.root).args(["check", "paired", "--json"]))["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|blocker| blocker["blocker"].as_str().unwrap().to_string())
        .collect()
}

fn verification_events(repo: &Repo) -> usize {
    stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "paired",
        "--type",
        "verification-recorded",
    ]))
    .lines()
    .filter(|line| !line.trim().is_empty())
    .count()
}

#[test]
fn a_clean_merge_breaking_a_gate_is_refused_before_it_can_ship() {
    let repo = Repo::new();
    let (_, worktree) = paired_change(&repo);
    // The target gains a commit the change never sees. Nothing overlaps, so
    // the merge is clean and the gate it breaks has already passed at the head.
    repo.commit(&repo.root, "a.txt", "two\n", "test: a.txt becomes two");
    git(
        &repo.root,
        &[
            "merge-tree",
            "--write-tree",
            "--no-messages",
            "master",
            "arc/paired",
        ],
    );

    let refused = status(&repo);
    assert_eq!(refused["ready_to_integrate"], false, "{refused}");
    assert!(
        blockers(&repo).contains(&"merged-tree-unevaluated".to_string()),
        "{refused}"
    );
    assert_eq!(refused["next_action"], "verify_against:master", "{refused}");
    let merged_tree = refused["merged_tree"].as_str().unwrap().to_string();

    repo.arc(&repo.root)
        .args(["check", "paired"])
        .assert()
        .code(14)
        .stdout(predicates::str::contains("arc verify --against master"));

    // Evaluating the merge is what produces the missing answer, and the answer
    // is that the gate fails on content neither branch committed.
    repo.arc(&worktree)
        .args(["verify", "paired", "--against", "master"])
        .assert()
        .code(1);

    let evaluated = status(&repo);
    let gate = &evaluated["gates"][0];
    assert_eq!(gate["result"], "fail", "{evaluated}");
    assert_eq!(gate["evaluated_tree"], merged_tree, "{evaluated}");
    assert!(
        blockers(&repo).contains(&"gates-not-green".to_string()),
        "{evaluated}"
    );

    let before = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args(["integrate", "paired"])
        .assert()
        .code(5);
    assert_eq!(repo.head(&repo.root), before, "the merge must not have run");
}

#[test]
fn evidence_at_the_merged_tree_authorizes_the_merge_that_carries_it() {
    let repo = Repo::new();
    let (_, worktree) = paired_change(&repo);
    // A sibling that leaves the gate's premise intact: the change is behind
    // its target, and the merge is sound rather than merely clean.
    repo.commit(&repo.root, "c.txt", "sibling\n", "test: unrelated sibling");

    repo.arc(&repo.root)
        .args(["check", "paired"])
        .assert()
        .code(14);
    repo.arc(&worktree)
        .args(["verify", "paired", "--against", "master"])
        .assert()
        .success();

    let ready = status(&repo);
    assert_eq!(ready["ready_to_integrate"], true, "{ready}");
    let merged_tree = ready["merged_tree"].as_str().unwrap().to_string();
    assert_eq!(ready["gates"][0]["evaluated_tree"], merged_tree, "{ready}");

    repo.arc(&repo.root)
        .args(["integrate", "paired"])
        .assert()
        .success();
    // What shipped is the content that was evaluated, not merely a merge of
    // the commits that were approved.
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "HEAD^{tree}"]),
        merged_tree
    );
}

#[test]
fn a_target_that_moves_again_spends_the_evidence_for_the_earlier_merge() {
    let repo = Repo::new();
    let (_, worktree) = paired_change(&repo);
    repo.commit(&repo.root, "c.txt", "sibling\n", "test: unrelated sibling");
    repo.arc(&worktree)
        .args(["verify", "paired", "--against", "master"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["check", "paired"])
        .assert()
        .success();

    // A different target makes a different merge, and nothing has evaluated
    // that one — exactly as a verdict stops covering a head that moved.
    repo.commit(&repo.root, "d.txt", "later\n", "test: target moves again");

    let stale = status(&repo);
    assert!(
        blockers(&repo).contains(&"merged-tree-unevaluated".to_string()),
        "{stale}"
    );
    assert_eq!(stale["next_action"], "verify_against:master", "{stale}");
}

fn deleting_change(repo: &Repo) -> (String, PathBuf) {
    repo.redeclare_gates("[gates.unit]\ncommand = 'true'\n");
    let begun = stdout(repo.arc(&repo.root).args(["begin", "delete"]));
    let change_id = opened_change_id(&begun);
    let worktree = repo.home.join(".worktrees/repo-delete");
    git(&worktree, &["rm", "README.md"]);
    git(&worktree, &["commit", "-m", "test: delete README"]);
    repo.arc(&worktree).args(["snapshot"]).assert().success();
    (change_id, worktree)
}

#[test]
fn a_modify_delete_conflict_refuses_verification_without_recording_any_event() {
    let repo = Repo::new();
    let (change_id, worktree) = deleting_change(&repo);
    repo.commit(
        &repo.root,
        "README.md",
        "target edit\n",
        "test: edit deleted README",
    );
    let merge = Command::new("git")
        .current_dir(&repo.root)
        .args(["merge-tree", "--write-tree", "master", "arc/delete"])
        .output()
        .unwrap();
    assert_eq!(merge.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&merge.stdout).contains("CONFLICT (modify/delete): README.md"));

    let recorded = event_count(&repo, &change_id);
    let refs = git_out(&repo.root, &["show-ref"]);
    for skip_green in [false, true] {
        let mut verify = repo.arc(&worktree);
        verify.args(["verify", "--against", "master"]);
        if skip_green {
            verify.arg("--skip-green");
        }
        verify
            .assert()
            .code(1)
            .stderr(predicates::str::contains("conflicts textually"))
            .stderr(predicates::str::contains("rebase first"));
        assert_eq!(event_count(&repo, &change_id), recorded);
        assert_eq!(git_out(&repo.root, &["show-ref"]), refs);
        assert!(!repo.home.join(".worktrees/repo-delete-against").exists());
    }
    let state = json_stdout(repo.arc(&repo.root).args(["show", &change_id, "--json"]));
    assert!(state["verifications"].as_array().unwrap().is_empty());
    assert!(state["verification_runs"].as_array().unwrap().is_empty());
}

#[test]
fn earlier_merge_evidence_does_not_answer_a_target_that_conflicts() {
    for verify_head in [false, true] {
        let repo = Repo::new();
        let (change_id, worktree) = deleting_change(&repo);
        if verify_head {
            repo.arc(&worktree)
                .args(["verify", "--all"])
                .assert()
                .success();
        }
        repo.commit(
            &repo.root,
            "sibling.txt",
            "sibling\n",
            "test: unrelated target edit",
        );
        let target = repo.head(&repo.root);
        repo.arc(&worktree)
            .args(["verify", "--against", "master"])
            .assert()
            .success();
        let evaluated = json_stdout(repo.arc(&repo.root).args(["status", &change_id]));
        assert_eq!(evaluated["gates"][0]["green_at_head"], true, "{evaluated}");
        assert!(evaluated["merged_tree"].is_string(), "{evaluated}");
        let evidence = json_stdout(repo.arc(&repo.root).args(["show", &change_id, "--json"]));
        assert_eq!(
            evidence["verifications"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["against_target"],
            target
        );

        repo.commit(
            &repo.root,
            "README.md",
            "target edit\n",
            "test: edit deleted README",
        );
        let stale = json_stdout(repo.arc(&repo.root).args(["status", &change_id]));
        assert_eq!(stale["needs_rebase"], true, "{stale}");
        assert!(stale["merged_tree"].is_null(), "{stale}");
        assert_eq!(stale["gates"][0]["green_at_head"], verify_head, "{stale}");
        assert_eq!(
            stale["gates"][0]["result"],
            if verify_head { "pass" } else { "pending" },
            "{stale}"
        );
        assert_eq!(stale["next_action"], "rebase", "{stale}");
        assert_eq!(stale["ready_to_integrate"], false, "{stale}");
        let checked = repo
            .arc(&repo.root)
            .args(["check", &change_id, "--json"])
            .assert()
            .code(11)
            .get_output()
            .stdout
            .clone();
        let checked: serde_json::Value = serde_json::from_slice(&checked).unwrap();
        assert!(checked["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["blocker"] == "needs-rebase"));
        assert!(!checked["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["blocker"] == "merged-tree-unevaluated"));
        assert_eq!(
            json_stdout(repo.arc(&repo.root).args(["show", &change_id, "--json"]))["verifications"],
            evidence["verifications"],
        );
        let recorded = event_count(&repo, &change_id);
        repo.arc(&worktree)
            .args(["verify", "--against", "master", "--skip-green"])
            .assert()
            .code(1);
        assert_eq!(event_count(&repo, &change_id), recorded);
    }
}

#[test]
fn a_head_already_on_the_target_tip_integrates_on_its_own_evidence() {
    let repo = Repo::new();
    let (_, _) = paired_change(&repo);
    // The target has not moved, so the merge ships the head's own tree and the
    // evidence recorded there is evidence about what ships.
    let recorded = verification_events(&repo);
    repo.arc(&repo.root)
        .args(["integrate", "paired"])
        .assert()
        .success();
    assert_eq!(
        verification_events(&repo),
        recorded,
        "integration must not run or record a gate"
    );
}

#[test]
fn a_textual_conflict_is_still_a_rebase_rather_than_an_unevaluated_merge() {
    let repo = Repo::new();
    let (_, _) = paired_change(&repo);
    // Both sides edit one file, so there is no single merged tree to evaluate
    // and the answer is the one a person has to give.
    repo.commit(
        &repo.root,
        "b.txt",
        "conflicting\n",
        "test: target takes b.txt",
    );

    let blocked = status(&repo);
    let refusals = blockers(&repo);
    assert!(refusals.contains(&"needs-rebase".to_string()), "{blocked}");
    assert!(
        !refusals.contains(&"merged-tree-unevaluated".to_string()),
        "{blocked}"
    );
    assert!(blocked["merged_tree"].is_null(), "{blocked}");
    repo.arc(&repo.root)
        .args(["check", "paired"])
        .assert()
        .code(11);
}

#[test]
fn the_scratch_checkout_is_gone_whether_the_merge_passes_or_fails() {
    let repo = Repo::new();
    let (_, worktree) = paired_change(&repo);
    repo.commit(&repo.root, "c.txt", "sibling\n", "test: unrelated sibling");
    repo.arc(&worktree)
        .args(["verify", "paired", "--against", "master"])
        .assert()
        .success();
    assert!(
        !scratch_worktree(&repo).exists(),
        "passing run left a checkout"
    );
    assert!(!git_out(&repo.root, &["worktree", "list"]).contains("paired-against"));

    repo.commit(&repo.root, "a.txt", "two\n", "test: a.txt becomes two");
    repo.arc(&worktree)
        .args(["verify", "paired", "--against", "master"])
        .assert()
        .code(1);
    assert!(
        !scratch_worktree(&repo).exists(),
        "failing run left a checkout"
    );
    assert!(!git_out(&repo.root, &["worktree", "list"]).contains("paired-against"));
}

#[test]
fn the_synthesized_merge_stays_reachable_while_evidence_cites_it() {
    let repo = Repo::new();
    let (change_id, worktree) = paired_change(&repo);
    repo.commit(&repo.root, "c.txt", "sibling\n", "test: unrelated sibling");
    let recorded = stdout(
        repo.arc(&worktree)
            .args(["verify", "paired", "--against", "master"]),
    );
    let synthesized = recorded
        .lines()
        .find_map(|line| line.strip_prefix("synthesized merge: "))
        .unwrap();

    let pins = git_out(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            &format!("refs/arc/tree/{change_id}/"),
        ],
    );
    assert!(
        pins.contains(&format!("merge-{synthesized} {synthesized}")),
        "{pins}"
    );
    // A pin is only worth having if it survives what would otherwise collect
    // the commit: nothing else refers to it.
    git(&repo.root, &["reflog", "expire", "--expire=now", "--all"]);
    git(&repo.root, &["gc", "--prune=now"]);
    assert_eq!(
        git_out(&repo.root, &["cat-file", "-t", synthesized]),
        "commit"
    );
}

#[test]
fn evaluating_a_merge_cannot_be_narrowed_to_one_check() {
    let repo = Repo::new();
    let (_, worktree) = paired_change(&repo);
    repo.arc(&worktree)
        .args([
            "verify",
            "paired",
            "--against",
            "master",
            "--gate",
            "paired",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--against"));
}
