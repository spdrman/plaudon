# plaudon threat model

Normative security documentation for the plaudon BYOK desktop application.
It implements SPEC v1.3 §16 (security and privacy), §6.3A (trust and process
boundaries), §6.3 invariants 14–16 and Appendix B.5/B.7 (device credentials,
legacy BLE cleartext, mode-B handshake).

This directory is **machine-checked**. The matrix below is generated from
[`traceability.json`](traceability.json); a row whose verification test does not
exist, an uncovered §16.2 threat, a broken link, or a missing not-guaranteed
statement fails CI.

## Documents

| Document | Contents |
|---|---|
| [`assets-and-adversaries.md`](assets-and-adversaries.md) | §16.1 assets (sensitivity, location, retention) and adversaries (capability assumptions) |
| [`trust-boundaries.md`](trust-boundaries.md) | §6.3A boundary diagram, what crosses each boundary, every untrusted input domain and the module that validates it |
| [`data-flows.md`](data-flows.md) | One flow per ingestion route and processing destination: exactly which bytes leave the machine |
| [`traceability.json`](traceability.json) | The normative threat/control/verification matrix (source of the table below) |
| [`open-findings.md`](open-findings.md) | Every `open`/`planned` row restated as a finding, with severity, owner and the release gate it blocks |
| [`egress-evidence.md`](egress-evidence.md) | How the OS-level egress monitor run is produced and what the §24 evidence bundle contains |
| [`../adr/ADR-0013-single-egress-gate.md`](../adr/ADR-0013-single-egress-gate.md) | Why one Rust-owned egress gate instead of per-subsystem checks |
| [`../adr/ADR-0014-no-plaintext-credential-fallback.md`](../adr/ADR-0014-no-plaintext-credential-fallback.md) | Why an unavailable secret store is a locked state, never a downgrade |
| [`../adr/ADR-0015-out-of-process-untrusted-input.md`](../adr/ADR-0015-out-of-process-untrusted-input.md) | Why decoders and inference helpers run out of process |

## How to run the checks

```sh
cargo test -p plaudon-security --test threat_model_traceability
python3 scripts/check-threat-model.py
```

Both entry points perform the same checks. After editing `traceability.json`,
regenerate the table below with:

```sh
python3 scripts/check-threat-model.py --update
```

## Status vocabulary

| Status | Meaning |
|---|---|
| `implemented` | The control exists in this repository and every cited test exists and runs. |
| `open` | The control is partially present. The cited tests cover the implemented part; the remaining gap is a finding in [`open-findings.md`](open-findings.md). |
| `planned` | The control is not in this repository yet. The owning epic is named and **no** test name is cited — we do not reference tests that do not exist. |

`open` and `planned` are both unresolved for the purpose of SPEC §22.8 (threat
model accepted "without unresolved critical/high issues") and §1.5 ("Critical
and high security findings: 0 unresolved at public release"). Severity is
assigned per finding, not per status.

## Traceability matrix

Threat column values for `§16.2` rows are verbatim from SPEC v1.3 §16.2, and the
checker asserts that all 25 of them are covered exactly once. Rows tagged
`§16.2A`, `§16.3`, `§16.4`, `§16.5` and `§16.6` are additional obligations from
the surrounding prose.

<!-- BEGIN GENERATED MATRIX -->

| ID | Spec | Threat | Control | Owner | Epic | Status | Verification |
|---|---|---|---|---|---|---|---|
| TM-001 | §16.2 | Credential disclosure | Keychain/Credential Manager; no plaintext DB; redacted logs | `crates/security/src/secrets.rs` | D | open | `locked_store_denies_every_operation_without_returning_a_value`, `secret_string_debug_is_redacted`, `secret_store_error_debug_and_display_never_contain_secret_material`, `database_stores_only_opaque_references_no_credential_material` |
| TM-002 | §16.2 | Legacy BLE cleartext audio and static credential | Compatibility warning, explicit local-only does not imply over-the-air encryption, no raw GATT payload logs | `crates/security/src/outbound.rs` | D | open | `describe_is_honest_about_what_is_not_guaranteed` |
| TM-003 | §16.2 | Spoofed PLAUD BLE advertisement | Do not authorize from local name/manufacturer alone; authenticate device profile and user-selected identity, fail closed on mismatch | `crates/plaud-ble/src/lib.rs` | L | planned | — |
| TM-004 | §16.2 | Firmware handshake downgrade | Mode detection and strict profile selection; no fallback from newer encrypted mode to legacy cleartext | `crates/plaud-protocol/src/lib.rs` | L | planned | — |
| TM-005 | §16.2 | Device-side destructive sync | Strict read-only opcode allowlist, prohibit delete/clear/write-settings/OTA, no auto-ack before verification | `crates/plaud-protocol/src/lib.rs` | L | planned | — |
| TM-006 | §16.2 | Device reconnect wrong identity / replay | Per-device vault records, transfer binding and identity/firmware revalidation | `crates/source-plaud-device/src/lib.rs` | L | planned | — |
| TM-007 | §16.2 | Cloud request in Local-only Mode | Rust outbound policy blocks PLAUD HTTP, OpenRouter and optional model fetches | `crates/security/src/outbound.rs` | D | implemented | `default_policy_is_local_only`, `local_only_denies_every_request_in_the_cross_product`, `decision_table_per_destination_class_and_mode`, `local_only_denies_every_http_request`, `local_only_never_launches_the_cloud_cli`, `local_only_mode_initiates_no_app_traffic`, `missing_monitor_is_a_failure_not_a_skip` |
| TM-008 | §16.2 | Device source depleted by phone transfer | Mark source as incomplete/unsynced-only; never infer historical completeness | `crates/source-plaud-device/src/lib.rs` | L | planned | — |
| TM-009 | §16.2 | Arbitrary command execution | No generic shell; typed fixed CLI args; version check | `crates/security/src/outbound/subprocess.rs` | D | implemented | `a_non_pinned_program_is_refused_even_when_network_is_permitted`, `hostile_identifiers_cannot_inject_flags_or_shell_syntax`, `overlong_identifier_is_refused`, `legitimate_identifiers_pass`, `shared_hostile_names_are_safe_as_cli_arguments`, `malicious_titles_never_render_as_identifiers` |
| TM-010 | §16.2 | Signed URL leak | Ephemeral memory only; URL scrubber | `crates/security/src/outbound/http.rs` | D | open | `debug_never_renders_url_headers_or_body`, `destination_url_keeps_no_path_or_query_in_the_label`, `denied_envelope_carries_no_destination_detail_or_secret`, `a_response_carrying_a_signed_url_is_rejected`, `a_response_carrying_a_bearer_token_is_rejected`, `detail_never_carries_a_url_path_or_query` |
| TM-011 | §16.2 | SSRF via redirect | HTTPS/allowlist; DNS/IP safety; bounded redirects | `crates/security/src/outbound.rs` | D | open | `destination_url_parsing_rejects_hostile_inputs`, `allowlist_suffix_match_cannot_be_spoofed_by_a_lookalike_domain`, `empty_allowlist_means_no_host_is_reachable` |
| TM-012 | §16.2 | Malicious audio decoder input | Untrusted input sandboxing where feasible; patched codecs; resource caps | `crates/security/src/sandbox.rs` | D | open | `a_failed_helper_never_produces_a_completed_outcome`, `burning_cpu_past_the_budget_fails_without_a_false_completion`, `allocation_beyond_the_memory_cap_is_capped_and_recoverable`, `a_helper_killed_mid_request_is_recoverable_and_leaves_the_input_intact`, `a_forged_frame_length_is_rejected_without_a_large_allocation`, `an_outbound_connection_is_denied`, `executing_another_program_is_denied`, `reduced_isolation_is_never_reported_as_isolated`, `isolation_report_states_exactly_what_was_applied`, `the_reduced_isolation_path_reports_exactly_what_it_did_not_apply`, `notes_are_carried_into_the_description_and_counted_in_the_summary`, `the_note_vocabulary_is_closed_and_non_empty` |
| TM-013 | §16.2 | Supply-chain compromise | Lockfiles, SBOM, pinned hashes/signatures, SCA | `Cargo.lock` | N | open | — |
| TM-014 | §16.2 | Unwanted cloud transfer | Central dispatch policy; consent token; default local | `crates/security/src/outbound.rs` | D | implemented | `decision_table_per_destination_class_and_mode`, `model_download_requires_explicit_user_approval_even_when_network_is_permitted`, `telemetry_requires_opt_in_even_when_network_is_permitted`, `switching_to_local_only_mode_closes_previously_open_capabilities`, `local_only_mode_denies_even_with_a_perfectly_matching_grant`, `egress_gate_runs_before_consent_so_local_only_is_not_bypassable_by_grant_count`, `approval_carries_the_egress_ticket`, `recording_grant_does_not_cover_another_recording` |
| TM-015 | §16.2 | Prompt injection in transcript | Transcript treated as data; schema checks; no external tools | `crates/ai-tools/src/lib.rs` | I | planned | — |
| TM-016 | §16.2 | Unauthorized frontend file access | A closed typed command surface with no generic shell, HTTP or filesystem command, plus server-side path validation | `crates/app/src/validate.rs` | D | implemented | `every_published_command_name_appears_exactly_once`, `no_generic_shell_http_or_filesystem_command_is_published`, `rejected_paths_are_table_driven`, `a_symlink_that_escapes_the_root_is_rejected_and_one_that_stays_is_accepted`, `an_empty_approved_root_set_rejects_everything`, `a_hostile_path_is_rejected_without_touching_the_library` |
| TM-017 | §16.2 | Local plaintext data exposure | OS disk encryption guidance; future encrypted vault; warn on plaintext backup | `crates/export/src/lib.rs` | J | planned | — |
| TM-018 | §16.2 | Watched-folder file loss | Copy-only staging, readiness probe, immutable archive; no source deletion | `crates/source-folder/src/lib.rs` | K | open | `imported_source_file_is_untouched_by_staging`, `stage_and_commit_by_rename`, `abandoned_staged_file_is_removed_on_drop` |
| TM-019 | §16.2 | Unofficial PLAUD login flow | Official CLI only, no bearer-token copy/paste or browser extension bridge | `crates/security/src/outbound/subprocess.rs` | D | open | `a_non_pinned_program_is_refused_even_when_network_is_permitted`, `permitted_mode_prepares_the_pinned_program` |
| TM-020 | §16.2 | Remote deletion propagation | Explicit tombstones; remote deletes cannot cascade into archive | `crates/source-plaud-cli/src/lib.rs` | K | planned | — |
| TM-021 | §16.2 | Local automation escape | No unauthenticated localhost server; OS user-level permissions and typed commands | `crates/automation/src/lib.rs` | M | planned | — |
| TM-022 | §16.2 | Restored archive injection | Decompression bounds, path validation, checksums, schema verification | `crates/security/src/fsguard.rs` | D | open | `archive_validation_rejects_every_hostile_entry_shape`, `archive_validation_rejects_decompression_bombs`, `archive_validation_rejects_duplicates_including_case_and_nfc_folding`, `archive_validation_never_touches_the_filesystem`, `archive_target_resolution_catches_a_symlink_planted_after_inspection`, `archive_error_label_is_escaped_and_truncated` |
| TM-023 | §16.2 | Insecure auto-update | Signed artifacts; verified release manifest; rollback strategy | `apps/plaudon-egui` | N | planned | — |
| TM-024 | §16.2 | Excessive telemetry | Opt-in; no sensitive content; aggregate only | `crates/security/src/outbound.rs` | D | open | `telemetry_requires_opt_in_even_when_network_is_permitted`, `every_decision_is_recorded` |
| TM-025 | §16.2 | Cost explosion | User budgets + provider guardrails + uncertain-job safeguards | `crates/ai-openrouter/src/lib.rs` | G | planned | — |
| TM-026 | §16.2A | App-initiated network egress while Local-only Mode is on | A single Rust-owned OutboundPolicy gate used by every network-capable subsystem (PLAUD CLI, OpenRouter, model manager, update checker, telemetry, future helpers); deny by default; every decision recorded | `crates/security/src/outbound.rs` | D | implemented | `default_policy_is_local_only`, `local_only_denies_every_request_in_the_cross_product`, `every_decision_is_recorded`, `describe_lists_disabled_capabilities_in_local_only_mode`, `switching_to_local_only_mode_closes_previously_open_capabilities`, `a_fresh_installation_is_local_only_and_file_based`, `network_permitted_without_a_recorded_grant_fails_closed`, `local_only_mode_overrides_both_route_and_destination`, `a_cloud_synced_watched_folder_is_disabled_and_explained_in_local_only_mode`, `local_only_mode_initiates_no_app_traffic`, `local_import_really_commits_a_file_while_observed`, `negative_control_proves_the_fixture_can_fail`, `an_unobserved_run_is_reported_as_monitor_unavailable`, `bundle_round_trips_and_states_its_limits` |
| TM-027 | §16.2A | PLAUD Cloud CLI subprocess launched in Local-only Mode | The cloud CLI is a separate executable with its own network stack, so it is never invoked in Local-only Mode; launch requires an EgressTicket and the environment is scrubbed | `crates/security/src/outbound/subprocess.rs` | D | implemented | `local_only_never_launches_the_cloud_cli`, `env_scrubbing_removes_secret_and_loader_variables`, `a_cloud_cli_exec_fails_the_run`, `cloud_cli_detection_uses_the_program_leaf` |
| TM-028 | §16.2A | Legacy BLE radio-layer disclosure presented as covered by Local-only Mode | Every effective-policy snapshot carries the explicit statement that disabling internet access adds no radio-layer confidentiality; the same statement is reproduced in user-facing documentation | `crates/security/src/outbound.rs` | D | open | `describe_is_honest_about_what_is_not_guaranteed`, `readme_states_every_not_guaranteed_string` |
| TM-029 | §16.2A | Device identity assurance overclaimed as cryptographic verification | Label identity assurance `profile-authenticated` unless the protocol actually proves cryptographic identity; no `Supported` compatibility cell without a hardware evidence record | `crates/plaud-ble/src/lib.rs` | L | planned | — |
| TM-030 | §16.3 | Expired or revoked consent grant still passes the dispatch preflight | Consent is a scoped capability (purpose, content type, model/provider set, retention/routing, disclosure version) re-checked at dispatch time; revoked or expired grants fail closed | `crates/security/src/consent.rs` | D | implemented | `a_recorded_revocation_is_final_even_if_the_clock_moves_backwards`, `expiry_is_inclusive_of_the_expiry_second`, `revoked_grants_remain_in_the_ledger_as_evidence`, `revoke_all_for_route_also_catches_model_policy_scopes` |
| TM-031 | §16.3 | Destination, purpose or privacy posture changes without re-prompting | Re-prompt when destination/purpose/privacy posture or disclosure version changes; an unresolved posture blocks dispatch | `crates/security/src/consent.rs` | D | open | `unknown_retention_never_satisfies_and_is_never_satisfied`, `retention_requirements_only_accept_equal_or_stronger_postures`, `grant_for_route_a_does_not_authorize_route_b`, `same_model_different_downstream_provider_is_a_different_route`, `transcript_grant_does_not_authorize_an_audio_upload` |
| TM-032 | §16.3 | Consent inferred from the mere presence of an API key | A stored OpenRouter key never implies permission to send content; permission comes from an explicit scoped grant | `crates/security/src/consent.rs` | D | implemented | `api_key_presence_is_not_consent`, `ask_every_time_never_satisfies_a_preflight_automatically` |
| TM-033 | §16.4 | Plaintext credential fallback when the OS secret store is unavailable | macOS Keychain / Windows Credential Manager only; an unavailable store yields an actionable locked state, never a silent downgrade | `crates/security/src/secrets.rs` | D | implemented | `unavailable_store_reports_locked_state_never_a_plaintext_downgrade`, `locked_store_denies_every_operation_without_returning_a_value`, `unsupported_platform_store_is_unavailable_and_never_returns_a_value` |
| TM-034 | §16.4 | Universal or shared BLE token reused across devices | Per-device vault entries keyed by device identity; universal/global tokens prohibited; DB and backups hold opaque references only | `crates/security/src/secrets.rs` | D | implemented | `device_secret_is_not_loadable_through_another_device_scope`, `scope_accounts_are_namespaced_and_distinct`, `device_rows_carry_distinct_references_so_no_token_is_shared`, `backup_projection_of_the_database_carries_no_credential_material` |
| TM-035 | §16.4 | Secrets reach environment variables, argv, crash traces or SQLite | Scrubbed subprocess environment, no secret-bearing CLI arguments, no secrets in persisted state or diagnostics | `crates/security/src/outbound/subprocess.rs` | D | open | `env_scrubbing_removes_secret_and_loader_variables`, `hostile_identifiers_cannot_inject_flags_or_shell_syntax`, `scrubbed_env_carries_no_secret_name_or_value`, `secrets_are_not_cli_argument_representable`, `gated_command_argv_and_env_carry_no_secret`, `database_contains_no_credential_shaped_value_in_any_column` |
| TM-036 | §16.4 | Transient secret buffers linger in process memory | Zeroize transient buffers where feasible, and state plainly that in-process memory cannot be perfectly protected from privileged malware | `crates/security/src/secrets.rs` | D | planned | — |
| TM-037 | §16.5 | Path traversal or non-canonical path accepted at a trust boundary | Canonicalize at the boundary, confine to managed roots, use private temp directories, reject traversing archive entries | `crates/security/src/fsguard.rs` | D | implemented | `every_accepted_path_resolves_inside_a_permitted_root`, `rejects_traversal_even_when_it_would_land_inside_the_root`, `rejects_paths_outside_every_root`, `symlink_escaping_the_root_is_rejected_and_its_target_is_not_opened`, `symlinked_directory_component_mid_path_is_resolved_and_contained`, `rejects_empty_nul_device_namespace_and_unc`, `every_windows_device_name_is_rejected_with_and_without_extension`, `private_temp_dir_is_owner_only_and_removed_on_drop`, `path_error_envelope_is_stable_and_never_echoes_the_path` |
| TM-038 | §16.5 | Managed data directory readable by other local users | Restrictive permissions on the managed data directory and on staged temp files | `crates/security/src/fsguard.rs` | D | implemented | `set_restrictive_permissions_locks_down_managed_data`, `private_temp_dir_is_owner_only_and_removed_on_drop`, `two_staging_directories_never_collide` |
| TM-039 | §16.5 | Production build ships a debuggable artifact or an escape hatch out of the platform's runtime protections | The packaging scripts audit the artifact they produce: no hardened-runtime escape hatch, no debugger entitlement, exactly the five reviewed macOS privileges, an asInvoker Windows manifest, and a fail-closed exit without a signing credential | `apps/plaudon-egui/tests/packaging.rs` | D | implemented | `the_entitlements_are_exactly_the_five_privileges_the_product_uses`, `no_hardened_runtime_escape_hatch_is_requested`, `the_application_manifest_asks_for_no_privilege`, `both_packaging_scripts_fail_closed_without_credentials` |
| TM-040 | §16.6 | Cloud processing is not visibly distinguishable from local processing | Per-route labelling in the UI (route and processing destination shown separately), `Cloud: not used` only when the route is genuinely cloud-independent | `crates/ui/src/screens/devices/presentation.rs` | E | open | `the_cloud_sentence_is_derived_from_the_dto`, `the_cloud_sentence_exists_in_exactly_one_place_and_is_derived_from_the_dto` |
| TM-041 | §16.6 | Local-only Mode or provider zero-retention presented as a compliance or residency guarantee | Documented non-claims enforced in CI: a prohibited-assurance-language list is checked against every threat-model document, and every NOT_GUARANTEED statement is reproduced verbatim | `docs/threat-model/README.md` | D | implemented | `readme_states_every_not_guaranteed_string`, `threat_model_docs_make_no_prohibited_assurance_claim` |
| TM-042 | §15.4 | Error envelope leaks a credential, signed URL, request body or transcript excerpt | Every security error crosses into the application taxonomy through one mapping module onto the closed AppErrorCode set; the user-facing message is chosen by the domain code and the security layer's specific reason travels only in AppError::detail, which is never shown to the user and never carried into an event | `crates/security/src/domain_map.rs` | D | implemented | `every_security_code_maps_to_a_domain_code`, `every_mapped_code_is_a_member_of_the_closed_domain_set`, `detail_never_carries_a_url_path_or_query`, `the_security_explanation_lands_in_detail_not_in_the_user_message`, `local_only_denial_is_reported_as_local_only_mode`, `a_failed_helper_is_retryable_so_the_job_engine_can_persist_a_recoverable_state` |
| TM-043 | §16.4 | Diagnostic redactor leaves embedded URLs and separator-bearing credentials in persisted text | Every diagnostic record and unmapped vendor string passes through one redactor before persistence; a URL anywhere in a token and any opaque credential shape are replaced, not only space-delimited bare-scheme tokens | `crates/domain/src/error.rs` | B | open | `domain_redactor_redacts_separator_bearing_credentials` |
| TM-044 | §16.2A | Voice profile embedding leaves the device through any egress path (cloud dispatch, model download, telemetry, diagnostics or a notification payload) | Speaker embeddings get their own egress payload class, VoiceBiometric, which OutboundPolicy::authorize refuses in every network mode for every subsystem with no consent scope that could permit it; the local-only egress watch is extended to the enrol-and-match flow, and diagnostics redaction treats profile names like transcript text and embedding bytes as unloggable | `crates/security/src/outbound.rs` | D | planned | — |
| TM-045 | §13.7 | Embedding inversion or re-identification from a stolen voice-profile store | Embedding vectors exist at rest only as XChaCha20-Poly1305 ciphertext under a feature-scoped key in the OS secret store (new SecretScope::VoiceProfileKey); an unavailable store is a locked feature per ADR-0014, never a plaintext fallback; embeddings are excluded from exports by default; the documents state honestly that encryption protects data at rest, not data in use | `crates/app-core/src/voice_profiles/store.rs` | D | open | `an_embedding_round_trips_through_the_sealed_form`, `the_ciphertext_never_contains_the_plaintext_vector`, `a_tampered_ciphertext_is_refused_rather_than_decoded`, `a_ciphertext_cannot_be_moved_to_another_person_or_model`, `destroying_the_key_makes_every_remaining_ciphertext_undecryptable`, `a_locked_store_locks_the_biometric_tier_and_nothing_else`, `a_locked_store_refuses_the_voice_profile_key_without_a_plaintext_fallback` |
| TM-046 | §13.5 | Profile export leaks voiceprints outside the user's control (plaintext backup, metadata export or an archive restored on another machine) | Voice data is excluded from every export format and from portable backups by default; a backup containing the voiceprofiles namespace is representable only with ADR-0071 password encryption, enforced in ExportService::create_portable_backup; the archive secret scanner refuses embedding-shaped blobs outside that namespace; restoring a voice-bearing archive re-runs the consent gate before profiles activate | `crates/export/src/archive/voiceprofiles.rs` | J | implemented | `a_plaintext_backup_carrying_voice_profiles_cannot_be_produced`, `a_staged_voice_entry_cannot_be_emitted_as_a_plaintext_archive`, `an_ordinary_backup_carries_the_roster_and_no_biometric_entry`, `an_encrypted_backup_may_carry_voice_profiles_and_a_restore_re_runs_the_consent_gate`, `the_scanner_refuses_an_embedding_outside_the_voice_namespace`, `a_recording_export_carries_names_and_never_voice_data`, `every_committed_corpus_entry_satisfies_the_parser_invariants` |
| TM-047 | §17.4 | Voice-profile poisoning: the wrong person enrolled into a profile, mislabelling them in every later recording | Enrolment enforces minimum clean single-speaker speech and excludes overlap; matches are persisted suggestions that never change a transcript without explicit confirmation; every exemplar keeps provenance (recording, cluster, span) and a profile can be split by provenance; rejections are remembered and feed per-profile calibration; the UI never presents a match as identity | `crates/app-core/src/voice_profiles/enroll.rs` | M | open | `the_roster_round_trips_with_matching_off_and_never_reaches_the_embedder`, `no_embedding_is_reachable_while_ungated_or_while_matching_is_off`, `turning_the_feature_on_requires_accepting_the_current_disclosure`, `enrolling_then_confirming_a_suggestion_names_the_cluster_and_its_segments`, `rejecting_never_renames_and_reject_then_pick_records_the_correction`, `splitting_an_enrolment_re_seals_the_exemplars_it_moves` |
| TM-048 | §16.6 | Cross-library correlation of voiceprints: two libraries' profile stores combined to track one voice across contexts | No sync and no stable cross-library identifier exist; each library has its own AEAD key; voice data is excluded from exports by default and representable only inside password-encrypted backups, so there is no plaintext artifact to correlate; the residual risk that a user restores one encrypted backup into two libraries is documented rather than denied | `crates/app-core/src/voice_profiles/mod.rs` | D | open | `a_plaintext_backup_carrying_voice_profiles_cannot_be_produced`, `a_staged_voice_entry_cannot_be_emitted_as_a_plaintext_archive`, `an_encrypted_backup_may_carry_voice_profiles_and_a_restore_re_runs_the_consent_gate` |

<!-- END GENERATED MATRIX -->

## What this threat model does not guarantee

SPEC §16.2A forbids unverifiable promises, and §16.6 forbids presenting a
provider policy as a property of local processing. The following statements are
part of the product contract and are reproduced verbatim from
`plaudon_security::outbound::NOT_GUARANTEED` (`crates/security/src/outbound.rs`);
the checker fails if any of them is missing here.

1. Operating-system update agents, platform stores and OS telemetry are outside this application's egress control.
2. Other applications and browser activity on this machine are outside this application's egress control.
3. A user-selected folder backed by a cloud-sync client may cause that client to transfer data; such folders are disabled or labelled while Local-only Mode is on.
4. Legacy PLAUD BLE sessions may be unencrypted on the radio link; disabling internet access does not add radio-layer confidentiality.
5. In-process memory cannot be perfectly protected from privileged malware on the same machine.

Five further limits follow from the same sections and from Appendix B:

6. **Local-only Mode is about traffic this process initiates.** It is enforced in
   Rust at `OutboundPolicy`, not by a UI badge, and it covers our HTTP client,
   our model downloads, our update checks, our telemetry and the launch of the
   PLAUD Cloud CLI subprocess. It does not and cannot stop unrelated OS,
   browser or platform-store network activity on the same machine (§16.2A).
7. **Legacy BLE is a radio-layer risk even with zero cloud use.** The examined
   legacy profile authenticates with a static per-device token and has no
   application- or link-layer encryption, so the credential and the transferred
   `.ASR` bytes may be observable or replayable by someone with a radio in range
   (App. B.5). Turning off every internet connection does not change this.
8. **Device identity is `profile-authenticated`, not cryptographically
   verified.** We authenticate against an expected device profile and the
   identity the user selected; we do not claim proof of device identity unless a
   firmware mode whose protocol actually provides it has been implemented and
   tested (App. B.2/B.5, §16.2A).
9. **Deletion is archive deletion, not media sanitisation.** Deleting a
   recording removes the row and the stored bytes from our managed archive, but
   we make no claim about whether the underlying blocks can still be recovered
   from an SSD, a filesystem snapshot, a Time Machine backup or a previously
   exported copy (§13.5, §16.5).
10. **Helper isolation is per-platform and partial.** Untrusted decoding and
    inference run in a separate process with CPU, wall-clock and memory caps
    on every platform, and the supervisor reports exactly what it applied. On
    macOS the filesystem/network/exec restriction uses `sandbox-exec`, a
    primitive Apple has deprecated, and the memory cap is enforced by the
    parent sampling the child's resident set rather than by the kernel. On
    Windows a real job object bounds memory, time and process count, but V1
    applies no filesystem or network restriction there. On other unix
    platforms only POSIX resource limits apply. Where a dimension is not
    enforced the run-time report says so instead of implying containment, and
    kernel vulnerabilities plus the files the helper is legitimately granted
    remain in scope for an attacker (see `trust-boundaries.md` B8).

Additionally, choosing a provider with a zero-data-retention policy does not
make a workflow local, and no configuration of this application is presented as
satisfying any jurisdiction's data-residency or certification requirements
(§16.6, §23 reference accuracy notes).

## Release gates this document serves

* **§22.8** — the threat model, licensing review and vulnerability scans must be
  accepted with no unresolved critical/high issue. `open-findings.md` is the
  list that must be empty of critical/high at release.
* **§22.15 / §1.5** — Local-only Mode must demonstrably make no app-initiated
  PLAUD, OpenRouter, telemetry, update-check or model-download request and must
  never launch the PLAUD Cloud CLI. Rows TM-007, TM-026 and TM-027 carry that
  evidence.
* **§22.16** — device secrets stay in the OS credential store and unsupported
  authentication paths fail closed (TM-033, TM-034, TM-004).
* **§24 release evidence bundle** — the archived bundle must contain the
  security findings, the consent and local-only egress traces and each published
  device compatibility record. `traceability.json` is the index of which test
  produced which piece of that evidence.
