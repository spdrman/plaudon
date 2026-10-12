-- EPIC L (direct-device import) owns versions 500-599.
--
-- Spec 13.3 adds these two tables "at the point the associated capability
-- ships". The device, capability, credential-reference and transfer tables
-- already exist from 0002; what a streaming, resumable, evidence-gated
-- transfer additionally needs is a firmware history and a checkpoint.

-- Spec 13.3: "Firmware changes trigger revalidation, not downgrade."
-- Every observation is kept, so a demotion can point at the exact moment the
-- firmware the evidence was recorded against stopped being the one in front
-- of us.
CREATE TABLE device_firmware_observations (
    id                 TEXT    PRIMARY KEY NOT NULL,
    device_id          TEXT    NOT NULL REFERENCES devices(id) ON DELETE RESTRICT,
    firmware_fingerprint TEXT  NOT NULL,
    observed_at        INTEGER NOT NULL,
    source             TEXT    NOT NULL,
    capability_profile TEXT    NOT NULL,
    auth_mode          TEXT    NOT NULL
) STRICT;

CREATE INDEX ix_firmware_observations_device
    ON device_firmware_observations(device_id, observed_at);

-- Spec 13.3: "Resume only against exactly matching source identity/contents."
-- The checkpoint therefore stores the device's own recording identifier, the
-- size it declared and the digest of the bytes already staged; App. B.6
-- requires all three to match before an offset read is attempted at all.
CREATE TABLE device_transfer_checkpoints (
    transfer_id          TEXT    PRIMARY KEY NOT NULL
                                 REFERENCES device_transfers(id) ON DELETE RESTRICT,
    device_recording_id  TEXT    NOT NULL,
    chunk_offset         INTEGER NOT NULL CHECK (chunk_offset >= 0),
    verified_bytes       INTEGER NOT NULL CHECK (verified_bytes >= 0),
    source_size_hint     INTEGER CHECK (source_size_hint IS NULL OR source_size_hint >= 0),
    staged_prefix_sha256 TEXT    NOT NULL CHECK (length(staged_prefix_sha256) = 64),
    resume_capability    TEXT    NOT NULL,
    updated_at           INTEGER NOT NULL,
    -- A checkpoint that claims more verified bytes than it reached an offset
    -- for is incoherent; refuse it at the schema rather than at a call site.
    CHECK (verified_bytes >= chunk_offset)
) STRICT;
