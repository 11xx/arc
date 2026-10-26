use crate::common::*;

fn arc(repo: &Repo, cwd: &Path) -> AssertCommand {
    match std::env::var_os("ARC_GATE_TIERS_BINARY") {
        Some(binary) => repo.fixture_env(AssertCommand::new(binary), cwd),
        None => repo.arc(cwd),
    }
}

fn declaring(text: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(repo.root.join(".arc/gates.toml"), text).unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare event gates"]);
    repo
}

fn change(repo: &Repo, slug: &str) -> PathBuf {
    arc(repo, &repo.root)
        .args(["begin", slug])
        .assert()
        .success();
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&wt, "feature", "one\n", "feat: fixture");
    arc(repo, &wt).args(["snapshot", slug]).assert().success();
    wt
}

/// Integration must select its own tier; a failing publication gate must not
/// hold an otherwise green integration.
#[test]
fn integration_omits_publication_only_gates() {
    let repo = declaring("[gates.fast]\ncommand='true'\nevents=['integration']\n[gates.full]\ncommand='false'\nevents=['publication']\n");
    let wt = change(&repo, "partition");
    arc(&repo, &wt)
        .args(["verify", "--all"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gates: 1/1 pass"));
    arc(&repo, &wt)
        .args(["verify", "--all", "--event", "publication"])
        .assert()
        .code(1);
    let status = json_stdout(arc(&repo, &wt).args(["status", "--json"]));
    assert_eq!(status["gates"].as_array().unwrap().len(), 1, "{status}");
    assert_eq!(status["gates"][0]["name"], "fast");
    assert_eq!(status["gates"][0]["green_at_head"], true);
}

/// A gate applicable to both events needs a publication run even when the
/// integration run passed at the same tree.
#[test]
fn publication_does_not_reuse_integration_passes() {
    let repo = declaring("[gates.check]\ncommand='true'\n");
    let wt = change(&repo, "identity");
    arc(&repo, &wt).args(["verify", "--all"]).assert().success();
    arc(&repo, &wt)
        .args(["verify", "--all", "--event", "publication", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped").not());
    arc(&repo, &wt)
        .args(["verify", "--all", "--event", "publication", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped"));
    arc(&repo, &wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped"));
}

/// A complete project mapping can reuse a passing source tree across a
/// content delta; the receipt must retain both the original evidence and
/// the selection that made it applicable.
#[test]
fn project_selection_records_delta_lineage_and_avoids_a_rerun() {
    let selector = "cat >/dev/null; printf '%s' '{\"schema\":\"arc-gate-selection/1\",\"complete\":true,\"gates\":[]}'";
    let repo = declaring(&format!(
        "[gates.check]\ncommand='true'\nselector={}\n",
        serde_json::to_string(selector).unwrap()
    ));
    let wt = change(&repo, "selected");
    arc(&repo, &wt).args(["verify", "--all"]).assert().success();
    let source_tree = git_out(&wt, &["rev-parse", "HEAD^{tree}"]);
    repo.commit(&wt, "feature", "two\n", "feat: next fixture");
    arc(&repo, &wt).args(["snapshot"]).assert().success();
    arc(&repo, &wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "project selector proves unchanged scope",
        ));
    let state = json_stdout(arc(&repo, &wt).args(["show", "--json"]));
    let selected = state["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["delta"].is_object())
        .expect("derived gate receipt");
    assert_eq!(selected["delta"]["source_tree"], source_tree);
    assert_ne!(
        selected["delta"]["source_tree"],
        selected["delta"]["target_tree"]
    );
    assert_eq!(
        selected["delta"]["changed_paths"],
        serde_json::json!(["feature"])
    );
    let status = json_stdout(arc(&repo, &wt).args(["status", "--json"]));
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
}

/// An unknown selector answer does not prove coverage: the gate runs in full.
#[test]
fn incomplete_or_unknown_project_selection_runs_the_gate() {
    for (index, answer) in [
        "{\"schema\":\"arc-gate-selection/1\",\"complete\":false,\"gates\":[]}",
        "{\"schema\":\"arc-gate-selection/1\",\"complete\":true,\"gates\":[\"missing\"]}",
        "unavailable",
    ]
    .iter()
    .enumerate()
    {
        let selector = format!("cat >/dev/null; printf '%s' '{}'", answer);
        let repo = declaring(&format!(
            "[gates.check]\ncommand='true'\nselector={}\n",
            serde_json::to_string(&selector).unwrap()
        ));
        let wt = change(&repo, &format!("fallback-{index}"));
        arc(&repo, &wt).args(["verify", "--all"]).assert().success();
        repo.commit(&wt, "feature", "two\n", "feat: changed fixture");
        arc(&repo, &wt).args(["snapshot"]).assert().success();
        arc(&repo, &wt)
            .args(["verify", "--all", "--skip-green"])
            .assert()
            .success()
            .stdout(predicates::str::contains("skipped").not());
    }
}

/// A local publication guard checks the target's exact content in a scratch
/// checkout and retains a separate full-pass receipt.
#[test]
fn publication_guard_checks_the_target_and_reuses_only_its_own_full_pass() {
    let repo = declaring("[gates.check]\ncommand='test -f README.md'\nevents=['publication']\n");
    arc(&repo, &repo.root)
        .args(["publication", "--target", "master"])
        .assert()
        .success()
        .stdout(predicates::str::contains("publication event:"));
    arc(&repo, &repo.root)
        .args(["publication", "--target", "master", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("publication pass at target tree"));
    let wt = change(&repo, "publication-batch");
    repo.commit(
        &wt,
        ".arc/gates.toml",
        "[gates.check]\ncommand='false'\nevents=['publication']\n",
        "test: failing publication",
    );
    arc(&repo, &wt).args(["snapshot"]).assert().success();
    let opened = json_stdout(arc(&repo, &wt).args(["show", "--json"]));
    let id = opened["change_id"].as_str().unwrap();
    arc(&repo, &repo.root)
        .args(["integrate", id, "--debt", "Fixture retains missing review"])
        .assert()
        .success();
    arc(&repo, &repo.root)
        .args(["publication", "--target", "master"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains(
            "integrations since last successful publication pass:",
        ))
        .stdout(predicates::str::contains(id));
}

/// Replaying a change onto a moved target evaluates the merge delta, rather
/// than paying a full gate when the project maps that delta to no checks.
#[test]
fn project_selection_applies_to_the_synthesized_merge_tree() {
    let selector = "cat >/dev/null; printf '%s' '{\"schema\":\"arc-gate-selection/1\",\"complete\":true,\"gates\":[]}'";
    let repo = declaring(&format!(
        "[gates.check]\ncommand='true'\nselector={}\n",
        serde_json::to_string(selector).unwrap()
    ));
    let wt = change(&repo, "merge-selection");
    arc(&repo, &wt).args(["verify", "--all"]).assert().success();
    repo.commit(&repo.root, "other", "target\n", "feat: target delta");
    arc(&repo, &wt)
        .args(["verify", "--against", "master", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "project selector proves unchanged scope",
        ));
    let state = json_stdout(arc(&repo, &wt).args(["show", "--json"]));
    let selected = state["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["delta"].is_object())
        .unwrap();
    assert_eq!(
        selected["delta"]["changed_paths"],
        serde_json::json!(["other"])
    );
    assert_eq!(selected["against_target"], repo.head(&repo.root));
}

/// A selector operating on dirty source cannot transfer a clean pass from
/// another tree. Its answer must leave the gate to execute and be judged
/// with the observed dirty-tree evidence.
#[test]
fn a_selector_cannot_create_green_evidence_after_mutating_the_source() {
    let selector = "cat >/dev/null; printf dirty > feature; printf '%s' '{\"schema\":\"arc-gate-selection/1\",\"complete\":true,\"gates\":[]}'";
    let repo = declaring(&format!(
        "[gates.check]\ncommand='true'\nselector={}\n",
        serde_json::to_string(selector).unwrap()
    ));
    let wt = change(&repo, "dirty-selection");
    arc(&repo, &wt).args(["verify", "--all"]).assert().success();
    repo.commit(&wt, "feature", "two\n", "feat: next fixture");
    arc(&repo, &wt).args(["snapshot"]).assert().success();
    arc(&repo, &wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped").not());
    let state = json_stdout(arc(&repo, &wt).args(["show", "--json"]));
    assert!(state["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| !entry["delta"].is_object()));
    let status = json_stdout(arc(&repo, &wt).args(["status", "--json"]));
    assert_eq!(status["gates"][0]["green_at_head"], false, "{status}");
}

/// Publication history carries its tested Git objects and restores only
/// dedicated publication pins when a change is transported to another store.
#[test]
fn publication_receipts_transport_objects_and_refuse_foreign_ref_names() {
    let source = declaring("[gates.check]\ncommand='true'\nevents=['publication']\n");
    arc(&source, &source.root)
        .args(["publication", "--target", "master"])
        .assert()
        .success();
    let wt = change(&source, "transport-publication");
    let bundle_file = source.home.join("publication-bundle.json");
    arc(&source, &wt)
        .args([
            "export",
            "transport-publication",
            "--output",
            bundle_file.to_str().unwrap(),
        ])
        .assert()
        .success();
    let bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle_file).unwrap()).unwrap();
    let receipt = bundle["repository_events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "publication-verified")
        .unwrap();
    let revision = receipt["revision"].as_str().unwrap();
    let pin = receipt["retention_ref"].as_str().unwrap();
    assert!(bundle["objects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|object| object["oid"] == revision && object["role"] == "retention-commit"));
    assert_eq!(git_out(&source.root, &["rev-parse", pin]), revision);
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    arc(&receiver, &receiver.root)
        .args(["import", bundle_file.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(git_out(&receiver.root, &["rev-parse", pin]), revision);

    let mut malformed = bundle;
    malformed["repository_events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "publication-verified")
        .unwrap()["retention_ref"] = serde_json::json!("refs/heads/master");
    let malformed_file = source.home.join("foreign-publication-pin.json");
    fs::write(&malformed_file, serde_json::to_vec(&malformed).unwrap()).unwrap();
    let other = Repo::new();
    deliver_git_objects(&source, &other);
    let old = other.head(&other.root);
    arc(&other, &other.root)
        .args(["import", malformed_file.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "publication receipt names no publication retention ref",
        ));
    assert_eq!(other.head(&other.root), old);
    assert!(!other.root.join(".git/arc").exists());
}

/// A pre-push or release caller names the commit it will publish; a moved
/// target must refuse before creating a receipt for another revision.
#[test]
fn publication_fences_the_callers_exact_revision() {
    let repo = declaring("[gates.check]\ncommand='true'\nevents=['publication']\n");
    let queued = repo.head(&repo.root);
    repo.commit(&repo.root, "feature", "new head\n", "feat: target moves");
    arc(&repo, &repo.root)
        .args(["publication", "--target", "master", "--revision", &queued])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not the requested revision"));
    assert!(!repo.root.join(".git/arc").exists());
    let current = repo.head(&repo.root);
    arc(&repo, &repo.root)
        .args(["publication", "--target", "master", "--revision", &current])
        .assert()
        .success();
}

/// A publication pass must describe the gate declarations and environment
/// still in force when the guard returns, even when a gate changes them.
#[test]
fn publication_rechecks_declarations_and_environment_before_passing() {
    let mut wrong_passes = Vec::new();
    for kind in ["declarations", "environment"] {
        let repo = Repo::new();
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        let (command, environment) = if kind == "declarations" {
            let operator = repo.root.join(".git/arc/operator-policy.toml");
            let declaration = "[gates.extra]\ncommand='false'\nevents=['publication']\n";
            (
                format!(
                    "printf %s {} > {}",
                    quote(declaration),
                    quote(operator.to_str().unwrap())
                ),
                String::new(),
            )
        } else {
            let marker = repo.home.join("environment");
            fs::write(&marker, "before").unwrap();
            (
                format!("printf after > {}", quote(marker.to_str().unwrap())),
                format!(
                    "environment={}\n",
                    serde_json::to_string(&format!("cat {}", quote(marker.to_str().unwrap())))
                        .unwrap()
                ),
            )
        };
        fs::create_dir_all(repo.root.join(".arc")).unwrap();
        fs::write(
            repo.root.join(".arc/gates.toml"),
            format!(
                "[gates.check]\ncommand={}\nevents=['publication']\n{environment}",
                serde_json::to_string(&command).unwrap()
            ),
        )
        .unwrap();
        git(&repo.root, &["add", ".arc/gates.toml"]);
        git(
            &repo.root,
            &["commit", "-m", "test: changing publication context"],
        );
        let result = arc(&repo, &repo.root)
            .args(["publication", "--target", "master"])
            .output()
            .unwrap();
        if result.status.success() {
            wrong_passes.push(kind);
        }
    }
    assert!(
        wrong_passes.is_empty(),
        "publication returned a false pass after changing {wrong_passes:?}"
    );
}

/// A successful selector cannot leave owned background writers running
/// after its receipt is checked and used to authorize reuse.
#[test]
fn selector_descendants_stop_before_source_stability_is_checked() {
    for redirect in [true, false] {
        let redirection = if redirect { ">/dev/null 2>&1 " } else { "" };
        let selector = format!("(sleep 0.3; printf dirty > feature) {redirection}& cat >/dev/null; printf '%s' '{{\"schema\":\"arc-gate-selection/1\",\"complete\":true,\"gates\":[]}}'");
        let repo = declaring(&format!(
            "[gates.check]\ncommand='true'\nselector={}\n",
            serde_json::to_string(&selector).unwrap()
        ));
        let wt = change(&repo, "selector-descendants");
        arc(&repo, &wt).args(["verify", "--all"]).assert().success();
        repo.commit(&wt, "feature", "two\n", "feat: next fixture");
        arc(&repo, &wt).args(["snapshot"]).assert().success();
        arc(&repo, &wt)
            .args(["verify", "--all", "--skip-green"])
            .assert()
            .success()
            .stdout(predicates::str::contains(
                "project selector proves unchanged scope",
            ));
        thread::sleep(Duration::from_millis(600));
        assert_eq!(fs::read_to_string(wt.join("feature")).unwrap(), "two\n",
            "selector left an owned writer after accepting its answer (redirected pipes: {redirect})");
    }
}
