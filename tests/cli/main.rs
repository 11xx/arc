#![allow(
    clippy::disallowed_methods,
    reason = "Fixtures own their subprocess environments."
)]

mod absent_actor;
mod absent_identity;
mod accept;
mod assurance;
mod audit;
mod audit_discharge;
mod base_pass;
mod briefs;
mod build_metadata;
mod bundle;
mod candidate;
mod catchup_open_change_reads;
mod catchup_reads;
mod chain;
mod changelog;
mod checkout_defaults;
mod checkpoint;
mod claims;
mod codex_model_checkpoint;
mod commit_lookup_gaps;
mod common;
mod confinement;
mod context;
mod contribution;
mod correspondence;
mod cost;
mod cost_backend_sets;
mod cost_completeness;
mod cost_evidence_export;
mod cost_lineage_whole;
mod cost_on_behalf_reviewer;
mod cost_participant_share;
mod cost_replay_once;
mod cost_report_entries;
mod cost_report_ranges;
mod cost_reviewer_subjects;
mod cost_share_entry;
mod cost_share_pricing;
mod cost_span_split;
mod cost_subject_membership;
mod cross_links;
mod debt_plan;
mod debt_standing;
mod declared_witnesses;
mod derived_attribution;
mod diff;
mod doctor;
mod exclusive;
mod explain;
mod external;
mod falsification_transport;
mod feedback;
mod final_wave_cost;
mod findings;
mod fixture_isolation;
mod fixture_seeds;
mod forge;
mod forge_custody;
mod forge_ingest;
mod forge_observation;
mod forge_round_trip;
mod forge_view;
mod fork;
mod gate_budgets;
mod gate_declarations;
mod gate_environment;
mod gate_paths;
mod gate_raises;
mod gate_reports;
mod gate_reuse;
mod gate_suspension;
mod gate_tiers;
mod gate_together;
mod gate_work;
mod git_environment;
mod git_questions;
mod guide_bounds;
mod guide_topics;
#[path = "../../src/harness_entry.rs"]
mod harness_entry;
mod hooks;
mod implementer_trailers;
mod import_authorization;
mod ineligible_review;
mod intake;
mod integrate;
mod journal;
mod journal_attachments;
mod journal_bind;
mod journal_exchange;
mod journal_living;
mod journal_pages;
mod journal_pins;
mod journal_reopen;
mod journal_staging;
mod journal_workbench;
mod ladder_clearance;
mod landing;
mod landing_budgets;
mod landing_reads;
mod lifecycle;
mod lock_waits;
mod lookup_reasons;
mod milestone;
mod model_resolution;
mod native_child_binding;
mod observe;
mod orchestrate;
mod pass;
mod path_stable_builds;
mod paths;
mod perf;
mod placeholders;
mod planners;
mod policy;
mod post_integration_verification;
mod progress;
mod project_gate_selection;
mod provenance;
mod provisional;
mod publication_budgets;
mod queue;
mod rebase;
mod receipts;
mod received_evidence;
mod registration_races;
mod registration_retirement;
mod relations;
mod release;
mod relocate_redact;
mod replicas;
mod rescue;
mod retention;
mod review;
mod review3_fixes_cost;
mod review4_fixes_cost;
mod review_clearance;
mod review_extents;
mod review_fixes_cost;
mod review_fixes_native_child;
mod review_fixes_span_split;
mod review_ladder;
mod review_roles;
mod rewrite;
mod role_carry;
mod roles;
mod run;
mod sandbox;
mod schemas;
mod selection;
mod skip_green;
mod slots;
mod stats_by_model;
mod take;
mod tiebreak_reviewers;
mod timeline;
mod trailer_edits;
mod trailer_projection;
mod tree_gates;
mod variant_share_pricing;
mod verification_outcomes;
mod verification_timing;
mod verify;
mod warm_worktrees;
mod workspace;
mod workspace_feed;
mod workspace_watch;

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

#[test]
fn operator_policy_unions_with_project_policy_without_tracked_changes() {
    policy::operator_policy_unions_with_project_policy_without_tracked_changes();
}

#[test]
fn conflicting_gate_commands_are_reported_and_refused() {
    policy::conflicting_gate_commands_are_reported_and_refused();
}

#[test]
fn layered_gates_take_the_stricter_timeout_and_refuse_a_different_environment() {
    policy::layered_gates_take_the_stricter_timeout_and_refuse_a_different_environment();
}
