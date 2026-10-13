-- Resumable speaker-matching jobs: the per-job scratch a crash must survive
-- (VOICE_PROFILES_SPEC VP-020, VP-100..VP-105; ADR-0080, ADR-0082).
--
-- The §4 pipeline is one resumable job per recording with a checkpoint per
-- stage, and "a crash between embed and match resumes at match" (VP-102) is
-- only true if the embed stage's output is durable. Cluster embeddings are
-- biometric data, so the durable form is the same one the enrolment tier uses:
-- XChaCha20-Poly1305 ciphertext whose key lives in the OS vault
-- (`SecretScope::VoiceProfileKey`). There is no plaintext column here and no
-- code path that could write one — the repository takes sealed bytes only.
--
-- Both tables are **scratch**, not a record: they exist for the lifetime of one
-- job and the executor discards them when the job settles, in the same
-- transaction family as the suggestions it produced. A cluster embedding is
-- never promoted to an exemplar here; that is enrolment, an explicit user
-- action (VP-007).
--
-- `job_id` is deliberately not a foreign key: the job rows live in the same
-- file but are written through the job engine's own connection pool, and a
-- foreign key across those two writers would turn an ordinary interleaving
-- into a constraint failure. Deletion is driven by the executor and by the
-- diarization-run cascade below, which is what a dropped recording triggers.

CREATE TABLE speaker_match_cluster_embeddings (
    job_id             TEXT    NOT NULL CHECK (length(job_id) BETWEEN 1 AND 64),
    diarization_run_id TEXT    NOT NULL REFERENCES diarization_runs(id) ON DELETE CASCADE,
    cluster_key        TEXT    NOT NULL CHECK (length(cluster_key) BETWEEN 1 AND 200),
    -- Clean single-speaker speech behind the vector (VP-022, VP-065). Kept so
    -- a resumed match applies the same identification minimum the embed stage
    -- applied, without re-reading the audio.
    speech_ms          INTEGER NOT NULL CHECK (speech_ms >= 0),
    model_id           TEXT    NOT NULL CHECK (length(model_id) BETWEEN 1 AND 200),
    model_version      TEXT    NOT NULL CHECK (length(model_version) BETWEEN 1 AND 64),
    embedding_dim      INTEGER NOT NULL CHECK (embedding_dim BETWEEN 1 AND 8192),
    embedding_cipher   BLOB    NOT NULL CHECK (length(embedding_cipher) >= 16),
    embedding_nonce    BLOB    NOT NULL CHECK (length(embedding_nonce) = 24),
    created_at         INTEGER NOT NULL,
    PRIMARY KEY (job_id, cluster_key)
) STRICT;

CREATE INDEX ix_speaker_match_cluster_embeddings_run
    ON speaker_match_cluster_embeddings(diarization_run_id);

-- The match stage's output, so the suggestion stage can be resumed without
-- re-scoring. A report carries person ids, scores and bands and nothing else
-- (`plaudon_app_core::voice_profiles::matching::MatchReport` is serializable
-- precisely because an embedding is not), so this column is score metadata in
-- the §15.4 sense, not biometric material.
CREATE TABLE speaker_match_reports (
    job_id             TEXT    NOT NULL CHECK (length(job_id) BETWEEN 1 AND 64),
    diarization_run_id TEXT    NOT NULL REFERENCES diarization_runs(id) ON DELETE CASCADE,
    cluster_key        TEXT    NOT NULL CHECK (length(cluster_key) BETWEEN 1 AND 200),
    report_json        TEXT    NOT NULL
                       CHECK (json_valid(report_json) AND json_type(report_json) = 'object'),
    created_at         INTEGER NOT NULL,
    PRIMARY KEY (job_id, cluster_key)
) STRICT;

CREATE INDEX ix_speaker_match_reports_run ON speaker_match_reports(diarization_run_id);
