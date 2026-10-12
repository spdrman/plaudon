-- Source reconciliation, vendor export artifacts and watched folders
-- (spec 7.5, 7.10, 13.2, 13.3, 13.3A, App. A.2, App. A.4).
--
-- Three separate facts about one source item are kept in three separate
-- columns, because conflating them is exactly how a sync loop either stalls or
-- re-downloads forever:
--
--   * `remote_version`          — a vendor *hint*, never proof that bytes moved.
--   * `observed_content_sha256` — what we actually hashed, once we held bytes.
--   * `acquisition_complete`    — whether those bytes reached the archive.
--
-- An unchanged version with `acquisition_complete = 0` therefore still
-- re-downloads (App. A.2 "idempotence"), and a changed version with an
-- unchanged digest reconciles metadata without appending a media revision
-- (spec 7.5 "create a new media revision only if bytes differ").

ALTER TABLE source_items ADD COLUMN observed_content_sha256 TEXT
    CHECK (observed_content_sha256 IS NULL OR length(observed_content_sha256) = 64);

ALTER TABLE source_items ADD COLUMN acquisition_complete INTEGER NOT NULL DEFAULT 0
    CHECK (acquisition_complete IN (0, 1));

-- Spec 7.5/13.3 `imported_source_artifacts`: a vendor-produced transcript,
-- summary or tag list is evidence about the vendor, never an app-generated
-- revision. The original bytes are identified by digest and length so the
-- import is attributable and repeatable; `imported_at` stays NULL while the
-- artifact is only known about and not yet turned into a revision.
CREATE TABLE source_export_artifacts (
    id                TEXT    PRIMARY KEY NOT NULL,
    source_item_id    TEXT    NOT NULL REFERENCES source_items(id) ON DELETE RESTRICT,
    artifact_kind     TEXT    NOT NULL CHECK (artifact_kind IN ('transcript', 'summary', 'tags')),
    source_vendor     TEXT    NOT NULL,
    content_sha256    TEXT    NOT NULL CHECK (length(content_sha256) = 64),
    byte_length       INTEGER NOT NULL CHECK (byte_length >= 0),
    original_filename TEXT,
    imported_at       INTEGER,
    import_mode       TEXT    NOT NULL CHECK (import_mode IN ('import_vendor_only', 'retain_both', 'skip_vendor_content')),
    created_at        INTEGER NOT NULL,
    -- A skipped artifact is recorded but never imported (spec 7.5 default).
    CHECK (import_mode <> 'skip_vendor_content' OR imported_at IS NULL)
) STRICT;

CREATE INDEX ix_source_export_artifacts_item ON source_export_artifacts(source_item_id);
CREATE UNIQUE INDEX ux_source_export_artifact_content
    ON source_export_artifacts(source_item_id, artifact_kind, content_sha256);

-- NULL means "produced locally by this application". Any other value names the
-- vendor whose export produced the revision (spec 7.5 `source_vendor=plaud`).
ALTER TABLE transcript_revisions ADD COLUMN source_vendor TEXT;

-- Spec 4.3: the folder the user picked, canonicalised once at configuration
-- time. The path is configuration, not identity: items below are identified by
-- stable file id and content digest, never by name.
CREATE TABLE watched_folders (
    id                TEXT    PRIMARY KEY NOT NULL,
    root_path         TEXT    NOT NULL UNIQUE,
    recursive         INTEGER NOT NULL DEFAULT 0 CHECK (recursive IN (0, 1)),
    rule              TEXT    NOT NULL CHECK (rule IN ('import_only', 'import_and_queue_local_stt', 'ask_before_cloud')),
    quiescence_ms     INTEGER NOT NULL CHECK (quiescence_ms >= 0),
    status            TEXT    NOT NULL CHECK (status IN ('active', 'paused', 'unavailable', 'permission_denied')),
    last_inventory_at INTEGER,
    created_at        INTEGER NOT NULL
) STRICT;

-- Spec 4.3(5)/(6): origin path, stable file identity when available, mtime,
-- size and digest are provenance. A vanished file becomes `unavailable`; the
-- archived recording it produced is never touched (spec 7.10).
CREATE TABLE watched_folder_items (
    id                TEXT    PRIMARY KEY NOT NULL,
    folder_id         TEXT    NOT NULL REFERENCES watched_folders(id) ON DELETE RESTRICT,
    origin_path       TEXT    NOT NULL,
    stable_file_id    TEXT,
    size_bytes        INTEGER NOT NULL CHECK (size_bytes >= 0),
    mtime_ms          INTEGER NOT NULL,
    content_sha256    TEXT    CHECK (content_sha256 IS NULL OR length(content_sha256) = 64),
    state             TEXT    NOT NULL CHECK (state IN ('observed', 'quiescing', 'placeholder', 'ready', 'imported', 'unavailable', 'rejected')),
    source_item_id    TEXT    REFERENCES source_items(id) ON DELETE RESTRICT,
    first_seen_at     INTEGER NOT NULL,
    last_checked_at   INTEGER NOT NULL,
    unavailable_since INTEGER,
    -- An item is only ever 'imported' once committed bytes exist (App. A.2).
    CHECK (state <> 'imported' OR (content_sha256 IS NOT NULL AND source_item_id IS NOT NULL)),
    CHECK (state <> 'unavailable' OR unavailable_since IS NOT NULL)
) STRICT;

CREATE UNIQUE INDEX ux_watched_folder_item_path ON watched_folder_items(folder_id, origin_path);
CREATE INDEX ix_watched_folder_items_state ON watched_folder_items(folder_id, state);
