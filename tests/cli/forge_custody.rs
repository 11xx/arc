//! Custody of the bytes forge observations reference: materialization at
//! observe, availability reads, and transport beside an export.
use super::common::*;
use super::forge_observation::{document, setup, state};
use serde_json::{json, Value};

const PAGE: &str = "{}";

pub(crate) fn digest(bytes: &str) -> String {
    format!("{:x}", Sha256::digest(bytes.as_bytes()))
}

/// An adapter's materialization directory: `DIR/sha256/<digest>` per body.
pub(crate) fn materialize(dir: &Path, bodies: &[&str]) {
    fs::create_dir_all(dir.join("sha256")).unwrap();
    for body in bodies {
        fs::write(dir.join("sha256").join(digest(body)), body).unwrap();
    }
}

pub(crate) fn custody_file(repo: &Repo, body: &str) -> PathBuf {
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

fn replica_status(repo: &Repo) -> Value {
    json_stdout(repo.arc(&repo.root).args(["replica", "status", "--json"]))
}

#[test]
fn forge_custody_round_trip_recovers_generations_and_original_objects() {
    let (source, id) = setup();
    let (wt, a, b) = rewritten_observed_head(&source, &id);
    let supplied = source.home.join("supplied");
    materialize(&supplied, &["first", "second", PAGE]);
    observe_in(
        &source,
        &id,
        &observed_at(&a, document("read-1", "g1", "first", None)),
        Some(&supplied),
    );
    let mut second = observed_at(&a, second_read());
    second["observed_base_sha"] = json!(b);
    observe_in(&source, &id, &second, Some(&supplied));
    rewrite_to(&source, &wt, &a, &b);

    // The provider-observed commit, no longer on any branch, stays held.
    let held = retained_refs(&source, &id);
    assert!(
        held.iter().any(|name| name.ends_with(&format!("/{a}"))),
        "{held:?}"
    );

    let before = replica_status(&source);
    assert!(before["project_id"].is_null() && before["local"].is_null());
    let replicas_dir = source.root.join(".git/arc/replicas");
    let had_replicas_dir = replicas_dir.exists();

    let bundle = source.home.join("forge.json");
    let objects = source.home.join("objects.json");
    let sidecar = source.home.join("forge.custody");
    let exported = stdout(source.arc(&source.root).args([
        "export",
        &id,
        "--output",
        bundle.to_str().unwrap(),
        "--objects-json",
        objects.to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    assert!(exported.contains("custody: 3 entries, 3 available, 0 unavailable"));
    assert!(exported.contains("complete"));
    assert!(!exported.contains("incomplete"));

    // Export is an archive operation: it creates no replica identity.
    assert_eq!(replica_status(&source), before);
    assert_eq!(replicas_dir.exists(), had_replicas_dir);

    // The sidecar carries each digest once and the bundle binds its manifest.
    assert_eq!(files_in(&sidecar.join("sha256")).len(), 3);
    let manifest_bytes = fs::read(sidecar.join("manifest.json")).unwrap();
    let manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(manifest["schema"], "arc-custody-bundle/1");
    assert_eq!(manifest["complete"], true);
    let bundle_json: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert_eq!(bundle_json["schema"], "arc-bundle/6");
    assert_eq!(bundle_json["custody"]["schema"], "arc-custody-bundle/1");
    assert_eq!(
        bundle_json["custody"]["manifest_sha256"],
        format!("{:x}", Sha256::digest(&manifest_bytes))
    );

    // The Git closure lists the held observed commit and omits the one the
    // provider reported that this repository never held.
    let listed: Value = serde_json::from_slice(&fs::read(&objects).unwrap()).unwrap();
    let listed = listed["objects"].as_array().unwrap();
    assert!(listed.contains(&json!({"oid": a, "role": "observed-commit"})));
    assert!(listed.contains(&json!({"oid": b, "role": "patchset-head"})));
    assert!(!listed
        .iter()
        .any(|object| object["oid"] == "cccccccccccccccccccccccccccccccccccccccc"));

    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    let before_replica = replica_status(&replica);
    let imported = stdout(
        admitted_change_command(&replica, &bundle)
            .args(["--custody-dir", sidecar.to_str().unwrap()]),
    );
    assert!(imported.contains("custody: 3 installed, 0 held, 0 unavailable"));
    assert_eq!(replica_status(&replica), before_replica);

    for body in ["first", "second", PAGE] {
        assert_eq!(
            fs::read(custody_file(&replica, body)).unwrap(),
            body.as_bytes()
        );
    }
    let first = observed_at(&a, document("read-1", "g1", "first", None));
    let receipt = observe_in(&replica, &id, &first, None);
    assert_eq!(receipt["outcome"], "already-recorded");
    assert_eq!(availability_of_generation(&receipt, "g1"), "available");
    let receipt = observe_in(&replica, &id, &second, None);
    assert_eq!(availability_of_generation(&receipt, "g2"), "available");
    assert_eq!(
        state(&replica, &id)["forge"]["observations"],
        state(&source, &id)["forge"]["observations"]
    );
    // The original object is held here under the same retention ref.
    git(
        &replica.root,
        &["cat-file", "-e", &format!("{a}^{{commit}}")],
    );
    assert!(retained_refs(&replica, &id)
        .iter()
        .any(|name| name.ends_with(&format!("/{a}"))));
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
fn forge_incomplete_custody_never_claims_complete() {
    let (source, id) = setup();
    let supplied = source.home.join("supplied");
    materialize(&supplied, &["first", "second", PAGE]);
    observe_in(
        &source,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&supplied),
    );
    observe_in(&source, &id, &second_read(), Some(&supplied));
    fs::remove_file(custody_file(&source, "second")).unwrap();
    // A damaged source is never shipped as if it were content.
    fs::write(custody_file(&source, "first"), "damaged").unwrap();

    let bundle = source.home.join("forge.json");
    let sidecar = source.home.join("forge.custody");
    let exported = stdout(source.arc(&source.root).args([
        "export",
        &id,
        "--output",
        bundle.to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    assert!(
        exported.contains("custody: 3 entries, 1 available, 2 unavailable"),
        "{exported}"
    );
    assert!(exported.contains("incomplete"));
    let manifest: Value =
        serde_json::from_slice(&fs::read(sidecar.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["complete"], false);
    let entries = manifest["entries"].as_array().unwrap();
    let state_of = |body: &str| {
        entries
            .iter()
            .find(|entry| entry["sha256"] == digest(body))
            .unwrap()
            .clone()
    };
    assert_eq!(state_of("second")["state"], "unavailable");
    assert_eq!(state_of("second")["reason"], "absent");
    assert_eq!(state_of("first")["state"], "unavailable");
    assert_eq!(state_of("first")["reason"], "corrupt");
    assert_eq!(state_of(PAGE)["state"], "available");
    assert_eq!(files_in(&sidecar.join("sha256")).len(), 1);

    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    let imported = stdout(
        admitted_change_command(&replica, &bundle)
            .args(["--custody-dir", sidecar.to_str().unwrap()]),
    );
    assert!(
        imported.contains("custody: 1 installed, 0 held, 2 unavailable"),
        "{imported}"
    );
    assert!(imported.contains("incomplete"));
    assert!(custody_file(&replica, PAGE).exists());
    assert!(!custody_file(&replica, "first").exists());
    assert!(!custody_file(&replica, "second").exists());
    let receipt = observe_in(&replica, &id, &second_read(), None);
    assert_eq!(availability_of_generation(&receipt, "g2"), "unavailable");

    // Repairing the source and exporting again completes the replica, even
    // though no event is new.
    materialize(&supplied, &["first", "second", PAGE]);
    observe_in(
        &source,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&supplied),
    );
    observe_in(&source, &id, &second_read(), Some(&supplied));
    let repaired = source.home.join("repaired.custody");
    let bundle = source.home.join("repaired.json");
    stdout(source.arc(&source.root).args([
        "export",
        &id,
        "--output",
        bundle.to_str().unwrap(),
        "--custody-dir",
        repaired.to_str().unwrap(),
    ]));
    let imported = stdout(
        admitted_change_command(&replica, &bundle)
            .args(["--custody-dir", repaired.to_str().unwrap()]),
    );
    assert!(
        imported.contains("custody: 2 installed, 1 held, 0 unavailable"),
        "{imported}"
    );
    let receipt = observe_in(&replica, &id, &second_read(), None);
    assert_eq!(availability_of_generation(&receipt, "g2"), "available");
    let mut dry = admitted_change_command(&replica, &bundle);
    let dry = stdout(dry.args(["--dry-run", "--custody-dir", repaired.to_str().unwrap()]));
    assert!(
        dry.contains("custody: 0 would install, 3 held, 0 unavailable"),
        "{dry}"
    );
}

#[test]
fn forge_bundle_binding_a_sidecar_reports_unsupplied_custody() {
    let (source, id) = setup();
    let supplied = source.home.join("supplied");
    materialize(&supplied, &["first", PAGE]);
    observe_in(
        &source,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&supplied),
    );
    let bundle = source.home.join("forge.json");
    let sidecar = source.home.join("forge.custody");
    stdout(source.arc(&source.root).args([
        "export",
        &id,
        "--output",
        bundle.to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    // The sidecar directory is not exported over an existing one.
    source
        .arc(&source.root)
        .args([
            "export",
            &id,
            "--output",
            bundle.to_str().unwrap(),
            "--custody-dir",
            sidecar.to_str().unwrap(),
        ])
        .assert()
        .failure();
    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    let imported = stdout(&mut admitted_change_command(&replica, &bundle));
    assert!(
        imported.contains("custody: sidecar not supplied; 2 references unavailable"),
        "{imported}"
    );
    assert!(!replica.root.join(".git/arc/custody").exists());
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

    // Import: a sidecar is judged before the first event or file lands.
    let (source, source_id) = setup();
    let supplied = source.home.join("supplied");
    materialize(&supplied, &["first", PAGE]);
    observe_in(&source, &source_id, &first, Some(&supplied));
    let bundle_path = source.home.join("forge.json");
    let sidecar = source.home.join("forge.custody");
    stdout(source.arc(&source.root).args([
        "export",
        &source_id,
        "--output",
        bundle_path.to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    let original_bundle: Value = serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
    let original_manifest: Value =
        serde_json::from_slice(&fs::read(sidecar.join("manifest.json")).unwrap()).unwrap();

    let attempt = |label: &str, edit: &dyn Fn(&Path, &mut Value, &mut Value), needle: &str| {
        let replica = Repo::new();
        deliver_git_objects(&source, &replica);
        let work = replica.home.join(label);
        fs::create_dir_all(&work).unwrap();
        let copy = work.join("sidecar");
        copy_dir(&sidecar, &copy);
        let mut bundle = original_bundle.clone();
        let mut manifest = original_manifest.clone();
        edit(&copy, &mut bundle, &mut manifest);
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(copy.join("manifest.json"), &manifest_bytes).unwrap();
        if bundle["custody"]["manifest_sha256"] == original_bundle["custody"]["manifest_sha256"] {
            bundle["custody"]["manifest_sha256"] =
                json!(format!("{:x}", Sha256::digest(&manifest_bytes)));
        }
        let bundle_file = work.join("bundle.json");
        fs::write(&bundle_file, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
        let output = admitted_change_command(&replica, &bundle_file)
            .args(["--custody-dir", copy.to_str().unwrap()])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{label}: {text}");
        assert!(
            text.contains(needle),
            "{label}: wanted {needle:?} in {text}"
        );
        assert!(
            !replica.root.join(".git/arc/custody").exists(),
            "{label}: custody was published"
        );
        assert_eq!(
            published_batches(&replica),
            0,
            "{label}: events were published"
        );
    };

    attempt(
        "unbound-manifest",
        &|_, bundle, manifest| {
            manifest["complete"] = json!(true);
            bundle["custody"]["manifest_sha256"] = json!("0".repeat(64));
        },
        "manifest",
    );
    attempt(
        "corrupt-content",
        &|dir, _, manifest| {
            let entry = &manifest["entries"][0];
            let path = dir.join(entry["path"].as_str().unwrap());
            let length = fs::metadata(&path).unwrap().len() as usize;
            fs::write(path, vec![b'x'; length]).unwrap();
        },
        "digest",
    );
    attempt(
        "short-content",
        &|dir, _, manifest| {
            let entry = &manifest["entries"][0];
            fs::write(dir.join(entry["path"].as_str().unwrap()), "").unwrap();
        },
        "length",
    );
    for (label, path) in [
        ("parent-path", "../outside"),
        ("absolute-path", "/etc/hostname"),
        ("nested-parent", "sha256/../../outside"),
        ("backslash-path", "sha256\\file"),
        ("empty-path", ""),
        ("other-name", "sha256/other"),
    ] {
        attempt(
            label,
            &|_, _, manifest| {
                manifest["entries"][0]["path"] = json!(path);
            },
            "path",
        );
    }
    attempt(
        "false-completeness",
        &|_, _, manifest| {
            manifest["entries"][0] = json!({
                "sha256": manifest["entries"][0]["sha256"],
                "byte_length": manifest["entries"][0]["byte_length"],
                "state": "unavailable",
                "reason": "absent",
            });
        },
        "complete",
    );
    attempt(
        "unreferenced-entry",
        &|_, _, manifest| {
            manifest["entries"].as_array_mut().unwrap().push(json!({
                "sha256": "b".repeat(64),
                "byte_length": 1,
                "state": "unavailable",
                "reason": "absent",
            }));
        },
        "reference",
    );
    attempt(
        "omitted-entry",
        &|_, _, manifest| {
            manifest["entries"].as_array_mut().unwrap().pop();
        },
        "reference",
    );
    attempt(
        "wrong-schema",
        &|_, _, manifest| {
            manifest["schema"] = json!("arc-custody-bundle/2");
        },
        "schema",
    );
    attempt(
        "symlinked-content",
        &|dir, _, manifest| {
            let path = dir.join(manifest["entries"][0]["path"].as_str().unwrap());
            let held = fs::read(&path).unwrap();
            let target = dir.parent().unwrap().join("target");
            fs::write(&target, held).unwrap();
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
        },
        "symlink",
    );

    // A sidecar offered to a bundle that binds none is refused too.
    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    let unbound = replica.home.join("unbound.json");
    let mut bundle = original_bundle.clone();
    bundle.as_object_mut().unwrap().remove("custody");
    fs::write(&unbound, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    admitted_change_command(&replica, &unbound)
        .args(["--custody-dir", sidecar.to_str().unwrap()])
        .assert()
        .failure();
    assert!(!replica.root.join(".git/arc/custody").exists());
    assert_eq!(published_batches(&replica), 0);

    // The same sidecar, untouched, imports.
    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    admitted_change_command(&replica, &bundle_path)
        .args(["--custody-dir", sidecar.to_str().unwrap()])
        .assert()
        .success();
}

/// Import batches the store has published; imported events live in them.
fn published_batches(repo: &Repo) -> usize {
    fs::read_dir(repo.root.join(".git/arc/imports"))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A receiver that admits the exporter's facts and its history rewrites.
pub(crate) fn admitted_change_command(repo: &Repo, path: &Path) -> AssertCommand {
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

/// Export a change with one observation whose content is in custody, and the
/// sidecar beside the bundle. Returns the bundle, the sidecar and the repo.
fn exported_with_custody() -> (Repo, String, PathBuf, PathBuf) {
    let (source, id) = setup();
    let supplied = source.home.join("supplied");
    materialize(&supplied, &["first", PAGE]);
    observe_in(
        &source,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&supplied),
    );
    let bundle = source.home.join("forge.json");
    let sidecar = source.home.join("forge.custody");
    stdout(source.arc(&source.root).args([
        "export",
        &id,
        "--output",
        bundle.to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    (source, id, bundle, sidecar)
}

#[cfg(debug_assertions)]
fn spawn_paused(template: AssertCommand, phase: &str) -> Child {
    let mut command = Command::new(template.get_program());
    command
        .args(template.get_args())
        .current_dir(template.get_current_dir().unwrap())
        .env("ARC_TEST_IMPORT_RENDEZVOUS", phase)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    command.spawn().unwrap()
}

#[cfg(debug_assertions)]
#[test]
fn forge_custody_is_not_reported_installed_when_publication_fails() {
    use std::io::Write;
    let (source, _id, bundle, sidecar) = exported_with_custody();
    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    let mut template = admitted_change_command(&replica, &bundle);
    template.args(["--custody-dir", sidecar.to_str().unwrap()]);
    let mut child = spawn_paused(template, "format");
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    assert_eq!(line, "arc test: import format\n");
    // The publisher's enrollment changes between admission and publication.
    let source_id = serde_json::from_slice::<Value>(&fs::read(&bundle).unwrap()).unwrap()
        ["source_replica_id"]
        .as_str()
        .unwrap()
        .to_string();
    replica.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='fixture-change'\nsource_replica_id='{source_id}'\n[[replica.publishers.origins]]\nreplica_id='{source_id}'\nroles=['review-input']\nactors=[]\n"));
    child.stdin.take().unwrap().write_all(b"+").unwrap();
    let output = child.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{out}");
    assert!(
        !out.contains("installed"),
        "custody claimed installed though nothing published: {out}"
    );
    assert!(!replica.root.join(".git/arc/custody").exists());
    assert_eq!(published_batches(&replica), 0);
}

#[test]
fn forge_custody_installs_after_publication_and_a_failure_names_the_published_events() {
    let (source, _id, bundle, sidecar) = exported_with_custody();
    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    // A custody directory that cannot be created fails after publication.
    fs::create_dir_all(replica.root.join(".git/arc")).unwrap();
    fs::write(replica.root.join(".git/arc/custody"), "in the way").unwrap();
    let output = admitted_change_command(&replica, &bundle)
        .args(["--custody-dir", sidecar.to_str().unwrap()])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{out}{err}");
    assert!(!out.contains("installed"), "{out}");
    assert!(
        err.contains("events are published") && err.contains("--custody-dir"),
        "{err}"
    );
    assert_eq!(published_batches(&replica), 1);

    // The same import again finishes the installation and records nothing.
    fs::remove_file(replica.root.join(".git/arc/custody")).unwrap();
    let again = stdout(
        admitted_change_command(&replica, &bundle)
            .args(["--custody-dir", sidecar.to_str().unwrap()]),
    );
    assert!(
        again.contains("custody: 2 installed, 0 held, 0 unavailable"),
        "{again}"
    );
    assert_eq!(published_batches(&replica), 1);
}

#[test]
fn forge_failed_custody_export_leaves_nothing_behind() {
    let (source, id) = setup();
    let supplied = source.home.join("supplied");
    materialize(&supplied, &["first", PAGE]);
    observe_in(
        &source,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&supplied),
    );
    let parent = source.home.join("outgoing");
    fs::create_dir(&parent).unwrap();
    let sidecar = parent.join("forge.custody");
    // The bundle cannot be written, so the sidecar must not be left behind.
    source
        .arc(&source.root)
        .args([
            "export",
            &id,
            "--output",
            source.home.join("missing/dir/forge.json").to_str().unwrap(),
            "--custody-dir",
            sidecar.to_str().unwrap(),
        ])
        .assert()
        .failure();
    assert!(files_in(&parent).is_empty(), "{:?}", files_in(&parent));

    // An existing empty directory stays as it was.
    fs::create_dir(&sidecar).unwrap();
    source
        .arc(&source.root)
        .args([
            "export",
            &id,
            "--output",
            source.home.join("missing/dir/forge.json").to_str().unwrap(),
            "--custody-dir",
            sidecar.to_str().unwrap(),
        ])
        .assert()
        .failure();
    assert_eq!(files_in(&parent), ["forge.custody"]);
    assert!(files_in(&sidecar).is_empty());

    // A refused non-empty directory is not touched.
    fs::write(sidecar.join("keep"), "mine").unwrap();
    source
        .arc(&source.root)
        .args([
            "export",
            &id,
            "--output",
            source.home.join("forge.json").to_str().unwrap(),
            "--custody-dir",
            sidecar.to_str().unwrap(),
        ])
        .assert()
        .failure();
    assert_eq!(files_in(&sidecar), ["keep"]);
    assert_eq!(files_in(&parent), ["forge.custody"]);

    // Success replaces the empty directory with the finished sidecar alone.
    fs::remove_file(sidecar.join("keep")).unwrap();
    stdout(source.arc(&source.root).args([
        "export",
        &id,
        "--output",
        source.home.join("forge.json").to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    assert_eq!(files_in(&parent), ["forge.custody"]);
    assert_eq!(files_in(&sidecar), ["manifest.json", "sha256"]);
}

fn doctor_custody(repo: &Repo) -> Vec<(String, String)> {
    let report = json_stdout_any_status(repo.arc(&repo.root).args(["doctor", "--json"]));
    ["problems", "advice"]
        .iter()
        .flat_map(|list| report[list].as_array().unwrap().clone())
        .filter(|finding| finding["code"].as_str().unwrap().starts_with("custody-"))
        .map(|finding| {
            (
                finding["code"].as_str().unwrap().to_string(),
                finding["detail"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn doctor_reports_damaged_custody_and_orphaned_temporary_files() {
    let (repo, id) = setup();
    let supplied = repo.home.join("supplied");
    materialize(&supplied, &["first", PAGE]);
    observe_in(
        &repo,
        &id,
        &document("read-1", "g1", "first", None),
        Some(&supplied),
    );
    assert!(doctor_custody(&repo).is_empty());

    let objects = repo.root.join(".git/arc/custody/sha256");
    // A write in progress is not an orphan.
    let fresh = objects.join(format!(".{}.fresh.tmp", digest("first")));
    fs::write(&fresh, "partial").unwrap();
    assert!(doctor_custody(&repo).is_empty());

    let orphan = objects.join(format!(".{}.interrupted.tmp", digest("first")));
    fs::write(&orphan, "partial").unwrap();
    fs::File::options()
        .write(true)
        .open(&orphan)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(7 * 3600))
        .unwrap();
    fs::write(custody_file(&repo, "first"), "FIRST").unwrap();
    let held_before = fs::read(custody_file(&repo, PAGE)).unwrap();

    let found = doctor_custody(&repo);
    let codes: Vec<_> = found.iter().map(|(code, _)| code.as_str()).collect();
    assert_eq!(
        codes,
        ["custody-content-mismatch", "custody-temp-orphaned"],
        "{found:?}"
    );
    assert!(found[0].1.contains(&digest("first")), "{found:?}");
    assert!(found[1].1.contains("interrupted.tmp"), "{found:?}");
    assert!(!found.iter().any(|(_, detail)| detail.contains("fresh.tmp")));

    // Doctor only reports: nothing is removed or rewritten.
    assert!(orphan.exists() && fresh.exists());
    assert_eq!(fs::read(custody_file(&repo, "first")).unwrap(), b"FIRST");
    assert_eq!(fs::read(custody_file(&repo, PAGE)).unwrap(), held_before);
    // The damaged file is a finding that does not fail the command.
    repo.arc(&repo.root).args(["doctor"]).assert().success();
}

/// A bundle in the object roles an earlier build wrote: forge fields listed as
/// `cited-commit` and `integration-commit`, and no custody binding.
#[test]
fn forge_bundle_in_the_earlier_object_roles_still_imports() {
    let (source, id) = setup();
    let wt = source.home.join(".worktrees/repo-frontier");
    source.commit(&wt, "observed.txt", "one\n", "feat: observed");
    let head = source.head(&wt);
    let first = observed_at(&head, document("read-1", "g1", "first", None));
    let receipt = observe_in(&source, &id, &first, None);
    stdout(source.arc(&source.root).args([
        "forge",
        "link",
        &id,
        "--observation",
        receipt["observation_id"].as_str().unwrap(),
    ]));
    stdout(source.arc(&source.root).args([
        "forge",
        "pr-state",
        &id,
        "--state",
        "merged",
        "--merge-sha",
        &head,
    ]));
    let bundle_path = source.home.join("forge.json");
    stdout(source.arc(&source.root).args([
        "export",
        &id,
        "--output",
        bundle_path.to_str().unwrap(),
    ]));
    let mut bundle: Value = serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
    let listed = bundle["objects"].as_array().unwrap().clone();
    assert!(listed.contains(&json!({"oid": head, "role": "observed-commit"})));

    // What the earlier build listed for the same facts: the link head and the
    // lifecycle head as cited-commit, the merge commit as integration-commit.
    bundle.as_object_mut().unwrap().remove("custody");
    let mut earlier_objects: Vec<Value> = listed
        .into_iter()
        .filter(|object| object["role"] != "observed-commit")
        .collect();
    earlier_objects.push(json!({"oid": head, "role": "cited-commit"}));
    earlier_objects.push(json!({"oid": head, "role": "integration-commit"}));
    bundle["objects"] = json!(earlier_objects);
    let earlier = source.home.join("earlier.json");
    fs::write(&earlier, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();

    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    stdout(&mut admitted_change_command(&replica, &earlier));
    assert_eq!(
        state(&replica, &id)["forge"]["observations"],
        state(&source, &id)["forge"]["observations"]
    );
    assert_eq!(
        state(&replica, &id)["forge"]["link"]["observed_head_sha"],
        head
    );

    // An earlier role still obliges the receiver: the object must be here.
    let bare = Repo::new();
    admitted_change_command(&bare, &earlier)
        .assert()
        .failure()
        .stderr(predicates::str::contains("missing bundled Git objects"));
}
