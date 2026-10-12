//! J2.4: the archive serializer is an allowlist, and this test is the thing
//! that keeps it one.
//!
//! Every column of every table is listed below with a decision: it is either
//! carried by a named archive document, or excluded with a reason. A column
//! added to SQLite later fails this test until somebody records the decision,
//! which is exactly the acceptance criterion ("adding a new DB column does not
//! leak it into archives without an explicit decision").

use std::collections::BTreeSet;

use plaudon_integration_tests::fakes::library;
use sqlx::Row;

/// Where a carried column ends up, or why a column stays behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// Serialized into the named archive document.
    Carried(&'static str),
    /// Deliberately absent, with the reason.
    Excluded(&'static str),
}

use Decision::{Carried, Excluded};

/// Reasons, written once so they read consistently.
const SECRET: &str = "credential or vault handle (spec 13.6 excludes secrets)";
const TRANSIENT: &str = "transient job/queue/event state (spec 13.6 excludes transient jobs)";
const LOCAL: &str = "machine-local state that must not travel to another install";
const DERIVED: &str = "re-derived on the target library during restore";
const LABELLING: &str =
    "speaker review history; the archive carries the current label, not its edits";
const BILLING: &str = "provider dispatch and billing record, not user content";
const PIPELINE: &str =
    "provenance of the generating run; the archive carries the output body, not the run";
const CONVERSATION: &str =
    "per-install tool conversation history; the archive carries outputs, not the chat";
const SEARCH_INDEX: &str = "full-text index shadow table, rebuilt from the restored transcripts";
const SUGGESTION: &str =
    "a pending decision about a diarization run; runs are never archived, so a restored library has nothing to attach one to and re-running matching recomputes it (VP-078)";
const CONSENT_JOURNAL: &str =
    "the local consent and deletion journal; it is evidence about what happened in this library, not library content, and it must not follow a profile onto another machine (VP-079)";
const MATCH_EMBEDDING: &str =
    "sealed cluster embeddings, the embed stage's scratch for one matching job; they are biometric material with no owner yet — enrolment is what promotes an embedding to an exemplar — and the executor discards them when the job settles, so no archive has anything to carry (VP-020, VP-022, VP-078)";
const MATCH_REPORT: &str =
    "the match stage's scratch for one matching job: scores and bands against a diarization run that is itself never archived, discarded when the job settles and recomputed by re-running matching (VP-020, VP-102)";

#[allow(clippy::too_many_lines)]
fn decisions() -> Vec<(&'static str, Vec<(&'static str, Decision)>)> {
    vec![
        (
            "recordings",
            vec![
                ("id", Carried("records/<id>.json")),
                ("title", Carried("annotations/<id>.json")),
                ("captured_at", Carried("records/<id>.json")),
                ("duration_ms", Carried("records/<id>.json")),
                ("preferred_media_asset_id", Carried("records/<id>.json")),
                ("annotation", Carried("annotations/<id>.json")),
                ("created_at", Carried("records/<id>.json")),
                (
                    "deleted_at",
                    Excluded("a tombstoned recording is not archived at all"),
                ),
            ],
        ),
        (
            "recording_tags",
            vec![
                ("recording_id", Carried("annotations/<id>.json")),
                ("tag", Carried("annotations/<id>.json")),
            ],
        ),
        (
            "media_assets",
            vec![
                ("id", Carried("records/<id>.json")),
                ("sha256", Carried("records/<id>.json")),
                ("byte_length", Carried("records/<id>.json")),
                (
                    "relative_path",
                    Excluded("the archive is content-addressed; the path is rebuilt on restore"),
                ),
                ("mime", Carried("records/<id>.json")),
                ("codec", Carried("records/<id>.json")),
                ("sample_rate", Carried("records/<id>.json")),
                ("channels", Carried("records/<id>.json")),
                ("duration_ms", Carried("records/<id>.json")),
                ("quality_class", Carried("records/<id>.json")),
                ("verification_state", Excluded(DERIVED)),
                ("verified_at", Excluded(DERIVED)),
                ("created_at", Carried("records/<id>.json")),
            ],
        ),
        (
            "recording_media",
            vec![
                ("recording_id", Carried("records/<id>.json")),
                ("media_asset_id", Carried("records/<id>.json")),
                ("quality_class", Carried("records/<id>.json")),
                ("obtained_at", Carried("records/<id>.json")),
            ],
        ),
        (
            "media_revisions",
            vec![
                ("id", Carried("records/<id>.json")),
                ("recording_id", Carried("records/<id>.json")),
                ("media_asset_id", Carried("records/<id>.json")),
                ("source_item_id", Carried("records/<id>.json")),
                ("source_version", Carried("records/<id>.json")),
                ("predecessor_id", Carried("records/<id>.json")),
                ("obtained_at", Carried("records/<id>.json")),
            ],
        ),
        (
            "sources",
            vec![
                ("id", Carried("sources/<id>.json")),
                ("source_kind", Carried("sources/<id>.json")),
                ("account_scope", Carried("sources/<id>.json")),
                ("display_name", Carried("sources/<id>.json")),
                ("status", Carried("sources/<id>.json")),
                (
                    "capabilities_json",
                    Excluded("a probe result for this machine, revalidated after restore"),
                ),
                ("last_verified_at", Excluded(DERIVED)),
                (
                    "sync_cursor",
                    Excluded("may embed a provider session or page token"),
                ),
                ("sync_policy", Carried("sources/<id>.json")),
                ("created_at", Carried("sources/<id>.json")),
            ],
        ),
        (
            "source_items",
            vec![
                ("id", Carried("records/<id>.json")),
                ("source_id", Carried("records/<id>.json")),
                ("external_id", Carried("records/<id>.json")),
                ("recording_id", Carried("records/<id>.json")),
                ("import_status", Carried("records/<id>.json")),
                ("last_seen_at", Carried("records/<id>.json")),
                ("remote_version", Carried("records/<id>.json")),
                ("remote_state", Carried("records/<id>.json")),
                ("tombstone_reason", Carried("records/<id>.json")),
                ("acquisition_complete", Carried("records/<id>.json")),
                ("observed_content_sha256", Carried("records/<id>.json")),
                ("created_at", Carried("records/<id>.json")),
            ],
        ),
        (
            "transcript_revisions",
            vec![
                ("id", Carried("transcripts/<id>.json")),
                ("recording_id", Carried("transcripts/<id>.json")),
                ("parent_id", Carried("transcripts/<id>.json")),
                ("origin", Carried("transcripts/<id>.json")),
                ("input_sha256", Carried("transcripts/<id>.json")),
                ("engine", Carried("transcripts/<id>.json")),
                ("model_id", Carried("transcripts/<id>.json")),
                ("model_version", Carried("transcripts/<id>.json")),
                ("locale", Carried("transcripts/<id>.json")),
                ("alignment_origin", Carried("transcripts/<id>.json")),
                ("status", Carried("transcripts/<id>.json")),
                ("normalization_profile", Carried("transcripts/<id>.json")),
                ("chunking_strategy", Carried("transcripts/<id>.json")),
                ("source_vendor", Carried("transcripts/<id>.json")),
                ("duration_ms", Carried("transcripts/<id>.json")),
                ("created_at", Carried("transcripts/<id>.json")),
                ("engine_backend", Carried("transcripts/<id>.json")),
                ("engine_params_json", Carried("transcripts/<id>.json")),
                (
                    "raw_provenance_cipher",
                    Excluded("encrypted with a machine-local key; undecryptable after restore"),
                ),
                ("raw_provenance_nonce", Excluded(LOCAL)),
                ("raw_provenance_scheme", Excluded(LOCAL)),
            ],
        ),
        (
            "transcript_segments",
            vec![
                ("id", Carried("transcripts/<id>.json")),
                ("transcript_revision_id", Carried("transcripts/<id>.json")),
                ("seq_index", Carried("transcripts/<id>.json")),
                ("start_ms", Carried("transcripts/<id>.json")),
                ("end_ms", Carried("transcripts/<id>.json")),
                ("speaker_id", Carried("transcripts/<id>.json")),
                ("text", Carried("transcripts/<id>.json")),
                ("confidence", Carried("transcripts/<id>.json")),
                ("confidence_scale", Carried("transcripts/<id>.json")),
                ("alignment_origin", Carried("transcripts/<id>.json")),
            ],
        ),
        (
            "transcript_words",
            vec![
                ("id", Carried("transcripts/<id>.json")),
                ("segment_id", Carried("transcripts/<id>.json")),
                ("seq_index", Carried("transcripts/<id>.json")),
                ("start_ms", Carried("transcripts/<id>.json")),
                ("end_ms", Carried("transcripts/<id>.json")),
                ("text", Carried("transcripts/<id>.json")),
                ("confidence", Carried("transcripts/<id>.json")),
                ("confidence_scale", Carried("transcripts/<id>.json")),
            ],
        ),
        (
            "transcript_annotations",
            vec![
                ("id", Carried("transcripts/<id>.json")),
                ("transcript_revision_id", Carried("transcripts/<id>.json")),
                ("segment_index", Carried("transcripts/<id>.json")),
                ("kind", Carried("transcripts/<id>.json")),
                ("reason", Carried("transcripts/<id>.json")),
                ("start_ms", Carried("transcripts/<id>.json")),
                ("end_ms", Carried("transcripts/<id>.json")),
            ],
        ),
        (
            "speakers",
            vec![
                ("id", Carried("annotations/<id>.json")),
                ("recording_id", Carried("annotations/<id>.json")),
                ("label", Carried("annotations/<id>.json")),
                ("name", Carried("annotations/<id>.json")),
                ("provenance", Carried("annotations/<id>.json")),
                ("user_confirmed", Carried("annotations/<id>.json")),
                ("person_id", Carried("annotations/<id>.json")),
                ("cluster_key", Excluded(DERIVED)),
                ("original_label", Excluded(LABELLING)),
                ("renamed_at", Excluded(LABELLING)),
                ("confirmed_at", Excluded(LABELLING)),
            ],
        ),
        (
            "ai_outputs",
            vec![
                ("template_id", Excluded(PIPELINE)),
                ("pipeline_version", Excluded(PIPELINE)),
                ("coverage_state", Excluded(PIPELINE)),
                ("coverage_json", Excluded(PIPELINE)),
                ("stale_at", Excluded(PIPELINE)),
                ("stale_reason", Excluded(PIPELINE)),
                ("superseded_by", Excluded(PIPELINE)),
                ("input_sha256", Excluded(PIPELINE)),
                ("output_sha256", Excluded(PIPELINE)),
                ("grounding_json", Excluded(PIPELINE)),
                ("rejected_json", Excluded(PIPELINE)),
                ("directives_json", Excluded(PIPELINE)),
                ("compatibility_mode", Excluded(PIPELINE)),
                ("id", Carried("outputs/<id>.json")),
                ("recording_id", Carried("outputs/<id>.json")),
                ("output_type", Carried("outputs/<id>.json")),
                ("transcript_revision_id", Carried("outputs/<id>.json")),
                ("template_version", Carried("outputs/<id>.json")),
                ("model_id", Carried("outputs/<id>.json")),
                ("prompt_version", Carried("outputs/<id>.json")),
                ("schema_version", Carried("outputs/<id>.json")),
                ("body_json", Carried("outputs/<id>.json")),
                ("status", Carried("outputs/<id>.json")),
                ("created_at", Carried("outputs/<id>.json")),
            ],
        ),
        (
            "output_claims",
            vec![
                ("id", Carried("outputs/<id>.json")),
                ("output_id", Carried("outputs/<id>.json")),
                ("item_path", Carried("outputs/<id>.json")),
                ("kind", Carried("outputs/<id>.json")),
                ("category", Carried("outputs/<id>.json")),
                ("claim_text", Carried("outputs/<id>.json")),
                ("state", Carried("outputs/<id>.json")),
                ("entailment", Carried("outputs/<id>.json")),
                ("review_reasons_json", Carried("outputs/<id>.json")),
                ("reviewer", Carried("outputs/<id>.json")),
                ("reviewed_at", Carried("outputs/<id>.json")),
                ("review_accepted", Carried("outputs/<id>.json")),
                ("review_note", Carried("outputs/<id>.json")),
            ],
        ),
        (
            "output_evidence",
            vec![
                ("id", Carried("outputs/<id>.json")),
                ("output_id", Carried("outputs/<id>.json")),
                ("claim_id", Carried("outputs/<id>.json")),
                ("position", Carried("outputs/<id>.json")),
                ("item_path", Carried("outputs/<id>.json")),
                ("cited_revision_id", Carried("outputs/<id>.json")),
                ("seq_index", Carried("outputs/<id>.json")),
                ("char_start", Carried("outputs/<id>.json")),
                ("char_end", Carried("outputs/<id>.json")),
                ("quote", Carried("outputs/<id>.json")),
                ("verification_status", Carried("outputs/<id>.json")),
                ("rejection_code", Carried("outputs/<id>.json")),
                ("matched_text", Carried("outputs/<id>.json")),
                ("alignment_origin", Carried("outputs/<id>.json")),
                ("seek_ms", Carried("outputs/<id>.json")),
            ],
        ),
        (
            "tombstones",
            vec![
                (
                    "id",
                    Excluded("a local deletion decision, not portable content"),
                ),
                ("entity_type", Excluded(LOCAL)),
                ("entity_id", Excluded(LOCAL)),
                ("reason", Excluded(LOCAL)),
                ("created_at", Excluded(LOCAL)),
                ("retention_until", Excluded(LOCAL)),
            ],
        ),
        (
            "devices",
            vec![
                ("id", Excluded(LOCAL)),
                ("vendor", Excluded(LOCAL)),
                ("model", Excluded(LOCAL)),
                ("serial_fingerprint", Excluded(SECRET)),
                ("discovered_name", Excluded(LOCAL)),
                ("observed_firmware", Excluded(LOCAL)),
                ("first_seen_at", Excluded(LOCAL)),
                ("last_seen_at", Excluded(LOCAL)),
                ("trust_state", Excluded(LOCAL)),
            ],
        ),
        (
            "device_capabilities",
            vec![
                ("device_id", Excluded(LOCAL)),
                ("host_os", Excluded(LOCAL)),
                ("transport", Excluded(LOCAL)),
                ("auth_mode", Excluded(LOCAL)),
                ("supported_features_json", Excluded(LOCAL)),
                ("evidence_level", Excluded(LOCAL)),
                ("validated_at", Excluded(LOCAL)),
                ("compatibility_revision", Excluded(LOCAL)),
            ],
        ),
        (
            "device_credential_refs",
            vec![
                ("device_id", Excluded(SECRET)),
                ("vault_service", Excluded(SECRET)),
                ("vault_account_reference", Excluded(SECRET)),
                ("auth_mode", Excluded(SECRET)),
                ("created_at", Excluded(SECRET)),
                ("last_verified_at", Excluded(SECRET)),
            ],
        ),
        (
            "device_transfers",
            vec![
                ("id", Excluded(TRANSIENT)),
                ("device_id", Excluded(TRANSIENT)),
                ("source_item_id", Excluded(TRANSIENT)),
                ("transport", Excluded(TRANSIENT)),
                ("native_format", Excluded(TRANSIENT)),
                ("expected_bytes", Excluded(TRANSIENT)),
                ("received_bytes", Excluded(TRANSIENT)),
                ("sha256", Excluded(TRANSIENT)),
                ("transfer_status", Excluded(TRANSIENT)),
                ("verification_status", Excluded(TRANSIENT)),
                ("started_at", Excluded(TRANSIENT)),
                ("ended_at", Excluded(TRANSIENT)),
            ],
        ),
        (
            "consent_grants",
            vec![
                (
                    "id",
                    Excluded("consent is granted on the machine that dispatches"),
                ),
                ("scope", Excluded(LOCAL)),
                ("privacy_policy_version", Excluded(LOCAL)),
                ("engine", Excluded(LOCAL)),
                ("model_or_provider", Excluded(LOCAL)),
                ("approved_at", Excluded(LOCAL)),
                ("revoked_at", Excluded(LOCAL)),
            ],
        ),
        (
            "app_preferences",
            vec![
                ("key", Excluded(LOCAL)),
                ("version", Excluded(LOCAL)),
                ("value_json", Excluded(LOCAL)),
            ],
        ),
        (
            "export_events",
            vec![
                (
                    "id",
                    Excluded("a local history of files written on this machine"),
                ),
                ("recording_id", Excluded(LOCAL)),
                ("format", Excluded(LOCAL)),
                ("destination_hint", Excluded(LOCAL)),
                ("created_at", Excluded(LOCAL)),
            ],
        ),
        (
            "export_derivatives",
            vec![
                ("id", Excluded("a derivative written outside the library")),
                ("recording_id", Excluded(LOCAL)),
                ("source_media_asset_id", Excluded(LOCAL)),
                ("kind", Excluded(LOCAL)),
                ("sha256", Excluded(LOCAL)),
                ("byte_length", Excluded(LOCAL)),
                ("stripped_metadata_json", Excluded(LOCAL)),
                ("created_at", Excluded(LOCAL)),
            ],
        ),
        (
            "archive_backups",
            vec![
                ("id", Excluded("diagnostics about archives, not content")),
                ("direction", Excluded(LOCAL)),
                ("manifest_schema_version", Excluded(LOCAL)),
                ("format_version", Excluded(LOCAL)),
                ("protection", Excluded(LOCAL)),
                ("location_hint", Excluded(LOCAL)),
                ("entry_count", Excluded(LOCAL)),
                ("total_byte_length", Excluded(LOCAL)),
                ("integrity_digest", Excluded(LOCAL)),
                ("verify_status", Excluded(LOCAL)),
                ("created_at", Excluded(LOCAL)),
            ],
        ),
        (
            "deletion_records",
            vec![
                ("id", Excluded(LOCAL)),
                ("recording_id", Excluded(LOCAL)),
                ("reason", Excluded(LOCAL)),
                ("state", Excluded(LOCAL)),
                ("disclosure_json", Excluded(LOCAL)),
                ("created_at", Excluded(LOCAL)),
                ("completed_at", Excluded(LOCAL)),
            ],
        ),
        (
            "deletion_pending_blobs",
            vec![
                ("deletion_id", Excluded(LOCAL)),
                ("relative_path", Excluded(LOCAL)),
                ("sha256", Excluded(LOCAL)),
                ("byte_length", Excluded(LOCAL)),
                ("removed_at", Excluded(LOCAL)),
            ],
        ),
        (
            "deletion_media_suppressions",
            vec![
                ("sha256", Excluded(LOCAL)),
                ("byte_length", Excluded(LOCAL)),
                ("deletion_id", Excluded(LOCAL)),
                ("created_at", Excluded(LOCAL)),
            ],
        ),
        (
            "deletion_source_suppressions",
            vec![
                ("source_id", Excluded(LOCAL)),
                ("external_id", Excluded(LOCAL)),
                ("deletion_id", Excluded(LOCAL)),
                ("created_at", Excluded(LOCAL)),
            ],
        ),
        (
            "jobs",
            vec![
                ("id", Excluded(TRANSIENT)),
                ("job_type", Excluded(TRANSIENT)),
                ("state", Excluded(TRANSIENT)),
                ("priority", Excluded(TRANSIENT)),
                ("retry_count", Excluded(TRANSIENT)),
                ("job_key", Excluded(TRANSIENT)),
                (
                    "config_snapshot_json",
                    Excluded("may embed a signed URL or an engine credential"),
                ),
                ("input_hash", Excluded(TRANSIENT)),
                ("created_at", Excluded(TRANSIENT)),
                ("started_at", Excluded(TRANSIENT)),
                ("finished_at", Excluded(TRANSIENT)),
                ("last_error_code", Excluded(TRANSIENT)),
            ],
        ),
        (
            "job_steps",
            vec![
                ("job_id", Excluded(TRANSIENT)),
                ("sequence", Excluded(TRANSIENT)),
                ("state", Excluded(TRANSIENT)),
                ("attempt", Excluded(TRANSIENT)),
                ("started_at", Excluded(TRANSIENT)),
                ("ended_at", Excluded(TRANSIENT)),
                ("progress_json", Excluded(TRANSIENT)),
            ],
        ),
        (
            "job_engine_state",
            vec![
                ("job_id", Excluded(TRANSIENT)),
                ("key_kind", Excluded(TRANSIENT)),
                ("key_canonical", Excluded(TRANSIENT)),
                ("is_reprocess", Excluded(TRANSIENT)),
                ("reprocess_nonce", Excluded(TRANSIENT)),
                ("reprocess_of", Excluded(TRANSIENT)),
                ("resource_class", Excluded(TRANSIENT)),
                ("wait_reason", Excluded(TRANSIENT)),
                ("cancel_requested_at", Excluded(TRANSIENT)),
                ("consent_id", Excluded(TRANSIENT)),
                ("recording_id", Excluded(TRANSIENT)),
                ("dispatched", Excluded(TRANSIENT)),
                ("updated_at", Excluded(TRANSIENT)),
            ],
        ),
        (
            "job_outputs",
            vec![
                ("id", Excluded(TRANSIENT)),
                ("job_id", Excluded(TRANSIENT)),
                ("output_kind", Excluded(TRANSIENT)),
                ("output_ref", Excluded(TRANSIENT)),
                ("output_sha256", Excluded(TRANSIENT)),
                ("byte_length", Excluded(TRANSIENT)),
                ("committed_at", Excluded(TRANSIENT)),
            ],
        ),
        (
            "job_transitions",
            vec![
                ("id", Excluded(TRANSIENT)),
                ("job_id", Excluded(TRANSIENT)),
                ("from_state", Excluded(TRANSIENT)),
                ("to_state", Excluded(TRANSIENT)),
                ("actor", Excluded(TRANSIENT)),
                ("reason", Excluded(TRANSIENT)),
                ("detail", Excluded(TRANSIENT)),
                ("occurred_at", Excluded(TRANSIENT)),
            ],
        ),
        (
            "job_checkpoints",
            vec![
                ("job_id", Excluded(TRANSIENT)),
                ("stage", Excluded(TRANSIENT)),
                ("stage_index", Excluded(TRANSIENT)),
                ("input_digest", Excluded(TRANSIENT)),
                ("output_ref", Excluded(TRANSIENT)),
                ("output_digest", Excluded(TRANSIENT)),
                ("attempt", Excluded(TRANSIENT)),
                ("prepared_at", Excluded(TRANSIENT)),
                ("committed_at", Excluded(TRANSIENT)),
            ],
        ),
        (
            "job_events",
            vec![
                ("seq", Excluded(TRANSIENT)),
                ("kind", Excluded(TRANSIENT)),
                ("job_id", Excluded(TRANSIENT)),
                ("recording_id", Excluded(TRANSIENT)),
                ("payload_json", Excluded(TRANSIENT)),
                ("occurred_at", Excluded(TRANSIENT)),
            ],
        ),
        (
            "event_log",
            vec![
                ("seq", Excluded(TRANSIENT)),
                ("kind", Excluded(TRANSIENT)),
                ("occurred_at", Excluded(TRANSIENT)),
                ("job_id", Excluded(TRANSIENT)),
                ("recording_id", Excluded(TRANSIENT)),
                ("payload_json", Excluded(TRANSIENT)),
            ],
        ),
        (
            "event_sequence",
            vec![("id", Excluded(TRANSIENT)), ("next", Excluded(TRANSIENT))],
        ),
        (
            "event_consumer_cursors",
            vec![
                ("consumer_id", Excluded(TRANSIENT)),
                ("last_seq", Excluded(TRANSIENT)),
                ("updated_at", Excluded(TRANSIENT)),
            ],
        ),
        (
            "activity_events",
            vec![
                (
                    "id",
                    Excluded("in-app notification history for this machine"),
                ),
                ("category", Excluded(LOCAL)),
                ("occurred_at", Excluded(LOCAL)),
                ("title", Excluded(LOCAL)),
                ("body", Excluded(LOCAL)),
                ("notification_delivered", Excluded(LOCAL)),
                ("notification_suppressed_reason", Excluded(LOCAL)),
                ("target_kind", Excluded(LOCAL)),
                ("target_id", Excluded(LOCAL)),
                ("read", Excluded(LOCAL)),
            ],
        ),
        (
            "recording_import_metadata",
            vec![
                (
                    "recording_id",
                    Excluded("the original file name is advisory local metadata (spec 13.4)"),
                ),
                ("original_file_name", Excluded(LOCAL)),
                ("imported_at", Excluded(LOCAL)),
            ],
        ),
        (
            "inference_attempts",
            vec![
                ("id", Excluded(BILLING)),
                ("job_id", Excluded(BILLING)),
                ("provider", Excluded(BILLING)),
                ("model_id", Excluded(BILLING)),
                ("request_family", Excluded(BILLING)),
                ("dispatch_state", Excluded(BILLING)),
                ("content_sha256", Excluded(BILLING)),
                ("route", Excluded(BILLING)),
                ("privacy_policy_snapshot", Excluded(BILLING)),
                ("consent_id", Excluded(BILLING)),
                ("estimated_units_json", Excluded(BILLING)),
                ("final_cost_json", Excluded(BILLING)),
                ("provider_request_id", Excluded(BILLING)),
                ("created_at", Excluded(BILLING)),
                ("dispatched_at", Excluded(BILLING)),
                ("responded_at", Excluded(BILLING)),
                ("settled_at", Excluded(BILLING)),
            ],
        ),
        (
            "usage_events",
            vec![
                ("id", Excluded(BILLING)),
                ("job_id", Excluded(BILLING)),
                ("attempt_id", Excluded(BILLING)),
                ("provider", Excluded(BILLING)),
                ("model_id", Excluded(BILLING)),
                ("usage_json", Excluded(BILLING)),
                ("estimated_cost", Excluded(BILLING)),
                ("actual_cost", Excluded(BILLING)),
                ("currency", Excluded(BILLING)),
                ("cost_confidence", Excluded(BILLING)),
                ("created_at", Excluded(BILLING)),
            ],
        ),
        (
            "import_journal",
            vec![
                ("id", Excluded(TRANSIENT)),
                ("state", Excluded(TRANSIENT)),
                ("job_id", Excluded(TRANSIENT)),
                ("recording_id", Excluded(TRANSIENT)),
                ("staging_path", Excluded(LOCAL)),
                ("target_relative_path", Excluded(LOCAL)),
                ("expected_sha256", Excluded(TRANSIENT)),
                ("expected_byte_length", Excluded(TRANSIENT)),
                ("quality_class", Excluded(TRANSIENT)),
                ("media_asset_id", Excluded(TRANSIENT)),
                ("failure_code", Excluded(TRANSIENT)),
                ("created_at", Excluded(TRANSIENT)),
                ("updated_at", Excluded(TRANSIENT)),
                ("hashed_at", Excluded(TRANSIENT)),
                ("flushed_at", Excluded(TRANSIENT)),
                ("intent_at", Excluded(TRANSIENT)),
                ("renamed_at", Excluded(TRANSIENT)),
                ("committed_at", Excluded(TRANSIENT)),
                ("abandoned_at", Excluded(TRANSIENT)),
            ],
        ),
        (
            "compatibility_evidence",
            vec![
                ("id", Excluded("hardware evidence for this machine")),
                ("profile_key", Excluded(LOCAL)),
                ("evidence_level", Excluded(LOCAL)),
                ("test_run_id", Excluded(LOCAL)),
                ("host_os", Excluded(LOCAL)),
                ("firmware", Excluded(LOCAL)),
                ("recorded_at", Excluded(LOCAL)),
                ("notes", Excluded(LOCAL)),
            ],
        ),
        (
            "automation_bridge_state",
            vec![
                ("singleton", Excluded(LOCAL)),
                ("enabled", Excluded(LOCAL)),
                ("owner_user_key", Excluded(LOCAL)),
                ("enabled_at", Excluded(LOCAL)),
                ("updated_at", Excluded(LOCAL)),
            ],
        ),
        (
            "conversation_messages",
            vec![
                ("id", Excluded(CONVERSATION)),
                ("conversation_id", Excluded(CONVERSATION)),
                ("seq_index", Excluded(CONVERSATION)),
                ("role", Excluded(CONVERSATION)),
                ("text", Excluded(CONVERSATION)),
                ("source_anchors_json", Excluded(CONVERSATION)),
                ("outcome", Excluded(CONVERSATION)),
                ("tool_run_id", Excluded(CONVERSATION)),
                ("created_at", Excluded(CONVERSATION)),
            ],
        ),
        (
            "device_firmware_observations",
            vec![
                ("id", Excluded(LOCAL)),
                ("device_id", Excluded(LOCAL)),
                ("firmware_fingerprint", Excluded(LOCAL)),
                ("observed_at", Excluded(LOCAL)),
                ("source", Excluded(LOCAL)),
                ("capability_profile", Excluded(LOCAL)),
                ("auth_mode", Excluded(LOCAL)),
            ],
        ),
        (
            "device_transfer_checkpoints",
            vec![
                ("transfer_id", Excluded(LOCAL)),
                ("device_recording_id", Excluded(LOCAL)),
                ("chunk_offset", Excluded(LOCAL)),
                ("verified_bytes", Excluded(LOCAL)),
                ("source_size_hint", Excluded(LOCAL)),
                ("staged_prefix_sha256", Excluded(LOCAL)),
                ("resume_capability", Excluded(LOCAL)),
                ("updated_at", Excluded(LOCAL)),
            ],
        ),
        (
            "diarization_runs",
            vec![
                ("id", Excluded(DERIVED)),
                ("recording_id", Excluded(DERIVED)),
                ("media_asset_id", Excluded(DERIVED)),
                ("provider_id", Excluded(DERIVED)),
                ("provider_version", Excluded(DERIVED)),
                ("privacy_class", Excluded(DERIVED)),
                ("status", Excluded(DERIVED)),
                ("input_sha256", Excluded(DERIVED)),
                ("detected_cluster_count", Excluded(DERIVED)),
                ("created_at", Excluded(DERIVED)),
                ("finished_at", Excluded(DERIVED)),
            ],
        ),
        (
            "model_capability_records",
            vec![
                ("model_id", Excluded(LOCAL)),
                ("task", Excluded(LOCAL)),
                ("request_family", Excluded(LOCAL)),
                ("evidence", Excluded(LOCAL)),
                ("fixture_id", Excluded(LOCAL)),
                ("validated_at", Excluded(LOCAL)),
                ("record_json", Excluded(LOCAL)),
                ("updated_at", Excluded(LOCAL)),
            ],
        ),
        (
            "model_task_defaults",
            vec![
                ("stage", Excluded(LOCAL)),
                ("engine", Excluded(LOCAL)),
                ("model_id", Excluded(LOCAL)),
                ("updated_at", Excluded(LOCAL)),
            ],
        ),
        (
            "output_dependencies",
            vec![
                ("output_id", Excluded(DERIVED)),
                ("transcript_revision_id", Excluded(DERIVED)),
                ("input_sha256", Excluded(DERIVED)),
            ],
        ),
        (
            "pipeline_stage_runs",
            vec![
                ("id", Excluded(BILLING)),
                ("output_id", Excluded(BILLING)),
                ("sequence", Excluded(BILLING)),
                ("stage", Excluded(BILLING)),
                ("stage_version", Excluded(BILLING)),
                ("pipeline_version", Excluded(BILLING)),
                ("transcript_revision_id", Excluded(BILLING)),
                ("input_sha256", Excluded(BILLING)),
                ("output_sha256", Excluded(BILLING)),
                ("engine", Excluded(BILLING)),
                ("model_id", Excluded(BILLING)),
                ("model_version", Excluded(BILLING)),
                ("compatibility_mode", Excluded(BILLING)),
                ("repair_attempts", Excluded(BILLING)),
                ("prompt_version", Excluded(BILLING)),
                ("prompt_envelope_version", Excluded(BILLING)),
                ("template_id", Excluded(BILLING)),
                ("template_version", Excluded(BILLING)),
                ("schema_version", Excluded(BILLING)),
                ("segment_first", Excluded(BILLING)),
                ("segment_last", Excluded(BILLING)),
                ("chunk_index", Excluded(BILLING)),
                ("section_response_json", Excluded(BILLING)),
                ("reused_from", Excluded(BILLING)),
                ("started_at", Excluded(BILLING)),
                ("ended_at", Excluded(BILLING)),
            ],
        ),
        (
            "recording_conversations",
            vec![
                ("id", Excluded(CONVERSATION)),
                ("recording_id", Excluded(CONVERSATION)),
                ("transcript_revision_id", Excluded(CONVERSATION)),
                ("created_at", Excluded(CONVERSATION)),
            ],
        ),
        (
            "recording_model_overrides",
            vec![
                ("recording_id", Excluded(LOCAL)),
                ("stage", Excluded(LOCAL)),
                ("engine", Excluded(LOCAL)),
                ("model_id", Excluded(LOCAL)),
                ("updated_at", Excluded(LOCAL)),
            ],
        ),
        (
            "route_privacy_policies",
            vec![
                ("model_id", Excluded(LOCAL)),
                ("downstream_provider", Excluded(LOCAL)),
                ("content_type", Excluded(LOCAL)),
                ("posture", Excluded(LOCAL)),
                ("retention_days", Excluded(LOCAL)),
                ("source_note", Excluded(LOCAL)),
                ("recorded_at", Excluded(LOCAL)),
            ],
        ),
        (
            "segment_speaker_assignments",
            vec![
                ("transcript_revision_id", Excluded(DERIVED)),
                ("seq_index", Excluded(DERIVED)),
                ("diarization_run_id", Excluded(DERIVED)),
                ("assignment", Excluded(DERIVED)),
                ("cluster_key", Excluded(DERIVED)),
                ("overlapping_keys_json", Excluded(DERIVED)),
                ("covered_ms", Excluded(DERIVED)),
            ],
        ),
        (
            "source_export_artifacts",
            vec![
                ("id", Excluded(LOCAL)),
                ("source_item_id", Excluded(LOCAL)),
                ("artifact_kind", Excluded(LOCAL)),
                ("source_vendor", Excluded(LOCAL)),
                ("content_sha256", Excluded(LOCAL)),
                ("byte_length", Excluded(LOCAL)),
                ("original_filename", Excluded(LOCAL)),
                ("imported_at", Excluded(LOCAL)),
                ("import_mode", Excluded(LOCAL)),
                ("created_at", Excluded(LOCAL)),
            ],
        ),
        (
            "speaker_turns",
            vec![
                ("id", Excluded(DERIVED)),
                ("diarization_run_id", Excluded(DERIVED)),
                ("seq_index", Excluded(DERIVED)),
                ("cluster_key", Excluded(DERIVED)),
                ("start_ms", Excluded(DERIVED)),
                ("end_ms", Excluded(DERIVED)),
                ("confidence", Excluded(DERIVED)),
                ("confidence_scale", Excluded(DERIVED)),
            ],
        ),
        (
            "tool_runs",
            vec![
                ("id", Excluded(BILLING)),
                ("recording_id", Excluded(BILLING)),
                ("transcript_revision_id", Excluded(BILLING)),
                ("tool_id", Excluded(BILLING)),
                ("tool_version", Excluded(BILLING)),
                ("engine", Excluded(BILLING)),
                ("model_id", Excluded(BILLING)),
                ("model_version", Excluded(BILLING)),
                ("request_family", Excluded(BILLING)),
                ("prompt_version", Excluded(BILLING)),
                ("schema_version", Excluded(BILLING)),
                ("input_sha256", Excluded(BILLING)),
                ("output_sha256", Excluded(BILLING)),
                ("status", Excluded(BILLING)),
                ("consent_id", Excluded(BILLING)),
                ("consent_policy_json", Excluded(BILLING)),
                ("estimated_cost", Excluded(BILLING)),
                ("actual_cost", Excluded(BILLING)),
                ("currency", Excluded(BILLING)),
                ("cost_confidence", Excluded(BILLING)),
                ("output_id", Excluded(BILLING)),
                ("created_at", Excluded(BILLING)),
                ("finished_at", Excluded(BILLING)),
            ],
        ),
        (
            "transcript_search",
            vec![
                ("text", Excluded(SEARCH_INDEX)),
                ("segment_id", Excluded(SEARCH_INDEX)),
                ("transcript_revision_id", Excluded(SEARCH_INDEX)),
                ("recording_id", Excluded(SEARCH_INDEX)),
                ("seq_index", Excluded(SEARCH_INDEX)),
            ],
        ),
        (
            "transcript_search_config",
            vec![("k", Excluded(SEARCH_INDEX)), ("v", Excluded(SEARCH_INDEX))],
        ),
        (
            "transcript_search_content",
            vec![
                ("id", Excluded(SEARCH_INDEX)),
                ("c0", Excluded(SEARCH_INDEX)),
                ("c1", Excluded(SEARCH_INDEX)),
                ("c2", Excluded(SEARCH_INDEX)),
                ("c3", Excluded(SEARCH_INDEX)),
                ("c4", Excluded(SEARCH_INDEX)),
            ],
        ),
        (
            "transcript_search_data",
            vec![
                ("id", Excluded(SEARCH_INDEX)),
                ("block", Excluded(SEARCH_INDEX)),
            ],
        ),
        (
            "transcript_search_docsize",
            vec![
                ("id", Excluded(SEARCH_INDEX)),
                ("sz", Excluded(SEARCH_INDEX)),
            ],
        ),
        (
            "transcript_search_idx",
            vec![
                ("segid", Excluded(SEARCH_INDEX)),
                ("term", Excluded(SEARCH_INDEX)),
                ("pgno", Excluded(SEARCH_INDEX)),
            ],
        ),
        (
            "transcript_tools",
            vec![
                ("tool_id", Carried("tools/<id>.json")),
                ("version", Carried("tools/<id>.json")),
                ("display_name", Carried("tools/<id>.json")),
                ("kind", Carried("tools/<id>.json")),
                ("prompt_version", Carried("tools/<id>.json")),
                ("output_schema_version", Carried("tools/<id>.json")),
                ("definition_sha256", Carried("tools/<id>.json")),
                ("origin", Carried("tools/<id>.json")),
                ("optional", Carried("tools/<id>.json")),
                ("created_at", Carried("tools/<id>.json")),
            ],
        ),
        (
            "watched_folder_items",
            vec![
                ("id", Excluded(LOCAL)),
                ("folder_id", Excluded(LOCAL)),
                ("origin_path", Excluded(LOCAL)),
                ("stable_file_id", Excluded(LOCAL)),
                ("size_bytes", Excluded(LOCAL)),
                ("mtime_ms", Excluded(LOCAL)),
                ("content_sha256", Excluded(LOCAL)),
                ("state", Excluded(LOCAL)),
                ("source_item_id", Excluded(LOCAL)),
                ("first_seen_at", Excluded(LOCAL)),
                ("last_checked_at", Excluded(LOCAL)),
                ("unavailable_since", Excluded(LOCAL)),
            ],
        ),
        (
            "watched_folders",
            vec![
                ("id", Excluded(LOCAL)),
                ("root_path", Excluded(LOCAL)),
                ("recursive", Excluded(LOCAL)),
                ("rule", Excluded(LOCAL)),
                ("quiescence_ms", Excluded(LOCAL)),
                ("status", Excluded(LOCAL)),
                ("last_inventory_at", Excluded(LOCAL)),
                ("created_at", Excluded(LOCAL)),
            ],
        ),
        (
            "people",
            vec![
                ("id", Carried("people/<id>.json")),
                ("display_name", Carried("people/<id>.json")),
                ("aliases_json", Carried("people/<id>.json")),
                ("email", Carried("people/<id>.json")),
                ("organisation", Carried("people/<id>.json")),
                ("status", Carried("people/<id>.json")),
                ("created_at", Carried("people/<id>.json")),
                ("updated_at", Carried("people/<id>.json")),
            ],
        ),
        (
            "person_voice_enrollments",
            vec![
                ("person_id", Carried("voiceprofiles/<person id>.json")),
                ("consent_version", Carried("voiceprofiles/<person id>.json")),
                ("consent_note", Carried("voiceprofiles/<person id>.json")),
                (
                    "retention_months",
                    Carried("voiceprofiles/<person id>.json"),
                ),
                ("last_matched_at", Carried("voiceprofiles/<person id>.json")),
                ("status", Carried("voiceprofiles/<person id>.json")),
                ("expired_at", Carried("voiceprofiles/<person id>.json")),
                ("created_at", Carried("voiceprofiles/<person id>.json")),
            ],
        ),
        (
            "voice_exemplars",
            vec![
                ("id", Carried("voiceprofiles/<person id>.json")),
                ("person_id", Carried("voiceprofiles/<person id>.json")),
                ("recording_id", Carried("voiceprofiles/<person id>.json")),
                ("diarization_run_id", Excluded(PIPELINE)),
                ("cluster_key", Carried("voiceprofiles/<person id>.json")),
                ("source_start_ms", Carried("voiceprofiles/<person id>.json")),
                ("source_end_ms", Carried("voiceprofiles/<person id>.json")),
                ("speech_ms", Carried("voiceprofiles/<person id>.json")),
                ("model_id", Carried("voiceprofiles/<person id>.json")),
                ("model_version", Carried("voiceprofiles/<person id>.json")),
                ("model_sha256", Carried("voiceprofiles/<person id>.json")),
                ("embedding_dim", Carried("voiceprofiles/<person id>.json")),
                (
                    "embedding_cipher",
                    Carried("voiceprofiles/<person id>.json"),
                ),
                ("embedding_nonce", Carried("voiceprofiles/<person id>.json")),
                ("created_at", Carried("voiceprofiles/<person id>.json")),
            ],
        ),
        (
            "speaker_suggestions",
            vec![
                ("id", Excluded(SUGGESTION)),
                ("recording_id", Excluded(SUGGESTION)),
                ("diarization_run_id", Excluded(SUGGESTION)),
                ("cluster_key", Excluded(SUGGESTION)),
                ("person_id", Excluded(SUGGESTION)),
                ("score", Excluded(SUGGESTION)),
                ("score_kind", Excluded(SUGGESTION)),
                ("band", Excluded(SUGGESTION)),
                ("state", Excluded(SUGGESTION)),
                ("corrected_person_id", Excluded(SUGGESTION)),
                ("decided_at", Excluded(SUGGESTION)),
                ("model_id", Excluded(SUGGESTION)),
                ("model_version", Excluded(SUGGESTION)),
                ("created_at", Excluded(SUGGESTION)),
            ],
        ),
        (
            "speaker_match_cluster_embeddings",
            vec![
                ("job_id", Excluded(MATCH_EMBEDDING)),
                ("diarization_run_id", Excluded(MATCH_EMBEDDING)),
                ("cluster_key", Excluded(MATCH_EMBEDDING)),
                ("speech_ms", Excluded(MATCH_EMBEDDING)),
                ("model_id", Excluded(MATCH_EMBEDDING)),
                ("model_version", Excluded(MATCH_EMBEDDING)),
                ("embedding_dim", Excluded(MATCH_EMBEDDING)),
                ("embedding_cipher", Excluded(MATCH_EMBEDDING)),
                ("embedding_nonce", Excluded(MATCH_EMBEDDING)),
                ("created_at", Excluded(MATCH_EMBEDDING)),
            ],
        ),
        (
            "speaker_match_reports",
            vec![
                ("job_id", Excluded(MATCH_REPORT)),
                ("diarization_run_id", Excluded(MATCH_REPORT)),
                ("cluster_key", Excluded(MATCH_REPORT)),
                ("report_json", Excluded(MATCH_REPORT)),
                ("created_at", Excluded(MATCH_REPORT)),
            ],
        ),
        (
            "voice_profile_events",
            vec![
                ("id", Excluded(CONSENT_JOURNAL)),
                ("person_id", Excluded(CONSENT_JOURNAL)),
                ("kind", Excluded(CONSENT_JOURNAL)),
                ("detail_json", Excluded(CONSENT_JOURNAL)),
                ("created_at", Excluded(CONSENT_JOURNAL)),
            ],
        ),
        (
            "voice_retention_sweeps",
            vec![
                ("id", Excluded(TRANSIENT)),
                ("planned_at", Excluded(TRANSIENT)),
                ("state", Excluded(TRANSIENT)),
                ("expired_count", Excluded(TRANSIENT)),
                ("purged_count", Excluded(TRANSIENT)),
                ("completed_at", Excluded(TRANSIENT)),
            ],
        ),
    ]
}

#[tokio::test]
async fn every_column_has_an_explicit_archive_decision() {
    let dir = tempfile::tempdir().unwrap();
    let (db, _) = library::empty_library(&dir.path().join("library")).await;

    let tables: Vec<String> = sqlx::query(
        "SELECT name FROM sqlite_master
          WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> 'schema_migrations'
          ORDER BY name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap()
    .iter()
    .map(|row| row.get::<String, _>("name"))
    .collect();

    let decided = decisions();
    let decided_tables: BTreeSet<&str> = decided.iter().map(|(table, _)| *table).collect();
    let actual_tables: BTreeSet<&str> = tables.iter().map(String::as_str).collect();

    let undecided: Vec<&&str> = actual_tables.difference(&decided_tables).collect();
    assert!(
        undecided.is_empty(),
        "these tables have no archive decision; add them to tests/integration/tests/j2_archive_allowlist.rs: {undecided:?}"
    );
    let stale: Vec<&&str> = decided_tables.difference(&actual_tables).collect();
    assert!(
        stale.is_empty(),
        "these decisions name tables that no longer exist: {stale:?}"
    );

    for (table, columns) in &decided {
        let actual: BTreeSet<String> =
            sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA table_info({table})")))
                .fetch_all(db.pool())
                .await
                .unwrap()
                .iter()
                .map(|row| row.get::<String, _>("name"))
                .collect();
        let listed: BTreeSet<String> = columns.iter().map(|(name, _)| (*name).to_owned()).collect();

        let missing: Vec<&String> = actual.difference(&listed).collect();
        assert!(
            missing.is_empty(),
            "{table} has columns with no archive decision: {missing:?}"
        );
        let removed: Vec<&String> = listed.difference(&actual).collect();
        assert!(
            removed.is_empty(),
            "{table} has decisions for columns that no longer exist: {removed:?}"
        );

        for (name, decision) in columns {
            if let Excluded(reason) = decision {
                assert!(
                    reason.len() > 20,
                    "{table}.{name} is excluded without a real reason"
                );
            }
        }
    }
    db.close().await;
}

#[test]
fn the_carried_documents_are_exactly_the_spec_namespaces() {
    let mut documents: BTreeSet<&str> = BTreeSet::new();
    for (_, columns) in decisions() {
        for (_, decision) in columns {
            if let Carried(document) = decision {
                documents.insert(document);
            }
        }
    }
    let namespaces: BTreeSet<&str> = documents
        .iter()
        .map(|document| document.split('/').next().unwrap())
        .collect();
    assert_eq!(
        namespaces,
        BTreeSet::from([
            "records",
            "annotations",
            "transcripts",
            "outputs",
            "sources",
            "tools",
            // VP-077: the roster is ordinary library data and travels like the
            // rest of it.
            "people",
            // VP-078: biometric material has a namespace of its own, writable
            // only under `Protection::Encrypted`.
            "voiceprofiles",
        ])
    );
}
