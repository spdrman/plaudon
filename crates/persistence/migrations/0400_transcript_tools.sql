-- EPIC M: transcript tools, per-recording conversations, cross-recording
-- search, diarization turns and speaker labelling (spec 8.7, 9.7, 9.8, 13.3).
--
-- Version range 0400-0499 belongs to EPIC M. The core schema (0002) already
-- owns `speakers`, `ai_outputs`, `output_evidence` and `transcript_segments`;
-- this migration extends them rather than redefining them.

-- ---------------------------------------------------------------------------
-- Tool definitions, versioned independently from runs (spec 13.3).
-- ---------------------------------------------------------------------------

-- A tool definition row is immutable. Editing a prompt or schema inserts a new
-- (tool_id, version) pair, so a prior run keeps pointing at the exact
-- definition that produced it.
CREATE TABLE transcript_tools (
    tool_id               TEXT    NOT NULL,
    version               INTEGER NOT NULL CHECK (version >= 1),
    display_name          TEXT    NOT NULL,
    kind                  TEXT    NOT NULL,
    prompt_version        TEXT    NOT NULL,
    output_schema_version TEXT    NOT NULL,
    -- SHA-256 over the canonical serialization of the whole definition.
    definition_sha256     TEXT    NOT NULL CHECK (length(definition_sha256) = 64),
    origin                TEXT    NOT NULL CHECK (origin IN ('builtin','user')),
    -- 1 for tools that are opt-in extras (study artifacts), 0 for the default
    -- meeting outputs. Spec 9.3: study tools must not obscure meeting focus.
    optional              INTEGER NOT NULL DEFAULT 0 CHECK (optional IN (0,1)),
    created_at            INTEGER NOT NULL,
    PRIMARY KEY (tool_id, version)
) STRICT;

-- One generation attempt. Spec 9.7: every generation records input/output
-- hashes, model/prompt identity, consent policy and cost where known.
CREATE TABLE tool_runs (
    id                     TEXT    PRIMARY KEY NOT NULL,
    recording_id           TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    transcript_revision_id TEXT    NOT NULL REFERENCES transcript_revisions(id) ON DELETE RESTRICT,
    tool_id                TEXT    NOT NULL,
    tool_version           INTEGER NOT NULL,
    engine                 TEXT    NOT NULL,
    model_id               TEXT    NOT NULL,
    model_version          TEXT,
    request_family         TEXT    NOT NULL,
    prompt_version         TEXT    NOT NULL,
    schema_version         TEXT    NOT NULL,
    input_sha256           TEXT    NOT NULL CHECK (length(input_sha256) = 64),
    output_sha256          TEXT    CHECK (output_sha256 IS NULL OR length(output_sha256) = 64),
    status                 TEXT    NOT NULL CHECK (status IN ('succeeded','rejected','failed','canceled')),
    consent_id             TEXT    REFERENCES consent_grants(id) ON DELETE RESTRICT,
    consent_policy_json    TEXT    NOT NULL DEFAULT '{}',
    estimated_cost         TEXT,
    actual_cost            TEXT,
    currency               TEXT,
    -- Spec 13.2: unknown is not zero.
    cost_confidence        TEXT    NOT NULL DEFAULT 'unknown' CHECK (cost_confidence IN ('unknown','estimated','finalized')),
    output_id              TEXT    REFERENCES ai_outputs(id) ON DELETE RESTRICT,
    created_at             INTEGER NOT NULL,
    finished_at            INTEGER,
    FOREIGN KEY (tool_id, tool_version)
        REFERENCES transcript_tools(tool_id, version) ON DELETE RESTRICT,
    CHECK (cost_confidence <> 'unknown'   OR (estimated_cost IS NULL AND actual_cost IS NULL)),
    CHECK (cost_confidence <> 'finalized' OR actual_cost IS NOT NULL),
    CHECK (status <> 'succeeded' OR output_sha256 IS NOT NULL)
) STRICT;

CREATE INDEX ix_tool_runs_recording ON tool_runs(recording_id, created_at);
CREATE INDEX ix_tool_runs_revision  ON tool_runs(transcript_revision_id);

-- ---------------------------------------------------------------------------
-- Per-recording conversations bound to one exact transcript revision (9.7).
-- ---------------------------------------------------------------------------

CREATE TABLE recording_conversations (
    id                     TEXT    PRIMARY KEY NOT NULL,
    recording_id           TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    -- NOT NULL by design: a conversation without provenance is exactly the
    -- "generic LLM conversation" spec 9.7 forbids.
    transcript_revision_id TEXT    NOT NULL REFERENCES transcript_revisions(id) ON DELETE RESTRICT,
    created_at             INTEGER NOT NULL
) STRICT;

CREATE INDEX ix_conversations_recording ON recording_conversations(recording_id, created_at);

CREATE TABLE conversation_messages (
    id                TEXT    PRIMARY KEY NOT NULL,
    conversation_id   TEXT    NOT NULL REFERENCES recording_conversations(id) ON DELETE CASCADE,
    seq_index         INTEGER NOT NULL CHECK (seq_index >= 0),
    role              TEXT    NOT NULL CHECK (role IN ('user','assistant')),
    text              TEXT    NOT NULL,
    -- Verified anchors as a JSON array. Empty array is legal only for a user
    -- turn or for an assistant turn whose outcome is not 'answered'.
    source_anchors_json TEXT  NOT NULL DEFAULT '[]',
    outcome           TEXT    CHECK (outcome IS NULL OR outcome IN ('answered','insufficient_evidence','not_supported_by_recording')),
    tool_run_id       TEXT    REFERENCES tool_runs(id) ON DELETE RESTRICT,
    created_at        INTEGER NOT NULL,
    UNIQUE (conversation_id, seq_index),
    CHECK (role <> 'user' OR outcome IS NULL),
    CHECK (role <> 'assistant' OR outcome IS NOT NULL),
    CHECK (outcome IS NULL OR outcome <> 'answered' OR source_anchors_json <> '[]')
) STRICT;

-- ---------------------------------------------------------------------------
-- Diarization (spec 8.7). A separate stage: nothing here edits an STT result.
-- ---------------------------------------------------------------------------

CREATE TABLE diarization_runs (
    id                     TEXT    PRIMARY KEY NOT NULL,
    recording_id           TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    media_asset_id         TEXT    NOT NULL REFERENCES media_assets(id) ON DELETE RESTRICT,
    provider_id            TEXT    NOT NULL,
    provider_version       TEXT,
    privacy_class          TEXT    NOT NULL,
    status                 TEXT    NOT NULL CHECK (status IN ('succeeded','failed','canceled')),
    input_sha256           TEXT    NOT NULL CHECK (length(input_sha256) = 64),
    detected_cluster_count INTEGER CHECK (detected_cluster_count IS NULL OR detected_cluster_count >= 0),
    created_at             INTEGER NOT NULL,
    finished_at            INTEGER
) STRICT;

CREATE INDEX ix_diarization_runs_recording ON diarization_runs(recording_id, created_at);

-- Raw provider turns, preserved verbatim. Reconciliation against transcript
-- segments is derived from these and can be recomputed.
CREATE TABLE speaker_turns (
    id                 TEXT    PRIMARY KEY NOT NULL,
    diarization_run_id TEXT    NOT NULL REFERENCES diarization_runs(id) ON DELETE CASCADE,
    seq_index          INTEGER NOT NULL CHECK (seq_index >= 0),
    cluster_key        TEXT    NOT NULL,
    start_ms           INTEGER NOT NULL CHECK (start_ms >= 0),
    end_ms             INTEGER NOT NULL CHECK (end_ms >= 0),
    confidence         REAL,
    confidence_scale   TEXT,
    CHECK (end_ms >= start_ms),
    CHECK (confidence IS NULL OR confidence_scale IS NOT NULL),
    UNIQUE (diarization_run_id, seq_index)
) STRICT;

CREATE INDEX ix_speaker_turns_time ON speaker_turns(diarization_run_id, start_ms);

-- The reconciled assignment of a transcript segment to a cluster. Stored
-- separately from `transcript_segments.speaker_id` so that an ambiguous or
-- unassigned span stays explicit instead of being guessed into a single id.
CREATE TABLE segment_speaker_assignments (
    transcript_revision_id TEXT    NOT NULL REFERENCES transcript_revisions(id) ON DELETE CASCADE,
    seq_index              INTEGER NOT NULL CHECK (seq_index >= 0),
    diarization_run_id     TEXT    NOT NULL REFERENCES diarization_runs(id) ON DELETE CASCADE,
    assignment             TEXT    NOT NULL CHECK (assignment IN ('single','overlapping','unassigned')),
    -- For 'single' the one cluster; for 'overlapping' a JSON array of clusters
    -- ordered by covered duration; NULL for 'unassigned'.
    cluster_key            TEXT,
    overlapping_keys_json  TEXT,
    covered_ms             INTEGER NOT NULL DEFAULT 0 CHECK (covered_ms >= 0),
    PRIMARY KEY (transcript_revision_id, seq_index, diarization_run_id),
    CHECK (assignment <> 'single'      OR (cluster_key IS NOT NULL AND overlapping_keys_json IS NULL)),
    CHECK (assignment <> 'overlapping' OR (cluster_key IS NULL AND overlapping_keys_json IS NOT NULL)),
    CHECK (assignment <> 'unassigned'  OR (cluster_key IS NULL AND overlapping_keys_json IS NULL))
) STRICT;

-- ---------------------------------------------------------------------------
-- Speaker labelling (spec 8.7, 17.6). `speakers` exists since 0002.
-- ---------------------------------------------------------------------------

-- Stable cluster identity produced by diarization. NULL for a speaker created
-- by hand without a diarization pass.
ALTER TABLE speakers ADD COLUMN cluster_key TEXT;
ALTER TABLE speakers ADD COLUMN renamed_at  INTEGER;
ALTER TABLE speakers ADD COLUMN confirmed_at INTEGER;
-- The machine-assigned label this speaker started life with, so a rename is
-- always reversible without consulting history.
ALTER TABLE speakers ADD COLUMN original_label TEXT;

CREATE UNIQUE INDEX ux_speakers_recording_cluster
    ON speakers(recording_id, cluster_key) WHERE cluster_key IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Cross-recording local full-text search (spec 3.1, 9.8, 17.5).
-- ---------------------------------------------------------------------------

-- `contentless_delete=1` keeps the index small while still allowing row
-- deletion; the authoritative text stays in `transcript_segments`.
CREATE VIRTUAL TABLE transcript_search USING fts5(
    text,
    segment_id            UNINDEXED,
    transcript_revision_id UNINDEXED,
    recording_id          UNINDEXED,
    seq_index             UNINDEXED,
    tokenize = 'unicode61 remove_diacritics 2'
);

-- Index maintenance is driven by triggers so a revision written by any crate
-- stays searchable without that crate knowing about the index.
CREATE TRIGGER trg_transcript_search_insert
AFTER INSERT ON transcript_segments
BEGIN
    INSERT INTO transcript_search(text, segment_id, transcript_revision_id, recording_id, seq_index)
    SELECT NEW.text, NEW.id, NEW.transcript_revision_id, r.recording_id, NEW.seq_index
    FROM transcript_revisions r
    WHERE r.id = NEW.transcript_revision_id;
END;

CREATE TRIGGER trg_transcript_search_delete
AFTER DELETE ON transcript_segments
BEGIN
    DELETE FROM transcript_search WHERE segment_id = OLD.id;
END;

CREATE TRIGGER trg_transcript_search_update
AFTER UPDATE OF text ON transcript_segments
BEGIN
    DELETE FROM transcript_search WHERE segment_id = OLD.id;
    INSERT INTO transcript_search(text, segment_id, transcript_revision_id, recording_id, seq_index)
    SELECT NEW.text, NEW.id, NEW.transcript_revision_id, r.recording_id, NEW.seq_index
    FROM transcript_revisions r
    WHERE r.id = NEW.transcript_revision_id;
END;

-- Backfill anything written before this migration ran.
INSERT INTO transcript_search(text, segment_id, transcript_revision_id, recording_id, seq_index)
SELECT s.text, s.id, s.transcript_revision_id, r.recording_id, s.seq_index
FROM transcript_segments s
JOIN transcript_revisions r ON r.id = s.transcript_revision_id;

-- ---------------------------------------------------------------------------
-- Local scripting bridge (spec 14.5, App. A.5). Disabled until opted in.
-- ---------------------------------------------------------------------------

CREATE TABLE automation_bridge_state (
    -- Exactly one row; the constant primary key makes that structural.
    singleton      INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    enabled        INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0,1)),
    -- Identity of the OS user that enabled it. Automation is restricted to the
    -- current signed-in user (App. A.5).
    owner_user_key TEXT,
    enabled_at     INTEGER,
    updated_at     INTEGER NOT NULL,
    CHECK (enabled = 0 OR owner_user_key IS NOT NULL)
) STRICT;

INSERT INTO automation_bridge_state (singleton, enabled, owner_user_key, enabled_at, updated_at)
VALUES (1, 0, NULL, NULL, 0);
