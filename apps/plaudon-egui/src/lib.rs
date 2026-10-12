//! The egui shell: the view, the host hooks and the window.
//!
//! EPIC O task O1.2 (#200). This is the spike that proves the architecture the
//! rest of EPIC O is built on, end to end and with no stubs:
//!
//! * one owned Tokio runtime and one set of ports, from
//!   [`plaudon_app::runtime::AppEngine`];
//! * every action goes through [`plaudon_app::command::Dispatcher`], so the
//!   render thread never awaits, never touches SQLite and never validates
//!   anything itself;
//! * results and §15.3 events arrive as [`plaudon_app::event::AppEvent`]s on a
//!   bounded coalescing channel whose [`plaudon_app::event::Waker`] is
//!   `egui::Context::request_repaint` — so the window repaints when something
//!   happened and **only** then;
//! * the native file dialog is a [host hook](HostHooks), never a direct `rfd`
//!   call from the view, which is what keeps the kittest harness hermetic;
//! * the split between the library list and the activity log is drawn with the
//!   vendored `plaudon_craft_ui::layout::ResizeHandle`, which carries
//!   `Role::Splitter`, a numeric value and keyboard resizing.
//!
//! What it is not: the finished interface. Screens, theming, navigation and the
//! command palette are O1.3 and later, and they live in `plaudon-ui`.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Align, Color32, Layout, RichText, Vec2};
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    DuplicatePolicy, ImportFilesRequest, ImportOutcome, ListRecordingsRequest, RecordingFilter,
    RecordingSort, RecordingSortField, RecordingSummary, SortDirection,
};
use plaudon_app::event::{AppEvent, ResyncReason, Waker};
use plaudon_app::runtime::AppEngine;
use plaudon_craft_ui::layout::{split_rect, ResizeHandle, ResizeHandleStyle, SplitAxis, SplitSize};
use plaudon_domain::error::ErrorEnvelope;

/// How many recordings one page of the spike's list holds.
const PAGE_SIZE: u32 = 100;

/// Minimum width of either side of the split, in points.
const MIN_PANE: f32 = 220.0;

/// Width of the drawn divider, in points.
const DIVIDER: f32 = 6.0;

/// How many activity lines are kept.
const LOG_LIMIT: usize = 200;

/// Platform capabilities the view is not allowed to reach for itself.
///
/// The view calls a hook; the binary decides what the hook does. `rfd` is wired
/// in `main.rs`; a test wires a closure that returns fixed paths, so the test
/// never opens a window or touches the OS.
#[derive(Clone)]
pub struct HostHooks {
    /// Ask the user for audio files to import. Returns an empty vector when the
    /// user cancels.
    pub pick_audio_files: Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync>,
}

impl fmt::Debug for HostHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HostHooks")
    }
}

impl HostHooks {
    /// Hooks that answer nothing, for a headless run.
    #[must_use]
    pub fn none() -> Self {
        Self {
            pick_audio_files: Arc::new(Vec::new),
        }
    }

    /// The real dialogs.
    #[must_use]
    pub fn native() -> Self {
        Self {
            pick_audio_files: Arc::new(|| {
                rfd::FileDialog::new()
                    .set_title("Import audio")
                    .add_filter(
                        "Audio",
                        &["wav", "mp3", "m4a", "flac", "ogg", "opus", "aac"],
                    )
                    .pick_files()
                    .unwrap_or_default()
            }),
        }
    }
}

/// The waker the engine calls: one repaint per batch of events, no polling.
#[derive(Debug, Clone)]
struct RepaintWaker {
    ctx: egui::Context,
}

impl Waker for RepaintWaker {
    fn wake(&self) {
        self.ctx.request_repaint();
    }
}

/// Everything the view renders, and nothing else.
///
/// Plain data: no handles, no futures, no database rows. The engine fills it in
/// response to commands the view sent.
#[derive(Debug, Default)]
pub struct ViewState {
    /// The current page of the library.
    pub recordings: Vec<RecordingSummary>,
    /// Total rows matching the query, as of the last answer.
    pub total: u64,
    /// One line per observed event or outcome, newest last.
    pub activity: Vec<String>,
    /// Status line under the toolbar.
    pub status: String,
    /// The command whose answer the list is waiting for.
    pub pending_list: Option<CommandId>,
    /// The import the user started, if one is running.
    pub pending_import: Option<CommandId>,
    /// Width of the library pane, in points.
    pub list_width: f32,
    /// Whether the library has been fetched since the last resynchronisation.
    pub library_fetched: bool,
}

/// The spike application.
#[derive(Debug)]
pub struct PlaudonEgui {
    engine: AppEngine,
    hooks: HostHooks,
    state: ViewState,
    waker_installed: bool,
}

impl PlaudonEgui {
    /// Build the view over a running engine.
    #[must_use]
    pub fn new(engine: AppEngine, hooks: HostHooks) -> Self {
        Self {
            engine,
            hooks,
            state: ViewState {
                list_width: 520.0,
                status: "Starting".to_owned(),
                ..ViewState::default()
            },
            waker_installed: false,
        }
    }

    /// The rendered state, for tests.
    #[must_use]
    pub fn state(&self) -> &ViewState {
        &self.state
    }

    /// Install the repaint waker once the interface's context exists.
    ///
    /// Idempotent: the context is only available from the first frame, which is
    /// exactly why the engine takes a waker rather than making one.
    pub fn install_waker(&mut self, ctx: &egui::Context) {
        if !self.waker_installed {
            self.engine
                .set_waker(Arc::new(RepaintWaker { ctx: ctx.clone() }));
            self.waker_installed = true;
        }
    }

    /// The engine, for tests that assert on the application layer.
    #[must_use]
    pub fn engine(&self) -> &AppEngine {
        &self.engine
    }

    fn list_command() -> AppCommand {
        AppCommand::ListRecordings(ListRecordingsRequest {
            cursor: None,
            limit: PAGE_SIZE,
            filter: RecordingFilter {
                query: None,
                import_status: Vec::new(),
                transcription_status: Vec::new(),
                sources: Vec::new(),
                tags: Vec::new(),
                date_from: None,
                date_to: None,
            },
            sort: RecordingSort {
                field: RecordingSortField::RecordedAt,
                direction: SortDirection::Desc,
            },
        })
    }

    fn log(&mut self, line: impl Into<String>) {
        self.state.activity.push(line.into());
        if self.state.activity.len() > LOG_LIMIT {
            self.state.activity.remove(0);
        }
    }

    fn refresh_library(&mut self) {
        let id = self.engine.dispatcher().send(Self::list_command());
        self.state.pending_list = Some(id);
        self.state.status = "Loading library…".to_owned();
    }

    fn start_import(&mut self) {
        let paths = (self.hooks.pick_audio_files)();
        if paths.is_empty() {
            self.state.status = "Import cancelled".to_owned();
            return;
        }
        let count = paths.len();
        let request = ImportFilesRequest {
            paths: paths
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            duplicate_policy: DuplicatePolicy::OpenExisting,
        };
        let id = self
            .engine
            .dispatcher()
            .send(AppCommand::ImportFiles(request));
        self.state.pending_import = Some(id);
        self.state.status = format!("Importing {count} file(s)…");
        self.log(format!("import started ({count} file(s))"));
    }

    /// Apply everything the engine has produced since the last frame.
    ///
    /// Pure state transition: it sends commands but never blocks, so it is safe
    /// on the render thread and can be driven directly by a test.
    pub fn pump_events(&mut self) {
        for event in self.engine.events().drain() {
            match event {
                AppEvent::Resync { reason, dropped } => {
                    // The event stream says "your cache may be stale"; the only
                    // correct answer is to refetch, never to guess.
                    self.state.library_fetched = false;
                    self.log(match reason {
                        ResyncReason::Subscribed => "subscribed: fetching the library".to_owned(),
                        ResyncReason::SeqGap => "event sequence gap: refetching".to_owned(),
                        ResyncReason::Overflow => {
                            format!("event backlog overflowed ({dropped} dropped): refetching")
                        }
                    });
                    self.refresh_library();
                }
                AppEvent::Ui(ui_event) => {
                    self.log(format!(
                        "event #{} {}",
                        ui_event.seq,
                        ui_event.kind.as_str()
                    ));
                    // Events never carry state: they invalidate, the view
                    // refetches.
                    self.refresh_library();
                }
                AppEvent::Response {
                    id,
                    command,
                    result,
                } => self.apply_response(id, command, result),
            }
        }
    }

    fn apply_response(
        &mut self,
        id: CommandId,
        command: &'static str,
        result: Result<AppResponse, ErrorEnvelope>,
    ) {
        match result {
            Ok(AppResponse::ListRecordings(page)) => {
                self.state.total = page.total;
                self.state.recordings = page.items;
                self.state.library_fetched = true;
                if self.state.pending_list == Some(id) {
                    self.state.pending_list = None;
                }
                self.state.status = format!("{} recording(s)", self.state.total);
            }
            Ok(AppResponse::ImportFiles(response)) => {
                if self.state.pending_import == Some(id) {
                    self.state.pending_import = None;
                }
                for outcome in &response.results {
                    self.log(describe_import(outcome));
                }
                self.state.status = format!("Imported {} file(s)", response.results.len());
                self.refresh_library();
            }
            Ok(other) => self.log(format!("{command} answered ({})", other.command())),
            Err(envelope) => {
                if self.state.pending_list == Some(id) {
                    self.state.pending_list = None;
                }
                if self.state.pending_import == Some(id) {
                    self.state.pending_import = None;
                }
                // The envelope is the only thing the user ever sees: it carries
                // a canonical message and a diagnostic id, never a path, a key
                // or transcript text (spec §15.4).
                self.state.status = envelope.message.clone();
                self.log(format!(
                    "{command} failed: {} ({})",
                    envelope.message, envelope.diagnostic_id
                ));
            }
        }
    }

    /// Paint one frame.
    ///
    /// Takes a plain `Ui` rather than a `Context` so the same code paints in
    /// `eframe` and in a kittest harness.
    pub fn view(&mut self, ui: &mut egui::Ui) {
        self.pump_events();
        if !self.state.library_fetched && self.state.pending_list.is_none() {
            self.refresh_library();
        }

        ui.horizontal(|ui| {
            if ui.button("Import audio…").clicked() {
                self.start_import();
            }
            if ui.button("Refresh library").clicked() {
                self.refresh_library();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(&self.state.status).color(Color32::GRAY));
            });
        });
        ui.separator();

        let available = ui.available_rect_before_wrap();
        let Some(layout) = split_rect(
            available,
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(self.state.list_width),
            DIVIDER,
            MIN_PANE,
        ) else {
            ui.label("The window is too small to lay out.");
            return;
        };

        let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(layout.first));
        self.library_list(&mut list_ui);

        let resize = ResizeHandle::new(
            egui::Id::new("plaudon-egui-divider"),
            SplitAxis::Horizontal,
            layout.divider,
            "Resize the library pane",
            ResizeHandleStyle {
                hovered: ui.visuals().widgets.hovered.bg_fill,
                dragged: ui.visuals().widgets.active.bg_fill,
                focused: ui.visuals().selection.bg_fill,
            },
        )
        .value(
            layout.first_size(),
            MIN_PANE..=(layout.available() - MIN_PANE),
        )
        .show(ui);
        if resize.delta != 0.0 {
            if let Some(SplitSize::FixedFirst(width)) = layout.resized(
                SplitSize::FixedFirst(self.state.list_width),
                resize.delta,
                MIN_PANE,
            ) {
                self.state.list_width = width;
            }
        }

        let mut log_ui = ui.new_child(egui::UiBuilder::new().max_rect(layout.second));
        self.activity_log(&mut log_ui);
    }

    fn library_list(&mut self, ui: &mut egui::Ui) {
        ui.heading("Library");
        if self.state.recordings.is_empty() {
            ui.label(if self.state.library_fetched {
                "No recordings yet. Import audio to get started."
            } else {
                "Loading…"
            });
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("plaudon-egui-library")
            .auto_shrink([false, false])
            .show_rows(
                ui,
                ui.text_style_height(&egui::TextStyle::Body) + 6.0,
                self.state.recordings.len(),
                |ui, range| {
                    for row in range {
                        let Some(recording) = self.state.recordings.get(row) else {
                            continue;
                        };
                        ui.horizontal(|ui| {
                            ui.label(&recording.title);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(format_duration(recording.duration_ms));
                            });
                        });
                    }
                },
            );
    }

    fn activity_log(&mut self, ui: &mut egui::Ui) {
        ui.heading("Activity");
        egui::ScrollArea::vertical()
            .id_salt("plaudon-egui-activity")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.state.activity {
                    ui.label(line);
                }
            });
    }
}

impl eframe::App for PlaudonEgui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.install_waker(ui.ctx());
        egui::Frame::central_panel(ui.style()).show(ui, |ui| self.view(ui));
        // No `request_repaint` here: at idle this application paints nothing.
    }
}

/// Human duration, or an honest dash when it was never measured.
fn format_duration(duration_ms: Option<u64>) -> String {
    let Some(ms) = duration_ms else {
        return "—".to_owned();
    };
    let total_seconds = ms / 1000;
    let (hours, minutes, seconds) = (
        total_seconds / 3600,
        (total_seconds % 3600) / 60,
        total_seconds % 60,
    );
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// One activity line per import outcome.
///
/// Identifiers and codes only: §15.4 keeps filesystem paths out of anything
/// that can be logged or copied out of the interface.
fn describe_import(outcome: &ImportOutcome) -> String {
    match outcome {
        ImportOutcome::Accepted { job_id, .. } => format!("import accepted, job {job_id}"),
        ImportOutcome::Duplicate {
            existing_recording_id,
            ..
        } => format!("duplicate of recording {existing_recording_id}"),
        ImportOutcome::Skipped {
            existing_recording_id,
            ..
        } => format!("skipped, already imported as {existing_recording_id}"),
        ImportOutcome::Rejected { error, .. } => {
            format!("rejected: {} ({})", error.message, error.diagnostic_id)
        }
    }
}

/// Window geometry, matching the shell being replaced (`tauri.conf.json`).
#[must_use]
pub fn viewport() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title("Plaudon")
        .with_inner_size(Vec2::new(1280.0, 820.0))
        .with_min_inner_size(Vec2::new(880.0, 560.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_that_was_never_measured_is_not_rendered_as_zero() {
        assert_eq!(format_duration(None), "—");
        assert_eq!(format_duration(Some(0)), "0:00");
        assert_eq!(format_duration(Some(61_000)), "1:01");
        assert_eq!(format_duration(Some(3_723_000)), "1:02:03");
    }
}
