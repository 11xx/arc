//! Execution evidence a candidate received from outside the ledger: recorded
//! beside the registration as the sender's claim, labelled external in every
//! view, carried through export and import, and never an evaluation.
use super::common::*;
use super::forge_custody::{custody_file, digest, materialize};
use serde_json::{json, Value};

const GATES: &str = "[gates.build]\ncommand = \"test -f answer.txt\"\n\n[candidates]\nevaluation_reuse = \"never\"\n";
const RECEIPT: &str = "make test: 42 passed\n";

struct Revision {
    commit: String,
    tree: String,
}

/// A change with a brief and a required gate, and three revisions an external
/// run could name: the source it started from, a report commit that is the
/// source's child, and a tested revision outside the report's ancestry.
struct Fixture {
    repo: Repo,
    change: String,
    source: Revision,
    report: Revision,
    tested: Revision,
}

fn piped(repo: &Repo, args: &[&str], input: &str) -> String {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(&repo.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn tree_with(repo: &Repo, files: &[(&str, &str)]) -> String {
    let mut listing = String::new();
    for (name, content) in files {
        let blob = piped(repo, &["hash-object", "-w", "--stdin"], content);
        listing.push_str(&format!("100644 blob {blob}\t{name}\n"));
    }
    piped(repo, &["mktree"], &listing)
}

fn revision(repo: &Repo, name: &str, files: &[(&str, &str)], parent: &str) -> Revision {
    let tree = tree_with(repo, files);
    let commit = git_out(
        &repo.root,
        &["commit-tree", &tree, "-p", parent, "-m", name],
    );
    // Reachable from a ref, so a delivery of every ref carries it.
    git(
        &repo.root,
        &[
            "update-ref",
            &format!("refs/heads/evidence-{name}"),
            &commit,
        ],
    );
    Revision { commit, tree }
}

impl Fixture {
    fn new() -> Fixture {
        let repo = Repo::new();
        repo.declare_gates_locally(GATES);
        let change = begin_without_checkout(&repo, "answer", &[]);
        let mut brief = repo.arc(&repo.root);
        brief.args(["brief", "answer", "--body-file", "-"]);
        brief
            .write_stdin("Answer the question.\n")
            .assert()
            .success();
        let init = git_out(&repo.root, &["rev-parse", "HEAD"]);
        let readme = ("README.md", "hello\n");
        let source = revision(
            &repo,
            "source",
            &[readme, ("answer.txt", "source answer\n")],
            &init,
        );
        let report = revision(
            &repo,
            "report",
            &[
                readme,
                ("answer.txt", "source answer\n"),
                ("report.md", "run report\n"),
            ],
            &source.commit,
        );
        let tested = revision(
            &repo,
            "tested",
            &[readme, ("answer.txt", "tested answer\n")],
            &init,
        );
        Fixture {
            repo,
            change,
            source,
            report,
            tested,
        }
    }

    fn evidence(&self) -> Value {
        json!({
            "schema": "arc-received-evidence/1",
            "run_handle": "run-handle-1",
            "packet": "packet-7",
            "heads": {
                "source": {"commit": self.source.commit, "tree": self.source.tree},
                "tested": {"commit": self.tested.commit, "tree": self.tested.tree},
                "report": {"commit": self.report.commit, "tree": self.report.tree},
            },
            "provenance": [],
            "receipts": [receipt(RECEIPT)],
            "commands": [{
                "command": "make test",
                "result": "pass",
                "exit_code": 0,
                "duration_ms": 1200,
                "receipt": 0,
            }],
            "environment": {"identity": "image-1", "detail": "container"},
            "applicability": {
                "applies_to": "tested",
                "note": "the report commit adds only the report",
            },
            "omissions": [{"what": "provenance", "reason": "not supplied"}],
        })
    }

    fn register(&self, id: &str, tree: &str, evidence: &Value) -> AssertCommand {
        let path = self.repo.home.join(format!("{id}.evidence.json"));
        fs::write(&path, evidence.to_string()).unwrap();
        let mut command = self.repo.arc(&self.repo.root);
        command.args([
            "candidate",
            "register",
            "--tree",
            tree,
            "--brief",
            "answer",
            "--producer",
            "lead-a",
            "--id",
            id,
            "--evidence-file",
            path.to_str().unwrap(),
        ]);
        command
    }

    fn registered(&self, id: &str, tree: &str, evidence: &Value) {
        self.register(id, tree, evidence).assert().success();
    }

    fn view(&self, id: &str) -> Value {
        let shown =
            json_stdout(
                self.repo
                    .arc(&self.repo.root)
                    .args(["candidate", "show", id, "--json"]),
            );
        assert_eq!(shown["schema"], "arc-candidate/1", "{shown}");
        shown["candidates"][0].clone()
    }

    fn select(&self, chosen: &str) -> std::process::Output {
        self.repo
            .arc(&self.repo.root)
            .args([
                "candidate",
                "select",
                "--chosen",
                chosen,
                "--into",
                &self.change,
                "--target",
                &git_out(&self.repo.root, &["rev-parse", "master"]),
                "--rationale",
                "it answers the brief",
            ])
            .output()
            .unwrap()
    }
}

fn receipt(body: &str) -> Value {
    json!({
        "provider": "adapter",
        "handle": format!("handle-{}", &digest(body)[..8]),
        "sha256": digest(body),
        "byte_length": body.len(),
        "media_type": "text/plain",
        "capture": "pinned",
    })
}

/// Every event in the repository scope and the change's log, by type.
fn event_types(repo: &Repo, change: &str) -> Vec<String> {
    let mut types = Vec::new();
    for dir in [
        repo.root.join(".git/arc/repository/events"),
        event_dir(repo, change),
    ] {
        for entry in fs::read_dir(dir).into_iter().flatten() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let event: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                types.push(event["event_type"].as_str().unwrap().to_string());
            }
        }
    }
    types.sort();
    types
}

fn repository_events(repo: &Repo, event_type: &str) -> Vec<Value> {
    let mut paths: Vec<_> = fs::read_dir(repo.root.join(".git/arc/repository/events"))
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap())
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

fn candidate_refs(repo: &Repo) -> String {
    git_out(
        &repo.root,
        &["for-each-ref", "--format=%(refname)", "refs/arc/candidate/"],
    )
}

/// A receiver that admits the exporter's facts.
fn import_command(replica: &Repo, bundle: &Path) -> AssertCommand {
    let bytes = fs::read(bundle).unwrap();
    let parsed: Value = serde_json::from_slice(&bytes).unwrap();
    let source = parsed["source_replica_id"].as_str().unwrap();
    replica.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='fixture-change'\nsource_replica_id='{source}'\n[[replica.publishers.origins]]\nreplica_id='{source}'\nroles=['facts']\nactors=[]\n"));
    let mut command = replica.arc(&replica.root);
    command.args([
        "import",
        bundle.to_str().unwrap(),
        "--publisher",
        "fixture-change",
        "--received-sha256",
        &format!("{:x}", Sha256::digest(&bytes)),
    ]);
    command
}

#[test]
fn received_report_child_does_not_move_tested_source_claim() {
    let f = Fixture::new();
    // The tested revision is on no path to the report, which is the source's
    // child.
    let ancestor = Command::new("git")
        .args([
            "merge-base",
            "--is-ancestor",
            &f.tested.commit,
            &f.report.commit,
        ])
        .current_dir(&f.repo.root)
        .status()
        .unwrap();
    assert!(!ancestor.success());
    assert_ne!(f.source.tree, f.report.tree);
    assert_ne!(f.source.tree, f.tested.tree);

    // The delivered content is the report child's tree.
    f.registered("delivered", &f.report.commit, &f.evidence());
    let view = f.view("delivered");
    assert_eq!(view["tree"], f.report.tree);
    let received = &view["received_evidence"];
    assert_eq!(received["origin"], "external", "{received}");
    assert_eq!(received["schema"], "arc-received-evidence/1");
    assert_eq!(received["run_handle"], "run-handle-1");
    assert_eq!(received["packet"], "packet-7");
    // Each head keeps the commit and tree the run reported; the report child
    // moves none of them.
    for (role, revision) in [
        ("source", &f.source),
        ("tested", &f.tested),
        ("report", &f.report),
    ] {
        assert_eq!(received["heads"][role]["commit"], revision.commit, "{role}");
        assert_eq!(received["heads"][role]["tree"], revision.tree, "{role}");
        // No rewrite is recorded, so the original resolves to itself.
        assert_eq!(
            received["heads"][role]["resolved_commit"], revision.commit,
            "{role}"
        );
    }
    assert_eq!(received["applicability"]["applies_to"], "tested");
    assert_eq!(received["tested_tree_is_registered_tree"], false);

    // The claim is not an evaluation: nothing was verified, and the gate it
    // reports on is still unanswered at the shipped tree.
    assert_eq!(view["evaluations"], json!([]));
    let types = event_types(&f.repo, &f.change);
    for forbidden in ["candidate-verified", "verification-recorded"] {
        assert!(!types.iter().any(|t| t == forbidden), "{types:?}");
    }
    assert_eq!(
        types
            .iter()
            .filter(|t| *t == "candidate-evidence-received")
            .count(),
        1
    );
    let refused = f.select("delivered");
    assert_eq!(refused.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("no-evaluation"), "{stderr}");
    assert!(repository_events(&f.repo, "candidate-selected").is_empty());

    // Registered at the very tree the run says it tested, the claim covers
    // that tree and still evaluates nothing.
    f.registered("tested-tree", &f.tested.commit, &f.evidence());
    let same = f.view("tested-tree");
    assert_eq!(
        same["received_evidence"]["tested_tree_is_registered_tree"],
        true
    );
    assert_eq!(same["evaluations"], json!([]));
    let refused = f.select("tested-tree");
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("no-evaluation"));
    assert!(repository_events(&f.repo, "candidate-selected").is_empty());
}

#[test]
fn forge_shared_account_does_not_collapse_agent_roles() {
    let f = Fixture::new();
    let account =
        json!({"provider": "forge", "host": "forge.example.invalid", "account": "shared-bot"});
    let identity = |name: &str| json!({"name": name, "email": format!("{name}@example.invalid")});
    let source = |kind: &str, reference: &str| json!({"kind": kind, "reference": reference});
    let mut evidence = f.evidence();
    evidence["omissions"] = json!([]);
    evidence["provenance"] = json!([
        {
            "role": "producer",
            "native": {"provider": "cloud", "identity": "agent-p", "source": source("api", "run/1")},
            "model": {"name": "model-p", "effort": "high", "source": source("recording", "rec-p")},
            "forge_account": account,
            "git_author": identity("Producer Author"),
            "git_committer": identity("Producer Committer"),
        },
        {
            "role": "collector",
            "native": {"provider": "cloud", "identity": "agent-c", "source": source("api", "run/2")},
            "model": {"name": "model-c", "source": source("recording", "rec-c")},
            "forge_account": account,
            "git_author": identity("Collector Author"),
            "git_committer": identity("Collector Committer"),
        },
        {
            "role": "reviewer",
            "model": {"name": "model-r", "effort": "xhigh", "source": source("recording", "rec-r")},
            "forge_account": account,
            "review_sender": account,
            "git_author": identity("Reviewer Author"),
        },
    ]);
    let path = f.repo.home.join("shared.evidence.json");
    fs::write(&path, evidence.to_string()).unwrap();
    f.repo
        .arc(&f.repo.root)
        .args([
            "--on-behalf-of",
            "scope owner",
            "candidate",
            "register",
            "--tree",
            &f.report.commit,
            "--brief",
            "answer",
            "--producer",
            "lead-a",
            "--id",
            "shared",
            "--evidence-file",
            path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let view = f.view("shared");
    // The candidate's own roles are the ones the registration named; no forge
    // account, Git identity or model observation entered them.
    assert_eq!(view["producers"], json!(["lead-a"]));
    assert_eq!(view["registered_by"], "scope owner");
    let received = &view["received_evidence"];
    assert_eq!(received["recorded_by"], "scope owner");
    let entries = received["provenance"].as_array().unwrap();
    let roles: Vec<_> = entries.iter().map(|entry| entry["role"].clone()).collect();
    assert_eq!(
        roles,
        [json!("producer"), json!("collector"), json!("reviewer")]
    );
    // One account stands behind all three, and the three stay three.
    for entry in entries {
        assert_eq!(entry["forge_account"], account, "{entry}");
    }
    assert_eq!(entries[2]["review_sender"], account);
    assert!(entries[0]["review_sender"].is_null());
    let models: Vec<_> = entries
        .iter()
        .map(|entry| entry["model"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(models, ["model-p", "model-c", "model-r"]);
    assert_eq!(entries[0]["model"]["source"], source("recording", "rec-p"));
    assert_eq!(entries[0]["native"]["source"], source("api", "run/1"));
    assert_eq!(entries[0]["git_author"], identity("Producer Author"));
    assert_eq!(entries[0]["git_committer"], identity("Producer Committer"));
    assert_ne!(entries[0]["git_author"], entries[1]["git_author"]);
    assert_ne!(entries[1]["git_author"], entries[2]["git_author"]);
    // A role with no committer observed keeps none.
    assert!(entries[2]["git_committer"].is_null());

    // The recorded event holds the same entries as the view, and the
    // registration beside it names only its producer.
    let stored = repository_events(&f.repo, "candidate-evidence-received");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0]["evidence"]["provenance"], received["provenance"]);
    let registered = repository_events(&f.repo, "candidate-registered");
    assert_eq!(registered[0]["producers"], json!(["lead-a"]));
    assert_eq!(registered[0]["on_behalf_of"], "scope owner");

    // The text view shows each role on its own line.
    let text = stdout(
        f.repo
            .arc(&f.repo.root)
            .args(["candidate", "show", "shared"]),
    );
    assert!(text.contains("received evidence [external]"), "{text}");
    for (role, model) in [
        ("producer", "model-p"),
        ("collector", "model-c"),
        ("reviewer", "model-r"),
    ] {
        let line = text
            .lines()
            .find(|line| line.contains(&format!("provenance {role}")))
            .unwrap_or_else(|| panic!("no {role} line: {text}"));
        assert!(line.contains(model), "{line}");
        assert!(line.contains("shared-bot"), "{line}");
    }
}

#[test]
fn received_evidence_with_a_digest_that_disagrees_with_custody_bytes_refuses_before_append() {
    let f = Fixture::new();
    let events = event_types(&f.repo, &f.change);

    // The adapter's file has the recorded length and other bytes.
    let supplied = f.repo.home.join("supplied");
    materialize(&supplied, &[RECEIPT]);
    let tampered = RECEIPT.replace("42 passed", "41 passed");
    assert_eq!(tampered.len(), RECEIPT.len());
    fs::write(supplied.join("sha256").join(digest(RECEIPT)), &tampered).unwrap();

    let out = f
        .register("refused", &f.report.commit, &f.evidence())
        .args(["--custody-dir", supplied.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does not match the recorded digest"),
        "{stderr}"
    );
    assert!(stderr.contains("nothing was recorded"), "{stderr}");

    // Nothing was appended, pinned or published.
    assert_eq!(event_types(&f.repo, &f.change), events);
    assert_eq!(candidate_refs(&f.repo), "");
    assert!(!custody_file(&f.repo, RECEIPT).exists());
    let listed = json_stdout(
        f.repo
            .arc(&f.repo.root)
            .args(["candidate", "list", "--json"]),
    );
    assert_eq!(listed["candidates"], json!([]));

    // The file with the recorded bytes records the evidence, and the
    // receipt reads available from custody.
    materialize(&supplied, &[RECEIPT]);
    f.register("accepted", &f.report.commit, &f.evidence())
        .args(["--custody-dir", supplied.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(
        fs::read(custody_file(&f.repo, RECEIPT)).unwrap(),
        RECEIPT.as_bytes()
    );
    let received = f.view("accepted")["received_evidence"].clone();
    assert_eq!(received["receipts"][0]["availability"], "available");
    assert_eq!(received["receipts"][0]["sha256"], digest(RECEIPT));

    // Without a directory a reference reads from custody alone, and damaged
    // bytes read as corrupt rather than as the receipt.
    fs::write(custody_file(&f.repo, RECEIPT), "").unwrap();
    let damaged = f.view("accepted")["received_evidence"].clone();
    assert_eq!(damaged["receipts"][0]["availability"], "corrupt");
    fs::remove_file(custody_file(&f.repo, RECEIPT)).unwrap();
    let absent = f.view("accepted")["received_evidence"].clone();
    assert_eq!(absent["receipts"][0]["availability"], "unavailable");
}

#[test]
fn received_evidence_that_breaks_a_rule_writes_nothing() {
    let f = Fixture::new();
    let events = event_types(&f.repo, &f.change);
    let mut cases: Vec<(&str, Value, &str)> = Vec::new();
    let mut case = |name, edit: &dyn Fn(&mut Value), expected| {
        let mut evidence = f.evidence();
        edit(&mut evidence);
        cases.push((name, evidence, expected));
    };
    case(
        "schema",
        &|e| e["schema"] = json!("arc-received-evidence/2"),
        "unsupported received-evidence schema",
    );
    case(
        "omissions-key",
        &|e| {
            e.as_object_mut().unwrap().remove("omissions");
        },
        "omissions",
    );
    case(
        "unlisted-absence",
        &|e| e["omissions"] = json!([]),
        "provenance are absent and not listed in omissions",
    );
    case(
        "contradicted-omission",
        &|e| {
            e["omissions"] = json!([
                {"what": "provenance", "reason": "not supplied"},
                {"what": "receipts", "reason": "withheld"},
            ])
        },
        "receipts are carried and also listed as omitted",
    );
    case(
        "unknown-field",
        &|e| e["verified"] = json!(true),
        "unknown field",
    );
    case(
        "short-commit",
        &|e| e["heads"]["source"]["commit"] = json!("abc1234"),
        "is not a full object ID",
    );
    case(
        "receipt-index",
        &|e| e["commands"][0]["receipt"] = json!(3),
        "names receipt 3",
    );
    case(
        "result-contradicts-exit",
        &|e| e["commands"][0]["exit_code"] = json!(1),
        "pass with exit code 1",
    );
    case(
        "receipt-digest",
        &|e| e["receipts"][0]["sha256"] = json!("abc"),
        "invalid full digest",
    );
    // A commit this repository holds has the tree it has.
    let (source_commit, wrong_tree) = (f.source.commit.clone(), f.tested.tree.clone());
    case(
        "held-commit-tree",
        &move |e| {
            e["heads"]["source"] = json!({"commit": source_commit, "tree": wrong_tree});
            e["heads"]["tested"] = json!({"commit": source_commit, "tree": wrong_tree});
        },
        "evidence-tree-mismatch",
    );
    for (name, evidence, expected) in cases {
        let out = f
            .register(name, &f.report.commit, &evidence)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{name} was accepted");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(expected), "{name}: {stderr}");
        assert_eq!(event_types(&f.repo, &f.change), events, "{name}");
        assert_eq!(candidate_refs(&f.repo), "", "{name}");
    }
    // Evidence is a file the caller names: a missing one refuses too.
    f.repo
        .arc(&f.repo.root)
        .args([
            "candidate",
            "register",
            "--tree",
            &f.report.commit,
            "--brief",
            "answer",
            "--producer",
            "lead-a",
            "--evidence-file",
            "absent.json",
        ])
        .assert()
        .failure();
    assert_eq!(event_types(&f.repo, &f.change), events);
}

#[test]
fn received_evidence_travels_with_its_receipts_and_the_objects_the_exporter_holds() {
    let f = Fixture::new();
    let supplied = f.repo.home.join("supplied");
    materialize(&supplied, &[RECEIPT]);
    // The tested head is a commit this repository never held.
    let unheld = ("c".repeat(40), "d".repeat(40));
    let mut evidence = f.evidence();
    evidence["heads"]["tested"] = json!({"commit": unheld.0, "tree": unheld.1});
    f.register("travelling", &f.report.commit, &evidence)
        .args(["--custody-dir", supplied.to_str().unwrap()])
        .assert()
        .success();

    let bundle = f.repo.home.join("bundle.json");
    let objects = f.repo.home.join("objects.json");
    let sidecar = f.repo.home.join("bundle.custody");
    let exported = stdout(f.repo.arc(&f.repo.root).args([
        "export",
        &f.change,
        "--output",
        bundle.to_str().unwrap(),
        "--objects-json",
        objects.to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    assert!(
        exported.contains("custody: 1 entries, 1 available, 0 unavailable"),
        "{exported}"
    );
    let bundle_json: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert!(bundle_json["repository_events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["event_type"] == "candidate-evidence-received"));
    let listed: Value = serde_json::from_slice(&fs::read(&objects).unwrap()).unwrap();
    let listed = listed["objects"].as_array().unwrap();
    for revision in [&f.source, &f.report] {
        assert!(
            listed.contains(&json!({"oid": revision.commit, "role": "received-commit"})),
            "{listed:?}"
        );
        assert!(
            listed.contains(&json!({"oid": revision.tree, "role": "received-tree"})),
            "{listed:?}"
        );
    }
    // What the exporter never held obliges no receiver.
    assert!(!listed.iter().any(|object| object["oid"] == unheld.0));
    assert!(!listed.iter().any(|object| object["oid"] == unheld.1));

    let replica = Repo::new();
    deliver_git_objects(&f.repo, &replica);
    let imported = stdout(
        import_command(&replica, &bundle).args(["--custody-dir", sidecar.to_str().unwrap()]),
    );
    assert!(
        imported.contains("custody: 1 installed, 0 held, 0 unavailable"),
        "{imported}"
    );
    assert_eq!(
        fs::read(custody_file(&replica, RECEIPT)).unwrap(),
        RECEIPT.as_bytes()
    );
    let shown =
        json_stdout(
            replica
                .arc(&replica.root)
                .args(["candidate", "show", "travelling", "--json"]),
        );
    let received = &shown["candidates"][0]["received_evidence"];
    assert_eq!(received["origin"], "external");
    assert_eq!(received["receipts"][0]["availability"], "available");
    assert_eq!(received["heads"]["tested"]["commit"], unheld.0);
    assert_eq!(received["heads"]["source"]["commit"], f.source.commit);
    // The same document, as the sender recorded it.
    let before = f.view("travelling")["received_evidence"].clone();
    assert_eq!(received["heads"], before["heads"]);
    assert_eq!(received["commands"], before["commands"]);
    assert_eq!(received["omissions"], before["omissions"]);
}

#[test]
fn a_registration_without_evidence_shows_none_and_cannot_gain_it_later() {
    let f = Fixture::new();
    f.repo
        .arc(&f.repo.root)
        .args([
            "candidate",
            "register",
            "--tree",
            &f.report.commit,
            "--brief",
            "answer",
            "--producer",
            "lead-a",
            "--id",
            "plain",
        ])
        .assert()
        .success();
    assert!(f.view("plain")["received_evidence"].is_null());
    let text = stdout(
        f.repo
            .arc(&f.repo.root)
            .args(["candidate", "show", "plain"]),
    );
    assert!(!text.contains("received evidence"), "{text}");
    assert!(repository_events(&f.repo, "candidate-evidence-received").is_empty());

    // The same candidate id cannot be registered again to add evidence.
    let out = f
        .register("plain", &f.report.commit, &f.evidence())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("duplicate-candidate"));
    assert!(repository_events(&f.repo, "candidate-evidence-received").is_empty());
}

#[test]
fn an_imported_second_evidence_event_for_one_candidate_is_refused_before_anything_is_written() {
    let f = Fixture::new();
    f.registered("once", &f.report.commit, &f.evidence());
    let bundle = f.repo.home.join("bundle.json");
    stdout(f.repo.arc(&f.repo.root).args([
        "export",
        &f.change,
        "--output",
        bundle.to_str().unwrap(),
    ]));
    let mut parsed: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let events = parsed["repository_events"].as_array_mut().unwrap();
    let mut second = events
        .iter()
        .find(|event| event["event_type"] == "candidate-evidence-received")
        .unwrap()
        .clone();
    let mut id = second["event_id"].as_str().unwrap().to_string();
    let last = if id.ends_with('Z') { 'Y' } else { 'Z' };
    id.replace_range(id.len() - 1.., &last.to_string());
    second["event_id"] = json!(id);
    events.push(second);
    fs::write(&bundle, serde_json::to_vec_pretty(&parsed).unwrap()).unwrap();

    let replica = Repo::new();
    deliver_git_objects(&f.repo, &replica);
    let out = import_command(&replica, &bundle).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("evidence-already-received"), "{stderr}");
    assert!(stderr.contains("nothing was imported"), "{stderr}");
    assert!(!replica.root.join(".git/arc/repository/events").exists());
}

#[test]
fn a_reported_commit_keeps_its_value_beside_its_resolution_through_a_rewrite() {
    let f = Fixture::new();
    f.registered("rewritten", &f.report.commit, &f.evidence());

    // The source is recreated under another message, and the tested revision
    // is dropped; the report is untouched.
    let successor = git_out(
        &f.repo.root,
        &[
            "commit-tree",
            &f.source.tree,
            "-p",
            &git_out(
                &f.repo.root,
                &["rev-parse", &format!("{}^", f.source.commit)],
            ),
            "-m",
            "source, recreated",
        ],
    );
    let map = f.repo.home.join("rewrite-map");
    fs::write(
        &map,
        format!(
            "{} {successor}\n{} {}\n",
            f.source.commit,
            f.tested.commit,
            "0".repeat(40)
        ),
    )
    .unwrap();
    stdout(f.repo.arc(&f.repo.root).args([
        "history",
        "rewrite",
        "--map",
        map.to_str().unwrap(),
        "--reason",
        "recreated after the run",
    ]));

    let heads = f.view("rewritten")["received_evidence"]["heads"].clone();
    // What the run reported never changes; resolution sits beside it.
    assert_eq!(heads["source"]["commit"], f.source.commit);
    assert_eq!(heads["source"]["resolved_commit"], successor);
    assert_eq!(heads["tested"]["commit"], f.tested.commit);
    assert!(heads["tested"]["resolved_commit"].is_null(), "{heads}");
    assert_eq!(heads["report"]["commit"], f.report.commit);
    assert_eq!(heads["report"]["resolved_commit"], f.report.commit);
    let text = stdout(
        f.repo
            .arc(&f.repo.root)
            .args(["candidate", "show", "rewritten"]),
    );
    assert!(
        text.contains(&format!("resolves here to {successor}")),
        "{text}"
    );
    assert!(text.contains("dropped by a recorded rewrite"), "{text}");
    // The stored event is the original claim.
    let stored = repository_events(&f.repo, "candidate-evidence-received");
    assert_eq!(
        stored[0]["evidence"]["heads"]["source"]["commit"],
        f.source.commit
    );
}

#[test]
fn held_raw_heads_refuse_both_intake_paths_before_publication() {
    for kind in ["tag", "blob", "tree", "commit"] {
        let f = Fixture::new();
        let receiver = Repo::new();
        let root = git_out(&receiver.root, &["rev-parse", "HEAD"]);
        let head = revision(
            &receiver,
            "receiver-only",
            &[("answer.txt", "receiver answer")],
            &root,
        );
        let oid = match kind {
            "tag" => {
                piped(&receiver, &["mktag"], &format!(
                    "object {}\ntype commit\ntag receipt-head\ntagger Fixture <fixture@example.invalid> 1 +0000\n\nreceipt head\n", head.commit))
            }
            "blob" => piped(&receiver, &["hash-object", "-w", "--stdin"], "a held blob"),
            "tree" => head.tree.clone(),
            _ => head.commit.clone(),
        };
        // The sender lacks this raw object and can only record an external claim.
        assert!(!Command::new("git")
            .args(["cat-file", "-e", &oid])
            .current_dir(&f.repo.root)
            .output()
            .unwrap()
            .status
            .success());
        let mut evidence = f.evidence();
        evidence["heads"]["source"] =
            json!({"commit": oid, "tree": if kind == "tag" { &head.tree } else { &f.tested.tree }});
        f.registered("external-head", &f.report.commit, &evidence);
        let bundle = f.repo.home.join("bundle.json");
        let sidecar = f.repo.home.join("sidecar");
        stdout(f.repo.arc(&f.repo.root).args([
            "export",
            &f.change,
            "--output",
            bundle.to_str().unwrap(),
            "--custody-dir",
            sidecar.to_str().unwrap(),
        ]));
        deliver_git_objects(&f.repo, &receiver);
        let refs = git_out(&receiver.root, &["show-ref"]);
        let expected = if kind == "commit" {
            "evidence-tree-mismatch"
        } else {
            "evidence-head-type"
        };
        for preview in [true, false] {
            let mut cmd = import_command(&receiver, &bundle);
            cmd.args(["--custody-dir", sidecar.to_str().unwrap()]);
            if preview {
                cmd.arg("--dry-run");
            }
            let out = cmd.output().unwrap();
            assert!(!out.status.success(), "{kind}");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.contains(expected), "{kind}: {stderr}");
            assert!(!receiver.root.join(".git/arc/repository/events").exists());
            assert!(!receiver.root.join(".git/arc/imports").exists());
            assert!(!custody_file(&receiver, RECEIPT).exists());
            assert_eq!(git_out(&receiver.root, &["show-ref"]), refs);
        }
        // The same held raw object is rejected by registration, including a tag
        // whose peeled commit has exactly the claimed tree.
        let git_bundle = receiver.home.join("receiver.git.bundle");
        git(&receiver.root, &["update-ref", "refs/evidence/raw", &oid]);
        git(
            &receiver.root,
            &["bundle", "create", git_bundle.to_str().unwrap(), "--all"],
        );
        git(
            &f.repo.root,
            &[
                "fetch",
                git_bundle.to_str().unwrap(),
                "refs/evidence/raw:refs/evidence/raw",
            ],
        );
        if kind == "tag" {
            evidence["heads"]["source"]["tree"] = json!(head.tree);
        }
        let before = event_types(&f.repo, &f.change);
        let pins = candidate_refs(&f.repo);
        let out = f
            .register("held-head", &f.report.commit, &evidence)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains(expected));
        assert_eq!(event_types(&f.repo, &f.change), before);
        assert_eq!(candidate_refs(&f.repo), pins);
    }
}

#[test]
fn received_originals_survive_rewrite_and_import_garbage_collection() {
    let f = Fixture::new();
    f.registered("retained", &f.report.commit, &f.evidence());
    let refs = git_out(
        &f.repo.root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/evidence-",
            "refs/arc/retained/",
        ],
    );
    // Removing existing refs forces rewrite retention to derive every original
    // from the repository events rather than an already held retention ref.
    for reference in refs.lines() {
        git(&f.repo.root, &["update-ref", "-d", reference]);
    }
    let map = f.repo.home.join("drop-map");
    fs::write(
        &map,
        [&f.source, &f.tested, &f.report]
            .iter()
            .map(|rev| format!("{} {}\n", rev.commit, "0".repeat(40)))
            .collect::<String>(),
    )
    .unwrap();
    stdout(f.repo.arc(&f.repo.root).args([
        "history",
        "rewrite",
        "--map",
        map.to_str().unwrap(),
        "--reason",
        "drop original execution history",
    ]));
    git(&f.repo.root, &["reflog", "expire", "--expire=now", "--all"]);
    git(&f.repo.root, &["gc", "--prune=now"]);
    for rev in [&f.source, &f.tested, &f.report] {
        assert_eq!(
            git_out(&f.repo.root, &["cat-file", "-t", &rev.commit]),
            "commit"
        );
    }
    let bundle = f.repo.home.join("retained.json");
    stdout(f.repo.arc(&f.repo.root).args([
        "export",
        &f.change,
        "--output",
        bundle.to_str().unwrap(),
    ]));
    // Without a rewrite event, import must still pin received originals.
    let mut parsed: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    parsed["repository_events"]
        .as_array_mut()
        .unwrap()
        .retain(|event| event["event_type"] != "history-rewritten");
    fs::write(&bundle, serde_json::to_vec_pretty(&parsed).unwrap()).unwrap();
    let receiver = Repo::new();
    deliver_git_objects(&f.repo, &receiver);
    stdout(&mut import_command(&receiver, &bundle));
    let refs = git_out(&receiver.root, &["for-each-ref", "--format=%(refname)"]);
    for reference in refs
        .lines()
        .filter(|reference| !reference.starts_with("refs/arc/"))
    {
        if reference != "refs/heads/master" {
            git(&receiver.root, &["update-ref", "-d", reference]);
        }
    }
    git(
        &receiver.root,
        &["reflog", "expire", "--expire=now", "--all"],
    );
    git(&receiver.root, &["gc", "--prune=now"]);
    let held = git_out(
        &receiver.root,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/arc/retained/",
        ],
    );
    for rev in [&f.source, &f.tested, &f.report] {
        assert!(held.lines().any(|oid| oid == rev.commit));
        assert_eq!(
            git_out(&receiver.root, &["cat-file", "-t", &rev.commit]),
            "commit"
        );
    }
}
