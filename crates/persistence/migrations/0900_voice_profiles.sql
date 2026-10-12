-- Voice profiles: the master speaker list and its optional biometric tier
-- (VOICE_PROFILES_SPEC VP-010, VP-011, VP-070..VP-080, ADR-0080).
--
-- The schema is deliberately two-tiered, because the law and the product draw
-- the line in the same place (spec §12.2A):
--
--   * `people` is a **roster**: typed names, aliases and plain contacts. It is
--     ordinary personal data, it is useful with biometric matching switched
--     off, and nothing in it is derived from anybody's voice.
--   * `person_voice_enrollments` + `voice_exemplars` + `speaker_suggestions`
--     are the **biometric tier**. They only come into existence after the
--     consent gate, they carry the retention clock, and an embedding exists at
--     rest only as AEAD ciphertext whose key lives in the OS secret store.
--
-- Deleting the enrolment therefore has to leave the person behind (the user
-- asked to forget a voiceprint, not a colleague), and deleting the person has
-- to leave each recording's own `speakers.name` snapshot behind (a roster edit
-- must never make a recording silently lose its labels, VP-213/VP-080). Both
-- of those are expressed here as `ON DELETE` behaviour rather than as
-- application code, so no repository bug can produce the other outcome.

-- The master speaker list. One per library, independent of any recording.
CREATE TABLE people (
    id            TEXT    PRIMARY KEY NOT NULL,
    display_name  TEXT    NOT NULL CHECK (length(display_name) BETWEEN 1 AND 120),
    -- Alternate spellings, used for search and merge-candidate detection only.
    aliases_json  TEXT    NOT NULL DEFAULT '[]'
                  CHECK (json_valid(aliases_json) AND json_type(aliases_json) = 'array'),
    email         TEXT    CHECK (email IS NULL OR length(email) BETWEEN 1 AND 254),
    organisation  TEXT    CHECK (organisation IS NULL OR length(organisation) BETWEEN 1 AND 200),
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    -- 'merged' is a tombstone kept so the journal's person ids stay resolvable
    -- after a merge (VP-213c); it is never shown as a pickable roster entry,
    -- and the survivor is named in the `person_merged` journal event.
    status        TEXT    NOT NULL CHECK (status IN ('active', 'merged'))
) STRICT;

CREATE INDEX ix_people_status_name ON people(status, display_name);

-- A person's optional biometric attachment, at most one per person. Deleting
-- it leaves a name-only roster entry (VP-080).
CREATE TABLE person_voice_enrollments (
    person_id        TEXT    PRIMARY KEY NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    -- Version of the disclosure text the user was shown when they attested
    -- that the named person agreed (VP-120..VP-123).
    consent_version  TEXT    NOT NULL CHECK (length(consent_version) BETWEEN 1 AND 64),
    consent_note     TEXT    CHECK (consent_note IS NULL OR length(consent_note) <= 2000),
    -- The §12.4 retention clock. 12 months is the outer bound (Texas CUBI);
    -- the user may shorten it, never lengthen it.
    retention_months INTEGER NOT NULL DEFAULT 12 CHECK (retention_months BETWEEN 1 AND 12),
    last_matched_at  INTEGER,
    status           TEXT    NOT NULL CHECK (status IN ('active', 'expired')),
    created_at       INTEGER NOT NULL
) STRICT;

-- One stored embedding with its full provenance. The vector is never at rest
-- in plaintext: `embedding_cipher` is XChaCha20-Poly1305 output under the
-- library's `SecretScope::VoiceProfileKey` data key (VP-071).
CREATE TABLE voice_exemplars (
    id                 TEXT    PRIMARY KEY NOT NULL,
    person_id          TEXT    NOT NULL REFERENCES person_voice_enrollments(person_id)
                               ON DELETE CASCADE,
    -- Provenance, not ownership: deleting the source recording keeps the
    -- exemplar (it belongs to the enrolment) and forgets where it came from.
    recording_id       TEXT    REFERENCES recordings(id) ON DELETE SET NULL,
    diarization_run_id TEXT    REFERENCES diarization_runs(id) ON DELETE SET NULL,
    cluster_key        TEXT,
    source_start_ms    INTEGER CHECK (source_start_ms IS NULL OR source_start_ms >= 0),
    source_end_ms      INTEGER CHECK (source_end_ms IS NULL OR source_end_ms >= 0),
    -- VP-064: an exemplar below the enrolment minimum is not storable.
    speech_ms          INTEGER NOT NULL CHECK (speech_ms >= 1000),
    model_id           TEXT    NOT NULL CHECK (length(model_id) BETWEEN 1 AND 200),
    model_version      TEXT    NOT NULL CHECK (length(model_version) BETWEEN 1 AND 64),
    model_sha256       TEXT    NOT NULL CHECK (length(model_sha256) = 64),
    embedding_dim      INTEGER NOT NULL CHECK (embedding_dim BETWEEN 1 AND 8192),
    embedding_cipher   BLOB    NOT NULL CHECK (length(embedding_cipher) >= 16),
    embedding_nonce    BLOB    NOT NULL CHECK (length(embedding_nonce) = 24),
    created_at         INTEGER NOT NULL,
    CHECK (source_start_ms IS NULL OR source_end_ms IS NULL OR source_end_ms >= source_start_ms)
) STRICT;

CREATE INDEX ix_voice_exemplars_person ON voice_exemplars(person_id, created_at);
CREATE INDEX ix_voice_exemplars_recording ON voice_exemplars(recording_id);
CREATE INDEX ix_voice_exemplars_model ON voice_exemplars(model_id, model_version);

-- Pending and settled match suggestions. A suggestion is a score, never a
-- label (ADR-0082): nothing here changes a transcript until a human confirms.
-- A rejected row doubles as negative evidence for per-enrolment calibration —
-- the score is kept, the rejected cluster's embedding never is (VP-216).
CREATE TABLE speaker_suggestions (
    id                  TEXT    PRIMARY KEY NOT NULL,
    recording_id        TEXT    NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    diarization_run_id  TEXT    NOT NULL REFERENCES diarization_runs(id) ON DELETE CASCADE,
    cluster_key         TEXT    NOT NULL CHECK (length(cluster_key) BETWEEN 1 AND 200),
    person_id           TEXT    NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    score               REAL    NOT NULL,
    score_kind          TEXT    NOT NULL CHECK (score_kind IN ('cosine_similarity_asnorm')),
    -- Two thresholds, three outcomes (VP-061): below the weak band nothing is
    -- persisted at all, so there is no 'none' band here.
    band                TEXT    NOT NULL CHECK (band IN ('suggest', 'weak')),
    state               TEXT    NOT NULL CHECK (state IN
                                ('pending', 'confirmed', 'rejected', 'corrected', 'expired')),
    -- VP-216 reject-then-pick: who the cluster actually was.
    corrected_person_id TEXT    REFERENCES people(id) ON DELETE SET NULL,
    decided_at          INTEGER,
    model_id            TEXT    NOT NULL CHECK (length(model_id) BETWEEN 1 AND 200),
    model_version       TEXT    NOT NULL CHECK (length(model_version) BETWEEN 1 AND 64),
    created_at          INTEGER NOT NULL,
    -- Re-running the match job is idempotent (VP-102).
    UNIQUE (diarization_run_id, cluster_key, person_id),
    -- A correction names somebody, and only a correction does.
    CHECK (state <> 'corrected' OR corrected_person_id IS NOT NULL),
    CHECK (corrected_person_id IS NULL OR state = 'corrected'),
    CHECK (corrected_person_id IS NULL OR corrected_person_id <> person_id),
    -- A settled suggestion carries the moment it was settled; a pending one
    -- cannot pretend it was decided.
    CHECK (state = 'pending' OR decided_at IS NOT NULL),
    CHECK (state <> 'pending' OR decided_at IS NULL)
) STRICT;

CREATE INDEX ix_speaker_suggestions_recording ON speaker_suggestions(recording_id, state);
CREATE INDEX ix_speaker_suggestions_person ON speaker_suggestions(person_id, state);

-- Append-only journal of roster and consent events (§12.3, VP-080).
--
-- `person_id` is deliberately NOT a foreign key: the journal has to survive the
-- deletion of the person it describes, otherwise "when did I delete this
-- profile, and what did the disclosure say?" becomes unanswerable exactly when
-- it matters. `detail_json` carries counts and versions only — never an
-- embedding, never a score against a third person.
CREATE TABLE voice_profile_events (
    id          TEXT    PRIMARY KEY NOT NULL,
    person_id   TEXT    NOT NULL CHECK (length(person_id) BETWEEN 1 AND 64),
    kind        TEXT    NOT NULL CHECK (kind IN
                        ('person_created', 'person_renamed', 'person_merged', 'person_deleted',
                         'enrolled', 'exemplar_added', 'enrollment_split', 'enrollment_expired',
                         'enrollment_deleted', 'suggestion_confirmed', 'suggestion_rejected',
                         'suggestion_corrected', 'assignment_set', 'assignment_cleared')),
    detail_json TEXT    NOT NULL
                        CHECK (json_valid(detail_json) AND json_type(detail_json) = 'object'),
    created_at  INTEGER NOT NULL
) STRICT;

CREATE INDEX ix_voice_profile_events_person ON voice_profile_events(person_id, created_at);

-- The link from a per-recording speaker row to the roster (VP-072). The row's
-- `name` stays a text snapshot beside the link: severing the link (person
-- deleted) must not erase what the recording already displayed.
ALTER TABLE speakers ADD COLUMN person_id TEXT REFERENCES people(id) ON DELETE SET NULL;

CREATE INDEX ix_speakers_person ON speakers(person_id);
