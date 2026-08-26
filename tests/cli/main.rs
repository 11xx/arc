mod audit;
mod briefs;
mod bundle;
mod chain;
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
mod metadata;
mod observe;
mod orchestrate;
mod pass;
mod paths;
mod provenance;
mod release;
mod rescue;
mod review;
mod roles;
mod run;
mod skip_green;
mod stats;
mod take;
mod timeline;
mod verify;
mod workspace;

use common::Repo;

#[test]
fn doctor_groups_advice_and_ignores_closed_claims() {
    doctor::doctor_groups_advice_and_ignores_closed_claims();
}

#[test]
fn doctor_reports_closed_registered_worktrees_without_removing_them() {
    doctor::doctor_reports_closed_registered_worktrees_without_removing_them();
}

#[test]
fn journal_dir_longest_prefix_and_git_identity_preserve_existing_slugs() {
    journal::journal_dir_longest_prefix_and_git_identity_preserve_existing_slugs();
}

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
