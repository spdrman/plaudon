# egui reference components: what Plaudon can reuse

Evaluation of the reference applications named in EPIC O (#197) for task O1.1 (#199):
`storytold/photocraft`, `storytold/filmcraft`, `storytold/wordcraft`, and — discovered during the
audit and added here because it holds the only directly reusable library code —
`storytold/craft-libs`.

Everything below was read from shallow clones taken on 2026-10-11/12. Signatures are copied from
source. Statements that could not be verified by reading are marked `[INFERENCE]`.

Companion documents: `docs/EGUI_MIGRATION_PLAN.md` (what Plaudon has and what must be built) and
`docs/EGUI_BASELINE.md` (the measured baseline of the app being replaced).

---

## 0. Executive summary

| Question | Answer |
| --- | --- |
| Can any of the three `ui-egui` crates be depended on? | **No.** All three are path crates inside their own workspaces, bound to an engine `Session` type, transitively pulling 15–17 sibling crates. photocraft's `ui-egui` alone is ~190 modules over 17 internal crate dependencies. None is published. |
| Is there a reusable egui component library? | **Yes, one**: `craft-ui` in `storytold/craft-libs`. It depends on **egui 0.36 only**, plus optional `serde`. 2,801 production lines. All three apps consume it as a git dependency pinned at `5c874d51eeb3de70e735c8ce87a59a7dedd47675`. |
| Does it cover what Plaudon needs? | **About 5–10 %.** It is docking + icon button + tab fitting + split geometry + a resize handle + an SVG catalog + a five-colour widget palette. It has no table, no virtualized list, no toast, no modal, no command palette, no editor, no timeline, no waveform, no progress bar, no file dialog, no search widget. |
| Can Plaudon take it as a git dependency? | **Not under current policy.** `deny.toml` sets `[sources] unknown-git = "deny"`. Vendoring with NOTICE propagation is the recommendation. |
| Is there a kittest snapshot precedent to copy? | **No.** None of the four repositories commits baseline PNGs or calls `Harness::snapshot`/`SnapshotOptions`. Two render to PNG only when an environment variable is set; craft-libs compares a widget against an independently painted reference on the same machine with **exact** RGBA and mesh equality. Plaudon must design its own baseline strategy. |
| Highest-value takeaways | filmcraft's **waveform painting technique** and **audio-clock playback**; craft-libs' **`ResizeHandle`** (the best accessibility asset in any of them); the **repaint-scheduling discipline** shared by all three apps; wordcraft's **command registry + keymap** shape. |

Everything is **MIT OR Apache-2.0**, which `deny.toml` already allows. No repository has per-file
SPDX headers, so vendored files lose provenance unless Plaudon adds it (§6).

All four are on **egui/eframe 0.36.2, wgpu 30.0.1, winit 0.30.13, accesskit 0.24.1, rfd 0.17.2,
egui_kittest 0.36.2, kittest 0.4.0** — the exact target of EPIC O, so there is no version friction.
All four are edition 2024; photocraft, filmcraft and craft-libs require **Rust 1.95**, wordcraft 1.90.
Plaudon is edition 2021 with `rust-version = "1.90"`, so vendored edition-2024 code (let-chains are
used throughout) needs either a crate-local edition bump or a rewrite.

---

## 1. craft-libs / `craft-ui` — the only library

Repository `storytold/craft-libs`, workspace `crates/*` with exactly two members: `craft-ui`
(`crates/ui`) and `craft-print` (a CUPS/Windows PDF spooler, irrelevant here).

```toml
[package]
name = "craft-ui"
version.workspace = true       # 0.1.0
edition.workspace = true       # 2024
license.workspace = true       # MIT OR Apache-2.0
rust-version.workspace = true  # 1.95
description = "Reusable egui controls and icon catalogs for Craft frontends"
[dependencies]
egui = { workspace = true }    # { version = "0.36", default-features = false }
serde = { version = "1", features = ["derive"], optional = true }
[features]
serde = ["dep:serde"]
```

Production dependencies really are **egui only**. `egui_kittest` (wgpu), `egui_extras` (svg),
`serde_json` and `image` (png) are dev-dependencies.

`lib.rs` opens with
`#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`
and the workspace forbids `unsafe_code`. There is no `cfg(target_os)` anywhere in the crate, no
`build.rs`, and no non-Rust build step.

### 1.1 Size

| Module | Prod LOC | Test LOC |
| --- | ---: | ---: |
| `lib.rs` | 13 | — |
| `buttons/` | 122 | 265 |
| `tabs/` | 165 | 235 |
| `layout/` (`mod` 16, `geometry` 135, `resize` 198, `toolbar` 152) | 501 | 238 |
| `docking/` (`mod` 17, `model` 792, `surface` 917, `targets` 99) | 1,825 | 1,092 |
| `icons/` | 78 | 84 |
| `theme/` | 97 | — |
| **Total `src/`** | **2,801** | 1,914 |
| `tests/docking_workflows.rs` | 252 | |
| `tests/visual_regression.rs` + `tests/visual/fixtures.rs` | 721 | |

≈ 81 `#[test]` functions in total (35 of them in `docking/tests.rs`).

### 1.2 Public API

**`buttons`** — one accessible icon button whose icon the caller paints:

```rust
pub struct ButtonVisuals { pub fill: Color32, pub stroke: Stroke, pub icon: Color32 }
pub struct IconButtonStyle {
    pub normal: ButtonVisuals, pub hovered: ButtonVisuals,
    pub pressed: Option<ButtonVisuals>, pub selected: ButtonVisuals,
    pub corner_radius: CornerRadius, pub focus_stroke: Stroke,
}
impl<'a> IconButton<'a> {
    pub fn new(label: &'a str, size: Vec2, style: &'a IconButtonStyle) -> Self;
    pub fn selected(self, selected: bool) -> Self;
    pub fn single_shape_frame(self) -> Self;
    pub fn show(self, ui: &mut Ui, paint_icon: impl FnOnce(&Ui, Rect, Color32)) -> Response;
}
```

Visual precedence is selected > pressed > hovered > normal; a non-finite, zero or >4096 size yields
an inert zero-size response rather than a panic.

**`tabs`** — a fitting algorithm and an accessible tab:

```rust
pub struct TabFit { pub shown: Vec<(usize, f32)>, pub overflow: Vec<usize>, pub overflow_width: f32 }
pub fn fit(natural: &[f32], selected: usize, available: f32, minimum_width: f32, overflow_width: f32) -> TabFit;
impl<'a> Tab<'a> {
    pub fn new(id: Id, label: &'a str, selected: bool) -> Self;
    pub fn sense(self, sense: Sense) -> Self;
    pub fn focus_stroke(self, stroke: Stroke) -> Self;
    pub fn show_at(self, ui: &mut Ui, rect: Rect, paint: impl FnOnce(&Ui, &Response)) -> Response;
}
```

`Tab` does not allocate space — the caller supplies the rect.

**`layout`** — pure geometry plus the one widget with first-class accessibility:

```rust
pub enum SplitAxis { Horizontal, Vertical }
pub enum SplitSize { Ratio(f32), FixedFirst(f32), FixedSecond(f32) }
pub struct SplitLayout { pub first: Rect, pub divider: Rect, pub second: Rect }
impl SplitLayout {
    pub fn first_size(self) -> f32;
    pub fn available(self) -> f32;
    pub fn resized(self, size: SplitSize, delta: f32, minimum: f32) -> Option<SplitSize>;
}
pub fn split_rect(rect: Rect, axis: SplitAxis, size: SplitSize, gap: f32, minimum: f32) -> Option<SplitLayout>;

pub struct ResizeHandleStyle { pub hovered: Color32, pub dragged: Color32, pub focused: Color32 }
pub struct ResizeResponse { pub response: Response, pub delta: f32 }
impl<'a> ResizeHandle<'a> {
    pub fn new(id: Id, axis: SplitAxis, rect: Rect, label: &'a str, style: ResizeHandleStyle) -> Self;
    pub fn hit_margin(self, margin: f32) -> Self;      // default 3.0
    pub fn keyboard_step(self, step: f32) -> Self;     // default 8.0
    pub fn value(self, value: f32, range: RangeInclusive<f32>) -> Self;
    pub fn show(self, ui: &mut Ui) -> ResizeResponse;
}

pub struct ToolbarFit { pub shown: Vec<(usize, f32)>, pub overflow: Vec<usize>, pub overflow_width: f32 }
pub fn fit_toolbar(natural: &[f32], available: f32, spacing: f32, overflow_width: f32) -> Option<ToolbarFit>;
```

`fit_toolbar` keeps an ordered prefix of indivisible groups, reserves the overflow width, caps at
4,096 groups, and returns `None` on invalid input or when `available < overflow_width` — the caller
must supply a scroll or wrap fallback.

**`docking`** — a validated, serde-able, transactional layout model with an optional renderer:

```rust
pub const MAX_DEPTH: usize = 64;  pub const MAX_PANELS: usize = 4096;
pub enum Zone { Center, Left, Right, Top, Bottom }
pub enum Node<P> {
    Split { axis: SplitAxis, size: SplitSize, first: Box<Node<P>>, second: Box<Node<P>> },
    Tabs { panels: Vec<P>, active: usize },
    Stack { entries: Vec<StackEntry<P>> },
}
pub struct Layout<P> { pub root: Option<Node<P>>, pub floating: Vec<Floating<P>> }
pub enum Placement<P> { Tab { before: Option<P> }, Split(Zone) }
pub enum Action<P> {
    Move { panel: P, anchor: P, placement: Placement<P> },
    Float { panel: P, rect: [f32; 4] },
    Close { panel: P }, Activate { panel: P },
    Open { panel: P, anchor: Option<P> },
    OpenAt { panel: P, anchor: P, placement: Placement<P> },
    MoveFloating { panel: P, rect: [f32; 4] },
    ResizeSplit { path: Vec<bool>, size: SplitSize },
    SetStackOpen { panel: P, open: bool },
    ResizeStack { panel: P, height: Option<f32> },
}
pub struct Permissions { pub movable: bool, pub floatable: bool, pub closable: bool, pub accepts_tabs: bool }
pub enum DockError { LimitExceeded, DuplicatePanel, MissingPanel, MissingTarget,
                     InvalidSelection, InvalidGeometry, InvalidPlacement, NotAllowed }
impl<P: Clone + Eq + Hash> Layout<P> {
    pub fn validate(&self) -> Result<(), DockError>;
    pub fn panels(&self) -> Vec<&P>;
    pub fn contains(&self, panel: &P) -> bool;
    pub fn location(&self, panel: &P) -> Result<Location<P>, DockError>;
    pub fn restore(&mut self, panel: P, location: &Location<P>) -> Result<(), DockError>;
    pub fn apply(&mut self, action: Action<P>) -> Result<(), DockError>;
    pub fn apply_with_permissions(&mut self, action: Action<P>, permissions: impl Fn(&P) -> Permissions) -> Result<(), DockError>;
}
```

The renderer takes an immutable snapshot and returns actions for the app to apply — the same
one-way-data-flow discipline Plaudon already uses:

```rust
impl<P: Clone + Eq + Hash + Debug + Send + Sync + 'static> DockArea<P> {
    pub fn new(id: Id) -> Self;
    pub fn raise_panel(self, panel: P) -> Self;
    pub fn blocked_rects(self, rects: &[Rect]) -> Self;
    pub fn chrome_has_focus(&self, ctx: &Context, layout: &Layout<P>) -> bool;
    pub fn show(self, ui: &mut Ui, layout: &Layout<P>, style: &DockStyle,
                label: impl Fn(&P) -> String, permissions: impl Fn(&P) -> Permissions,
                body: impl FnMut(&mut Ui, &P)) -> DockOutput<P>;
    pub fn show_with_limits(…, limits: impl Fn(&P) -> PanelLimits, body) -> DockOutput<P>;
    pub fn show_customized(…, content: &mut impl DockContent<P>) -> DockOutput<P>;
}
```

`Layout::apply` validates, clones, mutates, re-validates, then commits. Floating groups are
`egui::Area`s constrained to the viewport — **not** native child windows, so no multi-monitor
detach. All model types derive `Serialize`/`Deserialize` under the `serde` feature, which is how
photocraft persists `docking: Option<craft_ui::docking::Layout<String>>` in its serde UI state.

**`icons`** and **`theme`** are thin:

```rust
pub struct SvgStyle { pub current_color: &'static str, pub stroke_width: Option<&'static str> }
impl SvgIconSet {
    pub fn new(icons: &'static [(&'static str, &'static [u8])], style: SvgStyle,
               uri_prefix: &'static str, fallback: &'static str) -> Self;
    pub fn contains(&self, name: &str) -> bool;
    pub fn canonical_name(&self, name: &str) -> Option<&'static str>;
    pub fn source(&self, name: &str) -> ImageSource<'static>;
}
pub struct WidgetPalette { pub noninteractive: Color32, pub inactive: Color32,
                           pub hovered: Color32, pub active: Color32, pub open: Color32 }
impl WidgetPalette { pub fn apply_to(self, widgets: &mut Widgets, radius: CornerRadius, foreground: Stroke); }
```

`SvgIconSet` performs literal `currentColor` / `stroke-width="2"` string substitution over embedded
`&'static` bytes and needs `egui_extras::install_image_loaders` with the `svg` feature. No icon
assets are bundled. `WidgetPalette::apply_to` is about fifteen lines of logic; it sets widget fills
for five states and nothing else — **no theme switching, no spacing, no typography, no
high-contrast**. Plaudon's four-theme, twenty-role token system is entirely its own work.

### 1.3 Accessibility — the reason to look at this crate

This is the most careful accessibility work in any of the four repositories, and Plaudon's hardest
risk (ADR-0001 rejected a pure-Rust GUI on exactly this ground) is accessibility.

- `IconButton`: `response.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, selected, label))`
  → AccessKit `Role::Button` with a toggled state; an inset focus outline on `has_focus()`.
- `Tab`: `WidgetInfo::selected(WidgetType::SelectableLabel, …)` plus
  `ctx.accesskit_node_builder(id, |n| { n.set_role(Role::Tab); n.clear_toggled(); n.set_selected(selected); })`.
- `ResizeHandle`: `Role::Splitter` with orientation, `set_numeric_value`/`min`/`max`/`step`,
  the `Increment`, `Decrement` and `SetValue` actions consumed via
  `ui.input_mut(|i| i.consume_accesskit_action_requests(id, …))`, a focus-lock filter so arrow keys
  reach the handle, and `WidgetInfo::slider(enabled, value, label)`. Tested for Increment = +8,
  Decrement = −8, SetValue 250 → delta 150, NaN → 0.
- `DockArea` labels every chrome button, renders split and accordion dividers through `ResizeHandle`
  (so dividers are keyboard-resizable), and cancels drags on Escape or focus loss.

**Gaps Plaudon must fill**: there is no `TabList` parent role and no tab↔panel relationship; the tab
strip has **no arrow-key roving focus** (each tab is a separate tab stop), whereas Plaudon's
`AppNav` already implements roving tabindex and the screen-reader script requires "a single tab
stop". There is no keyboard path to move or float a panel — only pointer drag or a context menu.
There is no live region. Focus-ring contrast is the caller's responsibility.

### 1.4 Testing technique

`egui_kittest::Harness` is used **exactly once** in the entire crate (`docking/tests.rs:1043`), to
prove that popup action labels keep their logical accessibility label while painting custom text.
Every other unit test drives a bare `egui::Context`:

```rust
let ctx = egui::Context::default();
ctx.enable_accesskit();
ctx.global_style_mut(|s| s.animation_time = 0.0);
ctx.run_ui(RawInput { events, .. }, |ui| …);           // two frames to settle
// assert on the returned Response and on output.platform_output.accesskit_update
```

The visual-regression suite (`tests/visual_regression.rs` + `tests/visual/fixtures.rs`, 721 lines)
uses `egui_kittest` only for `wgpu::WgpuTestRenderer` — never `Harness`, never `snapshot`:

1. A font-free context (`set_fonts(FontDefinitions::empty())`), `animation_time = 0`, a 128×72-point
   canvas, four frames, with real pointer and focus events injected at frame 2.
2. The "reference" side paints the expected pixels with plain egui painter calls and literal
   geometry; the "actual" side builds the same control through craft-ui.
3. Both are tessellated and rendered offscreen; the comparison is **exact** — zero differing pixels
   **and** identical mesh vectors (positions, UVs, colours, indices, texture ids, clip rects).
4. 372 control pairs (5 components × 7 states × 2 themes × 3 scales × 2 densities, minus 2
   inapplicable selected states) + 18 toolbar + 12 palette + 12 SVG = **414 positive pairs**, plus
   **36 negative** pairs that deliberately mutate fill, spacing or stroke and must differ.
5. `CRAFT_UI_VISUAL_DIR` dumps before/after/difference PNGs and a `comparison.tsv`; nothing is
   written by default and **no baseline PNG is committed anywhere**.

The documentation is explicit that this does not assert cross-driver pixel identity. It is a
same-machine paired comparison, which is why it can demand exact equality.

**Implication for Plaudon**: this technique catches geometry and colour regressions in *primitive
widgets* with no golden-file maintenance, but it cannot catch text or font-layout regressions (fonts
are emptied) and it does not scale to whole screens. For screen-level regression Plaudon needs
`Harness` + `snapshot()` with committed baselines, which **no reference app demonstrates**.

### 1.5 Maintenance

The only workflow is `.github/workflows/print.yml`, path-filtered to `crates/print/**`. **No CI runs
craft-ui's tests, clippy, fmt or the wasm check** — the README lists them as manual pre-merge
commands. Version is an inherited `0.1.0`; there is no CHANGELOG and no versioning policy; there is
no evidence of a crates.io release and the consuming apps pin a git revision. The clone used here is
at `1aa81a773aa9198a0e4c0daeda954d622475af38`, which is **not** the revision photocraft and filmcraft
pin (`5c874d51…`); that commit is not in the shallow clone, so API identity with the pinned revision
is unverified.

Together these signals say: treat craft-ui as a **snapshot to vendor**, not as an upstream to track.

---

## 2. photocraft — shell, toolbar, tables, theming, screenshot tests

24-crate workspace (edition 2024, Rust 1.95), layered L0→L6 and enforced by `cargo xtask layers`.
`crates/ui-egui` is layer L6: ~190 modules, `lib.rs` alone 2,605 lines, `panels.rs` 5,601,
`prefs_ui.rs` 3,160, `widgets.rs` 2,292. It depends on craft-ui plus **17 internal crates** and is
built around `PhotocraftApp` / `Services` / `Session`. It is not extractable.

### 2.1 Toolbar

The reusable parts are craft-ui's (`IconButton` + `fit_toolbar`). photocraft's own
`panels::toolbar` (`panels.rs:81-320`) is a vertical Photoshop tool palette: an exact-width start
panel containing a `ScrollArea::vertical()` chunked into one or two columns, with a 0.35 s long-press
or right-click flyout `egui::Area` anchored beside the button, Escape and click-outside to close, and
a corner triangle indicator. Notably it **does not use `fit_toolbar`** — it uses scroll instead.
Hidden tools come from preferences.

For Plaudon a horizontal toolbar with overflow is the right shape, which is `fit_toolbar` plus an
overflow menu; the flyout and long-press behaviour is a useful pattern but not needed.

### 2.2 Docking

Two systems: a legacy Photoshop-specific `dock.rs` (803 lines, six fixed group enums) that is
unsuitable, and `panel_docking.rs` (1,010 lines), an adapter over `craft_ui::docking`. The adapter is
the valuable artefact as a *pattern*:

```rust
let mut style = craft_ui::docking::DockStyle::from_ui(ui);
style.tab_height = if t.pro { 28.0 } else { 40.0 };
style.min_pane = 0.0;
style.background = t.card;
let mut area = craft_ui::docking::DockArea::new(egui::Id::new("photocraft-panel-docking"));
if let Some(panel) = &reveal { area = area.raise_panel(panel.clone()); }
// … permissions closure, PanelLimits { min: vec2(220.0, 0.0), max: vec2(1e6, 0.0) } …
```

with `impl craft_ui::docking::DockContent<String> for PanelContent<'_>`. The cycle is: render a
snapshot → collect `DockOutput.actions` → apply each through the app's own command path (a
`window.panel.layout` command carrying the action as JSON) → persist `Layout<String>` plus a
`BTreeMap<String, Location<String>>` of hidden panels in the serde UI state. `Layout::location` and
`restore` are what let a closed panel reopen where it was.

### 2.3 Tables — the honest answer

**There is no table abstraction.** The crate contains exactly one `egui_extras::TableBuilder`, in
`credits.rs:376-390`:

```rust
let mut b = egui_extras::TableBuilder::new(ui).id_salt(id).striped(true).resizable(false)
    .auto_shrink([false, false]).min_scrolled_height(body_h).max_scroll_height(body_h)
    .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
for w in widths {
    b = b.column(match w { Some(w) => egui_extras::Column::exact(*w),
                           None => egui_extras::Column::exact(flex).clip(true) });
}
b.header(TABLE_HEADER_H, header).body(body);
```

A clickable sort header that reverses on a second click, column widths measured from the widest cell
plus a sort-arrow allowance, a fixed header with scrolling rows, and an outer horizontal
`ScrollArea`. **No row selection, no column resizing (`resizable(false)`), no virtualization**
(`body.rows(..)` is never used; the only virtualized construct anywhere is a
`ScrollArea::vertical().show_rows(..)` glyph grid). Layer and history lists are hand-painted rows.

Plaudon's 10,000-row library table (O1.4, #202) has to be written directly against
`egui_extras::TableBuilder` with `Column::auto/initial/remainder`, `resizable(true)`,
`.sense(Sense::click())` and `body.rows(height, n, …)`. photocraft contributes a sort-header pattern
and nothing else.

### 2.4 Theme and tokens

`theme.rs` (1,347 lines) is the most directly relevant file in the repository for Plaudon's design
system, as a *mechanism*:

- `struct Tokens` — 40+ `Color32`/`f32`/`bool` fields, `Copy`, with `Tokens::for_kind(ThemeKind)`
  supplying per-theme constants for 12 themes, and `Tokens::get(ctx)` reading it back out of
  `ctx.data` at `Id::new("photocraft-theme")`.
- `apply(ctx, kind)` builds `Visuals::dark()`/`light()`, overrides panel/window/extreme/faint fills,
  `override_text_color`, hyperlink, warn, error, shadows, selection and all five
  `widgets.*` states, then **copies the style into both `egui::Theme::Light` and `Theme::Dark` via
  `ctx.set_style_of` and sets `ThemePreference::System`** — so an OS appearance change does not undo
  the app's styling. That trick is exactly what Plaudon's `watchSystemAppearance` does today in the
  browser, and wordcraft does the same thing (`theme.rs:488-515`).
- Spacing is **not** a scale: values are inline in `apply()` (`item_spacing (8, 6)`,
  `button_padding (10, 4)`, `interact_size (24, 24)`, `slider_width 150`, `menu_margin 6`,
  `window_margin 16`, `icon_width 14`). Plaudon's existing nine-step scale is better and should be
  kept.
- Typography: `TextStyle` overrides with Inter Regular/Medium/SemiBold and JetBrains Mono registered
  as `FontFamily::Name("medium")` / `"semibold"`, plus an `egui::Plugin` that scales
  `FontData::tweak.scale` for four UI font-size presets, and lazy CJK fallback loading.
- Icons are **not** egui-phosphor: 118 Lucide SVGs (ISC) embedded by a generated
  `icon_data.rs` (`pub static ICONS: &[(&str, &[u8])]`) and drawn through `craft_ui::icons::SvgIconSet`
  with `SvgStyle { current_color: "#ffffff", stroke_width: Some("1.75") }`, rasterised at exact pixel
  size by `egui_extras::install_image_loaders`.
- A debug-only `LiveTokens::from_env()` hot-reloads a JSON token override file
  (`PHOTOCRAFT_THEME_FILE`), compiled out of release builds.

The 12 Photoshop-flavoured themes, the `pro`/`bevel`/`round_chips` flags and the histogram fields are
not relevant; the Solarized palette is MIT and carries its own licence file, so skip it.

### 2.5 kittest usage

Harness patterns, copied:

```rust
Harness::builder().with_size(vec2(320.0, h)).build_ui_state(|ui, app| panel(app, ui), app);
Harness::builder().with_size(vec2(1440.0, 900.0)).with_max_steps(64).with_step_dt(step_dt)
    .build_eframe(move |cc| { PhotocraftApp::setup_context(&cc.egui_ctx, Default::default()); … });
egui_kittest::Harness::builder().with_size(vec2(w, h)).with_pixels_per_point(scale)
    .with_max_steps(64).wgpu().build_eframe(..);
builder.wgpu_setup(photocraft_ui_egui::gpu_canvas::wgpu_setup()).build_eframe(..);
```

Helpers used: `run_steps(n)`, `step()`, `event(egui::Event::…)`, `state()`/`state_mut()`, `ctx`,
`input_mut()`, `output()`, `kittest::Queryable` (`get_by_label(..).click()`, `query_all_by_label`,
`query_all_by_role(Role::CheckBox)`, `.rect()`), and `render() -> Result<RgbaImage>`.
`with_step_dt(1.0 / 60.0)` is **required** for double-click timing.

**Despite enabling the `snapshot` feature, a repository-wide grep for `.snapshot(`, `try_snapshot`,
`SnapshotOptions`, `UPDATE_SNAPSHOTS` and `snapshots/` returns zero hits.** Tests assert on state,
widget rects and AccessKit labels. Screenshots are optional evidence written only when
`CRAFT_UI_VISUAL_FIXTURES`, `CRAFT_UI_DOCKING_FIXTURES` or similar environment variables are set, via
`h.render().unwrap().save(path)`. An `examples/snapshot.rs` CLI (172 lines) renders any UI state
offscreen — a ready-made model for a MARS evidence step:

```
cargo run -p photocraft-ui-egui --example snapshot -- --out ui.png --size 1440x900 --scale 2 \
  --script '[["ui.set", { … }]]'
```

One operational detail worth copying verbatim: `crates/testkit/src/gpu.rs` exposes
`pub fn gpu_lock() -> MutexGuard<'static, ()>`, held at the start of every test that creates a GPU
harness, because WARP on Windows and Mesa crash when parallel wgpu devices are created. Their
`.config/nextest.toml` additionally caps the GPU test group at four threads. Plaudon's MARS runner
will hit the same problem.

### 2.6 Other patterns worth copying

- **Toasts** (`notices.rs`, 467 lines): an `egui::Area` at `Order::Foreground` anchored
  `Align2::RIGHT_BOTTOM` with `vec2(-16.0, -36.0)`, one `egui::Frame` card per notice, max 3 notices
  and 8 lines, a dismiss ×, and an auto-hide that pauses while the stack is hovered, driven by
  `ctx.request_repaint_after(duration)`. About 120 lines to rewrite against Plaudon's toast list.
- **File dialogs** (`file_dialog.rs`, 313 lines + `apps/photocraft/src/services.rs:91-144`): a
  request enum, an `rfd::AsyncFileDialog` with `set_parent(frame)` on a background task, a reply sent
  over `std::sync::mpsc` followed by `ctx.request_repaint()`, and a per-frame poll. The `ui-egui`
  crate stays rfd-free because the dialog arrives as a `Services` hook — the pattern Plaudon should
  copy so `plaudon-ui` stays hermetic for kittest.
- **Async plumbing**: there is **no tokio anywhere**.
  `std::thread::Builder::new().name(..).spawn(move || { …; tx.send(result); repaint.request_repaint(); })`
  with a cloned `egui::Context`, `mpsc`/`sync_channel(1)`, `try_recv` each frame, and
  `request_repaint_after(16..100 ms)` while a job runs. Plaudon has tokio, but the shape is identical.
- **Jobs UI** (`jobs_ui.rs`, 772 lines): `DIALOG_DELAY_MS = 250.0` before a modal progress dialog
  appears, a `SaveCtl { progress, check, commit }` handle, `tick(app, ctx)` polled per frame, and
  `bar(ui, rect, frac: Option<f32>, t)` — a 40-line painter that handles the indeterminate case.
- **State persistence**: eframe's `persistence` feature with
  `NativeOptions { persistence_path: config_dir().map(|d| d.join("ui.ron")), persist_window: true }`,
  **plus a `ui_state::sanitize` pass that drops values which abort startup** (zoom factor 0, NaN,
  1e30). That failure mode is real and Plaudon should reproduce the guard.

---

## 3. filmcraft — timeline, waveform, playback, repaint scheduling

45 crates, ~343k lines, edition 2024, Rust 1.95; a Premiere-style editor. `crates/ui-egui` depends on
craft-ui plus 15 sibling crates; every panel is a free function
`fn x(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect)` reading `app.tokens`, `app.session.*`,
`app.tl`, `app.ui.*`. There is no crate boundary to depend on. Notably it uses **no `egui_extras`,
no `egui_plot`, no `egui_dock`** — timeline, waveform, dock chrome, icons and toasts are all
hand-painted with `egui::Painter`/`Mesh`.

This is the highest-value reference for Plaudon, because P1, P9, P10 and P11 in the parity matrix are
exactly its subject matter.

### 3.1 Waveform — the technique to copy

**Data type**: `Arc<Vec<(f32, f32)>>` — one `(min, max)` pair per 256 input samples at a fixed
48,000 Hz. That is 187.5 buckets per second, 8 bytes each: ≈ 1.5 KB/s, ≈ 5.4 MB per hour of mono.
Only channel 0 is used, and the fold starts at `(0, 0)` so `min ≤ 0 ≤ max`.

```rust
fn source_peaks(src: &dyn MediaSource, stream: usize, dur: Tick) -> Vec<(f32, f32)> {
    let sr = 48_000u32;
    let total = dur.to_units_floor(sr as i64).max(0) as usize;
    let mut out = Vec::with_capacity(total / 256 + 1);
    let chunk = 48_000 * 4;
    let mut s = 0usize;
    while s < total {
        let n = chunk.min(total - s);
        match src.audio_stream(stream, s as i64, n, sr) {
            Ok(buf) => {
                let ch = buf.channels.first().cloned().unwrap_or_default();
                for c in ch.chunks(256) {
                    let (lo, hi) = c.iter().fold((0f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)));
                    out.push((lo, hi));
                }
            }
            Err(_) => break,
        }
        s += n;
    }
    out
}
```

**Painting**: one `egui::Mesh` per clip built from `Mesh::add_colored_rect` per one-pixel column —
no lines, no texture, no `Shape::Path`:

```rust
let mut mesh = egui::Mesh::default();
for x in (clip.min.x.floor() as i32)..(clip.max.x.ceil() as i32) {
    // map the pixel column to a bucket range [s0, s1)
    let mut m = 0f32;
    for (a, b) in peaks.iter().skip(s0).take(s1 - s0) { m = m.max(a.abs()).max(b.abs()); }
    let h = if dynamic {
        let db = 20.0 * (m * gain).max(1e-5).log10();
        ((db + 48.0) / 48.0).clamp(0.0, 1.0) * area.height()
    } else { (m * gain).clamp(0.0, 1.0) * area.height() };
    if h > 0.3 {
        mesh.add_colored_rect(Rect::from_min_max(pos2(x as f32, area.max.y - h),
                                                 pos2(x as f32 + 1.0, area.max.y)), col);
    }
}
p.add(mesh);
```

The timeline style is a rectified bar graph in the lower 42 % of the clip. The source-monitor
waveform (`monitor_view.rs:604-670`) is the symmetric min/max variant — closest to what Plaudon's
player needs: `half = area.height() * 0.45`, bar from `mid - hi*half` to `mid - lo*half + 1.0`,
skipped when `hi - lo <= 0.002`, with a `Color32::from_white_alpha(30)` centre line, an in/out
shading rect at `from_white_alpha(14)` and a one-pixel playhead.

**Caching**: `peaks: Arc<Mutex<HashMap<PeakKey, Arc<Vec<(f32, f32)>>>>>` plus a pending set;
`request_peaks` returns the cached `Arc` or spawns a `std::thread` and returns `None`. Sequence
mixes are keyed by a content hash so peaks rebuild when the audio changes, and the old peaks stay on
screen until the new ones arrive. A `peak_max` map caches each list's maximum keyed by
`Arc::as_ptr` to avoid rescanning per frame — a fix their performance notes record explicitly.

**Three defects Plaudon must not inherit** (all observed in the source): the source-peaks worker has
no `catch_unwind` (the sequence worker does); it never calls `request_repaint()` on completion, so an
idle app may not show a finished waveform until something else repaints (only the source-monitor
polls, via `request_repaint_after(100 ms)`); and threads are spawned per item with no bound.

**Missing for Plaudon**: there are **no LOD/mip levels**. A single 5.33 ms resolution is
re-aggregated per pixel column every frame. At their maximum zoom of 24,000 px/s a bucket spans ~128
pixels and the display is blocky. Plaudon's O1.5 requirement is explicitly "multiresolution peaks",
so a mip pyramid (e.g. 64/256/1024/4096 samples) and sample-level drawing when fully zoomed in are
new work. Likewise there is no on-disk peak cache, no progressive display and no cancellation.

Their performance note: "the whole app frame costs ~3 ms with 1000 clips visible; it already culls
clips outside the view."

### 3.2 Playback

The audio crate is **cpal 0.16**, not rodio; decoding is their own codecs plus **symphonia 0.5.5**;
the mixer lives in the engine. The UI crate itself has no audio dependency — it consumes a trait:

```rust
pub trait AudioOut {
    fn start(&mut self, fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String>;
    fn stop(&mut self);
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> usize { 2 }
    fn played_frames(&self) -> Option<u64>;     // the master clock
    fn recover(&mut self) {}
    fn devices(&self) -> AudioDevices { AudioDevices::default() }
    fn configure(&mut self, hw: &AudioHardwarePrefs, document_rate: Option<u32>) {}
    fn note(&self) -> Option<String> { None }
}
```

**`play_ahead.rs`** (≈175 lines, `std` only apart from `serde_json` for stats) is the single best
vendoring candidate in any of the four repositories:

```rust
pub const LEAD_S: f64 = 0.2;
pub const BLOCK_FRAMES: usize = 1024;
pub struct AudioStats { /* callbacks, underruns, missing_frames, min_lead_frames: atomics */ }
pub fn spawn<M>(mix: M, start: i64, sample_rate: u32, channels: usize, stats: Arc<AudioStats>)
    -> Box<dyn FnMut(&mut [f32], usize) + Send>
where M: FnMut(i64, &mut [f32], usize) + Send + 'static;
```

A single-producer/single-consumer ring of `AtomicU32` slots holding f32 bits — **no `unsafe`**. A
named mixing thread keeps `LEAD_S` ahead of the device, sleeping 2 ms when full; the device callback
only pops, never decodes, allocates or locks. On underrun it writes silence for the missing frames,
counts them, and makes the mixer skip the same number so audio stays locked to picture. A mixer
panic is caught and becomes silence. The thread stops when the callback is dropped. Six tests.

`CpalOut` (`apps/filmcraft/src/audio.rs`, 580 lines) is the device layer: `build_output_stream` for
F32 plus converting paths for i8…f64 (clamping to `[-1, 1-ε]`, NaN → 0), a `try_lock` on the shared
mixer in the callback (`WouldBlock` → silence), `catch_unwind`, an `AtomicU64` played-frame counter,
a `Health` record fed by cpal's error callback, and a watchdog that restarts the stream if an error
was reported and no samples were consumed for 300 ms — at most 3 restarts in 10 s, keeping the same
mixer and clock. Linux device fallback is ranked `pipewire < pulse < other < hw:`.

**Playhead synchronisation** (`advance_playback`, ≈90 lines): every UI frame it calls
`audio.recover()`, reads `played_frames()` and the sample rate, computes
`elapsed = frames / sample_rate` (or wall-clock time when the audio clock is unavailable), and sets
`t = anchor_tick + Tick::from_seconds_f64(elapsed * speed)`. If the counter does not change for
`AUDIO_STALL_S = 1.0` it hands the clock to the wall clock, continues from `played * speed`, stops
the device and shows a status message. Because their `Tick` is an integer at 254,016,000,000 per
second — divisible by 24, 25, 30, 44,100 and 48,000 — frame boundaries and sample positions are
exact and there is no f64 accumulation. Plaudon's O1.5 requirement for an "integer-time timeline
model" is the same idea; milliseconds (matching `domain::SourceTimeMs`) are the natural unit, with
sample-accurate seeks computed from the sample rate.

Speed control is pitch-preserving over 0.5–4× via two `PitchShifter` stages plus a `StreamResampler`
from an unpublished `filmcraft_audio_dsp` crate — so Plaudon's 0.5–2× speed requirement needs its own
solution (rubato, signalsmith-stretch or similar). Scrubbing audio uses 80 ms grains with 4 ms fades
and keeps one output stream open for 250 ms after release.

### 3.3 Timeline

Time↔pixel mapping is two f64 values, `scroll` (left-edge seconds) and `pps` (points per second):

```rust
pub struct Layout { pub content: Rect, pub ruler: Rect, pub rows: Vec<Row>,
                    pub pps: f64, pub scroll: f64, pub split_y: f32 }
// x_of(t)   = content.min.x + ((t.seconds() - scroll) * pps) as f32
// tick_at(x) = Tick::from_seconds_f64(scroll + (x - content.min.x) / pps)
pub fn timeline_extent(sequence_seconds: f64) -> f64;
pub fn max_scroll(sequence_seconds: f64, width: f32, pps: f64) -> f64;
pub fn scroll_to_reveal(ph: f64, scroll: f64, pps: f64, width: f64) -> Option<f64>;
pub fn zoom_about(v: &mut TimelineView, factor: f64, anchor_secs: f64, width: f32);
```

`zoom_about` clamps `pps` to `0.05..24_000.0` and keeps the anchor time under the same x. Animation
is critically damped with `k = 1.0 - (-dt * 20.0).exp()` and `dt = stable_dt.min(0.05)`, and
`ctx.request_repaint()` is called **only while** `|pps - target| > 1e-6` — so zoom animation costs
frames only while moving. Wheel scrolls tracks, `Cmd`+wheel scrolls horizontally, `Option`+wheel or
pinch zooms (`1 + delta.y * 0.01`, clamped 0.5..2.0). Auto-follow centres the playhead once it passes
50 % of the view.

Ruler ticks (`timeline.rs:1465-1500`): candidate steps in frames
`[1, 2, 5, 10, base/2, base, base*2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600]`; the label step is
the first whose pixel width is ≥ 110, the minor step the first ≥ 12; 4 px minor and 10 px major
ticks. Plaudon needs the same algorithm over seconds rather than frames.

Snapping: candidates are every item edge (excluding the dragged one), the playhead, markers and the
in/out points; nearest by **screen-pixel** distance with `SNAP_PX = 9.0`, drawing a one-pixel
indicator. Segment blocks (`timeline_captions.rs`) are `Rect::from_min_max(pos2(x_of(s), row.min.y+2),
pos2(x_of(e).max(x_of(s)+1.0), row.max.y-2))` at `ROW_H = 28.0`, with live drag deltas parked in
`ctx.data` as a temp value — the closest analogue to Plaudon's transcript segments.

Every gesture ends in exactly one undoable engine command, and Escape aborts a drag. That discipline
matches Plaudon's "UI emits commands" rule.

### 3.4 Repaint scheduling — the discipline to adopt

eframe's default reactive mode, with no continuous repaint at idle:

| Situation | Call |
| --- | --- |
| While playing | unconditional `ctx.request_repaint()` at the end of `advance_playback` — one repaint per vsync |
| Zoom/scroll animation | `request_repaint()` only while the value differs from its target by more than 1e-6 |
| Progress panel while a job runs | `request_repaint_after(250 ms)` |
| Running export/render job | `150 ms` |
| Autosave dirty / status message | `1 s` |
| Native dialog pending, TTS pending, waveform generating | `100 ms` |
| Frame queue non-empty | `16 ms` |
| Background worker completion | the worker holds a cloned `egui::Context` and calls `request_repaint()` |

The frame server stores `Box<dyn Fn() + Send + Sync>` = `move || ctx.request_repaint()` and each
worker calls it on completion; the TCP control server holds a `Context` clone; channels are drained
with `try_recv` at frame start. This is precisely the policy EPIC O states ("repaint only on events
or animation") and Plaudon should adopt the table above wholesale.

### 3.5 Other pieces

`crash.rs` (≈95 lines: `install(log_dir)`, `record(msg)`, `take_last()`, `log_path()`) is a trivially
copyable panic hook — though Plaudon already has `diagnostics::install_crash_hook`.
`export::Progress` (≈60 lines: `done`/`total`/`cancel`/`finished` atomics, a status `Mutex<String>`,
`fraction()`, `eta()`) is a clean shape for the granular job progress Plaudon lacks.
`native_dialogs.rs` (181 lines) shows an enum-plus-continuation dialog API with a 100 ms poll — and
the Linux note that an XDG portal dialog must run off-thread or GNOME force-quits the app after ~5 s.
The shortcut system is worth reading for two rules: bindings sorted most-modifiers-first (because
egui ignores extra Shift/Alt), and a `lets_text_field_keep` guard so unmodified keys reach text
fields for IME.

Their dock chrome (`dock.rs`/`dock_ui.rs`, 1,471 lines) layers over craft-ui and adds
`WorkspacePrefs` — a `workspaces.json` written tmp-file-then-rename with a 4 MiB read cap, keeping
each workspace's layout as a raw `serde_json::Value` so one unreadable entry does not lose the
others. That robustness pattern is ~40 lines and worth copying for Plaudon's layout persistence.

### 3.6 kittest usage

76+ integration test files. Harness:
`Harness::builder().with_size(vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app)`.
`.wgpu()` and `.with_pixels_per_point(1.0)` are added **only** when `FILMCRAFT_UI_SNAPSHOT_DIR` or
`FILMCRAFT_UI_SHOTS` is set, and then `harness.render()` just saves a PNG. Grepping for
`assert_snapshot`, `SnapshotOptions`, `try_snapshot` and `failed_pixel_count` returns **zero matches**.

Locators are mostly **not** AccessKit queries: they drive the app's own `automation::Registry`
(`add(id, rect, label)`, `find(id)`, `query(prefix)`) through a JSON control channel with methods
`ui.click`, `ui.drag`, `ui.key`, `ui.set`, `ui.elements`, `engine.execute`, `ui.menu.invoke`, using a
`Driver::frames(n)` that calls `eframe::App::raw_input_hook` then `harness.step()`. A few tests do use
`h.get_by_label("OK").click()`, `h.query_by_label`, `click_secondary` and
`.accesskit_node().toggled()/role()`.

Their own documentation lists what is **not** coverable headless: screenshots through the wgpu monitor
path, audio output, and wall-clock playback (kittest frames do not advance real time) — audio is
covered with a fake `AudioOut`. Plaudon should plan the same: a fake audio output in kittest, with
real playback verified manually or on MARS.

Their Windows ARM64 workflow explicitly skips native `cargo test` because "PdfCraft saw an
intermittent access violation in egui UI tests there" — a warning for Plaudon's Windows CI.

---

## 4. wordcraft — editor, undo/redo, layout, search, command palette

Version 0.5.0, edition 2024, MSRV 1.90 (the only one matching Plaudon's current MSRV), alpha.
`crates/ui-egui` is ~33.8k lines over 50+ files and depends on seven sibling crates.

### 4.1 The editor is not an egui editor

This is the headline finding. The document is laid out by their own engine (`wordcraft-layout`,
HarfRust shaping), **pages are CPU-rasterised by `vello_cpu` into one egui texture per page**, and
egui only draws the caret, selection, rulers and overlays on top and routes events into a JSON
command registry:

```rust
let img = wordcraft_render::render_page(&app.session.doc, page, scale_px, &opts);
let ci = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.to_straight());
ui.ctx().load_texture(format!("page{i}"), ci, egui::TextureOptions::LINEAR)
```

The buffer model is per paragraph a `String` plus a run table — **no rope, no piece table**
(the only piece table in the repository is in the legacy `.doc` reader):

```rust
pub struct Run { pub len: usize, pub props: CharProps }
pub struct Paragraph { pub text: String, pub runs: Vec<Run>, pub props: ParaProps,
                       pub mark: CharProps, pub objects: Vec<InlineObject>,
                       pub section: Option<Box<SectionProps>>, pub rev: u64 }
pub enum Block { Para(Paragraph), Table(Table) }
pub type Blocks = Vec<Arc<Block>>;
pub struct Pos { pub story: StoryRef, pub path: Path, pub off: usize }  // byte offset
pub struct Selection { pub anchor: Pos, pub focus: Pos }
```

There is **no multi-cursor** — the closest thing is a rectangular `ColumnBlock` on Alt+drag.
Virtualization is page-level only (`if !band.intersects(ui.clip_rect().expand(200.0)) { continue; }`,
with at most two page re-renders per frame). IME is handled by hand, publishing
`egui::output::IMEOutput` each frame and painting the preedit underlined at the caret. The canvas
takes focus on click and sets `set_focus_lock_filter(id, EventFilter { tab, horizontal_arrows,
vertical_arrows, escape: true })`.

**For Plaudon this is unsuitable as code.** A transcript editor is a virtualized list of rows, each
with a speaker chip, a timestamp chip and an editable text field — a page raster cannot express that.
Plaudon's egui design is a `ScrollArea::show_rows`/`show_viewport` list of per-segment `TextEdit`s,
and wordcraft provides no code for that. What transfers is the vocabulary: a command id per edit,
run-length properties, `Arc` block sharing, the 530 ms caret blink driven by `request_repaint_after`,
the `IMEOutput` publish, and the focus-lock filter.

### 4.2 Undo/redo — whole-document snapshots

```rust
struct Undo { label: String, doc: Document, sel: Selection, pending: Option<CharProps> }
const MAX_UNDO: usize = 500;

pub fn undo(&mut self) -> bool {
    self.typing_open = false;
    let Some(u) = self.history.pop().map(Arc::unwrap_or_clone) else { return false };
    let cur = Undo { label: u.label.clone(),
                     doc: std::mem::replace(&mut self.doc, u.doc),
                     sel: std::mem::replace(&mut self.sel, u.sel),
                     pending: std::mem::replace(&mut self.pending, u.pending) };
    self.redo.push(Arc::new(cur));
    self.touch();
    true
}
```

Snapshot per step, not command inverses — cheap only because `Document.body: Vec<Arc<Block>>` shares
unchanged blocks. Coalescing: `text.insert` is labelled "Typing" and `checkpoint("Typing")` returns
early while `typing_open`; the group closes on any other mutating command, any caret command, any
selection change, and on undo/redo. One `Session::run` call is one undo step including nested
commands, with the whole history and redo vectors cloned before and restored on error; a panic inside
a command is caught and the entire state restored.

This maps cleanly onto Plaudon: a transcript as `Vec<Arc<Segment>>` gives the same cheap snapshot,
and Plaudon's existing `transcriptEditing.ts` already implements session coalescing with 12 tests
that can be ported directly. Plaudon's model is simpler and better suited; wordcraft is confirmation,
not code.

One integration detail worth copying: `keys::global_shortcuts` skips command dispatch whenever
`ctx.egui_wants_keyboard_input()` is true, so egui's own `TextEdit` undo keeps working inside fields.

### 4.3 Command palette — do not copy the UI

The registry is good:

```rust
pub struct CommandSpec {
    pub id: &'static str, pub label: &'static str, pub location: &'static str,
    pub shortcut: &'static str,     // "Mod+Shift+>", alternates separated by " / "
    pub params: &'static str, pub mutates: bool,
    pub enabled: fn(&Session) -> Option<&'static str>,
    pub run: fn(&mut Session, &Value) -> CmdResult,
}
pub struct Registry { /* specs, by_id */ }
impl Registry {
    pub fn new(specs: Vec<CommandSpec>) -> Self;
    pub fn get(&self, id: &str) -> Option<&CommandSpec>;
    pub fn all(&self) -> &[CommandSpec];
    pub fn describe(&self) -> Value;
    pub fn by_shortcut(&self, key: &str) -> Option<&CommandSpec>;
}
```

422 commands, and the same registry serves the GUI, the CLI, a JSON control channel and an MCP
server. The keymap layer is equally reusable:

```rust
pub struct KeyMap { assigned: BTreeMap<String, String>, removed: BTreeSet<String> }
// portable strings "Mod+Shift+K", "Alt+F7", "F2"; Mod = Ctrl or Cmd
pub fn normalize(key: &str) -> Option<String>;      // canonical order Mod+Ctrl+Alt+Shift+
impl KeyMap {
    pub fn resolve<'a>(&self, reg: &'a Registry, key: &str) -> Option<&'a CommandSpec>;
    pub fn keys_for(&self, reg: &Registry, id: &str) -> Vec<String>;
    pub fn assign(&mut self, reg: &Registry, id: &str, key: &str) -> Result<Option<String>, String>;
    pub fn remove(&mut self, key: &str);
    pub fn reset(&mut self);
    pub fn retain_known(&mut self, reg: &Registry);
}   // MAX_ENTRIES 2000, MAX_KEY_LEN 48
```

The palette UI, however, is 27 lines of modal `egui::Window` with a `TextEdit::singleline` and **plain
`contains()` substring matching truncated to 14 hits**:

```rust
let mut hits: Vec<&CommandSpec> = reg.all().iter().filter(|c| !q.is_empty() && (
    c.label.to_lowercase().contains(&q) || c.id.to_lowercase().contains(&q)
    || c.location.to_lowercase().contains(&q) || …)).collect();
hits.truncate(14);
```

No ranking, no arrow-key navigation, no Enter to run, no recency, no fuzzy matching anywhere in the
repository, and disabled commands are still listed. Plaudon needs a real palette (O1.3, #201): take
the registry and keymap shape, add a fuzzy matcher, list navigation and an `egui::Area` overlay.

### 4.4 Search

The Find/Replace dialog builds a JSON options blob and runs a command; the Navigation pane does
incremental search on every keystroke and renders results as `egui::text::LayoutJob` with
`TextFormat { background: Color32::from_rgb(0xFF, 0xF1, 0x76), color: BLACK, … }` — capped at 500
rows. The pattern compiler (`find_pattern.rs`, 417 lines, `regex` only) translates Word wildcards and
`^` codes into a `regex_automata::meta::Regex` with a 1 MiB NFA size limit, so there is no
catastrophic backtracking. Matches cap at 100,000; Replace All applies matches last-first so earlier
positions stay valid and the whole operation is one undo step. In-canvas highlighting is
`painter.rect_filled` over `layout.selection_rects(..)`, cached per document revision, capped at
5,000 marks.

For Plaudon, the `LayoutJob` highlight technique and the last-first replace ordering transfer
directly; the compiler is over-specified (Plaudon needs literal/regex/case/whole-word only) and the
search driver is document-structure coupled.

### 4.5 Genuinely liftable leaf modules

Three files in wordcraft depend on nothing but egui (+ serde) and are close to drop-in:

| File | Lines | What |
| --- | ---: | --- |
| `scroll.rs` | 332 | `Momentum` and `CanvasScroll`: wheel/touchpad easing and kinetic scrolling, `notch_px(zoom)`. Useful if Plaudon wants consistent kinetic scrolling in the transcript and waveform views. |
| `window_geometry.rs` | 213 | `WindowGeometry::{track, sanitized, apply, rescue_position}` — size, position, maximised and monitor persistence with sanitising and an off-screen rescue. **This is exactly Plaudon's S12 requirement**, and it is what the missing Tauri window-state plugin would have provided. Two lines reference their `UiState` and must be dropped. |
| `mini_toolbar::anchor_rect` | 12 | pure geometry for anchoring a floating toolbar to a selection. |

Their theme module repeats photocraft's trick of installing both palettes and keeping
`ThemePreference::System`, adds `InterfaceSize(u16)` (80–200 %, presets 80/90/100/110/125/150/175/200)
applied via `ctx.set_zoom_factor` with `zoom_with_keyboard` disabled, and a lenient
`UiState::from_json` that drops individually malformed fields instead of losing the whole file.
`options::lenient<T>` is the same idea for settings. Their autosave policy is 15 lines: write only a
file the user has already saved this session, more than 2.5 s after the last change and more than
1.5 s after the last caret activity, and disable autosave with a status message on failure.

### 4.6 kittest usage

```rust
let mut h = egui_kittest::Harness::builder()
    .with_size(egui::vec2(1440.0, 900.0))
    .build_ui_state(|ui, app: &mut WordApp| { let ctx = ui.ctx().clone(); app.logic(&ctx); app.ui(ui); }, app);
h.run_steps(4);
h.event(egui::Event::PointerMoved(p));
h.event(egui::Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Default::default() });
use egui_kittest::kittest::Queryable;
h.query(By::new().role(egui::accesskit::Role::Button).label_contains(expected)).is_some();
```

Assertions are on model state and AccessKit roles and labels. Again: **no `assert_snapshot`,
no `SnapshotOptions`, no tolerance, no committed baselines** anywhere in crates, apps, xtask or
workflows. `Harness::render()` appears only in an `examples/ui_shot.rs` that writes PNGs for humans
and agents to read. One practical note: an app with custom fonts needs an extra step before queries,
because the first frame installs fonts.

A pseudo-locale i18n audit that **fails the build on untranslated visible text** is a neat idea worth
remembering if Plaudon ever localises (it has no i18n today).

---

## 5. Classification

### 5.1 Directly reusable — vendor and use essentially as-is

| Item | Source | Size | Why / effort |
| --- | --- | ---: | --- |
| `craft_ui::layout::{SplitAxis, SplitSize, SplitLayout, split_rect}` | craft-libs | 135 | Pure, finite-checked split geometry with explicit ratio/fixed sizing and clamping. ~1 h to vendor. |
| `craft_ui::layout::ResizeHandle` | craft-libs | 198 | The best accessibility asset found: `Role::Splitter`, numeric value/min/max/step, `Increment`/`Decrement`/`SetValue`, arrow keys with focus lock, per-frame delta. Directly answers part of the ADR-0001 accessibility objection. ~2 h. |
| `craft_ui::layout::fit_toolbar` | craft-libs | 74 + 78 tests | Pure overflow-fitting for S6. Returns `None` on invalid input, so a fallback is required. ~1 h. |
| `craft_ui::buttons::IconButton` | craft-libs | 122 | Accessible icon button with selected state; the caller paints the icon. Partly overlaps egui's own `Button`; adopt only if Plaudon wants token-driven icon buttons. ~1 h, or reimplement in less. |
| `filmcraft::play_ahead` | filmcraft | ~175 + 6 tests | SPSC ring buffer and mixing thread with underrun accounting, no `unsafe`, `std`-only. The hardest part of Rust audio playback, already solved and tested. ~2 h to vendor (drop the `serde_json` stats export if unwanted). |
| `wordcraft::window_geometry` | wordcraft | 213 | Window size/position/maximised persistence with sanitising and off-screen rescue — S12 exactly. ~1 h (drop two `UiState` references). |

Everything in this table is MIT OR Apache-2.0 and must carry the provenance header described in §6.

### 5.2 Adaptable — copy and modify, with a stated effort basis

| Item | Source | Basis | Effort |
| --- | --- | --- | --- |
| `craft_ui::docking` (model + targets + `DockArea`) | craft-libs | 1,825 prod lines, but **cannot be separated from `layout` and `tabs`** (`model.rs` imports `SplitAxis`/`SplitSize`; `surface.rs` imports `ResizeHandle`, `split_rect` and `tabs`). Minimum viable vendoring is 2,339–2,491 lines. 35 unit tests + a workflow test. English labels and a 12 pt font are hard-coded in `DockStyle`; floating groups are `Area`s, not native windows; no keyboard panel move; no `TabList` role. | ~1 day to vendor, rename and de-workspace. **Only if** O1.3 really needs user-arrangeable panels; a fixed `SidePanel`/`CentralPanel` layout with `ResizeHandle` dividers may be enough. |
| `craft_ui::tabs` | craft-libs | 165 lines; `fit` is a well-tested pure algorithm; `Tab` has `Role::Tab` but no tab-list parent and no arrow-key roving focus, which Plaudon's screen-reader script requires. | ~0.5 day including the roving-focus layer. |
| `craft_ui::icons::SvgIconSet` | craft-libs | 78 lines; literal string substitution over `&'static` SVG bytes; needs `egui_extras` with `svg`. No assets bundled. Plaudon has no icon set at all today (it uses text glyphs), so an icon decision is needed first. | ~2 h once an icon set is chosen (Lucide, ISC, is what photocraft uses). |
| filmcraft `CpalOut` | filmcraft | 580 lines; the generic core (render, `Health`, watchdog, `convert_samples`, device fallback ranking, buffer-size negotiation) is ~450 lines; three engine types and the i18n macro must be replaced (~30 lines). | 1–1.5 days including testing on macOS and Windows. |
| filmcraft `AudioOut` + the playback clock | filmcraft | ~90 lines of clock logic inside `FilmcraftApp`; extract into a `PlaybackClock { anchor_time, anchor_tick, audio_seen }` with the 1 s stall fallback and port the three clock tests. | 0.5–1 day. |
| filmcraft timeline coordinate maths | filmcraft | ~90 lines of pure functions (`x_of`, `tick_at`, `zoom_about`, `max_scroll`, `scroll_to_reveal`, the `k = 1 - e^{-20·dt}` easing) tied only to their `Tick` type. | 0.5 day including unit tests. |
| filmcraft ruler tick selection | filmcraft | ~45 lines; frame/timecode oriented, needs a seconds variant (1, 2, 5, 10, 15, 30, 60, 300, …) with the same ≥110 px label and ≥12 px minor thresholds. | 0.5 day. |
| filmcraft snapping (`snap_to`) | filmcraft | ~35 lines; nearest candidate by screen-pixel distance, `SNAP_PX = 9.0`. | ~2 h. |
| photocraft `notices.rs` toast stack | photocraft | 467 lines, of which ~100 are egui-only rendering (`Area` + `Frame` cards + hover-paused auto-hide via `request_repaint_after`); the rest is `PhotocraftApp`/`tl!`/prefs coupling. | ~120–150 lines rewritten; 0.5 day. |
| photocraft file-dialog plumbing | photocraft | 313 lines + 50 in the host; the pattern is ~60 lines (request enum → `rfd::AsyncFileDialog` on a task → reply over a channel → `request_repaint`). | 0.5 day. |
| photocraft theme mechanism | photocraft | 1,347 lines; the portable part is the `Tokens`-in-`ctx.data` pattern, `apply()` mapping tokens to `Visuals`/`Style`, installing **both** egui themes, named font weights and the font-size plugin. Plaudon already owns its token *values* (20 roles × 4 themes). | ~300–400 new lines reusing ~150 lines of structure; 1 day. |
| wordcraft command registry + `KeyMap` | wordcraft | ~375 lines over two files, `serde`/`serde_json` only; coupled to their `Session` through `fn(&mut Session, &Value)` — make it generic over the app state. `by_shortcut` is an O(n) scan. | 0.5–1 day. |
| wordcraft `scroll.rs` | wordcraft | 332 lines, egui types only. | 1–2 h, only if kinetic scrolling is wanted. |
| filmcraft `WorkspacePrefs` persistence | filmcraft | ~40 lines: bounded read (4 MiB cap), atomic tmp-plus-rename write, per-entry `serde_json::Value` so one bad entry does not lose the rest. | ~2 h; pairs with `window_geometry`. |
| filmcraft `export::Progress` | filmcraft | ~60 lines of atomics with `fraction()` and `eta()`. Fills the J10 gap. | ~2 h. |

### 5.3 Architectural reference — read and reimplement

| Item | Source | What to take |
| --- | --- | --- |
| **Waveform peaks and painting** | filmcraft | `Arc<Vec<(f32, f32)>>` min/max buckets, per-pixel-column fold, one `egui::Mesh` of 1 px coloured rects. **Must add**: mip levels (filmcraft has one resolution), `request_repaint()` on worker completion (missing), `catch_unwind` in the worker (missing), a bounded worker pool (unbounded today), a disk cache (absent), progressive display and cancellation (absent). New implementation ≈ 250–350 lines + tests, 1.5–2 days. |
| **Repaint scheduling policy** | filmcraft | The table in §3.4. Reactive mode, repaint while playing or animating, `request_repaint_after` for pollers, a cloned `Context` in every worker. |
| **Table with sort header** | photocraft | The one `TableBuilder` wrapper (`credits.rs:355-392`): measured column widths, a clickable sort header that reverses, a fixed header with scrolling rows. Selection, resizing and virtualization must be built. |
| **Docking adapter cycle** | photocraft | Snapshot → `DockOutput.actions` → apply through the app command path → persist `Layout` plus a hidden-panel `Location` map. |
| **Undo/redo model** | wordcraft | Snapshot-per-step over `Vec<Arc<_>>`, a `typing_open` coalescing flag, one command = one undo step with rollback on error, a 500-step cap. Plaudon's existing `transcriptEditing.ts` already matches and should simply be ported. |
| **Find/replace semantics** | wordcraft | `LayoutJob` match highlighting, last-first replacement ordering, one undo step for Replace All, a match cap. |
| **Command palette** | wordcraft | The registry and keymap only; the palette UI is substring-matched and must be replaced. |
| **AccessKit for custom-painted views** | wordcraft `a11y.rs` | Build an AccessKit subtree for visible content ± one page, cache-invalidated by `Arc::ptr_eq`, capped at 6,000 nodes. Needed only where Plaudon paints text itself (the waveform and timeline); standard egui widgets get AccessKit for free. |
| **Services / host-hook dependency injection** | photocraft, wordcraft | Platform capabilities (file dialog, clipboard, print, system theme) arrive as boxed closures so the UI crate stays free of `rfd` and OS crates — the thing that keeps kittest hermetic. Adopt the idea, not the 35-hook struct. |
| **`ui_state::sanitize`** | photocraft | Drop persisted values that abort startup (zoom 0, NaN, 1e30). A real failure mode. |
| **`gpu_lock` + nextest GPU group cap** | photocraft | Serialise wgpu device creation in tests; WARP and Mesa crash on parallel devices. Directly relevant to the MARS runner. |
| **Paired reference-painter visual tests** | craft-libs | Independent reference painter vs production widget, same renderer, exact RGBA **and** mesh equality, deliberate negative mutations, artifacts behind an env var, font-free context. Good for primitives; cannot catch text regressions; does not scale to screens. |
| **Pitch-preserving speed change** | filmcraft `shuttle.rs` | Two `PitchShifter` stages plus a resampler for 0.5–4×; depends on an unpublished crate, so Plaudon needs rubato/signalsmith-stretch. |
| **Autosave policy** | wordcraft | Only autosave a file the user already saved; 2.5 s after the last change and 1.5 s after the last caret activity; disable with a message on failure (T13). |
| **Lenient settings load** | wordcraft | `UiState::from_json` / `options::lenient<T>` drop individually malformed fields rather than losing the file. |

### 5.4 Unsuitable

| Item | Why |
| --- | --- |
| `photocraft-ui-egui`, `filmcraft-ui-egui`, `wordcraft-ui-egui` as dependencies | Path crates in their own workspaces, bound to an engine `Session`, 15–17 internal crate dependencies each, unpublished, domain-specific. |
| wordcraft's editor stack (`canvas` + `doc` + `layout` + `render`) | A page-raster WYSIWYG word processor over ~60k lines of docx semantics and `vello_cpu`. A transcript is a list of rows with per-row widgets; a page raster cannot express that. |
| wordcraft's command palette UI | Substring matching, no ranking, no keyboard navigation, no Enter to run, disabled commands listed. |
| photocraft `dock.rs` (legacy) | Six hard-coded Photoshop group enums. |
| photocraft `dialogs.rs` (852 lines) | Photoshop dialog dispatch. |
| photocraft `widgets.rs` (2,292 lines) | Engine `prefs::Unit` coupling and Photoshop semantics; individual pieces (toggle, pill button, `scrub_label`, numeric-expression fields) are adaptable at 50–100 lines each. |
| filmcraft `theme.rs` / `icons.rs` | Tokens measured from Premiere screenshots (a look-and-feel risk as much as anything) and 100+ NLE-specific icons. |
| filmcraft `FrameServer` (1,056 lines) | Video-frame specific. The budgeted-LRU plus `set_context` repaint hook is an ~80-line idea worth re-writing. |
| filmcraft `timeline.rs` as a whole (3,204 lines) | 14 drag variants, multi-track rows, trims, rolls, slips, slides, transitions, thumbnails, volume lines. A single-track transcript timeline is ~10 % of it. |
| Any icon, font or brand asset from any repository | See §6. |
| `craft_ui::theme::WidgetPalette` as a theme system | Fifteen lines setting five widget fills. Plaudon needs four themes, twenty roles, spacing, typography and contrast guarantees. |

---

## 6. Licensing and attribution obligations

All four repositories are **MIT OR Apache-2.0**, dual-licensed at the recipient's option, which
`deny.toml`'s allow-list already permits. Copyright lines:

| Repository | LICENSE-MIT copyright |
| --- | --- |
| craft-libs | `Copyright (c) 2026 ArtCraft Team and the craft-libs contributors` |
| photocraft | `Copyright (c) 2026 ArtCraft Team and the PhotoCraft contributors` |
| filmcraft | `Copyright (c) 2026 ArtCraft Team and the FilmCraft contributors` |
| wordcraft | `Copyright (c) 2026 ArtCraft Team and the WordCraft contributors` |

**None of the four has per-file SPDX or copyright headers** (verified by grep). Licensing is
repository-level only, so a vendored file carries no provenance unless Plaudon adds it.

### 6.1 What each licence requires in practice

Choosing **MIT** for vendored files: include the MIT permission notice and the copyright line with
all copies or substantial portions. Nothing else.

Choosing **Apache-2.0** (§4): (a) give recipients a copy of the licence; (b) carry prominent notices
in modified files stating that they were changed; (c) retain all copyright, patent, trademark and
attribution notices present in the source — since there are none per file, the repository-level
copyright line and NOTICE content stand in; (d) if the work includes a NOTICE file, include a
readable copy of its *attribution notices* in the derivative work's NOTICE, documentation or display.

craft-libs', photocraft's, filmcraft's and wordcraft's NOTICE files are genuine attribution prose,
so under Apache-2.0 their relevant paragraphs must be propagated. craft-libs' NOTICE additionally
carries a **Source provenance** section crediting the sibling applications its `crates/ui` was
adapted from, each with a revision hash — the exact format Plaudon should imitate.

### 6.2 Recommended procedure for Plaudon

1. Vendor into `third_party/<name>/` with `LICENSE-MIT`, `LICENSE-APACHE` and `NOTICE` copied
   verbatim, plus a `README.md` recording the upstream URL and the exact commit SHA.
2. Add a provenance header to every vendored source file:

   ```
   // Adapted from storytold/craft-libs, crates/ui/src/layout/resize.rs @ <sha>
   // Copyright (c) 2026 ArtCraft Team and the craft-libs contributors
   // Licensed under MIT OR Apache-2.0. Modified: <what changed>
   ```

   This satisfies Apache-2.0 §4(b) and §4(c) and makes future upstream diffs tractable.
3. Add a row per vendored component to `docs/licensing/third-party-register.md`
   (Component | Licence | Redistribution | Attribution | Security note | Decision) — required by
   ADR-0009 and enforced by `tests/integration/tests/licensing_register.rs:101`.
4. Strip brand material. `ArtCraft`, `PhotoCraft`, `FilmCraft`, `WordCraft` names, wordmarks and
   everything under each repository's `docs/brand/` are **explicitly not open source**
   (`docs/brand/LICENSE-brand.txt`: usable only unmodified, only to identify the upstream product;
   forks must remove them). In craft-ui's **source** there are no brand strings at all (grep for
   `photocraft|filmcraft|artcraft|storytold` in `crates/ui/src` and `tests`: no matches); only the
   crate name, the `description` and the README mention "Craft".
5. Do not copy assets. Fonts (Inter, JetBrains Mono, Source Sans 3/Serif 4, Noto Serif — all OFL-1.1),
   Lucide icons (ISC), the SCOWL word list, hyphenation patterns (CC0-1.0), and in particular
   photocraft's `lasso-magnetic.svg`, which is **CC BY 3.0** from the Noun Project and would create an
   attribution obligation. photocraft's own AGENTS.md forbids committing font files.
6. If any Solarized palette values are copied from photocraft's `theme.rs`, carry
   `LICENSE-solarized.txt` (MIT, Ethan Schoonover). Simplest: do not copy them.

### 6.3 The git-dependency problem

`deny.toml` sets `[sources] unknown-registry = "deny"` and `unknown-git = "deny"`. `craft-ui` is not
published to crates.io (no `publish` field, version an inherited `0.1.0`, no release evidence); all
three apps consume it as `git = "https://github.com/storytold/craft-libs", rev = "5c874d51…"`.

**Recommendation: vendor.** The supporting facts are all maintenance signals rather than licensing
ones: there is no CI for craft-ui at all, no CHANGELOG, no versioning policy, version 0.1.0, and the
consuming apps pin a commit rather than a tag. A git dependency would also break any crates.io-only
build path and can vanish on a force-push or a visibility change. Vendoring ≤2,600 production lines
of pure, test-covered, asset-free Rust is cheaper than maintaining a policy exception.

If tracking upstream turns out to matter, the alternative is an ADR plus
`[sources] allow-git = ["https://github.com/storytold/craft-libs"]`; the `cargo deny` licence check
itself passes either way.

### 6.4 Licence check still outstanding

`epaint_default_fonts`, pulled in by `eframe`'s `default_fonts` feature, carries font licences
(OFL-1.1 and UFL-1.0) that are **not** in Plaudon's current `deny.toml` allow-list
`[INFERENCE: from the crate's licence expression; not read in this repository]`. Run
`scripts/license-audit.sh` against the egui dependency tree as the first task of O1.2 and extend the
allow-list with a recorded review if required.

---

## 7. Platform and maintenance summary

| | craft-libs | photocraft | filmcraft | wordcraft |
| --- | --- | --- | --- | --- |
| Licence | MIT OR Apache-2.0 | MIT OR Apache-2.0 | MIT OR Apache-2.0 | MIT OR Apache-2.0 |
| egui / eframe | 0.36.2 (egui only) | 0.36.2 | 0.36.2 | 0.36.2 |
| Edition / MSRV | 2024 / 1.95 | 2024 / 1.95 | 2024 / 1.95 | 2024 / 1.90 |
| `unsafe` | forbidden | forbidden except `tablet` | forbidden except `platform` | forbidden |
| Tests | ≈81 | "more than 1,700" (repo claim) | 2,520 (repo claim) | 821 `#[test]` (docs claim) |
| CI for the UI crate | **none** (only `print.yml`) | `ci.yml` on ubuntu + windows; no macOS | **no general PR workflow**; gates via `cargo xtask ci` | **no general PR workflow**; gates via `cargo xtask ci` |
| kittest snapshot baselines | none | none | none | none |
| Committed `Cargo.lock` | yes | yes | yes | yes |
| `cfg(target_os)` in the UI crate | **none** | shortcut notation, cursors, memory probe, title bar | `cfg(windows)` CPU clock, `cfg(unix)` rustix, wasm | ⌘ glyph, keytips, window geometry, wasm |
| Non-Rust build steps | none | `build.rs` i18n catalogues, optional font embedding, packaging scripts | `build.rs` codegen, winresource, packaging scripts | `build.rs`, macOS Apple-event crate |
| wasm | claimed, not in CI | supported (trunk) | supported (`cargo xtask wasm`) | supported (trunk) |
| Freshness | clone `1aa81a7`; copyright 2026 | docs dated 2026-10-11 | ROADMAP "last updated 2026-10-11" | last commit 2026-10-12 |

All four are "alpha", agent-driven and moving daily. Pin by commit, vendor, and do not track.

Two further operational notes for Plaudon's CI:

- photocraft installs `libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev
  libgl1-mesa-dev libgtk-3-dev` on Linux runners and relies on a software fallback for wgpu; Plaudon's
  MARS Docker image needs the equivalent plus a software Vulkan (lavapipe) adapter, since craft-libs'
  renderer tests fail outright if no adapter initialises.
- Both photocraft and wordcraft exclude native `cargo test` on Windows ARM64 because of "an
  intermittent access violation in egui UI tests there". Plaudon's Windows target is x64, but the
  note is worth carrying into the packaging plan.
