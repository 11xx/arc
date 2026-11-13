# Gate measurements and acceptance boundary

Integration protects the named invariants below. Area suites execute only on demand; publication adds the five release-artifact checks.

## Measurements

Seconds of complete command wall time, including Cargo work. Each touched-source sample touches that file immediately before each independently measured gate; sums are conservative sequential costs. Six rounds per scenario. Warm dependencies and incremental state are retained. Touching tests incremental invalidation; it is not a clean build or an arbitrary semantic edit. Host contention is observed, not normalized away.

### Warm unchanged content

| Round | Before build | Before lint | Before invariants | Before sum | After build | After lint | After invariants | After sum |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0.082 | 2.659 | 7.075 | 9.816 | 0.156 | 4.154 | 9.516 | 13.826 |
| 2 | 0.083 | 3.316 | 6.621 | 10.020 | 0.178 | 3.998 | 9.277 | 13.453 |
| 3 | 0.078 | 2.673 | 6.614 | 9.365 | 0.203 | 4.018 | 9.152 | 13.372 |
| 4 | 0.244 | 2.703 | 6.685 | 9.632 | 0.236 | 4.155 | 9.958 | 14.350 |
| 5 | 0.084 | 2.662 | 6.560 | 9.306 | 0.223 | 3.140 | 6.514 | 9.877 |
| 6 | 0.078 | 2.631 | 6.613 | 9.322 | 0.076 | 2.627 | 6.510 | 9.214 |

### Touched leaf source

| Round | Before build | Before lint | Before invariants | Before sum | After build | After lint | After invariants | After sum |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 8.413 | 17.658 | 20.301 | 46.372 | 6.082 | 17.172 | 12.439 | 35.694 |
| 2 | 6.974 | 17.456 | 20.866 | 45.296 | 5.780 | 17.235 | 12.348 | 35.362 |
| 3 | 6.971 | 17.765 | 20.297 | 45.033 | 6.134 | 17.163 | 12.375 | 35.672 |
| 4 | 8.564 | 17.490 | 20.004 | 46.058 | 6.041 | 17.424 | 12.470 | 35.935 |
| 5 | 7.420 | 17.626 | 19.186 | 44.231 | 6.405 | 18.596 | 16.240 | 41.241 |
| 6 | 6.597 | 17.575 | 22.189 | 46.361 | 7.596 | 18.856 | 14.525 | 40.977 |

### Touched shared model source

| Round | Before build | Before lint | Before invariants | Before sum | After build | After lint | After invariants | After sum |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 8.787 | 22.592 | 27.993 | 59.371 | 7.476 | 17.556 | 12.419 | 37.451 |
| 2 | 8.777 | 19.061 | 22.055 | 49.893 | 6.368 | 17.431 | 12.298 | 36.096 |
| 3 | 8.001 | 19.345 | 23.415 | 50.760 | 5.847 | 18.047 | 14.440 | 38.335 |
| 4 | 9.350 | 19.310 | 25.912 | 54.573 | 6.817 | 24.214 | 19.362 | 50.393 |
| 5 | 12.461 | 19.932 | 26.096 | 58.489 | 9.776 | 27.165 | 19.809 | 56.750 |
| 6 | 7.888 | 19.438 | 32.382 | 59.708 | 9.419 | 26.943 | 19.106 | 55.467 |

## Witnesses

### I1: Supported old ledger histories retain their recorded meaning. (5)

- `bundle::import_reads_the_previous_bundle_version_and_refuses_older` (cli)
- `state::tests::events_without_the_supersession_edges_replay_unchanged` (unit)
- `state::tests::guarded_integration_shapes_reduce_and_legacy_events_derive_shape` (unit)
- `state::tests::legacy_reuse_of_non_green_passing_evidence_still_replays` (unit)
- `state::tests::old_opening_events_default_new_metadata` (unit)

### I2: A reader never authorizes work using store semantics it does not understand. (4)

- `bundle::a_bundle_from_a_newer_arc_is_refused_rather_than_partially_imported` (cli)
- `store::budget_format_tests::budget_adoption_fences_older_readers_only_when_needed` (unit)
- `store::tests::format_stamp_waits_on_one_store_wide_lock_and_never_downgrades` (unit)
- `store::budget_format_tests::base_pass_adoption_fences_budget_aware_and_stale_readers` (unit)

### I3: Published ledger events preserve append-only identity and order. (5)

- `bundle::import_rejects_malformed_known_events_before_writing` (cli)
- `lifecycle::append_policy_has_a_single_authority` (cli)
- `store::tests::an_event_whose_directory_sync_failed_is_visible_but_unconfirmed` (unit)
- `store::tests::append_refuses_an_event_before_the_change_opening_without_writes` (unit)
- `store::tests::local_writers_issue_ids_in_publication_order_in_one_millisecond` (unit)

### I4: Acknowledged journal content survives interrupted publication. (2)

- `journal::journal_reader_and_writer_refuse_unprepared_torn_tail` (cli)
- `journal_exchange::failed_event_append_preserves_bodies_and_retry_converges` (cli)

### I5: Integration cannot weaken the target's required evidence. (14)

- `gate_declarations::a_deleted_gate_is_still_owed_from_the_change_worktree_and_the_target` (cli)
- `gate_environment::another_environment_is_inapplicable_without_a_local_pass` (cli)
- `gate_paths::dangerous_paths_cannot_skip_gates` (cli)
- `landing::a_gate_the_top_member_adds_fails_and_nothing_closes` (cli)
- `tree_gates::a_clean_merge_breaking_a_gate_is_refused_before_it_can_ship` (cli)
- `verify::declared_probe_blocks_until_discriminating_evidence_matches_patchset` (cli)
- `approval::tests::an_approval_left_behind_by_the_head_is_stale` (unit)
- `gate_budgets::ordinary_budget_refusal_preserves_the_pass_and_requires_explicit_rerun` (cli)
- `gate_raises::raise_unrecorded_green_integrate` (cli)
- `gate_raises::raise_removal_refuses_check_and_integrate` (cli)
- `base_pass::replay_refuses_fabricated_diff_and_ancestry` (cli)
- `base_pass::covered_target_changes_since_the_baseline_execute_the_gate` (cli)
- `base_pass::local_refusals_cannot_be_rescued_by_a_baseline` (cli)
- `verification_outcomes::retired_bypass_does_not_authorize_integration` (cli)

### I6: An implementer's own read cannot count as independent clearance. (4)

- `review_extents::a_tree_read_never_covers_a_diff_obligation` (cli)
- `review_roles::an_implementer_can_never_clear_its_own_work_even_when_trailers_disagree` (cli)
- `approval::tests::an_external_approval_never_counts_alone_on_a_dangerous_path` (unit)
- `state::tests::only_an_audit_of_what_landed_clears_a_replayed_unread_debt` (unit)

### I7: A standing refusal cannot be turned into permission by debt or retry. (7)

- `lifecycle::blocking_finding_blocks_until_resolved_and_reapproved` (cli)
- `lifecycle::hold_blocks_integration` (cli)
- `lifecycle::iterating_change` (cli)
- `orchestrate::blocker_chain_transitions_and_suggests_ready_alternatives` (cli)
- `provisional::unapplied_retry_respects_holds` (cli)
- `approval::tests::a_local_refusal_of_this_patchset_is_not_waived` (unit)
- `integration::tests::lifecycle_refusal_precedes_fast_forward_divergence` (unit)

### I8: Execution refuses an authorization basis that changed before its effect. (7)

- `import_authorization::policy_revocation_during_format_wait_refuses_without_writes` (cli)
- `integrate::audit_fix_shape_revalidates_after_waiting_for_target_lock` (cli)
- `selection::stale_target_and_stale_evaluation_are_refused_by_name` (cli)
- `integration::tests::a_basis_the_second_reading_disagrees_with_refuses` (unit)
- `integration::tests::an_unobservable_fact_refuses_with_its_own_error` (unit)
- `store::tests::a_scope_is_derived_after_a_write_not_kept_across_it` (unit)
- `store::tests::one_read_reuses_repository_events_and_a_fresh_read_reports_damage` (unit)

### I9: A published Git effect is never reported as a different durable completion. (3)

- `integrate::fast_forward_closure_publication_failure_reports_recovery` (cli)
- `landing::recovery_at_the_composed_crash_point_runs_twice_to_the_same_end` (cli)
- `provisional::explicit_provisional_integration_keeps_the_change_open` (cli)

### I10: An operation cannot delete or overwrite work it does not own. (4)

- `integrate::cleanup_keeps_a_worktree_whose_recorded_path_is_a_symlink` (cli)
- `integrate::merge_writing_an_untracked_target_path_is_refused_by_name` (cli)
- `retention::a_trailer_rewrite_keeps_the_reviewed_commit_reachable` (cli)
- `custody::tests::a_symlinked_custody_directory_is_refused_for_every_access` (unit)

### I11: Untrusted imported claims cannot acquire local write or review authority. (2)

- `import_authorization::enrolled_import_requires_exact_received_provenance_without_writes` (cli)
- `import_authorization::input_verdict_preserves_operator_claim_without_review_credit` (cli)

### I12: A command cannot cross its established identity, role or confinement boundary. (7)

- `absent_actor::a_ledger_write_with_no_establishable_actor_refuses_and_a_read_proceeds` (cli)
- `fork::fork_begin_creates_worktree_marker_and_refuses_integration` (cli)
- `roles::reviewer_role_can_review_and_resolve_but_cannot_close_or_integrate` (cli)
- `sandbox::a_sandboxed_gate_run_refuses_the_recorded_checkout_outside_the_prefix` (cli)
- `verify::gate_timeout_records_failure_and_kills_the_process_group` (cli)
- `session_store::tests::a_child_named_by_the_recording_cannot_break_out_of_its_line` (unit)
- `tests::every_leaf_command_has_an_io_classification` (unit)

### I13: Exchange cannot restore redacted text. (1)

- `relocate_redact::exchange_carries_a_redaction_and_never_restores_removed_text` (cli)

### I14: Every fixture executes the selected artifact. (1)

- `fixture_seeds::fixture_commands_use_the_selected_executable` (cli)

### I15: Rendered journal text cannot activate embedded code. (1)

- `journal_pages::render_never_emits_active_content` (cli)

### S1: A normal reviewed, verified change can complete the core lifecycle. (1)

- `lifecycle::green_path_integrates_with_merge_commit` (cli)

## Deleted names

- `a_hyphenated_option_value_names_the_attached_form`
- `accept::accept_delivery_guide_and_help_name_rebase_patchset_remedy`
- `audit::ordinary_refusing_verdicts_keep_their_action_out_of_debt_guidance`
- `briefs::brief_help_states_which_versions_require_or_refuse_a_cause`
- `context::guide_and_help_teach_current_debt_and_partial_opencode_identity`
- `context::help_points_at_the_guide_and_at_live_state`
- `doctor::help_states_each_exit_status`
- `findings::findings_json_is_the_format_json_shorthand`
- `guide_topics::bare_map_fits_both_budgets_and_names_bootstrap_routes`
- `guide_topics::check_and_catchup_point_to_topics_without_changing_refusal_outcomes`
- `guide_topics::gate_declarations_have_one_discoverable_guide_home`
- `guide_topics::guide_index_routes_are_bounded_and_full_depth_preserves_the_manual`
- `guide_topics::guide_navigation_is_stateless_with_broken_configuration_and_every_role`
- `guide_topics::guide_search_is_literal_case_insensitive_and_keeps_private_env_values_out`
- `guide_topics::help_is_bounded_and_env_values_never_change_instruction_bytes`
- `guide_topics::journal_family_and_literal_search_reach_directory_resolution`
- `implementer_trailers::guide_and_review_help_distinguish_approval_authority_from_independent_coverage`
- `instructions::documented_trailer_examples_parse_with_plain_git`
- `instructions::instructions_git_check_separates_malformed_from_unsupported`
- `instructions::instructions_git_prints_the_canonical_spec_outside_a_repository`
- `integrate::every_read_only_command_reads_past_a_broken_policy`
- `journal::doctor_and_open_json_help_describe_their_own_reports`
- `journal::journal_help_prints_complete_question_workflow_commands`
- `journal::the_fr_alias_and_the_kind_verb_share_one_help_source`
- `journal::the_guide_and_help_teach_reading_a_known_artifact_without_a_filesystem_search`
- `journal_exchange::journal_bundle_guide_and_help_describe_artifact_admission`
- `lifecycle::env_help_explains_detection_and_its_non_zero_exit`
- `lifecycle::the_guide_teaches_identity_and_how_a_session_ends`
- `lifecycle::the_guide_teaches_what_a_finding_carries_before_a_repair`
- `messaging::a_collected_deferral_leaves_the_inbox`
- `messaging::assignment_set_override_clear_and_filter`
- `messaging::catchup_names_the_open_review_subject`
- `messaging::catchup_reports_ledger_and_journal_together`
- `messaging::comment_only_verdict_leaves_an_open_change_visible_and_reasoned`
- `messaging::every_inbox_bucket_key_is_present_when_empty`
- `messaging::every_open_change_appears_in_some_inbox_bucket`
- `messaging::implementer_role_may_announce_and_assign`
- `messaging::inbox_and_catchup_carry_open_deferrals`
- `messaging::inbox_buckets_classify_open_changes`
- `messaging::inbox_claim_rows_show_active_details_and_keep_stale_claims_exclusive`
- `messaging::inbox_names_the_journal_backlog_even_when_the_ledger_is_empty`
- `messaging::inbox_omits_released_and_expired_claims`
- `messaging::message_appends_well_formed_event_and_rejects_bad_input`
- `messaging::messages_filter_across_open_and_closed_changes`
- `messaging::messages_never_affect_check`
- `messaging::review_queue_groups_uncovered_changes_and_names_an_open_pass`
- `messaging::status_lists_messages_and_assignment_without_changing_schema`
- `metadata::closed_change_metadata_remains_readable`
- `metadata::json_cannot_be_combined_with_mutation_flags`
- `metadata::json_metadata_has_exact_schema_and_values`
- `metadata::populated_metadata_reads_back_as_text`
- `metadata::reading_empty_metadata_prints_empty_fields`
- `metadata::removals_and_assignment_clear_are_reflected_in_reads`
- `nested_leaf_at_top_level_suggests_its_command_path`
- `planners::planned_by_help_and_refusal_name_the_accepted_fields`
- `review::review_help_states_the_findings_shape_and_the_cause_rule_its_refusals_enforce`
- `run::run_list_help_describes_the_versioned_envelope`
- `schemas::every_schema_constant_is_registered_in_the_guide`
- `schemas::staleness_guide_names_rebase_snapshot_journal_defaults`
- `schemas::staleness_inbox_help_matches_schema_constant`
- `stats::an_observed_gate_run_counts_toward_gate_seconds`
- `stats::provenance_and_by_model_are_exclusive`
- `stats::provenance_counts_every_class_on_one_fixture`
- `stats::provenance_respects_change_and_tag_selection`
- `stats::rework_requires_changes_requested_then_new_patchset_then_approval`
- `stats::round_trip_is_exclusive_with_the_other_views`
- `stats::round_trip_reports_integrated_changes_only`
- `stats::round_trip_times_a_rebase_and_integrate_from_their_recorded_starts`
- `stats::several_changes_requested_on_one_patchset_are_one_round`
- `stats::stats_by_model_attributes_patchsets_and_rework_to_the_subject`
- `stats::stats_json_carries_schema_and_reports_selected_change`
- `trailer_edits::the_help_states_the_format_and_the_order`
- `trailer_projection::a_bare_role_flag_keeps_a_declared_person_in_their_own_form`
- `trailer_projection::a_bare_role_flag_refuses_what_it_cannot_resolve_and_records_nothing`
- `trailer_projection::a_derived_recorder_is_not_credited_and_the_note_names_the_remedy`
- `trailer_projection::a_historical_placeholder_role_assertion_is_a_note_with_its_remedy`
- `trailer_projection::a_session_contributor_note_suggests_the_model_its_own_records_name`
- `trailer_projection::a_session_id_contributor_projects_no_implementer`
- `trailer_projection::a_solo_recorder_with_a_placeholder_model_projects_no_implementer`
- `trailer_projection::bare_role_flags_under_a_derived_actor_assert_the_invoking_coordinate`
- `trailer_projection::missing_trailer_advice_never_suggests_a_placeholder_line`
- `trailer_projection::optional_role_flags_begin_orchestrator`
- `trailer_projection::optional_role_flags_begin_planner`
- `trailer_projection::optional_role_flags_brief_planner`
- `trailer_projection::optional_role_flags_roles_orchestrator`
- `trailer_projection::optional_role_flags_roles_planner`
- `trailer_projection::placeholder_role_trailers_are_advised_and_never_block`
- `trailer_projection::solo_under_a_derived_actor_projects_the_recorder_coordinate`
- `workspace::report_help_lists_every_attention_rule`
- `workspace::workspace_backlog_detail_hint_preserves_selection`
- `workspace::workspace_json_schemas_are_registered_in_the_guide`

91 named tests deleted. No deletion name was absent.

## Scope and budget basis

Measured source revision: `742a0274`. The before and final cohorts execute the same 68 witnesses, with the 91 inventory deletions already applied. The before strategy builds an optimized `cli-test` executable separately from development test harnesses. The final strategy uses development optimization level 1, no debug symbols, debug assertions and overflow checks enabled for every development artifact. Cargo receipts identify the executable and both harnesses.

The final cohort uses the declared command environment and the ordinary temporary fixture filesystem. Retained diagnostics use the project cache. An intermediate cohort forced fixtures onto the cache filesystem; it is excluded from these tables and the budgets. Host load and incremental compiler state are observed, not normalized. The host has 8 physical and 16 logical cores.

Each touched-source sample touches the named file immediately before each independently measured gate. The leaf is `src/commands/timeline.rs`; the shared source is `src/model.rs`. These are warm-dependency, incremental invalidation measurements. They do not bound clean dependency builds or arbitrary larger edits. The sum columns combine the three independently invalidated commands; a shared-artifact suite can amortize compilation.

| Gate | Worst touched model, s | Additional observed bound, s | Budget, s |
| --- | ---: | ---: | ---: |
| build | 9.776 | 30.312 after a source commit | 31 |
| lint | 27.165 | none | 28 |
| invariants | 19.809 | none | 20 |
| release, publication only | not a source-cohort sample | 269.769 cold artifact command | 270 |

The integration total is **79 seconds**. Limits round observed full-command maxima upward to whole seconds. The build bound includes the first build after a commit changed compiled build identity: timestamp-only touches cost less than that observed source change. Publication uses one cold complete-command observation, not a six-round publication claim. Its limit is outside the integration total.

## Compilation levers

- A shared optimized profile removes the additional CLI profile build. In the six-model-round profile comparison, median Cargo-reported invariant preparation fell from 17.730 to 6.140 seconds, an observed 11.590-second reduction. These compiler observations exclude fixture execution.
- Read-only Git metadata queries preserve the watched index. The index-preservation acceptance test fails on the unfixed build script. In the full-artifact pipeline probe, invariant Cargo preparation fell from 11.66 to 0.18 seconds after this fix; the artifact was fresh on the second command.
- A metadata-only `cargo check` build gate was rejected. With the index fix, one model-touch pipeline took 60.045 seconds using check and 48.350 seconds using a full build. The latter prepares the artifacts consumed by invariants. The 11.695-second observed difference is a bounded pipeline comparison under changing host load, not an uncontended hardware guarantee.
- The next compiler lever is a separately compiled small witness target. Exact runtime filters still compile all remaining test source, and lint still checks every target. No structural type, append-capability, Git-cache or semantic-model redesign is included.

## Inventory and selection decisions

- Rust inventory: 568 unit tests and 3,010 CLI tests. Integration selects 22 unit and 46 CLI witnesses. Publication adds five CLI release checks. The remaining 546 unit and 2,959 CLI tests are 3,505 area tests.
- All 91 requested Rust deletions existed. Five were macro-generated role-flag cases; their macro invocations and unused helpers were removed. No requested deletion name was absent.
- Seventeen obsolete Python selector checks were removed with the selector. Five Python artifact-runner acceptance checks remain on demand. Three focused Rust area checks cover instruction ceilings, project declaration selection and build metadata reads.
- Eight inherited area names disappeared upstream before this implementation: seven bypass cases and one bypass unit case. They are not counted as requested deletions.
- One unified invariant gate keeps exact names in `.arc/invariants.tsv`, with each invariant definition and claim in the same list. Area suites use exact module paths, including an empty path for root tests.
- Source and shared build inputs are broadly covered by every gate. Root people-only Markdown, test documentation and the license are inert. Markdown embedded under `src/` remains source. Every tracked source path is explicitly claimed; unknown paths retain the conservative fallback.
- Cross-change base passes require destination adoption and a retained eligible baseline. A change cannot adopt this selection rule for itself. Declaration and protected input changes execute the required gates.

### Additional witnesses

The budget CLI cases already occur in the supplied inventory even though budgets were described among the newer behaviors. One existing budget case is promoted because budget applicability is required evidence under I5. The other additions represent distinct newer authorization or format boundaries. All other new cases stay in area suites.

| Witness | Invariant and reason |
| --- | --- |
| `store::budget_format_tests::base_pass_adoption_fences_budget_aware_and_stale_readers` | I2: Base-pass records fence readers that only understand budgets. |
| `gate_budgets::ordinary_budget_refusal_preserves_the_pass_and_requires_explicit_rerun` | I5: An over-budget pass cannot authorize integration or implicit reuse. |
| `gate_raises::raise_unrecorded_green_integrate` | I5: A higher declaration needs a raise even when execution fits the old bound. |
| `gate_raises::raise_removal_refuses_check_and_integrate` | I5: Removing budgets or total while gates remain cannot relax target constraints. |
| `base_pass::replay_refuses_fabricated_diff_and_ancestry` | I5: Replay verifies a base pass against retained Git content and ancestry. |
| `base_pass::covered_target_changes_since_the_baseline_execute_the_gate` | I5: Selection covers the full baseline-to-head diff including target changes. |
| `base_pass::local_refusals_cannot_be_rescued_by_a_baseline` | I5: An earlier local refusal cannot borrow permission from a baseline. |
| `verification_outcomes::retired_bypass_does_not_authorize_integration` | I5: Historical bypass and review debt cannot authorize an unrun gate. |

## Acceptance evidence

- The full remaining `cargo test --locked` passed at the measured source revision: 568 unit tests and 3,010 CLI tests, with no ignored tests. Budget and report edits retain that Rust source; final declaration consumers are checked again on demand.
- Each of the seven replayed commits passed `cargo check --all-targets` and `cargo build --all-targets` in the owned checkout.
- The five publication tests passed against Cargo's release-profile executable: calendar package version, version agreement, build metadata, Zsh completions and manual generation.

| Faulty state | Rejection |
| --- | --- |
| Missing manifest witness | Actual invariant command exits 1 naming the missing exact witness before dispatch |
| Missing-name guard disabled | Runner acceptance reports that ValueError was not raised |
| Empty list accepted | Runner acceptance reports that ValueError was not raised |
| `--exact` removed | Runner argument assertion rejects nonexact dispatch |
| Exit status ignored | Nonzero child with a success-looking summary is rejected by the acceptance assertion |
| Zero or ignored tests accepted | Runner acceptance rejects a false green result |
| Cargo artifact path guessed | Receipt-path assertion rejects the substituted path |
| Inert patterns removed | Warm documentation executes three commands where zero is required |
| Source coverage removed | Source inventory row is unclaimed, despite the all-gate fallback |
| Oversized capability map | Entry-instruction assertion rejects more than 80 lines |
| Build metadata refreshes the index | Real-Git index-byte assertion fails on the unfixed implementation |

Restoring each faulty variant passes the affected check. The selected existing witnesses retain their assertions. Their finite histories are not a proof of all histories; the old-history reducer fixtures and append-policy allowlist remain required until independently demonstrated replacements exist.

## Gate adoption and handoff

The measured destination was `77d4afaa`. Master advanced through `4e1e2cf3` during the work. Subsequent replay and merged-tree verification belong to the lead.

Destination obligations survive head declaration changes. The destination still requires the legacy selector command, while this head removes its files. Its inherited `test-integration` gate also has no budget under the proposed total. A staged declaration/compatibility transition is required before this end state can integrate. The implementation does not waive or reinterpret those obligations.

The lead owns the final `arc verify arc-gates-20s-8t7mj1 --all`, review/debt disposition and integration. Local command executions and measurement runs are not Arc verification receipts. Publication-only release checks are outside the integration total. `arc gates measure` runs all declared events unless a gate is selected explicitly; integration-only measurement selects build, invariants and lint individually.

The isolated project-declaration witness makes cold documentation execute three commands, warms an authoritative baseline, then proves documentation executes none. Source, embedded Markdown and `.arc/` edits execute all three integration gates; all four declarations explicitly cover those inputs. The actual change cannot borrow this baseline before destination adoption.

Final committed-source probe timings, local final-head command results, cleanup and the budget/report commit are recorded in the implementation handoff.

## Retained source commits

| Revision | Subject |
| --- | --- |
| `59366cc8` | test(cli): run fixtures against an optimized executable |
| `d49756a2` | test(cli): overlap subprocess waits with bounded parallelism |
| `d84ba74d` | test(observe): bound recovery probes with a debug period |
| `65bcccc3` | test(cli): cap subprocess concurrency at 64 workers |
| `9097332f` | test(fixtures): omit inactive templates and automatic maintenance |
| `d98f169b` | test(cli): wake child waits on process exit |
| `d3f2ceac` | test(catchup): reuse immutable ledgers across output modes |
| `447b947a` | test(inventory): remove checks outside the invariant boundary |
| `3d20a958` | test(gates): require exact invariant witnesses with shared build artifacts |
| `742a0274` | perf(build): preserve the watched index during metadata reads |
