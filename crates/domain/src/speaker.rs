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
}
