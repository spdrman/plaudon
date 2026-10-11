-- Remaining spec 13.2 core tables plus the spec 13.3A import journal.
--
-- Deletions never cascade into media_assets or transcript_revisions: those
-- references are RESTRICT, and user-visible deletion is expressed by a
-- tombstone row created in the same transaction.

CREATE TABLE devices (
    id                 TEXT    PRIMARY KEY NOT NULL,
    vendor             TEXT    NOT NULL,
    model              TEXT    NOT NULL,
    serial_fingerprint TEXT,
    discovered_name    TEXT,
    observed_firmware  TEXT,
    first_seen_at      INTEGER NOT NULL,
    last_seen_at       INTEGER NOT NULL,
    trust_state        TEXT    NOT NULL DEFAULT 'unverified'
) STRICT;

CREATE TABLE device_capabilities (
    device_id              TEXT    NOT NULL REFERENCES devices(id) ON DELETE RESTRICT,
    host_os                TEXT    NOT NULL,
    transport              TEXT    NOT NULL,
    auth_mode              TEXT    NOT NULL,
    supported_features_json TEXT   NOT NULL DEFAULT '{}',
    evidence_level         TEXT    NOT NULL,
    validated_at           INTEGER,
    compatibility_revision TEXT    NOT NULL,
    PRIMARY KEY (device_id, host_os, transport, auth_mode, compatibility_revision)
) STRICT;

-- Opaque vault handles only. Never token bytes, never a private key.
CREATE TABLE device_credential_refs (
    device_id                  TEXT    PRIMARY KEY NOT NULL REFERENCES devices(id) ON DELETE RESTRICT,
    vault_service              TEXT    NOT NULL,
    vault_account_reference    TEXT    NOT NULL,
    auth_mode                  TEXT    NOT NULL,
    created_at                 INTEGER NOT NULL,
    last_verified_at           INTEGER
) STRICT;

CREATE TABLE device_transfers (
    id                  TEXT    PRIMARY KEY NOT NULL,
    device_id           TEXT    NOT NULL REFERENCES devices(id) ON DELETE RESTRICT,
    source_item_id      TEXT    REFERENCES source_items(id) ON DELETE RESTRICT,
    transport           TEXT    NOT NULL,
    native_format       TEXT,
    expected_bytes      INTEGER CHECK (expected_bytes IS NULL OR expected_bytes >= 0),
    received_bytes      INTEGER NOT NULL DEFAULT 0 CHECK (received_bytes >= 0),
    sha256              TEXT    CHECK (sha256 IS NULL OR length(sha256) = 64),
    transfer_status     TEXT    NOT NULL,
    verification_status TEXT    NOT NULL DEFAULT 'unverified',
    started_at          INTEGER NOT NULL,
    ended_at            INTEGER,
    -- Spec 13.2: an incomplete transfer never becomes a committed audio asset.
    CHECK (verification_status <> 'verified' OR transfer_status = 'complete')
) STRICT;

CREATE TABLE transcript_revisions (
    id                    TEXT    PRIMARY KEY NOT NULL,
    recording_id          TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    parent_id             TEXT    REFERENCES transcript_revisions(id) ON DELETE RESTRICT,
    origin                TEXT    NOT NULL CHECK (origin IN ('machine','human_edit','derived_alignment','overlap_reconciliation')),
    input_sha256          TEXT    NOT NULL CHECK (length(input_sha256) = 64),
    engine                TEXT    NOT NULL,
    model_id              TEXT    NOT NULL,
    model_version         TEXT,
    locale                TEXT,
    alignment_origin      TEXT    NOT NULL CHECK (alignment_origin IN ('native','derived','none')),
    status                TEXT    NOT NULL CHECK (status IN ('complete','needs_review','partial')),
    normalization_profile TEXT    NOT NULL,
    chunking_strategy     TEXT    NOT NULL,
    duration_ms           INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    created_at            INTEGER NOT NULL
) STRICT;

CREATE INDEX ix_transcript_revisions_recording ON transcript_revisions(recording_id);

CREATE TABLE transcript_segments (
    id                     TEXT    PRIMARY KEY NOT NULL,
    transcript_revision_id TEXT    NOT NULL REFERENCES transcript_revisions(id) ON DELETE CASCADE,
    seq_index              INTEGER NOT NULL CHECK (seq_index >= 0),
    start_ms               INTEGER CHECK (start_ms IS NULL OR start_ms >= 0),
    end_ms                 INTEGER CHECK (end_ms IS NULL OR end_ms >= 0),
    speaker_id             TEXT    REFERENCES speakers(id) ON DELETE SET NULL,
    text                   TEXT    NOT NULL,
    confidence             REAL,
    confidence_scale       TEXT,
    -- Spec 8.6A: a revision without alignment never carries a seek time. The
    -- origin is denormalized here so the constraint is enforced by the engine.
    alignment_origin       TEXT    NOT NULL CHECK (alignment_origin IN ('native','derived','none')),
    CHECK (alignment_origin <> 'none' OR (start_ms IS NULL AND end_ms IS NULL)),
    CHECK (alignment_origin =  'none' OR (start_ms IS NOT NULL AND end_ms IS NOT NULL)),
    CHECK (start_ms IS NULL OR end_ms IS NULL OR end_ms >= start_ms),
    CHECK (confidence IS NULL OR confidence_scale IS NOT NULL),
    UNIQUE (transcript_revision_id, seq_index)
) STRICT;

CREATE INDEX ix_segments_revision_time ON transcript_segments(transcript_revision_id, start_ms);

CREATE TABLE speakers (
    id            TEXT    PRIMARY KEY NOT NULL,
    recording_id  TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    label         TEXT    NOT NULL,
    name          TEXT,
    provenance    TEXT    NOT NULL CHECK (provenance IN ('inferred','user_confirmed')),
    user_confirmed INTEGER NOT NULL DEFAULT 0 CHECK (user_confirmed IN (0,1)),
    -- Spec 13.2: an inferred identity cannot masquerade as verified.
    CHECK (user_confirmed = 0 OR provenance = 'user_confirmed')
) STRICT;

CREATE TABLE ai_outputs (
    id                     TEXT    PRIMARY KEY NOT NULL,
    recording_id           TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    output_type            TEXT    NOT NULL,
    transcript_revision_id TEXT    NOT NULL REFERENCES transcript_revisions(id) ON DELETE RESTRICT,
    template_version       TEXT    NOT NULL,
    model_id               TEXT    NOT NULL,
    prompt_version         TEXT    NOT NULL,
    schema_version         TEXT    NOT NULL,
    body_json              TEXT    NOT NULL,
    status                 TEXT    NOT NULL,
    created_at             INTEGER NOT NULL
) STRICT;

CREATE TABLE output_evidence (
    id                  TEXT    PRIMARY KEY NOT NULL,
    output_id           TEXT    NOT NULL REFERENCES ai_outputs(id) ON DELETE CASCADE,
    item_path           TEXT    NOT NULL,
    segment_id          TEXT    NOT NULL REFERENCES transcript_segments(id) ON DELETE RESTRICT,
    quote               TEXT    NOT NULL,
    verification_status TEXT    NOT NULL CHECK (verification_status IN ('source_linked','human_confirmed','unverified'))
) STRICT;

CREATE TABLE jobs (
    id                    TEXT    PRIMARY KEY NOT NULL,
    job_type              TEXT    NOT NULL,
    state                 TEXT    NOT NULL,
    priority              INTEGER NOT NULL DEFAULT 0,
    retry_count           INTEGER NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
    job_key               TEXT,
    config_snapshot_json  TEXT    NOT NULL DEFAULT '{}',
    input_hash            TEXT,
    created_at            INTEGER NOT NULL,
    started_at            INTEGER,
    finished_at           INTEGER,
    last_error_code       TEXT
) STRICT;

CREATE UNIQUE INDEX ux_jobs_key ON jobs(job_key) WHERE job_key IS NOT NULL;

CREATE TABLE job_steps (
    job_id        TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    sequence      INTEGER NOT NULL CHECK (sequence >= 0),
    state         TEXT    NOT NULL,
    attempt       INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    started_at    INTEGER,
    ended_at      INTEGER,
    progress_json TEXT    NOT NULL DEFAULT '{}',
    PRIMARY KEY (job_id, sequence)
) STRICT;

-- Spec 10.7: the attempt row is written before dispatch so a lost response is
-- still accounted for.
CREATE TABLE inference_attempts (
    id             TEXT    PRIMARY KEY NOT NULL,
    job_id         TEXT    NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT,
    provider       TEXT    NOT NULL,
    model_id       TEXT    NOT NULL,
    request_family TEXT    NOT NULL,
    dispatch_state TEXT    NOT NULL CHECK (dispatch_state IN ('not_sent','sending','accepted_by_provider','outcome_unknown','completed','failed_before_send')),
    created_at     INTEGER NOT NULL,
    settled_at     INTEGER
) STRICT;

CREATE UNIQUE INDEX ux_inference_attempt ON inference_attempts(id);

CREATE TABLE usage_events (
    id             TEXT    PRIMARY KEY NOT NULL,
    job_id         TEXT    NOT NULL REFERENCES jobs(id) ON DELETE RESTRICT,
    attempt_id     TEXT    REFERENCES inference_attempts(id) ON DELETE RESTRICT,
    provider       TEXT    NOT NULL,
    model_id       TEXT    NOT NULL,
    usage_json     TEXT    NOT NULL DEFAULT '{}',
    estimated_cost TEXT,
    actual_cost    TEXT,
    currency       TEXT,
    -- Spec 13.2: unknown is not zero.
    cost_confidence TEXT   NOT NULL CHECK (cost_confidence IN ('unknown','estimated','finalized')),
    created_at     INTEGER NOT NULL,
    CHECK (cost_confidence <> 'finalized' OR actual_cost IS NOT NULL),
    CHECK (cost_confidence <> 'unknown'   OR (estimated_cost IS NULL AND actual_cost IS NULL))
) STRICT;

CREATE TABLE consent_grants (
    id                     TEXT    PRIMARY KEY NOT NULL,
    scope                  TEXT    NOT NULL,
    privacy_policy_version TEXT    NOT NULL,
    engine                 TEXT    NOT NULL,
    model_or_provider      TEXT    NOT NULL,
    approved_at            INTEGER NOT NULL,
    revoked_at             INTEGER
) STRICT;

CREATE TABLE app_preferences (
    key        TEXT    PRIMARY KEY NOT NULL,
    version    INTEGER NOT NULL DEFAULT 1,
    value_json TEXT    NOT NULL
) STRICT;

CREATE TABLE export_events (
    id               TEXT    PRIMARY KEY NOT NULL,
    recording_id     TEXT    REFERENCES recordings(id) ON DELETE RESTRICT,
    format           TEXT    NOT NULL,
    destination_hint TEXT,
    created_at       INTEGER NOT NULL
) STRICT;

-- Spec 13.3A durable import order. The journal is the only thing that can
-- reconcile a filesystem rename with a SQLite commit.
CREATE TABLE import_journal (
    id                 TEXT    PRIMARY KEY NOT NULL,
    state              TEXT    NOT NULL CHECK (state IN ('prepared','staged','renamed','committed','abandoned','quarantined')),
    job_id             TEXT    REFERENCES jobs(id) ON DELETE RESTRICT,
    staging_path       TEXT    NOT NULL,
    target_relative_path TEXT  NOT NULL,
    expected_sha256    TEXT    NOT NULL CHECK (length(expected_sha256) = 64),
    expected_byte_length INTEGER NOT NULL CHECK (expected_byte_length >= 0),
    quality_class      TEXT    NOT NULL,
    media_asset_id     TEXT    REFERENCES media_assets(id) ON DELETE RESTRICT,
    failure_code       TEXT,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    CHECK (state <> 'committed' OR media_asset_id IS NOT NULL)
) STRICT;

CREATE INDEX ix_import_journal_state ON import_journal(state);

-- Spec 2.5B: compatibility claims are evidence-backed rows, not prose.
CREATE TABLE compatibility_evidence (
    id              TEXT    PRIMARY KEY NOT NULL,
    profile_key     TEXT    NOT NULL,
    evidence_level  TEXT    NOT NULL,
    test_run_id     TEXT    NOT NULL,
    host_os         TEXT    NOT NULL,
    firmware        TEXT,
    recorded_at     INTEGER NOT NULL,
    notes           TEXT
) STRICT;

CREATE INDEX ix_compatibility_profile ON compatibility_evidence(profile_key);
