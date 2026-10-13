# Tokens

Every colour, size and duration the design boards use, in one place. The machine-readable copy is
[`tokens.json`](tokens.json) (same data, valid JSON). Values come from the Tokens board
(`source/Tokens.dc.html`, picture in [`screens/tokens.png`](screens/tokens.png)) and from the
`THEMES` object that every light board carries in its script. I checked the two against each other
and they agree. Where a board does not say something I write "not specified" and leave it alone.

The target file is `crates/ui/src/theme/tokens.rs`. The Tokens board says "Light replaces LIGHT,
dark replaces DARK, HC_LIGHT and HC_DARK stay as they are". That one sentence drives most of what
follows.

Tags used below are the ones from [`README.md`](README.md): EXISTS, CHANGE, PROPOSED, NEW DEP.

## 1. Colour roles

"Field" is the name on `ColorRoles` in `crates/ui/src/theme/tokens.rs`. A field written as
**NEW** does not exist there yet. "Repo now" is what `LIGHT` and `DARK` hold today, so you can see
how far each value moves.

| Role (board name) | `ColorRoles` field | Light | Dark | Repo now (light / dark) | Usage and egui slot (from the board) | Tag |
| --- | --- | --- | --- | --- | --- | --- |
| bg_canvas | `bg_canvas` | `#f4f6fa` | `#0b1220` | `#f6f6f7` / `#17191c` | `panel_fill`. The window behind every surface: rail, toolbar, status bar. | CHANGE |
| bg_surface | `bg_surface` | `#ffffff` | `#111a2c` | `#ffffff` / `#1e2125` | `extreme_bg_color`, inactive widget fill, cards, table body, text fields. | CHANGE |
| bg_raised | `bg_raised` | `#ffffff` | `#18233a` | `#ffffff` / `#262a2f` | `window_fill`. Modal, popups, menus, toasts, the palette. | CHANGE |
| bg_selected | `bg_selected` | `#e2ebff` | `#1c3366` | `#dbe7fb` / `#2b3b55` | `selection.bg_fill`, `widgets.active` fill, selected row, current nav tab. | CHANGE |
| bg_hover | `bg_hover` | `#edf1f7` | `#1a2540` | `#eceef1` / `#2a2e34` | `widgets.hovered` fill, hovered row, `faint_bg_color`, `code_bg_color`. | CHANGE |
| text_primary | `text_primary` | `#172544` | `#f2f6ff` | `#14161a` / `#f2f4f6` | Body, headings, inactive and hovered `fg_stroke`. The light value is brand ink. | CHANGE |
| text_secondary | `text_secondary` | `#4d5873` | `#a9b4d0` | `#53585f` / `#b3bac2` | Supporting text, facts, column headers, noninteractive `fg_stroke`. | CHANGE |
| text_inverse | `text_inverse` | `#ffffff` | `#0b1220` | `#ffffff` / `#14161a` | Text on a danger fill (Delete permanently). | CHANGE |
| border_subtle | `border_subtle` | `#d9dfea` | `#27334d` | `#d7dade` / `#3a3f45` | Hairlines, separators, card strokes. Decorative only. | CHANGE |
| border_strong | `border_strong` | `#6b7690` | `#6d7a99` | `#767c84` / `#6c747d` | Edges that must be seen: text fields, checkboxes, ComboBox, hovered stroke. | CHANGE |
| accent | `accent` | `#0a4fe0` | `#6ea8ff` | `#1a4fa0` / `#8fb4f2` | Primary button fill, `hyperlink_color`, selected tab rule, waveform before the playhead. | CHANGE |
| accent_text | `accent_text` | `#ffffff` | `#0b1220` | `#ffffff` / `#101418` | Text and glyphs on accent. | CHANGE |
| status_neutral | `status_neutral` | `#4d5873` | `#a9b4d0` | `#53585f` / `#b3bac2` | Queued, Cancelled, Not evaluated, Not transcribed. | CHANGE |
| status_running | `status_running` | `#0a4fe0` | `#6ea8ff` | `#1a4fa0` / `#8fb4f2` | Running locally, Importing, Transcribing, Verifying. | CHANGE |
| status_waiting | `status_waiting` | `#7a4a00` | `#e3b667` | same | Waiting for you, network, model, source, authorization. | EXISTS |
| status_success | `status_success` | `#0f6b3f` | `#67c391` | same | Completed, Imported, Local only, anchor resolved. | EXISTS |
| status_warning | `status_warning` | `#8a4b00` | `#e8a35c` | same | Stale, Experimental, Cloud outcome unknown, network allowed. | EXISTS |
| status_danger | `status_danger` | `#a1121f` | `#f08a92` | same | Failed, Unsupported, Refused, destructive confirm. | EXISTS |
| focus_ring | `focus_ring` | `#0b57ff` | `#8fc2ff` | `#0b57d0` / `#9ec4ff` | `widgets.active.bg_stroke` at `tokens.focus.width`. The light value is brand royal. | CHANGE |
| overlay | `overlay` | `rgba(11,18,32,.45)` | `rgba(0,0,0,.60)` | `rgba(16,18,20,.45)` / `rgba(0,0,0,.6)` | Modal backdrop, `Modal::new(id).backdrop_color(overlay)`. | CHANGE |
| accent_hover | **NEW** `accent_hover` | `#0841b8` | `#8cbaff` | not present | Primary button hovered and pressed fill. | PROPOSED |
| wave_remaining | **NEW** `wave_remaining` | `#7c869e` | `#66739a` | not present | Waveform columns after the playhead. | PROPOSED |
| tint_neutral | **NEW** `tint_neutral` | `#edeef1` | `#293346` | not present | Badge fill behind `status_neutral` text. | PROPOSED |
| tint_running | **NEW** `tint_running` | `#e6edfc` | `#20314e` | not present | Badge fill behind `status_running` text; speaker region A. | PROPOSED |
| tint_waiting | **NEW** `tint_waiting` | `#f2ede6` | `#333335` | not present | Badge fill behind `status_waiting` text; speaker region B. | PROPOSED |
| tint_success | **NEW** `tint_success` | `#e7f0ec` | `#1f353c` | not present | Badge fill behind `status_success` text, Local only badge. | PROPOSED |
| tint_warning | **NEW** `tint_warning` | `#f3ede6` | `#333034` | not present | Badge fill behind `status_warning` text. | PROPOSED |
| tint_danger | **NEW** `tint_danger` | `#f6e7e9` | `#352c3c` | not present | Badge fill behind `status_danger` text, error notice. | PROPOSED |

Twenty roles exist, eight are new, so `ColorRoles` goes from 20 fields to 28.

### 1.1 What the eight new roles touch in `tokens.rs`

Nothing is generated, so each of these is a hand edit in the same file:

- the `ColorRoles` struct and `ColorRoles::get`;
- the `ColorRole` enum, `ColorRole::ALL` (typed `[Self; 20]` today, so the length changes) and
  `ColorRole::css_name`;
- the four constants `LIGHT`, `DARK`, `HIGH_CONTRAST_LIGHT`, `HIGH_CONTRAST_DARK`. A struct literal
  without every field does not compile, so the two HC constants need values too.

**The boards give no high-contrast value for any of the eight new roles.** "HC_LIGHT and HC_DARK
stay as they are" only covers the twenty existing roles. The implementer has to pick HC values for
`accent_hover`, `wave_remaining` and the six `tint_*` roles and defend them with the contrast tests
below. A sensible starting point (my suggestion, not from the boards): in HC the tint roles can equal
`bg_surface` so a badge is text on the surface with a visible border, `wave_remaining` can equal
`text_secondary`, and `accent_hover` can equal `accent`. Measure whichever you choose.

The existing tests in `tokens.rs` (`every_theme_defines_every_role`,
`text_meets_four_and_a_half_to_one_on_every_background_in_every_theme`, and friends) iterate
`ThemeKind::ALL` and `ColorRole::ALL`, so they will cover the new roles in all four themes as soon
as the constants exist. They do not yet assert the tint pairs in section 6; add those.

### 1.2 Two pairs that look the same

`status_waiting` and `status_warning` differ by 16 in the red channel in light (`#7a4a00` against
`#8a4b00`) and `tint_waiting` and `tint_warning` differ by one bit (`#f2ede6` against `#f3ede6`). The
dark pair is `#333335` against `#333034`. They are separate roles because the app has separate
Waiting and Warning tones, and they are unchanged from today. They are not distinguishable by hue in
practice, which is fine: the glyph and the word carry the meaning, as the product rule demands.

## 2. Overlay and shadow

| Item | Light | Dark | Notes |
| --- | --- | --- | --- |
| `overlay` | `rgba(11,18,32,.45)` | `rgba(0,0,0,.60)` | The only role with alpha. As `Color32` premultiplied that is about `(5, 8, 14, 115)` light and `(0, 0, 0, 153)` dark. The byte values are my arithmetic (`round(alpha * 255)`), the boards give decimals. |
| Shadow (Modal and popups) | `rgba(23,37,68,0.16)` | `rgba(0,0,0,0.5)` | `window_shadow` and `popup_shadow` = `Shadow` offset `[0, 8]`, blur 24, spread 0. `style_for` in `crates/ui/src/theme/apply.rs` sets neither today (I checked: no assignment to either field), so egui's defaults apply. |

The Tokens board says shadow is for "Modal and popups only". The component sheet and the Library
board draw the same shadow under the ComboBox popup, the row context menu and toasts. Those are
popups, so there is no conflict, but read it as "anything that floats".

## 3. Spacing

`SPACE[0..=8]` = 0, 2, 4, 8, 12, 16, 24, 32, 48 pt. Unchanged from `tokens.rs`. EXISTS.

| Style slot | Value | Repo now |
| --- | --- | --- |
| `item_spacing` | 8 by 4 | 8 by 4 (EXISTS) |
| `button_padding` | 12 by 4 | `SPACE[4]` by `SPACE[2]`, which is 12 by 4 (EXISTS) |
| `window_margin` | 16 | 16 (EXISTS) |
| `indent` | 24 | 24 (EXISTS) |
| Main region padding | `space(6)` = 24 | The boards actually measure 24 horizontal and 20 vertical on Library, Jobs, Settings and Devices. 20 is not on the scale. |

**Off-scale values in the boards.** The boards use 6, 10, 14, 18, 20 and 22 in paddings and gaps
(for example the Recording detail main column is `14 18 14 22`, card padding is often `10 12` or
`12 14`). The design does not claim these are tokens. When a screen spec in this folder quotes a
measurement I give it as measured and mark it "(off-scale)" if it is not in `SPACE`. Round to the
nearest `SPACE` step unless a screenshot comparison says otherwise; the egui layout will not match
CSS to the pixel anyway.

## 4. Radius, type, motion, focus, sizes

### Radius

| Token | Pt | Used for |
| --- | --- | --- |
| `radius.small` | 3 | badges, the two-letter tiles in the rail, small chips |
| `radius.medium` | 6 | controls: buttons, fields, ComboBox, menu items container |
| `radius.large` | 10 | cards, Modal, the main table container |
| pill | 13 | Jobs filter chips (`CornerRadius 13`, from the Jobs mapping strip) |

Also seen: the Activity count badge is an 18 pt circle (radius 9); avatars in People are 28 pt
circles (radius 14); progress bars use radius 3 on the track.

### Type scale

| Token | Pt | Weight in the boards | Use |
| --- | --- | --- | --- |
| `xxl` | 24 | 700 | Screen title |
| `xl` | 19 | 600 or 700 | Heading, Modal title |
| `lg` | 15 | 600 | Section heading |
| `md` | 13 | 400 | Body and buttons, line height 1.5 |
| `sm` | 12 | 400 or 600 | Column headers, facts, captions |
| `xs` | 11 | 400 or 700 | Shortcut hints, badge tags |
| `mono` | 13 | 400 or 500 | `0:01:07`, content hashes. The boards also set mono at 12, 11 and 10 in places (table durations, chips, ruler ticks) |

The line heights in `tokens.rs` are `line_height_tight` 1.25 and `line_height_normal` 1.5. The boards
state 1.5 for body and never draw 1.25. EXISTS for the scale itself.

**Font stack.** Proportional is `Inter, system-ui, sans-serif`. Monospace is
`'Source Code Pro', ui-monospace, monospace`. The boards load Inter at 400, 500, 600 and 700 and
Source Code Pro at 400 and 500. The Tokens board proposes Inter through `FontDefinitions`, Regular
and SemiBold TTF only, one row in the licence register, and says the layout holds with egui's
built-in face (Ubuntu-Light proportional, Hack monospace, from `epaint_default_fonts`) as well. That
is **NEW DEP** (font files and a licence row in `docs/licensing/third-party-register.md`). With only
two Inter weights, the 700 weight in the boards collapses onto SemiBold.

### Motion

| Token | Duration | Use |
| --- | --- | --- |
| `motion.fast` | 90 ms | `animation_time` |
| `motion.normal` | 160 ms | aside open and close |

EXISTS (`MOTION` in `tokens.rs` holds 0.09 and 0.16). The only continuous animation anywhere is a
spinner while work runs. Everything else goes through `animate_bool`.

### Focus

`focus_ring` role, 2 pt wide, 2 pt offset, 3 pt wide in high contrast. EXISTS as `FocusRing`
geometry. The board render is `box-shadow: 0 0 0 2px <surface>, 0 0 0 4px <focus_ring>`: a 2 pt gap
in the surrounding surface colour, then the 2 pt ring. Today the ring is `widgets.active.bg_stroke`,
which sits on the widget edge. The component sheet proposes painting it 2 pt outside the rect
(`rect_stroke` on `rect.expand(4)`) so it never lands on an accent fill (PROPOSED). I measured why:
`focus_ring` on `accent` is 1.19:1 light and 1.30:1 dark, so a ring painted on a primary button
vanishes.

### Sizes

| Item | Value | Source |
| --- | --- | --- |
| `MIN_TARGET_SIZE` | 24 pt, `interact_size.y` | Tokens board; EXISTS |
| Control height on screens | 28 pt (secondary and compact controls 24 to 26) | Component sheet |
| Window | 1280 by 820 default, minimum 880 by 560 | EXISTS (`viewport()` in `apps/plaudon-egui/src/lib.rs`) |
| Compact rail | below 1040 pt | EXISTS (`COMPACT_LAYOUT_WIDTH`) |
| Menu bar | 28 | Every screen board |
| Toolbar | 44 | Every screen board and the Library mapping strip |
| Status bar | 26 | Library mapping strip |
| Nav rail | 216 wide, rows 32 high, glyph tile 20 | Library board. Repo default is 208 (`DEFAULT_SIDEBAR_WIDTH`) |
| Collapsed rail | 52 on the component sheet | Repo `COLLAPSED_WIDTH` is 56 |
| Table row | 32 | Library and component sheet. Repo `ROW_HEIGHT` is 28 |
| Table header | 32 on the Library board, 30 on the component sheet | disagreement, see section 7 |

## 5. Token to egui slot

The Tokens board's "Usage and egui slot" column, regrouped by egui field. `style_for` in
`crates/ui/src/theme/apply.rs` already wires most of these to the same slots, so changing the
constants moves them without touching `apply.rs`. What `apply.rs` does not do is listed last.

| egui field | Token |
| --- | --- |
| `visuals.panel_fill` | `bg_canvas` |
| `visuals.extreme_bg_color`, `widgets.inactive` fill | `bg_surface` |
| `visuals.window_fill` | `bg_raised` |
| `visuals.selection.bg_fill`, `widgets.active` fill | `bg_selected` |
| `widgets.hovered` fill, `faint_bg_color`, `code_bg_color` | `bg_hover` |
| inactive and hovered `fg_stroke` | `text_primary` |
| noninteractive `fg_stroke` | `text_secondary` |
| `visuals.hyperlink_color` | `accent` |
| `widgets.hovered.bg_stroke` | `border_strong` |
| `widgets.active.bg_stroke` | `focus_ring` at `tokens.focus.width` |
| `window_shadow`, `popup_shadow` | shadow in section 2. NOT set by `apply.rs` today |
| `Modal::backdrop_color` | `overlay`. Not called anywhere in the repo today, and I could not compile against egui 0.36 here, so confirm the method exists when you wire it |

Not wired by `apply.rs` and needed by the design: `accent_hover` (primary hover and pressed),
every `tint_*` (badge frames), `wave_remaining` (waveform, slider track), `text_inverse` on a danger
fill. These are read through `Tokens::get(ctx)` in widget code, not through egui visuals.

## 6. Measured contrast

Copied from the Tokens board. WCAG 2.x formula, the same one as `tokens::contrast_ratio`. I
recomputed all 34 rows with a script over the hex values above; every number matches to two
decimals. All rows pass.

| Pair | Needs | Light | Dark |
| --- | --- | --- | --- |
| text_primary / bg_canvas | 4.5 | 14.02 | 17.30 |
| text_primary / bg_surface | 4.5 | 15.17 | 16.06 |
| text_primary / bg_raised | 4.5 | 15.17 | 14.48 |
| text_primary / bg_selected | 4.5 | 12.68 | 11.31 |
| text_primary / bg_hover | 4.5 | 13.38 | 14.03 |
| text_secondary / bg_canvas | 4.5 | 6.56 | 9.03 |
| text_secondary / bg_surface | 4.5 | 7.10 | 8.38 |
| text_secondary / bg_raised | 4.5 | 7.10 | 7.55 |
| text_secondary / bg_selected | 4.5 | 5.93 | 5.91 |
| text_secondary / bg_hover | 4.5 | 6.26 | 7.32 |
| accent (link) / bg_surface | 4.5 | 6.55 | 7.21 |
| accent (link) / bg_canvas | 4.5 | 6.05 | 7.76 |
| accent_text / accent | 4.5 | 6.55 | 7.76 |
| accent_text / accent_hover | 4.5 | 8.54 | 9.44 |
| status_neutral / bg_surface | 4.5 | 7.10 | 8.38 |
| status_running / bg_surface | 4.5 | 6.55 | 7.21 |
| status_waiting / bg_surface | 4.5 | 7.48 | 9.23 |
| status_success / bg_surface | 4.5 | 6.57 | 8.11 |
| status_warning / bg_surface | 4.5 | 6.80 | 8.13 |
| status_danger / bg_surface | 4.5 | 8.01 | 7.25 |
| status_running / tint_running | 4.5 | 5.58 | 5.41 |
| status_success / tint_success | 4.5 | 5.66 | 6.00 |
| status_warning / tint_warning | 4.5 | 5.85 | 6.09 |
| status_danger / tint_danger | 4.5 | 6.68 | 5.55 |
| status_waiting / tint_waiting | 4.5 | 6.42 | 6.70 |
| status_neutral / tint_neutral | 4.5 | 6.12 | 6.11 |
| border_strong / bg_surface | 3 | 4.55 | 4.05 |
| border_strong / bg_raised | 3 | 4.55 | 3.65 |
| border_strong / bg_canvas | 3 | 4.20 | 4.36 |
| focus_ring / bg_surface | 3 | 5.50 | 9.38 |
| focus_ring / bg_raised | 3 | 5.50 | 8.45 |
| focus_ring / bg_canvas | 3 | 5.08 | 10.10 |
| wave_remaining / bg_surface | 3 | 3.65 | 3.71 |
| accent (wave played) / bg_surface | 3 | 6.55 | 7.21 |

The board's footnote: `border_subtle` is decorative (1.34 and 1.38) and never the only edge of a
control. Disabled controls are exempt under WCAG but still draw `text_secondary` at 0.6 alpha so
they read as present.

### 6.1 Extra pairs I measured for this spec

The board does not list these, and the screens use them. Same script, same formula. **Bold** rows
fail or come close.

| Pair | Needs | Light | Dark | Where it matters |
| --- | --- | --- | --- | --- |
| text_inverse / status_danger | 4.5 | 8.01 | 7.81 | Delete permanently button. The component sheet quotes both numbers; they reproduce |
| accent (link) / bg_selected | 4.5 | 5.47 | 5.08 | Link text in a selected row |
| accent (link) / bg_hover | 4.5 | 5.78 | 6.30 | Link text in a hovered row |
| status_neutral / bg_selected | 4.5 | 5.93 | 5.91 | Status words in the selected Library row |
| status_running / bg_selected | 4.5 | 5.47 | 5.08 | same |
| status_waiting / bg_selected | 4.5 | 6.26 | 6.50 | same |
| status_success / bg_selected | 4.5 | 5.50 | 5.71 | same |
| status_warning / bg_selected | 4.5 | 5.69 | 5.73 | same |
| status_danger / bg_selected | 4.5 | 6.70 | 5.11 | same |
| status_* / bg_hover (six pairs) | 4.5 | 5.78 to 7.07 | 6.30 to 8.07 | hovered rows; lowest is status_running light and status_running dark |
| status_* / bg_raised (six pairs) | 4.5 | 6.55 to 8.01 | 6.50 to 8.32 | toasts and the palette |
| text_primary / tint_warning | 4.5 | 13.05 | 12.03 | find highlight in the transcript |
| **border_strong / bg_selected** | 3 | 3.80 | **2.85** | selected Jobs filter chip in dark. The chip also touches `bg_canvas` (4.36), so the edge is still visible, but it does not clear 3:1 against its own fill |
| border_strong / bg_hover | 3 | 4.01 | 3.54 | hovered text field |
| **text_primary / accent** | 4.5 | **2.32** | **2.23** | fails |
| **text_primary / accent_hover** | 4.5 | **1.78** | **1.83** | fails |
| **text_secondary / accent** | 4.5 | **1.08** | **1.16** | fails |
| **focus_ring / accent** | 3 | **1.19** | **1.30** | fails, which is why the ring is painted 2 pt outside the rect |

The three failing text pairs are not a token problem. They are a layout rule: **never put
`text_primary` or `text_secondary` over an accent fill.** The indeterminate progress bar on the
component sheet and on the Jobs board does exactly that: its label ("Preflight: progress not
reported") is `text_primary` and runs across the accent segment, so the part of the text that
crosses the fill is unreadable. See `COMPONENTS.md` section on progress for the fix.

## 7. Where the boards disagree

I list these rather than pick silently. Each is repeated in `GAP_ANALYSIS.md` with a recommendation.

1. **Table header height.** 32 on the Library board (mapping strip says `header(32)`), 30 on the
   component sheet ("row height 32, header 30"). Row height is 32 in both.
2. **Disabled alpha.** The Tokens board says disabled text draws `text_secondary` at 0.6 alpha. The
   component sheet draws disabled buttons at 0.5 opacity (and the disabled text input at 0.6), and
   the error-state mapping strip says "Disabled controls draw at 0.5 alpha". Pick 0.5 for buttons
   (boards: component sheet, error states, Jobs Retry job) and 0.6 for text inputs, or one value
   everywhere; WCAG exempts disabled controls either way.
3. **Modal title size.** The Tokens board says `xl 19` is "Heading, Modal title". The Consent
   board's `h2` is 19 bold, but the three dialogs on the component sheet use 15 bold.
4. **Rail mark size.** The Brand board text says 24 pt, every screen board draws the rail mark at 26
   high, the Brand mapping says `fit_to_exact_size(vec2(24.0, 28.0))`, and the compact rail mark on
   the component sheet is 20 high. 24 by 28 matches the mark's aspect ratio (the SVG viewBox is
   650 by 763).
5. **Collapsed rail width.** 52 on the component sheet, 56 in `nav::COLLAPSED_WIDTH`.

## 8. Brand colours the UI may and may not use

From the Brand board. The cyan family lives inside the mark only. It never carries text, a status, a
focus ring or a waveform.

| Colour | Hex | On white | Verdict |
| --- | --- | --- | --- |
| Highlight | `#46c9fc` | 1.91:1 | fail, mark only |
| Sky | `#56c9ff` | 1.88:1 | fail |
| Teal | `#17c7c7` | 2.09:1 | fail |
| Azure | `#2f86ff` | 3.51:1 | UI only (non-text) |
| Royal | `#0b57ff` | 5.50:1 | pass. Becomes the light `focus_ring` |
| Ink | `#172544` | 15.17:1 | pass. Becomes the light `text_primary` |

Mark palettes: light body `#0585fc` to `#0029b0`, highlight `#46c9fc`, fold `#04185a`. Dark body
`#5cb4ff` to `#5470ee`, fold `#3a58cc` (the SVG's own dark stops, so the fold does not sink into the
canvas). Monochrome blue `#0b57ff` (light, 5.50:1 on white), `#6ea8ff` (dark, 7.76:1 on the dark
canvas), white on royal 5.50:1. The accent is a slightly deeper `#0a4fe0` than royal so white text
on it clears 6.5:1 (it measures 6.55). See [`screens/brand.md`](screens/brand.md).
