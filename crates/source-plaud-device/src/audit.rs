//! The non-destructive audit that gates automatic sync (spec §7.9, §17.3A,
//! §22(14), App. B.6).
//!
//! > "We will specifically investigate whether the legacy `getFileList` call
//! > or a read request implicitly changes delta state; if it does, we will
//! > develop a demonstrated nondestructive policy or block that firmware from
//! > automatic sync." (App. B.6)
//!
//! The audit is mechanical: list, read, list again, and compare. If the
//! device's own listing or its delta cursor moved because we looked at it,
//! the profile is **not** eligible for automatic nearby sync, no matter what
//! the compatibility register says. [`SyncEligibility`] is the only type that
//! answers "may this profile sync by itself", and it is constructed solely
//! from an audit outcome.

use std::collections::BTreeMap;

use plaudon_plaud_protocol::payload::ListedRecording;
use serde::{Deserialize, Serialize};

/// One observation of the device's listing state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditObservation {
    /// Identifiers the device reported, with their declared lengths.
    pub entries: BTreeMap<String, u64>,
    /// Whether the device said its listing was truncated.
    pub truncated: bool,
    /// Delta cursor the device reported, when the profile exposes one.
    pub delta_cursor: Option<String>,
}

impl AuditObservation {
    /// Build an observation from a decoded listing.
    #[must_use]
    pub fn from_listing(
        entries: &[ListedRecording],
        truncated: bool,
        delta_cursor: Option<String>,
    ) -> Self {
        Self {
            entries: entries
                .iter()
                .map(|entry| (entry.id.clone(), entry.declared_byte_length))
                .collect(),
            truncated,
            delta_cursor,
        }
    }
}

/// What changed between two observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AuditOutcome {
    /// The audit has not been run for this profile.
    NotRun,
    /// Nothing the device reported changed because we read from it.
    Unchanged {
        /// Number of listings compared.
        rounds: u32,
    },
    /// The device's listing changed between identical read-only passes.
    ListingChanged {
        /// Identifiers that disappeared.
        disappeared: Vec<String>,
        /// Identifiers that appeared.
        appeared: Vec<String>,
    },
    /// The device's delta cursor moved.
    CursorMoved,
    /// The audit could not complete, for example the device disconnected.
    Inconclusive {
        /// Short machine-readable reason.
        reason: String,
    },
}

impl AuditOutcome {
    /// Whether the audit demonstrated that reading does not mutate the device.
    #[must_use]
    pub const fn is_non_destructive(&self) -> bool {
        matches!(self, Self::Unchanged { .. })
    }

    /// A stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotRun => "NOT_RUN",
            Self::Unchanged { .. } => "UNCHANGED",
            Self::ListingChanged { .. } => "LISTING_CHANGED",
            Self::CursorMoved => "CURSOR_MOVED",
            Self::Inconclusive { .. } => "INCONCLUSIVE",
        }
    }

    /// What the interface tells the user about automatic sync.
    #[must_use]
    pub const fn ui_explanation(&self) -> &'static str {
        match self {
            Self::NotRun => {
                "Automatic transfer when nearby is off: this device profile has not passed a \
                 non-destructive audit yet."
            }
            Self::Unchanged { .. } => {
                "Repeated read-only passes left the device's own file list unchanged."
            }
            Self::ListingChanged { .. } => {
                "Automatic transfer when nearby is unavailable: reading from this device changed \
                 its own list of files, so repeated background scans could lose recordings."
            }
            Self::CursorMoved => {
                "Automatic transfer when nearby is unavailable: reading from this device advanced \
                 its synchronization cursor."
            }
            Self::Inconclusive { .. } => {
                "Automatic transfer when nearby is off: the audit did not finish, so nothing was \
                 proven either way."
            }
        }
    }
}

/// Whether a profile may transfer automatically when the device is nearby.
///
/// The only constructor is [`SyncEligibility::from_audit`], so an eligibility
/// cannot be produced without an audit outcome in hand (fail closed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncEligibility {
    profile_name: String,
    outcome: AuditOutcome,
    cell_permits_transfer: bool,
}

impl SyncEligibility {
    /// Derive eligibility from an audit outcome and the compatibility gate.
    #[must_use]
    pub fn from_audit(
        profile_name: impl Into<String>,
        outcome: AuditOutcome,
        cell_permits_transfer: bool,
    ) -> Self {
        Self {
            profile_name: profile_name.into(),
            outcome,
            cell_permits_transfer,
        }
    }

    /// Eligibility for a profile that has never been audited.
    #[must_use]
    pub fn unaudited(profile_name: impl Into<String>) -> Self {
        Self::from_audit(profile_name, AuditOutcome::NotRun, false)
    }

    /// The profile this verdict is about.
    #[must_use]
    pub fn profile_name(&self) -> &str {
        &self.profile_name
    }

    /// The audit outcome behind the verdict.
    #[must_use]
    pub const fn outcome(&self) -> &AuditOutcome {
        &self.outcome
    }

    /// Whether automatic "sync when nearby" may be offered at all.
    #[must_use]
    pub const fn may_offer_automatic_sync(&self) -> bool {
        self.cell_permits_transfer && self.outcome.is_non_destructive()
    }

    /// Whether automatic sync is on by default. It never is (spec §7.9).
    #[must_use]
    pub const fn automatic_sync_default_on(&self) -> bool {
        false
    }
}

/// Runs the comparison between read-only passes.
#[derive(Debug, Clone, Default)]
pub struct NonDestructiveAudit {
    rounds: u32,
    baseline: Option<AuditObservation>,
    outcome: Option<AuditOutcome>,
}

impl NonDestructiveAudit {
    /// Start an audit.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rounds: 0,
            baseline: None,
            outcome: None,
        }
    }

    /// Record one observation.
    ///
    /// The first observation is the baseline; every later one is compared
    /// against it. The first difference is sticky: a device that mutated once
    /// cannot pass by behaving on the next round.
    pub fn observe(&mut self, observation: AuditObservation) -> &AuditOutcome {
        self.rounds += 1;
        let Some(baseline) = &self.baseline else {
            self.baseline = Some(observation);
            self.outcome = Some(AuditOutcome::Unchanged {
                rounds: self.rounds,
            });
            return self.outcome.as_ref().expect("just set");
        };

        if matches!(
            self.outcome,
            Some(AuditOutcome::ListingChanged { .. } | AuditOutcome::CursorMoved)
        ) {
            return self.outcome.as_ref().expect("already failed");
        }

        if baseline.delta_cursor != observation.delta_cursor {
            self.outcome = Some(AuditOutcome::CursorMoved);
            return self.outcome.as_ref().expect("just set");
        }

        let disappeared: Vec<String> = baseline
            .entries
            .keys()
            .filter(|id| !observation.entries.contains_key(*id))
            .cloned()
            .collect();
        let appeared: Vec<String> = observation
            .entries
            .keys()
            .filter(|id| !baseline.entries.contains_key(*id))
            .cloned()
            .collect();
        if disappeared.is_empty() && appeared.is_empty() {
            self.outcome = Some(AuditOutcome::Unchanged {
                rounds: self.rounds,
            });
        } else {
            self.outcome = Some(AuditOutcome::ListingChanged {
                disappeared,
                appeared,
            });
        }
        self.outcome.as_ref().expect("just set")
    }

    /// Abandon the audit with a reason.
    pub fn abandon(&mut self, reason: impl Into<String>) -> &AuditOutcome {
        self.outcome = Some(AuditOutcome::Inconclusive {
            reason: reason.into(),
        });
        self.outcome.as_ref().expect("just set")
    }

    /// The outcome so far.
    ///
    /// A single pass proves nothing: at least two observations are needed
    /// before "unchanged" means anything, so one round stays `NotRun`.
    #[must_use]
    pub fn outcome(&self) -> AuditOutcome {
        match &self.outcome {
            Some(AuditOutcome::Unchanged { rounds }) if *rounds < 2 => AuditOutcome::NotRun,
            Some(outcome) => outcome.clone(),
            None => AuditOutcome::NotRun,
        }
    }

    /// Observations recorded.
    #[must_use]
    pub const fn rounds(&self) -> u32 {
        self.rounds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(ids: &[&str]) -> AuditObservation {
        AuditObservation {
            entries: ids.iter().map(|id| ((*id).to_owned(), 4_096u64)).collect(),
            truncated: false,
            delta_cursor: Some("cursor-1".into()),
        }
    }

    #[test]
    fn a_single_pass_proves_nothing() {
        let mut audit = NonDestructiveAudit::new();
        audit.observe(listing(&["REC1", "REC2"]));
        assert_eq!(audit.outcome(), AuditOutcome::NotRun);
        assert!(!audit.outcome().is_non_destructive());
    }

    #[test]
    fn two_identical_passes_pass_the_audit() {
        let mut audit = NonDestructiveAudit::new();
        audit.observe(listing(&["REC1", "REC2"]));
        audit.observe(listing(&["REC1", "REC2"]));
        assert_eq!(audit.outcome(), AuditOutcome::Unchanged { rounds: 2 });
        assert!(audit.outcome().is_non_destructive());
    }

    #[test]
    fn a_listing_that_shrinks_after_a_read_fails_the_audit() {
        let mut audit = NonDestructiveAudit::new();
        audit.observe(listing(&["REC1", "REC2"]));
        let outcome = audit.observe(listing(&["REC1"])).clone();
        assert_eq!(
            outcome,
            AuditOutcome::ListingChanged {
                disappeared: vec!["REC2".into()],
                appeared: vec![],
            }
        );
        assert!(!outcome.is_non_destructive());
        assert!(outcome.ui_explanation().contains("changed its own list"));
    }

    #[test]
    fn a_moved_cursor_fails_the_audit() {
        let mut audit = NonDestructiveAudit::new();
        audit.observe(listing(&["REC1"]));
        let mut moved = listing(&["REC1"]);
        moved.delta_cursor = Some("cursor-2".into());
        assert_eq!(audit.observe(moved), &AuditOutcome::CursorMoved);
        assert!(!audit.outcome().is_non_destructive());
    }

    #[test]
    fn a_failure_is_sticky() {
        let mut audit = NonDestructiveAudit::new();
        audit.observe(listing(&["REC1", "REC2"]));
        audit.observe(listing(&["REC1"]));
        audit.observe(listing(&["REC1", "REC2"]));
        assert!(
            !audit.outcome().is_non_destructive(),
            "behaving later does not undo an observed mutation"
        );
    }

    #[test]
    fn an_abandoned_audit_is_inconclusive_not_passing() {
        let mut audit = NonDestructiveAudit::new();
        audit.observe(listing(&["REC1"]));
        audit.observe(listing(&["REC1"]));
        assert!(audit.outcome().is_non_destructive());
        audit.abandon("device_disconnected");
        assert_eq!(
            audit.outcome(),
            AuditOutcome::Inconclusive {
                reason: "device_disconnected".into()
            }
        );
        assert!(!audit.outcome().is_non_destructive());
    }

    #[test]
    fn automatic_sync_needs_both_a_passing_audit_and_a_permitting_cell() {
        let passing = AuditOutcome::Unchanged { rounds: 3 };
        assert!(
            SyncEligibility::from_audit("plaud-note-legacy-v0095", passing.clone(), true)
                .may_offer_automatic_sync()
        );
        assert!(
            !SyncEligibility::from_audit("plaud-note-legacy-v0095", passing, false)
                .may_offer_automatic_sync(),
            "an audit cannot substitute for a validated compatibility cell"
        );
        assert!(!SyncEligibility::from_audit(
            "plaud-note-legacy-v0095",
            AuditOutcome::CursorMoved,
            true
        )
        .may_offer_automatic_sync());
    }

    #[test]
    fn automatic_sync_is_never_on_by_default() {
        let eligible = SyncEligibility::from_audit(
            "plaud-note-legacy-v0095",
            AuditOutcome::Unchanged { rounds: 5 },
            true,
        );
        assert!(eligible.may_offer_automatic_sync());
        assert!(
            !eligible.automatic_sync_default_on(),
            "eligibility is permission to offer, not permission to run"
        );
    }

    #[test]
    fn an_unaudited_profile_is_not_eligible() {
        let eligibility = SyncEligibility::unaudited("plaud-note-legacy-v0095");
        assert_eq!(eligibility.outcome(), &AuditOutcome::NotRun);
        assert!(!eligibility.may_offer_automatic_sync());
        assert!(eligibility.outcome().ui_explanation().contains("off"));
    }

    #[test]
    fn every_outcome_has_a_distinct_code_and_explanation() {
        let outcomes = [
            AuditOutcome::NotRun,
            AuditOutcome::Unchanged { rounds: 2 },
            AuditOutcome::ListingChanged {
                disappeared: vec![],
                appeared: vec![],
            },
            AuditOutcome::CursorMoved,
            AuditOutcome::Inconclusive { reason: "x".into() },
        ];
        let mut codes: Vec<&str> = outcomes.iter().map(AuditOutcome::code).collect();
        codes.sort_unstable();
        let total = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), total);
        for outcome in &outcomes {
            assert!(!outcome.ui_explanation().is_empty());
        }
    }
}
