# Library, empty and importing

First-run library with no recordings, with an import in progress and two toasts. Source board
`source/LibraryEmpty.dc.html`. Pictures: [light](library-empty-light.png) (1280 by 1100) and
[dark](library-empty-dark.png) (1280 by 820).

Shared chrome is in [`../COMPONENTS.md`](../COMPONENTS.md) section 13. Tokens in
[`../TOKENS.md`](../TOKENS.md).

Code: `LibraryScreen::empty_state` and `import::drop_overlay` and `import::import_results_panel` in
`crates/ui/src/screens/library/` (`mod.rs`, `import.rs`), toasts in `crates/ui/src/widgets/toast.rs`.

## 1. What the board draws, and what it merges

The board draws three things at once that are three separate states in the app:

1. The resting empty library ("Your library is empty.", the card, the two buttons).
2. The drag-over state (accent dashed border round the card, the "Drop audio files to import" pill).
   The Library mapping strip says these two appear only "while `ctx.input(|i|
   !i.raw.hovered_files.is_empty())`".
3. An import in flight with two toasts on top.

**The resting border of the card is not specified.** The board only draws the 2 pt dashed `accent`
border, which per its own strip belongs to the drag-over state. Resting suggestion (mine, not the
board's): 1 pt `border_subtle`, no pill, no dashed line. Decide once and snapshot both.

## 2. Regions

Window 1280 by 820. The toolbar shows **Activity** with no count and no pill (nothing unread; the
AccessKit name is "Activity: no unread notifications"). Title "Library". Status bar: "Importing 3
file(s)…" on the left, "1 task running", then the Local-only badge.

Main region, padding 20 vertical, 24 horizontal, gap 14:

| Element | Spec | Token |
| --- | --- | --- |
| `h1` "Recordings" | `xxl` 24, weight 700 | `text_primary` |
| Sub line | `sm` 12, "Your library is empty." | `text_secondary` |
| Card | fills the remaining height, 2 pt dashed `accent` border (drag state), `radius.large` 10, fill `bg_surface`; content centred, gap 16 | `accent`, `bg_surface` |
| Mark | monochrome blue mark, 56 high (`plaudon-mono-blue-light.svg` or `-dark.svg`) | brand |
| `h2` "No recordings" | `xl` 19, weight 600 | `text_primary` |
| Paragraph | max width 560, centred, `md` 13, line height 1.5 | `text_secondary` |
| Buttons (gap 8) | primary **Import an audio file** (32 high, padding 0 14, shown keyboard-focused with the 2 + 2 ring), secondary **Connect PLAUD** (32 high) | `accent`, `border_strong` |
| Drop pill | margin-top 18, padding 8 by 14, radius 6, "⤓ Drop audio files to import", `md` 13 weight 600 | text `status_running` on `tint_running` |
| Copy note | `sm` 12, centred | `text_secondary` |

Copy, exact:

- Paragraph: "Import audio from this computer to get started. Direct PLAUD device import is not part of
  this build; Settings lists the sources that are supported."
- Copy note: "An import copies your file. It never moves, changes or deletes the original." (PROPOSED:
  new copy, not in the app today.)
- Pill: "⤓ Drop audio files to import".

## 3. Toasts (bottom right of the main region)

A stack with gap 8, 340 wide, 24 from the right edge and 20 from the bottom of the main region,
`role=status`, `aria-live=polite`. Each toast: `bg_raised`, 1 pt `border_subtle`, `radius.large` 10,
popup shadow, padding 12 by 14.

| Toast | Tone tile | Title | Body | Extras |
| --- | --- | --- | --- | --- |
| 1, in progress | `i` on `tint_running`, `status_running` | Importing files | Importing 3 files. | Dismiss at the right of the title row; indeterminate progress bar, 6 high, radius 3 |
| 2, finished | `!` on `tint_warning`, `status_warning` | Import finished with decisions to make | 2 imported, 1 duplicate | secondary button **Open the recording you already have** (26 high) and a ghost **Dismiss** |

Both strings come from `describe_batch` in `import.rs` ("Import finished", "Import finished with
decisions to make"; body "2 imported, 1 duplicate."). The first toast's text "Importing files" is
the existing progress label. Manual dismiss only, never takes focus. Details in `COMPONENTS.md`
section 9.

The toast stack sits on top of the copy note in the picture (it hides "...or deletes the original"
in the light PNG). That is a flaw of the mock layout, not a requirement. In the app, anchor the
stack so it cannot cover the card's text, or let the card avoid the stack's rectangle.

## 4. Behaviour

- Drop: on `i.raw.dropped_files` the files go through `dragdrop::classify` exactly as today. Hover:
  draw the dashed line round the main rect with `Shape::dashed_line` in `accent`, and the pill in
  `tint_running`.
- **Import an audio file** raises the host file picker (`LibraryIntent::PickFiles`).
- **Connect PLAUD.** The board's strip says it "opens the setup wizard with its three honest
  options; the direct and cloud routes read Unsupported there". In the code it navigates to
  `Route::Settings` (`LibraryIntent::OpenSettings`). The Devices destination owns the wizard
  (`wizard::setup_wizard`, opened by "Add PLAUD device"). The board and the code disagree; see
  `GAP_ANALYSIS.md`.
- A filtered-but-empty library shows a different state in the code ("No recordings match these
  filters", with Clear filters). The board does not draw it. Keep it, restyled like this card.

## 5. Keyboard map

Unchanged. Tab reaches **Import an audio file** first, then **Connect PLAUD**. Both the toolbar's
Import audio… and `Ctrl+O` do the same thing as the primary button.

## 6. Tagged lists

### EXISTS

- The empty card with heading "No recordings", the paragraph, and both buttons
  (`empty_state`), the filtered variant, the file drop path and the drop overlay.
- The import progress bar and the per-file results (`import_results_panel`), batch summary strings.
- Toast behaviour: bottom-right area, polite live region, manual dismiss.

### CHANGE

- The card is `Frame::group` today with an egui `heading()`; use `bg_surface`, `radius.large`,
  centred column, max width 560 (`ui.vertical_centered` plus `set_max_width`).
- Primary button in the card is a plain button today; make it the primary variant.
- `drop_overlay` draws a framed heading plus a list of accepted extensions; the board draws a dashed
  border and a pill. The extension list is not on the board; keep it somewhere (hint text under the
  pill) or it disappears.
- Toasts: restyle (tile, shadow, radius, action button). The import toast text comes from
  `describe_batch`; today `Shell::report_import` pushes "Imported {n} recording(s)" instead and the
  per-file results live in the inline panel.

### PROPOSED

- The monochrome mark in the empty state (needs the image loader, **NEW DEP**).
- The one-line copy note under the pill.
- Progress inside a toast and an action button inside a toast.

### NEW DEP

- SVG or PNG image loading for the mark (`egui_extras` with `svg`, plus `install_image_loaders`, or
  pre-rasterised PNG at 1x and 2x). Not used anywhere in the repo today.

## 7. Snapshots that move

`library_empty.png` (the empty library, `library_screen.rs`). `spike-empty-library.png`
(`apps/plaudon-egui/tests/spike_kittest.rs`) shows the spike view (`PlaudonEgui::view`: two buttons
and an Activity list), not this screen, so only the colour tokens change it. The committed
`shell-ready.png` lists six destinations, so it predates the People destination and was already
stale before this redesign; see the playbook.
