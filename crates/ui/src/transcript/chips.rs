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
//! The chip **reads**. Nothing on the wire persists a segment-to-speaker
//! assignment — `save_transcript_revision` carries `{index, text}` and
//! nothing else — so the chip offers no reassignment it could not save.

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
    "Speaker names are changed under Speaker identification in the Outputs tab. \
     Moving one segment to a different speaker is not something this build can save.";

/// Draw the speaker chip for one segment.
///
/// Read-only: see the module comment. The chip states who the backend
/// attributes the segment to, with the qualifier that says whether that is a
/// claim or a cluster, and points at the control that can actually change it.
pub fn speaker(ui: &mut Ui, rect: Rect, speaker_id: Option<SpeakerId>, speakers: &[SpeakerChoice]) {
    let current = speaker_id.and_then(|id| speakers.iter().find(|choice| choice.id == id));
    let shown = current.map_or_else(|| "No speaker".to_owned(), SpeakerChoice::render);
    let spoken = current.map_or_else(
        || "No speaker".to_owned(),
        SpeakerChoice::render_with_origin,
    );

    let response: Response = ui.put(rect, Label::new(elide(&shown, 24)).truncate());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("Speaker: {spoken}"))
    });
    response.on_hover_text(format!("{spoken}\n\n{SPEAKER_EDIT_HINT}"));
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
