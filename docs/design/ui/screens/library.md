# Library

The destination a user lands on: every recording in a searchable, filterable, sortable table.
Source board `source/Library.dc.html`. Pictures: [light](library-light.png) (1280 by 1100, the window
is the top 820 and the bottom 280 is the board's own egui mapping strip) and
[dark](library-dark.png) (1280 by 820, window only, same layout).

Shared chrome (menu bar, toolbar, rail, status bar, activity bell) is specified in
[`../COMPONENTS.md`](../COMPONENTS.md) section 13 and is not repeated. Token names are those in
[`../TOKENS.md`](../TOKENS.md). Tags: EXISTS, CHANGE, PROPOSED, NEW DEP (see
[`../README.md`](../README.md)).

State drawn on the board: a populated library with 100 of 1,284 rows loaded, two rows selected, the
search field focused with the text "sync", the row context menu open on the fourth row, and
"Activity" showing 2 unread. The same board also shows one row in each of the row states (hovered,
selected, selected with the keyboard cursor).

Code: `crates/ui/src/screens/library/` (`screen.rs`, `toolbar.rs`, `table.rs`, `status.rs`,
`list.rs`, `query.rs`, `selection.rs`, `format.rs`). Route `Route::Library`, destination
`Destination::Library`, `Ctrl+1`.

## 1. Regions, top to bottom

The window is 1280 by 820: menu bar 28, toolbar 44, body 722, status bar 26. The main region is
the body minus the 216 rail. It pads 20 vertical and 24 horizontal (the vertical 20 is off-scale)
and stacks its children with gap 14.

### 1.1 Title row

| Element | Spec | Token |
| --- | --- | --- |
| `h1` "Recordings" | `xxl` 24, weight 700 | `text_primary` |
| Count line | `sm` 12, "Showing 100 of 1,284 recordings (snapshot at query time)" | `text_secondary` |
| Selection group (right, `role=status`) | "2 selected" `md` 13 weight 600, then primary **Transcribe selected** (28), then secondary **Clear selection** (28), gap 8, baseline-aligned with the title block | `text_primary`, `accent`, `bg_surface` |

The group is a `role=status` region, so its text is announced when the selection changes. The board
only draws the "something selected" state. What the group shows with nothing selected is not
specified; the repo's status line says "No recordings selected. {n} loaded." and the sensible reading
is to hide the buttons and keep that sentence.

### 1.2 Filter bar

A card: `bg_surface`, 1 pt `border_subtle`, `radius.large` 10, padding 10 by 12, internal gap 8.

Row one (wraps, gap 10): the label "Search recordings" (`sm` `text_secondary`) beside a 230 by 26 text
field (shown focused, 2 pt `focus_ring`, text "sync" with a caret); then four labelled combo boxes
at minimum 150 by 26, each with the label to its left:

| Label | Closed text |
| --- | --- |
| Import status | All import states |
| Transcription status | All transcription states |
| Source | All sources |
| Tag | All tags |

Row two (gap 10): "From date (YYYY-MM-DD)" with a 110 by 26 field showing "2026-03-12"; "To date (YYYY-MM-DD)"
with the same; a link button **Clear filters** (26 high, `accent`, underlined); a spacer; secondary
**Search across all recordings** (26 high) at the right.

Behaviour: hint text "Title contains…", 200 ms debounce on typing (as today,
`SEARCH_DEBOUNCE`), page size 100. Field edges use `border_strong` (4.55:1 light).

### 1.3 Table card

A card: `bg_surface`, 1 pt `border_subtle`, `radius.large` 10, clips its content, fills the rest of
the height, `role=grid` with `aria-rowcount` 1284. Inside:

- **Header**, 32 high, padding 0 8, `sm` 12 weight 600 `text_secondary`, bottom border `border_subtle`.
  Nine columns, grid template `1.7fr 1.25fr 1.25fr 0.62fr 0.95fr 1fr 1.25fr 0.8fr 0.8fr`:
  **Title ▼** (the sorted column, `text_primary`), Recorded, Imported, Duration, Source, Import
  status, Transcription, Engine, Tags. Cell padding 0 8.
- **Rows**, 32 high, `md` 13, same grid, bottom border 1 pt `bg_hover`. Title truncates with an
  ellipsis. Recorded, Imported, Source, Engine and Tags are `text_secondary`; Duration is mono 12
  (`text_primary`, or `text_secondary` when the value is "Unknown"). The two status columns draw a
  14 pt wide glyph slot (weight 700) then the word, both in the status colour.
- **Footer**, 40 high, centred secondary button **Load more recordings** (28), top border
  `border_subtle`.

Row states (fills and focus are in `COMPONENTS.md` section 5):

| Row | State on the board |
| --- | --- |
| Weekly sync, platform team | default |
| Sync notes, design review | hovered (`bg_hover`) |
| Lecture 7, signal processing | selected and keyboard cursor (`bg_selected`, inset 2 pt `focus_ring`, title weight 600) |
| Voice memo | selected (`bg_selected`, title weight 600) |

Sample rows (so the data shapes are clear; these are the board's fixtures, not copy to ship):

| Title | Recorded | Imported | Duration | Source | Import | Transcription | Engine | Tags |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Weekly sync, platform team | 6 Oct 2026, 09:02 UTC | 6 Oct 2026, 11:40 UTC | 48:12 | Watched folder | ✓ Imported | ✓ Transcribed | whisper.cpp | work, weekly |
| Sync notes, design review | 5 Oct 2026, 14:30 UTC | 5 Oct 2026, 15:02 UTC | 31:47 | Manual import | ✓ Imported | ⟳ Transcribing | whisper.cpp | work |
| Interview, research participant 4 | 3 Oct 2026, 10:15 UTC | 3 Oct 2026, 10:51 UTC | 1:02:09 | Manual import | ✓ Imported | ! Stale | whisper.cpp | research |
| Lecture 7, signal processing | 2 Oct 2026, 13:00 UTC | 2 Oct 2026, 16:20 UTC | 1:21:30 | Watched folder | ✓ Imported | ◷ Queued | Not set | course |
| Voice memo | Unknown | 1 Oct 2026, 08:04 UTC | Unknown | Manual import | ✓ Imported | – Not transcribed | Not set | None |
| Kitchen renovation call | 29 Sep 2026, 17:45 UTC | 30 Sep 2026, 09:10 UTC | 22:05 | Watched folder | ✕ Import failed | – Not transcribed | Not set | home |
| Podcast draft, episode 12 | 25 Sep 2026, 11:20 UTC | 25 Sep 2026, 12:00 UTC | 38:51 | Manual import | ⟳ Importing | – Not transcribed | Not set | None |
| Field recording, river | 20 Sep 2026, 06:40 UTC | 21 Sep 2026, 10:02 UTC | 9:12 | Manual import | ◷ Pending | – Not transcribed | Not set | None |

Unknown facts print "Unknown" (never "0:00" or the epoch). Dates print in UTC with the zone named.
Both already hold in `format.rs`.

Status tones on the board: Imported and Transcribed are `status_success`; Importing and
Transcribing are `status_running`; **Pending and Queued are `status_neutral`** (the board maps its
`wait` class to the neutral colour); Import failed and Transcription failed are `status_danger`;
Not transcribed is `status_neutral`; Stale is `status_warning`.

### 1.4 Row context menu

Opened on a row (the board opens it at 230 from the left, 186 from the top of the card).

| Property | Value |
| --- | --- |
| Frame | `bg_raised`, 1 pt `border_subtle`, `radius.medium` 6, popup shadow, padding 4, width 230 |
| Item | 28 high, padding 0 10, radius 3, `md` 13 |
| Hovered item | `bg_hover` |
| Separator | 1 pt `border_subtle`, margin 4 6 |

Items on the board, in order: **Open recording**, **Remove from selection**, **Transcribe selected**,
separator, **Copy recording identities**. The code also has "Select all loaded recordings" and
"Clear selection" in this menu; the board shows a subset because the menu is cut short. Keep all of
them.

### 1.5 Status bar

26 high. Left: "Ready". Right: "2 tasks running", then the Local-only badge (`✓ Local only`,
`status_success` on `tint_success`). The word "PROPOSED" beside it on the board is an annotation,
not UI.

## 2. egui mapping (from the board's strip, corrected to the repo)

| Part | Build with |
| --- | --- |
| Menu bar | `egui::Panel::top("plaudon-menu-bar")` from `menu_model()` (EXISTS) |
| Toolbar | `egui::Panel::top("plaudon-toolbar")`, `toolbar::fit` moves overflow into "More actions" (EXISTS) |
| Status bar | `egui::Panel::bottom("plaudon-status-bar")`, `Role::Status` (EXISTS) |
| Filter bar | `Frame::new().fill(bg_surface).stroke(border_subtle).corner_radius(radius.large)` around `ui.horizontal_wrapped` |
| Search field | `TextEdit::singleline` with `hint_text("Title contains…")`, 200 ms debounce as today |
| Filters | `ComboBox::from_label` per filter. Edges use `border_strong` |
| Clear filters | `Button::new(..).frame(false)` in `accent` |
| Table | `egui_extras::TableBuilder`, `header(32)`, `body.rows(32, total, ..)` |
| Row fill | `painter().rect_filled`: `bg_selected` for selected, `bg_hover` for hovered; focused row adds `rect_stroke(focus_ring, 2 pt)` |
| Status cells | a painted glyph plus the word in the `status_*` colour |
| Context menu | `response.context_menu()` on the row |

## 3. Keyboard map (as built; must not change)

Up, Down, Home, End move the cursor; Enter opens the row; Space toggles selection; Shift extends;
`Ctrl+A` selects the loaded rows; Escape and Tab leave the grid. The grid is a single tab stop and
reports the active row as the active descendant. Global: `Ctrl+1` to `Ctrl+7`, `Ctrl+K`, `Ctrl+O`
(import), `Ctrl+R` (refresh), `Ctrl+Shift+F` (cross-recording search). Typing wins: while a text
field has focus only Escape is read.

## 4. Tagged lists

### EXISTS (the app does this today)

- The filter bar with search, four filters, two date fields, Clear filters, Search across all
  recordings, in the same two rows (`toolbar.rs`).
- Debounced search, page size 100, Load more, count line text "Showing {n} of {total} recordings
  (snapshot at query time)".
- The virtualised table: single tab stop, `Role::Grid`, rows with label, index and selected state,
  sort direction on the active column only, `row_label()` announcement.
- Selection model and keyboard (`selection.rs`), context menu (Open recording, Add or Remove from
  selection, Select all loaded recordings, Clear selection, Transcribe selected, Copy recording
  identities).
- "Unknown" for unmeasured duration and recorded-at.
- Status text and glyphs in the two status columns (`status.rs`, `StatusDescriptor`).

### CHANGE (restyled or rearranged)

- Row height 28 to 32, header 24 to 32; drop `.striped(true)`; paint hairline row separators.
- Unsorted sortable headers show "↕" today; the board shows an arrow only on the sorted column.
- Status colour: `StatusTone::color` (`status.rs`) reads egui `Visuals`, so Success and Running
  render in the ordinary text colour. Make it read `Tokens` and the tint roles.
- Pending and Queued: `StatusTone::Waiting` today (drawn as weak text); the board draws them with
  `status_neutral`. Same pixels in practice, but map them on purpose.
- Glyphs: ✓ ⟳ ◷ ✕ render as empty boxes in committed snapshots (`library_100_rows.png`). Draw them
  with the painter.
- Title block: the board puts "Recordings" and the count line above the filter bar. Today the screen
  has no `h1`, the toolbar title says "Library", and the count line sits in the footer below the
  table (`RecordingTable::footer`).
- Filter bar goes inside a bordered card (no frame today); the date placeholder and Clear filters
  placement already match.
- Count formatting: "1,284" with a thousands separator on the board, plain `{}` in `footer()`.
- Tenth column "Last error" (remainder width) exists in `columns()`; the board has nine columns. Keep
  the column (a failed import needs to say why) and give it the leftover width, or surface the message
  in a tooltip on the status cell; the board does not decide.
- "Load more recordings" moves from a bare button after the count line into the table card footer.

### PROPOSED (new behaviour the board suggests)

- Selection group at the right of the title row with visible **Transcribe selected** and **Clear
  selection** buttons. Today those actions exist only in the context menu and the status line; the
  board does not tag this PROPOSED but the code has no such strip.
- Status-bar Local-only badge fed by the egress gate's enforced policy (tagged PROPOSED on the board).
- Activity bell count as a pill (see `COMPONENTS.md` section 13).
- Painted status glyphs (tagged PROPOSED on the board).

### NEW DEP

None for this screen.

## 5. Accessibility notes

- Status is a glyph and a word in every row and in the status bar. Do not drop the word to save width;
  truncate other columns first.
- The count line and the selection group are polite live regions. Keep `Role::Status` and
  `Live::Polite` (`status_line` in `table.rs`).
- Every control keeps its AccessKit name; the Activity button's name states the count in words.
- Focus ring on the keyboard-cursor row is inset 2 pt `focus_ring`; measure it on `bg_selected`:
  4.60:1 light, 6.61:1 dark.

## 6. Contrast on this screen

Text on `bg_selected` is 12.68:1 light and 11.31:1 dark. Status words on a selected row are all at
least 5.08:1 (lowest: `status_running` dark). Link text on a hovered row is 5.78 and 6.30. Full
list in `TOKENS.md` section 6.1.

## 7. Snapshots that move

`crates/ui/tests/visual/library_100_rows.png`, `library_empty.png` (via `library_screen.rs`),
`shell-ready.png` (`shell_kittest.rs`), the three `scale-library-100-rows@*.png` and
`scale-shell-ready@*.png` (`scale_kittest.rs`). All 13 committed baselines are listed in
[`../REVISION_PLAYBOOK.md`](../REVISION_PLAYBOOK.md).
