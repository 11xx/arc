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
