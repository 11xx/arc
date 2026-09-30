use super::common::*;

/// A change carrying one brief version, returned as its id.
fn briefed_change(repo: &Repo, slug: &str, body: &str) -> String {
    let change_id = begin_no_worktree(repo, slug, &[]);
    brief(repo, slug, body, false);
    change_id
}

fn brief(repo: &Repo, slug: &str, body: &str, revision: bool) {
    let mut command = repo.arc(&repo.root);
    command.args(["brief", slug, "--body-file", "-"]);
    if revision {
        command.args(["--cause-note", "fixture revision"]);
    }
    command.write_stdin(body).assert().success();
}

/// A tree holding one file, written to the object store and referenced by
/// nothing.
fn tree_with(repo: &Repo, content: &str) -> String {
    let blob = piped(repo, &["hash-object", "-w", "--stdin"], content);
    piped(
        repo,
        &["mktree"],
        &format!("100644 blob {blob}\tfile.txt\n"),
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

fn register(repo: &Repo, args: &[&str]) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.args(["candidate", "register"]).args(args);
    command
}

fn registered(repo: &Repo, args: &[&str]) -> String {
    let out = register(repo, args).output().unwrap();
    assert!(
        out.status.success(),
        "register {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("candidate: "))
        .expect("register prints the candidate id")
        .to_string()
}

fn refused(repo: &Repo, args: &[&str], code: &str) -> String {
    let out = register(repo, args).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{args:?} was accepted");
    assert!(
        stderr.contains(code),
        "{args:?} refused without {code}: {stderr}"
    );
    stderr
}

fn list(repo: &Repo, root: &Path) -> serde_json::Value {
    json_stdout(repo.arc(root).args(["candidate", "list", "--json"]))
}

fn repository_event_files(repo: &Repo) -> Vec<String> {
    let dir = repo.root.join(".git/arc/repository/events");
    let mut names = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

fn candidate_refs(repo: &Repo) -> String {
    git_out(
        &repo.root,
        &["for-each-ref", "--format=%(refname)", "refs/arc/candidate/"],
    )
}

fn claim_ids(repo: &Repo, change_id: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for entry in fs::read_dir(event_dir(repo, change_id)).unwrap() {
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
        if value["event_type"] == "claim-set" {
            ids.push(value["claim_id"].as_str().unwrap().to_string());
        }
    }
    ids
}

#[test]
fn two_registrations_share_one_tree_as_two_identities() {
    let repo = Repo::new();
    briefed_change(&repo, "answer", "Answer the question.\n");
    let tree = tree_with(&repo, "one answer\n");
    let first = registered(
        &repo,
        &["--tree", &tree, "--brief", "answer", "--producer", "alice"],
    );
    let second = registered(
        &repo,
        &["--tree", &tree, "--brief", "answer", "--producer", "bob"],
    );
    assert_ne!(first, second);
    // A third continues the first under the same contract.
    let commit = git_out(
        &repo.root,
        &[
            "commit-tree",
            &tree_with(&repo, "repaired\n"),
            "-m",
            "repair",
        ],
    );
    let third = registered(
        &repo,
        &[
            "--tree",
            &commit,
            "--brief",
            "answer",
            "--producer",
            "carol",
            "--parent",
            &first,
        ],
    );

    let view = list(&repo, &repo.root);
    assert_eq!(view["schema"], "arc-candidate/1", "{view}");
    let candidates = view["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 3, "{view}");
    let by_id = |id: &str| {
        candidates
            .iter()
            .find(|candidate| candidate["candidate_id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by_id(&first)["tree"], tree);
    assert_eq!(by_id(&second)["tree"], tree);
    assert_eq!(by_id(&first)["producers"], serde_json::json!(["alice"]));
    assert_eq!(by_id(&second)["producers"], serde_json::json!(["bob"]));
    assert_eq!(by_id(&first)["pin"]["present"], true);
    // A commit is registered as its tree.
    assert_eq!(
        by_id(&third)["tree"],
        git_out(&repo.root, &["rev-parse", &format!("{commit}^{{tree}}")])
    );
    assert_eq!(by_id(&third)["parents"], serde_json::json!([first]));
    let mut sharing = vec![first.clone(), second.clone()];
    sharing.sort();
    assert_eq!(
        view["shared_trees"],
        serde_json::json!([{ "tree": tree, "candidates": sharing }]),
        "{view}"
    );
    let brief = &by_id(&first)["brief"];
    assert!(brief["digest"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(brief, &by_id(&second)["brief"]);

    // Registering opened no change and recorded no patchset.
    let changes = fs::read_dir(repo.root.join(".git/arc/changes"))
        .unwrap()
        .count();
    assert_eq!(changes, 1);
    let shown = stdout(repo.arc(&repo.root).args(["candidate", "show", &first]));
    assert!(shown.contains(&format!("shared tree {tree}")), "{shown}");
    assert!(shown.contains("producers: alice"), "{shown}");
}

#[test]
fn a_parent_must_share_the_contract() {
    let repo = Repo::new();
    briefed_change(&repo, "answer", "Version one.\n");
    briefed_change(&repo, "elsewhere", "Another contract.\n");
    let tree = tree_with(&repo, "content\n");
    let parent = registered(
        &repo,
        &["--tree", &tree, "--brief", "answer", "--producer", "alice"],
    );
    registered(
        &repo,
        &[
            "--tree",
            &tree,
            "--brief",
            "answer",
            "--producer",
            "bob",
            "--parent",
            &parent,
        ],
    );
    let stderr = refused(
        &repo,
        &[
            "--tree",
            &tree,
            "--brief",
            "elsewhere",
            "--producer",
            "bob",
            "--parent",
            &parent,
        ],
        "parent-other-contract",
    );
    assert!(stderr.contains(&parent), "{stderr}");
    // A later version of the same change's brief is another contract too.
    brief(&repo, "answer", "Version two.\n", true);
    refused(
        &repo,
        &[
            "--tree",
            &tree,
            "--brief",
            "answer",
            "--producer",
            "bob",
            "--parent",
            &parent,
        ],
        "parent-other-contract",
    );
}

#[test]
fn an_adoption_must_carry_the_adopted_producers() {
    let repo = Repo::new();
    briefed_change(&repo, "first", "The first contract.\n");
    briefed_change(&repo, "second", "The second contract.\n");
    let tree = tree_with(&repo, "content\n");
    let original = registered(
        &repo,
        &["--tree", &tree, "--brief", "first", "--producer", "alice"],
    );
    let stderr = refused(
        &repo,
        &[
            "--tree",
            &tree,
            "--brief",
            "second",
            "--producer",
            "bob",
            "--adopts",
            &original,
        ],
        "adoption-drops-producer",
    );
    assert!(stderr.contains("alice"), "{stderr}");
    registered(
        &repo,
        &[
            "--tree",
            &tree,
            "--brief",
            "second",
            "--producer",
            "alice",
            "--producer",
            "bob",
            "--adopts",
            &original,
        ],
    );

    // Adopting a repair carries the repaired candidate's producers as well.
    let repair = registered(
        &repo,
        &[
            "--tree",
            &tree,
            "--brief",
            "first",
            "--producer",
            "carol",
            "--parent",
            &original,
        ],
    );
    let stderr = refused(
        &repo,
        &[
            "--tree",
            &tree,
            "--brief",
            "second",
            "--producer",
            "carol",
            "--adopts",
            &repair,
        ],
        "adoption-drops-producer",
    );
    assert!(stderr.contains("alice"), "{stderr}");
    registered(
        &repo,
        &[
            "--tree",
            &tree,
            "--brief",
            "second",
            "--producer",
            "carol",
            "--producer",
            "alice",
            "--adopts",
            &repair,
        ],
    );
}

#[test]
fn registration_refusals_write_nothing() {
    let repo = Repo::new();
    let change = briefed_change(&repo, "answer", "Answer.\n");
    let other = briefed_change(&repo, "other", "Other.\n");
    repo.arc(&repo.root)
        .args(["claim", "answer"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["claim", "other"])
        .assert()
        .success();
    let own_claim = claim_ids(&repo, &change).pop().unwrap();
    let foreign_claim = claim_ids(&repo, &other).pop().unwrap();
    let tree = tree_with(&repo, "content\n");
    registered(
        &repo,
        &[
            "--id",
            "taken",
            "--tree",
            &tree,
            "--brief",
            "answer",
            "--producer",
            "alice",
            "--episode",
            &own_claim,
        ],
    );
    let events = repository_event_files(&repo);
    let refs = candidate_refs(&repo);

    let missing_tree = "0123456789abcdef0123456789abcdef01234567";
    for (args, code) in [
        (
            vec![
                "--id",
                "taken",
                "--tree",
                &tree,
                "--brief",
                "answer",
                "--producer",
                "bob",
            ],
            "duplicate-candidate",
        ),
        (
            vec!["--id", "fresh", "--tree", &tree, "--brief", "answer"],
            "no-producers",
        ),
        (
            vec![
                "--id",
                "fresh",
                "--tree",
                missing_tree,
                "--brief",
                "answer",
                "--producer",
                "bob",
            ],
            "unknown-tree",
        ),
        (
            vec![
                "--id",
                "fresh",
                "--tree",
                &tree,
                "--brief",
                "answer",
                "--producer",
                "bob",
                "--parent",
                "nobody",
            ],
            "unknown-parent",
        ),
        (
            vec![
                "--id",
                "fresh",
                "--tree",
                &tree,
                "--brief",
                "answer",
                "--producer",
                "bob",
                "--episode",
                &foreign_claim,
            ],
            "unknown-episode",
        ),
        (
            vec![
                "--id",
                "fresh",
                "--tree",
                &tree,
                "--brief",
                "answer@01NOSUCHBRIEF",
                "--producer",
                "bob",
            ],
            "unknown-brief",
        ),
    ] {
        refused(&repo, &args, code);
        assert_eq!(
            repository_event_files(&repo),
            events,
            "{code} wrote an event"
        );
        assert_eq!(candidate_refs(&repo), refs, "{code} wrote a ref");
    }
}

#[test]
fn export_and_import_carry_candidates() {
    let source = Repo::new();
    briefed_change(&source, "answer", "Answer.\n");
    let tree = tree_with(&source, "content\n");
    let first = registered(
        &source,
        &["--tree", &tree, "--brief", "answer", "--producer", "alice"],
    );
    registered(
        &source,
        &[
            "--tree",
            &tree,
            "--brief",
            "answer",
            "--producer",
            "bob",
            "--parent",
            &first,
        ],
    );
    let bundle = source.home.join("answer.json");
    source
        .arc(&source.root)
        .args(["export", "answer", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let destination = Repo::new();
    let imported = stdout(
        destination
            .arc(&destination.root)
            .args(["import", bundle.to_str().unwrap()]),
    );
    assert!(
        imported.contains("tree not held here; pin absent"),
        "{imported}"
    );

    let strip_pins = |mut view: serde_json::Value| {
        for candidate in view["candidates"].as_array_mut().unwrap() {
            candidate.as_object_mut().unwrap().remove("pin");
        }
        view
    };
    let there = list(&destination, &destination.root);
    assert_eq!(
        strip_pins(list(&source, &source.root)),
        strip_pins(there.clone())
    );
    assert!(there["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|candidate| candidate["pin"]["present"] == false));
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(destination.root.join(".git/arc/config.json")).unwrap())
            .unwrap();
    assert_eq!(config["schema_version"], 7, "{config}");
}

/// A bundle's candidate events are judged by the registration rules before
/// anything is written.
#[test]
fn import_refuses_a_bundle_whose_adoption_drops_a_producer() {
    let source = Repo::new();
    briefed_change(&source, "answer", "Answer.\n");
    briefed_change(&source, "later", "Later.\n");
    let tree = tree_with(&source, "content\n");
    let original = registered(
        &source,
        &["--tree", &tree, "--brief", "answer", "--producer", "alice"],
    );
    registered(
        &source,
        &[
            "--tree",
            &tree,
            "--brief",
            "later",
            "--producer",
            "alice",
            "--producer",
            "bob",
            "--adopts",
            &original,
        ],
    );
    let path = source.home.join("answer.json");
    source
        .arc(&source.root)
        .args(["export", "answer", "--output", path.to_str().unwrap()])
        .assert()
        .success();
    let mut bundle: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for event in bundle["repository_events"].as_array_mut().unwrap() {
        if event["adopts"].is_array() {
            event["producers"] = serde_json::json!(["bob"]);
        }
    }
    fs::write(&path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("adoption-drops-producer"));
    assert!(!destination.root.join(".git/arc/changes").exists());
    assert!(repository_event_files(&destination).is_empty());
}

#[test]
fn doctor_names_a_candidate_ref_without_a_registration() {
    let repo = Repo::new();
    briefed_change(&repo, "answer", "Answer.\n");
    let tree = tree_with(&repo, "content\n");
    let clean = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    assert!(!clean.to_string().contains("candidate"), "{clean}");

    let kept = registered(
        &repo,
        &["--tree", &tree, "--brief", "answer", "--producer", "alice"],
    );
    git(
        &repo.root,
        &["update-ref", "refs/arc/candidate/stray", &tree],
    );
    let out = repo
        .arc(&repo.root)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "advice alone keeps doctor clean"
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let advice = report["advice"].as_array().unwrap();
    let stray = advice
        .iter()
        .filter(|finding| finding["code"] == "unregistered-candidate-ref")
        .collect::<Vec<_>>();
    assert_eq!(stray.len(), 1, "{report}");
    assert!(stray[0]["detail"]
        .as_str()
        .unwrap()
        .contains("refs/arc/candidate/stray"));
    assert!(!stray[0]["detail"].as_str().unwrap().contains(&kept));
    assert!(
        advice
            .iter()
            .any(|finding| finding["code"] == "candidate-evaluation-reuse-undeclared"),
        "{report}"
    );

    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[candidates]\nevaluation_reuse = \"never\"\n",
    )
    .unwrap();
    let report = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    assert!(
        !report["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "candidate-evaluation-reuse-undeclared"),
        "{report}"
    );
    let policy = stdout(repo.arc(&repo.root).args(["policy", "show"]));
    assert!(
        policy.contains("candidates.evaluation_reuse = never (declared by .arc/policy.toml)"),
        "{policy}"
    );
}

#[test]
fn retire_deletes_an_unrooted_candidate_ref() {
    let repo = Repo::new();
    briefed_change(&repo, "answer", "Answer.\n");
    let tree = tree_with(&repo, "content\n");
    let kept = registered(
        &repo,
        &["--tree", &tree, "--brief", "answer", "--producer", "alice"],
    );
    let retired = registered(
        &repo,
        &["--tree", &tree, "--brief", "answer", "--producer", "bob"],
    );
    repo.arc(&repo.root)
        .args([
            "candidate",
            "judge",
            &retired,
            "--superseded-by",
            &kept,
            "--reason",
            "kept is smaller",
        ])
        .assert()
        .success();

    let out = stdout(repo.arc(&repo.root).args(["candidate", "retire", &retired]));
    assert!(out.contains(&format!("retired: {retired}")), "{out}");
    let refs = candidate_refs(&repo);
    assert!(!refs.contains(&retired), "{refs}");
    assert!(refs.contains(&kept), "{refs}");

    // The registration and its judgement stand; only the pin is gone.
    let shown = json_stdout(
        repo.arc(&repo.root)
            .args(["candidate", "show", &retired, "--json"]),
    );
    let candidate = &shown["candidates"][0];
    assert_eq!(candidate["tree"], tree);
    assert_eq!(candidate["pin"]["present"], false);
    assert_eq!(candidate["retired"]["declarant"], "tester");
    assert_eq!(candidate["judgements"][0]["kind"], "superseded-by");
    assert_eq!(candidate["judgements"][0]["candidate_id"], kept);

    let events = repository_event_files(&repo);
    let again = stdout(repo.arc(&repo.root).args(["candidate", "retire", &retired]));
    assert!(again.contains("already retired"), "{again}");
    assert_eq!(repository_event_files(&repo), events);
    // Content still shared with a pinned candidate stays in the object store.
    assert_eq!(git_out(&repo.root, &["cat-file", "-t", &tree]), "tree");
}
