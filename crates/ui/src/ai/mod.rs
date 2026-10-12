//! The AI workflow panels: transcription, summarization, outputs, the
//! source-grounded review panel, model and consent surfaces, spending,
//! processing history and export.
//!
//! Thin by construction. Every rule these panels obey — what a cost of unknown
//! confidence may be called, when a model may be dispatched to, which scope a
//! consent grant has, what a job state means, when a caption format is
//! available — lives in `plaudon-app`
//! ([`plaudon_app::cloud_presentation`], [`plaudon_app::job_presentation`],
//! [`plaudon_app::dto`]). A panel here turns that state into pixels and the
//! user's actions into [`AiIntent`]s; it composes no user-facing claim of its
//! own and reaches no port, no database and no network.
//!
//! The host (the shell, a screen, or a test) owns the data: it dispatches the
//! [`plaudon_app::command::AppCommand`]s a panel returns and feeds the
//! responses back as the borrowed DTOs the panel renders. That is what makes
//! every state in these panels reachable from a `kittest` test without a
//! backend.

mod badge;
pub mod budget;
pub mod consent;
pub mod export;
pub mod history;
pub mod job;
pub mod models;
pub mod outputs;
pub mod review;
mod stage;
pub mod summarize;
pub mod templates;
pub mod transcription;

pub use badge::{tone_color, Badge};
pub use budget::BudgetPanel;
pub use consent::{ConsentDecision, ConsentSheet};
pub use export::ExportMenu;
pub use history::ProcessingHistory;
pub use job::JobCard;
pub use models::ModelPicker;
pub use outputs::OutputsViewer;
pub use review::{ClaimFilter, ReviewPanel};
pub use summarize::SummarizationPanel;
pub use templates::TemplatesPlaceholder;
pub use transcription::TranscriptionPanel;

use plaudon_app::command::AppCommand;
use plaudon_app::dto::{Capabilities, ExportFormat};
use plaudon_app::job_presentation::ControlIntent;
use plaudon_domain::transcript::AlignmentOrigin;

/// A request to move audio playback to a cited position.
///
/// Produced by the review panel when the user activates an anchor that the
/// backend gave a real time for. The panel never computes a position itself:
/// if `seek_ms` was absent from the anchor there is no seek affordance at all,
/// because inventing a time would be inventing evidence (§8.6A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeekRequest {
    /// Recording the position belongs to.
    pub recording_id: String,
    /// Position in milliseconds, exactly as the backend reported it.
    pub position_ms: u64,
    /// The claim whose anchor was activated, for the player to highlight.
    pub claim_id: Option<String>,
    /// Zero-based transcript segment the anchor points at.
    pub seq_index: u32,
}

/// Something that can move playback: the player widget, or a test double.
///
/// Deliberately one method and no egui types, so the review panel can be
/// tested against a recording stub and the player can implement it without
/// depending on this module.
pub trait SeekSink {
    /// Move playback to the cited position.
    fn seek(&mut self, request: SeekRequest);
}

impl SeekSink for Vec<SeekRequest> {
    fn seek(&mut self, request: SeekRequest) {
        self.push(request);
    }
}

/// What a panel asks the host to do.
///
/// Everything that touches the application is an [`AppCommand`]; the other
/// variants are the three things a command cannot express — moving playback,
/// navigating, and asking the operating system for a path.
#[derive(Debug)]
pub enum AiIntent {
    /// Dispatch this command.
    Command(AppCommand),
    /// Move playback to a cited position.
    Seek(SeekRequest),
    /// Navigate, or run the corrective control a §5.4 condition asked for.
    Navigate(ControlIntent),
    /// Ask for a save path, then dispatch `export_recording` for it.
    PickTranscriptExportPath {
        /// Recording being exported.
        recording_id: String,
        /// Revision being rendered.
        revision_id: String,
        /// Chosen format.
        format: ExportFormat,
        /// Suggested file name, including the extension.
        suggested_file_name: String,
    },
    /// Ask for a folder, then dispatch `export_media` into it.
    PickMediaExportDirectory {
        /// Recording being exported.
        recording_id: String,
        /// Whether the user asked for a sanitized derivative as well.
        include_sanitized_derivative: bool,
    },
}

/// What one frame of a panel produced.
///
/// Panels are drawn every frame and own no queue: the host reads this, acts on
/// it and drops it.
#[derive(Debug, Default)]
pub struct AiOutput {
    /// Intents in the order the user produced them.
    pub intents: Vec<AiIntent>,
}

impl AiOutput {
    /// Record a command to dispatch.
    pub fn command(&mut self, command: AppCommand) {
        self.intents.push(AiIntent::Command(command));
    }

    /// Record any other intent.
    pub fn push(&mut self, intent: AiIntent) {
        self.intents.push(intent);
    }

    /// Append another frame's output. Used by panels that embed panels.
    pub fn extend(&mut self, other: Self) {
        self.intents.extend(other.intents);
    }

    /// Whether the frame produced nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.intents.is_empty()
    }

    /// The names of the commands this frame produced, for tests and logs.
    ///
    /// Only names: command arguments carry transcript text and paths, which
    /// §15.4 keeps out of logs.
    #[must_use]
    pub fn command_names(&self) -> Vec<&'static str> {
        self.intents
            .iter()
            .filter_map(|intent| match intent {
                AiIntent::Command(command) => Some(command.name()),
                _ => None,
            })
            .collect()
    }

    /// The commands this frame produced.
    pub fn commands(&self) -> impl Iterator<Item = &AppCommand> {
        self.intents.iter().filter_map(|intent| match intent {
            AiIntent::Command(command) => Some(command),
            _ => None,
        })
    }

    /// The seek requests this frame produced.
    pub fn seeks(&self) -> impl Iterator<Item = &SeekRequest> {
        self.intents.iter().filter_map(|intent| match intent {
            AiIntent::Seek(request) => Some(request),
            _ => None,
        })
    }

    /// Hand every seek request to a sink, keeping the other intents.
    pub fn drain_seeks_into(&mut self, sink: &mut impl SeekSink) {
        let mut kept = Vec::with_capacity(self.intents.len());
        for intent in self.intents.drain(..) {
            match intent {
                AiIntent::Seek(request) => sink.seek(request),
                other => kept.push(other),
            }
        }
        self.intents = kept;
    }
}

/// The facts about the recording a panel is working on.
///
/// Borrowed, never cloned into the panel: the host owns the DTOs and a panel
/// renders whatever the last response gave it.
#[derive(Debug, Clone, Copy)]
pub struct RecordingContext<'a> {
    /// Recording identity.
    pub recording_id: &'a str,
    /// Measured duration, or `None` when it has not been measured. Only a
    /// measured duration can produce a cost estimate.
    pub duration_ms: Option<u64>,
    /// Head transcript revision, when one exists.
    pub revision_id: Option<&'a str>,
    /// Alignment origin of that revision: what the caption formats depend on.
    pub alignment_origin: AlignmentOrigin,
    /// Display title, used for the suggested export file name.
    pub title: &'a str,
    /// The machine inventory and preferences, when they have been loaded.
    pub capabilities: Option<&'a Capabilities>,
}

impl<'a> RecordingContext<'a> {
    /// Whether Local-only Mode is enforced right now.
    ///
    /// Unknown capabilities are **not** read as permission: until the
    /// preferences have loaded the panels treat cloud work as blocked, because
    /// the alternative is sending content under an assumption (§4.3).
    #[must_use]
    pub fn local_only(&self) -> bool {
        self.capabilities
            .is_none_or(|capabilities| capabilities.preferences.local_only_mode)
    }

    /// Whether this engine runs without network egress, according to the
    /// capability inventory.
    ///
    /// An engine the inventory does not mention is treated as requiring the
    /// network: a claim that something is local has to come from evidence.
    #[must_use]
    pub fn engine_is_local(&self, engine_id: &str) -> bool {
        self.capabilities.is_some_and(|capabilities| {
            capabilities
                .engines
                .iter()
                .find(|engine| engine.engine_id == engine_id)
                .is_some_and(|engine| !engine.requires_network)
        })
    }

    /// Whether dispatching to this engine is refused by Local-only Mode.
    #[must_use]
    pub fn blocked_by_local_only(&self, engine_id: &str) -> bool {
        self.local_only() && !self.engine_is_local(engine_id)
    }

    /// The consent grants on record, empty until capabilities have loaded.
    #[must_use]
    pub fn consents(&self) -> &'a [plaudon_app::dto::ConsentSummary] {
        self.capabilities
            .map_or(&[][..], |capabilities| &capabilities.cloud_consents)
    }
}

/// Whether a caption format can be produced for this revision.
///
/// SRT and VTT need real segment times; a revision whose alignment origin is
/// `None` has none, and no time is invented to fill the gap (§4.6, §8.6A).
#[must_use]
pub fn format_available(format: ExportFormat, alignment_origin: AlignmentOrigin) -> bool {
    !format.needs_timestamps() || alignment_origin != AlignmentOrigin::None
}

/// Why a caption format is unavailable, for the disabled control's tooltip.
pub const NO_TIMESTAMPS_REASON: &str =
    "This transcript has no real timestamps; we do not invent seek times.";

/// A labelled fact row: the label and the value, both selectable text.
pub(crate) fn fact(ui: &mut egui::Ui, label: &str, value: &str) {
    // One text node, not two: a screen reader announces "Owner: Unassigned"
    // as one fact, which is how it reads on screen. Splitting the label and
    // the value into separate labels would announce them as unrelated runs.
    ui.label(format!("{label}: {value}"));
}
