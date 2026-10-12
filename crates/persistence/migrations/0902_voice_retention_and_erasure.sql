-- Retention expiry and wholesale erasure for the voice-profile biometric tier
-- (VOICE_PROFILES_SPEC VP-095, VP-130; ADR-0080; threat-model rows TM-046/TM-048).
--
-- Migration 0900 gave the tier its shape. Three things it did not carry are
-- needed before deletion and retention can be honest about what happened:
--
--   * `person_voice_enrollments.expired_at` — VP-130 keeps an expired profile
--     as a tombstoned *name* for 30 days before the row itself goes. Without
--     the moment of expiry there is no way to tell a row that expired today
--     from one that expired a year ago, so the second half of the retention
--     rule would be unimplementable.
--   * a journal kind for "forget everyone". VP-095 is **one** action over the
--     whole library, not a per-person loop, and it writes one event; the 0900
--     journal could only name a single person per row.
--   * `voice_retention_sweeps` — the expiry pass is a maintenance job that may
--     be interrupted. Recording the sweep makes it resumable (finish the
--     planned sweep) and idempotent (a second pass over the same cutoff
--     expires nothing twice).

ALTER TABLE person_voice_enrollments ADD COLUMN expired_at INTEGER;

-- `voice_profile_events` is rebuilt rather than extended because SQLite cannot
-- alter a CHECK constraint in place. Two changes:
--
--   * `person_id` becomes nullable, so a library-wide event ('all_forgotten')
--     can be recorded without inventing a sentinel person id that later reads
--     would have to special-case;
--   * 'all_forgotten' joins the closed kind list.
--
-- Everything else is copied verbatim, including the deliberate absence of a
-- foreign key to `people`: the journal has to outlive its subject.
CREATE TABLE voice_profile_events_0902 (
    id          TEXT    PRIMARY KEY NOT NULL,
    person_id   TEXT    CHECK (person_id IS NULL OR length(person_id) BETWEEN 1 AND 64),
    kind        TEXT    NOT NULL CHECK (kind IN
                        ('person_created', 'person_renamed', 'person_merged', 'person_deleted',
                         'enrolled', 'exemplar_added', 'enrollment_split', 'enrollment_expired',
                         'enrollment_deleted', 'suggestion_confirmed', 'suggestion_rejected',
                         'suggestion_corrected', 'assignment_set', 'assignment_cleared',
                         'all_forgotten')),
    detail_json TEXT    NOT NULL
                        CHECK (json_valid(detail_json) AND json_type(detail_json) = 'object'),
    created_at  INTEGER NOT NULL,
    -- A library-wide event names no person; every other kind names one.
    CHECK ((kind = 'all_forgotten') = (person_id IS NULL))
) STRICT;

INSERT INTO voice_profile_events_0902 (id, person_id, kind, detail_json, created_at)
SELECT id, person_id, kind, detail_json, created_at FROM voice_profile_events;

DROP INDEX ix_voice_profile_events_person;
DROP TABLE voice_profile_events;
ALTER TABLE voice_profile_events_0902 RENAME TO voice_profile_events;

CREATE INDEX ix_voice_profile_events_person ON voice_profile_events(person_id, created_at);

-- One row per retention pass. The sweep plans its work, expires enrolments one
-- transaction at a time, then completes; an interrupted pass leaves a 'planned'
-- row that the next start finishes with the cutoff it was planned under, so a
-- crash can neither skip an expiry nor move the clock forwards behind the
-- user's back.
CREATE TABLE voice_retention_sweeps (
    id            TEXT    PRIMARY KEY NOT NULL,
    -- Instant the pass was planned at; the per-enrolment cutoff is derived from
    -- it and each enrolment's own retention term, which may be shorter.
    planned_at    INTEGER NOT NULL,
    state         TEXT    NOT NULL CHECK (state IN ('planned', 'completed')),
    -- What the plan found, and what it has done so far. The counts are updated
    -- as the pass progresses, so a resumed sweep reports the total across both
    -- attempts rather than only the second one.
    expired_count INTEGER NOT NULL DEFAULT 0 CHECK (expired_count >= 0),
    purged_count  INTEGER NOT NULL DEFAULT 0 CHECK (purged_count >= 0),
    completed_at  INTEGER,
    CHECK ((state = 'completed') = (completed_at IS NOT NULL))
) STRICT;

CREATE INDEX ix_voice_retention_sweeps_state ON voice_retention_sweeps(state, planned_at);
