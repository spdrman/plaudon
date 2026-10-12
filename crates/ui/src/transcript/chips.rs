//! The timestamp gutter and the speaker chip.
//!
//! Both encode honesty rules the backend owns:
//!
//! * a timestamp is shown only when the revision's alignment origin permits
//!   times and the segment actually has a start (spec 8.6A). A derived
//!   alignment is labelled approximate, in the accessible name as well as in
//!   the tooltip, so the qualification is not colour- or hover-only.
//! * a speaker label is a *cluster* label until a human confirmed it, and the
//!   qualifier that says so travels with it (spec 8.7, 17.6). The UI never
//!   composes the qualifier itself: it renders what the backend supplied.

use egui::{Align, Button, Layout, Rect, Response, Ui};
use plaudon_domain::ids::SpeakerId;
use plaudon_domain::timerange::SourceTimeMs;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_transcript_doc::clipboard::format_clock;

/// One speaker the user may assign to a segment.
///
/// The label and qualifier come from `plaudon_app_core::speakers::SpeakerDisplay`;
/// this crate transports them, it does not invent them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerChoice {
    /// Speaker row identity.
    pub id: SpeakerId,
    /// Display label, as the backend resolved it.
    pub label: String,
    /// The honesty qualifier that must accompany the label.
    pub qualifier: String,
}

impl SpeakerChoice {
    /// A choice.
    #[must_use]
    pub fn new(id: SpeakerId, label: impl Into<String>, qualifier: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            qualifier: qualifier.into(),
        }
    }

    /// Label and qualifier as one string.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{} ({})", self.label, self.qualifier)
    }
}

/// What the user did with a speaker chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeakerAction {
    /// Nothing.
    None,
    /// Assign this speaker to the segment.
    Assign(Option<SpeakerId>),
    /// Assign this speaker to every segment currently attributed to the
    /// segment's speaker.
    AssignEverywhere(Option<SpeakerId>),
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

/// Draw the speaker chip for one segment, with its reassignment menu.
pub fn speaker(
    ui: &mut Ui,
    rect: Rect,
    speaker_id: Option<SpeakerId>,
    speakers: &[SpeakerChoice],
    editable: bool,
) -> SpeakerAction {
    let current = speaker_id.and_then(|id| speakers.iter().find(|choice| choice.id == id));
    let label = current.map_or_else(|| "No speaker".to_owned(), SpeakerChoice::render);
    let mut action = SpeakerAction::None;

    let response: Response = ui.put(rect, Button::new(elide(&label, 24)).small());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            editable,
            format!("Speaker: {label}"),
        )
    });
    let response = response.on_hover_text(&label);

    if editable {
        egui::Popup::menu(&response).show(|ui| {
            ui.label("Reassign this segment");
            for choice in speakers {
                if ui.button(choice.render()).clicked() {
                    action = SpeakerAction::Assign(Some(choice.id));
                    ui.close();
                }
            }
            if ui.button("No speaker").clicked() {
                action = SpeakerAction::Assign(None);
                ui.close();
            }
            if speaker_id.is_some() {
                ui.separator();
                ui.label("Reassign every segment of this speaker");
                for choice in speakers {
                    if Some(choice.id) != speaker_id
                        && ui.button(format!("All → {}", choice.render())).clicked()
                    {
                        action = SpeakerAction::AssignEverywhere(Some(choice.id));
                        ui.close();
                    }
                }
            }
        });
    }
    action
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
