-- Desktop shell tables (EPIC E), applied by the one MigrationRunner.
--
-- Everything the core schema already owns (recordings, media_assets,
-- recording_media, transcript_revisions, transcript_segments, jobs, job_steps,
-- app_preferences, export_events) is used as-is. This migration adds only what
-- no other crate provides yet:
--
--   * activity_events  - the in-app activity history (spec 5.5, 22(13)), which
--                        exists whether or not an OS notification is delivered;
--   * event_log        - the monotonic UI event sequence (spec 14.5, 15.3),
--                        appended only after the transaction that caused the
--                        event has committed;
--   * recording_import_metadata - the original file name of a manual import,
--                        kept as display metadata only (spec 13.4). It is never
--                        used to build a path: archive paths come from the
--                        content hash alone.
--
-- Version 0300 is the desktop-shell range (0001-0099 EPIC B, 0100-0199 EPIC C,
-- 0200-0299 EPIC F, 0300-0399 here). Anything added later must use a strictly
-- higher version, because MigrationRunner refuses a version below the applied
-- head.

CREATE TABLE activity_events (
    id                             TEXT    PRIMARY KEY NOT NULL,
    category                       TEXT    NOT NULL CHECK (category IN ('import_finished','transcription_finished','requires_attention','cloud_budget_exceeded')),
    occurred_at                    INTEGER NOT NULL,
    title                          TEXT    NOT NULL,
    body                           TEXT    NOT NULL,
    notification_delivered         INTEGER NOT NULL DEFAULT 0 CHECK (notification_delivered IN (0,1)),
    notification_suppressed_reason TEXT,
    target_kind                    TEXT    CHECK (target_kind IS NULL OR target_kind IN ('recording','job','settings')),
    target_id                      TEXT,
    read                           INTEGER NOT NULL DEFAULT 0 CHECK (read IN (0,1)),
    -- A delivered notification has no suppression reason, and vice versa.
    CHECK (notification_delivered = 0 OR notification_suppressed_reason IS NULL),
    CHECK (target_kind IS NULL OR target_id IS NOT NULL)
) STRICT;

CREATE INDEX ix_activity_occurred_at ON activity_events(occurred_at);

CREATE TABLE event_log (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         TEXT    NOT NULL,
    occurred_at  INTEGER NOT NULL,
    job_id       TEXT,
    recording_id TEXT,
    payload_json TEXT    NOT NULL DEFAULT '{}'
) STRICT;

CREATE TABLE recording_import_metadata (
    recording_id       TEXT PRIMARY KEY NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    original_file_name TEXT NOT NULL,
    imported_at        INTEGER NOT NULL
) STRICT;
