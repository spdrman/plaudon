-- Core identity tables (spec 13.2, 13.3A, 6.3B).
--
-- Identity separation is structural: media identity (sha256 + byte length),
-- recording identity (user-owned UUID), source association
-- (source_kind + account scope + external id) and media revisions are distinct
-- tables. No table in this migration stores a token, a signed URL or key
-- material; credentials live in the operating system vault and are referenced
-- only by opaque handles.

CREATE TABLE recordings (
    id                       TEXT    PRIMARY KEY NOT NULL,
    title                    TEXT    NOT NULL,
    captured_at              INTEGER,
    duration_ms              INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    preferred_media_asset_id TEXT    REFERENCES media_assets(id) ON DELETE RESTRICT,
    annotation               TEXT,
    created_at               INTEGER NOT NULL,
    deleted_at               INTEGER
) STRICT;

CREATE TABLE recording_tags (
    recording_id TEXT NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    tag          TEXT NOT NULL,
    PRIMARY KEY (recording_id, tag)
) STRICT;

CREATE TABLE media_assets (
    id                 TEXT    PRIMARY KEY NOT NULL,
    sha256             TEXT    NOT NULL CHECK (length(sha256) = 64),
    byte_length        INTEGER NOT NULL CHECK (byte_length >= 0),
    relative_path      TEXT    NOT NULL,
    mime               TEXT,
    codec              TEXT,
    sample_rate        INTEGER CHECK (sample_rate IS NULL OR sample_rate > 0),
    channels           INTEGER CHECK (channels IS NULL OR channels > 0),
    duration_ms        INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    quality_class      TEXT    NOT NULL,
    verification_state TEXT    NOT NULL CHECK (verification_state IN ('verified','failed','unverified')),
    verified_at        INTEGER,
    created_at         INTEGER NOT NULL
) STRICT;

-- Spec 13.3A: one stored blob per verified content hash.
CREATE UNIQUE INDEX ux_media_sha ON media_assets(sha256, byte_length);
CREATE UNIQUE INDEX ux_media_relative_path ON media_assets(relative_path);

-- Spec 6.3B: one recording may hold several acquired media candidates of
-- different fidelity without either overwriting the other.
CREATE TABLE recording_media (
    recording_id   TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    media_asset_id TEXT    NOT NULL REFERENCES media_assets(id) ON DELETE RESTRICT,
    quality_class  TEXT    NOT NULL,
    obtained_at    INTEGER NOT NULL,
    PRIMARY KEY (recording_id, media_asset_id)
) STRICT;

CREATE TABLE sources (
    id                TEXT    PRIMARY KEY NOT NULL,
    source_kind       TEXT    NOT NULL,
    account_scope     TEXT    NOT NULL,
    display_name      TEXT    NOT NULL,
    status            TEXT    NOT NULL,
    capabilities_json TEXT    NOT NULL DEFAULT '{}',
    last_verified_at  INTEGER,
    sync_cursor       TEXT,
    sync_policy       TEXT    NOT NULL DEFAULT 'manual',
    created_at        INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX ux_source_kind_scope ON sources(source_kind, account_scope);

CREATE TABLE source_items (
    id              TEXT    PRIMARY KEY NOT NULL,
    source_id       TEXT    NOT NULL REFERENCES sources(id) ON DELETE RESTRICT,
    external_id     TEXT,
    recording_id    TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    import_status   TEXT    NOT NULL,
    last_seen_at    INTEGER,
    remote_version  TEXT,
    remote_state    TEXT    NOT NULL DEFAULT 'unknown',
    tombstone_reason TEXT,
    created_at      INTEGER NOT NULL
) STRICT;

-- Spec 13.3A: an external identifier is unique only inside its source.
CREATE UNIQUE INDEX ux_source_external
    ON source_items(source_id, external_id)
    WHERE external_id IS NOT NULL;
CREATE INDEX ix_source_items_recording ON source_items(recording_id);

-- Spec 13.3: changed source content creates a new immutable media revision.
CREATE TABLE media_revisions (
    id             TEXT    PRIMARY KEY NOT NULL,
    recording_id   TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    media_asset_id TEXT    NOT NULL REFERENCES media_assets(id) ON DELETE RESTRICT,
    source_item_id TEXT    REFERENCES source_items(id) ON DELETE RESTRICT,
    source_version TEXT,
    predecessor_id TEXT    REFERENCES media_revisions(id) ON DELETE RESTRICT,
    obtained_at    INTEGER NOT NULL
) STRICT;

-- Spec 13.3A: every user-visible delete creates a tombstone with a retention
-- policy and stores no deleted content.
CREATE TABLE tombstones (
    id              TEXT    PRIMARY KEY NOT NULL,
    entity_type     TEXT    NOT NULL CHECK (entity_type IN ('recording','source_item','media_asset','ai_output')),
    entity_id       TEXT    NOT NULL,
    reason          TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    retention_until INTEGER
) STRICT;

CREATE UNIQUE INDEX ux_tombstone_entity ON tombstones(entity_type, entity_id);
