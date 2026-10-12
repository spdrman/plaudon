//! The kittest harness Plaudon's user-interface tests are written against.
//!
//! Three things live here so that no test has to reinvent them:
//!
//! 1. **A deterministic harness.** [`harness`] and [`builder`] fix the window
//!    size, the scale factor, the theme and the operating system kittest
//!    reports, and they enable AccessKit. Two runs of the same test on two
//!    machines then produce the same tree and the same pixels.
//! 2. **An accessibility view.** [`a11y`] turns the AccessKit tree into plain
//!    data (`role`, `label`, focusable), which is what replaces the axe
//!    assertions the React suite used — egui has no axe equivalent.
//! 3. **The snapshot policy.** [`visual`] implements the decision recorded in
//!    `docs/EGUI_TESTING.md`: baselines are committed under `tests/visual/`,
//!    they are rendered on MARS with a software adapter, and a developer
//!    machine does not compare pixels unless it asks to.
//!
//! The crate deliberately knows nothing about Plaudon's screens: it is a test
//! utility, not a place for view code.

pub use egui_kittest::kittest;
pub use egui_kittest::{Harness, HarnessBuilder, SnapshotOptions};

/// The window size every screen test starts from.
///
/// 1280×820 logical points is the size the Tauri shell opened with
/// (`tauri.conf.json`), so a snapshot of a screen is comparable with the
/// frontend it replaces.
pub const DEFAULT_SIZE: egui::Vec2 = egui::vec2(1280.0, 820.0);

/// The compact breakpoint the shell switches layout at, for tests that assert
/// the narrow arrangement.
pub const COMPACT_SIZE: egui::Vec2 = egui::vec2(1000.0, 820.0);

/// Scale factor used by every test.
///
/// 1.0, not the host's: a Retina machine and a CI container must render the
/// same image.
pub const DEFAULT_PIXELS_PER_POINT: f32 = 1.0;

/// A harness builder with Plaudon's deterministic defaults.
///
/// Fixed size and scale, light theme, and a fixed reported operating system so
/// that shortcut rendering (`Cmd` versus `Ctrl`) does not depend on where the
/// test runs.
#[must_use]
pub fn builder<State>() -> HarnessBuilder<State> {
    Harness::<State>::builder()
        .with_size(DEFAULT_SIZE)
        .with_pixels_per_point(DEFAULT_PIXELS_PER_POINT)
        .with_theme(egui::Theme::Light)
        .with_os(egui::os::OperatingSystem::Mac)
        .with_options(visual::options())
}

/// A harness over a closure that paints into a `Ui`.
pub fn harness<'a>(app: impl FnMut(&mut egui::Ui) + 'a) -> Harness<'a> {
    builder().build_ui(app)
}

/// A harness over a closure that paints from, and mutates, `state`.
pub fn harness_with_state<'a, State>(
    app: impl FnMut(&mut egui::Ui, &mut State) + 'a,
    state: State,
) -> Harness<'a, State> {
    builder().build_ui_state(app, state)
}

/// Reading the AccessKit tree a frame produced, and walking its focus order.
///
/// The React suite asserted accessibility with axe; egui has no equivalent, so
/// the replacement is to assert on roles, labels and on what a keyboard
/// actually reaches.
pub mod a11y {
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    use egui_kittest::{Harness, Node};

    /// One accessible node, reduced to what a test asserts on.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AccessibleNode {
        /// AccessKit role, rendered as its debug name.
        pub role: String,
        /// Accessible label, when the node has one. A control carries its name
        /// here; static text carries it in [`AccessibleNode::value`] instead,
        /// which is why both are reported.
        pub label: Option<String>,
        /// Accessible value: the text of a label or text run, a field's
        /// contents, a splitter's position.
        pub value: Option<String>,
    }

    impl AccessibleNode {
        /// The text a screen reader announces: the label, or the value when
        /// the node is static text.
        #[must_use]
        pub fn announced(&self) -> Option<&str> {
            self.label.as_deref().or(self.value.as_deref())
        }
    }

    fn describe(node: &Node<'_>) -> AccessibleNode {
        let accessible = node.accesskit_node();
        AccessibleNode {
            role: format!("{:?}", accessible.role()),
            label: accessible.label(),
            value: accessible.value(),
        }
    }

    /// Every accessible node of the current frame, in tree order.
    #[must_use]
    pub fn nodes<State>(harness: &Harness<'_, State>) -> Vec<AccessibleNode> {
        harness
            .root()
            .children_recursive()
            .map(|node| describe(&node))
            .collect()
    }

    /// Whether anything in the frame announces `label`.
    ///
    /// Uses kittest's own query, which resolves a control's label and a text
    /// run's contents the same way a screen reader does.
    #[must_use]
    pub fn has_label<State>(harness: &Harness<'_, State>, label: &str) -> bool {
        harness.query_all_by_label(label).next().is_some()
    }

    /// The label of the currently focused node, if any.
    #[must_use]
    pub fn focused_label<State>(harness: &Harness<'_, State>) -> Option<String> {
        harness
            .root()
            .children_recursive()
            .find(egui_kittest::Node::is_focused)
            .and_then(|node| describe(&node).announced().map(str::to_owned))
    }

    /// Walk the focus order by pressing Tab, and report the labels reached.
    ///
    /// Stops when the order cycles or after `max_stops` presses, so a widget
    /// that traps focus fails the assertion instead of hanging the test. This
    /// is the Rust form of the Playwright check that no interactive element is
    /// unreachable by keyboard.
    pub fn focus_order<State>(harness: &mut Harness<'_, State>, max_stops: usize) -> Vec<String> {
        let mut order: Vec<String> = Vec::new();
        for _ in 0..max_stops {
            harness.key_press(egui::Key::Tab);
            harness.run();
            let Some(label) = focused_label(harness) else {
                continue;
            };
            if order.first() == Some(&label) {
                break;
            }
            order.push(label);
        }
        order
    }
}

/// The snapshot policy.
///
/// **Decision (O1.2, recorded in `docs/EGUI_TESTING.md` §4).** None of the
/// reference applications commits baselines, so Plaudon defines its own rule:
///
/// * baselines are PNGs committed under each crate's `tests/visual/`;
/// * they are produced and compared **only** where the renderer is identical —
///   the MARS Docker image with a software wgpu adapter — which the runner
///   signals by setting `PLAUDON_VISUAL=1`;
/// * everywhere else [`assert_snapshot`] reports [`Outcome::Skipped`] and the
///   interaction assertions of the same test still run, so a developer machine
///   never fails on a GPU difference and never silently "passes" a comparison
///   it did not make;
/// * the tolerance is kittest's per-pixel YIQ threshold at its default `0.6`
///   with **zero** tolerated failing pixels: on one fixed renderer a difference
///   is a real change, not noise. A test that genuinely needs slack states it
///   locally with [`SnapshotOptions::max_failed_pixels`] and says why.
///
/// Regenerating a baseline is therefore a MARS operation:
///
/// ```text
/// scripts/e2e-remote.sh cargo 'UPDATE_SNAPSHOTS=1 PLAUDON_VISUAL=1 cargo test -p <crate> --test <test>'
/// ```
///
/// and the changed PNGs are copied back and committed with the change that
/// caused them.
pub mod visual {
    use egui_kittest::{Harness, SnapshotOptions};

    /// Environment variable that enables pixel comparison.
    pub const ENABLE_ENV: &str = "PLAUDON_VISUAL";

    /// Directory, relative to the crate root, holding committed baselines.
    pub const BASELINE_DIR: &str = "tests/visual";

    /// What a snapshot assertion did.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Outcome {
        /// The image was rendered and matched the committed baseline.
        Compared,
        /// Pixel comparison is not enabled on this machine.
        Skipped,
    }

    /// Whether this process compares pixels.
    #[must_use]
    pub fn enabled() -> bool {
        std::env::var(ENABLE_ENV).is_ok_and(|value| value == "1" || value == "true")
    }

    /// Plaudon's snapshot options: committed baselines, default per-pixel
    /// threshold, no tolerated failing pixels.
    #[must_use]
    pub fn options() -> SnapshotOptions {
        SnapshotOptions::new()
            .output_path(BASELINE_DIR)
            .max_failed_pixels(0)
    }

    /// Compare the current frame with the committed baseline `name`.
    ///
    /// Does nothing unless [`enabled`]; the caller's interaction assertions are
    /// unaffected either way.
    ///
    /// # Panics
    ///
    /// Panics when the rendered frame differs from the baseline, when the
    /// baseline is missing, or when rendering fails — the same conditions
    /// `Harness::snapshot` panics on.
    #[track_caller]
    pub fn assert_snapshot<State>(harness: &mut Harness<'_, State>, name: &str) -> Outcome {
        if !enabled() {
            return Outcome::Skipped;
        }
        harness.snapshot_options(name, &options());
        Outcome::Compared
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable as _;

    #[test]
    fn the_harness_is_deterministic_in_size_scale_and_platform() {
        let mut harness = harness_with_state(
            |ui, painted: &mut egui::Vec2| {
                *painted = ui.max_rect().size();
                ui.label("Plaudon");
            },
            egui::Vec2::ZERO,
        );
        harness.run();
        let painted = *harness.state();
        assert!(
            painted.x <= DEFAULT_SIZE.x && painted.x > DEFAULT_SIZE.x - 40.0,
            "the harness paints into the fixed window, not the host's: {painted:?}"
        );
        assert_eq!(harness.ctx.pixels_per_point(), DEFAULT_PIXELS_PER_POINT);
        assert_eq!(
            harness.ctx.os(),
            egui::os::OperatingSystem::Mac,
            "shortcut rendering must not depend on the host"
        );
        assert!(a11y::has_label(&harness, "Plaudon"));
    }

    #[test]
    fn the_accessibility_view_reports_roles_labels_and_focusability() {
        let mut harness = harness_with_state(
            |ui, clicked: &mut u32| {
                if ui.button("Import audio").clicked() {
                    *clicked += 1;
                }
                ui.label("Library");
            },
            0,
        );
        harness.run();

        let nodes = a11y::nodes(&harness);
        let button = nodes
            .iter()
            .find(|node| node.label.as_deref() == Some("Import audio"))
            .expect("the button is in the accessibility tree");
        assert_eq!(button.role, "Button");
        assert_eq!(
            a11y::focus_order(&mut harness, 4),
            vec!["Import audio".to_owned()],
            "the button is the only keyboard stop; a plain label is not one"
        );

        harness.get_by_label("Import audio").click();
        harness.run();
        assert_eq!(*harness.state(), 1);
    }

    #[test]
    fn pixel_comparison_is_off_unless_the_runner_asks_for_it() {
        // The developer default. MARS sets PLAUDON_VISUAL=1; see
        // docs/EGUI_TESTING.md §4.
        if std::env::var(visual::ENABLE_ENV).is_ok() {
            return;
        }
        let mut harness = harness(|ui| {
            ui.label("Plaudon");
        });
        harness.run();
        assert_eq!(
            visual::assert_snapshot(&mut harness, "unused-baseline"),
            visual::Outcome::Skipped
        );
    }

    #[test]
    fn the_snapshot_options_point_at_committed_baselines_with_no_pixel_slack() {
        let options = visual::options();
        let rendered = format!("{options:?}");
        assert!(rendered.contains("tests/visual"), "{rendered}");
        assert!(rendered.contains("max_failed_pixels: 0"), "{rendered}");
    }
}
