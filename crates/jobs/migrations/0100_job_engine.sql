-- Job engine schema (EPIC C; SPEC v1.3 §13.2, §13.3A, §14).
--
-- Migration range 0100-0199 is owned by the job engine. This file is the single
-- source of truth; `crates/persistence/migrations/0100_job_engine.sql` is a
-- symlink to it so the application migrator and the standalone `plaudon-jobs`
-- test harness apply byte-identical SQL.
--
-- `jobs`, `job_steps`, `import_journal` and `inference_attempts` are core
-- tables declared in §13.2/§13.3A/§10.7. They are repeated here with
-- IF NOT EXISTS so that (a) the job engine is testable without the whole
-- persistence stack and (b) migration ordering relative to `crates/persistence`
-- 0001-0099 is irrelevant. `JobStore::verify_schema` fails loudly if an
-- existing table is missing a required column, so divergence can never pass
-- silently.

PRAGMA foreign_keys = ON;

-- ---------------------------------------------------------------------------
-- Core job record (§13.2)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS jobs (
    id                  TEXT    PRIMARY KEY NOT NULL,
    type                TEXT    NOT NULL,
    state               TEXT    NOT NULL,
    priority            INTEGER NOT NULL DEFAULT 0,
    retry_count         INTEGER NOT NULL DEFAULT 0,
    job_key             TEXT    UNIQUE,
    config_snapshot_json TEXT   NOT NULL DEFAULT '{}',
    input_hash          TEXT,
    created_at          INTEGER NOT NULL,
    started_at          INTEGER,
    finished_at         INTEGER,
    last_error_code     TEXT
);

CREATE INDEX IF NOT EXISTS ix_jobs_state ON jobs(state);
CREATE INDEX IF NOT EXISTS ix_jobs_created_at ON jobs(created_at);

CREATE TABLE IF NOT EXISTS job_steps (
    job_id       TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    sequence     INTEGER NOT NULL,
    state        TEXT    NOT NULL,
    attempt      INTEGER NOT NULL DEFAULT 0,
    started_at   INTEGER,
    ended_at     INTEGER,
    progress_json TEXT,
    PRIMARY KEY (job_id, sequence)
);

-- ---------------------------------------------------------------------------
-- Durable import journal (§13.3A "durable import order", six steps)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS import_journal (
    id              TEXT    PRIMARY KEY NOT NULL,
    job_id          TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    recording_id    TEXT,
    -- Step 1: app-owned private staging file.
    staging_path    TEXT    NOT NULL,
    -- Step 4 destination inside the content-addressed archive (§13.4).
    target_path     TEXT    NOT NULL,
    expected_sha256 TEXT,
    byte_length     INTEGER,
    -- staged | hashed | flushed | intent_recorded | renamed | committed | abandoned
    step            TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    hashed_at       INTEGER,
    flushed_at      INTEGER,
    intent_at       INTEGER,
    renamed_at      INTEGER,
    committed_at    INTEGER,
    abandoned_at    INTEGER
);

CREATE INDEX IF NOT EXISTS ix_import_journal_step ON import_journal(step);
CREATE INDEX IF NOT EXISTS ix_import_journal_job ON import_journal(job_id);

-- ---------------------------------------------------------------------------
-- Cloud request ledger (§10.7): written BEFORE any user content is sent.
-- Never contains the API secret or a signed URL.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS inference_attempts (
    id                    TEXT    PRIMARY KEY NOT NULL,
    job_id                TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    content_sha256        TEXT    NOT NULL,
    request_family        TEXT    NOT NULL,
    provider              TEXT,
    model                 TEXT    NOT NULL,
    route                 TEXT,
    privacy_policy_snapshot TEXT,
    consent_id            TEXT,
    estimated_units_json  TEXT,
    dispatched_at         INTEGER,
    responded_at          INTEGER,
    -- prepared | dispatched | responded | outcome_unknown | refused | cancelled
    status                TEXT    NOT NULL,
    provider_request_id   TEXT,
    final_cost_json       TEXT,
    created_at            INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_inference_attempt ON inference_attempts(id);
CREATE INDEX IF NOT EXISTS ix_inference_attempts_job ON inference_attempts(job_id);
CREATE INDEX IF NOT EXISTS ix_inference_attempts_status ON inference_attempts(status);

-- ---------------------------------------------------------------------------
-- Job engine side state (1:1 with jobs; keeps core table untouched)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS job_engine_state (
    job_id             TEXT PRIMARY KEY NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    key_kind           TEXT    NOT NULL,
    key_canonical      TEXT    NOT NULL,
    is_reprocess       INTEGER NOT NULL DEFAULT 0,
    reprocess_token    TEXT,
    reprocess_of       TEXT REFERENCES jobs(id) ON DELETE SET NULL,
    resource_class     TEXT    NOT NULL,
    wait_reason        TEXT,
    cancel_requested_at INTEGER,
    consent_id         TEXT,
    recording_id       TEXT,
    -- Non-zero once a cloud dispatch has left the machine; makes the
    -- "possibly billed" determination durable and independent of job state.
    dispatched         INTEGER NOT NULL DEFAULT 0,
    updated_at         INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS ix_job_engine_state_reprocess_of ON job_engine_state(reprocess_of);

-- ---------------------------------------------------------------------------
-- Committed job outputs. A job may only reach `completed` when a row here was
-- written in the same transaction (§14.1).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS job_outputs (
    id             TEXT    PRIMARY KEY NOT NULL,
    job_id         TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    output_kind    TEXT    NOT NULL,
    output_ref     TEXT    NOT NULL,
    output_sha256  TEXT,
    byte_length    INTEGER,
    committed_at   INTEGER NOT NULL,
    UNIQUE (job_id, output_kind, output_ref)
);

CREATE INDEX IF NOT EXISTS ix_job_outputs_job ON job_outputs(job_id);

-- ---------------------------------------------------------------------------
-- Transition audit trail. Append-only; one row per applied transition.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS job_transitions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id      TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    from_state  TEXT    NOT NULL,
    to_state    TEXT    NOT NULL,
    actor       TEXT    NOT NULL,
    reason      TEXT    NOT NULL,
    detail      TEXT,
    occurred_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS ix_job_transitions_job ON job_transitions(job_id, id);

-- ---------------------------------------------------------------------------
-- Stage checkpoints (§14.3): prepared intent first, commit mark after the
-- output is durable.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS job_checkpoints (
    job_id        TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    stage         TEXT    NOT NULL,
    stage_index   INTEGER NOT NULL,
    input_digest  TEXT    NOT NULL,
    output_ref    TEXT,
    output_digest TEXT,
    attempt       INTEGER NOT NULL DEFAULT 0,
    prepared_at   INTEGER NOT NULL,
    committed_at  INTEGER,
    PRIMARY KEY (job_id, stage)
);

CREATE INDEX IF NOT EXISTS ix_job_checkpoints_job ON job_checkpoints(job_id, stage_index);

-- ---------------------------------------------------------------------------
-- Durable monotonic local event log (§14.5).
-- The sequence is allocated from `event_sequence` inside the committing
-- transaction, so a rollback leaves no gap and no dispatched event.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS event_sequence (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    next INTEGER NOT NULL
);

INSERT OR IGNORE INTO event_sequence (id, next) VALUES (1, 1);

CREATE TABLE IF NOT EXISTS job_events (
    seq          INTEGER PRIMARY KEY NOT NULL,
    kind         TEXT    NOT NULL,
    job_id       TEXT,
    recording_id TEXT,
    payload_json TEXT    NOT NULL,
    occurred_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS ix_job_events_job ON job_events(job_id, seq);
CREATE INDEX IF NOT EXISTS ix_job_events_kind ON job_events(kind, seq);

CREATE TABLE IF NOT EXISTS event_consumer_cursors (
    consumer_id TEXT    PRIMARY KEY NOT NULL,
    last_seq    INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- ---------------------------------------------------------------------------
-- Durable deletion tombstones (§14.5: "Jobs will not resurrect a user-deleted
-- local recording"). Created with IF NOT EXISTS so the core migration may own
-- the richer definition.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS recording_tombstones (
    recording_id   TEXT PRIMARY KEY NOT NULL,
    media_sha256   TEXT,
    source_id      TEXT,
    external_id    TEXT,
    deleted_at     INTEGER NOT NULL,
    retention_until INTEGER
);

CREATE INDEX IF NOT EXISTS ix_recording_tombstones_sha ON recording_tombstones(media_sha256);
CREATE INDEX IF NOT EXISTS ix_recording_tombstones_source
    ON recording_tombstones(source_id, external_id);
