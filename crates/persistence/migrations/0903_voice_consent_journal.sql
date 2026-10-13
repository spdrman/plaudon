-- The activation half of the consent gate, journalled
-- (VOICE_PROFILES_SPEC VP-120, VP-121, VP-122; ADR-0080).
--
-- §12.2 has three levels, and until now only the third one left a record.
-- A per-enrolment attestation lands in `voice_profile_events` as 'enrolled';
-- the *activation* disclosure — the versioned text the user accepted before
-- any enrolment was possible — lived only in the `app_preferences` gate row,
-- which the next activation overwrites. VP-122 promises "an exportable consent
-- record per profile", and the question an exported record has to answer first
-- is "under which disclosure, and when, was this feature turned on at all".
--
-- Two library-wide kinds therefore join the closed list, and the NULL-person
-- pairing from 0902 is widened to cover them: like 'all_forgotten', activating
-- and deactivating the feature are facts about the library, not about one
-- person, and inventing a sentinel person id would make every later read
-- special-case it.
--
-- The table is rebuilt because SQLite cannot alter a CHECK constraint in
-- place; everything else is copied verbatim, including the deliberate absence
-- of a foreign key to `people` (the journal has to outlive its subject).

CREATE TABLE voice_profile_events_0903 (
    id          TEXT    PRIMARY KEY NOT NULL,
    person_id   TEXT    CHECK (person_id IS NULL OR length(person_id) BETWEEN 1 AND 64),
    kind        TEXT    NOT NULL CHECK (kind IN
                        ('person_created', 'person_renamed', 'person_merged', 'person_deleted',
                         'enrolled', 'exemplar_added', 'enrollment_split', 'enrollment_expired',
                         'enrollment_deleted', 'suggestion_confirmed', 'suggestion_rejected',
                         'suggestion_corrected', 'assignment_set', 'assignment_cleared',
                         'all_forgotten', 'feature_activated', 'feature_deactivated')),
    detail_json TEXT    NOT NULL
                        CHECK (json_valid(detail_json) AND json_type(detail_json) = 'object'),
    created_at  INTEGER NOT NULL,
    -- A library-wide event names no person; every other kind names one.
    CHECK ((kind IN ('all_forgotten', 'feature_activated', 'feature_deactivated'))
           = (person_id IS NULL))
) STRICT;

INSERT INTO voice_profile_events_0903 (id, person_id, kind, detail_json, created_at)
SELECT id, person_id, kind, detail_json, created_at FROM voice_profile_events;

DROP INDEX ix_voice_profile_events_person;
DROP TABLE voice_profile_events;
ALTER TABLE voice_profile_events_0903 RENAME TO voice_profile_events;

CREATE INDEX ix_voice_profile_events_person ON voice_profile_events(person_id, created_at);
