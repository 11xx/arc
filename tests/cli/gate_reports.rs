use crate::common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn declared(command: &str) -> Repo {
    let repo = Repo::new();
    repo.redeclare_gates(&format!("[gates.unit]\ncommand = {command:?}\nprofiles = ['local']\ntimeout = '2s'\n[gates.release]\ncommand = 'true'\nevents = ['publication']\n"));
    repo
}

fn row(report: &Value) -> &Value {
    report["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "unit")
        .unwrap()
}

fn copy_event(repo: &Repo, id: &str, source: &str, mut edit: impl FnMut(&mut Value)) {
    let directory = event_dir(repo, id);
    let mut events: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    events.sort();
    let mut value: Value = events
        .iter()
        .rev()
        .map(|p| serde_json::from_slice::<Value>(&fs::read(p).unwrap()).unwrap())
        .find(|v| v["event_type"] == source)
        .unwrap();
    let last: Value = serde_json::from_slice(&fs::read(events.last().unwrap()).unwrap()).unwrap();
    let next = last["event_id"]
        .as_str()
        .unwrap()
        .parse::<ulid::Ulid>()
        .unwrap();
    let next = ulid::Ulid::from(u128::from(next) + 1).to_string();
    value["event_id"] = json!(next);
    edit(&mut value);
    fs::write(
        directory.join(format!("{next}.json")),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
}

/// Claim: declarations without budgets expose command costs across changes and separate
/// reuse and selector skips. Rejects either skip counted as a run, legacy total time
/// preferred over command time, or a later failure mistaken for the first failure.
#[test]
fn declarations_and_costs_exclude_reuse_and_selection() {
    let repo = declared("true");
    let (id, wt, _) = change_with_patchset(&repo, "costs");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    // A separate change contributes another observed run.
    let (other_id, other, _) = change_with_patchset(&repo, "other-costs");
    repo.arc(&other)
        .args(["verify", "--all"])
        .assert()
        .success();
    rewrite_event(&repo, &other_id, "verification-recorded", |v| {
        v["run_ms"] = json!(40);
    });
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v["run_ms"] = json!(20);
        v["duration_ms"] = json!(9000);
    });
    repo.arc(&wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success();
    copy_event(&repo, &id, "verification-reused", |v| {
        v["delta"] = json!({"source_tree":"a", "target_tree":"b", "declaration":"d", "selector":"s", "changed_paths":[], "selected_gates":[], "output_sha256":"d", "output":"{}"});
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v.as_object_mut().unwrap().remove("run_ms");
        v["duration_ms"] = json!(100);
        v["result"] = json!("fail");
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["gate"] = json!("release");
        v["duration_ms"] = json!(1000);
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["gate"] = json!("unit");
        v["result"] = json!("not-admitted");
        v["run_ms"] = json!(50000);
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["attested"] = json!(true);
        v["result"] = json!("pass");
    });
    let before = tree_bytes(&repo.root.join(".git/arc"));
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(report["schema"], "arc-gates/1");
    assert_eq!(report["budget_declared"], false);
    let unit = row(&report);
    assert!(unit["budget_ms"].is_null());
    assert_eq!(unit["declaration"]["command"], "true");
    assert_eq!(unit["declaration"]["profiles"], json!(["local"]));
    assert_eq!(unit["declaration"]["timeout_seconds"], 2);
    assert_eq!(
        unit["declaration"]["events"],
        json!(["integration", "publication"])
    );
    let h = &unit["history"];
    assert_eq!(h["runs"], 3);
    assert_eq!(h["reused"], 1);
    assert_eq!(h["selected_out"], 1);
    assert_eq!(h["attested"], 1);
    assert_eq!(h["not_admitted"], 1);
    assert_eq!(h["timing"]["samples"], 3);
    assert_eq!(h["timing"]["median_ms"], 40);
    assert_eq!(h["timing"]["p90_ms"], 100);
    assert_eq!(h["timing"]["worst_ms"], 100);
    assert_eq!(h["timing"]["legacy_duration_samples"], 1);
    assert_eq!(h["ever_failed"], true);
    assert_eq!(h["last_failed_first"]["change_id"], id);
    let release = &report["gates"][0];
    assert_eq!(release["name"], "release");
    assert_eq!(release["history"]["ever_failed"], true);
    assert!(release["history"]["last_failed_first"].is_null());
    let text = stdout(repo.arc(&wt).arg("gates"));
    assert!(text.contains("budget: none declared"));
    assert!(text.contains("1 legacy duration_ms samples"));
    assert_eq!(before, tree_bytes(&repo.root.join(".git/arc")));
}

pub(super) fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    if !root.exists() {
        return files;
    }
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(tree_bytes(&path));
        } else {
            files.insert(path.clone(), fs::read(&path).unwrap());
        }
    }
    files
}

/// Claim: measurement executes exactly the requested rounds on the current tree,
/// prints wall, CPU, physical/logical cores and spread, and leaves ledger and output artifacts untouched.
/// Rejects recording verification evidence, measuring a different gate or tree,
/// or treating concurrent measurement as an unrecognized switch.
#[test]
fn measurement_is_transient_and_honors_selection_and_failures() {
    let repo = declared("test -f marker && printf x >> measured");
    let (_, wt, _) = change_with_patchset(&repo, "measure");
    fs::write(wt.join("marker"), "present").unwrap();
    let before = tree_bytes(&repo.root.join(".git/arc"));
    let text = stdout(
        repo.arc(&wt)
            .args(["gates", "measure", "--runs", "3", "--gate", "unit"]),
    );
    assert_eq!(fs::read(wt.join("measured")).unwrap(), b"xxx");
    assert!(text.contains("unit 3/3: wall"), "{text}");
    assert!(text.contains("CPU") && text.contains("spread"), "{text}");
    assert!(
        text.contains("host cores: physical ") && text.contains("; logical "),
        "{text}"
    );
    let topology = fs::read_to_string("/proc/cpuinfo").unwrap();
    let physical = topology
        .split("\n\n")
        .filter_map(|block| {
            let field = |name: &str| {
                block.lines().find_map(|line| {
                    line.split_once(':')
                        .filter(|(k, _)| k.trim() == name)
                        .map(|(_, v)| v.trim())
                })
            };
            Some((field("physical id")?, field("core id")?))
        })
        .collect::<std::collections::BTreeSet<_>>();
    if !physical.is_empty() {
        assert!(
            text.contains(&format!("physical {};", physical.len())),
            "{text}"
        );
    }
    assert!(!text.contains("CPU -"), "{text}");
    assert!(!text.contains("release 1/"));
    assert_eq!(before, tree_bytes(&repo.root.join(".git/arc")));
    fs::remove_file(wt.join("marker")).unwrap();
    repo.arc(&wt)
        .args(["gates", "measure", "--runs", "1", "--gate", "unit"])
        .assert()
        .code(1);
    repo.arc(&wt)
        .args(["gates", "measure", "--runs", "0"])
        .assert()
        .code(2);
    repo.arc(&wt)
        .args(["gates", "measure", "--gate", "missing"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("undeclared gate"));
    repo.arc(&wt)
        .args(["gates", "measure", "--concurrent"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("at least two concurrent gates"));
}

/// Claim: declarations can be reported before a ledger exists and unreadable changes
/// produce an explicit partial report. Rejects creating a ledger or hiding read errors.
#[test]
fn reports_without_a_ledger_and_preserves_partial_history() {
    let repo = declared("true");
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert_eq!(row(&report)["history"]["runs"], 0);
    assert!(row(&report)["history"]["timing"]["median_ms"].is_null());
    assert!(!repo.root.join(".git/arc/config.json").exists());
    let (id, wt, _) = change_with_patchset(&repo, "damaged");
    fs::write(event_dir(&repo, &id).join("broken.json"), "{").unwrap();
    let out = repo
        .arc(&wt)
        .args(["gates", "--json"])
        .assert()
        .code(16)
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["failures"][0]["change_id"], id);
}

/// Claim: workspace gate reports use each project's ledger and the registry's scope,
/// retirement and unreachable accounting. Rejects substituting the caller's ledger,
/// dropping projects without gates, observing retirees, or hiding orphaned anchors.
#[test]
fn workspace_uses_own_ledgers_scope_and_registration_accounting() {
    let repo = declared("true");
    let (_, wt, _) = change_with_patchset(&repo, "workspace-costs");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let second = repo.home.join("second");
    git(
        &repo.home,
        &[
            "clone",
            "-q",
            repo.root.to_str().unwrap(),
            second.to_str().unwrap(),
        ],
    );
    repo.arc(&second)
        .args(["begin", "second-costs", "--no-worktree"])
        .assert()
        .success();
    for _ in 0..2 {
        repo.arc(&second)
            .args(["verify", "--all", "--rerun"])
            .assert()
            .success();
    }
    let empty = repo.home.join("empty");
    let gone = repo.home.join("gone");
    let retired = repo.home.join("retired");
    let mut retired_journal = PathBuf::new();
    for anchor in [&empty, &gone, &retired] {
        fs::create_dir_all(anchor).unwrap();
        git(anchor, &["init", "-q", "-b", "master"]);
        repo.arc(anchor)
            .args(["journal", "note", "registration", "--body-file", "-"])
            .write_stdin("registered\n")
            .assert()
            .success();
        if anchor == &retired {
            retired_journal =
                PathBuf::from(stdout(repo.arc(anchor).args(["journal", "dir"])).trim());
        }
    }
    fs::remove_dir_all(empty.join(".git")).unwrap();
    fs::remove_dir_all(&gone).unwrap();
    fs::remove_dir_all(&retired).unwrap();
    repo.arc(&repo.root)
        .args(["workspace", "retire"])
        .arg(&retired_journal)
        .args(["--reason", "fixture removed"])
        .assert()
        .success();
    let before = tree_bytes(&repo.home.join(".local/ai"));
    let out = repo
        .arc(&repo.root)
        .args(["workspace", "gates", "--global", "--retired", "--json"])
        .assert()
        .code(16)
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], "arc-workspace-gates/1");
    let c = &report["collection"];
    assert_eq!(c["discovered"], 4);
    assert_eq!(c["selected"], 4);
    assert_eq!(c["non_empty"], 2);
    assert_eq!(c["empty"], 1);
    assert_eq!(c["failed"], 1);
    assert_eq!(c["retired"], 1);
    assert_eq!(report["unreachable"].as_array().unwrap().len(), 1);
    assert_eq!(report["retired"].as_array().unwrap().len(), 1);
    let projects = report["projects"].as_array().unwrap();
    let first = projects
        .iter()
        .find(|p| p["anchor"] == repo.root.to_str().unwrap())
        .unwrap();
    let second = projects
        .iter()
        .find(|p| p["anchor"] == second.to_str().unwrap())
        .unwrap();
    assert_eq!(row(&first["report"])["history"]["runs"], 1);
    assert_eq!(row(&second["report"])["history"]["runs"], 2);
    assert!(projects.iter().any(|p| p["declares_gates"] == false));
    assert_eq!(before, tree_bytes(&repo.home.join(".local/ai")));
    let scoped = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "gates", "--here", "--json"]),
    );
    assert_eq!(scoped["collection"]["selected"], 1);
    assert_eq!(scoped["collection"]["skipped"], 3);
    assert_eq!(scoped["collection"]["retired"], 0);
    let under = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "gates", "--under"])
            .arg(&repo.root)
            .arg("--json"),
    );
    assert_eq!(scoped, under);
}

/// Claim: gate cost reports read imported and local verification history with one
/// ledger observation, one repository read for completed landings, and no state
/// reduction. Rejects ignoring landing outcomes, fetching import batches per
/// change, or reducing states to read gate events.
#[test]
fn reports_hold_one_typed_ledger_read_without_reducing_states() {
    let source = declared("true");
    let (imported, _, _) = change_with_patchset(&source, "imported-costs");
    let bundle = source.home.join("gate-costs.json");
    source
        .arc(&source.root)
        .args(["export", &imported, "--output"])
        .arg(&bundle)
        .assert()
        .success();
    let repo = declared("true");
    deliver_git_objects(&source, &repo);
    repo.arc(&repo.root)
        .arg("import")
        .arg(&bundle)
        .assert()
        .success();
    change_with_patchset(&repo, "local-costs");
    for args in [
        vec!["gates", "--json"],
        vec!["workspace", "gates", "--global", "--json"],
    ] {
        let (trace, reads, code) = crate::landing_reads::traced(&repo, &repo.root, &args);
        assert_eq!(code, Some(0), "{args:?}: {trace}");
        assert_eq!(trace["ledger"]["import"]["decoded"], 1, "{args:?}: {trace}");
        assert_eq!(
            trace["cache"]["import_batches"]["hits"]
                .as_u64()
                .unwrap_or(0),
            0,
            "{args:?} fetched held import batches per change: {trace}"
        );
        assert_eq!(
            trace["counts"]["repository_materializations"], 1,
            "{args:?}: {trace}"
        );
        for key in ["changes_reduced", "scope_derivations"] {
            assert_eq!(
                trace["counts"][key].as_u64().unwrap_or(0),
                0,
                "{args:?}: {key}: {trace}"
            );
        }
        assert_eq!(
            trace["ledger"]["change"]["opened"], trace["ledger"]["change"]["decoded"],
            "{args:?}: {trace}"
        );
        for kind in ["change", "import"] {
            assert_eq!(reads["most_reads_per_file"][kind], 1, "{args:?}: {reads}");
        }
    }
}

/// Claim: local and workspace reports expose the lock name and effective admission
/// limit, including the lock's default, in text and JSON. Rejects omitted admission
/// fields, an invented lock on an unrestricted gate, or dropping nested declarations.
#[test]
fn reports_include_exclusive_and_effective_admission() {
    let repo = Repo::new();
    let name = format!("report-{}", ulid::Ulid::new());
    repo.redeclare_gates(&format!("[gates.default]\ncommand='true'\nexclusive={name:?}\n[gates.limited]\ncommand='true'\nexclusive={name:?}\nadmission='150ms'\n[gates.plain]\ncommand='true'\n"));
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert_eq!(report["schema"], "arc-gates/1");
    let declared = |name: &str| {
        &report["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap()["declaration"]
    };
    assert_eq!(
        declared("default")["exclusive"],
        name,
        "gate report omitted lock"
    );
    assert_eq!(
        declared("default")["admission_ms"],
        60_000,
        "gate report omitted effective admission"
    );
    assert_eq!(declared("limited")["exclusive"], name);
    assert_eq!(declared("limited")["admission_ms"], 150);
    for field in ["exclusive", "admission_ms"] {
        assert!(
            declared("plain").get(field).is_some_and(Value::is_null),
            "missing declaration field {field}: {report}"
        );
    }
    let text = stdout(repo.arc(&repo.root).arg("gates"));
    assert!(
        text.contains(&format!("exclusive: {name}; admission: 60.000s")),
        "{text}"
    );
    assert!(
        text.contains(&format!("exclusive: {name}; admission: 0.150s")),
        "{text}"
    );
    assert!(text.contains("exclusive: none; admission: none"), "{text}");
    assert!(!repo.root.join(".git/arc/config.json").exists());
    repo.arc(&repo.root)
        .args(["begin", "declarations", "--no-worktree"])
        .assert()
        .success();
    let workspace =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "gates", "--here", "--json"]),
        );
    assert_eq!(workspace["schema"], "arc-workspace-gates/1");
    assert_eq!(workspace["projects"][0]["report"]["gates"], report["gates"]);
    let workspace_text = stdout(repo.arc(&repo.root).args(["workspace", "gates", "--here"]));
    assert!(
        workspace_text.contains(&format!("exclusive: {name}; admission: 0.150s")),
        "{workspace_text}"
    );
}

/// Claim: init uses every requested serial round, defaults to six, and writes
/// headroom above the worst run without evidence. Rejects exact-worst,
/// last-round or cheapest-round budgets and partial writes after a failure.
#[test]
fn initialization_uses_complete_rounds_and_is_atomic() {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.unit]\ncommand='printf x >> rounds; case $(wc -c < rounds) in 1) sleep 0.01;; 2) sleep 0.08;; *) sleep 0.02;; esac'\n");
    let text = stdout(repo.arc(&repo.root).args(["gates", "init", "--runs", "3"]));
    assert_eq!(fs::read(repo.root.join("rounds")).unwrap(), b"xxx");
    let measured: Vec<u64> = text
        .lines()
        .filter_map(|line| line.split_once("run_ms=").map(|(_, n)| n.parse().unwrap()))
        .collect();
    assert_eq!(measured.len(), 3, "{text}");
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    let budget = report["gates"][0]["budget_ms"].as_u64().unwrap();
    let worst = *measured.iter().max().unwrap();
    let spread = worst - measured.iter().min().unwrap();
    assert!(
        budget >= worst + spread,
        "budget {budget} lacks spread {spread} above worst {worst}"
    );
    assert!(
        budget * 100 >= worst * 105,
        "budget {budget} lacks 5% headroom above worst {worst}"
    );
    assert!(
        text.contains("from 3 rounds") && text.contains("spread"),
        "{text}"
    );
    assert!(!repo.root.join(".git/arc/config.json").exists());

    let default = Repo::new();
    default.redeclare_gates("[gates.unit]\ncommand='printf x >> rounds'\n");
    let text = stdout(default.arc(&default.root).args(["gates", "init"]));
    assert_eq!(fs::read(default.root.join("rounds")).unwrap(), b"xxxxxx");
    assert!(text.contains("from 6 rounds"), "{text}");
    let failed = Repo::new();
    failed.redeclare_gates(
        "[gates.unit]\ncommand='printf x >> rounds; test $(wc -c < rounds) -ne 2'\n",
    );
    let before = fs::read(failed.root.join(".arc/gates.toml")).unwrap();
    failed
        .arc(&failed.root)
        .args(["gates", "init", "--runs", "3"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("round 2/3"));
    assert_eq!(
        fs::read(failed.root.join(".arc/gates.toml")).unwrap(),
        before
    );
    assert_eq!(fs::read(failed.root.join("rounds")).unwrap(), b"xx");
    failed
        .arc(&failed.root)
        .args(["gates", "init", "--runs", "0"])
        .assert()
        .code(2);
}

/// Claim: a single round receives the five-percent floor and upward rounding,
/// including a sub-millisecond run. Rejects an exact-worst or rounded-down budget.
#[test]
fn initialization_single_round_has_rounded_headroom() {
    for (measured, budget) in [(9522, 10000), (1000, 1100), (95, 100), (1, 2), (0, 1)] {
        let repo = Repo::new();
        repo.redeclare_gates("[gates.unit]\ncommand='true'\n");
        let text = stdout(
            repo.arc(&repo.root)
                .env("ARC_TEST_COMMAND_RUN_MS", measured.to_string())
                .args(["gates", "init", "--runs", "1"]),
        );
        let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
        assert_eq!(report["gates"][0]["budget_ms"], budget);
        assert_eq!(report["budget_rules"]["total"], budget);
        assert!(
            text.contains(&format!("worst {measured}ms, spread 0ms")),
            "{text}"
        );
    }
}

/// Claim: init inserts budgets while preserving every existing byte, including
/// comments, ordering, spacing and line endings, and a publication-only gate it
/// was asked to budget stays out of the total. Rejects reserialization of the
/// author's declaration or mistaking table-like text in a command for a header.
#[test]
fn initialization_preserves_the_authored_declaration() {
    for newline in ["\n", "\r\n"] {
        let repo = Repo::new();
        let original = "# project checks\n[gates.zeta] # first on purpose\npaths  = [ 'src/',\n  'tests/', ] # coverage\ncommand = '''true\n# [budget] is command text\n''' # shell\n\n# keep this with alpha\n[gates.'alpha.dot']\nevents = ['publication'] # release\ncommand='true' # fast\n\n[selection] # keep last\ninert = [ '**/*.md' ]\n# end\n".replace('\n', newline);
        repo.redeclare_gates(&original);
        let text = stdout(
            repo.arc(&repo.root)
                .env("ARC_TEST_COMMAND_RUN_MS", "1000")
                .args(["gates", "init", "--runs", "1", "--publication"]),
        );
        let written = fs::read_to_string(repo.root.join(".arc/gates.toml")).unwrap();
        let expected = original
            .replace(
                &format!("''' # shell{newline}"),
                &format!("''' # shell{newline}budget = \"1100ms\"{newline}"),
            )
            .replace(
                &format!("command='true' # fast{newline}"),
                &format!("command='true' # fast{newline}budget = \"1100ms\"{newline}"),
            )
            + &format!("[budget]{newline}total = \"1100ms\"{newline}");
        assert_eq!(written, expected, "init rewrote authored bytes");
        assert!(text.contains("budget total: 1.1s"), "{text}");
        assert!(!repo.root.join(".git/arc/config.json").exists());
    }
}

/// Claim: init measures and budgets the integration gates of every profile,
/// sets the total from those budgets, and skips and names each
/// publication-only gate unless `--publication` asks for it, which budgets it
/// outside the total. Rejects running or budgeting a publication-only gate
/// unasked, counting one in the total, or skipping one silently.
#[test]
fn initialization_budgets_integration_gates_and_names_skipped_publication_gates() {
    let declaration = "[gates.unit]\ncommand='true'\nprofiles=['local']\n\
                       [gates.lint]\ncommand='true'\nprofiles=['ci']\n\
                       [gates.release]\ncommand='printf x >> released'\nevents=['publication']\n";
    let repo = Repo::new();
    repo.redeclare_gates(declaration);
    let text = stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_COMMAND_RUN_MS", "1000")
            .args(["gates", "init", "--runs", "1"]),
    );
    assert!(
        text.contains("gate release: skipped, publication only; --publication budgets it"),
        "{text}"
    );
    assert!(!repo.root.join("released").exists(), "{text}");
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    let budget = |report: &Value, name: &str| {
        report["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == name)
            .unwrap()["budget_ms"]
            .clone()
    };
    assert_eq!(budget(&report, "unit"), 1100);
    assert_eq!(budget(&report, "lint"), 1100);
    assert!(budget(&report, "release").is_null(), "{report}");
    assert_eq!(report["budget_rules"]["total"], 2200);
    assert_eq!(report["budget_errors"], json!([]));

    let asked = Repo::new();
    asked.redeclare_gates(declaration);
    let text = stdout(
        asked
            .arc(&asked.root)
            .env("ARC_TEST_COMMAND_RUN_MS", "1000")
            .args(["gates", "init", "--runs", "1", "--publication"]),
    );
    assert!(!text.contains("skipped"), "{text}");
    assert!(
        text.contains("gate release: budget 1.1s from 1 rounds; min 1000ms, worst 1000ms, spread 0ms; publication only, outside the total"),
        "{text}"
    );
    let report = json_stdout(asked.arc(&asked.root).args(["gates", "--json"]));
    assert_eq!(budget(&report, "release"), 1100);
    assert_eq!(report["budget_rules"]["total"], 2200);
}

/// Claim: inline and dotted gate declarations receive budgets without changing
/// their existing text, even at EOF. Rejects rewriting valid TOML layouts or
/// inserting a budget into the wrong table.
#[test]
fn initialization_preserves_inline_and_dotted_layouts() {
    for (original, insertion, expected) in [
        (
            "gates = { unit = {command='true'} } # inline",
            ", budget = \"1100ms\"",
            "gates = { unit = {command='true', budget = \"1100ms\"} } # inline",
        ),
        (
            "gates.'unit.dot'.command = 'true' # dotted\n",
            "gates.\"unit.dot\".budget = \"1100ms\"\n",
            "gates.'unit.dot'.command = 'true' # dotted\ngates.\"unit.dot\".budget = \"1100ms\"\n",
        ),
        (
            "[gates]\nunit.command='true' # dotted\n",
            "unit.budget = \"1100ms\"\n",
            "[gates]\nunit.command='true' # dotted\nunit.budget = \"1100ms\"\n",
        ),
        (
            "[gates.unit]\ncommand='true' # EOF",
            "\nbudget = \"1100ms\"\n",
            "[gates.unit]\ncommand='true' # EOF\nbudget = \"1100ms\"\n",
        ),
    ] {
        let repo = Repo::new();
        repo.redeclare_gates(original);
        repo.arc(&repo.root)
            .env("ARC_TEST_COMMAND_RUN_MS", "1000")
            .args(["gates", "init", "--runs", "1"])
            .assert()
            .success();
        let written = fs::read_to_string(repo.root.join(".arc/gates.toml")).unwrap();
        let end = if expected.ends_with('\n') { "" } else { "\n" };
        assert_eq!(
            written,
            format!("{expected}{end}[budget]\ntotal = \"1100ms\"\n"),
            "insertion {insertion:?}"
        );
    }
}

/// Claim: receipt diagnosis separates waiting, queueing, preparation and duplicate
/// executions with numbers, and leaves absent fields unmeasured. Rejects treating
/// reused evidence or legacy duration as observed command/CPU time, or excluding
/// integrated changes that needed no execution from the duplication denominator.
#[test]
fn receipt_cost_kinds_and_missing_fields_are_explicit() {
    let repo = declared("true");
    let (id, wt, _) = change_with_patchset(&repo, "diagnose");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v["run_ms"] = json!(1000);
        v["cpu_user_ms"] = json!(80);
        v["cpu_system_ms"] = json!(20);
        v["wait_ms"] = json!(200);
        v["prepare_ms"] = json!(150);
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v.as_object_mut().unwrap().remove("run_id");
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["event_type"] = json!("change-closed");
        v["outcome"] = json!("integrated");
    });
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let diagnosis = &row(&report)["diagnosis"];
    assert_eq!(
        diagnosis["kinds"],
        json!(["waiting", "queueing", "preparation", "duplication"])
    );
    assert_eq!(diagnosis["wall"]["median_ms"], 1000);
    assert_eq!(diagnosis["cpu"]["median_ms"], 100);
    assert_eq!(diagnosis["wait"]["median_ms"], 200);
    assert_eq!(diagnosis["preparation"]["median_ms"], 150);
    assert_eq!(diagnosis["runs_per_integrated_change"], 2.0);
    let text = stdout(repo.arc(&wt).args(["gates", "over", &id, "--gate", "unit"]));
    assert!(
        text.contains("waiting, queueing, preparation, duplication") && text.contains("1000ms"),
        "{text}"
    );
    assert!(text.contains("get the number first; measure serially; remove waste; rank checks by what they guard and cut from the bottom, never by cost alone; make what matters fit; lower concurrency until nothing fails; stop at a named floor; raise the budget last"), "{text}");
    let (other_id, _, _) = change_with_patchset(&repo, "no-executions");
    copy_event(&repo, &other_id, "patchset-added", |v| {
        v["event_type"] = json!("change-closed");
        v["outcome"] = json!("integrated");
    });
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(row(&report)["diagnosis"]["integrated_changes"], 2);
    assert_eq!(row(&report)["diagnosis"]["runs_per_integrated_change"], 1.0);
    for event in fs::read_dir(event_dir(&repo, &id)).unwrap() {
        let path = event.unwrap().path();
        let mut v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if v["event_type"] != "verification-recorded" {
            continue;
        }
        v["cpu_user_ms"] = json!(700);
        v["cpu_system_ms"] = json!(300);
        v["wait_ms"] = json!(99);
        v["prepare_ms"] = json!(99);
        fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    }
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(row(&report)["diagnosis"]["kinds"], json!(["real-work"]));
    for event in fs::read_dir(event_dir(&repo, &id)).unwrap() {
        let path = event.unwrap().path();
        let mut v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if v["event_type"] != "verification-recorded" {
            continue;
        }
        for key in [
            "run_ms",
            "cpu_user_ms",
            "cpu_system_ms",
            "wait_ms",
            "prepare_ms",
        ] {
            v.as_object_mut().unwrap().remove(key);
        }
        v["duration_ms"] = json!(99999);
        fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    }
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let diagnosis = &row(&report)["diagnosis"];
    assert!(diagnosis["wall"]["median_ms"].is_null(), "{diagnosis}");
    assert_eq!(
        diagnosis["unmeasured"],
        json!(["waiting/real-work", "queueing", "preparation"])
    );
}

/// Claim: timings receipts preserve bounded per-check cost and failed-change history,
/// and reuse contributes no extra observations. Rejects losing failures, counting
/// reuses as check executions, or making an advisory timings path change identity.
#[test]
fn check_costs_and_catches_are_retained_without_changing_identity() {
    let repo = Repo::new();
    let command =
        "printf 'arc-check-timings/1\\nfast\\t5\\tpass\\nslow\\t1200\\tfail\\n' > checks.tsv";
    repo.redeclare_gates(&format!(
        "[gates.unit]\ncommand={command:?}\ntimings='checks.tsv'\npaths=['src/']\n"
    ));
    repo.commit(
        &repo.root,
        ".gitignore",
        "checks.tsv\n",
        "test: ignore telemetry",
    );
    let (id, wt, _) = change_with_patchset(&repo, "checks");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let shown = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    let checks = &shown["verifications"][0]["check_timings"];
    assert_eq!(checks["checks"].as_array().unwrap().len(), 2, "{shown}");
    assert_eq!(checks["checks"][0]["name"], "slow");
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand={command:?}\npaths=['src/']\ntimings='other.tsv'\n"
    ));
    repo.arc(&wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped"));
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand={command:?}\npaths=['src/']\ntimings='checks.tsv'\n"
    ));
    let (other_id, other_wt, _) = change_with_patchset(&repo, "other-checks");
    repo.arc(&other_wt)
        .args(["verify", "--all"])
        .assert()
        .success();
    rewrite_event(&repo, &other_id, "verification-recorded", |v| {
        v["check_timings"]["checks"][0]["ms"] = json!(1400);
        v["check_timings"]["checks"][0]["failed"] = json!(false);
    });
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let costs = &report["gates"][0]["checks"];
    assert_eq!(costs["measured_runs"], 2, "{report}");
    assert_eq!(costs["slowest"][0]["median_ms"], 1200);
    assert_eq!(costs["slowest"][0]["runs"], 2);
    assert_eq!(costs["slowest"][0]["failed_on"], json!([id]));
    let over = stdout(repo.arc(&wt).args(["gates", "over"]));
    assert!(
        over.contains("never failed in 2 runs, costs 0.005 seconds"),
        "{over}"
    );
    assert!(
        over.contains("failed on") && over.contains("1.200 seconds"),
        "{over}"
    );
}

/// Claim: malformed, missing, stale, nonregular, oversized and over-entry-limit
/// files remain advisory, while compact receipts retain at most 32 checks and name
/// drops. Rejects failing a passing gate, stale reuse or unbounded receipt storage.
#[test]
fn check_timings_are_bounded_fresh_and_advisory() {
    let repo = Repo::new();
    let command = "case $(cat mode) in missing) :;; malformed) printf 'arc-check-timings/1\\nbad' > checks.tsv;; oversized) head -c 65537 /dev/zero > checks.tsv;; entries) { printf 'arc-check-timings/1\\n'; seq 1 513 | while read n; do printf 'c%s\\t1\\tpass\\n' \"$n\"; done; } > checks.tsv;; capped) { printf 'arc-check-timings/1\\n'; seq 1 40 | while read n; do printf 'c%s\\t%s\\tpass\\n' \"$n\" \"$n\"; done; printf 'caught\\t1\\tfail\\n'; } > checks.tsv;; stale) :;; fifo) rm -f checks.tsv; mkfifo checks.tsv;; esac";
    repo.redeclare_gates(&format!(
        "[gates.unit]\ncommand={command:?}\ntimings='checks.tsv'\n"
    ));
    let (id, wt, _) = change_with_patchset(&repo, "bounded");
    for mode in [
        "missing",
        "malformed",
        "oversized",
        "entries",
        "capped",
        "stale",
        "fifo",
    ] {
        fs::write(wt.join("mode"), mode).unwrap();
        repo.arc(&wt)
            .args(["verify", "--all", "--rerun"])
            .assert()
            .success();
        let shown = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
        let checks = &shown["verifications"].as_array().unwrap().last().unwrap()["check_timings"];
        if mode == "capped" {
            assert_eq!(checks["checks"].as_array().unwrap().len(), 32, "{checks}");
            assert_eq!(checks["dropped"], 9);
            assert_eq!(checks["checks"][0]["name"], "caught");
        } else {
            assert!(checks["error"].is_string(), "{mode}: {checks}");
            assert!(
                checks["checks"].as_array().unwrap().is_empty(),
                "{mode}: {checks}"
            );
        }
    }
}

/// Claim: gates over CHANGE diagnoses the selected overrun's declarations from
/// any checkout, including an explicitly named gate present only on the change.
/// Rejects omitting named-gate observations or using the invoking checkout's
/// older declaration set for its check timings.
#[test]
fn over_from_another_checkout_includes_change_only_checks() {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.base]\ncommand='true'\n");
    repo.commit(
        &repo.root,
        ".gitignore",
        "checks.tsv\n",
        "test: ignore observations",
    );
    let (id, wt, _) = change_with_patchset(&repo, "change-only-checks");
    let command = "printf 'arc-check-timings/1\\nchange-witness\\t25\\tpass\\n' > checks.tsv";
    repo.commit(&wt, ".arc/gates.toml", &format!("[gates.base]\ncommand='true'\n[gates.added]\ncommand={command:?}\nbudget='1s'\ntimings='checks.tsv'\n"), "test: declare an added gate");
    repo.arc(&wt).args(["snapshot", &id]).assert().success();
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "2000")
        .args(["verify", "--gate", "added"])
        .assert()
        .code(1);
    let report = stdout(
        repo.arc(&repo.root)
            .args(["gates", "over", &id, "--gate", "added"]),
    );
    assert!(report.contains("added cost kinds"), "{report}");
    assert!(
        report.contains("change-witness")
            && report.contains("never failed in 1 runs, costs 0.025 seconds"),
        "{report}"
    );
}

fn change_events(repo: &Repo, id: &str) -> Vec<Value> {
    let mut paths: Vec<_> = fs::read_dir(event_dir(repo, id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
        .collect()
}

/// Claim: every event an arc command writes names that command's invocation,
/// `inv-` and a lowercase ULID shared by all of its events, and how long it
/// had been running when it wrote each one. Rejects an unstamped event, an id
/// per event or shared across commands, and a running time longer than the
/// command took or running backwards within it.
#[test]
fn each_event_names_its_command_invocation_and_running_time() {
    let repo = declared("true");
    let (id, wt, _) = change_with_patchset(&repo, "stamped");
    let started = std::time::Instant::now();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let wall_ms = started.elapsed().as_millis() as u64;
    let events = change_events(&repo, &id);
    for event in &events {
        let invocation = event["invocation_id"].as_str().unwrap_or_default();
        let ulid = invocation.strip_prefix("inv-").unwrap_or_default();
        assert!(ulid.len() == 26 && ulid == ulid.to_lowercase(), "{event}");
        assert!(event["invocation_elapsed_ms"].is_u64(), "{event}");
    }
    let of = |kind: &str| {
        events
            .iter()
            .find(|event| event["event_type"] == kind)
            .unwrap()
    };
    let (opened, run, recorded) = (
        of("change-opened"),
        of("verification-run-started"),
        of("verification-recorded"),
    );
    assert_eq!(run["invocation_id"], recorded["invocation_id"]);
    assert_ne!(opened["invocation_id"], recorded["invocation_id"]);
    assert_ne!(
        of("patchset-added")["invocation_id"],
        recorded["invocation_id"]
    );
    let elapsed = |event: &Value| event["invocation_elapsed_ms"].as_u64().unwrap();
    assert!(elapsed(run) <= elapsed(recorded), "{run} {recorded}");
    assert!(elapsed(recorded) <= wall_ms, "{recorded} ran {wall_ms}ms");
}

fn close_at(repo: &Repo, id: &str, at: chrono::DateTime<chrono::Utc>) {
    copy_event(repo, id, "patchset-added", |v| {
        v["event_type"] = json!("change-closed");
        v["outcome"] = json!("integrated");
        v["created_at"] = json!(at);
    });
}

/// Claim: a landing pays every executed integration gate, including failed reruns
/// and removed declarations, exactly once. Rejects charging reuse, publication,
/// attestation or denied admission, substituting legacy duration for run_ms,
/// or claiming a window percentile that missing measurements could change.
#[test]
fn landing_cost_counts_executions_and_preserves_missing_measurements() {
    let repo = declared("true");
    let (id, wt, _) = change_with_patchset(&repo, "paid");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v["run_ms"] = json!(20);
    });
    repo.arc(&wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["verify", "--all", "--event", "publication"])
        .assert()
        .success();
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["gate"] = json!("removed-gate");
        if v["work_identity"].is_object() {
            v["work_identity"]["gate"] = json!("removed-gate");
        }
        v.as_object_mut().unwrap().remove("run_id");
        v["result"] = json!("fail");
        v["run_ms"] = json!(30);
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["result"] = json!("not-admitted");
        v["run_ms"] = json!(9000);
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["attested"] = json!(true);
        v["result"] = json!("pass");
    });
    close_at(&repo, &id, chrono::Utc::now());
    let (zero, _, _) = change_with_patchset(&repo, "zero-paid");
    close_at(&repo, &zero, chrono::Utc::now());
    let (legacy, legacy_wt, _) = change_with_patchset(&repo, "unknown-paid");
    repo.arc(&legacy_wt)
        .args(["verify", "--all"])
        .assert()
        .success();
    rewrite_event(&repo, &legacy, "verification-recorded", |v| {
        v.as_object_mut().unwrap().remove("run_ms");
        v["duration_ms"] = json!(5000);
    });
    close_at(&repo, &legacy, chrono::Utc::now());
    let before = tree_bytes(&repo.root.join(".git/arc"));
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let landing = &report["landing"];
    assert_eq!(landing["landings"], 3);
    assert_eq!(landing["unmeasured"], 1);
    assert_eq!(landing["timing"]["samples"], 2);
    assert!(landing["timing"]["median_ms"].is_null());
    assert!(landing["timing"]["p90_ms"].is_null());
    assert!(landing["timing"]["worst_ms"].is_null());
    assert_eq!(landing["zero_execution_share"], 1.0 / 3.0);
    let paid = landing["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["change_id"] == id)
        .unwrap();
    assert_eq!(paid["executions"], 2);
    assert_eq!(paid["run_ms"], 50);
    assert!(landing["last_7_days"]["paid_ms"].is_null());
    assert_eq!(landing["last_30_days"]["measured_ms"], 50);
    assert_eq!(landing["last_30_days"]["unmeasured"], 1);
    let text = stdout(repo.arc(&wt).arg("gates"));
    assert!(text.contains("3 landings, 1 unmeasured"), "{text}");
    assert!(text.contains("paid -; measured 0.050s"), "{text}");
    assert_eq!(before, tree_bytes(&repo.root.join(".git/arc")));
}

/// Rewrite the running time of every event of `id` from its shipped patchset
/// on: `elapsed` names it by event type, and any other event reads 50ms.
fn set_running_times(repo: &Repo, id: &str, elapsed: &[(&str, u64)]) {
    let mut paths: Vec<_> = fs::read_dir(event_dir(repo, id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    let mut started = false;
    for path in paths {
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let kind = value["event_type"].as_str().unwrap().to_string();
        started |= kind == "patchset-added";
        if !started {
            continue;
        }
        let ms = elapsed
            .iter()
            .find(|(named, _)| *named == kind)
            .map_or(50, |(_, ms)| *ms);
        value["invocation_elapsed_ms"] = json!(ms);
        fs::write(&path, json_file_bytes(&value)).unwrap();
    }
}

/// A landing whose snapshot ran 400ms, whose verify ran 2500ms around a
/// 1000ms gate with 300ms preparation, and whose integration ran 700ms.
fn timed_landing(repo: &Repo, slug: &str) -> (String, PathBuf) {
    let (id, wt, _) = change_with_patchset(repo, slug);
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "1000")
        .args(["verify", "--all"])
        .assert()
        .success();
    rewrite_event(repo, &id, "verification-recorded", |v| {
        v["prepare_ms"] = json!(300);
    });
    close_at(repo, &id, chrono::Utc::now());
    rewrite_event(repo, &id, "change-closed", |v| {
        v["invocation_id"] = json!(format!(
            "inv-{}",
            ulid::Ulid::new().to_string().to_lowercase()
        ));
    });
    set_running_times(
        repo,
        &id,
        &[
            ("patchset-added", 400),
            ("verification-run-started", 100),
            ("verification-recorded", 2500),
            ("change-closed", 700),
        ],
    );
    (id, wt)
}

/// Claim: a landing's own time is, for each command from the one that
/// recorded the shipped patchset through the integration event, the longest
/// running time among its events, less the gate run and preparation time it
/// recorded, summed; landing time adds gate and preparation, in `arc gates`
/// per landing, window and period, text and JSON, and in `arc show`, while
/// the landing budget keeps judging gate time. Rejects summing every event's
/// running time, a command's first event standing for it, gate or
/// preparation time counted twice, and a budget judging the sum.
#[test]
fn landing_time_is_gate_preparation_and_each_commands_longest_running_time() {
    let repo = Repo::new();
    repo.redeclare_gates("[budget]\nlanding = '2s'\n[gates.unit]\ncommand = 'true'\n");
    let (id, wt) = timed_landing(&repo, "timed");
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let l = &report["landing"];
    let change = &l["changes"][0];
    assert_eq!(change["run_ms"], 1000, "{change}");
    assert_eq!(change["prepare_ms"], 300, "{change}");
    assert_eq!(
        change["own_ms"],
        400 + (2500 - 1000 - 300) + 700,
        "{change}"
    );
    assert_eq!(change["total_ms"], 3600, "{change}");
    assert_eq!(l["own"]["median_ms"], 2300, "{l}");
    assert_eq!(l["total"]["median_ms"], 3600, "{l}");
    assert_eq!(l["own_unmeasured"], 0);
    assert_eq!(l["median_exceeds_budget"], false, "{l}");
    assert_eq!(l["last_7_days"]["own_paid_ms"], 2300);
    assert_eq!(l["last_7_days"]["total_paid_ms"], 3600);
    let text = stdout(repo.arc(&wt).arg("gates"));
    assert!(
        text.contains("arc's own commands: 0 unmeasured; median 2.300s, p90 2.300s"),
        "{text}"
    );
    assert!(
        text.contains("landing time (gate, preparation and arc's own): 0 unmeasured; median 3.600s, p90 3.600s; reported, not budgeted: the landing budget judges gate time"),
        "{text}"
    );
    assert!(
        text.contains("arc's own paid 2.300s; measured 2.300s; 0 unmeasured; landing time paid 3.600s; 0 unmeasured"),
        "{text}"
    );
    let shown = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    assert_eq!(
        shown["landing_time"],
        json!({"gate_ms": 1000, "prepare_ms": 300, "own_ms": 2300, "total_ms": 3600})
    );
    let text = stdout(repo.arc(&wt).args(["show", &id]));
    assert!(
        text.contains("## Landing time\n\ngate 1.000s, preparation 0.300s, arc's own commands 2.300s: landing time 3.600s; reported, not budgeted: the landing budget judges gate time"),
        "{text}"
    );
}

/// Claim: a landing with an event that names no invocation has no own time
/// or landing time, its events still load, and the window states a
/// percentile or a period total of either only when no value the unmeasured
/// landing could hold would change it. Rejects reading unstamped events as
/// zero, a window median the unmeasured landing could move, and a refusal to
/// read history written before the stamp.
#[test]
fn a_landing_with_unstamped_events_reads_unmeasured() {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.unit]\ncommand = 'true'\n");
    let (_, wt) = timed_landing(&repo, "stamped");
    let (legacy, _) = timed_landing(&repo, "legacy");
    rewrite_event(&repo, &legacy, "verification-run-started", |v| {
        let object = v.as_object_mut().unwrap();
        object.remove("invocation_id");
        object.remove("invocation_elapsed_ms");
    });
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(report["failures"], json!([]), "{report}");
    let l = &report["landing"];
    let row = |change: &str| {
        l["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["change_id"] == change)
            .unwrap()
            .clone()
    };
    let unstamped = row(&legacy);
    assert!(unstamped["own_ms"].is_null(), "{unstamped}");
    assert!(unstamped["total_ms"].is_null(), "{unstamped}");
    assert_eq!(unstamped["run_ms"], 1000, "{unstamped}");
    assert_eq!(l["own_unmeasured"], 1, "{l}");
    assert_eq!(l["own"]["samples"], 1, "{l}");
    assert!(l["own"]["median_ms"].is_null(), "{l}");
    assert!(l["total"]["median_ms"].is_null(), "{l}");
    assert_eq!(l["timing"]["median_ms"], 1000, "{l}");
    assert!(l["last_7_days"]["own_paid_ms"].is_null(), "{l}");
    assert!(l["last_7_days"]["total_paid_ms"].is_null(), "{l}");
    assert_eq!(l["last_7_days"]["measured_own_ms"], 2300, "{l}");
    let shown = json_stdout(repo.arc(&wt).args(["show", &legacy, "--json"]));
    assert!(shown["landing_time"]["own_ms"].is_null(), "{shown}");
    assert_eq!(shown["landing_time"]["gate_ms"], 1000, "{shown}");
}

/// Claim: landing declarations are destination-owned, combined with the operator
/// by minimum, and only advise on excessive median cost. Rejects change-head
/// overrides, required per-gate budgets, raise requirements or a doctor error.
#[test]
fn landing_budget_is_destination_owned_and_advisory() {
    let repo = Repo::new();
    repo.redeclare_gates("[budget]\nlanding = '10ms'\n[gates.unit]\ncommand = 'true'\n");
    let (id, wt, _) = change_with_patchset(&repo, "typical");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v["run_ms"] = json!(50);
    });
    close_at(&repo, &id, chrono::Utc::now());
    let (_, open, _) = change_with_patchset(&repo, "advisory-edit");
    fs::write(
        open.join(".arc/gates.toml"),
        "[budget]\nlanding = '999s'\n[gates.unit]\ncommand = 'true'\n",
    )
    .unwrap();
    git(&open, &["add", ".arc/gates.toml"]);
    git(&open, &["commit", "-qm", "test: edit typical budget"]);
    repo.arc(&open).args(["verify", "--all"]).assert().success();
    let report = json_stdout(repo.arc(&open).args(["gates", "--json"]));
    assert_eq!(report["landing"]["budget_destination"], "master");
    assert_eq!(report["landing"]["budget_ms"], 10);
    assert_eq!(report["landing"]["median_exceeds_budget"], true);
    assert_eq!(report["budget_errors"], json!([]));
    assert_eq!(report["budget_declared"], false);
    let doctor = json_stdout_any_status(repo.arc(&open).args(["doctor", "--json"]));
    assert!(
        doctor["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "landing-budget-exceeded"),
        "{doctor}"
    );
    assert!(
        !doctor["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "gate-budget-invalid"),
        "{doctor}"
    );
    repo.declare_gates_locally("[budget]\nlanding = '5ms'\n");
    let report = json_stdout(repo.arc(&open).args(["gates", "--json"]));
    assert_eq!(report["landing"]["budget_ms"], 5);
    repo.declare_gates_locally("[budget]\nlanding = '20ms'\n");
    let report = json_stdout(repo.arc(&open).args(["gates", "--json"]));
    assert_eq!(report["landing"]["budget_ms"], 10);
}

/// Claim: `arc gates` reports the preparation each landing paid beside its gate
/// time, from prepare_ms on the same receipts, per landing, per gate and in the
/// 7/30-day totals, and the advisory landing budget judges gate time alone.
/// Rejects a report that hides preparation, a preparation percentile an
/// unmeasured landing could change, and a landing budget judging preparation.
#[test]
fn landing_cost_reports_preparation_beside_gate_time() {
    let repo = Repo::new();
    repo.redeclare_gates("[budget]\nlanding = '10ms'\n[gates.unit]\ncommand = 'true'\n");
    let land = |slug: &str, prepare_ms: Option<u64>| {
        let (id, wt, _) = change_with_patchset(&repo, slug);
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
        rewrite_event(&repo, &id, "verification-recorded", |v| {
            v["run_ms"] = json!(5);
            match prepare_ms {
                Some(ms) => v["prepare_ms"] = json!(ms),
                None => {
                    v.as_object_mut().unwrap().remove("prepare_ms");
                }
            }
        });
        close_at(&repo, &id, chrono::Utc::now());
        wt
    };
    let wt = land("prepared", Some(300));
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let l = &report["landing"];
    assert_eq!(l["timing"]["median_ms"], 5);
    assert_eq!(l["preparation"]["median_ms"], 300);
    assert_eq!(l["prepare_unmeasured"], 0);
    assert_eq!(l["median_exceeds_budget"], false);
    assert_eq!(l["last_7_days"]["prepare_paid_ms"], 300);
    assert_eq!(l["changes"][0]["prepare_ms"], 300);
    let unit = report["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "unit")
        .unwrap();
    assert_eq!(unit["history"]["preparation"]["median_ms"], 300);
    let text = stdout(repo.arc(&wt).arg("gates"));
    assert!(
        text.contains("preparation: 0 unmeasured; median 0.300s, p90 0.300s; outside the landing budget, which judges gate time"),
        "{text}"
    );
    assert!(
        text.contains("preparation paid 0.300s; measured 0.300s; 0 unmeasured"),
        "{text}"
    );

    land("unprepared", None);
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let l = &report["landing"];
    assert_eq!(l["timing"]["median_ms"], 5);
    assert_eq!(l["prepare_unmeasured"], 1);
    assert!(l["preparation"]["median_ms"].is_null(), "{l}");
    assert!(l["last_30_days"]["prepare_paid_ms"].is_null(), "{l}");
    assert_eq!(l["last_30_days"]["measured_prepare_ms"], 300);
    assert_eq!(l["last_30_days"]["prepare_unmeasured"], 1);
}

/// Claim: reports use the latest 100 landings while 7/30-day totals include every
/// landing in each period. Rejects an unbounded median, execution-date charging,
/// oldest-first selection, missing zero landings or a zero median for no history.
#[test]
fn landing_window_and_calendar_periods_are_independent() {
    let repo = declared("true");
    let empty = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert!(empty["landing"]["timing"]["median_ms"].is_null());
    assert!(empty["landing"]["zero_execution_share"].is_null());
    let (id, wt, _) = change_with_patchset(&repo, "window");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let now = chrono::Utc::now();
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v["run_ms"] = json!(1000);
        v["created_at"] = json!(now - chrono::Duration::days(60));
    });
    close_at(&repo, &id, now - chrono::Duration::days(10));
    let template = fs::read_dir(event_dir(&repo, &id))
        .unwrap()
        .map(|e| serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).unwrap()).unwrap())
        .collect::<Vec<_>>();
    for i in 0..101 {
        let change = format!("window-{i}");
        let dir = event_dir(&repo, &change);
        fs::create_dir_all(&dir).unwrap();
        for e in template
            .iter()
            .filter(|e| e["event_type"] == "change-opened" || e["event_type"] == "change-closed")
        {
            let mut e = e.clone();
            let event_id = ulid::Ulid::new().to_string();
            e["event_id"] = json!(event_id);
            e["change_id"] = json!(change);
            e["created_at"] = json!(now - chrono::Duration::hours(i + 1));
            fs::write(
                dir.join(format!("{event_id}.json")),
                serde_json::to_vec(&e).unwrap(),
            )
            .unwrap();
        }
    }
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let l = &report["landing"];
    assert_eq!(l["window"], 100);
    assert_eq!(l["landings"], 100);
    assert_eq!(l["timing"]["median_ms"], 0);
    assert_eq!(l["timing"]["p90_ms"], 0);
    assert_eq!(l["zero_execution_share"], 1.0);
    assert_eq!(l["last_7_days"]["landings"], 101);
    assert_eq!(l["last_7_days"]["paid_ms"], 0);
    assert_eq!(l["last_30_days"]["landings"], 102);
    assert_eq!(l["last_30_days"]["paid_ms"], 1000);
}

/// Claim: each completed stack member is a landing and shared execution is paid
/// once by its recording member. Rejects missing derived closures or duplicating
/// the shared receipt across members.
#[test]
fn landing_stack_members_charge_shared_execution_once() {
    let repo = declared("true");
    let (a, a_wt, a_head) = change_with_patchset(&repo, "paid-stack-a");
    let b = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "paid-stack-b",
        "--base",
        &a_head,
    ])));
    let b_wt = repo.home.join(".worktrees/repo-paid-stack-b");
    repo.commit(&b_wt, "stack-b.txt", "b\n", "test: stack member");
    repo.arc(&b_wt).args(["snapshot", &b]).assert().success();
    for (id, wt) in [(&a, &a_wt), (&b, &b_wt)] {
        repo.arc(wt)
            .args(["review", id, "--verdict", "approved"])
            .assert()
            .success();
    }
    repo.arc(&repo.root)
        .args(["land", &a, &b])
        .assert()
        .success();
    rewrite_event(&repo, &b, "verification-recorded", |v| {
        v["run_ms"] = json!(20);
    });
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert_eq!(report["landing"]["landings"], 2);
    assert_eq!(report["landing"]["zero_execution_share"], 0.5);
    assert_eq!(report["landing"]["timing"]["median_ms"], 0);
    assert_eq!(report["landing"]["timing"]["p90_ms"], 20);
    assert_eq!(report["landing"]["last_30_days"]["paid_ms"], 20);
}

/// Claim: workspace projects rank by measured paid time in 30 days, expose the
/// total and landing distribution, and retain unreadable projects as unobserved.
/// Rejects registry order, caller-ledger substitution or an unreadable zero cost.
#[test]
fn workspace_ranks_paid_landings_and_retains_unobserved_projects() {
    let repo = Repo::new();
    repo.redeclare_gates("[budget]\ntotal='2s'\n[gates.unit]\ncommand='true'\nbudget='1s'\n");
    let (id, wt, _) = change_with_patchset(&repo, "cheap-paid");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v["run_ms"] = json!(10);
    });
    close_at(&repo, &id, chrono::Utc::now());
    let other = repo.home.join("expensive");
    git(
        &repo.home,
        &[
            "clone",
            "-q",
            repo.root.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    let expensive = opened_change_id(&stdout(repo.arc(&other).args([
        "begin",
        "expensive-paid",
        "--no-worktree",
    ])));
    repo.arc(&other)
        .args(["verify", "--all"])
        .assert()
        .success();
    let other_events = other
        .join(".git/arc/changes")
        .join(&expensive)
        .join("events");
    let mut receipt = None;
    for entry in fs::read_dir(&other_events).unwrap() {
        let path = entry.unwrap().path();
        let mut e: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if e["event_type"] == "verification-recorded" {
            e["run_ms"] = json!(1000);
            fs::write(path, serde_json::to_vec(&e).unwrap()).unwrap();
            receipt = Some(e);
        }
    }
    let mut e = receipt.unwrap();
    e["event_type"] = json!("change-closed");
    e["event_id"] = json!(ulid::Ulid::new().to_string());
    e["outcome"] = json!("integrated");
    e["created_at"] = json!(chrono::Utc::now());
    fs::write(
        other_events.join(format!("{}.json", e["event_id"].as_str().unwrap())),
        serde_json::to_vec(&e).unwrap(),
    )
    .unwrap();
    let report =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "gates", "--global", "--json"]),
        );
    let projects = report["projects"].as_array().unwrap();
    assert_eq!(projects[0]["anchor"], other.to_str().unwrap());
    assert_eq!(projects[0]["report"]["budget_rules"]["total"], 2000);
    assert_eq!(
        projects[0]["report"]["landing"]["timing"]["median_ms"],
        1000
    );
    assert_eq!(
        projects[0]["report"]["landing"]["last_7_days"]["paid_ms"],
        1000
    );
    assert_eq!(
        projects[1]["report"]["landing"]["last_30_days"]["paid_ms"],
        10
    );
    fs::write(other.join(".git/arc/config.json"), "malformed").unwrap();
    let report = json_stdout_any_status(repo.arc(&repo.root).args([
        "workspace",
        "gates",
        "--global",
        "--json",
    ]));
    assert_eq!(report["collection"]["failed"], 1);
    assert!(report["projects"][1]["report"].is_null());
    let text = stdout_any_status(
        repo.arc(&repo.root)
            .args(["workspace", "gates", "--global"]),
    );
    assert!(text.contains("unobserved"), "{text}");
}

/// Claim: landing percentiles describe the full window only when unknown costs
/// cannot change their values, and an unknown median has no budget judgment.
/// Rejects a measured-only zero median or p90, an invented worst cost, or a
/// budget verdict guessed from zero-execution samples in incomplete history.
#[test]
fn landing_percentiles_require_determined_window_positions() {
    let repo = Repo::new();
    repo.redeclare_gates("[budget]\nlanding='1ms'\n[gates.unit]\ncommand='true'\n");
    let (zero, wt, _) = change_with_patchset(&repo, "known-zero");
    close_at(&repo, &zero, chrono::Utc::now());
    for slug in ["unknown-one", "unknown-two"] {
        let (id, unknown, _) = change_with_patchset(&repo, slug);
        repo.arc(&unknown)
            .args(["verify", "--all"])
            .assert()
            .success();
        rewrite_event(&repo, &id, "verification-recorded", |v| {
            v.as_object_mut().unwrap().remove("run_ms");
        });
        close_at(&repo, &id, chrono::Utc::now());
    }
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let landing = &report["landing"];
    assert_eq!(landing["landings"], 3);
    assert_eq!(landing["unmeasured"], 2);
    assert_eq!(landing["timing"]["samples"], 1);
    assert!(landing["timing"]["median_ms"].is_null(), "{landing}");
    assert!(landing["timing"]["p90_ms"].is_null(), "{landing}");
    assert!(landing["timing"]["worst_ms"].is_null(), "{landing}");
    assert!(landing.get("median_exceeds_budget").is_none(), "{landing}");
    let text = stdout(repo.arc(&wt).arg("gates"));
    assert!(
        text.contains("3 landings, 2 unmeasured; median unmeasured, p90 unmeasured"),
        "{text}"
    );
    let doctor = json_stdout_any_status(repo.arc(&wt).args(["doctor", "--json"]));
    assert!(
        !doctor["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "landing-budget-exceeded"),
        "{doctor}"
    );
    for index in 0..17 {
        let (id, _, _) = change_with_patchset(&repo, &format!("extra-zero-{index}"));
        close_at(&repo, &id, chrono::Utc::now());
        if index == 1 || index == 16 {
            let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
            let landing = &report["landing"];
            assert_eq!(landing["timing"]["median_ms"], 0, "{landing}");
            assert_eq!(landing["median_exceeds_budget"], false, "{landing}");
            if index == 1 {
                assert!(landing["timing"]["p90_ms"].is_null(), "{landing}");
            } else {
                assert_eq!(landing["timing"]["p90_ms"], 0, "{landing}");
            }
            assert!(landing["timing"]["worst_ms"].is_null(), "{landing}");
        }
    }
}
