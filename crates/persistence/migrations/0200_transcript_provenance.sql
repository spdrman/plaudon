-- Transcript provenance beyond the core §13.2 shape (task F2.4).
--
-- The core schema (0002) already carries revisions and segments. What a
-- whisper.cpp run additionally produces, and what §8.5 requires us to keep, is:
--
--   * word-level alignment, when the engine reported it;
--   * non-destructive quality annotations (§8.6A) — flags, never edits;
--   * the exact engine parameters and compute backend, so a revision can be
--     reproduced and so a CPU fallback is visible rather than invisible;
--   * optionally, the raw provider response, encrypted at rest and off by
--     default (§8.5: "minimal and privacy-conscious" retention).

ALTER TABLE transcript_revisions ADD COLUMN engine_backend TEXT;

-- Canonical JSON object of the parameters that affect output. Stored as text
-- rather than columns because the set differs per engine, and promoted
-- attributes already live in their own columns.
ALTER TABLE transcript_revisions ADD COLUMN engine_params_json TEXT;

-- Raw provider response, encrypted at rest. NULL unless the user turned
-- detailed provenance on. Credentials are redacted before encryption; the
-- column holds ciphertext only.
ALTER TABLE transcript_revisions ADD COLUMN raw_provenance_cipher BLOB;
ALTER TABLE transcript_revisions ADD COLUMN raw_provenance_nonce BLOB;
ALTER TABLE transcript_revisions ADD COLUMN raw_provenance_scheme TEXT;

CREATE TABLE transcript_words (
    id               TEXT    PRIMARY KEY NOT NULL,
    segment_id       TEXT    NOT NULL REFERENCES transcript_segments(id) ON DELETE CASCADE,
    seq_index        INTEGER NOT NULL CHECK (seq_index >= 0),
    start_ms         INTEGER NOT NULL CHECK (start_ms >= 0),
    end_ms           INTEGER NOT NULL CHECK (end_ms >= 0),
    text             TEXT    NOT NULL,
    confidence       REAL,
    confidence_scale TEXT,
    CHECK (end_ms >= start_ms),
    CHECK (confidence IS NULL OR confidence_scale IS NOT NULL),
    UNIQUE (segment_id, seq_index)
) STRICT;

CREATE INDEX ix_words_segment ON transcript_words(segment_id, seq_index);

CREATE TABLE transcript_annotations (
    id                     TEXT    PRIMARY KEY NOT NULL,
    transcript_revision_id TEXT    NOT NULL REFERENCES transcript_revisions(id) ON DELETE CASCADE,
    segment_index          INTEGER NOT NULL CHECK (segment_index >= 0),
    kind                   TEXT    NOT NULL CHECK (kind IN (
                               'suspected_repetition_loop',
                               'suspected_non_speech',
                               'low_confidence',
                               'ambiguous_overlap')),
    reason                 TEXT    NOT NULL,
    start_ms               INTEGER CHECK (start_ms IS NULL OR start_ms >= 0),
    end_ms                 INTEGER CHECK (end_ms IS NULL OR end_ms >= 0),
    CHECK (start_ms IS NULL OR end_ms IS NULL OR end_ms >= start_ms)
) STRICT;

CREATE INDEX ix_annotations_revision ON transcript_annotations(transcript_revision_id, segment_index);
