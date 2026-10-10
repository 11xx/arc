use crate::common::*;

#[test]
fn completions_zsh_emits_a_compdef_for_arc() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicates::str::contains("#compdef arc"));
}

#[test]
fn mangen_writes_arc_1_into_the_target_dir() {
    let repo = Repo::new();
    let out = repo.home.join("man");
    repo.arc(&repo.root)
        .args(["mangen", out.to_str().unwrap()])
        .assert()
        .success();
    let page = out.join("arc.1");
    assert!(page.is_file(), "arc.1 was not written");
    let text = fs::read_to_string(&page).unwrap();
    assert!(text.contains("arc"), "man page is empty");
}

/// A release is named for the date it is published, so the package version
/// is a calendar date and nothing else: the three numeric fields Cargo's
/// semver parser accepts, no leading zeros, and no qualifier. One release is
/// cut per date, so there is never a counter to carry.
#[test]
fn the_package_version_is_a_calendar_date() {
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        !version.contains('+') && !version.contains('-'),
        "version {version} carries a qualifier a date has no room for"
    );
    let fields = version.split('.').collect::<Vec<_>>();
    assert_eq!(fields.len(), 3, "version {version} is not YYYY.M.D");
    for field in &fields {
        assert!(
            !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()),
            "version {version} has a non-numeric field"
        );
        assert!(
            field.len() == 1 || !field.starts_with('0'),
            "version {version} has a leading zero Cargo rejects"
        );
    }
    let field = |index: usize| fields[index].parse::<u32>().unwrap();
    assert_eq!(
        fields[0].len(),
        4,
        "version {version} has no four-digit year"
    );
    assert!(
        (1..=12).contains(&field(1)),
        "version {version} has no month"
    );
    assert!((1..=31).contains(&field(2)), "version {version} has no day");
}

/// `--version` is where an operator reads the release, so it carries the
/// package version rather than a string that can drift from it.
#[test]
fn version_flag_reports_the_package_version() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::starts_with(format!(
            "arc {}",
            env!("CARGO_PKG_VERSION")
        )));
}

/// `--version` names the build on a second line, so a build past a tag is
/// told from the release itself; the first line stays the version alone for
/// scripts, and `-V` is that first line only.
#[test]
fn version_flag_names_the_build_beside_the_release() {
    let repo = Repo::new();
    let long = stdout(repo.arc(&repo.root).arg("--version"));
    let lines: Vec<&str> = long.lines().collect();
    assert_eq!(lines.len(), 2, "{long}");
    assert_eq!(
        lines[0],
        format!("arc {}", env!("CARGO_PKG_VERSION")),
        "{long}"
    );
    let build = lines[1].strip_prefix("build: ").expect(&long);
    assert!(
        build == "unknown"
            || build.ends_with(" (clean)")
            || build.ends_with(" (dirty)")
            || build.ends_with(" (unknown)"),
        "{long}"
    );

    let short = stdout(repo.arc(&repo.root).arg("-V"));
    assert_eq!(
        short,
        format!("arc {}\n", env!("CARGO_PKG_VERSION")),
        "{short}"
    );
}
