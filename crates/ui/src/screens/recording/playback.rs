//! The playback panel of the recording detail, over the real player engine.
//!
//! This is the one place in the view layer that owns a
//! [`plaudon_audio_playback::Player`]. Everything it needs comes from the
//! application layer or from the host:
//!
//! * **where the bytes are** is the answer to the `resolve_media` command —
//!   the view never walks the archive, never reads SQLite and never composes
//!   a path (§15.1);
//! * **the output device** is a factory the binary supplies, so the widgets
//!   build in a container with no audio stack and a test plays into
//!   [`plaudon_audio_playback::FakeAudioOut`];
//! * **the waveform** is built from that same file by the off-thread peak
//!   worker, which repaints the window when it finishes and never blocks a
//!   frame.
//!
//! Two timing rules matter and are deliberate:
//!
//! * [`plaudon_audio_playback::PlayerEngine::pump`] is called once per frame,
//!   before and after the widgets, so a button press takes effect in the
//!   frame it happened in rather than the next one;
//! * a repaint is requested **only** while the transport is actually
//!   playing. At rest this panel asks for nothing, which is what keeps an
//!   idle window at zero frames.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::Ui;
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{MediaLocation, RecordingDetail, ResolveMediaRequest};
use plaudon_audio_playback::engine::{
    AudioOut, PlaybackError, Player, PlayerEngine, Speed, TransportState, Volume,
};
use plaudon_audio_playback::source::SymphoniaPcmSource;
use plaudon_audio_playback::sync::{PlaybackSnapshot, PlayerHandle};
use plaudon_audio_playback::time::Micros;
use plaudon_audio_playback::worker::PeakRequest;
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::RecordingId;

use crate::ai::AiOutput;
use crate::player::{PeakSource, PlayerPanel, PlayerStyle};

/// How the host opens an audio output device.
///
/// A factory rather than a device, because a device cannot be opened before
/// the user plays something and must be re-openable after it is lost.
pub type AudioOutFactory = Arc<dyn Fn() -> Result<Box<dyn AudioOut>, PlaybackError> + Send + Sync>;

/// A factory that opens no device at all.
///
/// The honest default for a host that has not supplied one: the transport
/// reports that this build has no audio output rather than pretending to
/// play.
#[must_use]
pub fn no_audio_output() -> AudioOutFactory {
    Arc::new(|| {
        Err(PlaybackError::Device(
            "this build was started without an audio output device".to_owned(),
        ))
    })
}

/// Everything the playback panel holds between frames.
pub struct PlaybackBinding {
    factory: AudioOutFactory,
    peak_cache_dir: Option<PathBuf>,
    panel: Option<PlayerPanel>,
    player: Option<Player>,
    recording_id: Option<RecordingId>,
    location: Option<MediaLocation>,
    pending: Option<CommandId>,
    /// Why there is no sound, in words the user can act on.
    failure: Option<String>,
    snapshot: PlaybackSnapshot,
}

impl std::fmt::Debug for PlaybackBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No path: §15.4 keeps the archive location out of anything that can
        // be logged.
        f.debug_struct("PlaybackBinding")
            .field("recording_id", &self.recording_id)
            .field("media_resolved", &self.location.is_some())
            .field("device_open", &self.player.is_some())
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}

impl PlaybackBinding {
    /// A binding that opens its device through `factory`.
    ///
    /// `peak_cache_dir` is where built waveforms are kept between runs;
    /// `None` rebuilds them every time, which is what a test wants.
    #[must_use]
    pub fn new(factory: AudioOutFactory, peak_cache_dir: Option<PathBuf>) -> Self {
        Self {
            factory,
            peak_cache_dir,
            panel: None,
            player: None,
            recording_id: None,
            location: None,
            pending: None,
            failure: None,
            snapshot: silent(),
        }
    }

    /// The transport state as of the last frame.
    #[must_use]
    pub const fn snapshot(&self) -> PlaybackSnapshot {
        self.snapshot
    }

    /// Whether a device is open and loaded.
    #[must_use]
    pub const fn is_loaded(&self) -> bool {
        self.player.is_some()
    }

    /// Point the panel at a recording and ask the application where its
    /// bytes are.
    pub fn open(&mut self, recording_id: RecordingId, out: &mut AiOutput) {
        if self.recording_id == Some(recording_id) {
            return;
        }
        self.close();
        self.recording_id = Some(recording_id);
        out.command(AppCommand::ResolveMedia(ResolveMediaRequest {
            recording_id: recording_id.to_string(),
        }));
        // The id is not known until the host dispatches, so the answer is
        // matched on the command name and the recording it names instead.
        self.pending = None;
    }

    /// Stop, unload and forget everything about the open recording.
    pub fn close(&mut self) {
        if let Some(player) = self.player.as_mut() {
            player.stop();
            player.unload();
            player.pump();
        }
        self.player = None;
        if let Some(panel) = self.panel.as_mut() {
            panel.peaks.clear();
        }
        self.recording_id = None;
        self.location = None;
        self.pending = None;
        self.failure = None;
        self.snapshot = silent();
    }

    /// Route a dispatcher answer. Returns whether it was this panel's.
    ///
    /// Matched by content: the answer names the recording it is about, so
    /// no command id has to be threaded through the panels.
    pub fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> bool {
        if command != "resolve_media" {
            return false;
        }
        match result {
            Ok(AppResponse::ResolveMedia(location)) => {
                if self.recording_id != Some(location.recording_id) {
                    // An answer for a recording the user has already left.
                    return true;
                }
                self.location = Some(location.clone());
                true
            }
            Ok(_) => false,
            Err(envelope) => {
                self.failure = Some(envelope.message.clone());
                true
            }
        }
    }

    /// Move playback, from a transcript timestamp or a review anchor.
    ///
    /// A seek before the device is open is not lost: it is applied as soon
    /// as the source loads.
    pub fn seek_ms(&mut self, position_ms: u64) {
        let position = Micros::from_millis(i64::try_from(position_ms).unwrap_or(i64::MAX));
        if let Some(player) = self.player.as_mut() {
            player.seek(position);
            player.pump();
            self.snapshot = player.snapshot();
        } else {
            // Remembered through the snapshot the panel renders, so the
            // playhead is already where the user asked when sound starts.
            self.snapshot.position = position;
        }
    }

    /// A handle on the live player, for a host that drives it directly.
    #[must_use]
    pub fn handle(&self) -> Option<PlayerHandle> {
        self.player.as_ref().map(PlayerEngine::handle)
    }

    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut Ui, detail: &RecordingDetail) {
        let panel = self.panel.get_or_insert_with(|| {
            PlayerPanel::new(
                PeakSource::new(ui.ctx(), self.peak_cache_dir.clone()),
                PlayerStyle::from_visuals(ui.visuals()),
            )
        });

        if self.player.is_none() && self.failure.is_none() {
            if let Some(location) = self.location.clone() {
                match load(&self.factory, Path::new(&location.path)) {
                    Ok(player) => {
                        let duration = player
                            .duration()
                            .or_else(|| detail.summary.duration_ms.and_then(micros_from_millis));
                        if let Some(duration) = duration {
                            panel.set_duration(duration);
                        }
                        panel.peaks.request(
                            PeakRequest::new(&location.path)
                                .with_content_hash(*detail.summary.sha256.as_bytes()),
                        );
                        self.player = Some(player);
                        // A seek that arrived before the device was open.
                        let pending_position = self.snapshot.position;
                        if pending_position > Micros::ZERO {
                            if let Some(player) = self.player.as_mut() {
                                player.seek(pending_position);
                            }
                        }
                    }
                    Err(reason) => self.failure = Some(reason),
                }
            }
        }

        if let Some(failure) = self.failure.clone() {
            // Verbatim: the device and the archive own these words, not the
            // view.
            ui.label(failure);
            return;
        }

        let Some(player) = self.player.as_mut() else {
            ui.label(if self.location.is_some() {
                "Opening the audio device…"
            } else {
                "Looking up this recording's audio…"
            });
            return;
        };

        // Before: apply whatever the previous frame's widgets queued.
        player.pump();
        let handle = player.handle();
        let snapshot = player.snapshot();
        panel.show(ui, snapshot, &handle);
        // After: the click that just happened takes effect now, not in the
        // frame the user has not asked for yet.
        player.pump();
        self.snapshot = player.snapshot();

        // The only repaint this panel ever asks for, and only while sound is
        // actually coming out.
        if self.snapshot.is_playing() {
            ui.ctx().request_repaint();
        }
    }
}

/// Open a device and load a source, or say why neither happened.
fn load(factory: &AudioOutFactory, path: &Path) -> Result<Player, String> {
    let out = factory().map_err(|error| match error {
        PlaybackError::Device(detail) => detail,
        PlaybackError::NoSource => "no audio output device is available".to_owned(),
    })?;
    let source = SymphoniaPcmSource::open(path).map_err(|error| match error {
        // The error type carries the path; §15.4 keeps it out of the
        // interface, so only the reason is shown.
        plaudon_audio_playback::source::SourceError::Open { .. } => {
            "this recording's audio file could not be opened".to_owned()
        }
        plaudon_audio_playback::source::SourceError::Unsupported { detail, .. } => {
            format!("this recording's audio cannot be played in this build: {detail}")
        }
    })?;
    let mut player = Player::new(out);
    player.load(Box::new(source));
    Ok(player)
}

/// The transport before any device has been opened.
///
/// `PlaybackSnapshot` has no `Default`, deliberately: a snapshot is normally
/// read off a running clock, and a silent one has to be asked for by name.
fn silent() -> PlaybackSnapshot {
    PlaybackSnapshot {
        position: Micros::ZERO,
        duration: None,
        state: TransportState::default(),
        speed: Speed::NORMAL,
        volume: Volume::FULL,
        revision: 0,
    }
}

/// Milliseconds as a player duration, or nothing when the number is not one.
fn micros_from_millis(duration_ms: u64) -> Option<Micros> {
    i64::try_from(duration_ms).ok().map(Micros::from_millis)
}
