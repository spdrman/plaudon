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

/// Scale factor used by every test that does not ask for another one.
///
/// 1.0, not the host's: a Retina machine and a CI container must render the
/// same image.
pub const DEFAULT_PIXELS_PER_POINT: f32 = 1.0;

/// The scale factors the high-DPI suite renders every screen at (parity row
/// S16 of `docs/EGUI_MIGRATION_PLAN.md` §4.1).
///
/// Three, because three are the ones that behave differently:
///
/// * **1.0** — the integral case and the default of every other test, so a
///   scale run is comparable with the suite it sits next to;
/// * **1.5** — the *fractional* case, where a point is not a whole number of
///   pixels. It is what Windows reports at 150% and what a GNOME fractional
///   session reports, and it is the one that exposes rounding: a border that
///   lands on a half pixel, a row height that no longer divides, a glyph laid
///   out at one size and rasterised at another;
/// * **2.0** — the Retina/200% case, the other integral one, where everything
///   doubles and nothing may be *drawn* twice as thick relative to its box.
pub const SCALE_AXIS: [f32; 3] = [1.0, 1.5, 2.0];

/// A harness builder with Plaudon's deterministic defaults.
///
/// Fixed size and scale, light theme, and a fixed reported operating system so
/// that shortcut rendering (`Cmd` versus `Ctrl`) does not depend on where the
/// test runs.
pub fn builder<State>() -> HarnessBuilder<State> {
    builder_at_scale(DEFAULT_PIXELS_PER_POINT)
}

/// [`builder`], at a chosen scale factor.
///
/// Everything else is the default harness: the window stays 1280×820
/// *logical points*, so a screen gets the same room at every scale and the
/// rendered image grows instead of the layout shrinking — which is what a
/// high-DPI display actually does.
///
/// A snapshot taken through this builder must be named with
/// [`visual::assert_snapshot_at_scale`], which writes the scale into the
/// baseline's file name; see [`visual::scaled_name`].
pub fn builder_at_scale<State>(pixels_per_point: f32) -> HarnessBuilder<State> {
    Harness::<State>::builder()
        .with_size(DEFAULT_SIZE)
        .with_pixels_per_point(pixels_per_point)
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
    use egui::accesskit::Role;
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
            // A fixed number of frames, not `run()`: a screen that is still
            // loading shows a spinner, which legitimately keeps repainting, and
            // a focus walk must not depend on how fast the engine answered.
            harness.run_steps(8);
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

    /// The roles a user *operates*, and which therefore must announce a name.
    ///
    /// WCAG 2.2 AA 4.1.2 requires a name, a role and a value for every user
    /// interface component; egui supplies the role and the value, and the
    /// name is the one a screen can forget. So this list is the definition of
    /// "control" that [`unnamed_controls`] enforces, and it is explicit rather
    /// than derived from `is_focusable`/`is_clickable` for two reasons: those
    /// predicates need a tree filter, which would make the check depend on how
    /// the filter was written; and a *disabled* control is neither focusable
    /// nor clickable while still needing a name, because a screen reader has
    /// to be able to say what it is that is unavailable.
    ///
    /// Membership, role by role:
    ///
    /// * `Button`, `DefaultButton`, `Link` — invoked; the name is the only
    ///   thing that says what invoking them does.
    /// * `CheckBox`, `RadioButton`, `Switch` — toggled; the value says on or
    ///   off and the name says what is on or off.
    /// * `Tab` — selected; Plaudon's navigation rail is a `TabList` of these
    ///   and each one is a destination.
    /// * `MenuItem`, `MenuItemCheckBox`, `MenuItemRadio`, `ListBoxOption`,
    ///   `MenuListOption`, `TreeItem` — chosen from a list; an unnamed one is
    ///   announced as its position and nothing else.
    /// * `TextInput`, `MultilineTextInput`, `PasswordInput`, `SearchInput`,
    ///   `NumberInput` — typed into; the *value* is the user's own text, so
    ///   the name is the only description of what the field is for.
    /// * `ComboBox`, `EditableComboBox` — opened and chosen from.
    /// * `Slider`, `SpinButton`, `Splitter`, `ColorWell` — dragged or
    ///   stepped; the value is a number, which means nothing without a name.
    /// * `ColumnHeader` — in Plaudon a column header is the sort control for
    ///   its column (`screens::library::table`), so it is operated, not decor.
    /// * `DisclosureTriangle` — expands a section; the name says which.
    ///
    /// Deliberately **not** in the list:
    ///
    /// * `ScrollBar` — egui emits one per scroll area and it is not a
    ///   keyboard stop; assistive technology announces the scroll *container*
    ///   and uses the bar as a gesture target, so requiring a name here would
    ///   be inventing a requirement rather than enforcing one.
    /// * `Row`, `Cell`, `GridCell`, `Group` — containers. A row is announced
    ///   through its cells and its own `label` when it has one; the grid is
    ///   what carries the name of the set.
    /// * `Label`, `TextRun`, `Image`, `Status`, `Alert` — not operated. Their
    ///   text is the value, and [`AccessibleNode::announced`] reads it.
    ///
    /// The list is typed, not stringly: a role that does not exist is a
    /// compile error rather than an entry that silently never matches.
    pub const INTERACTIVE_ROLES: &[Role] = &[
        Role::Button,
        Role::CheckBox,
        Role::ColorWell,
        Role::ColumnHeader,
        Role::ComboBox,
        Role::DefaultButton,
        Role::DisclosureTriangle,
        Role::EditableComboBox,
        Role::Link,
        Role::ListBoxOption,
        Role::MenuItem,
        Role::MenuItemCheckBox,
        Role::MenuItemRadio,
        Role::MenuListOption,
        Role::MultilineTextInput,
        Role::NumberInput,
        Role::PasswordInput,
        Role::RadioButton,
        Role::SearchInput,
        Role::Slider,
        Role::SpinButton,
        Role::Splitter,
        Role::Switch,
        Role::Tab,
        Role::TextInput,
        Role::TreeItem,
    ];

    /// One control the suite accepts without a name of its own.
    ///
    /// The exemption exists for the genuine case where the name belongs to
    /// the container: a single field inside a group that already announces
    /// what the field is for, where naming both makes a screen reader say the
    /// same words twice. It is deliberately narrow — it matches **one role
    /// inside one named container** — so it cannot be used to silence a whole
    /// role, and it carries [`NamedByContainer::why`], which is printed in
    /// the failure message of any *other* violation so the reader can see
    /// what was waived next to what was not.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct NamedByContainer {
        /// Role of the control that carries no name.
        pub role: Role,
        /// Label of the ancestor that names it instead. The ancestor must be
        /// in the tree above the control, or the exemption does not apply.
        pub container: &'static str,
        /// Why the container is the right place for that name.
        pub why: &'static str,
    }

    /// A control that is operable and announces nothing.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct UnnamedControl {
        /// Role of the offending node.
        pub role: Role,
        /// Its value, when it has one — the only clue left to identify it.
        pub value: Option<String>,
        /// Labels of its ancestors, nearest first, so the failure message
        /// says *where* in the screen the control is.
        pub ancestors: Vec<String>,
    }

    /// Every control in the current frame that has no accessible name.
    ///
    /// A node counts as named when its `label` has at least one
    /// non-whitespace character. Hidden nodes are skipped: AccessKit marks a
    /// node hidden exactly when it is not presented to the user.
    #[must_use]
    pub fn unnamed_controls<State>(
        harness: &Harness<'_, State>,
        exemptions: &[NamedByContainer],
    ) -> Vec<UnnamedControl> {
        harness
            .root()
            .children_recursive()
            .filter_map(|node| {
                let accessible = node.accesskit_node();
                if accessible.is_hidden() {
                    return None;
                }
                let role = accessible.role();
                if !INTERACTIVE_ROLES.contains(&role) {
                    return None;
                }
                if accessible
                    .label()
                    .is_some_and(|label| !label.trim().is_empty())
                {
                    return None;
                }
                let ancestors = ancestor_labels(&node);
                let exempt = exemptions.iter().any(|exemption| {
                    exemption.role == role
                        && ancestors.iter().any(|label| label == exemption.container)
                });
                if exempt {
                    return None;
                }
                Some(UnnamedControl {
                    role,
                    value: accessible.value(),
                    ancestors,
                })
            })
            .collect()
    }

    /// The labels of a node's ancestors, nearest first.
    fn ancestor_labels(node: &Node<'_>) -> Vec<String> {
        let mut labels = Vec::new();
        let mut current = node.parent();
        while let Some(parent) = current {
            if let Some(label) = parent.accesskit_node().label() {
                if !label.trim().is_empty() {
                    labels.push(label);
                }
            }
            current = parent.parent();
        }
        labels
    }

    /// Assert that every control of the current frame has a role *and* a
    /// non-empty name (parity row S17).
    ///
    /// `screen` names what is on screen, so a failure says which of the
    /// twenty-odd places the suite visits was the one at fault.
    ///
    /// # Panics
    ///
    /// Panics listing every unnamed control, with its role, its value and the
    /// labels of its ancestors.
    #[track_caller]
    pub fn assert_every_control_is_named<State>(
        harness: &Harness<'_, State>,
        screen: &str,
        exemptions: &[NamedByContainer],
    ) {
        let violations = unnamed_controls(harness, exemptions);
        assert!(
            violations.is_empty(),
            "{screen}: {} control(s) are operable but announce nothing.\n{}\nWaived here: {}",
            violations.len(),
            violations
                .iter()
                .map(|control| format!(
                    "  {:?} value={:?} under {:?}",
                    control.role, control.value, control.ancestors
                ))
                .collect::<Vec<_>>()
                .join("\n"),
            if exemptions.is_empty() {
                "nothing".to_owned()
            } else {
                exemptions
                    .iter()
                    .map(|exemption| {
                        format!(
                            "{:?} inside {:?} ({})",
                            exemption.role, exemption.container, exemption.why
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            }
        );
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
    /// Uses `Harness::try_snapshot_options` and fails here, rather than
    /// `Harness::snapshot_options`, which defers the failure to the harness's
    /// own `Drop`. The difference matters for a test that builds **more than
    /// one** harness — the scale suite builds three, one per scale — because
    /// kittest panics when a second unhandled `SnapshotResults` is dropped in
    /// the same thread, hiding the real comparison behind a complaint about
    /// bookkeeping. Failing at the call site also points `#[track_caller]` at
    /// the assertion that failed.
    ///
    /// # Panics
    ///
    /// Panics when the rendered frame differs from the baseline, when the
    /// baseline is missing, or when rendering fails.
    #[track_caller]
    pub fn assert_snapshot<State>(harness: &mut Harness<'_, State>, name: &str) -> Outcome {
        if !enabled() {
            return Outcome::Skipped;
        }
        if let Err(error) = harness.try_snapshot_options(name, &options()) {
            panic!("{error}");
        }
        Outcome::Compared
    }

    /// Separator between a baseline's name and the scale it was rendered at.
    pub const SCALE_SEPARATOR: char = '@';

    /// How a scale factor is written into a baseline's file name.
    ///
    /// `1.0` becomes `1x`, `1.5` becomes `1_5x`, `2.0` becomes `2x`: the
    /// decimal point becomes an underscore, because a file name already uses
    /// `.` for its extension and `library@1.5x.png` reads as if `5x` were
    /// one. Trailing zeros are dropped so `1.0` and `1` cannot name two
    /// different files.
    #[must_use]
    pub fn scale_suffix(pixels_per_point: f32) -> String {
        let mut text = format!("{pixels_per_point:.2}");
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
        format!("{}x", text.replace('.', "_"))
    }

    /// The baseline name `name` takes at `pixels_per_point`.
    ///
    /// `scaled_name("library_100_rows", 1.5)` is `library_100_rows@1_5x`.
    /// The scale is part of the name and not of a directory, so the whole
    /// scale axis of one screen sorts together in `tests/visual/` and a diff
    /// shows them side by side.
    ///
    /// The point of the convention is that it is **not possible** to compare
    /// a 1.5× frame against a 1.0× baseline: the two render to different
    /// pixel dimensions, and before that they resolve to different files.
    #[must_use]
    pub fn scaled_name(name: &str, pixels_per_point: f32) -> String {
        format!("{name}{SCALE_SEPARATOR}{}", scale_suffix(pixels_per_point))
    }

    /// The baseline name the current frame of `harness` would be compared
    /// against by [`assert_snapshot_at_scale`].
    ///
    /// Derived from the harness's own `pixels_per_point`, never from an
    /// argument, so a test cannot build at one scale and name at another.
    #[must_use]
    pub fn snapshot_name<State>(harness: &Harness<'_, State>, name: &str) -> String {
        scaled_name(name, harness.ctx.pixels_per_point())
    }

    /// Compare the current frame with the committed baseline for `name` *at
    /// the scale this harness renders at* (parity row S16).
    ///
    /// Does nothing unless [`enabled`], exactly like [`assert_snapshot`].
    ///
    /// # Panics
    ///
    /// Panics when the rendered frame differs from the baseline, when the
    /// baseline is missing, or when rendering fails.
    #[track_caller]
    pub fn assert_snapshot_at_scale<State>(
        harness: &mut Harness<'_, State>,
        name: &str,
    ) -> Outcome {
        let name = snapshot_name(harness, name);
        assert_snapshot(harness, &name)
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

    #[test]
    fn the_chosen_scale_reaches_the_harness_and_the_window_stays_the_same_size() {
        for scale in SCALE_AXIS {
            let mut harness = builder_at_scale(scale).build_ui_state(
                |ui, painted: &mut egui::Vec2| {
                    *painted = ui.max_rect().size();
                    let _ = ui.button("Import audio");
                },
                egui::Vec2::ZERO,
            );
            harness.run();
            assert!(
                (harness.ctx.pixels_per_point() - scale).abs() < f32::EPSILON,
                "the harness renders at {} rather than {scale}",
                harness.ctx.pixels_per_point()
            );
            // Logical points, not pixels: the screen gets the same room at
            // every scale, which is what makes a layout difference between
            // two scales a bug rather than arithmetic.
            let painted = *harness.state();
            assert!(
                painted.x <= DEFAULT_SIZE.x && painted.x > DEFAULT_SIZE.x - 40.0,
                "at {scale}× the window is {painted:?}, not {DEFAULT_SIZE:?} points"
            );
            let button = harness.get_by_label("Import audio").rect();
            assert!(
                button.width() > 0.0 && DEFAULT_SIZE.to_pos2().x >= button.max.x,
                "at {scale}× the button reports {button:?} logical points"
            );
        }
    }

    #[test]
    fn a_baseline_name_carries_the_scale_it_was_rendered_at() {
        assert_eq!(visual::scaled_name("library", 1.0), "library@1x");
        assert_eq!(visual::scaled_name("library", 1.5), "library@1_5x");
        assert_eq!(visual::scaled_name("library", 2.0), "library@2x");
        // A fractional scale that is not on the axis still names one file.
        assert_eq!(visual::scaled_name("library", 1.25), "library@1_25x");

        // Derived from the harness, never from an argument: a 1.5× frame
        // cannot be compared against the 1.0× baseline.
        let mut at_one = builder_at_scale(1.0).build_ui(|ui| {
            ui.label("Plaudon");
        });
        at_one.run();
        let mut at_one_and_a_half = builder_at_scale(1.5).build_ui(|ui| {
            ui.label("Plaudon");
        });
        at_one_and_a_half.run();
        assert_eq!(
            visual::snapshot_name(&at_one, "shell-ready"),
            "shell-ready@1x"
        );
        assert_eq!(
            visual::snapshot_name(&at_one_and_a_half, "shell-ready"),
            "shell-ready@1_5x"
        );
    }

    #[test]
    fn an_unnamed_control_is_reported_and_a_named_one_is_not() {
        let mut harness = harness(|ui| {
            let _ = ui.button("Import audio");
            // A button whose name is empty: operable, announces nothing.
            let _ = ui.button("");
        });
        harness.run();

        let violations = a11y::unnamed_controls(&harness, &[]);
        assert_eq!(
            violations.len(),
            1,
            "exactly the empty button is a violation: {violations:?}"
        );
        assert_eq!(violations[0].role, egui::accesskit::Role::Button);
    }

    #[test]
    fn a_control_named_by_its_container_is_exempt_only_inside_that_container() {
        let waived = [a11y::NamedByContainer {
            role: egui::accesskit::Role::Button,
            container: "Sidebar width",
            why: "the self-test's stand-in for a control a group already names",
        }];

        let mut inside = harness(|ui| {
            ui.push_id("group", |ui| {
                ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
                    node.set_role(egui::accesskit::Role::Group);
                    node.set_label("Sidebar width");
                });
                let _ = ui.button("");
            });
        });
        inside.run();
        assert!(
            a11y::unnamed_controls(&inside, &waived).is_empty(),
            "the exemption applies inside its container: {:?}",
            a11y::unnamed_controls(&inside, &waived)
        );
        assert_eq!(
            a11y::unnamed_controls(&inside, &[]).len(),
            1,
            "and without the exemption the same button is a violation"
        );

        let mut outside = harness(|ui| {
            let _ = ui.button("");
        });
        outside.run();
        assert_eq!(
            a11y::unnamed_controls(&outside, &waived).len(),
            1,
            "an exemption may not be honoured outside its container"
        );
    }
}
