//! The retention expiry maintenance pass (`VOICE_PROFILES_SPEC` VP-130,
//! §12.4; ADR-0072 journalling).
//!
//! A profile nobody has used for its retention term — no match, no added
//! exemplar, no confirmed or corrected suggestion — expires by itself. Expiry
//! happens in two stages, because "gone" and "explained" are different needs:
//!
//! 1. **Expire.** The exemplar ciphertexts are destroyed and the enrolment row
//!    stays as a tombstoned *name* so the People screen can say why matching
//!    stopped for this person. Journalled with the exact count destroyed.
//! 2. **Purge.** [`TOMBSTONE_DAYS`] later the tombstone row goes too, leaving
//!    an ordinary name-only roster entry. Also journalled.
//!
//! # Why it is a journalled sweep and not a loop
//!
//! The pass can be interrupted — it runs at startup, beside the other
//! reconcilers, on a laptop that gets closed. [`VoiceRetentionService::sweep`]
//! therefore opens a row in `voice_retention_sweeps` first and **adopts** an
//! unfinished one instead of starting a new pass, which gives two properties
//! the acceptance criteria name:
//!
//! * **resumable** — a crash mid-pass leaves a `planned` row; the next sweep
//!   continues it, evaluating the clock at the instant the pass was *planned*,
//!   so an interruption cannot move the retention clock forwards behind the
//!   user's back;
//! * **idempotent** — expiring an already-expired enrolment destroys nothing
//!   and journals nothing, so running the pass twice over the same library
//!   produces one set of events, not two.
//!
//! No key is needed anywhere in this file: destroying a ciphertext does not
//! require the ability to read it, which is why expiry keeps working while the
//! OS credential store is locked (ADR-0014).

use plaudon_domain::time::Timestamp;
use plaudon_persistence::voice_profiles::VoiceEnrollmentRepository;
use plaudon_persistence::Database;

use super::store::VoiceProfileResult;

/// How long an expired profile stays as a tombstoned name before the row is
/// removed (VP-130).
pub const TOMBSTONE_DAYS: u64 = 30;

/// Milliseconds in [`TOMBSTONE_DAYS`].
const TOMBSTONE_MS: i64 = (TOMBSTONE_DAYS * 24 * 60 * 60 * 1000) as i64;

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    /// Identity of the sweep row, so a caller can correlate with the journal.
    pub sweep_id: String,
    /// Instant the pass was planned at — the sweep's own clock.
    pub planned_at: Timestamp,
    /// Enrolments expired by this pass.
    pub expired: u32,
    /// Tombstones removed by this pass.
    pub purged: u32,
    /// Enrolments expired across every attempt at this pass, including
    /// attempts a crash interrupted.
    pub expired_total: u32,
    /// Tombstones removed across every attempt at this pass.
    pub purged_total: u32,
    /// Whether this pass continued an interrupted one.
    pub resumed: bool,
}

/// Runs the retention pass over one library.
#[derive(Debug, Clone)]
pub struct VoiceRetentionService {
    enrollments: VoiceEnrollmentRepository,
}

impl VoiceRetentionService {
    /// Wrap a library.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self {
            enrollments: VoiceEnrollmentRepository::new(db),
        }
    }

    /// Expire what the clock says is due, then remove tombstones older than
    /// [`TOMBSTONE_DAYS`].
    ///
    /// # Errors
    ///
    /// [`VoiceProfileError::Persistence`](super::VoiceProfileError::Persistence)
    /// when the library fails. A failure part-way leaves the sweep row open,
    /// which is what makes the next call resume rather than restart.
    pub async fn sweep(&self, now: Timestamp) -> VoiceProfileResult<RetentionReport> {
        let sweep = self.enrollments.open_retention_sweep(now).await?;
        let resumed = sweep.planned_at != now || sweep.expired > 0 || sweep.purged > 0;
        // Evaluate against the instant the pass was planned at, not the instant
        // it is being resumed at: a crash must not advance anybody's clock.
        let evaluated_at = sweep.planned_at;
        let mut expired = 0u32;
        for person_id in self.enrollments.retention_candidates(evaluated_at).await? {
            match self.enrollments.expire(person_id, evaluated_at).await {
                Ok(_) => {
                    expired += 1;
                    self.enrollments
                        .advance_retention_sweep(&sweep.id, 1, 0)
                        .await?;
                }
                // The user deleted this enrolment between the plan and here.
                // Their action is the stronger one; nothing to expire.
                Err(plaudon_persistence::PersistenceError::RecordNotFound) => {}
                Err(error) => return Err(error.into()),
            }
        }

        let cutoff = Timestamp::from_millis(evaluated_at.as_millis().saturating_sub(TOMBSTONE_MS));
        let mut purged = 0u32;
        for person_id in self.enrollments.tombstoned_before(cutoff).await? {
            if self
                .enrollments
                .purge_tombstone(person_id, evaluated_at)
                .await?
            {
                purged += 1;
                self.enrollments
                    .advance_retention_sweep(&sweep.id, 0, 1)
                    .await?;
            }
        }

        self.enrollments
            .complete_retention_sweep(&sweep.id, now)
            .await?;

        Ok(RetentionReport {
            sweep_id: sweep.id,
            planned_at: sweep.planned_at,
            expired,
            purged,
            expired_total: sweep.expired + expired,
            purged_total: sweep.purged + purged,
            resumed,
        })
    }
}
