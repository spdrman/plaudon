-- Desktop-local port schema (EPIC E).
--
-- This file is NOT part of `crates/persistence`. It lives in its own database
-- file (`desktop-local.sqlite3`), is applied by this crate's own tiny runner
-- and is tracked in `desktop_schema_version` — never in `schema_migrations`,
-- which belongs to EPIC B's tamper-detecting MigrationRunner. The number 9001
-- is deliberately far above the 0001-0199 range reserved for that crate so the
-- two histories can never be confused.
--
-- Every table here is superseded once EPIC B's persistence layer and EPIC C's
-- job engine are wired in behind the same `ports.rs` traits.

CREATE TABLE IF NOT EXISTS recordings (
    id                     TEXT    PRIMARY KEY NOT NULL,
    title                  TEXT    NOT NULL,
    recorded_at            INTEGER,
    imported_at            INTEGER NOT NULL,
    duration_ms            INTEGER,
    source_kind            TEXT    NOT NULL,
    source_label           TEXT    NOT NULL,
    import_status          TEXT    NOT NULL,
    transcription_status   TEXT    NOT NULL,
    processing_engine      TEXT,
    last_error_json        TEXT,
    tags_json              TEXT    NOT NULL DEFAULT '[]',
    original_file_name     TEXT    NOT NULL,
    current_revision_id    TEXT,
    deleted_at             INTEGER
);

CREATE INDEX IF NOT EXISTS ix_recordings_imported_at ON recordings (imported_at);
CREATE INDEX IF NOT EXISTS ix_recordings_recorded_at ON recordings (recorded_at);

CREATE TABLE IF NOT EXISTS media_assets (
    id                 TEXT    PRIMARY KEY NOT NULL,
    sha256             TEXT    NOT NULL,
    byte_length        INTEGER NOT NULL,
    relative_path      TEXT    NOT NULL,
    observed_codec     TEXT    NOT NULL,
    sample_rate_hz     INTEGER,
    channels           INTEGER,
    duration_ms        INTEGER,
    verification_state TEXT    NOT NULL,
    verified_at        INTEGER,
    created_at         INTEGER NOT NULL
);

-- Spec 13.3A: equivalent bytes are one blob, even across logical meetings.
CREATE UNIQUE INDEX IF NOT EXISTS ux_media_sha ON media_assets (sha256, byte_length);

CREATE TABLE IF NOT EXISTS recording_media (
    recording_id   TEXT    NOT NULL REFERENCES recordings (id) ON DELETE RESTRICT,
    media_asset_id TEXT    NOT NULL REFERENCES media_assets (id) ON DELETE RESTRICT,
    quality_class  TEXT    NOT NULL,
    is_preferred   INTEGER NOT NULL DEFAULT 0,
    attached_at    INTEGER NOT NULL,
    PRIMARY KEY (recording_id, media_asset_id)
);

CREATE TABLE IF NOT EXISTS transcript_revisions (
    id               TEXT    PRIMARY KEY NOT NULL,
    recording_id     TEXT    NOT NULL REFERENCES recordings (id) ON DELETE RESTRICT,
    parent_id        TEXT    REFERENCES transcript_revisions (id) ON DELETE RESTRICT,
    kind             TEXT    NOT NULL,
    created_at       INTEGER NOT NULL,
    engine_id        TEXT,
    model_id         TEXT,
    alignment_origin TEXT    NOT NULL,
    segment_count    INTEGER NOT NULL,
    superseded       INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS ix_revisions_recording ON transcript_revisions (recording_id, created_at);

CREATE TABLE IF NOT EXISTS transcript_segments (
    revision_id          TEXT    NOT NULL REFERENCES transcript_revisions (id) ON DELETE RESTRICT,
    seq_index            INTEGER NOT NULL,
    text                 TEXT    NOT NULL,
    start_ms             INTEGER,
    end_ms               INTEGER,
    alignment_origin     TEXT    NOT NULL,
    confidence_engine_id TEXT,
    confidence_value     REAL,
    speaker_label        TEXT,
    annotations_json     TEXT    NOT NULL DEFAULT '[]',
    PRIMARY KEY (revision_id, seq_index)
);

CREATE INDEX IF NOT EXISTS ix_segments_revision_time
    ON transcript_segments (revision_id, start_ms);

CREATE TABLE IF NOT EXISTS jobs (
    id                            TEXT    PRIMARY KEY NOT NULL,
    kind                          TEXT    NOT NULL,
    state                         TEXT    NOT NULL,
    stage                         TEXT,
    progress                      REAL,
    created_at                    INTEGER NOT NULL,
    updated_at                    INTEGER NOT NULL,
    started_at                    INTEGER,
    finished_at                   INTEGER,
    recording_id                  TEXT    REFERENCES recordings (id) ON DELETE RESTRICT,
    label                         TEXT    NOT NULL,
    error_json                    TEXT,
    cost_acknowledgement_required INTEGER NOT NULL DEFAULT 0,
    provider_request_id           TEXT,
    output_link_kind              TEXT,
    output_link_id                TEXT,
    config_snapshot_json          TEXT
);

CREATE INDEX IF NOT EXISTS ix_jobs_created_at ON jobs (created_at);
CREATE INDEX IF NOT EXISTS ix_jobs_state ON jobs (state);

CREATE TABLE IF NOT EXISTS activity_events (
    id                             TEXT    PRIMARY KEY NOT NULL,
    category                       TEXT    NOT NULL,
    occurred_at                    INTEGER NOT NULL,
    title                          TEXT    NOT NULL,
    body                           TEXT    NOT NULL,
    notification_delivered         INTEGER NOT NULL DEFAULT 0,
    notification_suppressed_reason TEXT,
    target_kind                    TEXT,
    target_id                      TEXT,
    read                           INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS ix_activity_occurred_at ON activity_events (occurred_at);

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT    PRIMARY KEY NOT NULL,
    value_json TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
);

-- The monotonic UI event sequence (spec 14.5, 15.3). Rows are appended only
-- after the transaction that caused them has committed.
CREATE TABLE IF NOT EXISTS event_log (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         TEXT    NOT NULL,
    occurred_at  INTEGER NOT NULL,
    job_id       TEXT,
    recording_id TEXT,
    payload_json TEXT    NOT NULL
);
