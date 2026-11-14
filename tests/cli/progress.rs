use super::common::*;

/// Start a series in `cwd` with `items`, returning its filename.
fn start(repo: &Repo, cwd: &Path, topic: &str, items: &[&str]) -> String {
    let mut command = repo.arc(cwd);
    command.args(["progress", "start", topic, "--title", topic]);
    for item in items {
        command.args(["--item", item]);
    }
    stdout(&mut command).lines().next().unwrap().to_string()
}

fn progress(repo: &Repo, cwd: &Path, args: &[&str]) -> String {
    stdout(repo.arc(cwd).arg("progress").args(args))
}

fn report(repo: &Repo, cwd: &Path, args: &[&str]) -> serde_json::Value {
    let value = json_stdout(repo.arc(cwd).arg("progress").args(args).arg("--json"));
    assert_eq!(value["schema"], "arc-progress/1");
    value
}

/// The one series a report holds under `file`.
fn series<'a>(report: &'a serde_json::Value, file: &str) -> &'a serde_json::Value {
    report["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|series| series["file"] == file)
        .unwrap_or_else(|| panic!("no series {file} in {report}"))
}

fn stage(repo: &Repo, file: &str, item: &str) -> serde_json::Value {
    let report = report(repo, &repo.root, &[file]);
    series(&report, file)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == item)
        .unwrap()
        .clone()
}

fn journal_dir(repo: &Repo, cwd: &Path) -> PathBuf {
    PathBuf::from(stdout(repo.arc(cwd).args(["journal", "dir"])).trim())
}

fn item_events(dir: &Path) -> usize {
    journal_event_log(dir)
        .iter()
        .filter(|event| {
            event["event"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("item-") || kind == "series-related")
        })
        .count()
}

fn sibling_project(repo: &Repo, name: &str) -> PathBuf {
    let root = repo.root.parent().unwrap().join(name);
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-b", "master"]);
    git(&root, &["config", "user.name", "Tester"]);
    git(&root, &["config", "user.email", "tester@example.invalid"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("README.md"), "other\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "init"]);
    fs::canonicalize(root).unwrap()
}

/// Claim: a linked item's stage is what its change's events record, read
/// afresh at every report with no progress write after the link: `opened`,
/// `snapshotted` once a patchset is recorded, `verified` once a pass is
/// recorded at it, `reviewed` with the verdict and the rounds that asked for
/// changes, back to `snapshotted` on a new patchset, and `integrated` with
/// its time and revision. A failure recorded after a pass at the same
/// patchset takes the stage back to `snapshotted`. Rejects a stage written
/// rather than recorded, one that stops following the change, and one that
/// keeps an earlier pass after a later recorded failure.
#[test]
fn a_linked_items_stage_follows_its_change_without_a_progress_write() {
    let repo = Repo::new();
    let flag = repo.home.join("gate-fails");
    repo.redeclare_gates(&format!(
        "[gates.unit]\ncommand = \"test ! -e {}\"\n",
        flag.display()
    ));
    let file = start(
        &repo,
        &repo.root,
        "ship",
        &["Wire the reader", "Fails after passing"],
    );
    let begun = stdout(
        repo.arc(&repo.root)
            .args(["begin", "reader", "--item", "ship:i1"]),
    );
    let change = opened_change_id(&begun);
    let dir = journal_dir(&repo, &repo.root);
    let writes = item_events(&dir);
    assert_eq!(stage(&repo, &file, "i1")["change"], change.as_str());
    assert_eq!(stage(&repo, &file, "i1")["stage"], "opened");

    let worktree = repo.home.join(".worktrees").join("repo-reader");
    repo.commit(&worktree, "reader.txt", "one\n", "feat: reader");
    assert_eq!(
        stage(&repo, &file, "i1")["stage"],
        "opened",
        "a commit is not a ledger record"
    );

    stdout(repo.arc(&worktree).args(["snapshot", "reader"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "snapshotted");

    stdout(repo.arc(&worktree).args(["verify", "reader", "--all"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "verified");

    stdout(repo.arc(&worktree).args([
        "review",
        "reader",
        "--verdict",
        "changes-requested",
        "--cause",
        "executor",
    ]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "reviewed");
    assert_eq!(row["verdict"], "changes-requested");
    assert_eq!(row["changes_requested_rounds"], 1);

    repo.commit(&worktree, "reader.txt", "two\n", "fix: reader");
    stdout(repo.arc(&worktree).args(["snapshot", "reader"]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "snapshotted");
    assert_eq!(row["changes_requested_rounds"], 1);

    stdout(repo.arc(&worktree).args(["verify", "reader", "--all"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "verified");
    stdout(
        repo.arc(&worktree)
            .args(["review", "reader", "--verdict", "approved"]),
    );
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "reviewed");
    assert_eq!(row["verdict"], "approved");

    stdout(repo.arc(&repo.root).args(["integrate", "reader"]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "integrated");
    assert!(row["integrated_at"].is_string(), "{row}");
    assert_eq!(row["revision"], repo.head(&repo.root).as_str());
    let report = report(&repo, &repo.root, &[]);
    let counts = &series(&report, &file)["counts"];
    assert_eq!(counts["integrated"], 1);
    assert_eq!(counts["complete"], 1);
    assert_eq!(counts["percent"], 50);
    assert_eq!(series(&report, &file)["last_integration"]["item"], "i1");

    stdout(
        repo.arc(&repo.root)
            .args(["begin", "flaky", "--item", "ship:i2"]),
    );
    let worktree = repo.home.join(".worktrees").join("repo-flaky");
    repo.commit(&worktree, "flaky.txt", "one\n", "feat: flaky");
    stdout(repo.arc(&worktree).args(["snapshot", "flaky"]));
    stdout(repo.arc(&worktree).args(["verify", "flaky", "--all"]));
    assert_eq!(stage(&repo, &file, "i2")["stage"], "verified");
    fs::write(&flag, "").unwrap();
    stdout_any_status(
        repo.arc(&worktree)
            .args(["verify", "flaky", "--all", "--rerun"]),
    );
    assert_eq!(
        stage(&repo, &file, "i2")["stage"],
        "snapshotted",
        "a pass outlived the failure recorded after it"
    );

    assert_eq!(
        item_events(&dir),
        writes + 1,
        "the stage moved through a progress write rather than the ledger"
    );
}

/// Claim: add, rename, drop, split and move each append one event and leave
/// every earlier event intact, and the report shows the list as it stands,
/// in its order, counted from the items that stand and from what changed
/// since the start. Rejects an edit that rewrites earlier events or a count
/// taken from the list as first declared.
#[test]
fn list_edits_keep_earlier_events_and_the_report_counts_the_list_as_it_stands() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "edits", &["One", "Two", "Three"]);
    let dir = journal_dir(&repo, &repo.root);
    let edits: [&[&str]; 6] = [
        &["add", "edits", "Zero", "--before", "i1"],
        &["rename", "edits", "i2", "Two, renamed"],
        &["drop", "edits", "i3", "--reason", "folded into i2"],
        &[
            "split",
            "edits",
            "i1",
            "--into",
            "One a",
            "--into",
            "One b",
            "--reason",
            "two parts",
        ],
        &["move", "edits", "i4", "--after", "i2"],
        &["done", "edits", "i5", "--note", "measured"],
    ];
    let mut log = journal_event_log(&dir);
    for edit in edits {
        progress(&repo, &repo.root, edit);
        let next = journal_event_log(&dir);
        assert_eq!(
            next.len(),
            log.len() + 1,
            "{edit:?} did not append one event"
        );
        assert_eq!(
            next[..log.len()],
            log[..],
            "{edit:?} rewrote an earlier event"
        );
        log = next;
    }

    let report = report(&repo, &repo.root, &[]);
    let series = series(&report, &file);
    let listed: Vec<(String, String, String)> = series["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["id"].as_str().unwrap().to_string(),
                row["title"].as_str().unwrap().to_string(),
                row["stage"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected = [
        ("i1", "One", "split"),
        ("i5", "One a", "done"),
        ("i6", "One b", "open"),
        ("i2", "Two, renamed", "open"),
        ("i4", "Zero", "open"),
        ("i3", "Three", "dropped"),
    ];
    assert_eq!(
        listed,
        expected
            .iter()
            .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
            .collect::<Vec<_>>()
    );
    let counts = &series["counts"];
    assert_eq!(counts["items"], 4, "{counts}");
    assert_eq!(counts["complete"], 1);
    assert_eq!(counts["open"], 3);
    assert_eq!(counts["percent"], 25);
    assert_eq!(counts["since_start"]["declared"], 3);
    assert_eq!(counts["since_start"]["added"], 1);
    assert_eq!(counts["since_start"]["dropped"], 1);
    assert_eq!(counts["since_start"]["split"], 1);

    let text = progress(&repo, &repo.root, &["--plain"]);
    assert!(
        text.contains("4 items; 3 declared at the start; since then 1 added, 1 dropped, 1 split"),
        "{text}"
    );
}

/// Claim: a dropped item leaves the share complete and stays visible with
/// its reason. Rejects a dropped item counted against the share, and one
/// that disappears from the list.
#[test]
fn a_dropped_item_leaves_the_share_and_stays_visible() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "share", &["Keep", "Lose"]);
    progress(&repo, &repo.root, &["done", "share", "i1"]);
    progress(
        &repo,
        &repo.root,
        &["drop", "share", "i2", "--reason", "out of scope"],
    );
    let report = report(&repo, &repo.root, &[]);
    let series = series(&report, &file);
    assert_eq!(series["counts"]["items"], 1);
    assert_eq!(series["counts"]["complete"], 1);
    assert_eq!(series["counts"]["percent"], 100);
    assert_eq!(series["items"][1]["stage"], "dropped");
    assert_eq!(series["items"][1]["note"], "out of scope");
    let text = progress(&repo, &repo.root, &["--plain"]);
    assert!(text.contains("1 of 1 complete"), "{text}");
    assert!(text.contains("dropped  Lose  [out of scope]"), "{text}");
}

/// Every RFC 3339 timestamp in `text` replaced by `<ts>`: when the session
/// last wrote is a clock fact every journal write moves, not a line of
/// progress.
fn without_timestamps(text: &str) -> String {
    let bytes = text.as_bytes();
    let stamp = |at: usize| {
        bytes.len() >= at + 19
            && bytes[at..at + 19]
                .iter()
                .enumerate()
                .all(|(index, byte)| match index {
                    4 | 7 => *byte == b'-',
                    10 => *byte == b'T',
                    13 | 16 => *byte == b':',
                    _ => byte.is_ascii_digit(),
                })
    };
    let mut out = String::new();
    let mut at = 0;
    while at < bytes.len() {
        if stamp(at) {
            out.push_str("<ts>");
            at += 19;
            while at < bytes.len() && !matches!(bytes[at], b' ' | b'"' | b',' | b'\n') {
                at += 1;
            }
        } else {
            let next = text[at..].chars().next().unwrap();
            out.push(next);
            at += next.len_utf8();
        }
    }
    out
}

/// Claim: progress is pulled, never pushed: `arc status`, `arc check` and
/// `arc catchup` print byte for byte the same, timestamps aside, before and
/// after a series' items are added, linked to the change those commands
/// read, marked done and dropped. The series is started before the first
/// read, since filing any artifact is session activity catchup reports for
/// every kind. Rejects any of them carrying a line of progress.
#[test]
fn progress_prints_nothing_into_status_check_or_catchup() {
    let repo = Repo::new();
    let change = begin_change(&repo, "quiet", None);
    start(&repo, &repo.root, "quiet-series", &[]);
    let reads = |repo: &Repo| -> Vec<String> {
        [
            vec!["status", change.as_str()],
            vec!["status", change.as_str(), "--json"],
            vec!["check", change.as_str()],
            vec!["catchup"],
            vec!["catchup", "--json"],
        ]
        .iter()
        .map(|args| {
            without_timestamps(&without_clock(&stdout_any_status(
                repo.arc(&repo.root).args(args),
            )))
        })
        .collect()
    };
    let before = reads(&repo);

    for title in ["Linked", "Free", "Gone"] {
        progress(&repo, &repo.root, &["add", "quiet-series", title]);
    }
    progress(&repo, &repo.root, &["link", "quiet-series", "i1", &change]);
    progress(&repo, &repo.root, &["done", "quiet-series", "i2"]);
    progress(
        &repo,
        &repo.root,
        &["drop", "quiet-series", "i3", "--reason", "not needed"],
    );
    let after = reads(&repo);
    for (before, after) in before.iter().zip(&after) {
        assert_eq!(
            before, after,
            "a read carried progress it was not asked for"
        );
    }
}

/// Claim: with several active series in one project, bare `arc progress`
/// prints every one in full under its own header line, and each series'
/// counts are its own. Rejects a summary line in place of a series, a
/// series left out, and counts that mix series.
#[test]
fn two_active_series_print_in_full_with_independent_counts() {
    let repo = Repo::new();
    let first = start(&repo, &repo.root, "first", &["A", "B"]);
    let second = start(&repo, &repo.root, "second", &["C", "D", "E"]);
    progress(&repo, &repo.root, &["done", "first", "i1"]);
    progress(&repo, &repo.root, &["done", "second", "i3"]);
    progress(&repo, &repo.root, &["done", "second", "i2"]);

    let report = report(&repo, &repo.root, &[]);
    assert_eq!(report["series"].as_array().unwrap().len(), 2);
    let one = series(&report, &first);
    let two = series(&report, &second);
    assert_eq!(
        (
            one["counts"]["complete"].clone(),
            one["counts"]["items"].clone()
        ),
        (1.into(), 2.into())
    );
    assert_eq!(
        (
            two["counts"]["complete"].clone(),
            two["counts"]["items"].clone()
        ),
        (2.into(), 3.into())
    );

    let text = progress(&repo, &repo.root, &["--plain"]);
    for header in [
        format!("== first ({first})"),
        format!("== second ({second})"),
    ] {
        assert!(text.contains(&header), "{text}");
    }
    for title in ["A", "B", "C", "D", "E"] {
        assert!(
            text.lines()
                .any(|line| line.trim_end().ends_with(&format!(" {title}"))),
            "item {title} missing:\n{text}"
        );
    }
    assert!(text.contains("1 of 2 complete"), "{text}");
    assert!(text.contains("2 of 3 complete"), "{text}");
}

/// Claim: a relation made in one project is recorded only there and shows
/// as one-sided, and becomes mutual once the same call is made in the other
/// project. Rejects a relation reported mutual on one call, or one-sided
/// after both.
#[test]
fn a_relation_is_one_sided_until_made_in_the_other_project() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    let here = start(&repo, &repo.root, "here", &["Local"]);
    let there = start(&repo, &other, "there", &["Remote"]);
    let label = repo.root.file_name().unwrap().to_string_lossy().to_string();

    let related = progress(&repo, &repo.root, &["relate", "here", "other::there"]);
    assert!(related.contains("one-sided"), "{related}");
    let report_here = report(&repo, &repo.root, &[]);
    let relation = &series(&report_here, &here)["related"][0];
    assert_eq!(relation["file"], there.as_str());
    assert_eq!(relation["project"], "other");
    assert_eq!(relation["mutual"], false);
    assert!(progress(&repo, &repo.root, &["--plain"]).contains("(one-sided)"));
    assert_eq!(
        series(&report(&repo, &other, &[]), &there)["related"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "the relation was written into the other project's journal"
    );

    let back = progress(
        &repo,
        &other,
        &["relate", "there", &format!("{label}::here")],
    );
    assert!(back.contains("(mutual)"), "{back}");
    let mutual = report(&repo, &repo.root, &[]);
    assert_eq!(series(&mutual, &here)["related"][0]["mutual"], true);
    let text = progress(&repo, &repo.root, &["--plain"]);
    let header = text
        .lines()
        .position(|line| line.starts_with("== here"))
        .unwrap();
    assert_eq!(
        text.lines().nth(header + 1).unwrap(),
        format!("related: other::{there} (mutual)")
    );
}

/// Claim: bare `arc progress` prints each related series of another project
/// after the local ones as a compact block marked with its project, read
/// from that project's journal and ledger; when that journal cannot be read
/// it prints one line and exits 0. Rejects a related series left out, one
/// printed without its project, and a failure that fails the report.
#[test]
fn a_related_series_prints_compact_and_an_unreadable_one_prints_one_line() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    start(&repo, &repo.root, "here", &["Local"]);
    let there = start(
        &repo,
        &other,
        "there",
        &["Remote one", "Remote two", "Remote three"],
    );
    progress(&repo, &other, &["done", "there", "i1"]);
    let change = begin_change_in(&repo, &other, "remote-work");
    progress(&repo, &other, &["link", "there", "i2", &change]);
    progress(&repo, &repo.root, &["relate", "here", "other::there"]);

    let text = progress(&repo, &repo.root, &["--plain"]);
    let block = text
        .find(&format!("-- other: there ({there})"))
        .unwrap_or_else(|| panic!("{text}"));
    let local = text.find("== here").unwrap();
    assert!(local < block, "{text}");
    let compact = &text[block..];
    assert!(compact.contains("1 of 3 complete"), "{compact}");
    assert!(compact.contains("1 in flight"), "{compact}");
    assert!(compact.contains("i2   opened  Remote two"), "{compact}");
    assert!(!compact.contains("Remote three"), "{compact}");
    let json = report(&repo, &repo.root, &[]);
    assert_eq!(json["related"][0]["project"], "other");
    assert_eq!(json["related"][0]["in_flight"][0]["id"], "i2");

    let other_journal = journal_dir(&repo, &other);
    fs::remove_dir_all(&other_journal).unwrap();
    let output = repo
        .arc(&repo.root)
        .args(["progress", "--plain"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    let unavailable: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("unavailable"))
        .collect();
    assert_eq!(unavailable.len(), 1, "{text}");
    assert!(
        unavailable[0].starts_with("related ") && unavailable[0].contains(&format!("::{there}")),
        "{text}"
    );
}

fn begin_change_in(repo: &Repo, cwd: &Path, slug: &str) -> String {
    let branch = format!("arc/{slug}");
    git(cwd, &["branch", &branch, "HEAD"]);
    opened_change_id(&stdout(
        repo.arc(cwd).args(["begin", slug, "--adopt", &branch]),
    ))
}

/// Claim: retiring a series by its conclusion records it consumed as
/// superseded with the conclusion named as what resolved it, which the
/// journal's library shows as its successor; removes it from the default
/// listing, keeps it readable by name, and refuses further edits. Rejects a
/// retired series still listed, one unreadable by name, and a retirement
/// whose successor the journal's own readers do not show.
#[test]
fn retiring_a_series_by_its_conclusion_leaves_the_listing_and_records_the_successor() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "finish", &["Only"]);
    let kept = start(&repo, &repo.root, "ongoing", &["Still"]);
    let (_, conclusion) = journal_artifact(&repo, "finish", "conclusion", "# Finished\n");
    let retired = progress(
        &repo,
        &repo.root,
        &["retire", "finish", "--by", &conclusion],
    );
    assert!(retired.contains(&conclusion), "{retired}");

    let dir = journal_dir(&repo, &repo.root);
    let consumed = journal_event_log(&dir)
        .into_iter()
        .rev()
        .find(|event| event["event"] == "consumed")
        .unwrap();
    assert_eq!(consumed["file"], file.as_str());
    assert_eq!(consumed["outcome"], "superseded");
    assert_eq!(consumed["decision"], conclusion.as_str());
    assert_eq!(consumed["decision_kind"], "conclusion");

    let listing = report(&repo, &repo.root, &[]);
    let files: Vec<&str> = listing["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|series| series["file"].as_str().unwrap())
        .collect();
    assert_eq!(files, vec![kept.as_str()]);
    let named = report(&repo, &repo.root, &[&file]);
    assert_eq!(series(&named, &file)["retired"]["by"], conclusion.as_str());
    repo.arc(&repo.root)
        .args(["progress", "add", &file, "Late"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("retired"));

    let library =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "report", "--library", "--json"]),
        );
    let row = library["projects"][0]["library"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["file"] == file)
        .unwrap()
        .clone();
    assert_eq!(row["status"], "superseded", "{row}");
    assert_eq!(row["superseded_by"], conclusion.as_str(), "{row}");
}

/// Claim: `arc progress --json` carries the `arc-progress/1` shape the guide
/// documents, for several series with relations and every item kind: each
/// field present with its type. Rejects a report missing a documented field
/// or carrying one of the wrong type.
#[test]
fn progress_json_has_the_arc_progress_1_shape() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    start(&repo, &other, "there", &["Remote"]);
    start(
        &repo,
        &repo.root,
        "one",
        &["Open", "Done", "Dropped", "Split"],
    );
    start(&repo, &repo.root, "two", &[]);
    let change = begin_change(&repo, "shape", None);
    progress(&repo, &repo.root, &["link", "one", "i1", &change]);
    progress(&repo, &repo.root, &["done", "one", "i2"]);
    progress(&repo, &repo.root, &["drop", "one", "i3", "--reason", "r"]);
    progress(
        &repo,
        &repo.root,
        &[
            "split", "one", "i4", "--into", "a", "--into", "b", "--reason", "r",
        ],
    );
    progress(&repo, &repo.root, &["relate", "one", "other::there"]);

    let report = report(&repo, &repo.root, &[]);
    let is = |value: &serde_json::Value, what: &str| match what {
        "string" => value.is_string(),
        "number" => value.is_u64(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "bool" => value.is_boolean(),
        _ => unreachable!(),
    };
    let fields = |value: &serde_json::Value, expected: &[(&str, &str)]| {
        for (name, kind) in expected {
            assert!(is(&value[name], kind), "{name} is not a {kind}: {value}");
        }
    };
    fields(
        &report,
        &[
            ("schema", "string"),
            ("journal_dir", "string"),
            ("series", "array"),
            ("related", "array"),
            ("waiting_on_person", "array"),
        ],
    );
    assert_eq!(report["series"].as_array().unwrap().len(), 2);
    for series in report["series"].as_array().unwrap() {
        fields(
            series,
            &[
                ("file", "string"),
                ("title", "string"),
                ("started_at", "string"),
                ("related", "array"),
                ("items", "array"),
                ("counts", "object"),
            ],
        );
        fields(
            &series["counts"],
            &[
                ("items", "number"),
                ("complete", "number"),
                ("integrated", "number"),
                ("done", "number"),
                ("in_flight", "number"),
                ("open", "number"),
                ("closed", "number"),
                ("since_start", "object"),
            ],
        );
        for row in series["items"].as_array().unwrap() {
            fields(
                row,
                &[("id", "string"), ("title", "string"), ("stage", "string")],
            );
        }
    }
    let one = &report["series"][0];
    fields(
        &one["related"][0],
        &[
            ("project", "string"),
            ("slug", "string"),
            ("file", "string"),
            ("mutual", "bool"),
        ],
    );
    fields(&one["items"][0], &[("change", "string")]);
    fields(&one["items"][2], &[("note", "string")]);
    fields(&one["items"][3], &[("split_into", "array")]);
    fields(&one["counts"], &[("percent", "number")]);
    fields(
        &report["related"][0],
        &[
            ("project", "string"),
            ("slug", "string"),
            ("file", "string"),
            ("related_from", "array"),
            ("title", "string"),
            ("counts", "object"),
            ("in_flight", "array"),
        ],
    );
}

/// The `arc-perf/1` record `ARC_TRACE` writes for one `arc progress`.
fn traced_progress(repo: &Repo) -> serde_json::Value {
    let output = repo
        .arc(&repo.root)
        .env("ARC_TRACE", "1")
        .arg("progress")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
        .lines()
        .find(|line| line.contains("\"arc-perf/1\""))
        .map(|line| serde_json::from_str(line).unwrap())
        .unwrap_or_else(|| panic!("no trace record: {stderr}"))
}

/// What one report read: change ledgers opened and decoded, changes
/// reduced, and Git processes started.
fn reads(record: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "change_files_opened": record["ledger"]["change"]["opened"],
        "change_events_decoded": record["ledger"]["change"]["decoded"],
        "changes_reduced": record["counts"]["changes_reduced"],
        "git": record["children"]["runs"],
    })
}

/// Claim: a report reads the ledger only for its linked items: a second
/// change that no item links, however many events it holds, changes neither
/// the ledger files the report opens and decodes, the changes it reduces,
/// nor the Git processes it starts. Rejects a report whose cost follows the
/// size of the ledger rather than its own items.
#[test]
fn a_report_reads_only_the_changes_its_items_link() {
    let repo = Repo::new();
    start(&repo, &repo.root, "bounded", &["Linked", "Free"]);
    let linked = begin_change(&repo, "linked", None);
    progress(&repo, &repo.root, &["link", "bounded", "i1", &linked]);
    let before = reads(&traced_progress(&repo));

    let unrelated = begin_change(&repo, "unrelated", None);
    for index in 0..12 {
        stdout(repo.arc(&repo.root).args([
            "keep",
            &unrelated,
            "--kind",
            "constraint",
            "--body",
            &format!("fact {index}"),
        ]));
    }
    let after = reads(&traced_progress(&repo));
    assert_eq!(
        before, after,
        "an unlinked change changed what the report read"
    );
}

/// Claim: `retire --by` and `start --follows` take only a bare artifact
/// filename of this journal that is a regular file: a relative path climbing
/// out, an absolute path, a filename-shaped symbolic link to a file outside
/// the journal, and a qualified `<directory>::<file>` reference through a
/// symbolically linked directory are each refused and record nothing; the
/// qualified one is refused by its spelling, before anything under the named
/// directory is read. Rejects a name that leads the journal to read a file
/// outside its directories, a decoy binding among them.
#[test]
fn artifact_names_on_the_command_line_never_leave_the_journal() {
    let repo = Repo::new();
    start(&repo, &repo.root, "contained", &["Only"]);
    let dir = journal_dir(&repo, &repo.root);
    let outside = repo.home.join("20261114T000000Z-outside-conclusion.md");
    fs::write(&outside, "# Outside\n").unwrap();
    let plan = repo.home.join("20261114T000000Z-outside-plan.md");
    fs::write(&plan, "# Outside plan\n").unwrap();
    let link = "20261114T000001Z-linked-conclusion.md";
    std::os::unix::fs::symlink(&outside, dir.join(link)).unwrap();
    let relative = pathdiff(&dir, &outside);
    let events = journal_event_log(&dir).len();
    for name in [relative.as_str(), outside.to_str().unwrap(), link] {
        repo.arc(&repo.root)
            .args(["progress", "retire", "contained", "--by", name])
            .assert()
            .failure();
    }
    let decoy = repo.home.join("decoy");
    fs::create_dir_all(&decoy).unwrap();
    fs::write(
        decoy.join("bindings.jsonl"),
        "{\"schema\":\"journal-binding/1\",\"ts\":\"2026-11-14T00:00:00Z\",\"event\":\"bound\",\"anchor\":\"DECOY-MARKER\"}\n",
    )
    .unwrap();
    fs::write(decoy.join("20261114T000000Z-x-conclusion.md"), "# X\n").unwrap();
    fs::write(decoy.join("20261114T000000Z-x-plan.md"), "# X\n").unwrap();
    let through = repo.home.join("through");
    std::os::unix::fs::symlink(&decoy, &through).unwrap();
    let qualified = |file: &str| format!("{}::{file}", through.display());
    for args in [
        vec![
            "retire".to_string(),
            "contained".to_string(),
            "--by".to_string(),
            qualified("20261114T000000Z-x-conclusion.md"),
        ],
        vec![
            "start".to_string(),
            "decoyed".to_string(),
            "--title".to_string(),
            "Decoyed".to_string(),
            "--follows".to_string(),
            qualified("20261114T000000Z-x-plan.md"),
        ],
    ] {
        let output = repo
            .arc(&repo.root)
            .arg("progress")
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("DECOY-MARKER"),
            "a qualified name was resolved outside the journal: {stderr}"
        );
        assert!(stderr.contains("bare"), "{stderr}");
    }
    let relative_plan = pathdiff(&dir, &plan);
    for name in [relative_plan.as_str(), plan.to_str().unwrap()] {
        repo.arc(&repo.root)
            .args([
                "progress",
                "start",
                "other",
                "--title",
                "Other",
                "--follows",
                name,
            ])
            .assert()
            .failure();
    }
    assert_eq!(
        journal_event_log(&dir).len(),
        events,
        "a refused name recorded something"
    );
    assert_eq!(
        report(&repo, &repo.root, &[])["series"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// `to` spelled relative to `from`, climbing with `..` to their common root.
fn pathdiff(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().to_string()),
    );
    parts.join("/")
}

/// Claim: `begin --item` checks the item before anything is written: a
/// series or item that does not exist is refused in a repository with no
/// ledger, and leaves no ledger, branch or change behind. Rejects a preflight
/// that creates the ledger before refusing.
#[test]
fn begin_with_an_unknown_item_leaves_nothing_behind() {
    let repo = Repo::new();
    start(&repo, &repo.root, "fresh", &["Only"]);
    let ledger = repo.root.join(".git/arc");
    assert!(!ledger.exists(), "the fixture starts without a ledger");
    for spec in ["nosuch:i1", "fresh:i9"] {
        repo.arc(&repo.root)
            .args(["begin", "never", "--item", spec])
            .assert()
            .failure();
        assert!(!ledger.exists(), "{spec} left a ledger behind");
    }
    assert!(git_out(&repo.root, &["branch", "--list", "arc/never"])
        .trim()
        .is_empty());
}

/// Claim: `reopen` undoes `drop` for an item that keeps its link, and the
/// reopened item reads its stage from the change again and counts in flight.
/// Rejects a reopen limited to unlinked items, or one that leaves a linked
/// item reading as open.
#[test]
fn reopening_a_dropped_linked_item_returns_it_to_its_derived_stage() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "back", &["Linked"]);
    let change = begin_change(&repo, "back-work", None);
    progress(&repo, &repo.root, &["link", "back", "i1", &change]);
    progress(
        &repo,
        &repo.root,
        &["drop", "back", "i1", "--reason", "parked"],
    );
    assert_eq!(stage(&repo, &file, "i1")["stage"], "dropped");
    progress(
        &repo,
        &repo.root,
        &["reopen", "back", "i1", "--note", "resumed"],
    );
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "opened");
    assert_eq!(row["change"], change.as_str());
    let report = report(&repo, &repo.root, &[]);
    assert_eq!(series(&report, &file)["counts"]["in_flight"], 1);
}

/// Claim: a link is written only while the item's link is still the one the
/// command read, so a link another writer made between that read and the
/// write is refused and kept, never silently replaced. Rejects a link decided
/// on a stale reading.
#[cfg(debug_assertions)]
#[test]
fn a_link_made_meanwhile_is_not_replaced() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "race", &["Contested"]);
    let first = begin_change(&repo, "first", None);
    let second = begin_change(&repo, "second", None);
    let dir = journal_dir(&repo, &repo.root);
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".locks/transition.lock"))
        .unwrap();
    held.lock().unwrap();
    let mut child = spawn_arc_paused_on_lock(
        &repo,
        &repo.root,
        &["progress", "link", "race", "i1", &second],
        "transition",
        &[],
    );
    let competing = serde_json::json!({
        "schema": "journal-events/2",
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "topic": "race",
        "event": "item-linked",
        "file": file,
        "item": "i1",
        "change": first,
    });
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    use std::io::Write;
    writeln!(log, "{competing}").unwrap();
    drop(held);
    release_lock_wait(&mut child);
    let status = wait_for_exit(&mut child);
    let stderr = child_stderr(&mut child);
    assert!(
        !status.success(),
        "the stale link replaced the newer one: {stderr}"
    );
    assert!(stderr.contains("since this command read it"), "{stderr}");
    assert_eq!(stage(&repo, &file, "i1")["change"], first.as_str());
}

/// Claim: a series' title as corrected through the journal is the title the
/// report shows, retired or not, as the journal's other views show it.
/// Rejects a report that keeps the title as first written.
#[test]
fn a_corrected_series_title_is_the_one_reported() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "titled", &["Only"]);
    let (_, conclusion) = journal_artifact(&repo, "titled", "conclusion", "# Done\n");
    progress(
        &repo,
        &repo.root,
        &["retire", "titled", "--by", &conclusion],
    );
    stdout(repo.arc(&repo.root).args([
        "journal",
        "correct",
        &file,
        "--target",
        "artifact",
        "--field",
        "title",
        "--value",
        "Titled, corrected",
    ]));
    let report = report(&repo, &repo.root, &[&file]);
    assert_eq!(series(&report, &file)["title"], "Titled, corrected");
    assert!(progress(&repo, &repo.root, &[&file, "--plain"])
        .starts_with(&format!("== Titled, corrected ({file})")));
}

/// Claim: `unrelate` removes a relation this journal records, appending one
/// event, so the series no longer names or prints the related series.
/// Rejects a relation that stays after it was removed.
#[test]
fn unrelate_removes_the_relation_here() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    let here = start(&repo, &repo.root, "here", &["Local"]);
    let there = start(&repo, &other, "there", &["Remote"]);
    progress(&repo, &repo.root, &["relate", "here", "other::there"]);
    let dir = journal_dir(&repo, &repo.root);
    let events = journal_event_log(&dir).len();
    let removed = progress(&repo, &repo.root, &["unrelate", "here", "other::there"]);
    assert!(removed.contains(&there), "{removed}");
    assert_eq!(journal_event_log(&dir).len(), events + 1);
    let report = report(&repo, &repo.root, &[]);
    assert!(series(&report, &here)["related"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(report["related"].as_array().unwrap().is_empty());
    repo.arc(&repo.root)
        .args(["progress", "unrelate", "here", "other::there"])
        .assert()
        .failure();
}

/// Claim: `arc workspace progress` reports a project whose ledger cannot be
/// opened as a failure and exits 16, naming the project. Rejects a ledger
/// error read as an `unknown` stage, which would report a broken ledger as
/// a series in good order.
#[test]
fn workspace_progress_fails_on_a_ledger_it_cannot_open() {
    let repo = Repo::new();
    start(&repo, &repo.root, "broken", &["Linked"]);
    let change = begin_change(&repo, "broken-work", None);
    progress(&repo, &repo.root, &["link", "broken", "i1", &change]);
    let config = repo.root.join(".git/arc/config.json");
    assert!(config.is_file(), "{}", config.display());
    fs::write(&config, "{ not json").unwrap();

    let output = repo
        .arc(&repo.root)
        .args(["workspace", "progress", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(16), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let failures = value["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 1, "{value}");
    assert!(
        failures[0]["reason"]
            .as_str()
            .unwrap()
            .contains("cannot read the ledger"),
        "{value}"
    );
    assert!(value["projects"].as_array().unwrap().is_empty(), "{value}");
}

/// Claim: a related series whose project's ledger cannot be opened prints
/// one `unavailable` line naming the ledger, and the report still exits 0
/// with the local series in full. Rejects a ledger error that fails the
/// local report, and one shown as a related block of `unknown` stages.
#[test]
fn a_related_series_with_an_unreadable_ledger_is_unavailable() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    start(&repo, &repo.root, "here", &["Local"]);
    let there = start(&repo, &other, "there", &["Remote"]);
    let change = begin_change_in(&repo, &other, "remote-work");
    progress(&repo, &other, &["link", "there", "i1", &change]);
    progress(&repo, &repo.root, &["relate", "here", "other::there"]);
    let config = other.join(".git/arc/config.json");
    assert!(config.is_file(), "{}", config.display());
    fs::write(&config, "{ not json").unwrap();

    let output = repo
        .arc(&repo.root)
        .args(["progress", "--plain"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("== here"), "{text}");
    let unavailable: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("unavailable"))
        .collect();
    assert_eq!(unavailable.len(), 1, "{text}");
    assert!(
        unavailable[0].contains(&format!("::{there}")) && unavailable[0].contains("ledger"),
        "{text}"
    );
    assert!(!text.contains("unknown"), "{text}");
}

/// Claim: a superseded consumption records a successor only when its
/// decision names an artifact this journal holds by a bare name: one naming
/// a missing file, or spelled as another directory's reference, records
/// none. Rejects a successor taken from whatever the event names.
#[test]
fn a_successor_is_only_an_artifact_the_journal_holds() {
    let repo = Repo::new();
    let held = start(&repo, &repo.root, "held", &["Only"]);
    let missing = start(&repo, &repo.root, "missing", &["Only"]);
    let qualified = start(&repo, &repo.root, "qualified", &["Only"]);
    let (_, conclusion) = journal_artifact(&repo, "held", "conclusion", "# Held\n");
    progress(&repo, &repo.root, &["retire", "held", "--by", &conclusion]);

    let dir = journal_dir(&repo, &repo.root);
    let retired = journal_event_log(&dir)
        .into_iter()
        .rev()
        .find(|event| event["event"] == "consumed")
        .unwrap();
    let mut lines = String::new();
    for (file, decision) in [
        (
            &missing,
            "20261114T000000Z-absent-conclusion.md".to_string(),
        ),
        (&qualified, format!("/x::{conclusion}")),
    ] {
        let mut event = retired.clone();
        event["file"] = serde_json::json!(file);
        event["decision"] = serde_json::json!(decision);
        lines.push_str(&format!("{event}\n"));
    }
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    std::io::Write::write_all(&mut log, lines.as_bytes()).unwrap();

    let library =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "report", "--library", "--json"]),
        );
    let row = |file: &str| {
        library["projects"][0]["library"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["file"] == file)
            .unwrap()
            .clone()
    };
    assert_eq!(row(&held)["superseded_by"], conclusion.as_str());
    for file in [&missing, &qualified] {
        let row = row(file);
        assert!(
            row["superseded_by"].is_null(),
            "a successor was taken from a name the journal does not hold: {row}"
        );
    }
}

/// Claim: `verified` needs a pass recorded at the newest patchset's own head.
/// Rejects a pass recorded before a rewrite reading as a run at the rewritten
/// head, which would show a patchset as verified that nothing was run at.
#[test]
fn a_pass_recorded_before_a_rewrite_does_not_verify_the_rewritten_head() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.unit]\ncommand='true'\n");
    let file = start(&repo, &repo.root, "rewritten", &["one"]);
    let begun = stdout(
        repo.arc(&repo.root)
            .args(["begin", "carried", "--item", "rewritten:i1"]),
    );
    let change = opened_change_id(&begun);
    let worktree = repo.home.join(".worktrees").join("repo-carried");
    repo.commit(&worktree, "carried.txt", "one\n", "feat: carried");
    stdout(repo.arc(&worktree).args(["snapshot", &change]));
    stdout(repo.arc(&worktree).args(["verify", &change, "--all"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "verified");

    stdout(repo.arc(&worktree).args([
        "rewrite",
        "trailers",
        "--from",
        "HEAD",
        "--append",
        "Assisted-by: fixture:model",
        "--no-sign",
    ]));
    stdout(repo.arc(&worktree).args(["snapshot", &change]));
    assert_eq!(
        stage(&repo, &file, "i1")["stage"],
        "snapshotted",
        "no run is recorded at the rewritten head"
    );
}

/// Claim: an item linked to a change this ledger does not hold reads
/// `unknown` and the report still prints. Rejects failing the whole report
/// for one absent change, and reading a link in the change's place as either
/// an absent change or the history it points at.
#[test]
fn an_item_whose_change_the_ledger_does_not_hold_reads_unknown() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "absent", &["one", "two"]);
    let begun = stdout(
        repo.arc(&repo.root)
            .args(["begin", "gone", "--item", "absent:i1"]),
    );
    let change = opened_change_id(&begun);
    assert_eq!(stage(&repo, &file, "i1")["stage"], "opened");
    let held = repo.root.join(".git/arc/changes").join(&change);
    let aside = repo.home.join("change-aside");
    fs::rename(&held, &aside).unwrap();
    assert_eq!(stage(&repo, &file, "i1")["stage"], "unknown");
    assert_eq!(stage(&repo, &file, "i2")["stage"], "open");

    // A link standing where the change's directory should be is a ledger
    // that cannot be read, not an absent change and not a way to another's
    // history.
    std::os::unix::fs::symlink(&aside, &held).unwrap();
    repo.arc(&repo.root)
        .args(["progress", &file, "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a directory of this ledger"));
}

/// Claim: without a form flag `arc progress` prints the view for a person,
/// unpainted when standard output is not a terminal, and `--plain` prints
/// the complete text with the series' filename; `arc workspace progress`
/// takes the same forms. Rejects escape bytes reaching a pipe, a default
/// that is still the complete text, and a `--plain` that lost a filename
/// or a note.
#[test]
fn the_default_is_the_unpainted_view_and_plain_keeps_every_fact() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "forms", &["Keep", "Wait"]);
    progress(
        &repo,
        &repo.root,
        &["done", "forms", "i1", "--note", "by hand"],
    );

    let view = progress(&repo, &repo.root, &[]);
    assert!(!view.contains('\u{1b}'), "{view:?}");
    let lines: Vec<&str> = view.lines().collect();
    assert_eq!(
        &lines[..7],
        [
            "forms",
            "██████████░░░░░░░░░░  1 of 2 · 50%   1 open",
            "forms",
            "",
            "  ✓ i1  Keep  done",
            "        by hand",
            "  ○ i2  Wait",
        ],
        "{view}"
    );
    assert!(!view.contains(&file), "{view}");

    let plain = progress(&repo, &repo.root, &["--plain"]);
    assert!(plain.contains(&format!("== forms ({file})")), "{plain}");
    assert!(plain.contains("done  Keep  [by hand]"), "{plain}");
    assert!(plain.is_ascii(), "{plain}");

    repo.arc(&repo.root)
        .args(["progress", "--plain", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used with"));

    let workspace = |form: &[&str]| {
        stdout(
            repo.arc(&repo.root)
                .args(["workspace", "progress", "--here"])
                .args(form),
        )
    };
    let view = workspace(&[]);
    assert!(view.contains("  ✓ i1  Keep  done"), "{view}");
    assert!(view.contains("(1 series)"), "{view}");
    assert!(!view.contains('\u{1b}'), "{view:?}");
    assert!(
        workspace(&["--plain"]).contains(&format!("== forms ({file})")),
        "{view}"
    );
}

/// Claim: `arc workspace report --json` carries a project's active series
/// only when given `--progress`, and then exactly the series
/// `arc workspace progress --json` reports for that project. Rejects
/// progress printed into a report that did not ask for it, a second reading
/// of the series that differs from the first, and a key on a project that
/// has no series.
#[test]
fn the_workspace_report_carries_series_only_when_asked() {
    let repo = Repo::new();
    let report = |flags: &[&str]| {
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "report", "--here", "--json"])
                .args(flags),
        )
    };
    assert!(report(&["--progress"])["projects"][0]
        .get("progress")
        .is_none());

    start(&repo, &repo.root, "carried", &["Linked", "By hand"]);
    let change = begin_change(&repo, "carried-work", None);
    progress(&repo, &repo.root, &["link", "carried", "i1", &change]);
    progress(&repo, &repo.root, &["done", "carried", "i2"]);

    let plain = report(&[]);
    assert_eq!(plain["schema"], "arc-workspace-report/3");
    assert!(plain["projects"][0].get("progress").is_none(), "{plain}");

    let asked = report(&["--progress"]);
    let own = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "progress", "--here", "--json"]),
    );
    assert_eq!(
        asked["projects"][0]["progress"],
        own["projects"][0]["series"]
    );
    assert_eq!(
        asked["projects"][0]["progress"][0]["items"][0]["stage"],
        "opened"
    );
    let (mut without, mut plain) = (asked.clone(), plain);
    without["projects"][0]
        .as_object_mut()
        .unwrap()
        .remove("progress");
    for report in [&mut without, &mut plain] {
        report["observation"] = serde_json::Value::Null;
    }
    assert_eq!(without, plain, "--progress changed the rest of the report");
}

/// Claim: with `--progress`, each project of the report carries its own
/// series even when two registered projects share a label. Rejects series
/// attached by label, which shows one project another's stages and leaves
/// the other without any.
#[test]
fn projects_sharing_a_label_each_carry_their_own_series() {
    let repo = Repo::new();
    let label = repo.root.file_name().unwrap().to_str().unwrap();
    let twin = sibling_project(&repo, &format!("nested/{label}"));
    start(&repo, &repo.root, "first-here", &["Here"]);
    start(&repo, &twin, "then-there", &["There"]);

    let report =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "report", "--json", "--progress"]),
        );
    let carried: std::collections::BTreeMap<String, String> = report["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|project| project["project"] == label)
        .map(|project| {
            let series = project["progress"].as_array().unwrap();
            assert_eq!(series.len(), 1, "{project}");
            (
                project["anchor"].as_str().unwrap().to_string(),
                series[0]["title"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        carried,
        std::collections::BTreeMap::from([
            (repo.root.display().to_string(), "first-here".to_string()),
            (twin.display().to_string(), "then-there".to_string()),
        ])
    );
}

/// Claim: `arc progress start` ends with one line naming how a change joins
/// the series as it opens, in the series' own topic, after the filename and
/// one line per item. Rejects a start that leaves the `begin --item` habit
/// unsaid, and one that names a series other than the one it started.
#[test]
fn start_ends_naming_how_a_change_joins_the_series() {
    let repo = Repo::new();
    let out = progress(
        &repo,
        &repo.root,
        &["start", "ship", "--title", "Ship", "--item", "One"],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(
        lines[2],
        "a change joins as it opens: arc begin <slug> --item ship:<item>"
    );
}
