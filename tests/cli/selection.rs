use super::common::*;

/// One brief on one change, a required gate with an environment probe, and
/// four registrations: `alice` and `bob` on one tree, `carol` and `dave`
/// each on a tree of their own.
struct Fixture {
    repo: Repo,
    change: String,
    claim: String,
    shared: String,
    carol: String,
    dave: String,
}

const GATES: &str = "[gates.build]\ncommand = \"test -f answer.txt\"\nenvironment = \"printf env-${FIXTURE_ENV:-one}\"\n";

fn declare(repo: &Repo, gates: &str, reuse: Option<&str>) {
    let candidates = reuse
        .map(|reuse| format!("\n[candidates]\nevaluation_reuse = \"{reuse}\"\n"))
        .unwrap_or_default();
    repo.declare_gates_locally(&format!("{gates}{candidates}"));
}

fn fixture(reuse: Option<&str>, must_read: &[&str], worktree: bool) -> Fixture {
    let repo = Repo::new();
    fs::write(repo.root.join("NOTES.md"), "one\ntwo\nthree\n").unwrap();
    git(&repo.root, &["add", "NOTES.md"]);
    git(&repo.root, &["commit", "-q", "-m", "notes"]);
    declare(&repo, GATES, reuse);
    let change = if worktree {
        opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "answer"])))
    } else {
        begin_without_checkout(&repo, "answer", &[])
    };
    repo.arc(&repo.root)
        .args(["claim", "answer"])
        .assert()
        .success();
    let claim = events_of(&repo, &change, "claim-set").pop().unwrap()["claim_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut brief = repo.arc(&repo.root);
    brief.args(["brief", "answer", "--body-file", "-"]);
    for locator in must_read {
        brief.args(["--must-read", locator]);
    }
    brief
        .write_stdin("Answer the question.\n")
        .assert()
        .success();

    let shared = tree_with(&repo, "the shared answer\n");
    let carol = tree_with(&repo, "carol's answer\n");
    let dave = tree_with(&repo, "dave's answer\n");
    for (id, tree, producer) in [
        ("alice", &shared, "alice"),
        ("bob", &shared, "bob"),
        ("carol", &carol, "carol"),
        ("dave", &dave, "dave"),
    ] {
        register(&repo, &change, id, tree, producer, &["--episode", &claim]);
    }
    Fixture {
        repo,
        change,
        claim,
        shared,
        carol,
        dave,
    }
}

/// A tree holding the fixture's README and `answer.txt`.
fn tree_with(repo: &Repo, answer: &str) -> String {
    let readme = piped(repo, &["hash-object", "-w", "--stdin"], "hello\n");
    let blob = piped(repo, &["hash-object", "-w", "--stdin"], answer);
    piped(
        repo,
        &["mktree"],
        &format!("100644 blob {readme}\tREADME.md\n100644 blob {blob}\tanswer.txt\n"),
    )
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

fn register(repo: &Repo, change: &str, id: &str, tree: &str, producer: &str, extra: &[&str]) {
    repo.arc(&repo.root)
        .args([
            "candidate",
            "register",
            "--tree",
            tree,
            "--brief",
            change,
            "--producer",
            producer,
            "--id",
            id,
        ])
        .args(extra)
        .assert()
        .success();
}

fn events_of(repo: &Repo, change_id: &str, event_type: &str) -> Vec<serde_json::Value> {
    let mut paths: Vec<_> = fs::read_dir(event_dir(repo, change_id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap()).unwrap())
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

/// Published repository events. An event is written beside its final name
/// and linked into place, so a read racing a write skips the partial file.
fn repository_events(repo: &Repo, event_type: &str) -> Vec<serde_json::Value> {
    let dir = repo.root.join(".git/arc/repository/events");
    let mut paths: Vec<_> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    paths.sort();
    paths
        .into_iter()
        .map(|path| serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap()).unwrap())
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

/// `candidate verify`, returning the evaluation event it recorded.
fn evaluated(repo: &Repo, candidate: &str) -> String {
    let out = stdout(
        repo.arc(&repo.root)
            .args(["candidate", "verify", candidate]),
    );
    out.lines()
        .find_map(|line| line.strip_prefix("evaluation: "))
        .unwrap_or_else(|| panic!("verify printed no evaluation: {out}"))
        .to_string()
}

fn target(repo: &Repo) -> String {
    git_out(&repo.root, &["rev-parse", "master"])
}

fn select(
    repo: &Repo,
    change: &str,
    chosen: &str,
    target: &str,
    evaluations: &[&str],
) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.args([
        "candidate",
        "select",
        "--chosen",
        chosen,
        "--into",
        change,
        "--target",
        target,
        "--rationale",
        "it answers the brief",
    ]);
    for evaluation in evaluations {
        command.args(["--evaluation", evaluation]);
    }
    command
}

fn refused(command: &mut AssertCommand) -> String {
    let out = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "not refused: {stderr}");
    assert!(stderr.contains("selection refused"), "{stderr}");
    stderr
}

/// The destination's branch: every fixture's change is `answer`.
fn branch_head(repo: &Repo, _change: &str) -> String {
    git_out(&repo.root, &["rev-parse", "refs/heads/arc/answer"])
}

fn patchsets(repo: &Repo, change: &str) -> Vec<serde_json::Value> {
    events_of(repo, change, "patchset-added")
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

#[test]
fn sibling_evidence_answers_only_under_matching_coordinates() {
    let f = fixture(Some("never"), &[], true);
    let repo = &f.repo;
    let from_alice = evaluated(repo, "alice");
    let before = repository_events(repo, "candidate-selected").len();

    // Under `never`, alice's evaluation of the very same tree is not bob's.
    let stderr = refused(&mut select(
        repo,
        &f.change,
        "bob",
        &target(repo),
        &[&from_alice],
    ));
    assert!(stderr.contains("other-registration"), "{stderr}");
    assert!(stderr.contains(&from_alice), "{stderr}");
    assert_eq!(repository_events(repo, "candidate-selected").len(), before);
    assert!(patchsets(repo, &f.change).is_empty());

    // Under `matching-coordinates` the same evidence answers for bob.
    declare(repo, GATES, Some("matching-coordinates"));
    let out = stdout(&mut select(
        repo,
        &f.change,
        "bob",
        &target(repo),
        &[&from_alice],
    ));
    assert!(out.contains("promoted: bob"), "{out}");
    let selected = repository_events(repo, "candidate-selected").pop().unwrap();
    assert_eq!(selected["reuse"], "matching-coordinates");
    assert_eq!(selected["evaluations"][0]["candidate_id"], "alice");
    assert_eq!(selected["tree"], f.shared);
    let patchset = patchsets(repo, &f.change).pop().unwrap();
    assert_eq!(patchset["contributors"], serde_json::json!(["bob"]));
    assert_eq!(patchset["candidate"]["candidate_id"], "bob");
    let head = branch_head(repo, &f.change);
    assert_eq!(patchset["head"], head);
    assert_eq!(
        git_out(&repo.root, &["rev-parse", &format!("{head}^{{tree}}")]),
        f.shared
    );
    // The destination's checkout moved with its branch.
    let worktree = repo.home.join(".worktrees/repo-answer");
    assert_eq!(
        fs::read_to_string(worktree.join("answer.txt")).unwrap(),
        "the shared answer\n"
    );
    assert_eq!(git_out(&worktree, &["status", "--porcelain"]), "");
}

/// Claim: candidate selection refuses, naming every ground at once, when the
/// target moved, a gate was redeclared, the evaluated tree differs, an
/// evaluation ran in another environment, or none exists, and records no
/// selection. Rejects a selection authorized by a stale or foreign evaluation.
#[test]
fn stale_target_and_stale_evaluation_are_refused_by_name() {
    let f = fixture(Some("matching-coordinates"), &[], false);
    let repo = &f.repo;
    let from_alice = evaluated(repo, "alice");
    let from_carol = evaluated(repo, "carol");
    let decided = target(repo);
    repo.commit(
        &repo.root,
        "later.txt",
        "later\n",
        "later work on the target",
    );
    // The gate is redeclared after alice's evaluation ran.
    declare(
        repo,
        &GATES.replace("test -f answer.txt", "test -s answer.txt"),
        Some("matching-coordinates"),
    );

    let stderr = refused(&mut select(
        repo,
        &f.change,
        "alice",
        &decided,
        &[&from_alice, &from_carol],
    ));
    // Every ground is reported, not the first.
    assert!(stderr.contains("target-moved"), "{stderr}");
    assert!(stderr.contains("other-declaration"), "{stderr}");
    assert!(stderr.contains("other-tree"), "{stderr}");
    assert!(stderr.contains(&from_carol), "{stderr}");
    assert!(repository_events(repo, "candidate-selected").is_empty());

    // An evaluation in another environment is named for what it is.
    declare(repo, GATES, Some("matching-coordinates"));
    let fresh = evaluated(repo, "dave");
    let stderr = refused(
        select(repo, &f.change, "dave", &target(repo), &[&fresh]).env("FIXTURE_ENV", "two"),
    );
    assert!(stderr.contains("environment-other"), "{stderr}");
    assert!(!stderr.contains("target-moved"), "{stderr}");
    let stderr = refused(&mut select(repo, &f.change, "dave", &target(repo), &[]));
    assert!(stderr.contains("no-evaluation"), "{stderr}");
    let _ = f.dave;
}

#[test]
fn a_must_read_met_only_by_a_declaration_is_refused() {
    let f = fixture(Some("never"), &["HEAD:NOTES.md"], false);
    let repo = &f.repo;
    let evaluation = evaluated(repo, "alice");
    let brief = json_stdout(repo.arc(&repo.root).args(["brief", "answer", "--json"]));
    assert_eq!(brief["schema"], "arc-brief/2");
    assert_eq!(brief["brief"]["must_read"][0]["path"], "NOTES.md");

    let stderr = refused(&mut select(
        repo,
        &f.change,
        "alice",
        &target(repo),
        &[&evaluation],
    ));
    assert!(stderr.contains("not-read"), "{stderr}");

    repo.arc(&repo.root)
        .args([
            "context",
            "declare",
            "--subject",
            "alice",
            "--relies-on",
            "--path",
            "NOTES.md",
        ])
        .assert()
        .success();
    let stderr = refused(&mut select(
        repo,
        &f.change,
        "alice",
        &target(repo),
        &[&evaluation],
    ));
    assert!(stderr.contains("only-declared"), "{stderr}");
    assert!(stderr.contains("tester"), "{stderr}");

    let read = |record: &str, extra: &[&str]| {
        repo.arc(&repo.root)
            .args([
                "context",
                "read",
                "--subject",
                "alice",
                "--episode",
                &f.claim,
                "--record",
                record,
                "--path",
                "NOTES.md",
            ])
            .args(extra)
            .assert()
            .success();
    };
    read(
        "first-line",
        &[
            "--digest",
            &digest(b"one\n"),
            "--lines",
            "1-1",
            "--at",
            "HEAD",
        ],
    );
    let stderr = refused(&mut select(
        repo,
        &f.change,
        "alice",
        &target(repo),
        &[&evaluation],
    ));
    assert!(stderr.contains("partial"), "{stderr}");

    read(
        "whole",
        &["--digest", &digest(b"one\ntwo\nthree\n"), "--whole"],
    );
    let out = stdout(&mut select(
        repo,
        &f.change,
        "alice",
        &target(repo),
        &[&evaluation],
    ));
    assert!(out.contains("promoted: alice"), "{out}");
    let selected = repository_events(repo, "candidate-selected").pop().unwrap();
    assert_eq!(selected["reads"][0]["record"], "whole");
}

#[test]
fn a_lead_repair_makes_the_lead_a_contributor_and_not_an_independent_reviewer() {
    let f = fixture(Some("never"), &[], false);
    let repo = &f.repo;
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n\n[danger]\npaths = [\"answer.txt\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-q", "-m", "policy"]);

    // The lead repairs carol's answer by registering the repair as a child.
    let repaired = tree_with(repo, "carol's answer, repaired\n");
    register(
        repo,
        &f.change,
        "carol-repair",
        &repaired,
        "lead",
        &["--parent", "carol"],
    );
    let evaluation = evaluated(repo, "carol-repair");
    let out = stdout(
        select(
            repo,
            &f.change,
            "carol-repair",
            &target(repo),
            &[&evaluation],
        )
        .env("ARC_ACTOR", "lead"),
    );
    assert!(out.contains("contributors: carol,lead"), "{out}");
    let patchset = patchsets(repo, &f.change).pop().unwrap();
    assert_eq!(
        patchset["contributors"],
        serde_json::json!(["carol", "lead"])
    );
    let promoted = git_out(
        &repo.root,
        &["log", "-1", "--format=%cn", &branch_head(repo, &f.change)],
    );
    assert_eq!(promoted, "lead", "the selector commits the promotion");

    let out = repo
        .arc(&repo.root)
        .env("ARC_ACTOR", "lead")
        .args(["review", "answer", "--verdict", "approved"])
        .output()
        .unwrap();
    assert!(out.status.success());
    // The verdict stands as a fact, and policy refuses it as an independent
    // review: the lead contributed to what it approved.
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("self-approval: reviewer matches contributor lead"),
        "{said}"
    );
    let check = repo
        .arc(&repo.root)
        .args(["check", "answer"])
        .output()
        .unwrap();
    assert!(!check.status.success());
    let _ = (f.carol, f.shared);
}

#[test]
fn a_target_moved_before_the_effect_leaves_the_selection_unpromoted_until_reselected() {
    let f = fixture(Some("never"), &[], false);
    let repo = &f.repo;
    let evaluation = evaluated(repo, "alice");
    let before = branch_head(repo, &f.change);
    let decided = target(repo);

    // The selection pauses once it is recorded; the target moves before the
    // promotion re-reads the basis, and only then is the pause released.
    let release = repo.home.join("release-promotion");
    let out = thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while repository_events(repo, "candidate-selected").is_empty() {
                assert!(Instant::now() < deadline, "no selection was recorded");
                thread::sleep(Duration::from_millis(10));
            }
            repo.commit(&repo.root, "later.txt", "later\n", "the target moves");
            fs::write(&release, "").unwrap();
        });
        select(repo, &f.change, "alice", &decided, &[&evaluation])
            .env("ARC_SELECT_PAUSE", &release)
            .output()
            .unwrap()
    });
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("basis-moved: "), "{stderr}");

    let first = repository_events(repo, "candidate-selected").pop().unwrap();
    let first_id = first["event_id"].as_str().unwrap().to_string();
    assert!(repository_events(repo, "candidate-promoted").is_empty());
    assert!(patchsets(repo, &f.change).is_empty());
    assert_eq!(branch_head(repo, &f.change), before);

    // The basis is never reused.
    repo.arc(&repo.root)
        .args(["candidate", "promote", &first_id])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("basis-moved"));
    let stale = refused(&mut select(
        repo,
        &f.change,
        "alice",
        &decided,
        &[&evaluation],
    ));
    assert!(stale.contains("target-moved"), "{stale}");

    // Selecting again validates afresh and supersedes the stranded one.
    let out = stdout(&mut select(
        repo,
        &f.change,
        "alice",
        &target(repo),
        &[&evaluation],
    ));
    assert!(out.contains(&format!("supersedes: {first_id}")), "{out}");
    assert!(out.contains("promoted: alice"), "{out}");
    let promoted = repository_events(repo, "candidate-promoted").pop().unwrap();
    assert_ne!(promoted["selection"], first_id.as_str());
    repo.arc(&repo.root)
        .args(["candidate", "promote", &first_id])
        .assert()
        .failure()
        .stderr(predicates::str::contains("superseded"));
}

/// A `reference-transaction` hook acting when the destination branch moves:
/// in `prepared` it fails the update, in `committed` it kills the arc
/// process that asked for it.
fn hook_on_branch(repo: &Repo, state: &str, action: &str) {
    let hook = repo.root.join(".git/hooks/reference-transaction");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\n[ \"$1\" = {state} ] || exit 0\n\
             grep -q ' refs/heads/arc/answer$' || exit 0\n{action}\n"
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&hook).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    fs::set_permissions(&hook, permissions).unwrap();
}

fn promotion_refs(repo: &Repo) -> String {
    git_out(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/arc/candidate-promotion/",
        ],
    )
}

#[test]
fn promote_completes_or_discards_an_interrupted_promotion() {
    // Interrupted after the ref: the branch never moved.
    let f = fixture(Some("never"), &[], false);
    let repo = &f.repo;
    let evaluation = evaluated(repo, "alice");
    let before = branch_head(repo, &f.change);
    hook_on_branch(repo, "prepared", "exit 1");
    let out = select(repo, &f.change, "alice", &target(repo), &[&evaluation])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("promotion stopped"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let selection = repository_events(repo, "candidate-selected").pop().unwrap();
    let selection = selection["event_id"].as_str().unwrap().to_string();
    assert!(promotion_refs(repo).contains(&selection));
    assert_eq!(branch_head(repo, &f.change), before);
    let doctor = repo.arc(&repo.root).args(["doctor"]).output().unwrap();
    let report = String::from_utf8_lossy(&doctor.stdout);
    assert!(report.contains("interrupted-promotion"), "{report}");
    let shown = stdout(repo.arc(&repo.root).args(["candidate", "show", "alice"]));
    assert!(shown.contains("interrupted promotion"), "{shown}");

    let out = stdout(
        repo.arc(&repo.root)
            .args(["candidate", "promote", &selection]),
    );
    assert!(out.contains("discarded"), "{out}");
    assert_eq!(promotion_refs(repo), "");
    fs::remove_file(repo.root.join(".git/hooks/reference-transaction")).unwrap();
    let out = stdout(
        repo.arc(&repo.root)
            .args(["candidate", "promote", &selection]),
    );
    assert!(out.contains("promoted: alice"), "{out}");
    assert_eq!(patchsets(repo, &f.change).len(), 1);

    // Interrupted after the branch moved: nothing recorded it.
    let g = fixture(Some("never"), &[], false);
    let repo = &g.repo;
    let evaluation = evaluated(repo, "bob");
    hook_on_branch(
        repo,
        "committed",
        "kill -9 \"$(awk '/^PPid:/ {print $2}' /proc/$PPID/status)\"",
    );
    let out = select(repo, &g.change, "bob", &target(repo), &[&evaluation])
        .output()
        .unwrap();
    assert!(!out.status.success());
    fs::remove_file(repo.root.join(".git/hooks/reference-transaction")).unwrap();
    let selection = repository_events(repo, "candidate-selected").pop().unwrap();
    let selection = selection["event_id"].as_str().unwrap().to_string();
    let held = promotion_refs(repo);
    assert!(held.contains(&branch_head(repo, &g.change)), "{held}");
    assert!(patchsets(repo, &g.change).is_empty());
    assert!(repository_events(repo, "candidate-promoted").is_empty());

    let out = stdout(
        repo.arc(&repo.root)
            .args(["candidate", "promote", &selection]),
    );
    assert!(out.contains("completing"), "{out}");
    assert!(out.contains("promoted: bob"), "{out}");
    assert_eq!(patchsets(repo, &g.change).len(), 1);
    let doctor = repo.arc(&repo.root).args(["doctor"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&doctor.stdout).contains("interrupted-promotion"));

    // A duplicate promote is a no-op that says so.
    let out = stdout(
        repo.arc(&repo.root)
            .args(["candidate", "promote", &selection]),
    );
    assert!(out.contains("promoted already"), "{out}");
    assert_eq!(patchsets(repo, &g.change).len(), 1);
    assert_eq!(repository_events(repo, "candidate-promoted").len(), 1);
}

#[test]
fn the_losing_sibling_keeps_its_ref_and_retire_refuses_while_selected() {
    let f = fixture(Some("never"), &[], false);
    let repo = &f.repo;
    let evaluation = evaluated(repo, "alice");
    stdout(&mut select(
        repo,
        &f.change,
        "alice",
        &target(repo),
        &[&evaluation],
    ));
    let selection = repository_events(repo, "candidate-selected").pop().unwrap();

    // The siblings stay registered, pinned, and unjudged.
    let pins = git_out(
        &repo.root,
        &["for-each-ref", "--format=%(refname)", "refs/arc/candidate/"],
    );
    for sibling in ["bob", "carol", "dave"] {
        assert!(
            pins.contains(&format!("refs/arc/candidate/{sibling}")),
            "{pins}"
        );
    }
    let refused = repo
        .arc(&repo.root)
        .args(["candidate", "retire", "alice"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("rooted-candidate"), "{stderr}");
    assert!(
        stderr.contains(selection["event_id"].as_str().unwrap()),
        "{stderr}"
    );
    assert!(git_out(&repo.root, &["rev-parse", "refs/arc/candidate/alice"]) == f.shared);

    // The explanation lists them as the alternatives the selection left.
    let explained = json_stdout(repo.arc(&repo.root).args(["explain", "answer", "--json"]));
    let rows = explained["rejected_alternatives"]["rows"]
        .as_array()
        .unwrap();
    for sibling in ["bob", "carol", "dave"] {
        assert!(
            rows.iter().any(|row| row["candidate_id"] == sibling),
            "{explained}"
        );
    }
    let contract = explained["contract"]["rows"].as_array().unwrap();
    assert!(
        contract
            .iter()
            .any(|row| row["selection"] == selection["event_id"]),
        "{explained}"
    );

    // A sibling no root reaches may be retired.
    repo.arc(&repo.root)
        .args([
            "candidate",
            "judge",
            "dave",
            "--rejected",
            "--reason",
            "weaker",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["candidate", "retire", "dave"])
        .assert()
        .success();
}

#[test]
fn reused_evaluation_supplier_retirement_preserves_basis_bytes_and_later_reuse() {
    for interrupted in [false, true] {
        let f = fixture(Some("matching-coordinates"), &[], false);
        let repo = &f.repo;
        let evaluation = evaluated(repo, "alice");
        let registrations = repository_events(repo, "candidate-registered");
        let evaluations = repository_events(repo, "candidate-verified");
        if interrupted {
            hook_on_branch(repo, "prepared", "exit 1");
            select(repo, &f.change, "bob", &target(repo), &[&evaluation])
                .assert()
                .failure()
                .stderr(predicates::str::contains("promotion stopped"));
            fs::remove_file(repo.root.join(".git/hooks/reference-transaction")).unwrap();
            assert!(repository_events(repo, "candidate-promoted").is_empty());
        } else {
            stdout(&mut select(
                repo,
                &f.change,
                "bob",
                &target(repo),
                &[&evaluation],
            ));
            assert_eq!(repository_events(repo, "candidate-promoted").len(), 1);
        }
        let selections = repository_events(repo, "candidate-selected");
        assert_eq!(selections[0]["evaluations"][0]["candidate_id"], "alice");
        repo.arc(&repo.root)
            .args(["candidate", "retire", "alice"])
            .assert()
            .success();
        let pin = Command::new("git")
            .current_dir(&repo.root)
            .args(["show-ref", "--verify", "refs/arc/candidate/alice"])
            .output()
            .unwrap();
        assert!(!pin.status.success());
        assert_eq!(
            git_out(&repo.root, &["rev-parse", "refs/arc/candidate/bob"]),
            f.shared
        );
        git(&repo.root, &["gc", "--prune=now"]);
        assert_eq!(
            git_out(&repo.root, &["show", &format!("{}:answer.txt", f.shared)]),
            "the shared answer"
        );
        assert_eq!(
            repository_events(repo, "candidate-registered"),
            registrations
        );
        assert_eq!(repository_events(repo, "candidate-verified"), evaluations);
        assert_eq!(repository_events(repo, "candidate-selected"), selections);
        let shown =
            json_stdout(
                repo.arc(&repo.root)
                    .args(["candidate", "show", "alice", "--json"]),
            );
        assert!(!shown["candidates"][0]["retired"].is_null(), "{shown}");
        let doctor = repo.arc(&repo.root).args(["doctor"]).output().unwrap();
        let report = String::from_utf8_lossy(&doctor.stdout);
        assert!(!report.contains("rooted-candidate-unpinned"), "{report}");
        stdout(&mut select(
            repo,
            &f.change,
            "bob",
            &target(repo),
            &[&evaluation],
        ));
        let latest = repository_events(repo, "candidate-selected").pop().unwrap();
        assert_eq!(latest["evaluations"][0]["candidate_id"], "alice");
        assert_eq!(latest["evaluations"][0]["event_id"], evaluation);
        repo.arc(&repo.root)
            .args(["candidate", "retire", "bob"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("rooted-candidate"));
        repo.arc(&repo.root)
            .args([
                "candidate",
                "select",
                "--chosen",
                "alice",
                "--into",
                &f.change,
                "--target",
                &target(repo),
                "--evaluation",
                &evaluation,
                "--rationale",
                "retired choice",
            ])
            .assert()
            .failure()
            .stderr(predicates::str::contains("candidate-retired"));
    }
}

#[test]
fn reused_supplier_retirement_refuses_when_content_ancestry_roots_it() {
    for edge in ["--parent", "--adopts"] {
        let f = fixture(Some("matching-coordinates"), &[], false);
        let repo = &f.repo;
        register(
            repo,
            &f.change,
            "child",
            &f.shared,
            "alice",
            &[edge, "alice"],
        );
        let evaluation = evaluated(repo, "alice");
        stdout(&mut select(
            repo,
            &f.change,
            "child",
            &target(repo),
            &[&evaluation],
        ));
        let before = repository_events(repo, "candidate-retired");
        repo.arc(&repo.root)
            .args(["candidate", "retire", "alice"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("rooted-candidate"));
        assert_eq!(repository_events(repo, "candidate-retired"), before);
        assert_eq!(
            git_out(&repo.root, &["rev-parse", "refs/arc/candidate/alice"]),
            f.shared
        );
    }
}

/// A candidate gate whose command exits 75 was not admitted: nothing ran, so
/// the evaluation records `not-admitted` and reports it as unrun rather than
/// as a failing gate.
#[test]
fn a_candidate_gate_exiting_75_records_not_admitted() {
    let f = fixture(None, &[], false);
    let repo = &f.repo;
    declare(repo, &GATES.replace("test -f answer.txt", "exit 75"), None);
    let out = repo
        .arc(&repo.root)
        .args(["candidate", "verify", "alice"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("gate build: not-admitted"), "{text}");
    let recorded = repository_events(repo, "candidate-verified");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["result"], "not-admitted");
}

fn budget_declaration(repo: &Repo, budget: &str) {
    declare(repo, &format!("{GATES}budget='{budget}'\n"), Some("never"));
}

fn budget_evaluation(repo: &Repo, candidate: &str, run_ms: u64, code: i32) -> String {
    repo.arc(&repo.root)
        .env("ARC_TEST_COMMAND_RUN_MS", run_ms.to_string())
        .args(["candidate", "verify", candidate])
        .assert()
        .code(code);
    repository_events(repo, "candidate-verified").pop().unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Claim: candidate verification and selection refuse the same over-budget raw
/// pass without moving either branch and fences readers through format 8.
/// Rejects discarded timing, pass-only selection and an understated format barrier.
#[test]
fn candidate_budget_refuses_execution_and_selection_without_moving_heads() {
    let f = fixture(Some("never"), &[], false);
    let repo = &f.repo;
    budget_declaration(repo, "10s");
    let head = branch_head(repo, &f.change);
    let target = target(repo);
    let evaluation = budget_evaluation(repo, "alice", 11300, 1);
    let raw = repository_events(repo, "candidate-verified").pop().unwrap();
    assert_eq!(raw["result"], "pass");
    assert_eq!(raw["run_ms"], 11300);
    assert_eq!(raw["execution_limit_ms"], 100000);
    assert!(raw["timeout_seconds"].is_null());
    let shown = json_stdout(
        repo.arc(&repo.root)
            .args(["candidate", "show", "alice", "--json"]),
    );
    assert_eq!(shown["candidates"][0]["evaluations"][0]["run_ms"], 11300);
    let refusal = refused(&mut select(
        repo,
        &f.change,
        "alice",
        &target,
        &[&evaluation],
    ));
    assert!(
        refusal.contains("over-budget") && refusal.contains("ran 11.3s of 10s"),
        "{refusal}"
    );
    assert_eq!(branch_head(repo, &f.change), head);
    assert_eq!(self::target(repo), target);
    assert!(repository_events(repo, "candidate-selected").is_empty());
    assert!(repository_events(repo, "candidate-promoted").is_empty());
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 9);
}

/// Claim: promotion rejudges the selected receipt under a reduced operator budget
/// even when neither coordinate moved. Rejects treating a recorded selection as cost authority.
#[test]
fn candidate_budget_reduction_between_selection_and_promotion_refuses_the_effect() {
    let f = fixture(Some("never"), &[], false);
    let repo = &f.repo;
    budget_declaration(repo, "20s");
    let evaluation = budget_evaluation(repo, "alice", 11300, 0);
    let head = branch_head(repo, &f.change);
    let target = target(repo);
    let release = repo.home.join("promotion-budget-release");
    let mut child = repo.spawn_arc_piped_with_env(
        &repo.root,
        &[
            "candidate",
            "select",
            "--chosen",
            "alice",
            "--into",
            &f.change,
            "--target",
            &target,
            "--evaluation",
            &evaluation,
            "--rationale",
            "answer",
        ],
        &[("ARC_SELECT_PAUSE", &release)],
    );
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let selection = loop {
        let mut line = String::new();
        assert!(
            output.read_line(&mut line).unwrap() > 0,
            "selection was not recorded"
        );
        if let Some(id) = line.trim().strip_prefix("selection: ") {
            break id.to_string();
        }
    };
    budget_declaration(repo, "10s");
    fs::write(&release, "").unwrap();
    let result = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("promotion refused") && stderr.contains("ran 11.3s of 10s"),
        "{stderr}"
    );
    assert_eq!(branch_head(repo, &f.change), head);
    assert_eq!(self::target(repo), target);
    assert!(repository_events(repo, "candidate-promoted").is_empty());
    repo.arc(&repo.root)
        .args(["candidate", "promote", &selection])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("promotion refused"));
}

/// Claim: the candidate tree's tighter and added budget obligations bind execution
/// and selection. Rejects consulting only the destination head's declaration.
#[test]
fn candidate_own_tighter_budget_is_required() {
    let f = fixture(Some("never"), &[], false);
    let repo = &f.repo;
    budget_declaration(repo, "20s");
    let gates = piped(
        repo,
        &["hash-object", "-w", "--stdin"],
        &format!("{GATES}budget='10s'\n[gates.extra]\ncommand='true'\nbudget='10s'\n"),
    );
    let arc_tree = piped(
        repo,
        &["mktree"],
        &format!("100644 blob {gates}\tgates.toml\n"),
    );
    let answer = piped(repo, &["hash-object", "-w", "--stdin"], "answer\n");
    let tree = piped(
        repo,
        &["mktree"],
        &format!("040000 tree {arc_tree}\t.arc\n100644 blob {answer}\tanswer.txt\n"),
    );
    register(repo, &f.change, "tight", &tree, "alice", &[]);
    let head = branch_head(repo, &f.change);
    let target = target(repo);
    budget_evaluation(repo, "tight", 11300, 1);
    let evaluations = repository_events(repo, "candidate-verified");
    assert_eq!(evaluations.len(), 2);
    let ids = evaluations
        .iter()
        .map(|e| e["event_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    let refusal = refused(&mut select(repo, &f.change, "tight", &target, &ids));
    assert!(
        refusal.contains("gate build") && refusal.contains("ran 11.3s of 10s"),
        "{refusal}"
    );
    assert!(refusal.contains("gate extra"), "{refusal}");
    assert_eq!(branch_head(repo, &f.change), head);
    assert_eq!(self::target(repo), target);
}

/// Claim: legacy untimed evaluations remain readable but cannot meet a budget.
/// Rejects treating duration_ms or an absent command observation as measured evidence.
#[test]
fn candidate_untimed_evaluation_is_readable_and_unmeasured() {
    let f = fixture(Some("never"), &[], false);
    let repo = &f.repo;
    let evaluation = evaluated(repo, "alice");
    let directory = repo.root.join(".git/arc/repository/events");
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if value["event_id"] == evaluation {
            value.as_object_mut().unwrap().remove("run_ms");
            value["duration_ms"] = serde_json::json!(1);
            fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        }
    }
    budget_declaration(repo, "10s");
    let refusal = refused(&mut select(
        repo,
        &f.change,
        "alice",
        &target(repo),
        &[&evaluation],
    ));
    assert!(
        refusal.contains("unmeasured") && refusal.contains("run_ms is missing"),
        "{refusal}"
    );
}

/// Claim: exported candidate evidence retains command timing and its absence
/// through import and reduction. Rejects transport losing measurements or inventing legacy ones.
#[test]
fn candidate_timing_and_absence_survive_export_import() {
    for timed in [true, false] {
        let f = fixture(Some("never"), &[], false);
        let source = &f.repo;
        budget_evaluation(source, "alice", 11300, 0);
        if !timed {
            for entry in fs::read_dir(source.root.join(".git/arc/repository/events")).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let mut event: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                if event["event_type"] == "candidate-verified" {
                    event.as_object_mut().unwrap().remove("run_ms");
                    fs::write(path, serde_json::to_vec(&event).unwrap()).unwrap();
                }
            }
        }
        let bundle = source.home.join("candidate-timing.json");
        source
            .arc(&source.root)
            .args(["export", &f.change, "--output", bundle.to_str().unwrap()])
            .assert()
            .success();
        let receiver = Repo::new();
        deliver_git_objects(source, &receiver);
        receiver
            .arc(&receiver.root)
            .args(["import", bundle.to_str().unwrap()])
            .assert()
            .success();
        let exported: serde_json::Value =
            serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
        let raw = exported["repository_events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["event_type"] == "candidate-verified")
            .unwrap();
        let shown = json_stdout(receiver.arc(&receiver.root).args([
            "candidate",
            "show",
            "alice",
            "--json",
        ]));
        let expected = if timed {
            serde_json::json!(11300)
        } else {
            serde_json::Value::Null
        };
        assert_eq!(raw["run_ms"], expected);
        assert_eq!(shown["candidates"][0]["evaluations"][0]["run_ms"], expected);
    }
}

/// Claim: candidate selection requires its own passing evaluation even when the
/// destination carries bypass history. Rejects a bypass replacing a missing or
/// failing candidate evaluation, or promoting the refused selection.
#[test]
fn retired_bypass_does_not_authorize_candidates() {
    let f = fixture(None, &[], false);
    let repo = &f.repo;
    declare(
        repo,
        "[gates.build]\ncommand='false'\nbypass='documentation only'\n",
        Some("never"),
    );
    crate::verification_outcomes::historical_bypass(repo, &repo.root, &f.change, "build");
    let before = branch_head(repo, &f.change);
    let missing = refused(&mut select(repo, &f.change, "alice", &target(repo), &[]));
    assert!(missing.contains("no-evaluation"), "{missing}");
    repo.arc(&repo.root)
        .args(["candidate", "verify", "alice"])
        .assert()
        .code(1);
    let evaluation = repository_events(repo, "candidate-verified").pop().unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let failed = refused(&mut select(
        repo,
        &f.change,
        "alice",
        &target(repo),
        &[&evaluation],
    ));
    assert!(failed.contains("failed"), "{failed}");
    assert_eq!(branch_head(repo, &f.change), before);
    assert!(repository_events(repo, "candidate-selected").is_empty());
}

/// I5: candidate execution, selection and promotion accept a measured busy-host
/// excuse and rejudge its reference after tightening. Rejects clock-only candidate
/// refusal and promoting a previously selected excuse under an incompatible budget.
#[test]
fn candidate_busy_host_excuse_is_rejudged_before_promotion() {
    let f = fixture(Some("never"), &[], false);
    let repo = &f.repo;
    budget_declaration(repo, "1s");
    for ms in [900, 1100] {
        repo.arc(&repo.root)
            .env("ARC_TEST_COMMAND_RUN_MS", ms.to_string())
            .env("ARC_TEST_GATE_WORK", r#"{"instructions":1000,"host_busy_ms":5100,"host_capacity_ms":10000,"cpu_user_ms":90,"cpu_system_ms":10}"#)
            .args(["candidate", "verify", "alice"]).assert().success();
    }
    let evaluation = repository_events(repo, "candidate-verified").pop().unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        repository_events(repo, "candidate-verified").pop().unwrap()["budget_at_run"]["verdict"],
        "excused"
    );
    let target = target(repo);
    let release = repo.home.join("busy-promotion-release");
    let mut child = repo.spawn_arc_piped_with_env(
        &repo.root,
        &[
            "candidate",
            "select",
            "--chosen",
            "alice",
            "--into",
            &f.change,
            "--target",
            &target,
            "--evaluation",
            &evaluation,
            "--rationale",
            "answer",
        ],
        &[("ARC_SELECT_PAUSE", &release)],
    );
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let selection = loop {
        let mut line = String::new();
        assert!(
            output.read_line(&mut line).unwrap() > 0,
            "selection not recorded"
        );
        if let Some(id) = line.trim().strip_prefix("selection: ") {
            break id.to_owned();
        }
    };
    budget_declaration(repo, "899ms");
    fs::write(&release, "").unwrap();
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(1), "{:?}", result);
    assert!(String::from_utf8_lossy(&result.stderr).contains("promotion refused"));
    repo.arc(&repo.root)
        .args(["candidate", "promote", &selection])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("promotion refused"));
}
