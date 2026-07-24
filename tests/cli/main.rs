mod briefs;
mod bundle;
mod changelog;
mod claims;
mod common;
mod context;
mod diff;
mod doctor;
mod findings;
mod forge;
mod hooks;
mod journal;
mod lifecycle;
mod messaging;
mod observe;
mod orchestrate;
mod paths;
mod provenance;
mod release;
mod rescue;
mod roles;
mod skip_green;
mod stats;
mod take;
mod timeline;
mod verify;
mod workspace;

use common::Repo;

#[test]
fn nested_leaf_at_top_level_suggests_its_command_path() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["note"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("journal note"));
}

#[test]
fn top_level_typo_retains_clap_suggestion() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journl"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "a similar subcommand exists: 'journal'",
        ));
}
