//! The timestamp gutter and the speaker chip.
//!
//! Both encode honesty rules the backend owns:
//!
//! * a timestamp is shown only when the revision's alignment origin permits
//!   times and the segment actually has a start (spec 8.6A). A derived
//!   alignment is labelled approximate, in the accessible name as well as in
//!   the tooltip, so the qualification is not colour- or hover-only.
//! * a speaker label is a *cluster* label until a human confirmed it, and the
//!   qualifier that says so travels with it (spec 8.7, 17.6). The wording is
//!   [`plaudon_domain::speaker::display`], the same function the export
//!   bundle renders with, so a transcript on screen and the same transcript
//!   in a bundle cannot disagree about who was speaking.
//!
//! The chip **assigns**. `set_segment_speaker` (spec §4.1, VP-031) persists
//! one segment's attribution, so the chip offers the recording's speakers and
//! "no speaker"; what it never offers is a guess — every entry in the list is
//! a row a human can point at, and nothing is pre-selected on the strength of
//! a similarity score.

use egui::{Align, Button, Label, Layout, Rect, Response, Ui};
use plaudon_domain::ids::SpeakerId;
use plaudon_domain::timerange::SourceTimeMs;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_transcript_doc::clipboard::format_clock;

/// One speaker, as a transcript chip presents them.
///
/// Carries the facts the honesty rule needs rather than a composed string,
/// so no caller can supply a second wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerChoice {
    /// Speaker row identity.
    pub id: SpeakerId,
    /// The row's current label, as the backend resolved it. A rename moves
    /// it; this is the field the export bundle renders.
    pub label: String,
    /// The machine-assigned label the row started with, which a reset
    /// restores. Shown alongside a renamed label so a user can always see
    /// which cluster an unconfirmed name is attached to.
    pub machine_label: String,
    /// The name a user typed, when one has been typed.
    pub name: Option<String>,
    /// Whether the user confirmed the name as this person's identity.
    pub confirmed: bool,
}

impl SpeakerChoice {
    /// Label and qualifier as one string.
    #[must_use]
    pub fn render(&self) -> String {
        plaudon_domain::speaker::display(&self.label, self.name.as_deref(), self.confirmed)
    }

    /// The rendered label, plus the cluster it came from when a rename has
    /// moved the two apart.
    #[must_use]
    pub fn render_with_origin(&self) -> String {
        let rendered = self.render();
        if self.label == self.machine_label {
            rendered
        } else {
            format!("{rendered}, machine label {}", self.machine_label)
        }
    }
}

/// Draw the timestamp gutter for one segment.
///
/// Returns the position to seek to when the user activated the chip.
pub fn timestamp(
    ui: &mut Ui,
    rect: Rect,
    start: Option<SourceTimeMs>,
    origin: AlignmentOrigin,
) -> Option<u64> {
    let mut seek = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::TOP)),
        |ui| {
            match (origin.permits_seek(), start) {
                (true, Some(start)) => {
                    let clock = format_clock(start.get());
                    let approximate = origin == AlignmentOrigin::Derived;
                    let label = if approximate {
                        format!("{clock} (approximate)")
                    } else {
                        clock.clone()
                    };
                    let response = ui.add(Button::new(clock).small().frame(false));
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &label)
                    });
                    let response = if approximate {
                        response.on_hover_text(
                            "Approximate: derived alignment, not reported by the engine",
                        )
                    } else {
                        response
                    };
                    if response.clicked() {
                        seek = Some(start.get());
                    }
                }
                _ => {
                    // No timeline: say so rather than render an empty chip that
                    // looks like a missing value.
                    ui.add_enabled(false, Button::new("—").small().frame(false))
                        .on_disabled_hover_text("This transcript has no timeline");
                }
            }
        },
    );
    seek
}

/// Where a speaker's name is changed, said once so the chip and its tests
/// quote the same sentence.
pub const SPEAKER_EDIT_HINT: &str =
    "Pick who is speaking in this segment. Speaker names are changed under \
     Speaker identification in the Outputs tab.";

/// Said instead when the revision is read-only, so the absence of a picker is
/// explained rather than silent.
pub const SPEAKER_READ_ONLY_HINT: &str =
    "This revision is not the newest one, so its speaker attribution is read-only.";

/// The "nobody" entry of the picker.
///
/// Clearing an attribution is a first-class answer: overlapping speech and
/// unattributable audio are what `transcript_segments.speaker_id` NULL means,
/// and a user has to be able to say so.
pub const NO_SPEAKER: &str = "No speaker";

/// What a user picked in the speaker chip.
///
/// "Nobody" is a decision, not the absence of one, which is why it is a
/// variant rather than a `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeakerPick {
    /// Attribute the segment to this speaker row.
    Speaker(SpeakerId),
    /// Clear the attribution: nobody identifiable, or two people at once.
    Nobody,
}

impl SpeakerPick {
    /// The picked row, `None` for [`Self::Nobody`].
    #[must_use]
    pub const fn speaker_id(self) -> Option<SpeakerId> {
        match self {
            Self::Speaker(id) => Some(id),
            Self::Nobody => None,
        }
    }
}

/// Draw the speaker chip for one segment.
///
/// The chip states who the segment is attributed to, with the qualifier that
/// says whether that is a claim or a cluster. When the revision is editable it
/// is also the picker: the returned value is the attribution the user chose.
pub fn speaker(
    ui: &mut Ui,
    rect: Rect,
    speaker_id: Option<SpeakerId>,
    speakers: &[SpeakerChoice],
    editable: bool,
) -> Option<SpeakerPick> {
    let current = speaker_id.and_then(|id| speakers.iter().find(|choice| choice.id == id));
    let shown = current.map_or_else(|| NO_SPEAKER.to_owned(), SpeakerChoice::render);
    let spoken = current.map_or_else(|| NO_SPEAKER.to_owned(), SpeakerChoice::render_with_origin);
    let elided = elide(&shown, 24);

    // Without rows to choose from there is nothing to pick: a transcript whose
    // recording has no speaker rows gets the label it always had.
    if !editable || speakers.is_empty() {
        let response: Response = ui.put(rect, Label::new(elided).truncate());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("Speaker: {spoken}"))
        });
        let hint = if editable {
            SPEAKER_EDIT_HINT
        } else {
            SPEAKER_READ_ONLY_HINT
        };
        response.on_hover_text(format!("{spoken}\n\n{hint}"));
        return None;
    }

    let response: Response = ui.put(rect, Button::new(elided).truncate());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Speaker: {spoken}"))
    });
    let response = response.on_hover_text(format!("{spoken}\n\n{SPEAKER_EDIT_HINT}"));

    let mut chosen = None;
    egui::Popup::menu(&response).show(|ui| {
        ui.label("Assign this segment to");
        for choice in speakers {
            if ui
                .selectable_label(Some(choice.id) == speaker_id, choice.render_with_origin())
                .clicked()
            {
                chosen = Some(SpeakerPick::Speaker(choice.id));
                ui.close();
            }
        }
        if ui
            .selectable_label(speaker_id.is_none(), NO_SPEAKER)
            .clicked()
        {
            chosen = Some(SpeakerPick::Nobody);
            ui.close();
        }
    });

    // Choosing what is already there is not a change, and must not cost a
    // write or an undo step.
    chosen.filter(|picked| picked.speaker_id() != speaker_id)
}

fn elide(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_speaker_label_is_elided_but_never_silently_truncated() {
        assert_eq!(elide("Speaker 1", 24), "Speaker 1");
        let long = "A very long confirmed speaker name indeed";
        let elided = elide(long, 10);
        assert_eq!(elided.chars().count(), 10);
        assert!(elided.ends_with('…'));
    }
}
