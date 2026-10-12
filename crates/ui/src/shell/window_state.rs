//! Window geometry: restoring it, and watching it change (parity row S12).
//!
//! The Tauri shell persisted nothing — the window-state plugin was never
//! installed — so every launch opened at 1280×820 wherever the window manager
//! felt like. The model, the sanitising and the off-screen rescue live in
//! [`plaudon_app::shell_state`] so they are testable without a window; this
//! module is the egui half: turning a geometry into a viewport, and reading
//! the live one back out of [`egui::ViewportInfo`].
//!
//! Shape taken from wordcraft's `window_geometry.rs` as described in
//! `docs/EGUI_REFERENCE_COMPONENTS.md` §4.5 (`track`, `sanitized`, `apply`,
//! `rescue_position`). No code was copied; that repository is not vendored.

use plaudon_app::shell_state::{WindowGeometry, MIN_WINDOW_SIZE};

/// The window title.
pub const WINDOW_TITLE: &str = "Plaudon";

/// The viewport the binary opens, with the persisted geometry applied.
///
/// The minimum size is the one `tauri.conf.json` declared, so the layout
/// never has to cope with a window smaller than it was designed for.
#[must_use]
pub fn viewport(geometry: WindowGeometry) -> egui::ViewportBuilder {
    let geometry = geometry.sanitized(None);
    let mut builder = egui::ViewportBuilder::default()
        .with_title(WINDOW_TITLE)
        .with_inner_size(egui::vec2(geometry.width, geometry.height))
        .with_min_inner_size(egui::vec2(MIN_WINDOW_SIZE[0], MIN_WINDOW_SIZE[1]))
        .with_maximized(geometry.maximized);
    if let Some([x, y]) = geometry.position {
        builder = builder.with_position(egui::pos2(x, y));
    }
    builder
}

/// Watches the live window and reports when its geometry changed.
///
/// Reading is free — egui already has the numbers — so there is no polling
/// and no extra repaint: the tracker only looks at what the frame already
/// knows.
#[derive(Debug, Clone)]
pub struct WindowTracker {
    geometry: WindowGeometry,
    dirty: bool,
}

impl WindowTracker {
    /// Track a window that was opened with `geometry`.
    #[must_use]
    pub fn new(geometry: WindowGeometry) -> Self {
        Self {
            geometry,
            dirty: false,
        }
    }

    /// The geometry as of the last [`WindowTracker::track`].
    #[must_use]
    pub fn geometry(&self) -> WindowGeometry {
        self.geometry
    }

    /// Read this frame's viewport and record any change.
    ///
    /// Returns `true` when something changed, which is the shell's cue to
    /// persist. A maximised window keeps the restored size it had before it
    /// was maximised, so unmaximising lands where the user left it.
    pub fn track(&mut self, ctx: &egui::Context) -> bool {
        let (inner, outer, maximized, monitor) = ctx.input(|input| {
            let viewport = input.viewport();
            (
                viewport.inner_rect,
                viewport.outer_rect,
                viewport.maximized.unwrap_or(false),
                viewport.monitor_size,
            )
        });
        let mut next = self.geometry;
        next.maximized = maximized;
        if !maximized {
            if let Some(rect) = inner {
                if rect.width() > 0.0 && rect.height() > 0.0 {
                    next.width = rect.width();
                    next.height = rect.height();
                }
            }
            if let Some(rect) = outer {
                next.position = Some([rect.min.x, rect.min.y]);
            }
        }
        let next = next.sanitized(monitor.map(|size| [size.x, size.y]));
        if next == self.geometry {
            return false;
        }
        self.geometry = next;
        self.dirty = true;
        true
    }

    /// The geometry to persist, if it changed since the last call.
    pub fn take_dirty(&mut self) -> Option<WindowGeometry> {
        self.dirty.then(|| {
            self.dirty = false;
            self.geometry
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_viewport_carries_the_persisted_size_and_the_minimum() {
        let geometry = WindowGeometry {
            width: 1400.0,
            height: 900.0,
            position: Some([64.0, 32.0]),
            maximized: true,
        };
        let builder = viewport(geometry);
        assert_eq!(builder.inner_size, Some(egui::vec2(1400.0, 900.0)));
        assert_eq!(
            builder.min_inner_size,
            Some(egui::vec2(MIN_WINDOW_SIZE[0], MIN_WINDOW_SIZE[1]))
        );
        assert_eq!(builder.position, Some(egui::pos2(64.0, 32.0)));
        assert_eq!(builder.maximized, Some(true));
    }

    #[test]
    fn a_corrupt_persisted_geometry_still_opens_a_usable_window() {
        let builder = viewport(WindowGeometry {
            width: 0.0,
            height: f32::NAN,
            position: Some([f32::NAN, 0.0]),
            maximized: false,
        });
        assert_eq!(builder.inner_size, Some(egui::vec2(1280.0, 820.0)));
        assert_eq!(builder.position, None);
    }

    #[test]
    fn nothing_is_dirty_until_the_window_moves() {
        let mut tracker = WindowTracker::new(WindowGeometry::default());
        assert_eq!(tracker.take_dirty(), None);
        assert_eq!(tracker.geometry(), WindowGeometry::default());
    }
}
