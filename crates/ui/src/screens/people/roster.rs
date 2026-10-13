//! The roster list of the People screen (`VOICE_PROFILES_SPEC` VP-091,
//! VP-213, VP-214).
//!
//! One row per person, and every fact on it comes from `list_people`: how many
//! recordings assign them, whether a voice is enrolled, what that enrolment is
//! made of, and when it expires. The row offers the edits; it carries none of
//! them out, because the screen owns the dialogs and the dialogs are where the
//! disclosures live.
//!
//! The dedupe groups at the bottom are **candidates**. Nothing is ever merged
//! automatically — a merge moves one person's whole history onto another and
//! is reversible only through the journalled split, so it is always an
//! explicit decision (VP-213c, VP-214).

use egui::Ui;
use plaudon_app::dto::{ListPeopleResponse, PersonDto, PersonVoiceDto};
use plaudon_app::tone::Tone;
use plaudon_domain::time::Timestamp;

use crate::ai::Badge;

/// Milliseconds in a day, for the expiry countdown.
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// Said above the merge candidates, so a group reads as an offer.
pub const CANDIDATES_ARE_SUGGESTIONS: &str =
    "These entries have the same name. Nothing is merged automatically — merging is always your \
     decision.";

/// What a row asked the screen to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterAction {
    /// Edit name, aliases and contacts.
    Edit(String),
    /// Merge one entry into another.
    Merge {
        /// The entry that becomes a merged-away tombstone.
        loser: String,
        /// The entry that survives and receives everything.
        survivor: String,
    },
    /// Delete an entry.
    Delete(String),
    /// Split one enrolment's exemplars by provenance onto another person
    /// (§3.5, VP-091).
    SplitEnrollment(String),
    /// Delete one person's voiceprint, keeping the person.
    DeleteEnrollment(String),
}

/// Draw the roster and report what the user asked for.
///
/// `matching_on` hides every enrolment affordance when biometric matching is
/// off: with it off there is no voiceprint to show, delete or explain, and an
/// affordance for one would be an invitation to turn the feature on by
/// accident (VP-215).
pub fn list(
    ui: &mut Ui,
    response: &ListPeopleResponse,
    matching_on: bool,
    now_ms: i64,
) -> Option<RosterAction> {
    let mut action = None;
    if response.people.is_empty() {
        ui.label("Nobody is on the list yet. Add a person, or assign one from a recording.");
    }
    for person in &response.people {
        ui.push_id(person.person_id.to_string(), |ui| {
            ui.group(|ui| {
                if let Some(chosen) = row(ui, person, &response.people, matching_on, now_ms) {
                    action = Some(chosen);
                }
            });
        });
    }

    if !response.merge_candidates.is_empty() {
        ui.separator();
        ui.label(egui::RichText::new("Possible duplicates").heading());
        ui.label(CANDIDATES_ARE_SUGGESTIONS);
        for group in &response.merge_candidates {
            ui.push_id(group.normalised_name.clone(), |ui| {
                ui.group(|ui| {
                    let names: Vec<&str> = group
                        .people
                        .iter()
                        .map(|person| person.display_name.as_str())
                        .collect();
                    ui.label(format!("{} — {} entries", names.join(", "), names.len()));
                    let Some(survivor) = group.people.first() else {
                        return;
                    };
                    for candidate in group.people.iter().skip(1) {
                        if ui
                            .button(format!(
                                "Merge {} into {}…",
                                candidate.display_name, survivor.display_name
                            ))
                            .clicked()
                        {
                            action = Some(RosterAction::Merge {
                                loser: candidate.person_id.to_string(),
                                survivor: survivor.person_id.to_string(),
                            });
                        }
                    }
                });
            });
        }
    }
    action
}

/// One person's row.
fn row(
    ui: &mut Ui,
    person: &PersonDto,
    roster: &[PersonDto],
    matching_on: bool,
    now_ms: i64,
) -> Option<RosterAction> {
    let mut action = None;
    ui.label(egui::RichText::new(&person.display_name).strong());
    if !person.aliases.is_empty() {
        ui.label(format!("Also known as {}", person.aliases.join(", ")));
    }
    if let Some(email) = &person.email {
        ui.label(format!("Email {email}"));
    }
    if let Some(organisation) = &person.organisation {
        ui.label(format!("Organisation {organisation}"));
    }
    ui.label(assignment_count(person.recordings));
    if person.status == "merged" {
        ui.label("Merged into another entry. Kept so the journal still explains what happened.");
    }

    if matching_on {
        match &person.voice {
            None => {
                Badge::new("—", "voice: none", Tone::Neutral)
                    .description(
                        "No voiceprint is stored for this person. Open a recording where they \
                         speak and choose “Enrol this speaker's voice…” there, so the enrolment \
                         is anchored to speech you can hear.",
                    )
                    .ui(ui);
            }
            Some(voice) => {
                let tone = if voice.status == "active" {
                    Tone::Success
                } else {
                    Tone::Warning
                };
                Badge::new("●", &format!("voice: {}", voice.status), tone).ui(ui);
                ui.label(enrolment_facts(voice));
                ui.label(expiry(voice, now_ms));
                if person.pending_suggestions > 0 {
                    ui.label(format!(
                        "{} pending suggestion(s) name this person.",
                        person.pending_suggestions
                    ));
                }
            }
        }
    }

    ui.horizontal_wrapped(|ui| {
        if ui
            .button(format!("Edit {}…", person.display_name))
            .clicked()
        {
            action = Some(RosterAction::Edit(person.person_id.to_string()));
        }
        ui.menu_button(format!("Merge {} into…", person.display_name), |ui| {
            ui.label("Everything this entry is linked to moves to the one you choose.");
            for other in roster {
                if other.person_id == person.person_id || other.status == "merged" {
                    continue;
                }
                if ui
                    .button(format!("Merge into {}", other.display_name))
                    .clicked()
                {
                    action = Some(RosterAction::Merge {
                        loser: person.person_id.to_string(),
                        survivor: other.person_id.to_string(),
                    });
                    ui.close();
                }
            }
        });
        // A split moves *some* of the exemplars away, so it is offered only
        // where there is more than one: moving all of them is a merge, and the
        // application refuses it as one (§3.5).
        if matching_on
            && person
                .voice
                .as_ref()
                .is_some_and(|voice| voice.exemplars > 1)
            && ui
                .button(format!("Split {}'s voiceprint…", person.display_name))
                .on_hover_text(
                    "Some of this voiceprint came from somebody else? Move those recordings' \
                     exemplars to the person they really belong to.",
                )
                .clicked()
        {
            action = Some(RosterAction::SplitEnrollment(person.person_id.to_string()));
        }
        if matching_on
            && person.voice.is_some()
            && ui
                .button(format!("Delete {}'s voiceprint…", person.display_name))
                .clicked()
        {
            action = Some(RosterAction::DeleteEnrollment(person.person_id.to_string()));
        }
        if ui
            .button(format!("Delete {}…", person.display_name))
            .clicked()
        {
            action = Some(RosterAction::Delete(person.person_id.to_string()));
        }
    });
    action
}

/// "in 14 recordings", said once.
#[must_use]
pub fn assignment_count(recordings: u32) -> String {
    match recordings {
        0 => "Not assigned in any recording yet".to_owned(),
        1 => "In 1 recording".to_owned(),
        count => format!("In {count} recordings"),
    }
}

/// What an enrolment is made of: exemplars, their provenance and the speech
/// behind them.
#[must_use]
pub fn enrolment_facts(voice: &PersonVoiceDto) -> String {
    format!(
        "{} exemplar(s) from {} recording(s), {} s of speech, retention {} month(s)",
        voice.exemplars,
        voice.source_recordings,
        voice.total_speech_ms / 1_000,
        voice.retention_months
    )
}

/// When the enrolment was last used and when it expires.
///
/// A countdown in whole days, because that is the resolution the retention
/// rule works at; an expiry that is already past says so rather than counting
/// backwards.
#[must_use]
pub fn expiry(voice: &PersonVoiceDto, now_ms: i64) -> String {
    let last = voice.last_matched_at.map_or_else(
        || "never used by a match".to_owned(),
        |at| format!("last used by a match on {}", date(at)),
    );
    match voice.expires_at {
        None => format!("{last}; no expiry recorded"),
        Some(at) => {
            let days = (at.as_millis() - now_ms).div_euclid(DAY_MS);
            if days < 0 {
                format!("{last}; expired on {}", date(at))
            } else {
                format!("{last}; expires in {days} day(s), on {}", date(at))
            }
        }
    }
}

/// The date part of an instant, which is all a retention line needs.
fn date(at: Timestamp) -> String {
    at.to_rfc3339().map_or_else(
        || "an unreadable date".to_owned(),
        |text| text.chars().take(10).collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice() -> PersonVoiceDto {
        PersonVoiceDto {
            status: "active".to_owned(),
            exemplars: 3,
            source_recordings: 2,
            total_speech_ms: 42_000,
            retention_months: 12,
            consent_version: "2026-10-12".to_owned(),
            enrolled_at: Timestamp::from_millis(0),
            last_matched_at: None,
            expires_at: Some(Timestamp::from_millis(10 * DAY_MS)),
        }
    }

    #[test]
    fn an_enrolment_states_what_it_is_made_of_rather_than_a_quality_claim() {
        let facts = enrolment_facts(&voice());
        assert_eq!(
            facts,
            "3 exemplar(s) from 2 recording(s), 42 s of speech, retention 12 month(s)"
        );
    }

    #[test]
    fn the_expiry_counts_down_in_days_and_says_so_when_it_has_passed() {
        assert!(expiry(&voice(), 0).contains("expires in 10 day(s)"));
        assert!(expiry(&voice(), 20 * DAY_MS).contains("expired on"));
        assert!(expiry(&voice(), 0).contains("never used by a match"));
    }

    #[test]
    fn the_assignment_count_is_a_count_and_never_an_empty_string() {
        assert_eq!(assignment_count(0), "Not assigned in any recording yet");
        assert_eq!(assignment_count(1), "In 1 recording");
        assert_eq!(assignment_count(9), "In 9 recordings");
    }
}
