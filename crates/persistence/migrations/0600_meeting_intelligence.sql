-- EPIC I: meeting intelligence and source grounding (migration range 0600-0699).
--
-- Spec §9.1 requires that every generated output records the exact transcript
-- revision and the model/prompt/template/stage versions each stage ran with.
-- Spec §9.6/§9.6A require that every claim carries a verification state and
-- that a transcript edit marks dependent outputs stale without mutating them.
-- This migration adds the storage for both.

-- --------------------------------------------------------------- tool registry
-- The DDL for `transcript_tools` and `tool_runs` is owned by EPIC M's
-- migration 0400. Both orders must work: when 0400 has already run these are
-- no-ops, and when EPIC M is not present this branch still has the §13.3
-- tables it writes run rows into. The column set is EPIC M's superset.
CREATE TABLE IF NOT EXISTS transcript_tools (
    tool_id               TEXT    NOT NULL,
    version               INTEGER NOT NULL CHECK (version >= 1),
    display_name          TEXT    NOT NULL,
    kind                  TEXT    NOT NULL,
    prompt_version        TEXT    NOT NULL,
    output_schema_version TEXT    NOT NULL,
    definition_sha256     TEXT    NOT NULL CHECK (length(definition_sha256) = 64),
    origin                TEXT    NOT NULL CHECK (origin IN ('builtin','user')),
    optional              INTEGER NOT NULL DEFAULT 0 CHECK (optional IN (0,1)),
    created_at            INTEGER NOT NULL,
    PRIMARY KEY (tool_id, version)
) STRICT;

CREATE TABLE IF NOT EXISTS tool_runs (
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
    input_sha256           TEXT    CHECK (input_sha256 IS NULL OR length(input_sha256) = 64),
    output_sha256          TEXT    CHECK (output_sha256 IS NULL OR length(output_sha256) = 64),
    status                 TEXT    NOT NULL CHECK (status IN ('succeeded','rejected','failed','canceled')),
    consent_id             TEXT    REFERENCES consent_grants(id) ON DELETE RESTRICT,
    consent_policy_json    TEXT,
    estimated_cost         TEXT,
    actual_cost            TEXT,
    currency               TEXT,
    cost_confidence        TEXT    NOT NULL DEFAULT 'unknown'
                                   CHECK (cost_confidence IN ('unknown','estimated','finalized')),
    output_id              TEXT    REFERENCES ai_outputs(id) ON DELETE RESTRICT,
    created_at             INTEGER NOT NULL,
    finished_at            INTEGER,
    -- Unknown is never zero (§13.2).
    CHECK (cost_confidence <> 'unknown'   OR (estimated_cost IS NULL AND actual_cost IS NULL)),
    CHECK (cost_confidence <> 'finalized' OR actual_cost IS NOT NULL),
    -- A succeeded run has an output digest; a partial result is never success.
    CHECK (status <> 'succeeded' OR output_sha256 IS NOT NULL),
    FOREIGN KEY (tool_id, tool_version)
        REFERENCES transcript_tools(tool_id, version) ON DELETE RESTRICT
) STRICT;

CREATE INDEX IF NOT EXISTS ix_tool_runs_recording ON tool_runs(recording_id);

-- ----------------------------------------------------------- generated outputs
-- §13.2 created `ai_outputs` with the minimum column set. The summarization
-- pipeline additionally needs the pipeline/template identity it ran with, the
-- input and output digests that make a run replayable, the coverage claim and
-- the staleness state.
ALTER TABLE ai_outputs ADD COLUMN pipeline_version     TEXT NOT NULL DEFAULT '';
ALTER TABLE ai_outputs ADD COLUMN template_id          TEXT NOT NULL DEFAULT '';
ALTER TABLE ai_outputs ADD COLUMN input_sha256         TEXT;
ALTER TABLE ai_outputs ADD COLUMN output_sha256        TEXT;
ALTER TABLE ai_outputs ADD COLUMN compatibility_mode   TEXT;
ALTER TABLE ai_outputs ADD COLUMN coverage_state       TEXT NOT NULL DEFAULT 'full';
ALTER TABLE ai_outputs ADD COLUMN coverage_json        TEXT NOT NULL DEFAULT '{"state":"full"}';
ALTER TABLE ai_outputs ADD COLUMN grounding_json       TEXT NOT NULL DEFAULT '{}';
ALTER TABLE ai_outputs ADD COLUMN rejected_json        TEXT NOT NULL DEFAULT '[]';
ALTER TABLE ai_outputs ADD COLUMN directives_json      TEXT NOT NULL DEFAULT '[]';
ALTER TABLE ai_outputs ADD COLUMN stale_at             INTEGER;
ALTER TABLE ai_outputs ADD COLUMN stale_reason         TEXT;
ALTER TABLE ai_outputs ADD COLUMN superseded_by        TEXT REFERENCES ai_outputs(id) ON DELETE RESTRICT;

CREATE INDEX ix_ai_outputs_recording ON ai_outputs(recording_id, created_at);
CREATE INDEX ix_ai_outputs_revision  ON ai_outputs(transcript_revision_id);

-- SQLite cannot add a CHECK with ALTER TABLE, so the staleness invariant is a
-- trigger. An output is either not stale, or stale with a recorded reason and
-- instant; "stale with no reason" is unrepresentable.
CREATE TRIGGER ai_outputs_stale_pair_insert
BEFORE INSERT ON ai_outputs
WHEN (NEW.stale_at IS NULL) <> (NEW.stale_reason IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'stale_at and stale_reason must be set together');
END;

CREATE TRIGGER ai_outputs_stale_pair_update
BEFORE UPDATE ON ai_outputs
WHEN (NEW.stale_at IS NULL) <> (NEW.stale_reason IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'stale_at and stale_reason must be set together');
END;

-- An output body is immutable once written: regeneration creates a new row
-- (§9.6A "we will not silently rewrite a human-reviewed decision").
CREATE TRIGGER ai_outputs_body_is_immutable
BEFORE UPDATE OF body_json, transcript_revision_id, template_version, model_id,
                 prompt_version, schema_version, input_sha256, output_sha256
ON ai_outputs
WHEN NEW.body_json <> OLD.body_json
  OR NEW.transcript_revision_id <> OLD.transcript_revision_id
  OR NEW.output_sha256 IS NOT OLD.output_sha256
BEGIN
    SELECT RAISE(ABORT, 'a generated output is immutable; regenerate into a new row');
END;

-- ------------------------------------------------------------- stage provenance
CREATE TABLE pipeline_stage_runs (
    id                      TEXT    PRIMARY KEY NOT NULL,
    output_id               TEXT    NOT NULL REFERENCES ai_outputs(id) ON DELETE CASCADE,
    sequence                INTEGER NOT NULL CHECK (sequence >= 0),
    stage                   TEXT    NOT NULL CHECK (stage IN (
                                        'normalize',
                                        'section_summary',
                                        'consolidated_summary',
                                        'structured_extraction',
                                        'citation_verification')),
    stage_version           TEXT    NOT NULL CHECK (length(stage_version) > 0),
    pipeline_version        TEXT    NOT NULL CHECK (length(pipeline_version) > 0),
    transcript_revision_id  TEXT    NOT NULL REFERENCES transcript_revisions(id) ON DELETE RESTRICT,
    input_sha256            TEXT    NOT NULL CHECK (length(input_sha256) = 64),
    output_sha256           TEXT    NOT NULL CHECK (length(output_sha256) = 64),
    engine                  TEXT,
    model_id                TEXT,
    model_version           TEXT,
    compatibility_mode      TEXT    CHECK (compatibility_mode IS NULL
                                           OR compatibility_mode IN ('schema_enforced','prompt_constrained')),
    repair_attempts         INTEGER NOT NULL DEFAULT 0 CHECK (repair_attempts >= 0),
    prompt_version          TEXT    NOT NULL CHECK (length(prompt_version) > 0),
    prompt_envelope_version TEXT    NOT NULL CHECK (length(prompt_envelope_version) > 0),
    template_id             TEXT    NOT NULL CHECK (length(template_id) > 0),
    template_version        TEXT    NOT NULL CHECK (length(template_version) > 0),
    schema_version          TEXT    NOT NULL CHECK (length(schema_version) > 0),
    segment_first           INTEGER CHECK (segment_first IS NULL OR segment_first >= 0),
    segment_last            INTEGER CHECK (segment_last IS NULL OR segment_last >= 0),
    chunk_index             INTEGER CHECK (chunk_index IS NULL OR chunk_index >= 0),
    -- Section response retained so a targeted regeneration can reuse it
    -- instead of re-billing an unchanged range (§9.6A, I2.4).
    section_response_json   TEXT,
    reused_from             TEXT,
    started_at              INTEGER NOT NULL,
    ended_at                INTEGER NOT NULL,
    -- §9.1: an inference stage either dispatched to a named model under a
    -- known compatibility mode, or reused an earlier stage run. There is no
    -- third case, and therefore no stage without provenance.
    CHECK (stage NOT IN ('section_summary','consolidated_summary','structured_extraction')
           OR reused_from IS NOT NULL
           OR (model_id IS NOT NULL AND compatibility_mode IS NOT NULL)),
    CHECK (ended_at >= started_at),
    CHECK (segment_first IS NULL OR segment_last IS NULL OR segment_last >= segment_first),
    UNIQUE (output_id, sequence)
) STRICT;

CREATE INDEX ix_stage_runs_output ON pipeline_stage_runs(output_id, sequence);

-- -------------------------------------------------------------------- claims
CREATE TABLE output_claims (
    id                  TEXT    PRIMARY KEY NOT NULL,
    output_id           TEXT    NOT NULL REFERENCES ai_outputs(id) ON DELETE CASCADE,
    item_path           TEXT    NOT NULL,
    kind                TEXT    NOT NULL CHECK (kind IN (
                                    'summary_statement','decision','action_item',
                                    'risk','question','topic')),
    category            TEXT    NOT NULL CHECK (category IN (
                                    'general','financial_amount','obligation',
                                    'deadline','safety_commitment')),
    claim_text          TEXT    NOT NULL,
    state               TEXT    NOT NULL CHECK (state IN ('source_linked','human_confirmed','unverified')),
    entailment          TEXT    NOT NULL CHECK (entailment IN ('supported','unsupported','not_checked')),
    review_reasons_json TEXT    NOT NULL DEFAULT '[]',
    reviewer            TEXT,
    reviewed_at         INTEGER,
    review_accepted     INTEGER CHECK (review_accepted IS NULL OR review_accepted IN (0,1)),
    review_note         TEXT,
    -- §9.6A: only an explicit human review produces human_confirmed. A model
    -- output cannot reach this state because it cannot write a reviewer.
    CHECK (state <> 'human_confirmed'
           OR (reviewer IS NOT NULL AND reviewed_at IS NOT NULL AND review_accepted = 1)),
    CHECK ((reviewer IS NULL) = (reviewed_at IS NULL)),
    CHECK ((reviewer IS NULL) = (review_accepted IS NULL)),
    UNIQUE (output_id, item_path)
) STRICT;

CREATE INDEX ix_output_claims_output ON output_claims(output_id);
CREATE INDEX ix_output_claims_state  ON output_claims(state);

-- ------------------------------------------------------------------- anchors
-- §13.2 created `output_evidence` keyed by `transcript_segments.id`. That
-- shape cannot represent the case the spec actually requires us to record: an
-- anchor the model invented, pointing at a segment that does not exist. The
-- table is recreated with the anchor shape plus its verification outcome.
-- It has never been written to by any shipped code path, so no rows are lost.
DROP TABLE output_evidence;

CREATE TABLE output_evidence (
    id                     TEXT    PRIMARY KEY NOT NULL,
    output_id              TEXT    NOT NULL REFERENCES ai_outputs(id) ON DELETE CASCADE,
    claim_id               TEXT    NOT NULL REFERENCES output_claims(id) ON DELETE CASCADE,
    position               INTEGER NOT NULL CHECK (position >= 0),
    item_path              TEXT    NOT NULL,
    -- The revision the *model* cited. Deliberately not a foreign key: a
    -- fabricated citation names a revision that may not exist, and recording
    -- that fact is the point of this row.
    cited_revision_id      TEXT    NOT NULL,
    seq_index              INTEGER NOT NULL CHECK (seq_index >= 0),
    char_start             INTEGER NOT NULL CHECK (char_start >= 0),
    char_end               INTEGER NOT NULL CHECK (char_end > char_start),
    quote                  TEXT    NOT NULL,
    verification_status    TEXT    NOT NULL CHECK (verification_status IN ('verified','rejected')),
    rejection_code         TEXT,
    matched_text           TEXT,
    alignment_origin       TEXT    CHECK (alignment_origin IS NULL
                                          OR alignment_origin IN ('native','derived','none')),
    seek_ms                INTEGER CHECK (seek_ms IS NULL OR seek_ms >= 0),
    CHECK (verification_status <> 'verified'
           OR (matched_text IS NOT NULL AND alignment_origin IS NOT NULL)),
    CHECK (verification_status <> 'rejected' OR rejection_code IS NOT NULL),
    -- §8.6A: a revision without alignment never yields a seek position.
    CHECK (seek_ms IS NULL OR alignment_origin IN ('native','derived')),
    UNIQUE (claim_id, position)
) STRICT;

CREATE INDEX ix_output_evidence_claim  ON output_evidence(claim_id, position);
CREATE INDEX ix_output_evidence_output ON output_evidence(output_id, verification_status);

-- ----------------------------------------------------------- dependency graph
-- §9.6A: transcript edits invalidate dependent output revisions via a
-- version/hash dependency graph. Edges are append-only; a stale output keeps
-- its edges so it stays fully retrievable and explicable.
CREATE TABLE output_dependencies (
    output_id              TEXT    NOT NULL REFERENCES ai_outputs(id) ON DELETE CASCADE,
    transcript_revision_id TEXT    NOT NULL REFERENCES transcript_revisions(id) ON DELETE RESTRICT,
    input_sha256           TEXT    NOT NULL CHECK (length(input_sha256) = 64),
    PRIMARY KEY (output_id, transcript_revision_id)
) STRICT;

CREATE INDEX ix_output_dependencies_revision ON output_dependencies(transcript_revision_id);
