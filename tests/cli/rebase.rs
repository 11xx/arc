use super::common::*;
use super::retention::{collect_garbage, has_object};

fn commit_gates(repo: &Repo) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.alpha]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gates"]);
}

fn change_id_of(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .expect("begin prints the change id")
        .to_string()
}

/// Open a change whose branch commits one file, and move the target so a
/// rebase has something to replay.
fn diverged(repo: &Repo, slug: &str, branch_file: &str, target_content: &str) -> (String, PathBuf) {
    let change_id = change_id_of(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&wt, branch_file, "branch\n", "feat: branch work");
    stdout(repo.arc(&wt).args(["snapshot", slug]));
    repo.commit(&repo.root, "README.md", target_content, "feat: move target");
    (change_id, wt)
}

#[test]
fn clean_rebase_snapshots_the_replayed_head_and_names_the_owed_gates() {
    let repo = Repo::new();
    commit_gates(&repo);
    let (change_id, wt) = diverged(&repo, "clean-replay", "branch.txt", "target\n");
    let before = repo.head(&wt);
    let target_head = repo.head(&repo.root);

    let out = stdout(repo.arc(&wt).args(["rebase", "clean-replay"]));
    let replayed = repo.head(&wt);
    assert_ne!(replayed, before, "the branch must be replayed");
    assert!(
        out.contains(&format!(
            "rebased arc/clean-replay onto master at {replayed}"
        )),
        "{out}"
    );
    assert!(out.contains("gates owed:"), "{out}");
    assert!(
        out.contains("`alpha` at head: no evidence at head"),
        "{out}"
    );

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "clean-replay"]))).unwrap();
    assert_eq!(status["needs_rebase"], false, "{status}");
    assert_eq!(status["head_matches_latest_patchset"], true, "{status}");
    assert_eq!(status["latest_patchset"]["head"], replayed, "{status}");
    assert!(
        git_out(
            &wt,
            &["rev-list", "--count", &format!("{target_head}..HEAD")]
        ) == "1",
        "the replayed branch sits directly on the target"
    );
    assert!(change_id.starts_with("clean-replay"), "{change_id}");
}

#[test]
fn rebase_verify_runs_the_required_gates_at_the_replayed_head() {
    let repo = Repo::new();
    commit_gates(&repo);
    let (_, wt) = diverged(&repo, "verify-replay", "branch.txt", "target\n");

    let out = stdout(repo.arc(&wt).args(["rebase", "verify-replay", "--verify"]));
    assert!(
        out.contains("gates: every required gate is green at head"),
        "{out}"
    );
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "verify-replay"]))).unwrap();
    assert_eq!(status["gates"][0]["name"], "alpha", "{status}");
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
}

#[test]
fn a_conflicting_rebase_stops_in_progress_and_names_the_conflicting_files() {
    let repo = Repo::new();
    let change_id = change_id_of(&stdout(
        repo.arc(&repo.root).args(["begin", "conflict-replay"]),
    ));
    let wt = repo.home.join(".worktrees").join("repo-conflict-replay");
    repo.commit(&wt, "README.md", "branch head\n", "feat: change readme");
    stdout(repo.arc(&wt).args(["snapshot", "conflict-replay"]));
    repo.commit(
        &repo.root,
        "README.md",
        "target head\n",
        "feat: move target",
    );
    let before = repo.head(&wt);

    let assertion = repo
        .arc(&wt)
        .args(["rebase", "conflict-replay"])
        .assert()
        .code(11);
    let out = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    assert!(out.contains("stopped on a conflict"), "{out}");
    assert!(out.contains("  - README.md"), "{out}");
    assert!(out.contains("rebase --continue"), "{out}");
    assert!(out.contains(&format!("arc snapshot {change_id}")), "{out}");

    // The partial resolution is the operator's work: arc must leave it standing.
    assert_eq!(
        git_out(&wt, &["diff", "--name-only", "--diff-filter=U"]),
        "README.md",
        "the conflict is still unresolved in the index"
    );

    // A second invocation names the state rather than restarting the replay.
    let second = repo
        .arc(&wt)
        .args(["rebase", "conflict-replay"])
        .assert()
        .code(11);
    let err = String::from_utf8_lossy(&second.get_output().stderr).into_owned();
    assert!(err.contains("already mid-rebase"), "{err}");

    // No patchset was recorded for a head nobody produced.
    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "conflict-replay"]),
    ))
    .unwrap();
    assert_eq!(status["latest_patchset"]["head"], before, "{status}");

    // The printed recovery finishes the replay and clears the blocker.
    fs::write(wt.join("README.md"), "resolved\n").unwrap();
    git(&wt, &["add", "README.md"]);
    git(&wt, &["rebase", "--continue"]);
    repo.arc(&wt)
        .args(["snapshot", "conflict-replay"])
        .assert()
        .success();
    let cleared: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "conflict-replay"]))).unwrap();
    assert_eq!(cleared["needs_rebase"], false, "{cleared}");
    assert_eq!(
        cleared["latest_patchset"]["head"],
        repo.head(&wt),
        "{cleared}"
    );
}

/// Naming the gates a replayed head owes is advice about a rebase that already
/// stands. A target policy that does not parse leaves that advice unknown,
/// not the rebase failed: the replayed head stays recorded, the command says
/// so, and it succeeds.
#[test]
fn an_unreadable_target_policy_warns_after_rebase_records() {
    let repo = Repo::new();
    let (change_id, wt) = diverged(&repo, "advisory-replay", "branch.txt", "target\n");
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    repo.commit(
        &repo.root,
        ".arc/policy.toml",
        "[policy\n",
        "test: break the target policy",
    );

    let out = repo
        .arc(&wt)
        .args(["rebase", "advisory-replay"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    let replayed = repo.head(&wt);
    assert!(stdout.contains("patchset: ps-02"), "{stdout}");
    assert!(stderr.contains("warning: "), "{stderr}");
    assert!(stderr.contains(".arc/policy.toml"), "{stderr}");
    let recorded = fs::read_dir(event_dir(&repo, &change_id))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .filter(|event| event.contains("\"patchset-added\""))
        .any(|event| event.contains(&replayed));
    assert!(recorded, "the replayed head {replayed} is not recorded");
}

#[test]
fn a_dirty_worktree_is_refused_by_name() {
    let repo = Repo::new();
    let (_, wt) = diverged(&repo, "dirty-replay", "branch.txt", "target\n");
    fs::write(wt.join("branch.txt"), "uncommitted\n").unwrap();
    let before = repo.head(&wt);

    let assertion = repo
        .arc(&wt)
        .args(["rebase", "dirty-replay"])
        .assert()
        .code(11);
    let err = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();
    assert!(err.contains("uncommitted changes"), "{err}");
    // Every worktree shares the stash, so the advice keeps the dirty work
    // out of it.
    assert!(!err.contains("stash"), "{err}");
    assert_eq!(repo.head(&wt), before, "nothing may be replayed");
}

#[test]
fn a_branch_already_on_its_target_is_a_no_op() {
    let repo = Repo::new();
    let change_id = change_id_of(&stdout(
        repo.arc(&repo.root).args(["begin", "already-there"]),
    ));
    let wt = repo.home.join(".worktrees").join("repo-already-there");
    repo.commit(&wt, "branch.txt", "branch\n", "feat: branch work");
    stdout(repo.arc(&wt).args(["snapshot", "already-there"]));
    let head = repo.head(&wt);

    let out = stdout(repo.arc(&wt).args(["rebase", "already-there"]));
    assert!(out.contains("nothing to replay"), "{out}");
    assert!(!out.contains("gates owed"), "{out}");
    assert_eq!(repo.head(&wt), head, "a no-op replays nothing");

    let events = stdout(repo.arc(&wt).args(["events", "--change", &change_id]));
    assert_eq!(
        events.matches("patchset-added").count(),
        1,
        "a no-op records no second patchset: {events}"
    );
}

#[test]
fn the_needs_rebase_blocker_names_the_command_that_clears_it() {
    let repo = Repo::new();
    let change_id = change_id_of(&stdout(
        repo.arc(&repo.root).args(["begin", "blocked-replay"]),
    ));
    let wt = repo.home.join(".worktrees").join("repo-blocked-replay");
    repo.commit(&wt, "README.md", "branch head\n", "feat: change readme");
    stdout(repo.arc(&wt).args(["snapshot", "blocked-replay"]));
    repo.commit(
        &repo.root,
        "README.md",
        "target head\n",
        "feat: move target",
    );

    let assertion = repo
        .arc(&wt)
        .args(["check", "blocked-replay"])
        .assert()
        .code(11);
    let out = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    assert!(out.contains(&format!("arc rebase {change_id}")), "{out}");
}

#[test]
fn rebase_verify_from_another_checkout_gates_the_changes_worktree() {
    let repo = Repo::new();
    commit_gates(&repo);
    let (_, wt) = diverged(&repo, "anchor-replay", "branch.txt", "target\n");

    let out = stdout(
        repo.arc(&repo.root)
            .args(["rebase", "anchor-replay", "--verify"]),
    );
    assert!(
        out.contains(&format!("running in {}", wt.display())),
        "the gate run names the checkout it happened in: {out}"
    );
    assert!(
        out.contains("gates: every required gate is green at head"),
        "{out}"
    );

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "anchor-replay"]),
    ))
    .unwrap();
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
    assert_eq!(
        status["latest_patchset"]["head"],
        repo.head(&wt),
        "{status}"
    );
}

fn retained(repo: &Repo) -> Vec<String> {
    git_out(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/arc/retained/",
        ],
    )
    .lines()
    .filter(|line| !line.is_empty())
    .map(str::to_string)
    .collect()
}

fn recorded_event(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap_or_else(|| panic!("{output}"))
        .to_string()
}

/// Whether the last replay went through `git rebase`, which leaves
/// `ORIG_HEAD` behind, rather than `git replay`, which does not.
fn rebased_by_git_rebase(wt: &Path) -> bool {
    let orig_head = git_out(
        wt,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "ORIG_HEAD",
        ],
    );
    Path::new(&orig_head).exists()
}

/// Claim: a rebase through `git replay` holds a checkpoint revision it
/// replaces under the checkpoint's retention ref, and holds nothing a keep
/// pin already reaches. Rejects: a checkpoint on a commit above the recorded
/// head, which no pin reaches once the branch moves, left to garbage
/// collection.
#[test]
fn a_replayed_rebase_retains_a_checkpoint_revision_above_the_recorded_head() {
    let repo = Repo::new();
    let (_, wt) = diverged(&repo, "held-replay", "branch.txt", "target\n");
    let recorded_head = repo.head(&wt);
    repo.commit(&wt, "above.txt", "above\n", "feat: above the recorded head");
    let tested = repo.head(&wt);
    let event = recorded_event(&stdout(repo.arc(&repo.root).args([
        "checkpoint",
        "record",
        "rc",
        "--rev",
        &tested,
    ])));

    stdout(repo.arc(&wt).args(["rebase", "held-replay"]));
    assert!(!rebased_by_git_rebase(&wt), "the replay fell back");
    assert_ne!(repo.head(&wt), tested);
    collect_garbage(&repo.root);

    assert!(
        has_object(&repo.root, &tested),
        "garbage collection took it"
    );
    assert!(has_object(&repo.root, &recorded_head));
    assert_eq!(
        retained(&repo),
        [format!(
            "refs/arc/retained/repository/{event}/{tested} {tested}"
        )]
    );
}

/// Claim: a rebase that falls back to `git rebase` holds a verdict revision
/// it replaces under the verdict's retention ref. Rejects: retention only on
/// the `git replay` path, which leaves the fallback's replaced commits to
/// garbage collection.
#[test]
fn a_fallback_rebase_retains_a_verdict_revision_above_the_recorded_head() {
    let repo = Repo::new();
    let (change_id, wt) = diverged(&repo, "held-fallback", "branch.txt", "target\n");
    repo.commit(&wt, "above.txt", "above\n", "feat: above the recorded head");
    let read = repo.head(&wt);
    let event = recorded_event(&stdout(repo.arc(&wt).args([
        "external",
        "verdict",
        "held-fallback",
        "--verdict",
        "approved",
        "--decided-by",
        "Upstream Maintainer",
        "--reference",
        "https://example.invalid/pull/7",
        "--revision",
        &read,
    ])));

    stdout(
        repo.arc(&wt)
            .args(["rebase", "held-fallback"])
            .env("PATH", path_without_git_replay(&repo)),
    );
    assert!(rebased_by_git_rebase(&wt), "the replay did not fall back");
    git(&wt, &["update-ref", "-d", "ORIG_HEAD"]);
    collect_garbage(&repo.root);

    assert!(has_object(&repo.root, &read), "garbage collection took it");
    assert_eq!(
        retained(&repo),
        [format!(
            "refs/arc/retained/{change_id}/{event}/{read} {read}"
        )]
    );
}

/// Claim: a rebase that stops on a conflict has already held the cited
/// commits it is replacing, since nothing of arc runs when it is finished by
/// hand. Rejects: holding them only after a replay that completes.
#[test]
fn a_rebase_stopped_on_a_conflict_already_holds_what_it_replaces() {
    let repo = Repo::new();
    let (_, wt) = diverged(&repo, "held-stop", "branch.txt", "target\n");
    repo.commit(
        &wt,
        "README.md",
        "branch\n",
        "feat: conflicts with the target",
    );
    let tested = repo.head(&wt);
    let event = recorded_event(&stdout(repo.arc(&repo.root).args([
        "checkpoint",
        "record",
        "rc",
        "--rev",
        &tested,
    ])));

    repo.arc(&wt)
        .args(["rebase", "held-stop"])
        .assert()
        .failure();
    assert!(git_out(&wt, &["status"]).contains("rebase in progress"));
    assert_eq!(
        retained(&repo),
        [format!(
            "refs/arc/retained/repository/{event}/{tested} {tested}"
        )]
    );
}

/// A source whose change was rebased past a checkpoint on a commit above its
/// recorded head, exported: the bundle, the checkpoint event and the commit.
fn rebased_past_a_checkpoint(source: &Repo) -> (PathBuf, String, String) {
    let (_, wt) = diverged(source, "carried", "branch.txt", "target\n");
    source.commit(&wt, "above.txt", "above\n", "feat: above the recorded head");
    let tested = source.head(&wt);
    let event = recorded_event(&stdout(source.arc(&source.root).args([
        "checkpoint",
        "record",
        "rc",
        "--rev",
        &tested,
    ])));
    stdout(source.arc(&wt).args(["rebase", "carried"]));
    let bundle = source.home.join("bundle.json");
    stdout(
        source
            .arc(&wt)
            .args(["export", "carried", "--output", bundle.to_str().unwrap()]),
    );
    (bundle, event, tested)
}

/// Deliver the source's objects and leave no ref behind them, so only what
/// the import creates holds the delivered commits.
fn deliver_without_refs(source: &Repo, receiver: &Repo) {
    deliver_git_objects(source, receiver);
    for name in git_out(
        &receiver.root,
        &["for-each-ref", "--format=%(refname)", "refs/delivered/"],
    )
    .lines()
    {
        git(&receiver.root, &["update-ref", "-d", name]);
    }
}

/// Claim: an import holds every commit its events cite that arrived and no
/// ref reaches on the receiver, and only those, so a cited commit a rebase
/// held at the source survives garbage collection there and `arc doctor` is
/// quiet about it; a dry run creates nothing. Rejects: a rebase's
/// retention, which no event names, leaving the commit held by nothing once
/// it is delivered, and a ref for a cited commit a pin already reaches.
#[test]
fn an_import_holds_a_cited_commit_a_rebase_held_at_its_source() {
    let source = Repo::new();
    let (bundle, event, tested) = rebased_past_a_checkpoint(&source);
    let receiver = Repo::new();
    deliver_without_refs(&source, &receiver);
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["import", bundle.to_str().unwrap(), "--dry-run"]),
    );
    assert!(retained(&receiver).is_empty(), "a dry run created a ref");

    stdout(
        receiver
            .arc(&receiver.root)
            .args(["import", bundle.to_str().unwrap()]),
    );
    collect_garbage(&receiver.root);

    assert!(
        has_object(&receiver.root, &tested),
        "garbage collection took it"
    );
    assert_eq!(
        retained(&receiver),
        [format!(
            "refs/arc/retained/repository/{event}/{tested} {tested}"
        )],
        "only the cited commit no pin reaches is held"
    );
    let report = json_stdout_any_status(receiver.arc(&receiver.root).args(["doctor", "--json"]));
    let unretained: Vec<_> = report["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "unretained-commit")
        .collect();
    assert!(unretained.is_empty(), "{unretained:?}");
}

/// Claim: an import holds only commits the bundle lists as delivered. A
/// cited commit that is here, but that the bundle's object list leaves out,
/// gets no ref. Rejects: holding whatever the receiver happens to have.
#[test]
fn an_import_holds_no_commit_its_bundle_did_not_deliver() {
    let source = Repo::new();
    let (bundle, _, tested) = rebased_past_a_checkpoint(&source);
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    value["objects"]
        .as_array_mut()
        .unwrap()
        .retain(|object| object["oid"] != tested.as_str());
    fs::write(&bundle, json_file_bytes(&value)).unwrap();

    let receiver = Repo::new();
    deliver_without_refs(&source, &receiver);
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["import", bundle.to_str().unwrap()]),
    );
    assert!(has_object(&receiver.root, &tested));
    assert!(retained(&receiver).is_empty(), "{:?}", retained(&receiver));
}
