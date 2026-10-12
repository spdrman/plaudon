-- Job engine schema (EPIC C; SPEC v1.3 §13.2, §13.3A, §14).
--
-- Migration range 0100-0199 is owned by the job engine. Core tables from
-- 0001/0002 (`jobs`, `job_steps`, `inference_attempts`, `tombstones`) are owned
-- by `crates/persistence` and are NOT redefined here; this file only adds
-- engine-owned tables and the additive columns the engine needs on
-- `inference_attempts`.
--
-- Registered as version 100 in `plaudon_persistence::migrations::MIGRATIONS`,
-- so the one digest-recording runner applies it. The import journal
-- (`import_journal`) and the cloud request ledger (`inference_attempts`) are
-- owned by 0002 and are not touched here: the job engine drives
-- `plaudon_persistence::journal::ImportJournal` rather than keeping a second
-- journal.

-- ---------------------------------------------------------------------------
-- Job engine side state (1:1 with jobs; the core table stays untouched)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS job_engine_state (
    job_id              TEXT PRIMARY KEY NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    key_kind            TEXT    NOT NULL,
    key_canonical       TEXT    NOT NULL,
    is_reprocess        INTEGER NOT NULL DEFAULT 0 CHECK (is_reprocess IN (0,1)),
    reprocess_token     TEXT,
    reprocess_of        TEXT REFERENCES jobs(id) ON DELETE SET NULL,
    resource_class      TEXT    NOT NULL,
    wait_reason         TEXT,
    cancel_requested_at INTEGER,
    consent_id          TEXT,
    recording_id        TEXT,
    -- Non-zero once a cloud dispatch has left the machine. Makes the
    -- "possibly billed" determination durable and independent of job state.
    dispatched          INTEGER NOT NULL DEFAULT 0 CHECK (dispatched IN (0,1)),
    updated_at          INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS ix_job_engine_state_reprocess_of ON job_engine_state(reprocess_of);

-- ---------------------------------------------------------------------------
-- Committed job outputs. A job may only reach `completed` when a row here was
-- written in the same transaction (§14.1).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS job_outputs (
    id            TEXT    PRIMARY KEY NOT NULL,
    job_id        TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    output_kind   TEXT    NOT NULL,
    output_ref    TEXT    NOT NULL,
    output_sha256 TEXT    CHECK (output_sha256 IS NULL OR length(output_sha256) = 64),
    byte_length   INTEGER CHECK (byte_length IS NULL OR byte_length >= 0),
    committed_at  INTEGER NOT NULL,
    UNIQUE (job_id, output_kind, output_ref)
) STRICT;

CREATE INDEX IF NOT EXISTS ix_job_outputs_job ON job_outputs(job_id);

-- ---------------------------------------------------------------------------
-- Transition audit trail. Append-only; one row per applied transition.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS job_transitions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id      TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    from_state  TEXT    NOT NULL,
    to_state    TEXT    NOT NULL,
    actor       TEXT    NOT NULL CHECK (actor IN ('system','user','recovery')),
    reason      TEXT    NOT NULL,
    detail      TEXT,
    occurred_at INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS ix_job_transitions_job ON job_transitions(job_id, id);

-- ---------------------------------------------------------------------------
-- Stage checkpoints (§14.3): prepared intent first, commit mark only after the
-- output is durable.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS job_checkpoints (
    job_id        TEXT    NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    stage         TEXT    NOT NULL,
    stage_index   INTEGER NOT NULL CHECK (stage_index >= 0),
    input_digest  TEXT    NOT NULL,
    output_ref    TEXT,
    output_digest TEXT,
    attempt       INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    prepared_at   INTEGER NOT NULL,
    committed_at  INTEGER,
    PRIMARY KEY (job_id, stage)
) STRICT;

CREATE INDEX IF NOT EXISTS ix_job_checkpoints_job ON job_checkpoints(job_id, stage_index);

-- ---------------------------------------------------------------------------
-- Durable monotonic local event log (§14.5).
--
-- The sequence is allocated from `event_sequence` inside the committing
-- transaction rather than from AUTOINCREMENT, because a rolled-back
-- AUTOINCREMENT insert consumes its row id and would leave a permanent gap.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS event_sequence (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    next INTEGER NOT NULL
) STRICT;

INSERT OR IGNORE INTO event_sequence (id, next) VALUES (1, 1);

CREATE TABLE IF NOT EXISTS job_events (
    seq          INTEGER PRIMARY KEY NOT NULL,
    kind         TEXT    NOT NULL,
    job_id       TEXT,
    recording_id TEXT,
    payload_json TEXT    NOT NULL,
    occurred_at  INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS ix_job_events_job ON job_events(job_id, seq);
CREATE INDEX IF NOT EXISTS ix_job_events_kind ON job_events(kind, seq);

CREATE TABLE IF NOT EXISTS event_consumer_cursors (
    consumer_id TEXT    PRIMARY KEY NOT NULL,
    last_seq    INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;
