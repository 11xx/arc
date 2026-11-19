use super::common::*;

fn note(repo: &Repo, cwd: &Path, topic: &str, body: &str) -> String {
    let out = stdout(
        repo.arc(cwd)
            .args(["journal", "note", topic, "--body-file", "-"])
            .write_stdin(body.to_string()),
    );
    Path::new(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string()
}

fn append(repo: &Repo, cwd: &Path, file: &str, title: &str, body: &str) -> String {
    stdout(
        repo.arc(cwd)
            .args([
                "journal",
                "append",
                file,
                "--body-file",
                "-",
                "--title",
                title,
            ])
            .write_stdin(body.to_string()),
    );
    let dir = PathBuf::from(stdout(repo.arc(cwd).args(["journal", "dir"])).trim());
    journal_event_log(&dir)
        .iter()
        .rev()
        .find(|event| event["event"] == "evidence")
        .and_then(|event| event["evidence_id"].as_str())
        .unwrap()
        .to_string()
}

fn render(
    repo: &Repo,
    cwd: &Path,
    address: &str,
    extra: &[&str],
) -> (std::process::Output, PathBuf) {
    let out = repo.home.join("page.html");
    let _ = fs::remove_file(&out);
    let mut cmd = repo.arc(cwd);
    cmd.args(["journal", "render", address, "--out", out.to_str().unwrap()]);
    cmd.args(extra);
    (cmd.output().unwrap(), out)
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

/// Whole-artifact display and rendering require a regular body in hot or cold
/// storage. A second name linked to a note must not become another artifact.
#[test]
fn whole_artifacts_require_regular_bodies_in_both_stores() {
    let repo = Repo::new();
    let file = note(&repo, &repo.root, "original", "# Original\n");
    let hot = PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim());
    let cold = PathBuf::from(format!("{}-archive", hot.display()));
    fs::create_dir_all(&cold).unwrap();
    let alias = file.replace("-original-", "-alias-");
    let mut accepted = Vec::new();
    for dir in [&hot, &cold] {
        let link = dir.join(&alias);
        std::os::unix::fs::symlink(hot.join(&file), &link).unwrap();
        for command in ["show", "render"] {
            let output = repo
                .arc(&repo.root)
                .args(["journal", command, &alias])
                .output()
                .unwrap();
            if output.status.success() || !output.stdout.is_empty() {
                accepted.push((dir.clone(), command));
            }
        }
        fs::remove_file(link).unwrap();
    }
    assert!(
        accepted.is_empty(),
        "accepted linked artifact bodies: {accepted:?}"
    );
}

/// Claim: a render is one static page carrying the corrected title, the
/// body, each appended block under its title and anchored by its ID, and a
/// contents list linking them. Rejects a page missing a block or its anchor.
#[test]
fn render_writes_one_page_with_contents_and_titled_blocks() {
    let repo = Repo::new();
    let file = note(
        &repo,
        &repo.root,
        "levels",
        "# Rendering levels\n\nIntro paragraph.\n\n## Layers\n\nToolkit, renderer, compositor.\n",
    );
    let first = append(
        &repo,
        &repo.root,
        &file,
        "Damage measurements",
        "Repaint fell to 1.2 ms.\n",
    );
    let second = append(
        &repo,
        &repo.root,
        &file,
        "Glyph atlas",
        "## Scale\n\nRebuilt on scale change.\n",
    );

    let (output, page) = render(&repo, &repo.root, &file, &[]);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("rendered: {file} (2 blocks)")),
        "{stderr}"
    );
    let html = fs::read_to_string(&page).unwrap();
    assert!(html.starts_with("<!doctype html>\n"), "{html}");
    assert!(html.contains("<title>Rendering levels</title>"), "{html}");
    assert!(html.contains("<h1>Rendering levels</h1>"), "{html}");
    assert_eq!(
        html.matches("<h1>").count(),
        1,
        "the body title became a second h1"
    );
    assert!(html.contains("<h2 id=\"layers\">Layers</h2>"), "{html}");
    for (id, title) in [(&first, "Damage measurements"), (&second, "Glyph atlas")] {
        assert!(
            html.contains(&format!(
                "<section class=\"block block-evidence\" id=\"{id}\">"
            )),
            "{html}"
        );
        assert!(html.contains(&format!("<h2>{title}</h2>")), "{html}");
        assert!(
            html.contains(&format!(
                "<a href=\"#{id}\">{title} <span class=\"toc-stamp\">tester · "
            )),
            "{html}"
        );
    }
    // A block's own headings sit under its title.
    assert!(html.contains("<h3 id=\"scale\">Scale</h3>"), "{html}");
    assert!(html.contains("<a href=\"#layers\">Layers</a>"), "{html}");
}

/// Claim: nothing in an artifact's Markdown makes the page run, embed or
/// leave by an unsafe scheme: raw HTML shows as code, a `javascript:` link
/// keeps only its text, an image loads only over https, and no request the
/// page makes sends a referrer. Rejects a page that passes raw HTML or an
/// unsafe URL through.
#[test]
fn render_never_emits_active_content() {
    let repo = Repo::new();
    let file = note(
        &repo,
        &repo.root,
        "hostile",
        "# Hostile\n\n<script>alert(1)</script>\n\nInline <img src=x onerror=alert(1)> here.\n\n\
         [click](javascript:alert(1)) and [docs](https://example.com/docs).\n\n\
         ![local](file.png) ![remote](https://example.com/a.png)\n",
    );
    let (output, page) = render(&repo, &repo.root, &file, &[]);
    assert!(output.status.success(), "{output:?}");
    let html = fs::read_to_string(&page).unwrap();
    assert!(
        html.contains("<meta name=\"referrer\" content=\"no-referrer\">"),
        "{html}"
    );
    let lower = html.to_ascii_lowercase();
    assert!(!lower.contains("<script"), "{html}");
    assert!(!lower.contains("<img src=\"x\""), "{html}");
    assert!(!lower.contains("javascript:alert"), "{html}");
    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{html}"
    );
    assert!(html.contains("click"), "{html}");
    assert!(
        html.contains("<a href=\"https://example.com/docs\">docs</a>"),
        "{html}"
    );
    assert!(
        html.contains("<img src=\"https://example.com/a.png\" alt=\"remote\""),
        "{html}"
    );
    assert!(!html.contains("file.png"), "{html}");
}

/// Claim: a home path, the project checkout's own path, and a private
/// address refuse the render, naming line and text, and write nothing;
/// `--strip-private` replaces them in the page. Rejects publishing a
/// machine's layout or network.
#[test]
fn render_refuses_private_text_and_strips_it_on_request() {
    let repo = Repo::new();
    let checkout = repo.root.display().to_string();
    let file = note(
        &repo,
        &repo.root,
        "private",
        &format!(
            "# Private\n\nSee /home/someone/notes.md and {checkout}/src/main.rs.\n\nThe NAS is at 192.168.1.9 or nas.local.\n"
        ),
    );
    let (output, page) = render(&repo, &repo.root, &file, &[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("line 3: local path /home/someone/notes.md"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("line 3: local path {checkout}/src/main.rs")),
        "{stderr}"
    );
    assert!(
        stderr.contains("line 5: private address 192.168.1.9"),
        "{stderr}"
    );
    assert!(
        stderr.contains("line 5: private host nas.local"),
        "{stderr}"
    );
    assert!(!page.exists(), "a refused render wrote its page");

    let (output, page) = render(&repo, &repo.root, &file, &["--strip-private"]);
    assert!(output.status.success(), "{output:?}");
    let html = fs::read_to_string(&page).unwrap();
    assert!(!html.contains("/home/someone"), "{html}");
    assert!(!html.contains(&checkout), "{html}");
    assert!(!html.contains("192.168.1.9"), "{html}");
    assert!(html.contains("[local path]"), "{html}");
    assert!(html.contains("[private address]"), "{html}");
    assert!(html.contains("[private host]"), "{html}");
}

/// Claim: `published` records where a copy lives; an address of that
/// artifact, or of one of its blocks, written in another page links to the
/// copy, and an ID of a block on the page itself links to its anchor. The
/// URL in force shows in `pins`. Rejects a page whose addresses lead nowhere.
#[test]
fn published_copies_are_where_addresses_link() {
    let repo = Repo::new();
    let target = note(&repo, &repo.root, "target", "# Target\n");
    let target_block = append(&repo, &repo.root, &target, "Finding", "Found.\n");
    stdout(repo.arc(&repo.root).args([
        "journal",
        "published",
        &target,
        "--url",
        "https://target.example.dev/",
    ]));
    let again = stdout(repo.arc(&repo.root).args([
        "journal",
        "published",
        &target,
        "--url",
        "https://target.example.dev/",
    ]));
    assert!(again.starts_with("already recorded:"), "{again}");
    repo.arc(&repo.root)
        .args([
            "journal",
            "published",
            &target,
            "--url",
            "file:///tmp/x.html",
        ])
        .assert()
        .failure();

    let page_file = note(
        &repo,
        &repo.root,
        "citing",
        &format!("# Citing\n\nBuilds on {target} and `{target_block}`.\n"),
    );
    let own = append(&repo, &repo.root, &page_file, "Own", "Self.\n");
    append(
        &repo,
        &repo.root,
        &page_file,
        "Later",
        &format!("As {own} showed.\n"),
    );
    let (output, page) = render(&repo, &repo.root, &page_file, &[]);
    assert!(output.status.success(), "{output:?}");
    let html = fs::read_to_string(&page).unwrap();
    assert!(
        html.contains(&format!(
            "<a href=\"https://target.example.dev/\">{target}</a>"
        )),
        "{html}"
    );
    assert!(
        html.contains(&format!(
            "<a href=\"https://target.example.dev/#{target_block}\"><code>{target_block}</code></a>"
        )),
        "{html}"
    );
    assert!(
        html.contains(&format!("<a href=\"#{own}\">{own}</a>")),
        "{html}"
    );

    stdout(repo.arc(&repo.root).args(["journal", "pin", &target]));
    let pins = json_stdout(repo.arc(&repo.root).args(["journal", "pins", "--json"]));
    assert_eq!(pins["pins"][0]["published"], "https://target.example.dev/");
    let (output, _) = render(&repo, &repo.root, &target, &[]);
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("published copy: https://target.example.dev/"));
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

/// Claim: `<project>::<file>` in a page links to the copy the other
/// project's journal records, and `render` reads another project's artifact
/// by the same address. Rejects links resolved against the wrong journal.
#[test]
fn qualified_addresses_link_across_projects() {
    let owner = Repo::new();
    let target = note(&owner, &owner.root, "shared", "# Shared\n");
    stdout(owner.arc(&owner.root).args([
        "journal",
        "published",
        &target,
        "--url",
        "https://shared.example.dev/",
    ]));
    let label = owner
        .root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let reader = sibling_project(&owner, "reader");
    let citing = note(
        &owner,
        &reader,
        "citing",
        &format!("# Citing\n\nSee `{label}::{target}`.\n"),
    );
    let (output, page) = render(&owner, &reader, &citing, &[]);
    assert!(output.status.success(), "{output:?}");
    let html = fs::read_to_string(&page).unwrap();
    assert!(
        html.contains(&format!(
            "<a href=\"https://shared.example.dev/\"><code>{label}::{target}</code></a>"
        )),
        "{html}"
    );
    let (output, page) = render(&owner, &reader, &format!("{label}::{target}"), &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(fs::read_to_string(&page)
        .unwrap()
        .contains("<h1>Shared</h1>"));
}

fn journal_dir(repo: &Repo, cwd: &Path) -> PathBuf {
    PathBuf::from(stdout(repo.arc(cwd).args(["journal", "dir"])).trim())
}

/// Claim: `--json` renders every address in one run, in order: the
/// stylesheet once, then per page its journal, file, title, kind, block
/// count, the sha256 and length of the body as read, and its HTML. An address
/// that fails carries its error, the others still render, and the run exits
/// 16. Several addresses without `--json` refuse. Rejects a batch that stops
/// at the first failure, or a digest of anything but the body.
#[test]
fn several_addresses_render_in_one_json_document() {
    let owner = Repo::new();
    let first = note(&owner, &owner.root, "first", "# First\n\nOne.\n");
    append(&owner, &owner.root, &first, "Later", "Two.\n");
    let reader = sibling_project(&owner, "reader");
    let second = note(&owner, &reader, "second", "# Second\n\nThree.\n");
    let owner_dir = journal_dir(&owner, &owner.root);
    let reader_dir = journal_dir(&owner, &reader);
    let addresses = [
        format!("{}::{first}", owner_dir.display()),
        format!("{}::20260101T000000Z-gone-note.md", owner_dir.display()),
        format!("{}::{second}", reader_dir.display()),
    ];

    let output = owner
        .arc(&owner.root)
        .args(["journal", "render", "--json"])
        .args(&addresses)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(16), "{output:?}");
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema"], "arc-journal-render/1");
    assert!(document["style"]
        .as_str()
        .unwrap()
        .contains(".block-header"));
    let pages = document["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3);

    let body = fs::read(owner_dir.join(&first)).unwrap();
    assert_eq!(pages[0]["address"], addresses[0]);
    assert_eq!(pages[0]["file"], first);
    assert_eq!(pages[0]["title"], "First");
    assert_eq!(pages[0]["kind"], "note");
    assert_eq!(pages[0]["blocks"], 1);
    assert_eq!(pages[0]["sha256"], hex::encode(Sha256::digest(&body)));
    assert_eq!(pages[0]["bytes"], body.len());
    assert_eq!(pages[0]["published"], serde_json::Value::Null);
    let html = pages[0]["html"].as_str().unwrap();
    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(html.contains("<h2>Later</h2>"), "{html}");

    assert_eq!(pages[1]["address"], addresses[1]);
    assert!(pages[1]["html"].is_null(), "{}", pages[1]);
    assert!(
        pages[1]["error"]
            .as_str()
            .unwrap()
            .contains("20260101T000000Z-gone-note.md"),
        "{}",
        pages[1]
    );

    assert_eq!(pages[2]["file"], second);
    assert_eq!(
        pages[2]["journal_dir"],
        reader_dir.display().to_string(),
        "{}",
        pages[2]
    );
    assert!(pages[2]["html"]
        .as_str()
        .unwrap()
        .contains("<h1>Second</h1>"));

    owner
        .arc(&owner.root)
        .args(["journal", "render", &first, &first])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "several addresses render only with --json",
        ));
}

/// Claim: `--fragment` is the page's markup alone, header, contents and
/// text, without the document shell, the stylesheet, or the footer that
/// dates the render, so a host page supplies those and the same journal
/// gives the same bytes. Rejects a fragment carrying a shell, a style, or
/// the render date.
#[test]
fn a_fragment_is_the_markup_without_shell_style_or_date() {
    let repo = Repo::new();
    let file = note(
        &repo,
        &repo.root,
        "fragment",
        "# Fragment\n\n## One\n\nText.\n\n## Two\n\nMore.\n",
    );
    let (output, page) = render(&repo, &repo.root, &file, &["--fragment"]);
    assert!(output.status.success(), "{output:?}");
    let html = fs::read_to_string(&page).unwrap();
    assert!(
        html.starts_with("<header class=\"page-header\" id=\"top\">"),
        "{html}"
    );
    assert!(html.contains("<details class=\"contents\""), "{html}");
    assert!(html.contains("<main>"), "{html}");
    for shell in [
        "<!doctype",
        "<html",
        "<head>",
        "<style",
        "<body",
        "page-footer",
        "Rendered by arc",
    ] {
        assert!(!html.contains(shell), "{shell} in {html}");
    }
}

/// Claim: `--keep-private` renders private text as written and counts it,
/// for a page that stays on its owner's network; it cannot be combined with
/// `--strip-private`, and under `--json` without either a page holding
/// private text is an error entry naming it. Rejects a keep mode that still
/// refuses or strips.
#[test]
fn keep_private_renders_private_text_as_written() {
    let repo = Repo::new();
    let file = note(
        &repo,
        &repo.root,
        "lan",
        "# LAN\n\nThe NAS is at 192.168.1.9, notes in /home/someone/nas.md.\n",
    );
    let (output, page) = render(&repo, &repo.root, &file, &["--keep-private"]);
    assert!(output.status.success(), "{output:?}");
    let html = fs::read_to_string(&page).unwrap();
    assert!(html.contains("192.168.1.9"), "{html}");
    assert!(html.contains("/home/someone/nas.md"), "{html}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("2 private stretches kept"),
        "{output:?}"
    );

    let (output, _) = render(
        &repo,
        &repo.root,
        &file,
        &["--keep-private", "--strip-private"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");

    let output = repo
        .arc(&repo.root)
        .args(["journal", "render", "--json", &file])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(16), "{output:?}");
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let error = document["pages"][0]["error"].as_str().unwrap();
    assert!(error.contains("private address 192.168.1.9"), "{error}");

    let kept = json_stdout(repo.arc(&repo.root).args([
        "journal",
        "render",
        "--json",
        "--keep-private",
        &file,
    ]));
    assert_eq!(kept["pages"][0]["private_stretches"], 2);
}

/// Claim: on a narrow screen the contents list starts open when it has at
/// most eight entries, and folded under a summary naming its length when it
/// has more, so a long page's text comes first on a phone. Rejects a long
/// list that starts open, or a short one that starts folded.
#[test]
fn a_long_contents_list_starts_folded() {
    let repo = Repo::new();
    let sections = |count: usize| {
        (1..=count)
            .map(|n| format!("## Section {n}\n\nText {n}.\n\n"))
            .collect::<String>()
    };
    for (topic, count, open) in [("eight", 8, true), ("nine", 9, false)] {
        let file = note(
            &repo,
            &repo.root,
            topic,
            &format!("# {topic}\n\n{}", sections(count)),
        );
        let (output, page) = render(&repo, &repo.root, &file, &[]);
        assert!(output.status.success(), "{output:?}");
        let html = fs::read_to_string(&page).unwrap();
        let details = if open {
            "<details class=\"contents\" id=\"contents\" open>"
        } else {
            "<details class=\"contents\" id=\"contents\">"
        };
        assert!(html.contains(details), "{topic}: {html}");
        assert!(
            html.contains(&format!(
                "<summary><h2>Contents</h2> <span class=\"toc-count\">{count} entries</span></summary>"
            )),
            "{topic}: {html}"
        );
    }
}
