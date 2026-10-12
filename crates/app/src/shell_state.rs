//! Presentation state the desktop shell keeps between runs.
//!
//! Window size and position, panel sizes, the appearance and contrast choices
//! and any keyboard bindings the user changed. None of it is library data, so
//! none of it belongs in the database: it is one small JSON file next to the
//! library, written atomically and read leniently.
//!
//! It lives in `plaudon-app` rather than in the view layer for the same reason
//! everything else does — the user interface may call this crate and nothing
//! else, and a render thread must never open a file itself. The types are
//! plain data with no `egui` dependency, so the sanitising rules below are
//! testable headlessly.
//!
//! **Why appearance and contrast are here.** `dto::Preferences` carries both,
//! but nothing writes them: `backend::preferences` returns
//! [`Appearance::System`] and [`Contrast::Standard`] unconditionally and no
//! published command changes them (the React shell kept the choice in a
//! component reducer and lost it on restart). Until a `set_appearance`
//! command exists, the shell's own file is the only honest home for the
//! choice; `Preferences` is still read at startup and is the initial value.
//!
//! **Why bindings are here.** O1.3 ships a configurable command map; O2.4
//! (#208) adds `get_shortcuts`/`set_shortcuts` to the published command
//! surface. Until then the overrides live in this file, isolated in one field
//! so moving them is a deletion.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use plaudon_domain::error::{AppError, AppErrorCode};
use serde::{Deserialize, Serialize};

use crate::dto::{Appearance, Contrast};

/// File name, inside the application data directory.
pub const SHELL_STATE_FILE_NAME: &str = "shell-state.json";

/// Largest file that will be read.
///
/// The document is a few hundred bytes; anything larger is corruption or
/// someone else's file, and reading it into memory is not an improvement over
/// starting from the defaults.
pub const MAX_SHELL_STATE_BYTES: u64 = 256 * 1024;

/// Window size the shell being replaced opened with (`tauri.conf.json`).
pub const DEFAULT_WINDOW_SIZE: [f32; 2] = [1280.0, 820.0];

/// Smallest window the layout supports (`tauri.conf.json`, `tokens.ts`).
pub const MIN_WINDOW_SIZE: [f32; 2] = [880.0, 560.0];

/// Largest window dimension that will be restored, in points.
///
/// A persisted size beyond this is not a window the user had; it is a corrupt
/// value, and applying it would open a window nobody can reach.
pub const MAX_WINDOW_DIMENSION: f32 = 16_384.0;

/// Default width of the navigation sidebar, in points.
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 208.0;

/// Sidebar width bounds, in points.
pub const SIDEBAR_WIDTH_RANGE: (f32, f32) = (168.0, 420.0);

/// Default width of the right-hand pane, in points.
pub const DEFAULT_ASIDE_WIDTH: f32 = 340.0;

/// Right-hand pane width bounds, in points.
pub const ASIDE_WIDTH_RANGE: (f32, f32) = (240.0, 720.0);

/// Keeps a restored window on a monitor: at least this many points of the
/// title area must remain visible.
const RESCUE_MARGIN: f32 = 64.0;

/// Size, position and maximised state of the main window.
///
/// The Tauri shell persisted none of this (parity row S12); the shape of the
/// model and the off-screen rescue follow `window_geometry.rs` from the
/// wordcraft reference application as described in
/// `docs/EGUI_REFERENCE_COMPONENTS.md` §4.5. No code was copied: that
/// repository is not vendored here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowGeometry {
    /// Inner width in logical points.
    pub width: f32,
    /// Inner height in logical points.
    pub height: f32,
    /// Outer position in monitor space, when the platform reports one.
    pub position: Option<[f32; 2]>,
    /// Whether the window was maximised when it was last closed.
    pub maximized: bool,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self {
            width: DEFAULT_WINDOW_SIZE[0],
            height: DEFAULT_WINDOW_SIZE[1],
            position: None,
            maximized: false,
        }
    }
}

impl WindowGeometry {
    /// A geometry that can be applied to a window on `monitor`.
    ///
    /// Non-finite and out-of-range sizes fall back to the defaults, sizes are
    /// clamped to the supported range, and a position that would open the
    /// window off-screen is rescued rather than dropped — losing the position
    /// moves the window, losing the rescue loses the window.
    #[must_use]
    pub fn sanitized(self, monitor: Option<[f32; 2]>) -> Self {
        let width = sane_dimension(self.width, DEFAULT_WINDOW_SIZE[0], MIN_WINDOW_SIZE[0]);
        let height = sane_dimension(self.height, DEFAULT_WINDOW_SIZE[1], MIN_WINDOW_SIZE[1]);
        let (width, height) = match monitor {
            Some([mw, mh]) if mw.is_finite() && mh.is_finite() && mw > 0.0 && mh > 0.0 => (
                width.min(mw.max(MIN_WINDOW_SIZE[0])),
                height.min(mh.max(MIN_WINDOW_SIZE[1])),
            ),
            _ => (width, height),
        };
        Self {
            width,
            height,
            position: self
                .position
                .and_then(|position| rescue_position(position, [width, height], monitor)),
            maximized: self.maximized,
        }
    }
}

/// Move `position` back onto `monitor` when it would leave the window
/// unreachable, or drop it when it cannot be rescued.
///
/// `None` means "let the window manager place the window", which is always a
/// usable outcome.
#[must_use]
pub fn rescue_position(
    position: [f32; 2],
    size: [f32; 2],
    monitor: Option<[f32; 2]>,
) -> Option<[f32; 2]> {
    let [x, y] = position;
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let Some([mw, mh]) = monitor else {
        // No monitor information: keep a position that is at least plausible.
        return (x > -MAX_WINDOW_DIMENSION
            && x < MAX_WINDOW_DIMENSION
            && y > -MAX_WINDOW_DIMENSION
            && y < MAX_WINDOW_DIMENSION)
            .then_some([x, y]);
    };
    if !mw.is_finite() || !mh.is_finite() || mw <= 0.0 || mh <= 0.0 {
        return Some([x, y]);
    }
    let max_x = (mw - RESCUE_MARGIN).max(0.0);
    let max_y = (mh - RESCUE_MARGIN).max(0.0);
    let min_x = -(size[0] - RESCUE_MARGIN).max(0.0);
    Some([x.clamp(min_x, max_x), y.clamp(0.0, max_y)])
}

fn sane_dimension(value: f32, default: f32, minimum: f32) -> f32 {
    if !value.is_finite() || value <= 0.0 || value > MAX_WINDOW_DIMENSION {
        return default;
    }
    value.max(minimum)
}

/// Sizes of the shell's resizable panes (parity row S7).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelLayout {
    /// Width of the navigation sidebar in points.
    pub sidebar_width: f32,
    /// Whether the user collapsed the sidebar to its icon rail.
    pub sidebar_collapsed: bool,
    /// Width of the right-hand pane in points, for screens that offer one.
    pub aside_width: f32,
}

impl Default for PanelLayout {
    fn default() -> Self {
        Self {
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            sidebar_collapsed: false,
            aside_width: DEFAULT_ASIDE_WIDTH,
        }
    }
}

impl PanelLayout {
    /// Widths clamped to what the layout can actually draw.
    #[must_use]
    pub fn sanitized(self) -> Self {
        Self {
            sidebar_width: clamp_width(
                self.sidebar_width,
                DEFAULT_SIDEBAR_WIDTH,
                SIDEBAR_WIDTH_RANGE,
            ),
            sidebar_collapsed: self.sidebar_collapsed,
            aside_width: clamp_width(self.aside_width, DEFAULT_ASIDE_WIDTH, ASIDE_WIDTH_RANGE),
        }
    }
}

fn clamp_width(value: f32, default: f32, (low, high): (f32, f32)) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        default
    }
}

/// Everything the shell restores at startup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellState {
    /// Main window geometry.
    pub window: WindowGeometry,
    /// Pane sizes.
    pub panels: PanelLayout,
    /// Appearance choice: follow the system, or force light or dark.
    pub appearance: Appearance,
    /// Contrast choice.
    pub contrast: Contrast,
    /// Keyboard bindings the user changed, by command id. An empty vector
    /// means "this command has no binding", which is different from absent.
    pub bindings: BTreeMap<String, Vec<String>>,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            window: WindowGeometry::default(),
            panels: PanelLayout::default(),
            appearance: Appearance::System,
            contrast: Contrast::Standard,
            bindings: BTreeMap::new(),
        }
    }
}

impl ShellState {
    /// Parse a persisted document, keeping every field that is still readable.
    ///
    /// A single malformed entry — a hand-edited width, a renamed enum — must
    /// not cost the user their window size and their bindings, so each field
    /// is decoded on its own and a bad one falls back to its default.
    #[must_use]
    pub fn from_json(text: &str) -> Self {
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text)
        else {
            return Self::default();
        };
        let mut state = Self::default();
        if let Some(value) = map.get("window") {
            state.window = serde_json::from_value(value.clone()).unwrap_or_default();
        }
        if let Some(value) = map.get("panels") {
            state.panels = serde_json::from_value(value.clone()).unwrap_or_default();
        }
        if let Some(value) = map.get("appearance") {
            state.appearance = serde_json::from_value(value.clone()).unwrap_or(Appearance::System);
        }
        if let Some(value) = map.get("contrast") {
            state.contrast = serde_json::from_value(value.clone()).unwrap_or(Contrast::Standard);
        }
        if let Some(value) = map.get("bindings") {
            state.bindings = serde_json::from_value(value.clone()).unwrap_or_default();
        }
        state.sanitized(None)
    }

    /// The document as it is written to disk.
    ///
    /// # Errors
    ///
    /// Fails only if the state cannot be serialised, which would be a bug in
    /// this crate rather than a user-visible condition.
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string_pretty(self).map_err(|err| {
            AppError::new(
                AppErrorCode::InternalError,
                format!("the shell state could not be serialised: {err}"),
            )
        })
    }

    /// Every value brought back into a range the shell can render.
    #[must_use]
    pub fn sanitized(mut self, monitor: Option<[f32; 2]>) -> Self {
        self.window = self.window.sanitized(monitor);
        self.panels = self.panels.sanitized();
        self
    }
}

/// Reads and writes [`ShellState`] in the application data directory.
#[derive(Debug, Clone)]
pub struct ShellStateStore {
    path: PathBuf,
}

impl ShellStateStore {
    /// Store over `<data_dir>/shell-state.json`.
    #[must_use]
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join(SHELL_STATE_FILE_NAME),
        }
    }

    /// The file this store reads and writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The persisted state, or the defaults.
    ///
    /// Never fails: a missing, oversized, unreadable or malformed file means
    /// the shell opens with its default window and layout, which is a working
    /// application, and the reason is logged.
    #[must_use]
    pub fn load(&self) -> ShellState {
        match std::fs::metadata(&self.path) {
            Ok(metadata) if metadata.len() > MAX_SHELL_STATE_BYTES => {
                tracing::warn!(
                    bytes = metadata.len(),
                    "the shell state file is too large to be one; using defaults"
                );
                return ShellState::default();
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ShellState::default();
            }
            Err(error) => {
                tracing::warn!(%error, "the shell state file could not be read; using defaults");
                return ShellState::default();
            }
        }
        match std::fs::read_to_string(&self.path) {
            Ok(text) => ShellState::from_json(&text),
            Err(error) => {
                tracing::warn!(%error, "the shell state file could not be read; using defaults");
                ShellState::default()
            }
        }
    }

    /// Write `state`, replacing the previous file atomically.
    ///
    /// Blocking I/O: call it from the runtime or at shutdown, never from a
    /// render thread.
    ///
    /// # Errors
    ///
    /// Fails when the directory cannot be created or the file cannot be
    /// written or renamed.
    pub fn save(&self, state: &ShellState) -> Result<(), AppError> {
        let body = state.to_json()?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| storage_error("directory", err))?;
        }
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, body.as_bytes()).map_err(|err| storage_error("file", err))?;
        std::fs::rename(&temporary, &self.path).map_err(|err| {
            let _ = std::fs::remove_file(&temporary);
            storage_error("file", err)
        })
    }
}

fn storage_error(what: &str, err: std::io::Error) -> AppError {
    AppError::new(
        AppErrorCode::StorageUnavailable,
        format!("the shell state {what} could not be written: {err}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_window_the_shell_being_replaced_opened_with() {
        let state = ShellState::default();
        assert!((state.window.width - 1280.0).abs() < f32::EPSILON);
        assert!((state.window.height - 820.0).abs() < f32::EPSILON);
        assert!(!state.window.maximized);
        assert_eq!(state.appearance, Appearance::System);
        assert_eq!(state.contrast, Contrast::Standard);
    }

    #[test]
    fn a_nonsense_size_falls_back_instead_of_opening_an_unusable_window() {
        let broken = WindowGeometry {
            width: f32::NAN,
            height: 1e30,
            position: Some([f32::INFINITY, 0.0]),
            maximized: false,
        }
        .sanitized(None);
        assert!((broken.width - DEFAULT_WINDOW_SIZE[0]).abs() < f32::EPSILON);
        assert!((broken.height - DEFAULT_WINDOW_SIZE[1]).abs() < f32::EPSILON);
        assert_eq!(broken.position, None);
    }

    #[test]
    fn a_size_below_the_minimum_grows_to_the_minimum() {
        let small = WindowGeometry {
            width: 100.0,
            height: 100.0,
            position: None,
            maximized: false,
        }
        .sanitized(None);
        assert!((small.width - MIN_WINDOW_SIZE[0]).abs() < f32::EPSILON);
        assert!((small.height - MIN_WINDOW_SIZE[1]).abs() < f32::EPSILON);
    }

    #[test]
    fn a_window_remembered_on_a_monitor_that_is_gone_is_rescued_onto_this_one() {
        let geometry = WindowGeometry {
            width: 1280.0,
            height: 820.0,
            position: Some([4000.0, 3000.0]),
            maximized: false,
        }
        .sanitized(Some([1920.0, 1080.0]));
        let position = geometry
            .position
            .expect("the position is rescued, not lost");
        assert!(position[0] <= 1920.0 - RESCUE_MARGIN);
        assert!(position[1] <= 1080.0 - RESCUE_MARGIN);
        assert!(position[1] >= 0.0);
    }

    #[test]
    fn a_window_mostly_off_the_left_edge_keeps_a_grabbable_strip() {
        let rescued = rescue_position([-5000.0, 10.0], [1280.0, 820.0], Some([1920.0, 1080.0]))
            .expect("rescued");
        assert!(rescued[0] >= -(1280.0 - RESCUE_MARGIN));
    }

    #[test]
    fn panel_widths_are_clamped_to_what_can_be_drawn() {
        let panels = PanelLayout {
            sidebar_width: 5.0,
            sidebar_collapsed: true,
            aside_width: 10_000.0,
        }
        .sanitized();
        assert!((panels.sidebar_width - SIDEBAR_WIDTH_RANGE.0).abs() < f32::EPSILON);
        assert!((panels.aside_width - ASIDE_WIDTH_RANGE.1).abs() < f32::EPSILON);
        assert!(panels.sidebar_collapsed);
    }

    #[test]
    fn a_round_trip_through_json_preserves_every_field() {
        let mut state = ShellState {
            window: WindowGeometry {
                width: 1400.0,
                height: 900.0,
                position: Some([120.0, 80.0]),
                maximized: true,
            },
            panels: PanelLayout {
                sidebar_width: 260.0,
                sidebar_collapsed: true,
                aside_width: 400.0,
            },
            appearance: Appearance::Dark,
            contrast: Contrast::High,
            bindings: BTreeMap::new(),
        };
        state
            .bindings
            .insert("command_palette".to_owned(), vec!["Mod+Shift+P".to_owned()]);

        let text = state.to_json().expect("serialises");
        assert_eq!(ShellState::from_json(&text), state);
    }

    #[test]
    fn one_malformed_field_does_not_cost_the_others() {
        let text = r#"{
            "window": {"width": 1400.0, "height": 900.0, "maximized": true},
            "panels": "this is not an object",
            "appearance": "chartreuse",
            "bindings": {"command_palette": ["Mod+Shift+P"]}
        }"#;
        let state = ShellState::from_json(text);
        assert!((state.window.width - 1400.0).abs() < f32::EPSILON);
        assert!(state.window.maximized);
        assert_eq!(state.panels, PanelLayout::default());
        assert_eq!(state.appearance, Appearance::System);
        assert_eq!(
            state.bindings.get("command_palette").map(Vec::as_slice),
            Some(["Mod+Shift+P".to_owned()].as_slice())
        );
    }

    #[test]
    fn a_file_that_is_not_json_at_all_starts_from_the_defaults() {
        assert_eq!(ShellState::from_json("<html>"), ShellState::default());
        assert_eq!(ShellState::from_json(""), ShellState::default());
    }

    #[test]
    fn the_store_round_trips_through_the_data_directory() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let store = ShellStateStore::new(dir.path());
        assert_eq!(store.load(), ShellState::default());

        let mut state = ShellState::default();
        state.window.width = 1500.0;
        state.panels.sidebar_collapsed = true;
        store.save(&state).expect("the state is written");

        let loaded = store.load();
        assert!((loaded.window.width - 1500.0).abs() < f32::EPSILON);
        assert!(loaded.panels.sidebar_collapsed);
        assert!(!dir.path().join("shell-state.json.tmp").exists());
    }

    #[test]
    fn an_oversized_file_is_refused_rather_than_read() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let store = ShellStateStore::new(dir.path());
        let limit = usize::try_from(MAX_SHELL_STATE_BYTES).expect("the limit fits a usize");
        let filler = "x".repeat(limit + 1);
        std::fs::write(store.path(), filler).expect("write");
        assert_eq!(store.load(), ShellState::default());
    }
}
