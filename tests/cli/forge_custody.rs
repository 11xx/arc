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
