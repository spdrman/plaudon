# Components

Everything on the component sheet ([light](screens/components-light.png),
[dark](screens/components-dark.png), source `source/Components.dc.html`) plus the shell chrome that
every screen board repeats (menu bar, toolbar, rail, status bar, activity bell). The screens in
[`screens/`](screens/) refer back here instead of repeating dimensions.

Each component lists: measurements in points, the tokens it reads, the states the board draws, the
egui widget or container to build it with, keyboard and focus, accessibility, and what the repo
does today. Tags are the ones from [`README.md`](README.md). Token names are the ones in
[`TOKENS.md`](TOKENS.md). A value that is not in the boards is "not specified".

Two cross-cutting rules apply to everything below, and both are product rules from the overview
board:

- **Never colour alone.** Every status carries a glyph and a word. Colour reinforces them.
- **Every pair is measured.** See `TOKENS.md` section 6. Do not add a text-on-fill pair without
  measuring it with `tokens::contrast_ratio`.

A note on the egui calls the boards name. The boards were written against egui 0.36 vocabulary but
use older container names in places (`TopBottomPanel`, `SidePanel`). The repo's own shell uses
`egui::Panel::top` and `egui::Panel::bottom` plus its own `shell::layout::{pane, vertical_divider}`
for the rail and aside (`crates/ui/src/shell/mod.rs`). Follow the repo's calls, not the board's
names.

---

## 1. Buttons

Board: component sheet, "Buttons". Picture: top block of
[`components-light.png`](screens/components-light.png).

| Property | Value |
| --- | --- |
| Height | 28 on screens (minimum 24, `MIN_TARGET_SIZE`) |
| Padding | 0 12 (`button_padding` is 12 by 4) |
| Radius | `radius.medium` 6 |
| Border | 1 pt |
| Text | `md` 13, primary and destructive-pressed at weight 600 |

| Variant | Default | Hovered | Pressed | Keyboard focus | Disabled |
| --- | --- | --- | --- | --- | --- |
| Primary ("Import audio…") | fill `accent`, text `accent_text`, border `accent`, weight 600 | fill and border `accent_hover` | `accent_hover` (same as hovered) | default look plus ring | default look at 0.5 alpha |
| Secondary ("Refresh") | fill `bg_surface`, text `text_primary`, border `border_strong` | fill `bg_hover` | fill `bg_selected` | default plus ring | text `text_secondary`, border `border_subtle`, 0.5 alpha |
| Destructive ("Delete recording…") | fill `bg_surface`, text and border `status_danger` | fill `tint_danger` | `tint_danger`, weight 600 | default plus ring | default at 0.5 alpha |
| Link ("Clear filters") | transparent, text `accent`, no border | underline | fill `bg_hover`, underline | ring | 0.5 alpha |

**Ring.** 2 pt `focus_ring` painted 2 pt outside the rect, with a 2 pt gap in the surrounding
surface colour. See `TOKENS.md` section 4 for why it cannot sit on the accent fill.

**Destructive confirm** (inside a dialog) is a different thing: a `status_danger` fill with
`text_inverse` text, weight 600 (white on `#a1121f` 8.01:1, `#0b1220` on `#f08a92` 7.81:1).

egui: `egui::Button::new(text).fill(accent)` with `RichText::color(accent_text)` for primary.
Secondary uses the inactive widget visuals that `style_for` already installs. Hovered swaps to
`bg_hover` through `widgets.hovered`, or to `accent_hover` for primary. Link is
`Button::new(..).frame(false)` in `accent`. Disabled is `ui.add_enabled(false, ..)` plus alpha
(`ui.multiply_opacity` or `gamma_multiply(0.5)` on the colours).

Keyboard: Tab and Shift+Tab reach every button; Enter and Space activate. Focus is never removed.
Disabled controls stay in the AccessKit tree with their names.

State of the repo:

- Secondary buttons: EXISTS (inactive visuals).
- Primary: EXISTS in one place only. `settings::section::primary_button` (used by Settings and
  onboarding) fills with `SettingsTokens::accent()` and picks black or white by luminance instead of
  reading `accent_text`. Inside the shell that trait is implemented by `Tokens`
  (`settings/shell_adapter.rs`), so the fill is the real `accent`; rendered alone (tests) it falls
  back to `VisualsTokens`, whose accent is `visuals.selection.bg_fill`, the pale `bg_selected`.
  Everywhere else a primary action is a plain `ui.button` and renders as secondary: the Library empty
  state "Import an audio file", the Jobs "Retry job", the review "Seek to this passage". CHANGE: one
  primary helper that reads `Tokens` and `accent_text`.
- `accent_hover`: PROPOSED (new role).
- Destructive: EXISTS as red text only (`error_fg_color`), no border and no tint hover. CHANGE.
- Ring outside the rect: PROPOSED. Today the ring is `widgets.active.bg_stroke` on the widget edge.
- Height 28: PROPOSED. `interact_size.y` is 24, so buttons render at about 24.
- Link: EXISTS (`Button::frame(false)` is used for "Skip to main content" only when unfocused).

---

## 2. Text inputs

Board: component sheet, "Text inputs".

| Property | Value |
| --- | --- |
| Height | 28 (26 inside the Library filter bar; 24 in the transcript find bar; 30 in the palette) |
| Padding | 0 8 (0 7 when the border is 2 pt) |
| Radius | `radius.medium` 6 |
| Text | `md` 13 |
| Label | `sm` 12 `text_secondary`, above the field (sheet) or to its left (filter bars) |

| State | Border | Fill | Text |
| --- | --- | --- | --- |
| Default | 1 pt `border_strong` | `bg_surface` | hint text in `text_secondary` ("Title contains…") |
| Hovered | 1 pt `text_secondary` | `bg_hover` | hint |
| Focused | 2 pt `focus_ring` | `bg_surface` | `text_primary` with a 1 pt caret |
| Disabled | 1 pt `border_subtle` | `bg_canvas` | `text_secondary` at 0.6 alpha ("Not editable") |
| With an error | 2 pt `status_danger` | `bg_surface` | `text_primary`, plus a line below: "✕ Use YYYY-MM-DD" in `status_danger`, `sm` |
| Password | as default | as default | bullets with letter spacing 2 (`TextEdit::password(true)`) |

egui: `TextEdit::singleline(&mut s).hint_text(..).desired_width(..)`. The 4.55:1 and 4.05:1 numbers
on the board are `border_strong` against `bg_surface`. Error is a danger stroke plus a glyph-and-word
line under the field, never colour alone.

Keyboard and accessibility: every field has a visible label bound with `labelled_by`; hint text is
not a name. While a field has focus only Escape is read by the shell (`egui_wants_keyboard_input`),
so shortcut letters do not fire mid-word. The palette field is the one place the repo names a field
by hand (`node.set_label("Search commands")`).

State of the repo: EXISTS. The filter bar and day fields already use `labelled_by`. CHANGE: border
colours and the 2 pt focus stroke follow the tokens automatically through `style_for`; the error
variant (stroke plus line) has no shared helper today, only free-standing red labels, so add one.
`settings::section::secret_field` is the password field.

---

## 3. ComboBox

Board: component sheet, "Choice controls", left.

| Property | Value |
| --- | --- |
| Closed height | 28 (26 in the Library filter bar), minimum width 150 in filter bars, 190 on the sheet |
| Chevron | 10 by 6, stroke 1.5, `text_secondary`; points down when closed, up when open |
| Open state | 2 pt `focus_ring` border |
| Popup | fill `bg_raised`, 1 pt `border_subtle`, radius `radius.medium` 6, shadow (section 2 of `TOKENS.md`), padding 4, same width as the control |
| Item | padding 5 by 8, radius 3, `md` 13 |
| Current item | fill `bg_selected`, weight 600 |
| Hovered item | fill `bg_hover` |

Items on the sheet: "All import states", "Pending", "Importing", "Imported", "Import failed". The
first entry is always the explicit "All ..." choice, because an empty combo does not say that
nothing is filtered.

egui: `ComboBox::from_label(label)` with `selected_text`, then `selectable_label` rows. The popup
reads `window_fill` (`bg_raised`) and `popup_shadow`. Keyboard: Space or Enter opens, Up and Down
move, Enter chooses, Escape closes (`close_overlay`), focus returns to the control.

State of the repo: EXISTS (`choice_filter` in `crates/ui/src/screens/library/toolbar.rs` builds
exactly this, including the "All ..." entry). CHANGE: colours and shadow through tokens; the
popup shadow is egui's default today.

---

## 4. Checkbox, radio, slider

Board: component sheet, "Choice controls", right.

**Checkbox.** 16 by 16, radius 3. Off: 1 pt `border_strong` on `bg_surface`. On: `accent` fill with
an `accent_text` tick (stroke 1.8, path `M1 4 l3 3 5-6` in a 10 by 8 box). Focused: ring with a 2 pt
surface gap. Disabled: 0.5 alpha. Label is `md` 13, gap 8. egui: `Checkbox`, reading
`widgets.inactive/hovered/active`.

**Radio.** 16 circle. Selected: a 5 pt `accent` ring around the surface fill (reads as a dot).
Unselected: 1 pt `border_strong`. Label `md` 13, gap 8. egui: `radio_value`. Where a radio group chooses a
policy (Consent scopes, onboarding Network policy) the selected row also fills `bg_selected`, so the
choice is not only a dot.

**Slider.** Track 4 high, radius 2, 140 wide on the sheet (96 for Volume in the player). Fill is
`accent` before the handle and `wave_remaining` after it. Handle is a 16 circle in `bg_surface` with a
1 pt `border_strong` ring; focused adds the 2 + 2 ring. egui: `Slider` with `show_value(false)`.
Keyboard: arrows nudge, Home and End jump.

State of the repo: EXISTS for all three. The checked fill and tick colours come from
`widgets.active`, which is `bg_selected` plus a focus-coloured stroke today, not `accent`.
CHANGE: draw the checked fill in `accent`. The slider's track colours (`wave_remaining`) are
PROPOSED because the role is new.

---

## 5. Table rows

Board: component sheet, "Table rows"; the real table is the Library board
([`library-light.png`](screens/library-light.png)).

| Property | Value |
| --- | --- |
| Header height | 30 on the sheet, 32 on the Library board (disagreement, see `TOKENS.md` section 7) |
| Row height | 32 |
| Header text | `sm` 12, weight 600, `text_secondary`; the sorted column is `text_primary` with a "▼" or "▲" |
| Cell padding | 0 8 on the Library board, 0 10 on the sheet |
| Row separator | 1 pt `bg_hover` along the bottom of each row |
| Body text | `md` 13; secondary columns `text_secondary`; duration is mono 12 |

| Row state | Fill | Notes |
| --- | --- | --- |
| Default | none | |
| Hovered | `bg_hover` | |
| Selected | `bg_selected`, title weight 600 | |
| Selected and focused | `bg_selected` plus an inset 2 pt `focus_ring` | the keyboard cursor |

Text stays `text_primary` in every state (12.68:1 on selected light, 11.31:1 dark). Status cells
draw the glyph and the word in the `status_*` colour; measured pairs on selected and hovered rows are
in `TOKENS.md` section 6.1 and all clear 4.5:1.

egui: `egui_extras::TableBuilder`, `body.rows(32, total, ..)` so only visible rows build. Row fills
are painted per state with `painter().rect_filled`. Only the sorted column shows an arrow and reports
`sort_direction` to AccessKit. Context menu is `response.context_menu()` on the row.

State of the repo: EXISTS in `crates/ui/src/screens/library/table.rs` (virtualised, one tab stop,
`Role::Grid`/`Role::Row`/`Role::ColumnHeader`, sort direction on the active column only). CHANGE:
`ROW_HEIGHT` 28 to 32, `HEADER_HEIGHT` 24 to 32; `.striped(true)` goes (the board has hairlines,
no stripes); unsorted sortable headers print "↕" today and the board prints nothing; paint the row
fill states; draw the status glyph with the painter. The nine columns on the board lack "Last error",
which the repo has as a tenth column (see `screens/library.md`).

---

## 6. Tabs

Board: component sheet, "Tabs and navigation", top row; used by the Recording outputs column.

| Property | Value |
| --- | --- |
| Tab height | 28, padding 0 8, `sm` 12, gap 4 between tabs |
| Strip | 1 pt `border_subtle` along the bottom |
| Current | weight 600, `text_primary`, 2 pt `accent` rule under it |
| Inactive | `text_secondary` |
| Hovered | fill `bg_hover`, radius 3 3 0 0 |
| Keyboard focus | inset 2 pt `focus_ring`, radius 3 (the focused tab draws 26 high so the ring fits) |

Tabs, in order: Transcription, Summarize, Outputs, Review, History, Export.

egui: `selectable_label` with a painted 2 pt `accent` rule under the current one. AccessKit
`Role::Tab`, arrow keys move between tabs. On the Recording screen the strip wraps onto a second row
at 330 pt wide (see `screens/recording-transcript.md`).

State of the repo: EXISTS as plain `selectable_label` in `OutputsTab`
(`crates/ui/src/screens/recording/ai_panels.rs`, `AiBinding::ui`), laid out in `horizontal_wrapped`.
No `Role::Tab` is set there, no arrow-key handling, no rule. CHANGE: a shared tab strip helper with
`Role::Tab`, roving focus and the rule. The nav rail already has the roving pattern
(`shell::nav::arrow_target`) to copy.

---

## 7. Navigation rail

Board: component sheet, "Tabs and navigation" (states), and every screen board (the real rail).
Pictures: left edge of [`library-light.png`](screens/library-light.png).

**Full rail.**

| Property | Value |
| --- | --- |
| Width | 216 (sidebar drag range stays 168 to 420) |
| Padding | 12 vertical, 10 horizontal; rows stack with gap 2 |
| Header | mark 26 high plus "Plaudon" `lg` 15, weight 700, letter spacing -0.01 em; padding 4 8 10 |
| Skip link | "Skip to main content", `sm` 12, `text_secondary`, padding 2 8 8, directly under the header |
| Row | 32 high, padding 0 8, radius 6, gap 10, `md` 13 |
| Glyph tile | 20 by 20, radius 3, `xs` 11, weight 700 |
| Shortcut hint | `xs` 11, `text_secondary`, right-aligned ("Ctrl+1" to "Ctrl+7"; macOS shows Cmd) |

| Row state | Row fill | Tile |
| --- | --- | --- |
| Current | `bg_selected`, label weight 600 | fill `accent`, letter `accent_text` |
| Inactive | none | fill `bg_hover`, letter `text_secondary` |
| Hovered | `bg_hover` | fill `bg_surface`, letter `text_secondary` |
| Focused | inset 2 pt `focus_ring` | fill `bg_hover` |

Destinations in order, with their tiles: L Library, I Inbox, J Jobs, T Templates, S Settings, D
Devices, P People. Seven of them. Order is a contract: `Ctrl+1` to `Ctrl+7` index into
`Destination::ALL`.

**Compact rail** (window narrower than 1040 pt). A 52-wide strip, 1 pt `border_subtle`, radius 6,
padding 6 0. The mark sits on top at 20 high with 4 below it. The current destination is a 32 by 32
`bg_selected` square (radius 6) holding the 20 pt `accent` tile; the others are bare 20 pt
`bg_hover` tiles with 6 pt above and below. The full label stays in the AccessKit name.

**Keyboard.** Up, Down, Left, Right move and wrap; Home and End jump; Enter and Space activate;
`Ctrl+1`..`Ctrl+7` go straight to a destination. The skip link is the first focusable control.

egui: `selectable_label` per destination inside `nav_rail`, `Role::Tab` with `set_position_in_set` and
`set_size_of_set`, inside a `Role::Navigation` landmark labelled "Primary".

State of the repo: `crates/ui/src/shell/nav.rs` EXISTS and already does the keyboard model, the
roles and the collapsed glyph-only mode. CHANGE: it draws a plain `selectable_label` per
destination, with no header, no tile, no shortcut hint, and the skip link lives in
`Shell::body` above the panes, not inside the rail. `COLLAPSED_WIDTH` is 56, `DEFAULT_SIDEBAR_WIDTH` is
208. The header mark is PROPOSED and needs an image loader (**NEW DEP**, see section 15).

---

## 8. Badges

Board: component sheet, "Badges". Picture: the badge block in
[`components-light.png`](screens/components-light.png).

| Property | Value |
| --- | --- |
| Frame | fill `tint_*`, radius `radius.small` 3, inner margin `Margin::symmetric(8, 2)` |
| Text | glyph then word, `sm` 12, weight 600, colour `status_*` |
| Line height | 22 on the sheet |
| Small variant | 11 weight 600, padding 0 6, line height 18, used for claim badges in Outputs and Review |

The 15 badges on the sheet:

| Badge | Tone |
| --- | --- |
| ✓ Local only | success |
| ! Network allowed for providers you enable | warning |
| ✓ Imported | success |
| ⟳ Transcribing | running |
| ◷ Queued | neutral |
| – Not transcribed | neutral |
| ! Stale | warning |
| ✕ Transcription failed | danger |
| ✓ Supported | success |
| ! Experimental | warning |
| ? Not evaluated | neutral |
| × Unsupported | danger |
| ✓ Source linked | success |
| ! Review required | warning |
| ! Waiting for you | waiting |

Measured: text on its own tint is 5.41 to 6.70:1 in both themes. Register words (Supported,
Experimental, Not evaluated, Unsupported) come from the claim register, never from the UI.

**Glyphs.** The built-in fonts render ✓ ✕ ◷ ⊘ (and the arrow in "Back to library") as empty boxes in
today's snapshots; I confirmed this in the committed baseline
`crates/ui/tests/visual/library_100_rows.png`, where the Imported, Queued and Transcribed
cells show a hollow square instead of a tick. PROPOSED: draw ✓ ✕ ◷ ⊘ with the painter, as small
shapes laid out next to the text. Keep the glyph in the AccessKit label so the sentence a screen
reader gets is unchanged.

State of the repo: **four separate badge renderers**, none of which uses `Tokens` for colour:

| Renderer | File | Tone type | How it colours |
| --- | --- | --- | --- |
| `status_badge` | `crates/ui/src/screens/library/status.rs` | `StatusTone` (six variants) | `Neutral` and `Waiting` use `weak_text_color`; `Success` and `Running` use `text_color`, so they are not green or blue at all; `Warning` and `Danger` use `warn_fg_color` and `error_fg_color` |
| `Badge` | `crates/ui/src/ai/badge.rs` | `plaudon_app::tone::Tone` (six) | `Running` is `strong_text_color`, `Waiting` and `Warning` share `warn_fg_color`, `Success` is two hard-coded greens |
| `section::status_badge` | `crates/ui/src/settings/section.rs` | `settings::theme::Tone` (four) | text only, no tint. In the shell the colours come from `Tokens` (`impl SettingsTokens for Tokens`); rendered alone they come from `VisualsTokens`, which hard-codes reds, ambers and greens |
| `badge_ui` and `badge_inline` | `crates/ui/src/screens/devices/badge.rs` | `devices::presentation::Tone` | `Warning` and `Danger` only; the other four tones use the ordinary text colour |

CHANGE: replace all four with one badge built on `Tokens` and the tint roles. This is the single
largest shared change in the revision (see `GAP_ANALYSIS.md`).

---

## 9. Toasts

Board: component sheet, "Toasts"; in place on
[`library-empty-light.png`](screens/library-empty-light.png).

| Property | Value |
| --- | --- |
| Frame | fill `bg_raised`, 1 pt `border_subtle`, radius `radius.large` 10, popup shadow |
| Padding | 10 by 12 on the sheet, 12 by 14 on the Library empty board |
| Width | 340 on the Library empty board; not specified elsewhere |
| Tone tile | min 22 by 22, radius 3, `xs` 11 (12 on the Library board), weight 700, glyph on a `tint_*` fill in `status_*` |
| Title | `md` 13, weight 600 |
| Body | `sm` 12 (13 on the Library board), `text_secondary` |
| Dismiss | right-aligned ghost button, 22 high (24 on the Library board), `sm` 12, `text_secondary` |
| Stack | gap 8, bottom right |

The four tones and their shipped glyphs: Info `i` (running), Success `OK` (success), Warning `!`
(warning), Danger `X` (danger). The Library board adds an optional progress bar (6 high, radius 3,
track `bg_hover`, fill `accent`, indeterminate segment) and an optional action button ("Open the
recording you already have", 26 high, secondary).

Behaviour (unchanged): anchored `Area` at `RIGHT_BOTTOM`, `Role::Status` with `Live::Polite`, never
takes focus, dismissed only by its own button, no auto-dismiss.

State of the repo: `crates/ui/src/widgets/toast.rs` EXISTS and already honours the behaviour. CHANGE:

- It draws one line `"{glyph} {label}: {title}"` in the tone colour (for example "OK Success:
  Import finished"); the board shows the glyph alone in a tile and the title beside it. Keep the tone
  word in the AccessKit label.
- `Info` is `status_neutral` in the repo and `status_running` on the board.
- Frame radius is `medium` 6 (board 10), no shadow set, Dismiss is a full vertical button, anchor
  offset is `(-16, -16)` against the window (the Library board places the stack 24 from the right and
  20 from the bottom of the main region).
- The progress bar and action button inside a toast: PROPOSED. Today import progress and the
  per-file results are an inline "Import" frame (`import_results_panel` in
  `screens/library/import.rs`), not toasts. The board does not say what happens to the per-file rows;
  keep them.

---

## 10. Progress and spinner

Board: component sheet, "Progress and notices"; in place on the Jobs board.

**Determinate.** 18 high, radius 3, track `bg_hover`, fill `accent`, label `xs` 11 weight 600 at 8
from the left: "Running locally: 62% complete". egui: `ProgressBar::new(f).text(..)`.

**Indeterminate.** Same bar with a moving segment (24 percent wide), label "Preflight: progress
not reported". egui: `ProgressBar::animate(true)`; only animate while the work runs. Never show a
percentage that was not reported.

**Label colour rule (do not copy the board literally).** The board draws the determinate label in
`accent_text` ("switches to `accent_text` over the fill", 6.55 and 7.76:1) and the indeterminate
label in `text_primary`. Two defects, both measured:

1. A determinate label wider than the fill spills onto the pale track. `accent_text` on `bg_hover` is
   about 1.1:1, so a low percentage makes the text unreadable.
2. The indeterminate label crosses the accent segment. `text_primary` on `accent` is **2.32:1 light
   and 2.23:1 dark**, a fail (visible in the Preflight card on
   [`jobs-light.png`](screens/jobs-light.png), where "reported" is half over the fill).

Fix, which keeps every pair above 4.5:1: paint the label twice with two clip rects, `text_primary`
clipped to the track and `accent_text` clipped to the fill. `text_primary` on `bg_hover` is 13.38
light and 14.03 dark; `accent_text` on `accent` is 6.55 and 7.76. egui's `ProgressBar::text` paints
one colour, so this is a custom painter (PROPOSED), or put the text above the bar and keep the bar
text-free.

**Spinner.** 20 across on the sheet (28 on the error board), a ring of radius 11 and stroke 3, track
`bg_hover`, a quarter arc in `accent` with round caps. Only while work runs; it is the only
continuous animation. egui: `Spinner`.

State of the repo: `egui::ProgressBar` is used by `ai/job.rs` (`JobCard::progress`) and the import
panel; `Spinner` by `shell/statusbar.rs` and `loading_ui`. CHANGE: bar colours come from egui's
defaults (the bar fill is `selection.bg_fill`, the pale `bg_selected`), so they need the accent
fill and the two-clip label.

---

## 11. Error notice

Board: component sheet, "Progress and notices"; in place on the Error states board
([`error-states-light.png`](screens/error-states-light.png)).

| Property | Value |
| --- | --- |
| Frame | fill `tint_danger` (`tint_warning` for a guided condition such as offline), radius 6, padding 10 by 12 (12 on the Library variant), gap 4 to 8 |
| Title | `md` 13, weight 600, `status_danger` (or `status_warning`), starts with the glyph: "✕ The import failed." |
| Message | `md` 13 `text_primary` (Library variant), the envelope's own words |
| Diagnostic id | mono 12, `text_secondary`, "Diagnostic id: [id]" |
| Controls | secondary, 24 high on the sheet, 28 on the Library variant; the Library variant has Retry as primary plus the envelope's action as secondary |

egui: `widgets::error_notice` is one `Frame`. It renders the envelope and never classifies or
rewrites the failure. `Role::Alert`.

State of the repo: `crates/ui/src/widgets/error_notice.rs` EXISTS. It draws `Frame::group` with a
red "✕ {message}" label, **one** control (`ErrorControl` is a single value: Retry or Settings), and
the diagnostic id in small weak text. CHANGE: tinted frame, tone title, monospace id, and two
controls when the envelope is both retryable and carries an action (the Library variant shows
Retry plus Open diagnostics). The copy is the app's and stays: `control_for` owns the button labels
("Retry import", "Open storage settings", "Review local-only mode" and so on). Note the casing
difference with `job_presentation` ("Review Local-only Mode"); leave both.

---

## 12. Dialogs

Board: component sheet, "Dialogs"; Consent board for the large one.

| Property | Value |
| --- | --- |
| Backdrop | `overlay` over the whole window |
| Frame | fill `bg_raised`, 1 pt `border_subtle`, radius `radius.large` 10, popup shadow |
| Padding | 16 on the sheet, 20 by 22 on the Consent board |
| Title | 15 bold on the sheet, 19 bold on the Consent board (disagreement; see `TOKENS.md` section 7) |
| Body | `sm` 12, line height 1.5, `text_secondary` |
| Footer | buttons right-aligned, 28 high, padding 0 10, `sm` 12, gap 8 |

The three dialogs on the sheet:

1. **Cancel this job?** Body: "Stopping our remaining work and discarding incomplete output. Work already
   sent to a provider cannot be recalled." Buttons: "Keep running" (secondary, the safe one) and
   "Cancel the job" (danger fill, `text_inverse`). **Do not use this body copy.** The board says the
   sentence "is my placeholder for the §14.7 disclosure". The real text is
   `plaudon_app::job_presentation::CANCEL_DISCLOSURE`, already used by `JobCard`.
2. **Delete "Voice memo"?** Body names what will be removed ("1 audio file removed · transcript and
   outputs · Storage freed: 4.2 MB") under "What will be removed", then the limits under "What deleting cannot do".
   The board says "the five DELETION_LIMITS sentences, verbatim"; the constant has **six**
   (`crates/app-core/src/deletion.rs`) and the test `recording_detail.rs` asserts every one. Render
   all of them. The board also omits the "Audio files" media section that `deletion.rs` renders. Keep
   it. Buttons: Cancel, "Delete permanently" (danger fill).
3. **This audio is already in your library.** "Matches Weekly sync, platform team · content hash
   3f9a1c0e2b7d". Four buttons: "Open existing recording" (primary, full width), "Import anyway as a
   separate recording" (secondary, full width), then "Skip" and "Decide later" side by side.

Behaviour: `egui::Modal` with the overlay as backdrop colour. Escape is `close_overlay` (fixed, not
rebindable) and maps to the safe choice. Focus starts on the safe button, stays inside the dialog,
and returns to the opener on close. A destructive dialog never has the destructive button as the
default focus.

State of the repo: EXISTS as `egui::Modal` everywhere (`settings::section::confirm_modal`,
`JobCard`, `DeletionFlow`, `ConsentSheet`, `duplicate_dialog`). They do not set a backdrop colour,
so egui's default scrim is used. CHANGE: `overlay`, tokens, shadow, danger fill with
`text_inverse`, and make sure focus starts on the safe button in each (check each dialog's test).

---

## 13. Shell chrome

Every screen board repeats this. Pictures: top, left and bottom edges of
[`library-light.png`](screens/library-light.png). Tokens as noted; background of all chrome is
`bg_canvas`, separated by 1 pt `border_subtle`.

| Region | Height or width | Content |
| --- | --- | --- |
| Menu bar | 28 | File, View, Go, Help; `md` 13, gap 18, padding 0 16. Hidden at run time on macOS where the NSMenu takes over (`set_in_window_menu`) |
| Toolbar | 44 | padding 0 16, gap 8. Left: **Import audio…** (primary), **Search recordings**, **Command palette** with an inline "Ctrl+K" hint (`xs` 11, `text_secondary`), **Refresh**. Right: Activity bell, then the screen title (`lg` 15, weight 600, margin-left 8) |
| Rail | 216 | section 7 |
| Main region | rest | `Role::Main`, id `main`; padding 24 horizontal, 20 vertical (off-scale) on list screens |
| Status bar | 26 | padding 0 16, `sm` 12, `text_secondary`: status text left; right: "N tasks running" then the Local-only badge |

The toolbar fits by overflow: `toolbar::fit` moves what does not fit into "More actions". The title
is the route title (`Route::title`): "Library", "Jobs", "Devices", "Settings", "Recording", "Search"
and so on. On every Recording board the title still reads "Library"; the repo would say "Recording"
(`Route::title` for `LibraryRecording`). The board is wrong there, the repo is right. See
`screens/recording-transcript.md`.

**Activity bell.** A secondary button 28 high, padding 0 10: the word "Activity" and, when
there are unread entries, a count pill (18 high, min 18 wide, circle radius 9, fill `accent`, text
`accent_text`, `xs` 11, weight 700). The AccessKit name states the count in words: "Activity: 2
unread notifications", "Activity: 1 unread notification", "Activity: no unread notifications". With
zero unread there is no pill (nothing is decorated). State of the repo: `shell/bell.rs` EXISTS with the same word, the same
AccessKit strings, and the count as text ("Activity 2") in `status_running` bold. CHANGE: pill.

**Local-only badge in the status bar.** `✓ Local only` in `status_success` on `tint_success`
(padding 1 by 8, radius 3, weight 600), or `! Network allowed for providers you enable` in
`status_warning` on `tint_warning`. It renders what the egress gate answered, never the stored
preference. The boards tag it PROPOSED. The strings exist already (`local_only_status` in
`crates/app/src/settings_presentation.rs`); what is missing is the shell holding the enforced policy.
The Consent and Jobs boards show the warning variant, which is how a user sees that Local-only is off
while a cloud dialog is open. PROPOSED.

**Status bar behaviour (unchanged).** `Role::Status` with `Live::Polite`; the spinner and count appear
only while commands are in flight; the narrow-window notice below 880 pt stays.

**Skip link.** First focusable control in the window; it moves focus to the main region. The board
places it under the rail header, `sm` 12 `text_secondary`. EXISTS (`Shell::skip_link` in
`shell/mod.rs`): a small frameless button above the panes whose frame appears once any control has
focus. CHANGE: move it inside the rail, under the header, so it is still the first Tab stop (the
focus-order tests in `shell_kittest.rs` assert it is in the Tab order).

---

## 14. Keyboard map (shell level)

Unchanged by the design, written down so the revision does not lose any of it. Source:
`crates/ui/src/shell/commands.rs`.

| Chord | Action |
| --- | --- |
| `Mod+1` to `Mod+7` | Library, Inbox, Jobs, Templates, Settings, Devices, People |
| `Mod+Shift+F` | Search recordings |
| `Mod+K` | Command palette |
| `Mod+O` | Import audio |
| `Mod+R` | Refresh |
| `Mod+B`, `Mod+Shift+B` | Toggle sidebar, toggle aside |
| `?` | Keyboard shortcuts |
| `Escape` | `close_overlay`, fixed |

`Mod` is Cmd on macOS and Ctrl elsewhere; the boards always print Ctrl.

---

## 15. Things the components need that the repo does not have

| Need | Why | Tag |
| --- | --- | --- |
| Image loader: `egui_extras` with the `svg` feature (resvg) and `egui_extras::install_image_loaders`, or pre-rasterised PNGs at 1x and 2x with the `png` feature | the mark in the rail header, the monochrome mark in empty and error states, the lockup in onboarding. `crates/ui` uses `egui_extras` for tables only today; the workspace entry is `default-features = false` | NEW DEP |
| Inter Regular and SemiBold TTF through `FontDefinitions`, plus a licence-register row | the brand face. Optional; the layout holds with the built-in face | NEW DEP |
| A painter-drawn glyph set (✓ ✕ ◷ ⊘, ⟳) | fonts render them as boxes today | PROPOSED |
| A tinted badge frame helper reading six `tint_*` roles | one badge for the whole app | PROPOSED |
| `accent_hover`, `wave_remaining`, six `tint_*` | new token roles | PROPOSED |
| Dashed rounded rectangle for the drop target | `Shape::dashed_line` round the main rect while files hover. Name unverified here, I had no egui source to check | PROPOSED |
| A vertex-coloured `Mesh` for the app icon tile's gradient | the overview says "one linear gradient in the app icon tile only". The boards do not draw an in-app icon tile, so this is only needed if the dock or window icon is drawn in egui | not specified |
