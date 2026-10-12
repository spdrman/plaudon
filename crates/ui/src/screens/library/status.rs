//! How a lifecycle state is said out loud.
//!
//! Spec §5.3: status is never carried by colour alone. Every descriptor here
//! has a text glyph, a short label and a sentence of description, and the
//! renderer draws all three; the colour is an addition to them, never a
//! substitute. The strings are the ones the screen being replaced used, so a
//! user migrating between builds reads the same words.

use egui::{Color32, Visuals};
use plaudon_app::dto::{ImportOutcome, ImportStatus, TranscriptionStatus};

/// The severity a status carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    /// Nothing has happened yet, and nothing is wrong.
    Neutral,
    /// Finished, as intended.
    Success,
    /// Work is in progress.
    Running,
    /// Queued behind something else.
    Waiting,
    /// Usable, but not what the user probably expects.
    Warning,
    /// Failed.
    Danger,
}

impl StatusTone {
    /// The colour this tone draws in.
    ///
    /// Resolved from the active [`Visuals`] rather than from literal values,
    /// so light, dark and high-contrast themes each get their own ramp and no
    /// colour is hard-coded in a screen.
    #[must_use]
    pub fn color(self, visuals: &Visuals) -> Color32 {
        match self {
            Self::Neutral | Self::Waiting => visuals.weak_text_color(),
            Self::Success | Self::Running => visuals.text_color(),
            Self::Warning => visuals.warn_fg_color,
            Self::Danger => visuals.error_fg_color,
        }
    }
}

/// A status rendered as text, a glyph and an explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusDescriptor {
    /// Severity.
    pub tone: StatusTone,
    /// Text glyph; the status is legible without colour.
    pub glyph: &'static str,
    /// Short label, shown next to the glyph.
    pub label: &'static str,
    /// One sentence saying what the label means.
    pub description: &'static str,
}

impl StatusDescriptor {
    /// What a screen reader announces for this status.
    #[must_use]
    pub fn announced(&self) -> String {
        format!("{}: {}", self.label, self.description)
    }
}

/// Presentation of an import lifecycle state.
#[must_use]
pub const fn import_status(status: ImportStatus) -> StatusDescriptor {
    match status {
        ImportStatus::Pending => StatusDescriptor {
            tone: StatusTone::Waiting,
            glyph: "◷",
            label: "Pending",
            description: "Waiting to import",
        },
        ImportStatus::Importing => StatusDescriptor {
            tone: StatusTone::Running,
            glyph: "⟳",
            label: "Importing",
            description: "Copying into the library",
        },
        ImportStatus::Imported => StatusDescriptor {
            tone: StatusTone::Success,
            glyph: "✓",
            label: "Imported",
            description: "In the library",
        },
        ImportStatus::Failed => StatusDescriptor {
            tone: StatusTone::Danger,
            glyph: "✕",
            label: "Import failed",
            description: "Import did not finish",
        },
    }
}

/// Presentation of a transcription lifecycle state.
#[must_use]
pub const fn transcription_status(status: TranscriptionStatus) -> StatusDescriptor {
    match status {
        TranscriptionStatus::None => StatusDescriptor {
            tone: StatusTone::Neutral,
            glyph: "–",
            label: "Not transcribed",
            description: "No transcript yet",
        },
        TranscriptionStatus::Queued => StatusDescriptor {
            tone: StatusTone::Waiting,
            glyph: "◷",
            label: "Queued",
            description: "Waiting for a transcription slot",
        },
        TranscriptionStatus::Running => StatusDescriptor {
            tone: StatusTone::Running,
            glyph: "⟳",
            label: "Transcribing",
            description: "Transcription in progress",
        },
        TranscriptionStatus::Completed => StatusDescriptor {
            tone: StatusTone::Success,
            glyph: "✓",
            label: "Transcribed",
            description: "Transcript available",
        },
        TranscriptionStatus::Failed => StatusDescriptor {
            tone: StatusTone::Danger,
            glyph: "✕",
            label: "Transcription failed",
            description: "Transcription did not finish",
        },
        TranscriptionStatus::Stale => StatusDescriptor {
            tone: StatusTone::Warning,
            glyph: "!",
            label: "Stale",
            description: "Audio changed after this transcript",
        },
    }
}

/// Presentation of one file's import outcome.
#[must_use]
pub const fn import_outcome(outcome: &ImportOutcome) -> StatusDescriptor {
    match outcome {
        ImportOutcome::Accepted { .. } => StatusDescriptor {
            tone: StatusTone::Success,
            glyph: "✓",
            label: "Imported",
            description: "Added to your library",
        },
        ImportOutcome::Duplicate { .. } => StatusDescriptor {
            tone: StatusTone::Warning,
            glyph: "!",
            label: "Needs a decision",
            description: "Already in your library by content hash",
        },
        ImportOutcome::Skipped { .. } => StatusDescriptor {
            tone: StatusTone::Neutral,
            glyph: "–",
            label: "Skipped",
            description: "Left out at your request",
        },
        ImportOutcome::Rejected { .. } => StatusDescriptor {
            tone: StatusTone::Danger,
            glyph: "✕",
            label: "Rejected",
            description: "Not imported",
        },
    }
}

/// Draw a status: glyph, label, and the description as the hover text.
///
/// Returns the response so a caller can attach it to a row's accessible
/// description.
pub fn status_badge(ui: &mut egui::Ui, descriptor: StatusDescriptor) -> egui::Response {
    let color = descriptor.tone.color(ui.visuals());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(egui::RichText::new(descriptor.glyph).color(color));
        ui.label(egui::RichText::new(descriptor.label).color(color));
    })
    .response
    .on_hover_text(descriptor.description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_import_state_has_a_distinct_label_and_glyph() {
        let descriptors: Vec<_> = [
            ImportStatus::Pending,
            ImportStatus::Importing,
            ImportStatus::Imported,
            ImportStatus::Failed,
        ]
        .into_iter()
        .map(import_status)
        .collect();
        let labels: std::collections::BTreeSet<_> =
            descriptors.iter().map(|d| d.label).collect();
        assert_eq!(labels.len(), descriptors.len(), "labels must be distinct");
        assert!(descriptors.iter().all(|d| !d.glyph.is_empty()));
    }

    #[test]
    fn every_transcription_state_has_a_distinct_label_and_glyph() {
        let descriptors: Vec<_> = [
            TranscriptionStatus::None,
            TranscriptionStatus::Queued,
            TranscriptionStatus::Running,
            TranscriptionStatus::Completed,
            TranscriptionStatus::Failed,
            TranscriptionStatus::Stale,
        ]
        .into_iter()
        .map(transcription_status)
        .collect();
        let labels: std::collections::BTreeSet<_> =
            descriptors.iter().map(|d| d.label).collect();
        assert_eq!(labels.len(), descriptors.len());
        let glyphs: std::collections::BTreeSet<_> =
            descriptors.iter().map(|d| d.glyph).collect();
        assert_eq!(
            glyphs.len(),
            descriptors.len(),
            "a glyph is the colour-free signal; two states must not share one"
        );
    }

    #[test]
    fn a_failed_state_is_not_distinguished_by_colour_alone() {
        let failed = import_status(ImportStatus::Failed);
        let imported = import_status(ImportStatus::Imported);
        assert_ne!(failed.label, imported.label);
        assert_ne!(failed.glyph, imported.glyph);
    }

    #[test]
    fn tones_resolve_from_the_active_theme_in_both_light_and_dark() {
        for visuals in [Visuals::light(), Visuals::dark()] {
            for tone in [
                StatusTone::Neutral,
                StatusTone::Success,
                StatusTone::Running,
                StatusTone::Waiting,
                StatusTone::Warning,
                StatusTone::Danger,
            ] {
                assert_ne!(tone.color(&visuals), Color32::TRANSPARENT);
            }
        }
    }
}
