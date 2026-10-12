//! The recording detail container (§5.2 L14).
//!
//! This module owns the **frame** of the detail view — fetching the
//! recording, the header with its provenance and privacy facts, the error and
//! loading states, and where the three working panels sit — and nothing
//! inside those panels. Player, transcript and outputs arrive through
//! [`DetailPanels`], so the widgets that fill them (O1.5's player, O2.1's
//! transcript editor, O2.2's review panel) plug in without this container
//! depending on them.
//!
//! The header's rules are the ones a user can be misled by:
//!
//! * the title is read-only and says why — no command persists a new one;
//! * the privacy badge states the **enforced** mode, not an intention;
//! * provenance names the source, the original file name and the content
//!   hash, which is how a user checks that this is the recording they think
//!   it is;
//! * a recorded-at that was never captured renders as unknown.

pub mod ai_panels;
pub mod live;
pub mod playback;
pub mod transcript_panel;

pub use live::LivePanels;
pub use playback::{no_audio_output, AudioOutFactory, PlaybackBinding};

use egui::{RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{EventKind, GetRecordingRequest, RecordingDetail, UiEvent};
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::RecordingId;

use crate::ai::AiOutput;
use crate::screens::library::format::{format_duration, format_timestamp, short_hash};
use crate::screens::library::list::Sender;
use crate::screens::library::status::{
    status_badge, transcription_status, StatusDescriptor, StatusTone,
};
use crate::widgets::error_notice::{error_notice, ErrorNoticeAction};

/// Events that change what the detail view shows.
pub const DETAIL_EVENTS: [EventKind; 3] = [
    EventKind::TranscriptCompleted,
    EventKind::RecordingSourceUpdated,
    EventKind::LibraryChanged,
];

/// The three working areas of the detail view.
///
/// The container decides where they go and how big they are; the
/// implementation decides what is in them. A host that has no widget for one
/// of them yet says so in that panel — the container never draws a
/// placeholder on its behalf.
///
/// A panel reaches the application the same way every other view does: it
/// records [`AiIntent`](crate::ai::AiIntent)s into the [`AiOutput`] the
/// container hands it, and the screen adapter above dispatches them. The
/// panels therefore need no dispatcher, no ports and no `ScreenCtx`, which
/// is what keeps them drivable from a kittest without a backend.
pub trait DetailPanels: std::fmt::Debug {
    /// Playback controls and waveform.
    fn player(&mut self, ui: &mut Ui, detail: &RecordingDetail, out: &mut AiOutput);
    /// Transcript, revisions and editing.
    fn transcript(&mut self, ui: &mut Ui, detail: &RecordingDetail, out: &mut AiOutput);
    /// Summary, actions, decisions and their review state.
    fn outputs(&mut self, ui: &mut Ui, detail: &RecordingDetail, out: &mut AiOutput);

    /// A recording was opened. Called once per recording, before the first
    /// frame that draws it, so a panel can ask for what it needs.
    fn opened(&mut self, _recording_id: RecordingId, _out: &mut AiOutput) {}

    /// The detail view is being left: another recording, another
    /// destination, or the window closing.
    ///
    /// This is the panels' only chance to finish what the user started —
    /// flushing a pending autosave, stopping the audio device — so it is
    /// called on navigation **and** on shutdown.
    fn closed(&mut self, _out: &mut AiOutput) {}

    /// A command a panel sent has an outcome.
    fn on_response(
        &mut self,
        _id: CommandId,
        _command: &'static str,
        _result: &Result<AppResponse, ErrorEnvelope>,
        _out: &mut AiOutput,
    ) {
    }

    /// Something changed in the application (spec §15.3).
    fn on_ui_event(&mut self, _event: &UiEvent, _out: &mut AiOutput) {}

    /// Whether Local-only Mode is enforced, as the capability report states
    /// it.
    ///
    /// The default is `true`: until a panel has been told otherwise, the
    /// header says local-only, because claiming cloud eligibility without
    /// evidence is the one direction that can mislead a user into sending
    /// content (§4.3).
    fn local_only(&self) -> bool {
        true
    }
}

/// How the three panels are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailLayout {
    /// Player and transcript on the left, outputs in a side column.
    Wide,
    /// Everything in one column, outputs last.
    Stacked,
}

/// Width, in points, below which the side column is folded into the stack.
pub const STACK_BELOW: f32 = 900.0;

impl DetailLayout {
    /// The layout for a container of this width.
    #[must_use]
    pub fn for_width(width: f32) -> Self {
        if width >= STACK_BELOW {
            Self::Wide
        } else {
            Self::Stacked
        }
    }
}

/// Facts the container needs that do not come from `get_recording`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailContext {
    /// Whether local-only mode is enforced, as the capability report states
    /// it. Not a preference the view guesses.
    pub local_only: bool,
}

/// Something the detail view needs the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailIntent {
    /// Go back to the library listing.
    Back,
    /// Open settings.
    OpenSettings,
}

/// The detail container.
#[derive(Debug)]
pub struct RecordingDetailScreen {
    recording_id: Option<RecordingId>,
    detail: Option<RecordingDetail>,
    pending: Option<CommandId>,
    error: Option<ErrorEnvelope>,
    /// The recording the panels were last told about, so `opened` and
    /// `closed` fire exactly once each per recording.
    announced: Option<RecordingId>,
}

impl Default for RecordingDetailScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingDetailScreen {
    /// A container with no recording open.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            recording_id: None,
            detail: None,
            pending: None,
            error: None,
            announced: None,
        }
    }

    /// The recording currently open.
    #[must_use]
    pub const fn recording_id(&self) -> Option<RecordingId> {
        self.recording_id
    }

    /// The loaded detail, if it has arrived.
    #[must_use]
    pub const fn detail(&self) -> Option<&RecordingDetail> {
        self.detail.as_ref()
    }

    /// Open a recording, fetching it if it is not the one already shown.
    pub fn open(&mut self, recording_id: RecordingId, send: Sender<'_>) {
        if self.recording_id == Some(recording_id)
            && (self.detail.is_some() || self.pending.is_some())
        {
            return;
        }
        self.recording_id = Some(recording_id);
        self.detail = None;
        self.error = None;
        self.fetch(send);
    }

    fn fetch(&mut self, send: Sender<'_>) {
        let Some(recording_id) = self.recording_id else {
            return;
        };
        let id = send(AppCommand::GetRecording(GetRecordingRequest {
            recording_id: recording_id.to_string(),
        }));
        self.pending = Some(id);
    }

    /// Route a dispatcher answer. Returns whether it was ours.
    pub fn on_response(
        &mut self,
        id: CommandId,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> bool {
        if self.pending != Some(id) {
            return false;
        }
        self.pending = None;
        match result {
            Ok(AppResponse::GetRecording(detail)) => {
                self.error = None;
                self.detail = Some(detail.clone());
            }
            Ok(_) => return false,
            Err(envelope) => self.error = Some(envelope.clone()),
        }
        true
    }

    /// Route a §15.3 event.
    pub fn on_ui_event(&mut self, kind: EventKind, send: Sender<'_>) {
        if DETAIL_EVENTS.contains(&kind) && self.recording_id.is_some() {
            self.fetch(send);
        }
    }

    /// Route a resynchronisation.
    pub fn on_resync(&mut self, send: Sender<'_>) {
        if self.recording_id.is_some() {
            self.fetch(send);
        }
    }

    /// Leave the detail view: tell the panels, and forget the recording.
    ///
    /// Called when the route moves off this recording and when the window is
    /// closing, which is what gives a half-typed transcript edit its last
    /// chance to be saved.
    pub fn close(&mut self, panels: &mut dyn DetailPanels, out: &mut AiOutput) {
        if self.announced.take().is_some() {
            panels.closed(out);
        }
        self.recording_id = None;
        self.detail = None;
        self.pending = None;
        self.error = None;
    }

    /// Draw the container around `panels`.
    ///
    /// Anything the panels asked the application for is appended to `out`;
    /// the container itself only reports [`DetailIntent`]s, which are the
    /// two things a command cannot express.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        panels: &mut dyn DetailPanels,
        context: DetailContext,
        send: Sender<'_>,
        out: &mut AiOutput,
    ) -> Vec<DetailIntent> {
        let mut intents = Vec::new();
        if self.announced != self.recording_id {
            if self.announced.is_some() {
                panels.closed(out);
            }
            self.announced = self.recording_id;
            if let Some(recording_id) = self.recording_id {
                panels.opened(recording_id, out);
            }
        }

        if ui.button("← Back to library").clicked() {
            intents.push(DetailIntent::Back);
        }

        if let Some(envelope) = self.error.clone() {
            match error_notice(ui, &envelope, "This recording could not be loaded.") {
                Some(ErrorNoticeAction::Retry) => self.fetch(send),
                Some(ErrorNoticeAction::OpenSettings) => {
                    intents.push(DetailIntent::OpenSettings);
                }
                None => {}
            }
            return intents;
        }

        let Some(detail) = self.detail.clone() else {
            ui.label(if self.pending.is_some() {
                "Loading recording…"
            } else {
                "This recording is not available."
            });
            return intents;
        };

        header(ui, &detail, context);
        ui.separator();

        match DetailLayout::for_width(ui.available_width()) {
            DetailLayout::Wide => {
                let side = (ui.available_width() * 0.32).clamp(260.0, 420.0);
                ui.horizontal_top(|ui| {
                    let main = ui.available_width() - side;
                    ui.allocate_ui(egui::vec2(main, ui.available_height()), |ui| {
                        panel(ui, "Playback", |ui| panels.player(ui, &detail, out));
                        panel(ui, "Transcript", |ui| panels.transcript(ui, &detail, out));
                    });
                    ui.separator();
                    ui.allocate_ui(egui::vec2(side, ui.available_height()), |ui| {
                        panel(ui, "Outputs", |ui| panels.outputs(ui, &detail, out));
                    });
                });
            }
            DetailLayout::Stacked => {
                panel(ui, "Playback", |ui| panels.player(ui, &detail, out));
                panel(ui, "Transcript", |ui| panels.transcript(ui, &detail, out));
                panel(ui, "Outputs", |ui| panels.outputs(ui, &detail, out));
            }
        }

        intents
    }
}

/// One named region, so each panel is reachable and announceable on its own.
fn panel(ui: &mut Ui, name: &str, contents: impl FnOnce(&mut Ui)) {
    let heading = ui.heading(name);
    heading.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, name));
    let label = name.to_owned();
    ui.ctx().accesskit_node_builder(heading.id, |node| {
        node.set_role(egui::accesskit::Role::Group);
        node.set_label(label);
    });
    egui::Frame::group(ui.style()).show(ui, contents);
}

/// The privacy badge: what is actually enforced, not what is intended.
#[must_use]
pub const fn privacy_badge(local_only: bool) -> StatusDescriptor {
    if local_only {
        StatusDescriptor {
            tone: StatusTone::Success,
            glyph: "◆",
            label: "Local only",
            description: "Local-only mode is on: this recording is never sent to a provider.",
        }
    } else {
        StatusDescriptor {
            tone: StatusTone::Warning,
            glyph: "◇",
            label: "Cloud eligible",
            description: "Cloud processing is allowed; each cloud run still asks for consent.",
        }
    }
}

fn header(ui: &mut Ui, detail: &RecordingDetail, context: DetailContext) {
    let summary = &detail.summary;
    ui.heading(&summary.title);
    ui.label(
        RichText::new(
            "Renaming is not part of this build: no command persists a new title, so the title \
             is shown exactly as it was imported.",
        )
        .small()
        .color(ui.visuals().weak_text_color()),
    );

    ui.horizontal_wrapped(|ui| {
        status_badge(ui, transcription_status(summary.transcription_status));
        status_badge(ui, privacy_badge(context.local_only));
        if let Some(engine) = &summary.processing_engine {
            ui.label(format!("Engine {engine}"));
        }
    });

    egui::Grid::new("recording_provenance")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for (name, value) in provenance_rows(detail) {
                ui.label(RichText::new(name).strong());
                ui.label(value);
                ui.end_row();
            }
        });

    if let Some(envelope) = &summary.last_error {
        ui.label(
            RichText::new(format!("Last error: {}", envelope.message))
                .color(ui.visuals().error_fg_color),
        );
    }
}

/// The provenance table, as name/value pairs.
#[must_use]
pub fn provenance_rows(detail: &RecordingDetail) -> Vec<(&'static str, String)> {
    let summary = &detail.summary;
    vec![
        ("Source", summary.source.label.clone()),
        ("Original file", detail.media.original_file_name.clone()),
        ("Content hash (SHA-256)", short_hash(&summary.sha256)),
        ("Recorded", format_timestamp(summary.recorded_at.as_deref())),
        ("Imported", format_timestamp(Some(&summary.imported_at))),
        ("Duration", format_duration(summary.duration_ms)),
        ("Codec", detail.media.observed_codec.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::library::testing::{detail, row_with};

    #[test]
    fn the_layout_folds_the_side_column_on_a_narrow_container() {
        assert_eq!(DetailLayout::for_width(1_280.0), DetailLayout::Wide);
        assert_eq!(DetailLayout::for_width(700.0), DetailLayout::Stacked);
    }

    #[test]
    fn the_privacy_badge_states_the_enforced_mode_in_words() {
        assert_eq!(privacy_badge(true).label, "Local only");
        assert!(privacy_badge(true).description.contains("never sent"));
        assert_eq!(privacy_badge(false).label, "Cloud eligible");
        assert!(privacy_badge(false).description.contains("consent"));
    }

    #[test]
    fn provenance_names_the_source_the_file_and_the_hash() {
        let detail = detail(row_with("Board meeting", |row| {
            row.recorded_at = Some("2026-03-12T14:03:22Z".to_owned());
            row.duration_ms = Some(61_000);
        }));
        let rows = provenance_rows(&detail);
        let names: Vec<&str> = rows.iter().map(|(name, _)| *name).collect();
        assert!(names.contains(&"Source"));
        assert!(names.contains(&"Original file"));
        assert!(names.contains(&"Content hash (SHA-256)"));
        let hash = &rows
            .iter()
            .find(|(name, _)| *name == "Content hash (SHA-256)")
            .expect("the hash row")
            .1;
        assert!(
            hash.ends_with('…'),
            "the hash is a fingerprint, not a value to retype"
        );
    }

    #[test]
    fn an_unmeasured_recording_says_unknown_in_the_provenance_table() {
        let detail = detail(row_with("Unmeasured", |row| {
            row.recorded_at = None;
            row.duration_ms = None;
        }));
        let rows = provenance_rows(&detail);
        for name in ["Recorded", "Duration"] {
            let value = &rows
                .iter()
                .find(|(candidate, _)| *candidate == name)
                .expect("the row")
                .1;
            assert_eq!(value, "Unknown", "{name} must not be invented");
        }
    }
}
