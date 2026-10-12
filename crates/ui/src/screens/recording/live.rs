//! The recording detail's real panels: player, transcript editor, AI.
//!
//! [`UnbuiltPanels`](crate::screens::library::panels::UnbuiltPanels) says
//! "not built yet"; this is what replaces it in the shipped binary. It owns
//! the three bindings and, crucially, the **one** thing none of them can own
//! alone: the synchronisation between them.
//!
//! * A timestamp the user activates in the transcript seeks the player.
//! * An anchor the user activates in the review panel seeks the player, and
//!   the position is the backend's own — the panel offers no control at all
//!   for an anchor with no reported time (§8.6A).
//! * The transcript's highlight follows the player's clock through
//!   [`SnapshotPosition`].
//!
//! The transport is stopped and the transcript is flushed in
//! [`DetailPanels::closed`], which the container calls when the user
//! navigates away and the host calls when the window closes. There is no
//! third path, so there is no way to lose an edit that one of them forgot.

use std::path::PathBuf;

use egui::Ui;
use plaudon_app::command::{AppResponse, CommandId};
use plaudon_app::dto::{RecordingDetail, UiEvent};
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::RecordingId;

use crate::ai::AiOutput;
use crate::player::SnapshotPosition;
use crate::screens::recording::ai_panels::AiBinding;
use crate::screens::recording::playback::{AudioOutFactory, PlaybackBinding};
use crate::screens::recording::transcript_panel::TranscriptBinding;
use crate::screens::recording::DetailPanels;

/// The recording detail's working panels, bound to the engine.
#[derive(Debug)]
pub struct LivePanels {
    playback: PlaybackBinding,
    transcript: TranscriptBinding,
    ai: AiBinding,
    recording_id: Option<RecordingId>,
    /// Wall clock in milliseconds. `None` reads the real clock; a test
    /// fixes it so the elapsed labels it asserts on cannot drift.
    now_ms: Option<i128>,
}

impl LivePanels {
    /// Panels that open their audio device through `audio_out`.
    ///
    /// `peak_cache_dir` is where built waveforms are kept between runs.
    #[must_use]
    pub fn new(audio_out: AudioOutFactory, peak_cache_dir: Option<PathBuf>) -> Self {
        Self {
            playback: PlaybackBinding::new(audio_out, peak_cache_dir),
            transcript: TranscriptBinding::new(),
            ai: AiBinding::new(),
            recording_id: None,
            now_ms: None,
        }
    }

    /// The playback binding, for a host that drives the transport from a
    /// keyboard shortcut and for tests.
    #[must_use]
    pub const fn playback(&self) -> &PlaybackBinding {
        &self.playback
    }

    /// The playback binding, mutably.
    pub const fn playback_mut(&mut self) -> &mut PlaybackBinding {
        &mut self.playback
    }

    /// The transcript binding, for tests.
    #[must_use]
    pub const fn transcript(&self) -> &TranscriptBinding {
        &self.transcript
    }

    /// The AI binding, for tests.
    #[must_use]
    pub const fn ai(&self) -> &AiBinding {
        &self.ai
    }

    /// Fix the wall clock the elapsed labels are measured against.
    ///
    /// Only a test needs this; the shipped binary reads the real clock.
    pub const fn set_now_ms(&mut self, now_ms: i128) {
        self.now_ms = Some(now_ms);
    }
}

impl DetailPanels for LivePanels {
    fn player(&mut self, ui: &mut Ui, detail: &RecordingDetail, _out: &mut AiOutput) {
        self.playback.ui(ui, detail);
    }

    fn transcript(&mut self, ui: &mut Ui, detail: &RecordingDetail, out: &mut AiOutput) {
        let position = SnapshotPosition::new(self.playback.snapshot());
        self.transcript.ui(ui, detail, &position, out);
        if let Some(position_ms) = self.transcript.take_seek() {
            self.playback.seek_ms(position_ms);
        }
    }

    fn outputs(&mut self, ui: &mut Ui, detail: &RecordingDetail, out: &mut AiOutput) {
        let mut produced = AiOutput::default();
        let now_ms = self
            .now_ms
            .unwrap_or_else(crate::shell::intents::wall_clock_ms);
        self.ai.ui(ui, detail, now_ms, &mut produced);
        // A review anchor's seek is carried out here and never leaves the
        // panels: the player is theirs.
        let mut seeks = Vec::new();
        produced.drain_seeks_into(&mut seeks);
        for request in seeks {
            self.playback.seek_ms(request.position_ms);
        }
        out.extend(produced);
    }

    fn opened(&mut self, recording_id: RecordingId, out: &mut AiOutput) {
        self.recording_id = Some(recording_id);
        self.playback.open(recording_id, out);
        self.transcript.open(recording_id, out);
        self.ai.open(recording_id, out);
    }

    fn closed(&mut self, out: &mut AiOutput) {
        self.transcript.close(out);
        self.playback.close();
        self.ai.close();
        self.recording_id = None;
    }

    fn on_response(
        &mut self,
        _id: CommandId,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut AiOutput,
    ) {
        if self.playback.on_response(command, result) {
            return;
        }
        if self.transcript.on_response(command, result, out) {
            return;
        }
        self.ai.on_response(command, result);
    }

    fn on_ui_event(&mut self, event: &UiEvent, out: &mut AiOutput) {
        self.ai.on_ui_event(event, out);
    }

    fn local_only(&self) -> bool {
        self.ai.local_only()
    }
}
