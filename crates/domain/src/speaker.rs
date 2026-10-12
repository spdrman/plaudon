//! How a speaker may be presented (spec 8.7, 17.6).
//!
//! Diarization produces **clusters**, not identities. `Speaker 1` names a
//! cluster; a human name exists only because a user typed it, and it is
//! presented as identity only after that user confirmed it. The wording that
//! carries that distinction is defined once, here, because three independent
//! layers render it: the export bundle (`plaudon-export`), the speaker
//! registry (`plaudon-app-core`) and the desktop interface (`plaudon-ui`).
//! A second copy of the wording is exactly how one of those surfaces ends up
//! quietly presenting an inferred cluster as a person.
//!
//! This module is in `plaudon-domain` because it is the only crate all three
//! already depend on, and the rule needs nothing else: no database, no
//! platform, no allocation beyond the string it returns.

/// Qualifier for an identity a human confirmed.
pub const CONFIRMED: &str = "confirmed";

/// Qualifier for a row that is still a diarization cluster.
pub const INFERRED_CLUSTER: &str = "inferred cluster";

/// Maximum stored speaker name length, in characters.
pub const MAX_NAME_CHARS: usize = 120;

/// The qualifier that must accompany a speaker label.
#[must_use]
pub const fn qualifier(user_confirmed: bool) -> &'static str {
    if user_confirmed {
        CONFIRMED
    } else {
        INFERRED_CLUSTER
    }
}

/// Speaker display text that never presents an inferred cluster as identity.
///
/// `label` is the row's current label and `name` the name a user typed, if
/// any. An unconfirmed row renders its label with the cluster qualifier
/// whatever has been typed into it, so a guess cannot be read as a claim.
#[must_use]
pub fn display(label: &str, name: Option<&str>, user_confirmed: bool) -> String {
    match (name, user_confirmed) {
        (Some(name), true) if !name.is_empty() => format!("{name} ({CONFIRMED})"),
        _ => format!("{label} ({INFERRED_CLUSTER})"),
    }
}

/// Qualifier for a name a voice match proposed and a user then confirmed.
///
/// It is deliberately longer than [`CONFIRMED`]: the user agreed, so the row
/// is an identity, but how the name was proposed stays visible, because a
/// confirmed suggestion and a typed name are not the same evidence.
pub const VOICE_MATCH_PROVENANCE: &str = "named via voice match (confirmed by you)";

/// What a match score is, for the surface that shows one.
///
/// Kept beside the wording it qualifies: a number that looks like a
/// probability will be read as one unless the sentence beside it says
/// otherwise.
pub const SCORE_TOOLTIP: &str =
    "A match score is how similar two voice samples sound, not the probability that this is the person.";

/// How strong a recorded voice match is, as the user sees it (VP-172).
///
/// There is no third band: a score below the floor records nothing, so it
/// never reaches a surface, and nothing above `Strong` exists because no score
/// assigns a speaker on its own (ADR-0082).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchBand {
    /// Between the floor and the suggest threshold: shown on request, never
    /// surfaced as a notification or a queue entry.
    Possible,
    /// At or above the suggest threshold: a suggestion the user may settle.
    Strong,
}

impl MatchBand {
    /// The band for a stored `speaker_suggestions.band` value.
    ///
    /// Accepts exactly the two values that column holds; anything else is
    /// `None`, so a row written by a future version renders no claim at all
    /// rather than the nearest guess.
    #[must_use]
    pub fn from_column(value: &str) -> Option<Self> {
        match value {
            "weak" => Some(Self::Possible),
            "suggest" => Some(Self::Strong),
            _ => None,
        }
    }

    /// The words shown for this band.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Possible => "possible match",
            Self::Strong => "strong match",
        }
    }
}

/// The one sentence that presents a voice match (VP-172).
///
/// Renders as `sounds like Priya — strong match, 0.82`. It says what the
/// system noticed, not who the speaker is: "sounds like" is a comparison and
/// the band and score are its evidence. No surface composes this sentence
/// itself, because the second copy is where "identified" appears.
#[must_use]
pub fn suggestion(person: &str, band: MatchBand, score: f64) -> String {
    format!("sounds like {person} — {}, {score:.2}", band.label())
}

/// Speaker display text for a row named by a confirmed voice match.
///
/// Identical to [`display`] except for the qualifier: a confirmed row carries
/// [`VOICE_MATCH_PROVENANCE`] so the match stays visible after the fact, and
/// an unconfirmed row still renders as an inferred cluster, because a
/// suggestion nobody settled is not an identity.
#[must_use]
pub fn display_via_voice_match(label: &str, name: Option<&str>, user_confirmed: bool) -> String {
    match (name, user_confirmed) {
        (Some(name), true) if !name.is_empty() => format!("{name} ({VOICE_MATCH_PROVENANCE})"),
        _ => format!("{label} ({INFERRED_CLUSTER})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unconfirmed_name_is_never_rendered_as_identity() {
        assert_eq!(
            display("Speaker 1", Some("Dana"), false),
            "Speaker 1 (inferred cluster)"
        );
        assert_eq!(display("Dana", Some("Dana"), true), "Dana (confirmed)");
        assert_eq!(
            display("Speaker 2", None, false),
            "Speaker 2 (inferred cluster)"
        );
    }

    #[test]
    fn a_confirmed_row_with_an_empty_name_falls_back_to_the_cluster_label() {
        // Nothing stores an empty name, but a confirmed row that somehow had
        // one must not render as " (confirmed)".
        assert_eq!(
            display("Speaker 1", Some(""), true),
            "Speaker 1 (inferred cluster)"
        );
    }

    #[test]
    fn the_qualifier_matches_the_rendered_one() {
        assert!(display("Dana", Some("Dana"), true).ends_with(&format!("({})", qualifier(true))));
        assert!(display("Speaker 1", None, false).ends_with(&format!("({})", qualifier(false))));
    }

    #[test]
    fn a_match_sentence_reads_as_a_comparison_and_never_as_an_identity() {
        let sentence = suggestion("Priya", MatchBand::Strong, 0.82);
        assert_eq!(sentence, "sounds like Priya — strong match, 0.82");
        for forbidden in ["identified", "verified", "recognised", "recognized"] {
            assert!(
                !sentence.contains(forbidden),
                "{sentence} must not contain {forbidden}"
            );
        }
    }

    #[test]
    fn a_score_always_renders_with_two_decimals() {
        assert_eq!(
            suggestion("Dana", MatchBand::Possible, 0.5),
            "sounds like Dana — possible match, 0.50"
        );
        assert_eq!(
            suggestion("Dana", MatchBand::Possible, 0.816),
            "sounds like Dana — possible match, 0.82"
        );
    }

    #[test]
    fn only_the_two_stored_band_values_are_a_band() {
        assert_eq!(MatchBand::from_column("weak"), Some(MatchBand::Possible));
        assert_eq!(MatchBand::from_column("suggest"), Some(MatchBand::Strong));
        assert_eq!(MatchBand::from_column("none"), None);
        assert_eq!(MatchBand::from_column(""), None);
        assert_eq!(MatchBand::from_column("Suggest"), None);
    }

    #[test]
    fn a_voice_matched_row_carries_its_provenance_only_once_confirmed() {
        assert_eq!(
            display_via_voice_match("Priya", Some("Priya"), true),
            "Priya (named via voice match (confirmed by you))"
        );
        assert_eq!(
            display_via_voice_match("Speaker 1", Some("Priya"), false),
            "Speaker 1 (inferred cluster)"
        );
        assert_eq!(
            display_via_voice_match("Speaker 1", None, false),
            "Speaker 1 (inferred cluster)"
        );
    }

    #[test]
    fn the_score_tooltip_denies_that_a_score_is_a_probability() {
        assert!(SCORE_TOOLTIP.contains("not the probability"));
    }
}
