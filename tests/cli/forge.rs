use super::common::*;

/// Begin a forge-profile change, commit, and snapshot; returns
/// (change_id, worktree, head_sha).
fn forge_change(repo: &Repo, slug: &str) -> (String, PathBuf, String) {
    let out = stdout(repo.arc(&repo.root).args([
        "begin",
        slug,
        "--profile",
        "forge",
        "--target",
        "master",
    ]));
    let change_id = opened_change_id(&out);
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    let head = repo.head(&worktree);
    (change_id, worktree, head)
}

fn status_json(repo: &Repo, reference: &str) -> serde_json::Value {
    serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", reference]))).unwrap()
}

#[test]
fn forge_profile_without_declaration_is_undeclared_then_declared() {
    let repo = Repo::new();
    let (change_id, _wt, _head) = forge_change(&repo, "proj-declare");

    let status = status_json(&repo, "proj-declare");
    assert_eq!(status["schema"], "arc-status/27");
    assert_eq!(status["forge"]["projection"], "undeclared");

    let before = event_count(&repo, &change_id);
    repo.arc(&repo.root)
        .args([
            "forge",
            "declare",
            "proj-declare",
            "--host",
            "github.com",
            "--base-repo",
            "example-owner/example-repo",
            "--base-ref",
            "dev",
            "--head-repo",
            "example-owner/example-repo",
            "--head-ref",
            "arc/proj-declare",
        ])
        .assert()
        .success();
    assert_eq!(event_count(&repo, &change_id), before + 1);

    let status = status_json(&repo, "proj-declare");
    assert_eq!(status["forge"]["projection"], "declared");
    assert_eq!(status["forge"]["declared"]["host"], "github.com");
    assert_eq!(
        status["forge"]["declared"]["base_repo"],
        "example-owner/example-repo"
    );
    assert_eq!(status["forge"]["declared"]["base_ref"], "dev");
    assert_eq!(
        status["forge"]["declared"]["head_repo"],
        "example-owner/example-repo"
    );
    assert_eq!(status["forge"]["declared"]["head_ref"], "arc/proj-declare");
    assert_eq!(
        status["forge"]["declared"]["policy"],
        "same-repository-only"
    );
}

#[test]
fn non_forge_change_without_forge_events_omits_the_block() {
    let repo = Repo::new();
    let (_id, _wt, _head) = change_with_patchset(&repo, "plain");
    let status = status_json(&repo, "plain");
    assert_eq!(status["schema"], "arc-status/27");
    assert!(status.get("forge").is_none() || status["forge"].is_null());
}

fn declare_same_repo(repo: &Repo, reference: &str, head_ref: &str) {
    repo.arc(&repo.root)
        .args([
            "forge",
            "declare",
            reference,
            "--host",
            "github.com",
            "--base-repo",
            "example-owner/example-repo",
            "--base-ref",
            "dev",
            "--head-repo",
            "example-owner/example-repo",
            "--head-ref",
            head_ref,
        ])
        .assert()
        .success();
}

#[test]
fn forge_link_matches_declaration_and_refuses_each_mismatch_axis() {
    let repo = Repo::new();
    let (change_id, _wt, head) = forge_change(&repo, "linker");
    declare_same_repo(&repo, "linker", "arc/linker");

    // Each mismatch axis refuses with exit 10 and appends no event. Exactly
    // one axis is wrong per run; the rest match the declaration.
    let before = event_count(&repo, &change_id);
    for axis in ["base-repo", "base-ref", "head-repo", "head-ref"] {
        let mut base_repo = "example-owner/example-repo";
        let mut base_ref = "dev";
        let mut head_repo = "example-owner/example-repo";
        let mut head_ref = "arc/linker";
        match axis {
            "base-repo" => base_repo = "other/repo",
            "base-ref" => base_ref = "main",
            "head-repo" => head_repo = "other/repo",
            "head-ref" => head_ref = "arc/wrong",
            _ => unreachable!(),
        }
        let assert = repo
            .arc(&repo.root)
            .args([
                "forge",
                "link",
                "linker",
                "--pr",
                "1",
                "--url",
                "https://example.invalid/pr/1",
                "--base-repo",
                base_repo,
                "--base-ref",
                base_ref,
                "--head-repo",
                head_repo,
                "--head-ref",
                head_ref,
                "--head-sha",
                &head,
            ])
            .assert()
            .failure();
        assert_eq!(assert.get_output().status.code(), Some(10));
        assert_eq!(
            event_count(&repo, &change_id),
            before,
            "refused {axis} must not append an event"
        );
    }

    // The matching link succeeds and records the tuple.
    link_at(&repo, "linker", "1", &head, "arc/linker");
    assert_eq!(event_count(&repo, &change_id), before + 1);
    let status = status_json(&repo, "linker");
    assert_eq!(status["forge"]["projection"], "linked");
    assert_eq!(status["forge"]["link"]["pr_number"], 1);
    assert_eq!(status["forge"]["link"]["observed_head_sha"], head);
    assert_eq!(status["forge"]["observed_head_match"], true);
}

#[test]
fn forge_link_same_repository_only_refuses_cross_repo_tuple() {
    let repo = Repo::new();
    let (change_id, _wt, head) = forge_change(&repo, "cross");
    // Declare a cross-repo tuple but keep the default same-repository-only
    // policy: the declaration tuple matches but the policy refuses it.
    repo.arc(&repo.root)
        .args([
            "forge",
            "declare",
            "cross",
            "--host",
            "github.com",
            "--base-repo",
            "upstream-owner/example-repo",
            "--base-ref",
            "dev",
            "--head-repo",
            "example-owner/example-repo",
            "--head-ref",
            "arc/cross",
        ])
        .assert()
        .success();
    let before = event_count(&repo, &change_id);
    let assert = repo
        .arc(&repo.root)
        .args([
            "forge",
            "link",
            "cross",
            "--pr",
            "2",
            "--url",
            "https://github.com/upstream-owner/example-repo/pull/2",
            "--base-repo",
            "upstream-owner/example-repo",
            "--base-ref",
            "dev",
            "--head-repo",
            "example-owner/example-repo",
            "--head-ref",
            "arc/cross",
            "--head-sha",
            &head,
        ])
        .assert()
        .failure();
    assert_eq!(assert.get_output().status.code(), Some(10));
    assert_eq!(event_count(&repo, &change_id), before);
}

#[test]
fn forge_link_allowed_base_repo_accepts_target_and_refuses_others() {
    let repo = Repo::new();
    let (change_id, _wt, head) = forge_change(&repo, "allow");
    repo.arc(&repo.root)
        .args([
            "forge",
            "declare",
            "allow",
            "--host",
            "github.com",
            "--base-repo",
            "upstream-owner/example-repo",
            "--base-ref",
            "dev",
            "--head-repo",
            "example-owner/example-repo",
            "--head-ref",
            "arc/allow",
            "--policy",
            "allowed-base-repo=upstream-owner/example-repo",
        ])
        .assert()
        .success();

    // The declared base repo equals the allowed base repo: accepted.
    let before = event_count(&repo, &change_id);
    repo.arc(&repo.root)
        .args([
            "forge",
            "link",
            "allow",
            "--pr",
            "3",
            "--url",
            "https://github.com/upstream-owner/example-repo/pull/3",
            "--base-repo",
            "upstream-owner/example-repo",
            "--base-ref",
            "dev",
            "--head-repo",
            "example-owner/example-repo",
            "--head-ref",
            "arc/allow",
            "--head-sha",
            &head,
        ])
        .assert()
        .success();
    assert_eq!(event_count(&repo, &change_id), before + 1);

    // Re-declare with a different allowed base repo; the same observed
    // base repo is now refused.
    repo.arc(&repo.root)
        .args([
            "forge",
            "declare",
            "allow",
            "--host",
            "github.com",
            "--base-repo",
            "upstream-owner/example-repo",
            "--base-ref",
            "dev",
            "--head-repo",
            "example-owner/example-repo",
            "--head-ref",
            "arc/allow",
            "--policy",
            "allowed-base-repo=someone/else",
        ])
        .assert()
        .success();
    let before = event_count(&repo, &change_id);
    let assert = repo
        .arc(&repo.root)
        .args([
            "forge",
            "link",
            "allow",
            "--pr",
            "4",
            "--url",
            "https://github.com/upstream-owner/example-repo/pull/4",
            "--base-repo",
            "upstream-owner/example-repo",
            "--base-ref",
            "dev",
            "--head-repo",
            "example-owner/example-repo",
            "--head-ref",
            "arc/allow",
            "--head-sha",
            &head,
        ])
        .assert()
        .failure();
    assert_eq!(assert.get_output().status.code(), Some(10));
    assert_eq!(event_count(&repo, &change_id), before);
}

fn link_at(repo: &Repo, reference: &str, pr: &str, head: &str, head_ref: &str) {
    repo.arc(&repo.root)
        .args([
            "forge",
            "link",
            reference,
            "--pr",
            pr,
            "--url",
            "https://github.com/example-owner/example-repo/pull/1",
            "--base-repo",
            "example-owner/example-repo",
            "--base-ref",
            "dev",
            "--head-repo",
            "example-owner/example-repo",
            "--head-ref",
            head_ref,
            "--head-sha",
            head,
        ])
        .assert()
        .success();
}

#[test]
fn forge_checks_vocabulary_never_greens_zero_checks_and_marks_stale() {
    let repo = Repo::new();
    let (_id, _wt, head) = forge_change(&repo, "checks");
    declare_same_repo(&repo, "checks", "arc/checks");
    link_at(&repo, "checks", "1", &head, "arc/checks");

    // Zero-checks states are first-class and never render as passed.
    for state in [
        "not-configured",
        "not-triggered",
        "pending",
        "failed",
        "passed",
    ] {
        repo.arc(&repo.root)
            .args([
                "forge",
                "checks",
                "checks",
                "--pr-head",
                &head,
                "--state",
                state,
            ])
            .assert()
            .success();
        let status = status_json(&repo, "checks");
        assert_eq!(status["forge"]["checks"], state);
        if state != "passed" {
            assert_ne!(status["forge"]["checks"], "passed");
        }
    }

    // A rollup recorded for a different head than the linked one is stale.
    repo.arc(&repo.root)
        .args([
            "forge",
            "checks",
            "checks",
            "--pr-head",
            "0000000000000000000000000000000000000000",
            "--state",
            "passed",
        ])
        .assert()
        .success();
    let status = status_json(&repo, "checks");
    assert_eq!(status["forge"]["checks"], "stale");

    // With no checks event at the linked head the rollup is unknown.
    let (_id2, _wt2, head2) = forge_change(&repo, "checks2");
    declare_same_repo(&repo, "checks2", "arc/checks2");
    link_at(&repo, "checks2", "2", &head2, "arc/checks2");
    let status = status_json(&repo, "checks2");
    assert_eq!(status["forge"]["checks"], "unknown");
}

#[test]
fn forge_ready_truth_table() {
    let repo = Repo::new();
    let (_id, worktree, head) = forge_change(&repo, "ready");
    declare_same_repo(&repo, "ready", "arc/ready");
    link_at(&repo, "ready", "1", &head, "arc/ready");
    repo.arc(&repo.root)
        .args(["forge", "pr-state", "ready", "--state", "open"])
        .assert()
        .success();

    // passed + head_match + open => ready.
    repo.arc(&repo.root)
        .args([
            "forge",
            "checks",
            "ready",
            "--pr-head",
            &head,
            "--state",
            "passed",
        ])
        .assert()
        .success();
    let status = status_json(&repo, "ready");
    assert_eq!(status["forge"]["observed_head_match"], true);
    assert_eq!(status["forge"]["forge_ready"], true);

    // not-configured => ready, but with an explicit caveat.
    repo.arc(&repo.root)
        .args([
            "forge",
            "checks",
            "ready",
            "--pr-head",
            &head,
            "--state",
            "not-configured",
        ])
        .assert()
        .success();
    let status = status_json(&repo, "ready");
    assert_eq!(status["forge"]["forge_ready"], true);
    let caveats = status["forge"]["caveats"].as_array().unwrap();
    assert!(caveats
        .iter()
        .any(|caveat| caveat.as_str().unwrap().contains("not-configured")));

    // failed and pending block.
    for state in ["failed", "pending"] {
        repo.arc(&repo.root)
            .args([
                "forge",
                "checks",
                "ready",
                "--pr-head",
                &head,
                "--state",
                state,
            ])
            .assert()
            .success();
        assert_eq!(status_json(&repo, "ready")["forge"]["forge_ready"], false);
    }

    // Non-open pr_state blocks even with passed checks.
    repo.arc(&repo.root)
        .args([
            "forge",
            "checks",
            "ready",
            "--pr-head",
            &head,
            "--state",
            "passed",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["forge", "pr-state", "ready", "--state", "draft"])
        .assert()
        .success();
    assert_eq!(status_json(&repo, "ready")["forge"]["forge_ready"], false);
    repo.arc(&repo.root)
        .args(["forge", "pr-state", "ready", "--state", "open"])
        .assert()
        .success();

    // A head mismatch (new snapshot moves the approved head) blocks.
    repo.commit(&worktree, "ready2.txt", "more\n", "feat: more ready");
    stdout(repo.arc(&worktree).args(["snapshot", "ready"]));
    let status = status_json(&repo, "ready");
    assert_eq!(status["forge"]["observed_head_match"], false);
    assert_eq!(status["forge"]["forge_ready"], false);
}

#[test]
fn forge_pr_state_merged_requires_merge_sha() {
    let repo = Repo::new();
    let (_id, _wt, head) = forge_change(&repo, "merged");
    declare_same_repo(&repo, "merged", "arc/merged");
    link_at(&repo, "merged", "1", &head, "arc/merged");
    repo.arc(&repo.root)
        .args(["forge", "pr-state", "merged", "--state", "merged"])
        .assert()
        .failure()
        .code(1);
    repo.arc(&repo.root)
        .args([
            "forge",
            "pr-state",
            "merged",
            "--state",
            "merged",
            "--merge-sha",
            "abc123",
        ])
        .assert()
        .success();
    let status = status_json(&repo, "merged");
    assert_eq!(status["forge"]["pr_state"]["state"], "merged");
    assert_eq!(status["forge"]["pr_state"]["observed_merge_sha"], "abc123");
}

/// A lifecycle fact is an observation of one PR at one head. Pairing the
/// newest such fact with the newest link let an "open" read from a PR that was
/// since replaced speak for its replacement, which is how `forge_ready` could
/// go true for a PR nobody had looked at. After a relink the state is unknown
/// until it is observed again.
#[test]
fn pr_state_is_bound_to_link_and_head_after_relink() {
    let repo = Repo::new();
    let (_id, wt, head) = forge_change(&repo, "relink");
    declare_same_repo(&repo, "relink", "arc/relink");
    link_at(&repo, "relink", "1", &head, "arc/relink");
    repo.arc(&repo.root)
        .args(["forge", "pr-state", "relink", "--state", "open"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "forge",
            "checks",
            "relink",
            "--pr-head",
            &head,
            "--state",
            "passed",
        ])
        .assert()
        .success();
    let status = status_json(&repo, "relink");
    assert_eq!(status["forge"]["pr_state"]["state"], "open", "{status}");
    assert_eq!(status["forge"]["forge_ready"], true, "{status}");

    // A second PR at a new head: the old "open" describes the old PR, so the
    // current lifecycle state is unknown and readiness goes false.
    repo.commit(&wt, "more.txt", "more\n", "feat: more");
    stdout(repo.arc(&wt).args(["snapshot", "relink"]));
    let head2 = repo.head(&wt);
    link_at(&repo, "relink", "2", &head2, "arc/relink");
    repo.arc(&repo.root)
        .args([
            "forge",
            "checks",
            "relink",
            "--pr-head",
            &head2,
            "--state",
            "passed",
        ])
        .assert()
        .success();
    let status = status_json(&repo, "relink");
    assert!(status["forge"]["pr_state"].is_null(), "{status}");
    assert_eq!(status["forge"]["forge_ready"], false, "{status}");
    assert!(
        status["forge"]["caveats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|caveat| caveat.as_str().unwrap().contains("pr-state unknown")),
        "{status}"
    );

    // Observing the new PR restores readiness, and the fact binds to it.
    repo.arc(&repo.root)
        .args(["forge", "pr-state", "relink", "--state", "open"])
        .assert()
        .success();
    let status = status_json(&repo, "relink");
    assert_eq!(status["forge"]["pr_state"]["state"], "open", "{status}");
    assert_eq!(status["forge"]["forge_ready"], true, "{status}");

    // Naming a link that is not the current one is refused rather than
    // recorded as though it described the current PR.
    repo.arc(&repo.root)
        .args([
            "forge",
            "pr-state",
            "relink",
            "--state",
            "closed",
            "--link",
            "01SUPERSEDED",
        ])
        .assert()
        .failure();
}

/// The cases around the binding, which the relink test does not reach: a
/// second reading of the same PR must not invalidate what was observed, a
/// prefix shared by two links names neither, and a link recorded twice is not
/// a relink.
#[test]
fn pr_state_binding_survives_a_re_read_and_refuses_an_ambiguous_link() {
    let repo = Repo::new();
    let (_id, _wt, head) = forge_change(&repo, "rebind");
    declare_same_repo(&repo, "rebind", "arc/rebind");
    link_at(&repo, "rebind", "1", &head, "arc/rebind");
    repo.arc(&repo.root)
        .args(["forge", "pr-state", "rebind", "--state", "open"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "forge",
            "checks",
            "rebind",
            "--pr-head",
            &head,
            "--state",
            "passed",
        ])
        .assert()
        .success();

    // Reading the same PR again records the same link a second time. It is
    // one PR observed twice, not a relink, so the lifecycle fact still holds.
    link_at(&repo, "rebind", "1", &head, "arc/rebind");
    let status = status_json(&repo, "rebind");
    assert_eq!(status["forge"]["pr_state"]["state"], "open", "{status}");
    assert_eq!(status["forge"]["forge_ready"], true, "{status}");

    // An empty --link matches every link, so it names none.
    repo.arc(&repo.root)
        .args([
            "forge", "pr-state", "rebind", "--state", "closed", "--link", "",
        ])
        .assert()
        .failure();

    // A prefix shared by both recorded links names neither.
    repo.arc(&repo.root)
        .args([
            "forge", "pr-state", "rebind", "--state", "closed", "--link", "01",
        ])
        .assert()
        .failure();
    let status = status_json(&repo, "rebind");
    assert_eq!(status["forge"]["pr_state"]["state"], "open", "{status}");
}

#[test]
fn forge_held_and_linked_renders_awaiting_user() {
    let repo = Repo::new();
    let (_id, _wt, head) = forge_change(&repo, "awaiting");
    declare_same_repo(&repo, "awaiting", "arc/awaiting");
    link_at(&repo, "awaiting", "1", &head, "arc/awaiting");
    repo.arc(&repo.root)
        .args(["hold", "awaiting", "--reason", "keep personal PR open"])
        .assert()
        .success();
    let status = status_json(&repo, "awaiting");
    assert_eq!(
        status["forge"]["awaiting_user"]["pr_url"],
        "https://github.com/example-owner/example-repo/pull/1"
    );
    assert_eq!(status["forge"]["awaiting_user"]["observed_head_sha"], head);
    // The awaiting-user fact also shows in the Markdown Forge section.
    let shown = stdout(repo.arc(&repo.root).args(["show", "awaiting"]));
    assert!(shown.contains("Awaiting user"));
    assert!(shown.contains("https://github.com/example-owner/example-repo/pull/1"));
}

#[test]
fn forge_events_round_trip_through_export_import() {
    let source = Repo::new();
    let (change_id, _wt, head) = forge_change(&source, "roundtrip");
    declare_same_repo(&source, "roundtrip", "arc/roundtrip");
    link_at(&source, "roundtrip", "1", &head, "arc/roundtrip");
    source
        .arc(&source.root)
        .args([
            "forge",
            "checks",
            "roundtrip",
            "--pr-head",
            &head,
            "--state",
            "passed",
        ])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args(["forge", "pr-state", "roundtrip", "--state", "open"])
        .assert()
        .success();
    let source_status = status_json(&source, "roundtrip");

    let bundle = source.home.join("forge-bundle.json");
    source
        .arc(&source.root)
        .args(["export", "roundtrip", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let destination = Repo::new();
    deliver_git_objects(&source, &destination);
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();

    let imported_status: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["status", &change_id]),
    ))
    .unwrap();
    assert_eq!(imported_status["forge"], source_status["forge"]);
    assert_eq!(imported_status["forge"]["projection"], "linked");
    assert_eq!(imported_status["forge"]["checks"], "passed");
    assert_eq!(imported_status["forge"]["pr_state"]["state"], "open");
}

/// The typed consumers (show/status/check/watch) reduce the ledger through the
/// strongly typed Event enum. A change carrying an imported unknown-type event
/// must still replay: typed loading skips the unknown event while raw storage
/// and re-export preserve it byte-identically.
#[test]
fn typed_consumers_tolerate_imported_unknown_event_type() {
    let source = Repo::new();
    let (change_id, _wt, _head) = change_with_patchset(&source, "future-typed");
    // Inject an event whose payload type this build does not recognize. A full
    // envelope keeps only the event_type unknown; the ULID-max id sorts last.
    let event_id = "ZZZZZZZZZZZZZZZZZZZZZZZZZZ";
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(source.root.join(".git/arc/config.json")).unwrap())
            .unwrap();
    let unknown = serde_json::json!({
        "schema_version": 1,
        "event_id": event_id,
        "repository_id": config["repository_id"],
        "change_id": change_id,
        "actor": "future-agent",
        "created_at": "2999-01-01T00:00:00Z",
        "event_type": "quantum-entangled",
        "future_payload": {"kept": [1, 2, 3], "nested": true}
    });
    let unknown_bytes = json_file_bytes(&unknown);
    fs::write(
        event_dir(&source, &change_id).join(format!("{event_id}.json")),
        &unknown_bytes,
    )
    .unwrap();

    let bundle = source.home.join("future-typed.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "future-typed",
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();

    let dest = Repo::new();
    deliver_git_objects(&source, &dest);
    dest.arc(&dest.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();

    // Typed consumers must succeed instead of failing to parse the unknown event.
    dest.arc(&dest.root)
        .args(["show", &change_id])
        .assert()
        .success();
    dest.arc(&dest.root)
        .args(["status", &change_id])
        .assert()
        .success();
    dest.arc(&dest.root)
        .args(["check", &change_id])
        .assert()
        .stderr(predicates::str::contains("malformed event file").not())
        .stderr(predicates::str::contains("unknown variant").not());
    // The patchset is present in the ledger, so this condition is immediately
    // satisfied and watch (also a typed consumer) returns without waiting.
    dest.arc(&dest.root)
        .args(["watch", &change_id, "--until", "snapshot", "--timeout", "5"])
        .assert()
        .success();

    // Storage kept the unknown event byte-identically, and re-export round-trips it.
    let streamed = stdout(dest.arc(&dest.root).args([
        "events",
        "--change",
        &change_id,
        "--type",
        "quantum-entangled",
    ]));
    let stored: serde_json::Value = serde_json::from_str(streamed.trim()).unwrap();
    assert_eq!(
        stored,
        serde_json::from_slice::<serde_json::Value>(&unknown_bytes).unwrap()
    );
    let reexport = dest.home.join("reexport.json");
    dest.arc(&dest.root)
        .args(["export", &change_id, "--output", reexport.to_str().unwrap()])
        .assert()
        .success();
    let reexported: serde_json::Value =
        serde_json::from_slice(&fs::read(&reexport).unwrap()).unwrap();
    let found = reexported["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_id"] == event_id)
        .expect("unknown event survives re-export");
    assert_eq!(*found, unknown);
}

#[test]
fn forge_rewrite_preserves_observed_shas_until_real_readback() {
    use super::rewrite::{git_signing, signing_key};
    let repo = Repo::new();
    let key = signing_key(&repo).expect("forge successor requires a fixture signing key");
    let (_id, wt, a) = forge_change(&repo, "observed");
    declare_same_repo(&repo, "observed", "arc/observed");
    link_at(&repo, "observed", "1", &a, "arc/observed");
    stdout(repo.arc(&wt).args([
        "forge",
        "checks",
        "observed",
        "--pr-head",
        &a,
        "--state",
        "passed",
    ]));
    git_signing(
        &wt,
        &key,
        &[
            "-c",
            &format!("user.signingkey={}", key.fingerprint),
            "commit",
            "--amend",
            "-S",
            "--no-edit",
        ],
    );
    git_signing(&wt, &key, &["verify-commit", "HEAD"]);
    let b = repo.head(&wt);
    assert_ne!(a, b);
    let map = repo.home.join("forge-map");
    fs::write(&map, format!("{a} {b}\n")).unwrap();
    stdout(repo.arc(&wt).args([
        "history",
        "rewrite",
        "--map",
        map.to_str().unwrap(),
        "--reason",
        "signed successor",
    ]));
    for _ in 0..2 {
        let shown: serde_json::Value =
            serde_json::from_str(&stdout(repo.arc(&wt).args(["show", "observed", "--json"])))
                .unwrap();
        assert_eq!(shown["forge"]["link"]["observed_head_sha"], a);
        assert_eq!(shown["forge"]["link"]["resolved_head_sha"], b);
        assert_eq!(shown["forge"]["checks"]["observed_pr_head"], a);
        assert_eq!(shown["forge"]["checks"]["resolved_pr_head"], b);
        let status = status_json(&repo, "observed");
        assert_eq!(status["forge"]["observed_head_match"], false);
        assert_eq!(status["forge"]["resolved_head_match"], true);
        assert_eq!(status["forge"]["checks_applicable"], false);
        assert_eq!(status["forge"]["forge_ready"], false);
        link_at(&repo, "observed", "1", &a, "arc/observed");
    }
    stdout(repo.arc(&wt).args([
        "forge",
        "pr-state",
        "observed",
        "--state",
        "merged",
        "--merge-sha",
        &a,
    ]));
    let shown: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["show", "observed", "--json"]))).unwrap();
    let lifecycle = &shown["forge"]["pr_states"][0];
    assert_eq!(lifecycle["observed_pr_head"], a);
    assert_eq!(lifecycle["resolved_pr_head"], b);
    assert_eq!(lifecycle["observed_merge_sha"], a);
    assert_eq!(lifecycle["resolved_merge_sha"], b);
    let text = stdout(repo.arc(&wt).args(["show", "observed"]));
    assert!(text.contains(&a) && text.contains(&b), "{text}");
    link_at(&repo, "observed", "1", &b, "arc/observed");
    assert_eq!(status_json(&repo, "observed")["forge"]["checks"], "stale");
    stdout(repo.arc(&wt).args([
        "forge",
        "checks",
        "observed",
        "--pr-head",
        &b,
        "--state",
        "passed",
    ]));
    stdout(
        repo.arc(&wt)
            .args(["forge", "pr-state", "observed", "--state", "open"]),
    );
    let status = status_json(&repo, "observed");
    assert_eq!(status["forge"]["checks"], "passed");
    assert_eq!(status["forge"]["checks_applicable"], true);
    assert_eq!(status["forge"]["observed_head_match"], true);
    assert_eq!(status["forge"]["resolved_head_match"], true);
    assert_eq!(status["forge"]["forge_ready"], true);
    let shown: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["show", "observed", "--json"]))).unwrap();
    assert_eq!(shown["forge"]["links"][0]["observed_head_sha"], a);
    assert_eq!(shown["forge"]["link"]["observed_head_sha"], b);
    assert_eq!(shown["forge"]["pr_states"][1]["observed_pr_head"], b);
    link_at(&repo, "observed", "2", &b, "arc/observed");
    assert_eq!(status_json(&repo, "observed")["forge"]["checks"], "stale");
}

#[test]
fn forge_dropped_revision_has_null_resolved_successor() {
    let repo = Repo::new();
    let (_id, wt, a) = forge_change(&repo, "dropped-forge");
    declare_same_repo(&repo, "dropped-forge", "arc/dropped-forge");
    link_at(&repo, "dropped-forge", "1", &a, "arc/dropped-forge");
    stdout(repo.arc(&wt).args([
        "forge",
        "checks",
        "dropped-forge",
        "--pr-head",
        &a,
        "--state",
        "passed",
    ]));
    stdout(repo.arc(&wt).args([
        "forge",
        "pr-state",
        "dropped-forge",
        "--state",
        "merged",
        "--merge-sha",
        &a,
    ]));
    let map = repo.home.join("drop-map");
    fs::write(&map, format!("{a} {}\n", "0".repeat(40))).unwrap();
    stdout(repo.arc(&wt).args([
        "history",
        "rewrite",
        "--map",
        map.to_str().unwrap(),
        "--reason",
        "dropped revision",
    ]));
    let shown: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&wt).args([
        "show",
        "dropped-forge",
        "--json",
    ])))
    .unwrap();
    for (record, observed, resolved) in [
        (
            &shown["forge"]["link"],
            "observed_head_sha",
            "resolved_head_sha",
        ),
        (
            &shown["forge"]["checks"],
            "observed_pr_head",
            "resolved_pr_head",
        ),
        (
            &shown["forge"]["pr_states"][0],
            "observed_pr_head",
            "resolved_pr_head",
        ),
        (
            &shown["forge"]["pr_states"][0],
            "observed_merge_sha",
            "resolved_merge_sha",
        ),
    ] {
        assert_eq!(record[observed], a);
        assert!(record.as_object().unwrap().contains_key(resolved));
        assert!(record[resolved].is_null());
    }
    assert_eq!(
        status_json(&repo, "dropped-forge")["forge"]["resolved_head_match"],
        false
    );
}

#[test]
fn forge_checks_do_not_cross_pr_identity_at_same_head() {
    let repo = Repo::new();
    let (_id, wt, head) = forge_change(&repo, "check-identity");
    declare_same_repo(&repo, "check-identity", "arc/check-identity");
    link_at(&repo, "check-identity", "1", &head, "arc/check-identity");
    stdout(repo.arc(&wt).args([
        "forge",
        "checks",
        "check-identity",
        "--pr-head",
        &head,
        "--state",
        "passed",
    ]));
    link_at(&repo, "check-identity", "2", &head, "arc/check-identity");
    assert_eq!(
        status_json(&repo, "check-identity")["forge"]["checks"],
        "stale"
    );
}

fn import_interleaved_checks(legacy: bool, linked: bool) {
    let source = Repo::new();
    let (id, wt, head) = forge_change(&source, "interleaved-checks");
    declare_same_repo(&source, &id, "arc/interleaved-checks");
    if linked {
        link_at(&source, &id, "1", &head, "arc/interleaved-checks");
    }
    let seed = source.home.join("seed.json");
    stdout(
        source
            .arc(&source.root)
            .args(["export", &id, "--output", seed.to_str().unwrap()]),
    );
    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    stdout(
        replica
            .arc(&replica.root)
            .args(["import", seed.to_str().unwrap()]),
    );
    link_at(&replica, &id, "2", &head, "arc/interleaved-checks");
    stdout(
        replica
            .arc(&replica.root)
            .args(["forge", "pr-state", &id, "--state", "open"]),
    );
    stdout(source.arc(&wt).args([
        "forge",
        "checks",
        &id,
        "--pr-head",
        &head,
        "--state",
        "passed",
    ]));
    if legacy {
        rewrite_event(&source, &id, "forge-checks", |e| {
            e.as_object_mut().unwrap().remove("link_event_id");
            e.as_object_mut().unwrap().remove("binding_recorded");
        });
        if linked {
            assert_eq!(status_json(&source, &id)["forge"]["checks"], "passed");
        }
    }
    let bundle = source.home.join("checks.json");
    stdout(
        source
            .arc(&source.root)
            .args(["export", &id, "--output", bundle.to_str().unwrap()]),
    );
    stdout(
        replica
            .arc(&replica.root)
            .args(["import", bundle.to_str().unwrap()]),
    );
    let status = status_json(&replica, &id);
    assert_eq!(status["forge"]["link"]["pr_number"], 2);
    assert_eq!(status["forge"]["pr_state"]["state"], "open");
    assert_eq!(status["forge"]["checks_applicable"], false, "{status}");
    assert_eq!(status["forge"]["forge_ready"], false, "{status}");
    let reexport = replica.home.join("reexport.json");
    stdout(replica.arc(&replica.root).args([
        "export",
        &id,
        "--output",
        reexport.to_str().unwrap(),
    ]));
    let third = Repo::new();
    deliver_git_objects(&source, &third);
    stdout(
        third
            .arc(&third.root)
            .args(["import", reexport.to_str().unwrap()]),
    );
    assert_eq!(
        status_json(&third, &id)["forge"]["checks_applicable"],
        false
    );
}

#[test]
fn forge_import_interleaving_preserves_explicit_checks_binding() {
    import_interleaved_checks(false, true);
}

#[test]
fn forge_import_interleaving_does_not_infer_ambiguous_legacy_checks_binding() {
    import_interleaved_checks(true, true);
}

#[test]
fn forge_unlinked_checks_cannot_acquire_an_imported_pr_binding() {
    import_interleaved_checks(false, false);
}

#[test]
fn forge_content_rewrite_compares_observations_to_approval_anchor() {
    let repo = Repo::new();
    let (id, wt, a) = forge_change(&repo, "approval-anchor");
    declare_same_repo(&repo, &id, "arc/approval-anchor");
    fs::write(wt.join("approval-anchor.txt"), "different content\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "--amend", "--no-edit"]);
    let b = repo.head(&wt);
    let map = repo.home.join("content-map");
    fs::write(&map, format!("{a} {b}\n")).unwrap();
    stdout(repo.arc(&wt).args([
        "history",
        "rewrite",
        "--map",
        map.to_str().unwrap(),
        "--reason",
        "content rewrite",
    ]));
    let shown: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["show", &id, "--json"]))).unwrap();
    assert_eq!(shown["patchsets"][0]["head"], b);
    assert_ne!(a, b);
    link_at(&repo, &id, "1", &b, "arc/approval-anchor");
    stdout(
        repo.arc(&wt)
            .args(["forge", "checks", &id, "--pr-head", &b, "--state", "passed"]),
    );
    stdout(
        repo.arc(&wt)
            .args(["forge", "pr-state", &id, "--state", "open"]),
    );
    let status = status_json(&repo, &id);
    assert_eq!(status["forge"]["observed_head_match"], false, "{status}");
    assert_eq!(status["forge"]["checks_applicable"], false, "{status}");
    assert_eq!(status["forge"]["forge_ready"], false, "{status}");
    stdout(repo.arc(&wt).args(["snapshot", &id]));
    assert_eq!(status_json(&repo, &id)["forge"]["forge_ready"], true);
}

#[test]
fn forge_legacy_lifecycle_cannot_acquire_a_foreign_link_on_import() {
    let source = Repo::new();
    let (id, wt, head) = forge_change(&source, "legacy-lifecycle");
    declare_same_repo(&source, &id, "arc/legacy-lifecycle");
    let seed = source.home.join("seed.json");
    stdout(
        source
            .arc(&source.root)
            .args(["export", &id, "--output", seed.to_str().unwrap()]),
    );
    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    stdout(
        replica
            .arc(&replica.root)
            .args(["import", seed.to_str().unwrap()]),
    );
    link_at(&replica, &id, "2", &head, "arc/legacy-lifecycle");
    stdout(replica.arc(&replica.root).args([
        "forge",
        "checks",
        &id,
        "--pr-head",
        &head,
        "--state",
        "passed",
    ]));
    stdout(source.arc(&wt).args([
        "forge",
        "checks",
        &id,
        "--pr-head",
        &head,
        "--state",
        "pending",
    ]));
    rewrite_event(&source, &id, "forge-checks", |e| {
        e["event_type"] = serde_json::json!("forge-pr-state");
        e["state"] = serde_json::json!("open");
        for field in ["pr_head", "detail", "link_event_id", "observation"] {
            e.as_object_mut().unwrap().remove(field);
        }
    });
    let bundle = source.home.join("lifecycle.json");
    stdout(
        source
            .arc(&source.root)
            .args(["export", &id, "--output", bundle.to_str().unwrap()]),
    );
    stdout(
        replica
            .arc(&replica.root)
            .args(["import", bundle.to_str().unwrap()]),
    );
    let status = status_json(&replica, &id);
    assert!(status["forge"]["pr_state"].is_null(), "{status}");
    assert_eq!(status["forge"]["forge_ready"], false, "{status}");
}
