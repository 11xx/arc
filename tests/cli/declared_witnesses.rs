//! Declared `Witness:` and `Change-size:` commit trailers (`arc-gate-trailers/1`),
//! `interface_budget`, and target-branch `invariants` list files.

use crate::common::*;

fn env_dump_command(out: &Path) -> String {
    format!(
        "python3 -c 'import json,os,pathlib; \
         pathlib.Path({:?}).write_text(json.dumps({{k: os.environ.get(k) for k in (\"ARC_WITNESSES\", \"ARC_CHANGE_SIZE\", \"ARC_INVARIANTS\")}}))'",
        out.display().to_string()
    )
}

fn counting_command(counter: &Path) -> String {
    format!("echo run >> '{}'", counter.display())
}

fn runs(counter: &Path) -> usize {
    fs::read_to_string(counter)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

fn doctor_problems(repo: &Repo, cwd: &Path) -> serde_json::Value {
    let output = repo
        .arc(cwd)
        .args(["doctor", "--json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    serde_json::from_slice(&output.stdout).unwrap()
}

fn gate_row(repo: &Repo, wt: &Path, id: &str, name: &str) -> serde_json::Value {
    let status = json_stdout(repo.arc(wt).args(["status", id]));
    status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("no gate {name} in {status}"))
        .clone()
}

/// Claim: executed gates receive deterministic `ARC_WITNESSES` and `ARC_CHANGE_SIZE`
/// from `merge-base..head` when declared, and have both (plus `ARC_INVARIANTS`)
/// cleanly unset when absent even if the caller's environment exports them.
#[test]
fn witness_and_change_size_env_propagation_and_clean_unset() {
    let repo = Repo::new();
    let dump = repo.home.join("gate-env.json");
    repo.redeclare_gates(&format!(
        "[gates.check]\ncommand = {:?}\n",
        env_dump_command(&dump)
    ));

    let (_id, wt, _) = change_with_patchset(&repo, "env-unset");
    repo.arc(&wt)
        .env("ARC_WITNESSES", "leaked")
        .env("ARC_CHANGE_SIZE", "leaked")
        .env("ARC_INVARIANTS", "leaked")
        .args(["verify", "--all"])
        .assert()
        .success();
    let seen: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&dump).unwrap()).unwrap();
    assert!(seen["ARC_WITNESSES"].is_null(), "{seen}");
    assert!(seen["ARC_CHANGE_SIZE"].is_null(), "{seen}");
    assert!(seen["ARC_INVARIANTS"].is_null(), "{seen}");

    // Multi-commit aggregation: deduplicates repeats across commits in first-seen order.
    repo.commit(
        &wt,
        "a.txt",
        "one\n",
        "feat: one\n\nWitness: unit:alpha\nWitness: unit:beta\n",
    );
    repo.commit(
        &wt,
        "b.txt",
        "two\n",
        "feat: two\n\nWitness: unit:beta\nWitness: unit:gamma\nChange-size: interface\n",
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    let seen: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&dump).unwrap()).unwrap();
    assert_eq!(seen["ARC_WITNESSES"], "unit:alpha\nunit:beta\nunit:gamma");
    assert_eq!(seen["ARC_CHANGE_SIZE"], "interface");
    assert!(seen["ARC_INVARIANTS"].is_null(), "{seen}");

    // `Witness: none (<REASON>)` passes the exemption deterministically.
    let (_id2, wt2, _) = change_with_patchset(&repo, "env-none");
    repo.commit(
        &wt2,
        "notes.txt",
        "docs\n",
        "docs: clarify\n\nWitness: none (documentation only)\n",
    );
    repo.arc(&wt2).args(["snapshot"]).assert().success();
    repo.arc(&wt2).args(["verify", "--all"]).assert().success();
    let seen: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&dump).unwrap()).unwrap();
    assert_eq!(seen["ARC_WITNESSES"], "none (documentation only)");
    assert!(seen["ARC_CHANGE_SIZE"].is_null(), "{seen}");
}

/// Claim: changing `Witness:` or `Change-size:` trailers invalidates verification
/// reuse even when the Git tree is unchanged, while adding unrelated contribution
/// trailers preserves reuse.
#[test]
fn verification_reuse_invalidates_on_witness_or_change_size_edit() {
    let repo = Repo::new();
    let counter = repo.home.join("check.runs");
    repo.redeclare_gates(&format!(
        "[gates.check]\ncommand = {:?}\n",
        counting_command(&counter)
    ));

    let (id, wt, _) = change_with_patchset(&repo, "reuse-trailers");
    git(
        &wt,
        &[
            "commit",
            "--amend",
            "-m",
            "feat: work\n\nWitness: unit:alpha\n",
        ],
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(runs(&counter), 1);

    // Same tree and same Witness: trailer -> reused.
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(runs(&counter), 1);

    // Adding an unrelated contribution trailer keeps the same Witness: declaration -> reused.
    git(
        &wt,
        &[
            "commit",
            "--amend",
            "-m",
            "feat: work\n\nWitness: unit:alpha\nImplemented-by: antigravity:test-model\n",
        ],
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(runs(&counter), 1);

    // Amending Witness: to another witness at the same tree -> invalidates reuse.
    git(
        &wt,
        &[
            "commit",
            "--amend",
            "-m",
            "feat: work\n\nWitness: unit:beta\n",
        ],
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    let row = gate_row(&repo, &wt, &id, "check");
    assert_eq!(row["green_at_head"], false, "{row}");
    assert_eq!(row["declaration_changed"], true, "{row}");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(runs(&counter), 2);

    // Adding Change-size: interface at the same tree -> invalidates reuse.
    git(
        &wt,
        &[
            "commit",
            "--amend",
            "-m",
            "feat: work\n\nWitness: unit:beta\nChange-size: interface\n",
        ],
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(runs(&counter), 3);

    // Removing all gate trailers at the same tree -> invalidates reuse.
    git(&wt, &["commit", "--amend", "-m", "feat: work"]);
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(runs(&counter), 4);
}

/// Claim: malformed, contradictory, or excessive `Witness:` / `Change-size:`
/// declarations refuse `verify`, `check`, and report in `doctor`.
#[test]
fn invalid_gate_trailers_refuse_verify_check_and_doctor() {
    let repo = Repo::new();
    let counter = repo.home.join("never.runs");
    repo.redeclare_gates(&format!(
        "[gates.check]\ncommand = {:?}\n",
        counting_command(&counter)
    ));

    for (slug, message, needle) in [
        (
            "four-witnesses",
            "feat: too many\n\nWitness: a\nWitness: b\nWitness: c\nWitness: d\n",
            "maximum is 3",
        ),
        (
            "none-no-reason",
            "docs: empty\n\nWitness: none\n",
            "parenthesised reason",
        ),
        (
            "mixed-none",
            "feat: mixed\n\nWitness: none (docs)\nWitness: unit:a\n",
            "contradictory",
        ),
        (
            "bad-size",
            "feat: bad size\n\nChange-size: huge\n",
            "expected \"interface\"",
        ),
    ] {
        let (id, wt, _) = change_with_patchset(&repo, slug);
        git(&wt, &["commit", "--amend", "-m", message]);
        repo.arc(&wt).args(["snapshot"]).assert().success();
        repo.arc(&wt)
            .args(["review", "--verdict", "approved"])
            .assert()
            .success();

        let verify = repo.arc(&wt).args(["verify", "--all"]).assert().failure();
        let stderr = String::from_utf8_lossy(&verify.get_output().stderr);
        assert!(stderr.contains(needle), "{slug}: {stderr}");

        let check = repo.arc(&wt).args(["check", &id]).assert().code(5);
        let check_out = String::from_utf8_lossy(&check.get_output().stdout);
        assert!(check_out.contains(needle), "{slug}: {check_out}");

        let doctor = doctor_problems(&repo, &wt);
        assert!(
            doctor["problems"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["code"] == "gate-trailers-invalid"),
            "{slug}: {doctor}"
        );
        repo.arc(&wt)
            .args(["close", &id, "--abandoned"])
            .assert()
            .success();
    }
    assert_eq!(
        runs(&counter),
        0,
        "invalid trailers must never execute gates"
    );
}

/// Claim: gate trailers are read over `merge-base..head`, so commits landed on
/// the target branch after a change opened are not counted once rebased.
#[test]
fn trailers_read_over_merge_base_not_stale_recorded_base() {
    let repo = Repo::new();
    let dump = repo.home.join("rebased-env.json");
    repo.redeclare_gates(&format!(
        "[gates.check]\ncommand = {:?}\n",
        env_dump_command(&dump)
    ));

    let (_id, wt, _) = change_with_patchset(&repo, "rebased");
    git(
        &wt,
        &[
            "commit",
            "--amend",
            "-m",
            "feat: my change\n\nWitness: unit:mine\n",
        ],
    );

    // Advance master with 3 witnesses of its own; counting from the stale base
    // would produce 4 witnesses and fail.
    repo.commit(
        &repo.root,
        "other.txt",
        "landed\n",
        "feat: other landed\n\nWitness: unit:other1\nWitness: unit:other2\nWitness: unit:other3\n",
    );
    git(&wt, &["rebase", "master"]);
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();

    let seen: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&dump).unwrap()).unwrap();
    assert_eq!(seen["ARC_WITNESSES"], "unit:mine");
}

/// Claim: `interface_budget` widens the effective budget strictly for changes
/// declaring `Change-size: interface`, is enforced as a failure when exceeded,
/// and cannot be added by a change to loosen a target's stricter `budget`.
#[test]
fn interface_budget_enforced_and_protected_by_target() {
    let repo = Repo::new();
    repo.redeclare_gates(
        "[gates.unit]\ncommand = 'true'\nbudget = '10s'\ninterface_budget = '30s'\n",
    );

    // Ordinary change: 15s exceeds 10s budget.
    let (ordinary_id, ordinary_wt, _) = change_with_patchset(&repo, "ordinary-budget");
    repo.arc(&ordinary_wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "15000")
        .args(["verify", "--all"])
        .assert()
        .code(1);
    assert_eq!(
        gate_row(&repo, &ordinary_wt, &ordinary_id, "unit")["budget"]["verdict"],
        "over-budget"
    );

    // Interface change: 15s fits 30s interface_budget; 31s fails.
    let (iface_id, iface_wt, _) = change_with_patchset(&repo, "interface-budget");
    git(
        &iface_wt,
        &[
            "commit",
            "--amend",
            "-m",
            "feat: interface\n\nChange-size: interface\n",
        ],
    );
    repo.arc(&iface_wt).args(["snapshot"]).assert().success();
    repo.arc(&iface_wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "15000")
        .args(["verify", "--all"])
        .assert()
        .success();
    let row = gate_row(&repo, &iface_wt, &iface_id, "unit");
    assert_eq!(row["green_at_head"], true, "{row}");
    assert_eq!(row["budget"]["budget_ms"], 30_000, "{row}");

    repo.arc(&iface_wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "31000")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(1);
    assert_eq!(
        gate_row(&repo, &iface_wt, &iface_id, "unit")["budget"]["verdict"],
        "over-budget"
    );

    // Target with `budget = '10s'` and NO `interface_budget` cannot be loosened
    // by a change adding `interface_budget = '30s'` in its own `.arc/gates.toml`.
    let repo2 = Repo::new();
    repo2.redeclare_gates("[gates.unit]\ncommand = 'true'\nbudget = '10s'\n");
    let (sneaky_id, sneaky_wt, _) = change_with_patchset(&repo2, "sneaky-interface");
    repo2.commit(
        &sneaky_wt,
        ".arc/gates.toml",
        "[gates.unit]\ncommand = 'true'\nbudget = '10s'\ninterface_budget = '30s'\n",
        "feat: widen own budget\n\nChange-size: interface\n",
    );
    repo2.arc(&sneaky_wt).args(["snapshot"]).assert().success();
    repo2
        .arc(&sneaky_wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "15000")
        .args(["verify", "--all"])
        .assert()
        .code(1);
    assert_eq!(
        gate_row(&repo2, &sneaky_wt, &sneaky_id, "unit")["budget"]["verdict"],
        "over-budget"
    );
}

/// Claim: a gate's `invariants` file is read from the target branch ref so a
/// change cannot weaken or remove entries, and updating the target invariant
/// list invalidates prior passes.
#[test]
fn target_branch_invariants_cannot_be_weakened_and_bind_reuse() {
    let repo = Repo::new();
    let dump = repo.home.join("invariants-env.json");
    fs::create_dir_all(repo.root.join("scripts")).unwrap();
    repo.commit(
        &repo.root,
        "scripts/check-invariants.json",
        r#"[
  {"name": "one", "check": "unit:one"},
  {"name": "two", "check": "unit:two"}
]
"#,
        "chore: add fixed invariants",
    );
    repo.redeclare_gates(&format!(
        "[gates.check]\ncommand = {:?}\ninvariants = 'scripts/check-invariants.json'\n",
        env_dump_command(&dump)
    ));

    let (id, wt, _) = change_with_patchset(&repo, "weakened-invariants");
    // Attempt to drop `two` from the change's own copy AND drop `invariants` from gates.toml.
    repo.commit(
        &wt,
        "scripts/check-invariants.json",
        r#"[{"name": "one", "check": "unit:one"}]"#,
        "feat: drop invariant locally",
    );
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &format!("[gates.check]\ncommand = {:?}\n", env_dump_command(&dump)),
        "feat: drop invariants key locally",
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();

    // Gate STILL received the target branch's full invariant list!
    let seen: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&dump).unwrap()).unwrap();
    assert_eq!(seen["ARC_INVARIANTS"], "unit:one\nunit:two");

    // Advance master with a third invariant: existing pass is no longer green.
    repo.commit(
        &repo.root,
        "scripts/check-invariants.json",
        r#"[
  {"name": "one", "check": "unit:one"},
  {"name": "two", "check": "unit:two"},
  {"name": "three", "check": "unit:three"}
]
"#,
        "chore: add third invariant on target",
    );
    let row = gate_row(&repo, &wt, &id, "check");
    assert_eq!(row["green_at_head"], false, "{row}");

    // Re-verifying picks up the updated target invariant list.
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let seen: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&dump).unwrap()).unwrap();
    assert_eq!(seen["ARC_INVARIANTS"], "unit:one\nunit:two\nunit:three");
}

/// Claim: `arc doctor` rejects unknown keys in `.arc/gates.toml`, invalid
/// `interface_budget` combinations, and missing or malformed `invariants` files.
#[test]
fn doctor_validates_unknown_keys_interface_budget_and_invariants() {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.check]\ncommand = 'true'\nbogus_key = 1\n");
    let doctor = doctor_problems(&repo, &repo.root);
    assert!(
        doctor["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "gate-policy-unreadable"),
        "{doctor}"
    );

    repo.redeclare_gates("[gates.check]\ncommand = 'true'\ninterface_budget = '30s'\n");
    let doctor = doctor_problems(&repo, &repo.root);
    assert!(
        doctor["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "gate-budget-invalid"
                && p["detail"]
                    .as_str()
                    .unwrap_or("")
                    .contains("interface_budget without budget")),
        "{doctor}"
    );

    repo.redeclare_gates(
        "[gates.check]\ncommand = 'true'\nbudget = '30s'\ninterface_budget = '10s'\n",
    );
    let doctor = doctor_problems(&repo, &repo.root);
    assert!(
        doctor["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "gate-budget-invalid"
                && p["detail"]
                    .as_str()
                    .unwrap_or("")
                    .contains("is less than budget")),
        "{doctor}"
    );

    repo.redeclare_gates(
        "[gates.check]\ncommand = 'true'\ninvariants = 'scripts/missing-invariants.json'\n",
    );
    let doctor = doctor_problems(&repo, &repo.root);
    assert!(
        doctor["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "gate-invariants-invalid"),
        "{doctor}"
    );
}

/// Claim: an operator policy gate budget without `interface_budget` caps
/// `Change-size: interface` runs just like ordinary runs.
#[test]
fn operator_ordinary_budget_caps_interface_change() {
    let repo = Repo::new();
    repo.redeclare_gates(
        "[gates.unit]\ncommand = 'true'\nbudget = '10s'\ninterface_budget = '30s'\n",
    );
    repo.declare_gates_locally("[gates.unit]\ncommand = 'true'\nbudget = '10s'\n");

    let (id, wt, _) = change_with_patchset(&repo, "operator-capped-iface");
    git(
        &wt,
        &[
            "commit",
            "--amend",
            "-m",
            "feat: interface\n\nChange-size: interface\n",
        ],
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "15000")
        .args(["verify", "--all"])
        .assert()
        .code(1);
    let row = gate_row(&repo, &wt, &id, "unit");
    assert_eq!(row["budget"]["verdict"], "over-budget", "{row}");
    assert_eq!(row["budget"]["budget_ms"], 10_000, "{row}");

    // Operator cap between target budget and interface_budget caps interface runs at operator's limit.
    repo.declare_gates_locally("[gates.unit]\ncommand = 'true'\nbudget = '20s'\n");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "15000")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    let row = gate_row(&repo, &wt, &id, "unit");
    assert_eq!(row["budget"]["verdict"], "within-budget", "{row}");
    assert_eq!(row["budget"]["budget_ms"], 20_000, "{row}");

    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "25000")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(1);
    assert_eq!(
        gate_row(&repo, &wt, &id, "unit")["budget"]["verdict"],
        "over-budget"
    );
}

/// Claim: publication refuses acceptance when `interface_budget` changes while
/// checks are running, even when ordinary `budget` and `command` stay identical.
#[test]
fn publication_detects_mid_run_interface_budget_tightening() {
    let repo = Repo::new();
    let policy = repo.root.join(".git/arc/operator-policy.toml");
    let script = repo.home.join("tighten-interface-budget.py");
    let command = format!("python3 '{}'", script.display());
    let tightened = format!(
        "[gates.unit]\ncommand = {command:?}\nevents = ['publication']\nbudget = '10s'\ninterface_budget = '12s'\n"
    );
    fs::write(
        &script,
        format!(
            "from pathlib import Path\nPath({:?}).write_text({tightened:?})\n",
            policy.to_str().unwrap()
        ),
    )
    .unwrap();
    repo.redeclare_gates(&format!(
        "[gates.unit]\ncommand = {command:?}\nevents = ['publication']\nbudget = '10s'\ninterface_budget = '30s'\n"
    ));
    let (id, wt, _) = change_with_patchset(&repo, "pub-iface-drift");
    git(
        &wt,
        &[
            "commit",
            "--amend",
            "-m",
            "feat: interface\n\nChange-size: interface\n",
        ],
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    let branch = git_out(&wt, &["branch", "--show-current"]);

    let output = repo
        .arc(&repo.root)
        .env("ARC_TEST_COMMAND_RUN_MS", "15000")
        .args(["publication", "--target", &branch, "--change", &id])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(
        text.contains("publication gate declarations changed while checks ran"),
        "{text}"
    );
}

/// Claim: a composed landing with distinct member `Witness:` declarations plans
/// and lands cleanly, executing the shared gate for each distinct witness set at
/// the composed head and recording each member's receipt in its authorization.
#[test]
fn composed_landing_runs_distinct_member_witnesses() {
    let repo = Repo::new();
    let log = repo.home.join("composed-witnesses.log");
    let command = format!(
        "python3 -c 'import os,pathlib; \
         open({:?}, \"a\").write((os.environ.get(\"ARC_WITNESSES\") or \"NONE\") + \"\\n\")'",
        log.display().to_string()
    );
    repo.redeclare_gates(&format!("[gates.check]\ncommand = {command:?}\n"));

    let (alpha_id, alpha_wt, _) = change_with_patchset(&repo, "alpha-witness");
    repo.commit(
        &alpha_wt,
        "alpha.txt",
        "alpha\n",
        "feat: alpha\n\nWitness: unit:alpha\n",
    );
    repo.arc(&alpha_wt).args(["snapshot"]).assert().success();

    let (beta_id, beta_wt, _) = change_with_patchset(&repo, "beta-witness");
    repo.commit(
        &beta_wt,
        "beta.txt",
        "beta\n",
        "feat: beta\n\nWitness: unit:beta\n",
    );
    repo.arc(&beta_wt).args(["snapshot"]).assert().success();

    repo.arc(&repo.root)
        .args([
            "land",
            &alpha_id,
            &beta_id,
            "--dry-run",
            "--debt",
            "composed",
        ])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["land", &alpha_id, &beta_id, "--debt", "composed"])
        .assert()
        .success();

    let seen: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        seen.iter().any(|line| line == "unit:alpha"),
        "expected unit:alpha in {seen:?}"
    );
    assert!(
        seen.iter().any(|line| line == "unit:beta"),
        "expected unit:beta in {seen:?}"
    );

    let alpha = json_stdout(repo.arc(&repo.root).args(["show", &alpha_id, "--json"]));
    let beta = json_stdout(repo.arc(&repo.root).args(["show", &beta_id, "--json"]));
    assert_eq!(alpha["closure"]["outcome"], "integrated", "{alpha}");
    assert_eq!(beta["closure"]["outcome"], "integrated", "{beta}");
    let alpha_ev = alpha["closure"]["authorization"]["gate_evidence"]["check"]
        .as_str()
        .expect("alpha check evidence");
    let beta_ev = beta["closure"]["authorization"]["gate_evidence"]["check"]
        .as_str()
        .expect("beta check evidence");
    assert_ne!(
        alpha_ev, beta_ev,
        "distinct witness runs must attach distinct gate receipts"
    );
}

/// Claim: concurrent batch measurement inside a change carrying `Witness:`
/// trailers matches `verify`, so `verify` runs those gates concurrently in a
/// batch with `ARC_WITNESSES` propagated.
#[test]
fn concurrent_batch_measurement_and_verify_with_witness_trailers() {
    const BATCH_SCRIPT: &str = r#"import os, pathlib, signal, sys
name = sys.argv[1]
home = pathlib.Path(os.environ['ARC_SANDBOX'])
signal.alarm(8)
if os.environ.get('BATCH_MODE') == 'together':
    if name == 'a':
        with open(home / 'a-to-b', 'w') as out: out.write('a')
        with open(home / 'b-to-a') as inp: assert inp.read() == 'b'
    else:
        with open(home / 'a-to-b') as inp: assert inp.read() == 'a'
        with open(home / 'b-to-a', 'w') as out: out.write('b')
with open(home / 'batch-witnesses', 'a') as out:
    out.write(f"{name}:{os.environ.get('ARC_WITNESSES', '')}\n")
"#;

    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    repo.commit(
        &repo.root,
        ".arc/batch.py",
        BATCH_SCRIPT,
        "test: batch script",
    );
    repo.redeclare_gates(
        "[gates.a]\ncommand = 'python3 .arc/batch.py a'\nconcurrent = true\nbudget = '10s'\ntimeout = '5s'\n\
         [gates.b]\ncommand = 'python3 .arc/batch.py b'\nconcurrent = true\nbudget = '10s'\ntimeout = '5s'\n",
    );
    for fifo in ["a-to-b", "b-to-a"] {
        assert!(Command::new("mkfifo")
            .arg(repo.home.join(fifo))
            .status()
            .unwrap()
            .success());
    }

    let (id, wt, _) = change_with_patchset(&repo, "concurrent-witness");
    git(
        &wt,
        &[
            "commit",
            "--amend",
            "-m",
            "feat: concurrent witness\n\nWitness: unit:gamma\n",
        ],
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();

    repo.arc(&wt)
        .env("BATCH_MODE", "together")
        .args(["gates", "measure", "--concurrent"])
        .assert()
        .success()
        .stdout(predicates::str::contains("concurrent batch accepted"));

    let log = repo.home.join("batch-witnesses");
    let _ = fs::remove_file(&log);

    repo.arc(&wt)
        .env("BATCH_MODE", "together")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();

    let recorded = fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = recorded.lines().collect();
    assert!(
        lines.contains(&"a:unit:gamma") && lines.contains(&"b:unit:gamma"),
        "expected both concurrent gates to receive unit:gamma: {lines:?}"
    );
    let shown = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    let verifications = shown["verifications"].as_array().unwrap();
    for gate in ["a", "b"] {
        let entry = verifications
            .iter()
            .rev()
            .find(|v| v["gate"] == gate)
            .unwrap_or_else(|| panic!("missing verification for {gate}: {shown}"));
        assert!(
            entry["concurrent_batch"].is_object(),
            "gate {gate} did not run in a concurrent batch: {entry}"
        );
    }
}

/// Claim: invalid UTF-8 bytes in a candidate checkout's copy of an `invariants`
/// file do not block verification or status against the valid target-branch list.
#[test]
fn candidate_non_utf8_invariants_file_does_not_block_valid_target() {
    let repo = Repo::new();
    let dump = repo.home.join("non-utf8-invariants-env.json");
    fs::create_dir_all(repo.root.join("scripts")).unwrap();
    repo.commit(
        &repo.root,
        "scripts/check-invariants.json",
        r#"[{"name": "one", "check": "unit:one"}]"#,
        "chore: add valid target invariants",
    );
    repo.redeclare_gates(&format!(
        "[gates.check]\ncommand = {:?}\ninvariants = 'scripts/check-invariants.json'\n",
        env_dump_command(&dump)
    ));

    let (id, wt, _) = change_with_patchset(&repo, "binary-invariants");
    fs::write(
        wt.join("scripts/check-invariants.json"),
        b"\xff\xfe\x00not-utf8\n",
    )
    .unwrap();
    git(&wt, &["add", "scripts/check-invariants.json"]);
    git(
        &wt,
        &["commit", "-m", "feat: corrupt local invariants copy"],
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();

    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let seen: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&dump).unwrap()).unwrap();
    assert_eq!(seen["ARC_INVARIANTS"], "unit:one");

    let row = gate_row(&repo, &wt, &id, "check");
    assert_eq!(row["green_at_head"], true, "{row}");
}
