-- Export, portable backup and deletion accounting (spec 4.6, 7.10, 13.3, 13.5, 13.6).
--
-- Three things here are load-bearing:
--
-- * A deletion is a journalled, resumable operation, not a single statement.
--   `deletion_records` holds the disclosure the user actually confirmed and the
--   stage the deletion reached, so a crash mid-deletion reconciles forward
--   instead of leaving a half-deleted recording that still looks intact.
-- * Suppressions are the spec 7.10 durable tombstone in its matching form:
--   content identity and stable source identity. A filename is never identity,
--   so no path column exists here.
-- * `archive_backups` records the manifest version and verification outcome of
--   every portable archive we produced or restored (spec 13.3).

-- One row per user-confirmed deletion. `disclosure_json` is the exact payload
-- shown before confirmation, kept so the deletion record is auditable.
CREATE TABLE deletion_records (
    id               TEXT    PRIMARY KEY NOT NULL,
    recording_id     TEXT    NOT NULL,
    reason           TEXT    NOT NULL,
    state            TEXT    NOT NULL CHECK (state IN ('planned','rows_removed','media_removed','completed')),
    disclosure_json  TEXT    NOT NULL,
    created_at       INTEGER NOT NULL,
    completed_at     INTEGER,
    CHECK (state <> 'completed' OR completed_at IS NOT NULL)
) STRICT;

CREATE INDEX ix_deletion_records_state ON deletion_records(state);
CREATE INDEX ix_deletion_records_recording ON deletion_records(recording_id);

-- Media blobs a completed deletion is still responsible for removing. The row
-- survives until the bytes are gone, so an interrupted deletion can finish.
CREATE TABLE deletion_pending_blobs (
    deletion_id   TEXT    NOT NULL REFERENCES deletion_records(id) ON DELETE CASCADE,
    relative_path TEXT    NOT NULL,
    sha256        TEXT    NOT NULL CHECK (length(sha256) = 64),
    byte_length   INTEGER NOT NULL CHECK (byte_length >= 0),
    removed_at    INTEGER,
    PRIMARY KEY (deletion_id, relative_path)
) STRICT;

-- Spec 7.10: content identity suppression. The next sync that offers these
-- exact bytes must not resurrect the recording without explicit confirmation.
CREATE TABLE deletion_media_suppressions (
    sha256      TEXT    NOT NULL CHECK (length(sha256) = 64),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    deletion_id TEXT    NOT NULL REFERENCES deletion_records(id) ON DELETE RESTRICT,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (sha256, byte_length)
) STRICT;

-- Spec 7.10/A.2: stable source identity suppression, independent of bytes.
CREATE TABLE deletion_source_suppressions (
    source_id   TEXT    NOT NULL,
    external_id TEXT    NOT NULL,
    deletion_id TEXT    NOT NULL REFERENCES deletion_records(id) ON DELETE RESTRICT,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (source_id, external_id)
) STRICT;

-- Spec 13.3: export and restore round-trips are versioned and validated.
CREATE TABLE archive_backups (
    id                     TEXT    PRIMARY KEY NOT NULL,
    direction              TEXT    NOT NULL CHECK (direction IN ('backup','restore')),
    manifest_schema_version TEXT   NOT NULL,
    format_version         INTEGER NOT NULL,
    protection             TEXT    NOT NULL CHECK (protection IN ('plaintext','encrypted')),
    -- Directory hint only; spec 13.2 keeps complete sensitive paths out of the
    -- database by default, and the file name is never stored.
    location_hint          TEXT,
    entry_count            INTEGER NOT NULL CHECK (entry_count >= 0),
    total_byte_length      INTEGER NOT NULL CHECK (total_byte_length >= 0),
    integrity_digest       TEXT    NOT NULL CHECK (length(integrity_digest) = 64),
    verify_status          TEXT    NOT NULL CHECK (verify_status IN ('verified','failed','unverified')),
    created_at             INTEGER NOT NULL
) STRICT;

-- Spec 7.8/J1.3: a sanitized export derivative is a separately tagged asset.
-- It never replaces the immutable original it was produced from.
CREATE TABLE export_derivatives (
    id                    TEXT    PRIMARY KEY NOT NULL,
    recording_id          TEXT    NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    source_media_asset_id TEXT    NOT NULL REFERENCES media_assets(id) ON DELETE RESTRICT,
    kind                  TEXT    NOT NULL CHECK (kind IN ('sanitized')),
    sha256                TEXT    NOT NULL CHECK (length(sha256) = 64),
    byte_length           INTEGER NOT NULL CHECK (byte_length >= 0),
    stripped_metadata_json TEXT   NOT NULL DEFAULT '[]',
    created_at            INTEGER NOT NULL
) STRICT;

CREATE INDEX ix_export_derivatives_recording ON export_derivatives(recording_id);
