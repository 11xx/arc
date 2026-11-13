use crate::common::*;
use serde_json::{json, Value};

const SCOPED: &str = "[selection]\ninert=['**/*.md']\n[gates.left]\ncommand='true'\npaths=['left/']\n[gates.right]\ncommand='true'\npaths=['right/']\n[gates.all]\ncommand='true'\n";

const EMPTY_SELECTION: &str = "python3 -c 'import json,sys; r=json.load(sys.stdin); print(json.dumps(dict(schema=\"arc-gate-selection/1\",complete=True,request=r,gates=[])))'";

fn commit_path(repo: &Repo, wt: &Path, file: &str, content: &str, message: &str) {
    fs::create_dir_all(wt.join(file).parent().unwrap()).unwrap();
    repo.commit(wt, file, content, message);
}

fn fixture(text: &str) -> (Repo, String, PathBuf) {
    let repo = Repo::new();
    repo.redeclare_gates(text);
    let (id, wt, _) = change_with_patchset(&repo, "coverage");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    (repo, id, wt)
}

fn selected(repo: &Repo, wt: &Path) -> Vec<Value> {
    let state = json_stdout(repo.arc(wt).args(["show", "--json"]));
    state["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["delta"].is_object())
        .cloned()
        .collect()
}

fn verify(repo: &Repo, wt: &Path) {
    repo.arc(wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success();
}

/// Claim: a rebase touching another gate's paths keeps the unaffected pass,
/// with a replayable arc selection receipt. Rejects executing every gate again
/// or letting a gate without paths skip a non-inert change.
#[test]
fn rebase_preserves_only_unaffected_passes() {
    let (repo, _, wt) = fixture(SCOPED);
    commit_path(
        &repo,
        &repo.root,
        "right/new.rs",
        "target\n",
        "test: right path",
    );
    git(&wt, &["rebase", "master"]);
    repo.arc(&wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gate left: skipped (arc paths"))
        .stdout(predicates::str::contains("gate right: skipped").not())
        .stdout(predicates::str::contains("gate all: skipped").not());
    let receipts = selected(&repo, &wt);
    assert_eq!(
        receipts.len(),
        1,
        "only left is outside the delta: {receipts:?}"
    );
    let delta = &receipts[0]["delta"];
    assert_eq!(delta["selector"], "arc:paths");
    assert_eq!(delta["changed_paths"], json!(["right/new.rs"]));
    assert_eq!(delta["selected_gates"], json!(["all", "right"]));
    let answer: Value = serde_json::from_str(delta["output"].as_str().unwrap()).unwrap();
    assert_eq!(answer["request"]["paths"], delta["changed_paths"]);
    let status = json_stdout(repo.arc(&wt).args(["status", "--json"]));
    assert!(
        status["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gate| gate["green_at_head"] == true),
        "{status}"
    );
    verify(&repo, &wt);
    assert_eq!(
        selected(&repo, &wt).len(),
        1,
        "same-tree reuse preserves the original receipt"
    );
}

/// Claim: unnamed paths run every gate even when a script claims no effect.
/// Rejects the open-world fallback silently skipping a required gate.
#[test]
fn unnamed_paths_cannot_skip_gates() {
    let gates = format!(
        "{SCOPED}\n[gates.script]\ncommand='true'\npaths=['script/']\nselector={}\n",
        serde_json::to_string(EMPTY_SELECTION).unwrap()
    );
    let (repo, _, wt) = fixture(&gates);
    commit_path(
        &repo,
        &wt,
        "unclaimed.xyz",
        "unknown\n",
        "test: unknown path",
    );
    verify(&repo, &wt);
    assert!(
        selected(&repo, &wt).is_empty(),
        "unnamed path must run every gate"
    );
}

/// Claim: danger overrides inert patterns and gate paths, including script
/// answers. Rejects selecting out a gate for a dangerous Markdown file.
#[test]
fn dangerous_paths_cannot_skip_gates() {
    let repo = Repo::new();
    repo.redeclare_gates(&format!(
        "{SCOPED}\n[gates.script]\ncommand='true'\npaths=['script/']\nselector={}\n",
        serde_json::to_string(EMPTY_SELECTION).unwrap()
    ));
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[danger]\npaths=['**/risk.md']\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare danger"]);
    let (_, wt, _) = change_with_patchset(&repo, "danger");
    verify(&repo, &wt);
    commit_path(
        &repo,
        &wt,
        "right/risk.md",
        "danger\n",
        "test: dangerous inert path",
    );
    verify(&repo, &wt);
    assert!(
        selected(&repo, &wt).is_empty(),
        "danger must run every gate"
    );
}

/// Claim: inert paths affect no gate, including one with no paths, and path
/// selection does not execute a selector for an unaffected gate. Rejects paying
/// either the script or gate command for an inert delta.
#[test]
fn inert_paths_skip_all_gates_before_scripts() {
    let gates = format!(
        "{SCOPED}\n[gates.script]\ncommand='true'\npaths=['script/']\nselector='exit 91'\n"
    );
    let (repo, _, wt) = fixture(&gates);
    commit_path(&repo, &wt, "nested/notes.md", "docs\n", "test: inert path");
    verify(&repo, &wt);
    assert_eq!(
        selected(&repo, &wt).len(),
        4,
        "every gate must select out before its script"
    );
}

/// Claim: a declared script may refine an affected known path. Rejects ignoring
/// the script once declarative coverage exists.
#[test]
fn affected_paths_ask_the_declared_selector() {
    let gates = format!(
        "[gates.check]\ncommand='true'\npaths=['src/']\nselector={}\n",
        serde_json::to_string(EMPTY_SELECTION).unwrap()
    );
    let (repo, _, wt) = fixture(&gates);
    commit_path(
        &repo,
        &wt,
        "src/known.rs",
        "known\n",
        "test: affected known path",
    );
    verify(&repo, &wt);
    assert_eq!(
        selected(&repo, &wt).len(),
        1,
        "script proves the known path unaffected"
    );
}

/// Claim: path and inert adoption invalidate existing evidence, including a
/// same-tree pass and legacy receipts without identity. Rejects borrowing a
/// pass across coverage declarations before obtaining a fresh pass.
#[test]
fn coverage_adoption_requires_fresh_identity_bound_evidence() {
    for ((before, after), legacy) in [
        (
            "[gates.check]\ncommand='true'\n",
            "[gates.check]\ncommand='true'\npaths=['src/']\n",
        ),
        (
            "[gates.check]\ncommand='true'\n",
            "[selection]\ninert=['**/*.md']\n[gates.check]\ncommand='true'\n",
        ),
    ]
    .into_iter()
    .flat_map(|pair| [(pair, false), (pair, true)])
    {
        let repo = Repo::new();
        repo.declare_gates_locally(before);
        let (id, wt, _) = change_with_patchset(&repo, "adoption");
        verify(&repo, &wt);
        if legacy {
            rewrite_event(&repo, &id, "verification-recorded", |event| {
                event.as_object_mut().unwrap().remove("declaration");
            });
        }
        repo.declare_gates_locally(after);
        let status = json_stdout(repo.arc(&wt).args(["status", "--json"]));
        assert_eq!(
            status["gates"][0]["green_at_head"], false,
            "coverage adoption must reject legacy evidence: {status}"
        );
        repo.arc(&wt)
            .args(["verify", "--all", "--skip-green"])
            .assert()
            .success()
            .stdout(predicates::str::contains("skipped").not());
        let status = json_stdout(repo.arc(&wt).args(["status", "--json"]));
        assert_eq!(
            status["gates"][0]["green_at_head"], true,
            "fresh evidence must count: {status}"
        );
    }
}

/// Claim: a selection receipt must replay its acknowledged paths and source
/// identity. Rejects accepting either a changed delta or a substituted identity.
#[test]
fn replay_rejects_uncheckable_path_selection() {
    for tamper in ["paths", "identity", "source-identity"] {
        let (repo, id, wt) = fixture(SCOPED);
        commit_path(
            &repo,
            &wt,
            "right/known.rs",
            "known\n",
            "test: covered path",
        );
        verify(&repo, &wt);
        assert_eq!(selected(&repo, &wt).len(), 1);
        if tamper == "source-identity" {
            for entry in fs::read_dir(event_dir(&repo, &id)).unwrap() {
                let path = entry.unwrap().path();
                let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                if event["event_type"] == "verification-recorded" && event["gate"] == "left" {
                    event.as_object_mut().unwrap().remove("declaration");
                    fs::write(path, serde_json::to_vec(&event).unwrap()).unwrap();
                }
            }
        } else {
            rewrite_event(&repo, &id, "verification-reused", |event| {
                if tamper == "paths" {
                    event["delta"]["changed_paths"] = json!(["unacknowledged"]);
                } else {
                    event["delta"]["declaration"] = json!("sha256:substitution");
                }
            });
        }
        repo.arc(&wt)
            .args(["show", "--json"])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "does not match passing left evidence",
            ));
    }
}

/// Claim: inventory and report use policy globs and one tracked inventory,
/// exposing unnamed paths. Rejects hidden unknown files, inert danger skips,
/// per-path Git commands, or omitted gate patterns in the declaration report.
#[test]
fn tracked_inventory_exposes_coverage_and_unknown_paths() {
    let repo = Repo::new();
    repo.redeclare_gates(SCOPED);
    for file in [
        "left/a.rs",
        "right/b.rs",
        "notes.md",
        "nested/doc.md",
        "right/risk.md",
        "new.xyz",
    ] {
        fs::create_dir_all(repo.root.join(file).parent().unwrap()).unwrap();
        fs::write(repo.root.join(file), "fixture\n").unwrap();
    }
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[danger]\npaths=['**/risk.md']\n",
    )
    .unwrap();
    git(&repo.root, &["add", "."]);
    git(&repo.root, &["commit", "-m", "test: inventory"]);
    let output = repo
        .arc(&repo.root)
        .env("ARC_TRACE", "1")
        .args(["gates", "paths", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let inventory: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inventory["schema"], "arc-gate-paths/1");
    let row = |name: &str| {
        inventory["paths"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["path"] == name)
            .unwrap()
    };
    assert_eq!(row("left/a.rs")["gates"], json!(["all", "left"]));
    assert_eq!(row("right/b.rs")["gates"], json!(["all", "right"]));
    assert_eq!(row("notes.md")["gates"], json!([]));
    assert_eq!(row("nested/doc.md")["gates"], json!([]));
    assert_eq!(
        row("right/risk.md")["gates"],
        json!(["all", "left", "right"])
    );
    assert_eq!(row("right/risk.md")["inert"], false);
    assert_eq!(row("new.xyz")["gates"], json!(["all", "left", "right"]));
    assert!(inventory["unclaimed_paths"]
        .as_array()
        .unwrap()
        .contains(&json!("new.xyz")));
    let trace: Value = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|v| v["schema"] == "arc-perf/1")
        .unwrap();
    assert_eq!(
        trace["children"]["by_kind"]["git ls-files"]["runs"], 1,
        "one tracked inventory: {trace}"
    );
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert_eq!(
        report["unclaimed_paths"].as_u64().unwrap() as usize,
        inventory["unclaimed_paths"].as_array().unwrap().len()
    );
    let left = report["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gate| gate["name"] == "left")
        .unwrap();
    assert_eq!(left["declaration"]["paths"], json!(["left/"]));
    assert_eq!(left["declaration"]["inert"], json!(["**/*.md"]));
}

/// Claim: path reports explain every mandatory full-coverage reason in text and
/// JSON, overriding inert labels where necessary. Rejects unexplained all-gate
/// rows, inert protected inputs, and attributing normal scoped paths to fallback.
#[test]
fn paths_explains_why_every_gate_is_affected() {
    let repo = Repo::new();
    repo.redeclare_gates("[selection]\nbase_passes=true\ninert=['.arc/', 'scripts/', '**/*.md']\n[gates.left]\ncommand='sh scripts/check.sh'\nselector='python3 scripts/select.py'\nenvironment='sh scripts/probe.sh'\npaths=['left/']\n[gates.right]\ncommand='true'\npaths=['right/']\n");
    let reasons = [
        ("new.xyz", "unclaimed", "unnamed by any gate"),
        ("left/risk.md", "dangerous", "matches a dangerous pattern"),
        (".arc/check.sh", "protected-arc", "under protected .arc/"),
        (
            "scripts/check.sh",
            "selection-input",
            "named by a gate's command, selector or probe",
        ),
        (
            "scripts/select.py",
            "selection-input",
            "named by a gate's command, selector or probe",
        ),
        (
            "scripts/probe.sh",
            "selection-input",
            "named by a gate's command, selector or probe",
        ),
    ];
    for file in reasons
        .iter()
        .map(|(file, _, _)| *file)
        .chain(["left/known.rs", "notes.md"])
    {
        fs::create_dir_all(repo.root.join(file).parent().unwrap()).unwrap();
        fs::write(repo.root.join(file), "fixture\n").unwrap();
    }
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[danger]\npaths=['**/risk.md']\n",
    )
    .unwrap();
    git(&repo.root, &["add", "."]);
    git(&repo.root, &["commit", "-m", "test: path explanations"]);
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "paths", "--json"]));
    let text = stdout(repo.arc(&repo.root).args(["gates", "paths"]));
    let row = |name: &str| {
        report["paths"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["path"] == name)
            .unwrap()
    };
    for (file, reason, explanation) in reasons {
        assert_eq!(
            row(file)["all_gates_reason"],
            reason,
            "missing reason for {file}"
        );
        assert_eq!(row(file)["gates"], json!(["left", "right"]));
        assert_eq!(row(file)["inert"], false);
        let prefix = format!("{file:?}:");
        let line = text.lines().find(|line| line.starts_with(&prefix)).unwrap();
        assert!(line.contains(explanation), "{line}");
    }
    assert!(row("left/known.rs")["all_gates_reason"].is_null());
    assert_eq!(row("left/known.rs")["gates"], json!(["left"]));
    assert!(row("notes.md")["all_gates_reason"].is_null());
    assert_eq!(row("notes.md")["gates"], json!([]));
    assert_eq!(row("notes.md")["inert"], true);
}

/// Claim: renaming a file checks the departed and arrived paths. Rejects a
/// similarity-detected rename letting the gate covering the old path skip.
#[test]
fn renames_affect_both_sides() {
    let (repo, _, wt) = fixture(SCOPED);
    commit_path(
        &repo,
        &wt,
        "left/before.rs",
        "same content\n",
        "test: original path",
    );
    verify(&repo, &wt);
    fs::create_dir_all(wt.join("right")).unwrap();
    git(&wt, &["mv", "left/before.rs", "right/after.rs"]);
    git(&wt, &["commit", "-m", "test: rename"]);
    repo.arc(&wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped").not());
}

/// Claim: path reporting reads coverage without requiring valid unrelated
/// append-policy fields or writer identity. Rejects coupling a read-only
/// inventory to write policy or requiring a recording identity. Unreadable
/// danger policy affects every path, rejecting an inert skip under uncertainty.
#[test]
fn paths_report_ignores_unrelated_append_policy() {
    let repo = Repo::new();
    repo.redeclare_gates(SCOPED);
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval='invalid boolean'\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .env("ARC_SESSION", "unknown")
        .args(["gates", "paths", "--json"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["gates", "--json"])
        .assert()
        .success();
    fs::write(repo.root.join(".arc/policy.toml"), "[policy\n").unwrap();
    let inventory = json_stdout(repo.arc(&repo.root).args(["gates", "paths", "--json"]));
    let readme = inventory["paths"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["path"] == "README.md")
        .unwrap();
    assert_eq!(
        readme["gates"],
        json!(["all", "left", "right"]),
        "uncertain policy must affect even an inert path"
    );
    repo.arc(&repo.root)
        .args(["gates", "--json"])
        .assert()
        .success();
}

/// Claim: a named integration gate can keep its pass across an unaffected
/// delta. Rejects restricting declarative selection to whole-suite runs.
#[test]
fn named_gate_selects_an_unaffected_delta() {
    let (repo, _, wt) = fixture(SCOPED);
    commit_path(
        &repo,
        &wt,
        "right/known.rs",
        "known\n",
        "test: named gate delta",
    );
    repo.arc(&wt)
        .args(["verify", "--gate", "left", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gate left: skipped (arc paths"));
    assert_eq!(selected(&repo, &wt).len(), 1);
}
