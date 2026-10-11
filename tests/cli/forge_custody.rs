//! Custody of the bytes forge observations reference: materialization at
//! observe, availability reads, and transport beside an export.
use super::common::*;
use super::forge_observation::{document, setup, state};
use serde_json::{json, Value};

const PAGE: &str = "{}";

fn digest(bytes: &str) -> String {
    format!("{:x}", Sha256::digest(bytes.as_bytes()))
}

/// An adapter's materialization directory: `DIR/sha256/<digest>` per body.
fn materialize(dir: &Path, bodies: &[&str]) {
    fs::create_dir_all(dir.join("sha256")).unwrap();
    for body in bodies {
        fs::write(dir.join("sha256").join(digest(body)), body).unwrap();
    }
}

fn custody_file(repo: &Repo, body: &str) -> PathBuf {
    repo.root.join(".git/arc/custody/sha256").join(digest(body))
}

fn observe_in(repo: &Repo, id: &str, v: &Value, dir: Option<&Path>) -> Value {
    fs::write(repo.root.join("frontier.json"), v.to_string()).unwrap();
    let mut command = repo.arc(&repo.root);
    command.args(["forge", "observe", id, "--file", "frontier.json", "--json"]);
    if let Some(dir) = dir {
        command.args(["--custody-dir", dir.to_str().unwrap()]);
    }
    serde_json::from_str(&stdout(&mut command)).unwrap()
}

fn availability_of_generation<'a>(receipt: &'a Value, generation: &str) -> &'a str {
    receipt["availability"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["supports"]["generation_id"] == generation)
        .unwrap_or_else(|| panic!("no reference supports {generation}: {receipt}"))["availability"]
        .as_str()
        .unwrap()
}

fn availability_of_response<'a>(receipt: &'a Value, response: &str) -> &'a str {
    receipt["availability"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["supports"]["response_id"] == response)
        .unwrap_or_else(|| panic!("no reference supports {response}: {receipt}"))["availability"]
        .as_str()
        .unwrap()
}

fn second_read() -> Value {
    let mut v = document("read-2", "g2", "second", Some("g1"));
    v["previous_observation"] = json!("read-1");
    v
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

#[test]
fn forge_observe_materializes_adapter_files_into_custody() {
    let (repo, id) = setup();
    let supplied = repo.home.join("supplied");
    materialize(&supplied, &["first", PAGE]);
    let receipt = observe_in(
        &repo,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&supplied),
    );
    assert_eq!(availability_of_generation(&receipt, "g1"), "available");
    assert_eq!(availability_of_response(&receipt, "page-1"), "available");
    for body in ["first", PAGE] {
        assert_eq!(
            fs::read(custody_file(&repo, body)).unwrap(),
            body.as_bytes()
        );
    }
    // Only referenced digests are copied: an unrelated file in the adapter's
    // directory is not custody.
    fs::write(supplied.join("sha256").join("a".repeat(64)), "stray").unwrap();
    observe_in(
        &repo,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&supplied),
    );
    assert_eq!(
        files_in(&repo.root.join(".git/arc/custody/sha256")).len(),
        2
    );
    // Without the directory the reference reads from custody alone.
    let mut text = repo.arc(&repo.root);
    text.args(["forge", "observe", &id, "--file", "frontier.json"]);
    assert!(stdout(&mut text).contains("2 available"));
}

#[test]
fn forge_unmaterialized_reference_reads_unavailable_never_empty() {
    let (repo, id) = setup();
    let supplied = repo.home.join("supplied");
    materialize(&supplied, &[PAGE]);
    let receipt = observe_in(
        &repo,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&supplied),
    );
    assert_eq!(availability_of_generation(&receipt, "g1"), "unavailable");
    assert_eq!(availability_of_response(&receipt, "page-1"), "available");
    assert!(!custody_file(&repo, "first").exists());

    // Content that is genuinely empty is available only while its file is.
    let mut empty = document("read-empty", "g-empty", "", None);
    empty["items"][0]["node_id"] = json!("comment-2");
    empty["generations"][0]["node_id"] = json!("comment-2");
    let empty_dir = repo.home.join("empty-supplied");
    materialize(&empty_dir, &["", PAGE]);
    let receipt = observe_in(&repo, &id, &empty, Some(&empty_dir));
    assert_eq!(availability_of_generation(&receipt, "g-empty"), "available");
    fs::remove_file(custody_file(&repo, "")).unwrap();
    let receipt = observe_in(&repo, &id, &empty, None);
    assert_eq!(
        availability_of_generation(&receipt, "g-empty"),
        "unavailable"
    );

    // Damaged bytes are neither available nor absent, and a truncated file is
    // not the empty content.
    let again = repo.home.join("again");
    materialize(&again, &["first"]);
    observe_in(
        &repo,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&again),
    );
    fs::write(custody_file(&repo, "first"), "").unwrap();
    let receipt = observe_in(&repo, &id, &document("read-1", "g1", "first", None), None);
    assert_eq!(availability_of_generation(&receipt, "g1"), "corrupt");
    fs::write(custody_file(&repo, "first"), "fifth").unwrap();
    let receipt = observe_in(&repo, &id, &document("read-1", "g1", "first", None), None);
    assert_eq!(availability_of_generation(&receipt, "g1"), "corrupt");
}

#[test]
fn forge_missing_content_degrades_only_dependent_claims() {
    let (repo, id) = setup();
    let supplied = repo.home.join("supplied");
    materialize(&supplied, &["first", "second", PAGE]);
    let first = document("read-1", "g1", "first", None);
    let second = second_read();
    observe_in(&repo, &id, &first, Some(&supplied));
    observe_in(&repo, &id, &second, Some(&supplied));
    let events = event_count(&repo, &id);
    let recorded = state(&repo, &id);

    fs::remove_file(custody_file(&repo, "first")).unwrap();

    let intact = observe_in(&repo, &id, &second, None);
    assert_eq!(availability_of_generation(&intact, "g2"), "available");
    assert_eq!(availability_of_response(&intact, "page-1"), "available");
    let degraded = observe_in(&repo, &id, &first, None);
    assert_eq!(availability_of_generation(&degraded, "g1"), "unavailable");
    assert_eq!(availability_of_response(&degraded, "page-1"), "available");

    // The retained facts and judgments are untouched, and nothing is invented.
    assert_eq!(degraded["outcome"], "already-recorded");
    assert_eq!(event_count(&repo, &id), events);
    assert_eq!(state(&repo, &id)["forge"], recorded["forge"]);
    let mut text = repo.arc(&repo.root);
    text.args(["forge", "observe", &id, "--file", "frontier.json"]);
    let text = stdout(&mut text);
    assert!(text.contains("1 unavailable"), "{text}");
}

#[test]
fn forge_observe_republishes_missing_content_from_a_new_directory() {
    let (repo, id) = setup();
    let first = document("read-1", "g1", "first", None);
    observe_in(&repo, &id, &first, None);
    assert!(!custody_file(&repo, "first").exists());
    let supplied = repo.home.join("later");
    materialize(&supplied, &["first"]);
    let receipt = observe_in(&repo, &id, &first, Some(&supplied));
    assert_eq!(receipt["outcome"], "already-recorded");
    assert_eq!(availability_of_generation(&receipt, "g1"), "available");
    assert_eq!(availability_of_response(&receipt, "page-1"), "unavailable");
}

/// A commit held locally that the provider observed, then rewritten away.
fn rewritten_observed_head(repo: &Repo, id: &str) -> (PathBuf, String, String) {
    let wt = repo.home.join(".worktrees/repo-frontier");
    repo.commit(&wt, "observed.txt", "one\n", "feat: observed");
    let a = repo.head(&wt);
    fs::write(wt.join("observed.txt"), "two\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "--amend", "--no-edit"]);
    let b = repo.head(&wt);
    assert_ne!(a, b);
    stdout(repo.arc(&wt).args(["snapshot", id]));
    (wt, a, b)
}

fn rewrite_to(repo: &Repo, wt: &Path, a: &str, b: &str) {
    let map = repo.home.join("rewrite-map");
    fs::write(&map, format!("{a} {b}\n")).unwrap();
    stdout(repo.arc(wt).args([
        "history",
        "rewrite",
        "--map",
        map.to_str().unwrap(),
        "--reason",
        "amended after observation",
    ]));
}

fn observed_at(head: &str, mut v: Value) -> Value {
    v["observed_head_sha"] = json!(head);
    v
}

fn retained_refs(repo: &Repo, id: &str) -> Vec<String> {
    git_out(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/arc/retained/{id}/"),
        ],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

fn doctor_advice(repo: &Repo) -> Vec<String> {
    let report = json_stdout_any_status(repo.arc(&repo.root).args(["doctor", "--json"]));
    report["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "unretained-commit")
        .map(|finding| finding["detail"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn forge_observed_commit_is_retained_across_a_rewrite() {
    let (repo, id) = setup();
    let (wt, a, b) = rewritten_observed_head(&repo, &id);
    observe_in(
        &repo,
        &id,
        &observed_at(&a, document("read-1", "g1", "first", None)),
        None,
    );
    // The provider's commit is cited and held here, and nothing reaches it.
    let advice = doctor_advice(&repo);
    assert!(
        advice
            .iter()
            .any(|detail| detail.contains(&a[..8]) && detail.contains("forge observed head")),
        "{advice:?}"
    );
    assert!(retained_refs(&repo, &id).is_empty());

    rewrite_to(&repo, &wt, &a, &b);
    let held = retained_refs(&repo, &id);
    assert_eq!(held.len(), 1, "{held:?}");
    assert!(held[0].ends_with(&format!("/{a}")));
    assert!(doctor_advice(&repo).is_empty());
    // The commit the provider never reported is not held.
    assert!(!held
        .iter()
        .any(|name| name.ends_with("cccccccccccccccccccccccccccccccccccccccc")));
}

#[test]
fn forge_observed_commit_is_pinned_on_import_without_a_rewrite() {
    let (source, id) = setup();
    let wt = source.home.join(".worktrees/repo-frontier");
    source.commit(&wt, "observed.txt", "one\n", "feat: observed");
    let a = source.head(&wt);
    observe_in(
        &source,
        &id,
        &observed_at(&a, document("read-1", "g1", "first", None)),
        None,
    );
    let bundle = source.home.join("forge.json");
    let objects = source.home.join("objects.json");
    stdout(source.arc(&source.root).args([
        "export",
        &id,
        "--output",
        bundle.to_str().unwrap(),
        "--objects-json",
        objects.to_str().unwrap(),
    ]));
    let listed: Value = serde_json::from_slice(&fs::read(&objects).unwrap()).unwrap();
    assert!(listed["objects"]
        .as_array()
        .unwrap()
        .contains(&json!({"oid": a, "role": "observed-commit"})));
    // No sidecar was written, so the bundle claims no custody.
    let bundle_json: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert!(bundle_json.get("custody").is_none());

    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    stdout(&mut admitted_change_command(&replica, &bundle));
    let pins = retained_refs(&replica, &id);
    assert!(
        pins.iter().any(|name| name.ends_with(&format!("/{a}"))),
        "{pins:?}"
    );
}

#[test]
fn forge_custody_corruption_and_unsafe_paths_refuse_before_publication() {
    let (repo, id) = setup();
    let custody = repo.root.join(".git/arc/custody");
    let first = document("read-1", "g1", "first", None);

    // Observe: every refusal leaves the ledger and custody as they were,
    // including when other files in the same directory are valid.
    let refuse = |dir: &Path, why: &str, needle: &str| {
        let before = event_count(&repo, &id);
        fs::write(repo.root.join("frontier.json"), first.to_string()).unwrap();
        repo.arc(&repo.root)
            .args([
                "forge",
                "observe",
                &id,
                "--file",
                "frontier.json",
                "--custody-dir",
                dir.to_str().unwrap(),
            ])
            .assert()
            .failure()
            .stderr(predicates::str::contains(needle));
        assert_eq!(event_count(&repo, &id), before, "{why}");
        assert!(
            files_in(&custody.join("sha256")).is_empty(),
            "{why}: custody was published"
        );
    };
    let valid_page = |dir: &Path| {
        fs::create_dir_all(dir.join("sha256")).unwrap();
        fs::write(dir.join("sha256").join(digest(PAGE)), PAGE).unwrap();
    };

    let wrong_digest = repo.home.join("wrong-digest");
    valid_page(&wrong_digest);
    fs::write(wrong_digest.join("sha256").join(digest("first")), "FIRST").unwrap();
    refuse(&wrong_digest, "same length, other bytes", "recorded digest");

    let wrong_length = repo.home.join("wrong-length");
    valid_page(&wrong_length);
    fs::write(wrong_length.join("sha256").join(digest("first")), "first!").unwrap();
    refuse(
        &wrong_length,
        "longer content",
        "longer than the recorded length",
    );

    let truncated = repo.home.join("truncated");
    valid_page(&truncated);
    fs::write(truncated.join("sha256").join(digest("first")), "").unwrap();
    refuse(&truncated, "emptied content", "has length 0");

    let directory = repo.home.join("directory");
    valid_page(&directory);
    fs::create_dir(directory.join("sha256").join(digest("first"))).unwrap();
    refuse(
        &directory,
        "directory in place of a file",
        "not a regular file",
    );

    let outside = repo.home.join("outside-first");
    fs::write(&outside, "first").unwrap();
    let escaping = repo.home.join("escaping");
    valid_page(&escaping);
    std::os::unix::fs::symlink(&outside, escaping.join("sha256").join(digest("first"))).unwrap();
    refuse(
        &escaping,
        "symlink out of the directory",
        "leaves the custody directory",
    );

    let linked_layout = repo.home.join("linked-layout");
    fs::create_dir_all(&linked_layout).unwrap();
    let elsewhere = repo.home.join("elsewhere");
    materialize(&elsewhere, &["first", PAGE]);
    std::os::unix::fs::symlink(elsewhere.join("sha256"), linked_layout.join("sha256")).unwrap();
    refuse(
        &linked_layout,
        "layout directory out of the directory",
        "leaves the custody directory",
    );

    let no_layout = repo.home.join("no-layout");
    fs::create_dir_all(&no_layout).unwrap();
    refuse(
        &no_layout,
        "directory without the sha256 layout",
        "no sha256/ directory",
    );

    // A valid directory still publishes afterwards.
    let good = repo.home.join("good");
    materialize(&good, &["first", PAGE]);
    let receipt = observe_in(&repo, &id, &first, Some(&good));
    assert_eq!(availability_of_generation(&receipt, "g1"), "available");
}

/// A receiver that admits the exporter's facts and its history rewrites.
fn admitted_change_command(repo: &Repo, path: &Path) -> AssertCommand {
    let bytes = fs::read(path).unwrap();
    let bundle: Value = serde_json::from_slice(&bytes).unwrap();
    let source = bundle["source_replica_id"].as_str().unwrap();
    repo.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='fixture-change'\nsource_replica_id='{source}'\n[[replica.publishers.origins]]\nreplica_id='{source}'\nroles=['facts','rewrite']\nactors=[]\n"));
    let mut command = repo.arc(&repo.root);
    command.args([
        "import",
        path.to_str().unwrap(),
        "--publisher",
        "fixture-change",
        "--received-sha256",
        &format!("{:x}", Sha256::digest(&bytes)),
    ]);
    command
}
