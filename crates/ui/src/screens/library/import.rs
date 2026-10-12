//! Manual and drag-and-drop import (§7.4), over the one `import_files`
//! command.
//!
//! The interface never constructs, resolves or rewrites a path: it forwards
//! the exact strings the OS picker or the window drop produced and lets Rust
//! validate them, so a path that is not an approved media file comes back as a
//! per-file `rejected` outcome rather than being filtered out silently. A
//! batch never aborts on one bad file, which is why every path gets a row in
//! the results panel whatever happened to it.

use std::collections::BTreeSet;
use std::path::Path;

use egui::{Context, RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{DuplicatePolicy, ImportFilesRequest, ImportOutcome};
use plaudon_app::validate::MAX_IMPORT_PATHS;
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::RecordingId;

use super::list::Sender;
use super::status::{import_outcome, StatusTone};

/// Extensions the picker offers. Rust re-validates the real bytes (§8.1), so
/// this list is an affordance, not a security boundary.
pub const AUDIO_EXTENSIONS: [&str; 6] = ["wav", "mp3", "m4a", "flac", "ogg", "opus"];

/// How the user resolved one duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateChoice {
    /// Keep both: create a second recording for the same bytes.
    ImportAnyway,
    /// Keep the existing recording and go to it.
    OpenExisting,
    /// Keep the existing recording and stay here.
    Skip,
}

impl DuplicateChoice {
    const fn policy(self) -> DuplicatePolicy {
        match self {
            Self::ImportAnyway => DuplicatePolicy::ImportAnyway,
            Self::OpenExisting => DuplicatePolicy::OpenExisting,
            Self::Skip => DuplicatePolicy::Skip,
        }
    }
}

/// How many files ended in each outcome class.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OutcomeCounts {
    /// Imported.
    pub accepted: usize,
    /// Already present; waiting for a decision.
    pub duplicate: usize,
    /// Already present; nothing new was created.
    pub skipped: usize,
    /// Not imported.
    pub rejected: usize,
}

impl OutcomeCounts {
    /// Files accounted for.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.accepted + self.duplicate + self.skipped + self.rejected
    }
}

/// The sentence a finished batch is announced with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchSummary {
    /// Headline.
    pub title: String,
    /// Per-class detail.
    pub body: String,
    /// Severity.
    pub tone: StatusTone,
}

/// Count the outcome classes of a batch.
#[must_use]
pub fn summarize(results: &[ImportOutcome]) -> OutcomeCounts {
    let mut counts = OutcomeCounts::default();
    for result in results {
        match result {
            ImportOutcome::Accepted { .. } => counts.accepted += 1,
            ImportOutcome::Duplicate { .. } => counts.duplicate += 1,
            ImportOutcome::Skipped { .. } => counts.skipped += 1,
            ImportOutcome::Rejected { .. } => counts.rejected += 1,
        }
    }
    counts
}

/// Describe a finished batch, stating every outcome class rather than only
/// the happy one (§5.5).
#[must_use]
pub fn describe_batch(counts: OutcomeCounts) -> BatchSummary {
    let mut parts = Vec::new();
    if counts.accepted > 0 {
        parts.push(format!("{} imported", counts.accepted));
    }
    if counts.duplicate > 0 {
        parts.push(format!("{} duplicate", counts.duplicate));
    }
    if counts.skipped > 0 {
        parts.push(format!("{} skipped", counts.skipped));
    }
    if counts.rejected > 0 {
        parts.push(format!("{} rejected", counts.rejected));
    }
    let total = counts.total();
    let tone = if counts.rejected > 0 {
        StatusTone::Warning
    } else if counts.accepted > 0 {
        StatusTone::Success
    } else {
        StatusTone::Neutral
    };
    BatchSummary {
        title: if counts.accepted == total && total > 0 {
            "Import finished".to_owned()
        } else {
            "Import finished with decisions to make".to_owned()
        },
        body: if parts.is_empty() {
            "Nothing to import.".to_owned()
        } else {
            format!("{}.", parts.join(", "))
        },
        tone,
    }
}

/// The path of one outcome, whatever class it is.
#[must_use]
pub fn outcome_path(outcome: &ImportOutcome) -> &str {
    match outcome {
        ImportOutcome::Accepted { path, .. }
        | ImportOutcome::Duplicate { path, .. }
        | ImportOutcome::Skipped { path, .. }
        | ImportOutcome::Rejected { path, .. } => path,
    }
}

/// Replace outcomes for paths that were decided again, keeping first-seen
/// order so the results panel does not reshuffle under the user.
pub fn merge_outcomes(previous: &mut Vec<ImportOutcome>, incoming: Vec<ImportOutcome>) {
    for outcome in incoming {
        let path = outcome_path(&outcome).to_owned();
        match previous
            .iter_mut()
            .find(|existing| outcome_path(existing) == path)
        {
            Some(existing) => *existing = outcome,
            None => previous.push(outcome),
        }
    }
}

/// Whether a path looks like audio the importer accepts.
///
/// Used only to decide whether a *hover* should show the drop affordance; a
/// dropped file is always forwarded so that the rejection comes from Rust and
/// says why.
#[must_use]
pub fn looks_like_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(extension))
        })
}

/// Whether files are currently being dragged over the window.
#[must_use]
pub fn hovering_files(ctx: &Context) -> bool {
    ctx.input(|input| !input.raw.hovered_files.is_empty())
}

/// Paths dropped on the window this frame, split into the ones that can be
/// sent and the ones that cannot.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DroppedPaths {
    /// Paths forwarded to `import_files` verbatim.
    pub paths: Vec<String>,
    /// Paths that are not valid Unicode and therefore cannot be carried by
    /// the command's `Vec<String>`. Shown lossily, and never imported.
    pub unrepresentable: Vec<String>,
}

impl DroppedPaths {
    /// Whether anything was dropped.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.unrepresentable.is_empty()
    }
}

/// Collect this frame's dropped files.
#[must_use]
pub fn dropped_paths(ctx: &Context) -> DroppedPaths {
    ctx.input(|input| {
        let mut dropped = DroppedPaths::default();
        for file in &input.raw.dropped_files {
            match file.path().to_str() {
                Some(path) => dropped.paths.push(path.to_owned()),
                None => dropped
                    .unrepresentable
                    .push(file.path().to_string_lossy().into_owned()),
            }
        }
        dropped
    })
}

/// What the import panel asked the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportIntent {
    /// Open the OS file picker.
    PickFiles,
    /// Import this one path again, asking about duplicates.
    RetryPath(String),
    /// Go to this recording.
    OpenRecording(RecordingId),
    /// Open settings, because an outcome's envelope asked for it.
    OpenSettings,
}

/// The state of the current import batch.
#[derive(Debug, Default)]
pub struct ImportSession {
    batch: Vec<String>,
    results: Vec<ImportOutcome>,
    pending: BTreeSet<CommandId>,
    unrepresentable: Vec<String>,
    announcement: String,
    error: Option<ErrorEnvelope>,
    decide_later: bool,
}

impl ImportSession {
    /// Paths of the current batch, in the order they were requested.
    #[must_use]
    pub fn batch(&self) -> &[String] {
        &self.batch
    }

    /// Outcomes reported so far.
    #[must_use]
    pub fn results(&self) -> &[ImportOutcome] {
        &self.results
    }

    /// Whether a request is in flight.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The live-region text for the current phase.
    #[must_use]
    pub fn announcement(&self) -> &str {
        &self.announcement
    }

    /// The envelope of a request that failed as a whole, if one did.
    #[must_use]
    pub const fn error(&self) -> Option<&ErrorEnvelope> {
        self.error.as_ref()
    }

    /// Duplicates still waiting for an explicit decision.
    #[must_use]
    pub fn duplicates(&self) -> Vec<&ImportOutcome> {
        self.results
            .iter()
            .filter(|outcome| matches!(outcome, ImportOutcome::Duplicate { .. }))
            .collect()
    }

    /// Whether the duplicate dialog should be on screen.
    #[must_use]
    pub fn wants_duplicate_dialog(&self) -> bool {
        !self.decide_later && !self.duplicates().is_empty()
    }

    /// Leave the duplicates undecided and visible in the results panel.
    pub fn decide_later(&mut self) {
        self.decide_later = true;
    }

    /// Record paths that cannot be sent at all.
    pub fn note_unrepresentable(&mut self, paths: Vec<String>) {
        self.unrepresentable.extend(paths);
    }

    /// Paths that could not be forwarded because they are not valid Unicode.
    #[must_use]
    pub fn unrepresentable(&self) -> &[String] {
        &self.unrepresentable
    }

    /// Start importing `paths`.
    ///
    /// Requests are chunked at the command's documented maximum so that a
    /// large drop is imported rather than rejected wholesale.
    pub fn start(&mut self, paths: &[String], policy: DuplicatePolicy, send: Sender<'_>) {
        if paths.is_empty() {
            return;
        }
        self.error = None;
        self.decide_later = false;
        for path in paths {
            if !self.batch.contains(path) {
                self.batch.push(path.clone());
            }
        }
        self.announcement = format!(
            "Importing {} {}.",
            paths.len(),
            if paths.len() == 1 { "file" } else { "files" }
        );
        for chunk in paths.chunks(MAX_IMPORT_PATHS) {
            let id = send(AppCommand::ImportFiles(ImportFilesRequest {
                paths: chunk.to_vec(),
                duplicate_policy: policy,
            }));
            self.pending.insert(id);
        }
    }

    /// Re-run one path under an explicit duplicate decision.
    ///
    /// Returns the recording to open when the user chose to keep the one they
    /// already have.
    pub fn resolve_duplicate(
        &mut self,
        path: &str,
        choice: DuplicateChoice,
        send: Sender<'_>,
    ) -> Option<RecordingId> {
        let existing = self.results.iter().find_map(|outcome| match outcome {
            ImportOutcome::Duplicate {
                path: candidate,
                existing_recording_id,
                ..
            } if candidate == path => Some(*existing_recording_id),
            _ => None,
        });
        self.start(&[path.to_owned()], choice.policy(), send);
        match choice {
            DuplicateChoice::OpenExisting => existing,
            _ => None,
        }
    }

    /// Route a dispatcher answer. Returns whether it was for this session.
    pub fn on_response(
        &mut self,
        id: CommandId,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> bool {
        if !self.pending.remove(&id) {
            return false;
        }
        match result {
            Ok(AppResponse::ImportFiles(response)) => {
                merge_outcomes(&mut self.results, response.results.clone());
                let summary = describe_batch(summarize(&self.results));
                self.announcement = format!("{} {}", summary.title, summary.body);
            }
            Ok(_) => return false,
            Err(envelope) => {
                self.announcement = format!("Import failed. {}", envelope.message);
                self.error = Some(envelope.clone());
            }
        }
        true
    }

    /// Forget the batch and its outcomes.
    pub fn clear(&mut self) {
        self.batch.clear();
        self.results.clear();
        self.unrepresentable.clear();
        self.announcement.clear();
        self.error = None;
        self.decide_later = false;
    }

    /// The outcome recorded for one path, if any.
    #[must_use]
    pub fn outcome_for(&self, path: &str) -> Option<&ImportOutcome> {
        self.results
            .iter()
            .find(|outcome| outcome_path(outcome) == path)
    }
}

/// The overlay drawn while audio files are dragged over the window.
pub fn drop_overlay(ui: &mut Ui) {
    egui::Frame::group(ui.style())
        .fill(ui.visuals().extreme_bg_color)
        .show(ui, |ui| {
            let title = ui.label(RichText::new("⤓ Drop audio files to import").heading());
            title.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Label,
                    true,
                    "Drop audio files to import",
                )
            });
            ui.label(
                RichText::new(
                    AUDIO_EXTENSIONS
                        .iter()
                        .map(|extension| extension.to_uppercase())
                        .collect::<Vec<_>>()
                        .join(", "),
                )
                .color(ui.visuals().weak_text_color()),
            );
        });
}

/// Per-file outcomes for the batch, with progress while it runs.
///
/// A rejected file never hides the rest: every requested path has a row, and
/// a row without an outcome yet says so rather than claiming success.
pub fn import_results_panel(ui: &mut Ui, session: &mut ImportSession) -> Option<ImportIntent> {
    if session.batch().is_empty() && session.unrepresentable().is_empty() {
        return None;
    }
    let mut intent = None;
    let mut dismiss = false;
    let mut retry: Option<String> = None;

    ui.push_id("library_import_results", |ui| {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Import");
                if ui.button("Dismiss").clicked() {
                    dismiss = true;
                }
            });
            if session.is_pending() {
                ui.add(
                    egui::ProgressBar::new(progress_fraction(session))
                        .show_percentage()
                        .desired_width(240.0),
                )
                .widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::ProgressIndicator,
                        true,
                        "Importing files",
                    )
                });
            }
            if let Some(envelope) = session.error() {
                if let Some(action) =
                    crate::widgets::error_notice::error_notice(ui, envelope, "The import failed.")
                {
                    intent = match action {
                        crate::widgets::error_notice::ErrorNoticeAction::Retry => {
                            Some(ImportIntent::PickFiles)
                        }
                        crate::widgets::error_notice::ErrorNoticeAction::OpenSettings => {
                            Some(ImportIntent::OpenSettings)
                        }
                    };
                }
            }
            for path in session.batch() {
                ui.horizontal(|ui| {
                    ui.label(path);
                    match session.outcome_for(path) {
                        Some(outcome) => {
                            super::status::status_badge(ui, import_outcome(outcome));
                            if let ImportOutcome::Rejected { error, .. } = outcome {
                                ui.label(
                                    RichText::new(&error.message)
                                        .color(ui.visuals().error_fg_color),
                                );
                                if error.retryable && ui.button("Retry this file").clicked() {
                                    retry = Some(path.clone());
                                }
                            }
                            if let ImportOutcome::Skipped {
                                existing_recording_id,
                                ..
                            } = outcome
                            {
                                let id = *existing_recording_id;
                                if ui.button("Open the recording you already have").clicked() {
                                    intent = Some(ImportIntent::OpenRecording(id));
                                }
                            }
                        }
                        None => {
                            ui.label(if session.is_pending() {
                                "Importing…"
                            } else {
                                "No outcome reported"
                            });
                        }
                    }
                });
            }
            for path in session.unrepresentable() {
                ui.horizontal(|ui| {
                    ui.label(path);
                    ui.label(
                        RichText::new(
                            "Not imported: this file's name is not valid Unicode and cannot be \
                             sent to the importer.",
                        )
                        .color(ui.visuals().error_fg_color),
                    );
                });
            }
        });
    });

    if dismiss {
        session.clear();
    }
    if let Some(path) = retry {
        // The caller owns the sender, so a retry leaves here as an intent
        // rather than being dispatched from inside a paint.
        intent = Some(ImportIntent::RetryPath(path));
    }
    intent
}

fn progress_fraction(session: &ImportSession) -> f32 {
    let total = session.batch().len();
    if total == 0 {
        return 0.0;
    }
    session.results().len() as f32 / total as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::library::testing::recording_id;
    use plaudon_app::dto::ImportFilesResponse;
    use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog};
    use plaudon_domain::ids::JobId;
    use plaudon_domain::media::Sha256Digest;
    use plaudon_domain::time::Timestamp;

    #[derive(Debug, Default)]
    struct Recorder {
        sent: Vec<AppCommand>,
        next: u64,
    }

    impl Recorder {
        fn send(&mut self) -> impl FnMut(AppCommand) -> CommandId + '_ {
            |command| {
                self.next += 1;
                self.sent.push(command);
                CommandId::from_raw(self.next)
            }
        }

        fn requests(&self) -> Vec<&ImportFilesRequest> {
            self.sent
                .iter()
                .filter_map(|command| match command {
                    AppCommand::ImportFiles(request) => Some(request),
                    _ => None,
                })
                .collect()
        }
    }

    fn envelope() -> ErrorEnvelope {
        let mut log = DiagnosticLog::new();
        AppError::new(AppErrorCode::UnsupportedMedia, "detail")
            .to_envelope(&mut log, Timestamp::now())
    }

    fn accepted(path: &str) -> ImportOutcome {
        ImportOutcome::Accepted {
            path: path.to_owned(),
            job_id: JobId::new(),
        }
    }

    fn duplicate(path: &str) -> ImportOutcome {
        ImportOutcome::Duplicate {
            path: path.to_owned(),
            sha256: Sha256Digest::of(b"same"),
            existing_recording_id: recording_id(3),
            existing_title: "Board meeting".to_owned(),
        }
    }

    fn rejected(path: &str) -> ImportOutcome {
        ImportOutcome::Rejected {
            path: path.to_owned(),
            error: envelope(),
        }
    }

    #[test]
    fn a_batch_forwards_every_path_verbatim() {
        let mut session = ImportSession::default();
        let mut recorder = Recorder::default();
        session.start(
            &["/tmp/a.wav".to_owned(), "/tmp/notes.txt".to_owned()],
            DuplicatePolicy::Ask,
            &mut recorder.send(),
        );
        assert_eq!(
            recorder.requests()[0].paths,
            ["/tmp/a.wav", "/tmp/notes.txt"],
            "filtering locally would hide why Rust rejected a file"
        );
        assert!(session.is_pending());
    }

    #[test]
    fn a_batch_larger_than_the_command_maximum_is_chunked_not_rejected() {
        let mut session = ImportSession::default();
        let mut recorder = Recorder::default();
        let paths: Vec<String> = (0..MAX_IMPORT_PATHS + 3)
            .map(|index| format!("/tmp/{index}.wav"))
            .collect();
        session.start(&paths, DuplicatePolicy::Ask, &mut recorder.send());
        let requests = recorder.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].paths.len(), MAX_IMPORT_PATHS);
        assert_eq!(requests[1].paths.len(), 3);
    }

    #[test]
    fn one_rejected_file_does_not_abort_the_batch() {
        let mut session = ImportSession::default();
        let mut recorder = Recorder::default();
        session.start(
            &["/tmp/a.wav".to_owned(), "/tmp/b.txt".to_owned()],
            DuplicatePolicy::Ask,
            &mut recorder.send(),
        );
        session.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ImportFiles(ImportFilesResponse {
                results: vec![accepted("/tmp/a.wav"), rejected("/tmp/b.txt")],
            })),
        );
        let counts = summarize(session.results());
        assert_eq!(counts.accepted, 1);
        assert_eq!(counts.rejected, 1);
        assert_eq!(session.batch().len(), 2, "both files keep a row");
    }

    #[test]
    fn the_batch_summary_states_every_outcome_class() {
        let summary = describe_batch(OutcomeCounts {
            accepted: 2,
            duplicate: 1,
            skipped: 0,
            rejected: 1,
        });
        assert_eq!(summary.title, "Import finished with decisions to make");
        assert_eq!(summary.body, "2 imported, 1 duplicate, 1 rejected.");
        assert_eq!(summary.tone, StatusTone::Warning);
    }

    #[test]
    fn an_all_accepted_batch_is_announced_plainly() {
        let summary = describe_batch(OutcomeCounts {
            accepted: 3,
            ..OutcomeCounts::default()
        });
        assert_eq!(summary.title, "Import finished");
        assert_eq!(summary.body, "3 imported.");
        assert_eq!(summary.tone, StatusTone::Success);
    }

    #[test]
    fn a_duplicate_waits_for_an_explicit_decision() {
        let mut session = ImportSession::default();
        let mut recorder = Recorder::default();
        session.start(
            &["/tmp/a.wav".to_owned()],
            DuplicatePolicy::Ask,
            &mut recorder.send(),
        );
        session.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ImportFiles(ImportFilesResponse {
                results: vec![duplicate("/tmp/a.wav")],
            })),
        );
        assert_eq!(session.duplicates().len(), 1);
        assert!(session.wants_duplicate_dialog());
        session.decide_later();
        assert!(
            !session.wants_duplicate_dialog(),
            "deciding later keeps the file visible but closes the dialog"
        );
        assert_eq!(session.duplicates().len(), 1);
    }

    #[test]
    fn resolving_a_duplicate_re_runs_that_path_under_the_chosen_policy() {
        let mut session = ImportSession::default();
        let mut recorder = Recorder::default();
        session.start(
            &["/tmp/a.wav".to_owned()],
            DuplicatePolicy::Ask,
            &mut recorder.send(),
        );
        session.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ImportFiles(ImportFilesResponse {
                results: vec![duplicate("/tmp/a.wav")],
            })),
        );
        let open = session.resolve_duplicate(
            "/tmp/a.wav",
            DuplicateChoice::ImportAnyway,
            &mut recorder.send(),
        );
        assert!(open.is_none());
        let requests = recorder.requests();
        assert_eq!(requests[1].duplicate_policy, DuplicatePolicy::ImportAnyway);
        assert_eq!(requests[1].paths, ["/tmp/a.wav"]);
    }

    #[test]
    fn choosing_the_existing_recording_reports_which_one_to_open() {
        let mut session = ImportSession::default();
        let mut recorder = Recorder::default();
        session.start(
            &["/tmp/a.wav".to_owned()],
            DuplicatePolicy::Ask,
            &mut recorder.send(),
        );
        session.on_response(
            CommandId::from_raw(1),
            &Ok(AppResponse::ImportFiles(ImportFilesResponse {
                results: vec![duplicate("/tmp/a.wav")],
            })),
        );
        let open = session.resolve_duplicate(
            "/tmp/a.wav",
            DuplicateChoice::OpenExisting,
            &mut recorder.send(),
        );
        assert_eq!(open, Some(recording_id(3)));
    }

    #[test]
    fn a_second_decision_replaces_the_first_outcome_for_that_path() {
        let mut results = vec![duplicate("/tmp/a.wav"), accepted("/tmp/b.wav")];
        merge_outcomes(&mut results, vec![accepted("/tmp/a.wav")]);
        assert_eq!(results.len(), 2);
        assert!(matches!(results[0], ImportOutcome::Accepted { .. }));
        assert_eq!(outcome_path(&results[1]), "/tmp/b.wav");
    }

    #[test]
    fn a_whole_request_failure_is_kept_as_an_envelope() {
        let mut session = ImportSession::default();
        let mut recorder = Recorder::default();
        session.start(
            &["/tmp/a.wav".to_owned()],
            DuplicatePolicy::Ask,
            &mut recorder.send(),
        );
        session.on_response(CommandId::from_raw(1), &Err(envelope()));
        assert!(session.error().is_some());
        assert!(!session.is_pending());
        assert!(session.announcement().starts_with("Import failed."));
    }

    #[test]
    fn the_extension_filter_is_case_insensitive_and_only_an_affordance() {
        assert!(looks_like_audio(Path::new("/tmp/A.WAV")));
        assert!(looks_like_audio(Path::new("/tmp/a.opus")));
        assert!(!looks_like_audio(Path::new("/tmp/a.txt")));
    }
}
