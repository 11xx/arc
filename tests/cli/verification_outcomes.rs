use crate::common::*;

fn declaring(text: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(repo.root.join(".arc/gates.toml"), text).unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    repo
}

fn events(repo: &Repo, wt: &Path, change: &str, event_type: &str) -> Vec<serde_json::Value> {
    stdout(repo.arc(wt).args(["events", "--change", change]))
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

fn gate<'a>(status: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gate| gate["name"] == name)
        .unwrap()
}

const BYPASSABLE: &str =
    "[gates.docs]\ncommand = \"false\"\nbypass = \"documentation or comments only\"\n";

/// Claim: a gate command exiting 75 within its timeout records `not-admitted`,
/// which blocks integration as a gate that did not run, not as one that
/// failed, and which `--skip-green` never reuses. Rejects recording the exit
/// as a failing test, as arc did before the convention was read.
#[test]
fn exit_75_records_not_admitted_and_blocks_integration_as_unrun() {
    let repo = declaring("[gates.heavy]\ncommand = \"echo queue full; exit 75\"\n");
    let (id, wt, head) = change_with_patchset(&repo, "queued");
    let verified = stdout_any_status(repo.arc(&wt).args(["verify", "--gate", "heavy"]));
    assert!(
        verified.contains("verification: NotAdmitted at"),
        "{verified}"
    );
    assert!(verified.contains("before any check ran"), "{verified}");

    let recorded = events(&repo, &wt, &id, "verification-recorded");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["result"], "not-admitted");
    assert_eq!(recorded[0]["exit_code"], 75);
    assert_eq!(recorded[0]["revision"], head.as_str());

    let status = json_stdout(repo.arc(&wt).args(["status"]));
    let heavy = gate(&status, "heavy");
    assert_eq!(heavy["result"], "not-admitted", "{status}");
    assert_eq!(heavy["green_at_head"], false, "{status}");
    assert_eq!(status["next_action"], "run_gate:heavy", "{status}");

    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    let check = stdout_any_status(repo.arc(&wt).args(["check"]));
    assert!(check.contains("was not admitted"), "{check}");
    assert!(!check.contains("the gate failed"), "{check}");
    repo.arc(&wt).args(["check"]).assert().code(5);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .code(5);

    let rerun = stdout_any_status(repo.arc(&wt).args(["verify", "--all", "--skip-green"]));
    assert!(!rerun.contains("skipped"), "{rerun}");
    assert_eq!(events(&repo, &wt, &id, "verification-recorded").len(), 2);
}

/// Claim: a timeout outranks the exit status, so a command that exits 75 and
/// is still holding its output when the timeout elapses timed out. Rejects
/// reading the exit status before the timeout.
#[test]
fn exit_75_after_a_timeout_is_a_timeout() {
    let repo = declaring("[gates.heavy]\ncommand = \"sleep 5 & exit 75\"\ntimeout = \"1s\"\n");
    let (id, wt, _) = change_with_patchset(&repo, "overrun");
    repo.arc(&wt)
        .args(["verify", "--gate", "heavy"])
        .assert()
        .code(1);
    let recorded = events(&repo, &wt, &id, "verification-recorded");
    assert_eq!(recorded[0]["result"], "fail", "{recorded:?}");
    assert_eq!(recorded[0]["timed_out"], true, "{recorded:?}");
    let status = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&status, "heavy")["timed_out"], true, "{status}");
}

/// Construct the stored event an older writer emitted, without invoking the retired flag.
pub(crate) fn historical_bypass(repo: &Repo, wt: &Path, id: &str, name: &str) {
    let mut paths = fs::read_dir(event_dir(repo, id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    let mut event: serde_json::Value =
        serde_json::from_slice(&fs::read(paths.last().unwrap()).unwrap()).unwrap();
    let next = ulid::Ulid::from(
        u128::from(ulid::Ulid::from_string(event["event_id"].as_str().unwrap()).unwrap()) + 1,
    )
    .to_string();
    event.as_object_mut().unwrap().retain(|key, _| {
        [
            "schema_version",
            "repository_id",
            "change_id",
            "actor",
            "actor_source",
            "harness",
            "session",
            "created_at",
        ]
        .contains(&key.as_str())
    });
    event["event_id"] = next.clone().into();
    event["event_type"] = "verification-bypassed".into();
    event["gate"] = name.into();
    event["revision"] = repo.head(wt).into();
    event["tree"] = git_out(wt, &["rev-parse", "HEAD^{tree}"]).into();
    event["reason"] = "only README.md changed".into();
    event["qualification"] = "documentation or comments only".into();
    fs::write(
        event_dir(repo, id).join(format!("{next}.json")),
        serde_json::to_vec(&event).unwrap(),
    )
    .unwrap();
}

fn bypassed_change(repo: &Repo, slug: &str) -> (String, PathBuf, String) {
    let change = change_with_patchset(repo, slug);
    historical_bypass(repo, &change.1, &change.0, "docs");
    change
}

/// Claim: every --bypass request refuses with the budget/debt remedies and writes
/// no event. Rejects a declared gate or flag combination re-enabling bypass.
#[test]
fn retired_bypass_flag_refuses_without_writes() {
    let repo = declaring(BYPASSABLE);
    let (id, wt, _) = change_with_patchset(&repo, "refusals");
    let before = event_count(&repo, &id);
    for args in [
        vec!["verify", "--gate", "docs", "--bypass", "README only"],
        vec!["verify", "--bypass", ""],
        vec!["verify", "--all", "--bypass", "README only"],
    ] {
        repo.arc(&wt)
            .args(args)
            .assert()
            .failure()
            .stderr(predicates::str::contains("--bypass is retired"))
            .stderr(predicates::str::contains("arc gates over"))
            .stderr(predicates::str::contains("integrate with recorded debt"));
    }
    repo.arc(&repo.home)
        .args(["verify", "--bypass", "README only"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--bypass is retired"));
    assert_eq!(event_count(&repo, &id), before);
}

/// Claim: an inert bypass key is a doctor finding and check warning, and leaves
/// status readable. Rejects silent keys, declaration failure, and key permission.
#[test]
fn retired_bypass_key_warns_without_bricking_status() {
    for placement in ["target", "head", "operator", "head-conflict", "empty"] {
        let repo = declaring(if placement == "target" {
            BYPASSABLE
        } else {
            "[gates.docs]\ncommand='false'\n"
        });
        let (id, wt, _) = change_with_patchset(&repo, "legacy-key");
        match placement {
            "head-conflict" => {
                repo.commit(
                    &wt,
                    ".arc/gates.toml",
                    &BYPASSABLE.replace("false", "true"),
                    "test: conflicting legacy declaration",
                );
            }
            "empty" => {
                repo.commit(
                    &wt,
                    ".arc/gates.toml",
                    &BYPASSABLE.replace("documentation or comments only", ""),
                    "test: empty legacy key",
                );
            }
            "head" => {
                repo.commit(
                    &wt,
                    ".arc/gates.toml",
                    BYPASSABLE,
                    "test: legacy declaration",
                );
            }
            "operator" => repo.declare_gates_locally(BYPASSABLE),
            _ => {}
        }
        let s = json_stdout(repo.arc(&wt).args(["status", &id]));
        assert_eq!(gate(&s, "docs")["green_at_head"], false, "{s}");
        assert!(
            s["advisories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["code"] == "gate-bypass-retired"),
            "{s}"
        );
        repo.arc(&wt)
            .args(["doctor", "--json"])
            .assert()
            .failure()
            .stdout(predicates::str::contains("gate-bypass-retired"));
        repo.arc(&wt)
            .args(["check"])
            .assert()
            .failure()
            .stdout(predicates::str::contains("`bypass` has no effect"));
    }
}

/// Claim: old bypass events replay and remain visible as history in show/explain;
/// status refuses to treat that history as green. Rejects lost history or authority.
#[test]
fn retired_bypass_history_replays_without_authorizing_status() {
    let repo = declaring(BYPASSABLE);
    let (id, wt, _) = bypassed_change(&repo, "history");
    assert_eq!(events(&repo, &wt, &id, "verification-bypassed").len(), 1);
    let s = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&s, "docs")["result"], "bypassed", "{s}");
    assert_eq!(gate(&s, "docs")["green_at_head"], false, "{s}");
    let show = stdout(repo.arc(&wt).args(["show"]));
    assert!(show.contains("historical bypass by"), "{show}");
    assert!(show.contains("satisfies no requirement"), "{show}");
    let raw = json_stdout(repo.arc(&wt).args(["show", "--json"]));
    assert_eq!(
        raw["verifications"][0]["bypass"]["reason"], "only README.md changed",
        "{raw}"
    );
    let explain = stdout(repo.arc(&wt).args(["explain"]));
    assert!(explain.contains("only README.md changed"), "{explain}");
    assert!(
        explain.contains("historical bypass satisfies no requirement"),
        "{explain}"
    );
    let explanation = json_stdout(repo.arc(&wt).args(["explain", "--json"]));
    assert!(explanation["evaluation"]["rows"].as_array().unwrap().iter().any(|r| r["result"] == "bypassed" && r["bypass"]["reason"] == "only README.md changed"), "{explanation}");
}

/// Claim: check refuses when the only gate answer is a historical bypass, while
/// retaining check.bypassed for readers. Rejects a historical waiver making ready.
#[test]
fn retired_bypass_does_not_authorize_check() {
    let repo = declaring(BYPASSABLE);
    let (_, wt, _) = bypassed_change(&repo, "check-history");
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    let output = repo
        .arc(&wt)
        .args(["check", "--json"])
        .assert()
        .code(5)
        .get_output()
        .clone();
    let check: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(check["ready"], false, "{check}");
    assert_eq!(
        check["bypassed"]["docs"]["reason"], "only README.md changed",
        "{check}"
    );
}

/// Claim: integration refuses historical bypass evidence even with review debt
/// supplied, leaving its destination unmoved. Rejects debt granting gate authority.
#[test]
fn retired_bypass_does_not_authorize_integration() {
    let repo = declaring(BYPASSABLE);
    let (id, wt, _) = bypassed_change(&repo, "integration-history");
    let before = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args(["integrate", &id, "--debt", "review remains owed"])
        .assert()
        .code(5)
        .stderr(predicates::str::contains("satisfies no gate requirement"));
    assert_eq!(repo.head(&repo.root), before);
    let raw = json_stdout(repo.arc(&wt).args(["show", "--json"]));
    assert!(raw["closure"].is_null(), "{raw}");
}

/// Claim: importing a bypass retains the event but confers no gate authority at
/// the receiver. Rejects transport changing a historical waiver into a green gate.
#[test]
fn retired_bypass_import_grants_nothing() {
    let source = declaring(BYPASSABLE);
    let (id, wt, head) = bypassed_change(&source, "import-history");
    let input = source.home.join("bypass.json");
    source
        .arc(&wt)
        .args(["export", &id, "--output", input.to_str().unwrap()])
        .assert()
        .success();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    let branch = git_out(&wt, &["branch", "--show-current"]);
    git(&receiver.root, &["branch", &branch, &head]);
    receiver.declare_gates_locally(BYPASSABLE);
    receiver
        .arc(&receiver.root)
        .args(["import", input.to_str().unwrap()])
        .assert()
        .success();
    let s = json_stdout(receiver.arc(&receiver.root).args(["status", &id]));
    assert_eq!(gate(&s, "docs")["green_at_head"], false, "{s}");
    let raw = json_stdout(receiver.arc(&receiver.root).args(["show", &id, "--json"]));
    assert_eq!(raw["verifications"][0]["result"], "bypassed", "{raw}");
    receiver
        .arc(&receiver.root)
        .args(["integrate", &id, "--debt", "review remains owed"])
        .assert()
        .code(5);
}

/// Claim: both landing forms execute required gates rather than accept bypass
/// history, and refuse a failing gate without moving the target. Rejects bypass
/// reuse or the landing gate union dropping the requirement.
fn bypass_history_landing(composed: bool) {
    let repo = declaring(BYPASSABLE);
    let (lower, _, lower_head) = bypassed_change(&repo, "lower");
    let mut args = vec!["begin", "upper"];
    if !composed {
        args.extend(["--base", &lower_head]);
    }
    let upper = opened_change_id(&stdout(repo.arc(&repo.root).args(args)));
    let wt = repo.home.join(".worktrees/repo-upper");
    repo.commit(&wt, "upper.txt", "upper\n", "test: upper");
    repo.arc(&wt).args(["snapshot"]).assert().success();
    historical_bypass(&repo, &wt, &upper, "docs");
    let before = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args(["land", &lower, &upper, "--debt", "review remains owed"])
        .assert()
        .code(5);
    assert_eq!(repo.head(&repo.root), before);
    let raw = json_stdout(repo.arc(&repo.root).args(["show", &upper, "--json"]));
    assert!(raw["closure"].is_null(), "{raw}");
    assert!(
        raw["verifications"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["result"] == "fail" && v["gate"] == "docs"),
        "{raw}"
    );
}

/// Claim: a stack landing refuses a gate answered only by historical bypass.
/// Rejects bypass reuse authorizing the target move.
#[test]
fn retired_bypass_does_not_authorize_land_stack() {
    bypass_history_landing(false);
}

/// Claim: a composed landing refuses a gate answered only by historical bypass.
/// Rejects bypass reuse authorizing the target move.
#[test]
fn retired_bypass_does_not_authorize_land_composed() {
    bypass_history_landing(true);
}

/// Claim: publication never reuses a change's bypass, including --skip-green;
/// a failing gate refuses publication. Rejects substituting the change ledger.
#[test]
fn retired_bypass_does_not_authorize_publication() {
    let repo = declaring(BYPASSABLE);
    let (id, _, _) = bypassed_change(&repo, "publication-history");
    for cached in [false, true] {
        let mut cmd = repo.arc(&repo.root);
        cmd.args(["publication", "--target", "master"]);
        if cached {
            cmd.arg("--skip-green");
        }
        cmd.assert()
            .code(1)
            .stdout(predicates::str::contains("publication gate docs: fail"));
    }
    assert!(events(&repo, &repo.root, &id, "verification-recorded").is_empty());
}
