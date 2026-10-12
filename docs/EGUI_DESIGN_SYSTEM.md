# egui design system and application shell

The design tokens, the themes and the shell every screen is rendered inside. Written for EPIC O task
O1.3 (#201); `docs/EGUI_MIGRATION_PLAN.md` §4.1 rows S1–S17 are the contract this document records
against. O2.5 (#209) extends it with whatever the remaining screens add; everything below is in
force now.

Companion documents: `docs/EGUI_MIGRATION_PLAN.md` (what exists and what is being built),
`docs/EGUI_TESTING.md` (how any of it is tested), `docs/EGUI_REFERENCE_COMPONENTS.md` (what was
reused and from where).

## 1. Tokens

`crates/ui/src/theme/tokens.rs` is the single source of truth, ported value-for-value from
`apps/desktop/src/design/tokens.ts`. **No widget in `plaudon-ui` may hard-code a colour or a
spacing value**, exactly as no React component was allowed to. Read them with

```rust
let tokens = plaudon_ui::theme::Tokens::get(ui.ctx());   // published by the shell every frame
ui.label(egui::RichText::new(text).color(tokens.color.text_secondary));
ui.add_space(tokens.space(4));                            // --space-4 = 12 pt
```

`Tokens` is `Copy`. Outside the shell (a widget test, say) `Tokens::get` falls back to the tokens
matching egui's own light/dark choice, so a widget rendered in isolation is still coherent.

### 1.1 The twenty colour roles

| Role (field on `tokens.color`) | `tokens.ts` name | light | dark | hc-light | hc-dark |
| --- | --- | --- | --- | --- | --- |
| `bg_canvas` | `color-bg-canvas` | `#f6f6f7` | `#17191c` | `#ffffff` | `#000000` |
| `bg_surface` | `color-bg-surface` | `#ffffff` | `#1e2125` | `#ffffff` | `#000000` |
| `bg_raised` | `color-bg-raised` | `#ffffff` | `#262a2f` | `#ffffff` | `#0d0d0d` |
| `bg_selected` | `color-bg-selected` | `#dbe7fb` | `#2b3b55` | `#cfe0ff` | `#003c74` |
| `bg_hover` | `color-bg-hover` | `#eceef1` | `#2a2e34` | `#e6e6e6` | `#1a1a1a` |
| `text_primary` | `color-text-primary` | `#14161a` | `#f2f4f6` | `#000000` | `#ffffff` |
| `text_secondary` | `color-text-secondary` | `#53585f` | `#b3bac2` | `#1c1c1c` | `#f0f0f0` |
| `text_inverse` | `color-text-inverse` | `#ffffff` | `#14161a` | `#ffffff` | `#14161a` |
| `border_subtle` | `color-border-subtle` | `#d7dade` | `#3a3f45` | `#000000` | `#ffffff` |
| `border_strong` | `color-border-strong` | `#767c84` | `#6c747d` | `#000000` | `#ffffff` |
| `accent` | `color-accent` | `#1a4fa0` | `#8fb4f2` | `#00349b` | `#b8d4ff` |
| `accent_text` | `color-accent-text` | `#ffffff` | `#101418` | `#ffffff` | `#000000` |
| `status_neutral` | `color-status-neutral` | `#53585f` | `#b3bac2` | `#1c1c1c` | `#f0f0f0` |
| `status_running` | `color-status-running` | `#1a4fa0` | `#8fb4f2` | `#00349b` | `#b8d4ff` |
| `status_waiting` | `color-status-waiting` | `#7a4a00` | `#e3b667` | `#5c3100` | `#ffd48a` |
| `status_success` | `color-status-success` | `#0f6b3f` | `#67c391` | `#004d2b` | `#86e3ad` |
| `status_warning` | `color-status-warning` | `#8a4b00` | `#e8a35c` | `#6b2f00` | `#ffc078` |
| `status_danger` | `color-status-danger` | `#a1121f` | `#f08a92` | `#8a0010` | `#ffb3b8` |
| `focus_ring` | `color-focus-ring` | `#0b57d0` | `#9ec4ff` | `#00349b` | `#ffffff` |
| `overlay` | `color-overlay` | `rgba(16,18,20,.45)` | `rgba(0,0,0,.6)` | as light | as dark |

Colour is never the only carrier of meaning (spec §5.3): every status also carries a glyph and a
word — see `widgets::toast::ToastTone::{glyph, label}`.

### 1.2 Scales

| Scale | Values |
| --- | --- |
| `SPACE[0..=8]` | 0, 2, 4, 8, 12, 16, 24, 32, 48 pt |
| `tokens.radius` | small 3, medium 6, large 10 pt |
| `tokens.text` | xs 11, sm 12, md 13 (body), lg 15, xl 19, 2xl 24 pt; line heights 1.25 / 1.5 |
| `tokens.motion` | fast 90 ms, normal 160 ms |
| `tokens.focus` | width 2 pt (3 pt in high contrast), offset 2 pt |
| Breakpoints | `MIN_DESKTOP_WIDTH` 880, `MIN_DESKTOP_HEIGHT` 560, `COMPACT_LAYOUT_WIDTH` 1040 pt |
| Target size | `MIN_TARGET_SIZE` 24 pt (`style.spacing.interact_size.y`) |

### 1.3 Contrast guarantees, asserted as tests

`tokens.test.ts` is ported one-for-one into `crates/ui/src/theme/tokens.rs`'s unit tests, over the
same four themes:

- primary **and** secondary text ≥ 4.5:1 on canvas, surface and raised;
- every `status_*` colour ≥ 4.5:1 on surface;
- `accent_text` ≥ 4.5:1 on `accent`;
- `border_strong` and `focus_ring` ≥ 3:1 on surface;
- the focus ring is never below 2 pt and widens to 3 pt in high contrast.

`cargo test -p plaudon-ui theme::` runs them; they need no GPU.

## 2. Themes

Four themes: light, dark and a high-contrast variant of each. The user's choice is two independent
preferences, as in `design/theme.ts`: an appearance (`System`, `Light`, `Dark`) and a contrast
(`Standard`, `High`). `plaudon_ui::theme::ThemeState::apply` builds a style for the light theme and
one for the dark theme and installs **both** with `Context::set_style_of`, then sets
`ThemePreference` from the appearance. An operating-system appearance change therefore flips the
window with no application involvement, which is what row S4's "follows OS live" means. The
technique is photocraft's and wordcraft's (`docs/EGUI_REFERENCE_COMPONENTS.md` §2.4); the token
values and the mapping are Plaudon's.

Token → egui mapping worth knowing: a **focused** widget gets egui's `widgets.active` visuals, so
the focus ring is `widgets.active.bg_stroke` = `focus_ring` at `tokens.focus.width`.

**Where the appearance choice lives.** `dto::Preferences` carries `appearance` and `contrast`, but
nothing writes them: `backend::preferences` returns `System`/`Standard` unconditionally and no
published command changes them (the React shell kept the choice in a component reducer and lost it
on restart). Until O2.4 (#208) adds `set_appearance`, the shell persists the choice in its own
state file, `plaudon_app::shell_state::ShellState`. That is the only honest home for it today and it
is isolated in two fields so moving it is a deletion.

## 3. The shell

```
┌───────────────────────────────────────────────────────────────┐
│ File   View   Go   Help                                       │  menu bar      (S8)
├───────────────────────────────────────────────────────────────┤
│ [Import audio…] [Search] [Palette] [Refresh]        Library   │  toolbar       (S6)
├──────────┬──╥─────────────────────────────┬──╥────────────────┤
│ Library  │  ║                             │  ║                │
│ Inbox    │  ║   main region (Role::Main)  │  ║  side pane     │  nav rail      (S1)
│ Jobs     │  ║                             │  ║  (optional)    │  splits        (S7)
│ …        │  ║                             │  ║                │
├──────────┴──╨─────────────────────────────┴──╨────────────────┤
│ Ready                                        2 tasks running  │  status bar    (S11/S14)
└───────────────────────────────────────────────────────────────┘
```

| Part | Module | Notes |
| --- | --- | --- |
| Destinations and routes | `shell::route` | six destinations, twelve route variants, `Route::{path, parse}` for deep links |
| Navigation rail | `shell::nav` | `Role::Tab` in a navigation landmark, arrow keys and Home/End, glyph-only below 1040 pt |
| Menu bar | `shell::menu` | egui menus, see §6 |
| Toolbar | `shell::toolbar` | pure `fit` function moves what does not fit into an overflow menu |
| Panes | `shell::layout` | vendored `craft_ui::layout::{split_rect, ResizeHandle}`: `Role::Splitter`, numeric value, arrow-key resize |
| Status bar | `shell::statusbar` | `Role::Status` + `Live::Polite`; the narrow-window notice below 880 pt |
| Toasts | `widgets::toast` | anchored `Area`, polite live region, manual dismiss only |
| Command map | `shell::commands` | one registry behind the menu, toolbar, palette, keyboard and screens |
| Keyboard | `shell::shortcuts` | chord grammar, keymap, generated reference dialog |
| Palette | `shell::palette` | ranked fuzzy match, arrow keys, Enter to run |
| Window geometry | `shell::window_state` | size, position, maximised; sanitised and rescued on load |
| Startup and onboarding gate | `shell` | loading, capabilities error with retry, onboarding gate |

### 3.1 Adding a screen

One line, once the screen exists:

```rust
shell.register(Box::new(plaudon_ui::screens::library::LibraryScreen::new()));
```

A destination with no registered screen renders an honest empty state naming the task that owns it,
so "not built yet" can never be mistaken for "built and empty". The trait is small:

```rust
pub trait Screen: std::fmt::Debug {
    fn destination(&self) -> Destination;
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>);
    fn has_aside(&self) -> bool { false }
    fn aside(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {}
    fn on_response(&mut self, id: CommandId, command: &'static str,
                   result: &Result<AppResponse, ErrorEnvelope>, cx: &mut ScreenCtx<'_>) {}
    fn on_ui_event(&mut self, event: &UiEvent, cx: &mut ScreenCtx<'_>) {}
    fn on_resync(&mut self, reason: ResyncReason, cx: &mut ScreenCtx<'_>) {}
    fn commands(&self) -> &[CommandSpec] { &[] }
    fn handle_command(&mut self, id: &str, cx: &mut ScreenCtx<'_>) -> bool { false }
}
```

`ScreenCtx` is everything a screen may do to the world: `send(AppCommand)` (the dispatcher, never
blocking), `route()`, `navigate(Route)`, `toast(tone, title, body)`, `set_status(text)`, `tokens()`,
`hooks()` and `request_repaint()`. A screen never calls a backend, never awaits, and never reaches
the operating system: file pickers and the state writer arrive as host hooks from the binary, which
is what keeps the kittest harness hermetic.

## 4. The command map

Every action — menu item, toolbar button, palette entry, keyboard chord, screen command — is one
`CommandSpec` in one registry, so the keyboard reference cannot drift from what the keyboard does.

| Id | Default chord | Group |
| --- | --- | --- |
| `nav_library` … `nav_devices` | `Mod+1` … `Mod+6` | Navigation |
| `search_library` | `Mod+Shift+F` | Navigation |
| `toggle_sidebar` / `toggle_aside` | `Mod+B` / `Mod+Shift+B` | Navigation |
| `focus_main_region` | — (the skip-to-content control) | Navigation |
| `import_audio` | `Mod+O` | Library |
| `command_palette` | `Mod+K` | General |
| `keyboard_shortcuts` | `?` | General |
| `refresh` | `Mod+R` | General |
| `close_overlay` | `Escape` (**not** rebindable) | General |
| `appearance_system` / `_light` / `_dark`, `contrast_standard` / `_high` | — | General |

`Mod+1`…`Mod+6` and their order come from `app/router.ts` and `app/shortcuts.ts`; reordering the
destinations would change what those chords mean, so the order is part of the contract. Playback and
transcript commands are registered at runtime by the player (O1.5) and the transcript editor (O2.1)
and appear in the palette and the reference as soon as they are.

### 4.1 Chords

Portable strings in the canonical order `Mod+Ctrl+Alt+Shift+KEY`, where `Mod` is Command on macOS
and Control elsewhere and `KEY` is an `egui::Key` name (`1`, `F`, `ArrowLeft`, `Escape`, `?`).
Parsing is case-insensitive; `shortcuts::normalize` rewrites to the canonical form and
`shortcuts::display` renders `Mod` as `Cmd` or `Ctrl` for the platform egui reports.

Typing wins over shortcuts: while a text field has keyboard focus only `Escape` is read
(`Context::egui_wants_keyboard_input`), which is the rule `isTextEntryTarget` enforced in the React
shell.

Rebinding is `Shell::apply_bindings(map)` (command id → chords) and `Shell::bindings()`; conflicts,
unknown ids and fixed commands are refused with a reason rather than silently dropped. Overrides are
persisted; defaults are not, so changing a default later still reaches users who never rebound it.

## 5. What is persisted

`plaudon_app::shell_state::ShellState`, one JSON file (`shell-state.json`) beside the library,
written atomically through a temporary file and read leniently — a single malformed field falls back
to its default instead of costing the user everything else, and a file over 256 KiB is refused
rather than read.

| Field | Row |
| --- | --- |
| `window` — size, position, maximised | S12 |
| `panels` — sidebar width, sidebar collapsed, side-pane width | S7 |
| `appearance`, `contrast` | S4 (until `set_appearance` exists, §2) |
| `bindings` — changed keyboard chords only | C9 (until `set_shortcuts` exists) |

A restored geometry is sanitised before it is applied: non-finite or absurd sizes fall back to
1280×820, sizes below 880×560 grow to the minimum, and a window remembered on a monitor that is no
longer there is moved back on-screen rather than lost. The shape follows wordcraft's
`window_geometry.rs` as described in `docs/EGUI_REFERENCE_COMPONENTS.md` §4.5; no code was copied
(that repository is not vendored here), and the rules are unit-tested in `plaudon-app`.

Writing never happens on the render thread: the shell hands the state to a host hook two seconds
after the last change, and the binary writes it on the runtime; closing the window flushes whatever
is pending.

## 6. Decisions

**egui menus, not a native menu bar (row S8).** The plan asks for a "platform menu where practical".
A native bar means `muda`: a new external dependency outside the vendored set, needing its own
licence-register row and `cargo deny` review, rendering nothing a kittest harness can see, and
carrying no AccessKit tree — so the keyboard-reachability and screen-reader evidence ADR-0023
requires would stop at the menu. egui's own menu bar is in the AccessKit tree, is driven by the same
command registry as the palette and the keyboard map, and is covered by the same tests. Entries
render the platform's own modifier.

**`ResizeHandle` splits, not docking (row S7).** `docs/EGUI_REFERENCE_COMPONENTS.md` §5.2 puts
vendoring `craft_ui::docking` at a day's work plus 2,339–2,491 lines, and only "if O1.3 really needs
user-arrangeable panels". It does not: the arrangement is fixed (rail, screen, optional side pane)
and what the user changes is the sizes. The two dividers are the vendored `ResizeHandle`, which is
the strongest accessibility asset found in any reference application.

**The palette matcher is Plaudon's.** wordcraft's is substring matching with no ranking and no
keyboard navigation (§4.3 of the reference document), so `shell::palette::score` is a ranked
subsequence matcher: consecutive runs, word starts and prefixes score highest, longer labels score
lower, identifier matches rank below label matches.

## 7. Accessibility

| Requirement | How it is met | Asserted by |
| --- | --- | --- |
| Landmarks | `Role::Navigation`, `Role::Main`, `Role::Toolbar`, `Role::Status` nodes | `shell_kittest::the_window_has_the_landmarks_…` |
| Skip link | "Skip to main content" is the first focusable control and moves focus to the main region | same, and the focus-order walk |
| Destinations | `Role::Tab` with selected state, position in set, and the full label even when collapsed to glyphs | `the_six_destinations_are_tabs_…`, `a_narrow_window_collapses_the_rail_…` |
| Dividers | `Role::Splitter`, numeric value with bounds, arrow-key resize | `the_sidebar_divider_is_a_splitter_…` |
| Keyboard reachability | Tab reaches every control; nothing traps focus | `every_interactive_element_is_reachable_by_keyboard` |
| Announcements | toasts and the status line are `Role::Status` with `Live::Polite`, and never steal focus | `widgets::toast`, `shell::statusbar` |
| Contrast | §1.3 | `theme::tokens` unit tests |
| Target size | 24 pt minimum interact size | `theme::apply` unit test |
| Screen readers | the manual VoiceOver/Narrator script stays the release gate | `docs/accessibility/manual-screen-reader-script.md` |

High-DPI (row S16) is eframe's `native_pixels_per_point`; the shell neither overrides the scale
factor nor enables keyboard zoom, so the operating system's setting is what applies.

## 8. Repainting

The shell paints when there is an event, an input or a running animation, and never on a timer. The
only `request_repaint_after` it owns is the two-second state-save debounce; the only continuous
animation is the startup spinner and the in-flight indicator, both of which stop when the work does.
Screens ask for a frame with `cx.request_repaint()`; the engine wakes the window through the
`Waker` the binary installs.
