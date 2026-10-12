-- EPIC G (OpenRouter BYOK). Versions 450-499 are reserved for it.
--
-- The cloud request ledger itself (`inference_attempts`) and `usage_events`
-- are owned by 0002_core.sql and are NOT redefined here. This migration adds
-- only what EPIC G needs on top:
--
--   * the capability registry that decides what the model picker may claim
--     (spec 10.1: `Verified` requires an observed exchange, never a catalog
--     entry);
--   * per-task default models and per-recording overrides (spec 10.2);
--   * documented per-route retention policies, keyed by content type so a
--     text-generation policy can never answer for an audio upload
--     (spec 10.8);
--   * indexes the usage/budget queries need.
--
-- Local budgets live in `app_preferences` under the `cloud.budget` key: they
-- are a single user-scoped record with a versioned JSON schema, which is
-- exactly what that table exists for.

-- ---------------------------------------------------------------------------
-- Capability registry (App. A.3)
-- ---------------------------------------------------------------------------
CREATE TABLE model_capability_records (
    model_id        TEXT    NOT NULL,
    -- 'transcription' | 'text_generation'. A model may be a candidate for both
    -- and must be verified separately for each.
    task            TEXT    NOT NULL CHECK (task IN ('transcription','text_generation')),
    request_family  TEXT    NOT NULL CHECK (request_family IN ('multipart_stt','audio_chat','unsupported','unknown')),
    -- 'observed_exchange' | 'catalog_only' | 'none'. Only observed_exchange
    -- may support a verified card.
    evidence        TEXT    NOT NULL CHECK (evidence IN ('observed_exchange','catalog_only','none')),
    -- Named wire fixture or probe run backing the record.
    fixture_id      TEXT,
    -- YYYY-MM-DD of the observation.
    validated_at    TEXT,
    -- Verbatim probe record, so a newer field is preserved rather than lost.
    record_json     TEXT    NOT NULL DEFAULT '{}',
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (model_id, task),
    CHECK (evidence <> 'observed_exchange' OR (fixture_id IS NOT NULL AND validated_at IS NOT NULL))
) STRICT;

-- ---------------------------------------------------------------------------
-- Model selection (spec 10.2)
-- ---------------------------------------------------------------------------

-- Default model per processing stage. One row per stage.
CREATE TABLE model_task_defaults (
    stage       TEXT    PRIMARY KEY NOT NULL,
    engine      TEXT    NOT NULL,
    model_id    TEXT,
    updated_at  INTEGER NOT NULL
) STRICT;

-- Per-recording override. Never mutates the default; deleting the row restores
-- it.
CREATE TABLE recording_model_overrides (
    recording_id TEXT    NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    stage        TEXT    NOT NULL,
    engine       TEXT    NOT NULL,
    model_id     TEXT,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (recording_id, stage)
) STRICT;

-- ---------------------------------------------------------------------------
-- Documented retention policies, per exact route and content type (spec 10.8)
-- ---------------------------------------------------------------------------
CREATE TABLE route_privacy_policies (
    model_id            TEXT    NOT NULL,
    -- NULL means "the route whose downstream provider is not disclosed"; it
    -- never matches a named provider.
    downstream_provider TEXT,
    content_type        TEXT    NOT NULL CHECK (content_type IN ('audio_bytes','transcript_text','metadata_only')),
    posture             TEXT    NOT NULL CHECK (posture IN ('zero_retention','limited_retention','provider_default')),
    retention_days      INTEGER,
    -- Where the user got the documentation. Never a credential or a signed URL.
    source_note         TEXT    NOT NULL DEFAULT '',
    recorded_at         INTEGER NOT NULL,
    CHECK (posture <> 'limited_retention' OR retention_days IS NOT NULL)
) STRICT;

CREATE UNIQUE INDEX ux_route_privacy
    ON route_privacy_policies(model_id, content_type, IFNULL(downstream_provider, ''));

-- ---------------------------------------------------------------------------
-- Indexes for the ledger and usage queries (spec 10.4)
-- ---------------------------------------------------------------------------
CREATE INDEX ix_inference_attempts_job ON inference_attempts(job_id);
CREATE INDEX ix_inference_attempts_created ON inference_attempts(created_at);
CREATE INDEX ix_usage_events_created ON usage_events(created_at);
CREATE INDEX ix_usage_events_attempt ON usage_events(attempt_id);
