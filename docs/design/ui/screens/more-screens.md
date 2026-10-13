# Inbox, search, people, templates, activity, command palette

Six smaller surfaces on one board. The board shows main-region content only; the shell around each is
the Library's. Source board `source/MoreScreens.dc.html`. Pictures: [light](more-screens-light.png) (1280
by 1100) and [dark](more-screens-dark.png) (1280 by 820).

The page is 1280 by 820, padded 24 by 28, a title row and a 3 by 2 grid of panels (gap 16). Panels are
`bg_surface`, 1 pt `border_subtle`, `radius.large` 10, padding 14, column gap 8, clipped. Headings are
`h2` at `xl` 19 weight 700. The notes in `text_secondary` under each panel are board annotations.

Components: [`../COMPONENTS.md`](../COMPONENTS.md). Tokens: [`../TOKENS.md`](../TOKENS.md).

Shared list-row rule from the board's mapping strip: Inbox, People, Templates and Activity are
`ScrollArea::show_rows` lists of row `Frame`s: 1 pt `bg_hover` border, `radius.medium` 6, padding 8;
selected rows fill `bg_selected`; focus adds the 2 pt ring.

---

## 1. Inbox

Code: `crates/ui/src/screens/inbox/mod.rs` (`InboxScreen`). Route `Route::Inbox`, `Ctrl+2`.

| Element | Spec | Token |
| --- | --- | --- |
| Header | `h2` "Inbox" and the chord hint "Ctrl+2" (`xs` 11) at the right | `text_primary`, `text_secondary` |
| Sub line | "Inbox recordings, newest import first" (`sm` 12) | `text_secondary` |
| Row | title (`md` 13 weight 600) over a status line (`sm` 12 in the status colour) | |

Rows: **Sync notes, design review**, "⟳ Transcribing" (`status_running`, selected, `bg_selected`);
**Podcast draft, episode 12**, "⟳ Importing"; **Field recording, river**, "◷ Pending"
(`status_neutral`); **Voice memo**, "– Not transcribed" (`status_neutral`).

Empty state (annotation): "Nothing waiting" with **Import an audio file** and **Go to Library**. The
repo's empty state is a framed group: heading "Nothing waiting", "Recordings appear here while they are
importing or waiting to be transcribed. Finished recordings live in Library." and those two buttons.

**The repo's Inbox is the Library table, not a card list.** `InboxScreen::ui` hands its rows to the same
`RecordingTable` the Library uses (ten columns, sort, selection, load more). The board shows a compact
list of title and status. Decide: restyle the table (and keep every column) or build the card list
(and lose the columns). The board's mapping says "lists of row Frames", so it intends a list. If you
build the list, keep the selection, keyboard model and virtualisation the table has
(`TableBuilder` rows or `show_rows`), and keep sorting reachable somewhere. EXISTS (content and
empty state), CHANGE (presentation), and the heading "Inbox" with sub line is PROPOSED (no heading today).

## 2. Search transcripts

Code: `crates/ui/src/screens/search/mod.rs` (`SearchScreen`, `group_by_recording`, `seek_label`). Route
`Route::LibrarySearch`, `Ctrl+Shift+F`. It lives inside Library.

| Element | Spec | Token |
| --- | --- | --- |
| Header | `h2` "Search transcripts" | |
| Field | 28 high, 2 pt `focus_ring` (focused), text "retry limit" | |
| Count | "2 results in 2 recordings", `sm` 12 | `text_secondary` |
| Hit card | padding 8, radius 6, 1 pt `bg_hover`; recording title (`md` 13 weight 600), the excerpt (`sm` 12, line height 1.5) with the matched words on a `tint_warning` background, and a secondary 24 high button | |

Hits: **Weekly sync, platform team**, "…check whether the retry limit is the culprit.", button **Play
from 23:18**. **Voice memo**, "…raise the retry limit before Friday.", button **Open segment 12**.
Annotation: "No timing, no play button: the second result opens the segment instead."

Behaviour from the strip: results are grouped by recording; the hit is a `LayoutJob` section in
`tint_warning`; "Play from" appears only when the result has a seek time, and "Play from about" when the
timing is derived.

Where the board and the repo differ:

1. **Buttons per hit.** The repo always shows **Open segment N** and adds **Play from {time}** (or
   "Play from about {time}") beside it when the hit has a position. The board shows only one of the two.
   Keep both; opening the segment is always possible.
2. **"Play from" does not play.** In `screens/library/screen.rs` both `SearchIntent::OpenSegment` and
   `SearchIntent::Seek` just navigate to the recording; no seek is performed. The label promises
   playback. Not a design issue, but the revision should not make the label more truthful than the
   behaviour: either seek on arrival or reword.
3. **Highlight.** `SearchResultDto` has no match offsets (`excerpt`, `seek_ms`, `alignment_origin`
   only). A highlight means finding the submitted query in the excerpt on the client. If it is not found
   (stemming, normalisation) draw no highlight; never invent one.
4. **Search button and hint.** The repo has a visible **Search** button and the hint "Words spoken in any
   recording"; the board shows neither (the field is focused with text). Keep them.
5. **Empty and first states.** "Search the library" with "Enter a search term to find words spoken across
   every recording." and "No results" with "No transcript segment matches “{query}”." exist and the board
   does not draw them. Keep, restyle as the Inbox empty card.

EXISTS: grouping, count sentence, seek labels, error notice. CHANGE: heading style, field, hit cards,
highlight. PROPOSED: none tagged.

## 3. People

Code: `crates/ui/src/screens/people/` (`mod.rs` `PeopleScreen`, `roster.rs`, `picker.rs`, `review.rs`).
Route `Route::People`, `Ctrl+7`.

| Element | Spec | Token |
| --- | --- | --- |
| Header | `h2` "People" and the hint "Ctrl+7" | |
| Sub line | "One list of people for the whole library. Every recording assigns speakers from it." (`sm` 12, line height 1.5) | `text_secondary` |
| Search row (gap 8) | a flexible 26 high field with hint "Search people", primary **Add a person** (26 high, `sm` 12 weight 600) | `accent` |
| Row | avatar, then name over meta; padding 8, gap 10 | |
| Avatar | 28 pt circle, the person's initial (`sm` 12 weight 700) | `status_running` on `tint_running` |

Rows: **Maya**, "In 14 recordings · voice: none"; **Jonas**, "In 6 recordings · voice: none"; **Ruth**, "Not
assigned in any recording yet". Annotation: "Works with voice matching off. Review queue keys: j, k, Enter,
Del, n."

As built: rename, merge and delete each with their disclosures; possible duplicates are never merged by
themselves ("These entries have the same name. Nothing is merged automatically — merging is always your
decision."). The repo also has a **Review** tab (shown only while voice matching is on) and a
`ReviewQueue`; the board draws neither. Keep them.

EXISTS: heading, sub line, search, Add a person, rows with "In N recordings" and "Not assigned in any
recording yet" (`roster.rs`), the dialogs. CHANGE: row style. **PROPOSED (tagged on the board):** the
initial avatars. The meta line badge "voice: none" is drawn by `roster.rs` today (`Badge::new("—", "voice: none", ..)`)
and becomes "voice: {status}" once a voice is enrolled.

## 4. Templates

Code: `crates/ui/src/screens/templates.rs` (`TemplatesScreen`), `crates/ui/src/ai/templates.rs`
(`TemplateManager`). Route `Route::Templates`, `Ctrl+4`.

| Element | Spec | Token |
| --- | --- | --- |
| Header | `h2` "Templates" and "Ctrl+4" | |
| Row | name (`md` 13 weight 600) over meta (`sm` 12, `text_secondary`), a small badge at the right (`xs` 11 weight 600, line height 18, padding 0 6, radius 3) | |
| Buttons (gap 8) | secondary **Copy this template**, **Validate** (26) | |
| Result line | "✓ This document would be accepted by Save." (`sm` 12 weight 600) | `status_success` |

Rows: **Meeting summary**, "Version 3 · ships with the application", badge **Built-in** (neutral);
**Lecture notes**, "Version 2 · ships with the application", **Built-in**; **Team retro (your copy)**,
"Version 4 · stored 2 Oct 2026", badge **Your copy** (`status_running` on `tint_running`), selected.

As built: built-ins ship read-only; **Copy this template** makes your own; Save refuses a document with
problems and lists each one. The repo's manager also has "Edit this template", "Version history", and
per-field headings; the board draws none of them. Keep them. The repo's built-in badge is "◆ Built-in"
(a test asserts that text, `ai_templates.rs`); the board's is "Built-in". The glyph is part of the
badge; keep a glyph (painted) so colour is not the only signal.

## 5. Activity history

Code: `crates/ui/src/activity/mod.rs` (`ActivityLog::ui`, `entry_ui`, `category_presentation`),
`shell/bell.rs`. Route `Route::SettingsActivity`; opened from the toolbar bell.

| Element | Spec | Token |
| --- | --- | --- |
| Header | `h2` "Activity history" with a secondary 24 high **Refresh activity** at the right | |
| Row | a marker column (8 wide; "•" in `accent`, `sm` 12 weight 700, for unread), then a title (`md` 13; weight 600 when unread) over a meta line (`sm` 12, `text_secondary`); padding 8, gap 8 | |
| Unread row fill | `bg_selected` | |

Rows: unread **Transcription finished** / "Weekly sync, platform team · 6 Oct, 12:01 · Desktop notification
delivered."; unread **Requires attention** / "Kitchen renovation call · Open the job for Kitchen
renovation call"; read **Import finished** / "3 recordings · 5 Oct, 15:02"; read **Import finished** / "1
recording · 3 Oct, 10:51". Annotation: "Unread carries a '•' and the word Unread for screen readers, not
colour alone."

Where it differs from the repo: the repo draws each entry as a group with a category badge, a **Read** or
**Unread** badge (`Badge::new("*", "Unread", Tone::Running)`), the title in bold, the body, an "Occurred"
fact, the desktop-notification sentence, and a button "Open the job for {title}" or "Open the recording
for {title}". It also has a category filter (checkboxes), "Unread when this page was read" facts, "Load
older activity" and an empty sentence. The board is a condensed form. Keep every item the repo has; the
marker becomes the visible unread cue and the word "Unread" stays in the AccessKit name. The bell is in
`COMPONENTS.md` section 13.

## 6. Command palette

Code: `crates/ui/src/shell/palette.rs` (`Palette`, `rank`, `score`). Opened by `Ctrl+K` and the toolbar
button.

The board draws the palette inside a `bg_canvas` panel (the panel header reads "COMMAND PALETTE · Ctrl+K").
The dialog: `bg_raised`, 1 pt `border_subtle`, `radius.large` 10, popup shadow, padding 8, gap 4.

| Element | Spec | Token |
| --- | --- | --- |
| Field | 30 high, 2 pt `focus_ring`, text "imp", name "Search commands" | |
| Row | 30 high, padding 0 8, radius 6; label (`md` 13, flexible), group (`xs` 11), chord (`xs` 11, min width 48, right-aligned) | group and chord `text_secondary` |
| Selected row | fill `bg_selected` | |

Rows: **Import audio…** | Library | Ctrl+O (selected); **Go to Library** | Navigation | Ctrl+1; **Command
palette** | General | Ctrl+K; **Keyboard shortcuts** | General | ?. Annotation: "Arrow keys move, Enter runs,
Escape closes. Same registry as menu, toolbar and keymap."

Behaviour (as built; keep): arrow keys, Home and End move the selection, Enter runs, Escape closes
(`close_overlay`), a ranked subsequence matcher (`score`: runs, word starts, prefixes score highest;
identifier matches rank below label matches), results capped, the field named "Search commands" for
assistive technology, "No matching command" when empty.

Where it differs: the board's mapping says "An `egui::Modal`-free `Area` anchored top-centre with
`window_shadow`". The repo uses `egui::Modal` (`Modal::new(Id::new("plaudon-command-palette")).show`),
which dims the window. The board's picture has no backdrop. The row text is one string today ("Label  ·
Group  ·  Chord", `command_row_text`) so a screen reader reads it as a sentence; the board lays it out in
three columns. If you draw three columns, keep one AccessKit name that reads in the same order. The
palette width is 520 in the repo; the board does not specify one. The field hint is "Type a command".

## 7. Keyboard map

- Palette: `Ctrl+K`, arrows, Home, End, Enter, Escape.
- Search: `Ctrl+Shift+F` opens, Enter in the field submits.
- People review queue: `j` and `k` (or the arrows), Enter, Delete or Backspace, `n` (verified in
  `screens/people/review.rs`; the board lists j, k, Enter, Del, n).
- Rows in lists: Up and Down move the focus ring, Enter opens, as the Library table.

## 8. Tagged lists

### EXISTS

- Inbox content and empty state; search (grouping, labels, errors); People roster, dialogs, review tab;
  Templates (built-ins, copy, edit, history, validate); Activity entries with read state and links; the
  palette with ranking and the "Search commands" name.

### CHANGE

- Row styling for all four lists; Inbox as a list instead of the ten-column table; hit highlight in
  Search; palette surface and columns; Activity condensed rows (keep the content).

### PROPOSED

- Initial avatars in People (tagged on the board).
- Section headings with chord hints on Inbox, People and Templates.
- An Area-based, backdrop-free palette (the board's mapping implies this; the repo has a Modal).

### NEW DEP

None.

## 9. Snapshots that move

None committed for these. Tests: `crates/ui/tests/people_screen.rs`, `ai_templates.rs`, `activity.rs`,
`library_screen.rs` (inbox and search paths), `shell_kittest.rs` (palette).
