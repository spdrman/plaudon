# Recording detail, Transcription tab

One recording: header and provenance, the player, the transcript editor, and the outputs column
open on its Transcription tab. Source board `source/Recording.dc.html`. Pictures:
[light](recording-light.png) (1280 by 1100) and [dark](recording-dark.png) (1280 by 820). The other
two tabs of the same screen are in [`recording-outputs.md`](recording-outputs.md) and
[`recording-review.md`](recording-review.md).

Shared chrome is in [`../COMPONENTS.md`](../COMPONENTS.md) section 13. Tokens in
[`../TOKENS.md`](../TOKENS.md).

Code: `crates/ui/src/screens/recording/` (`mod.rs` the container, `playback.rs`, `transcript_panel.rs`,
`ai_panels.rs`, `live.rs`, `artifacts.rs`, `deletion.rs`), `crates/ui/src/player/`,
`crates/ui/src/transcript/`, `crates/ui/src/ai/transcription.rs` and `stage.rs`. Route
`Route::LibraryRecording { recording_id }`, owned by Library (`screens/library/screen.rs`).

**The board's toolbar title says "Library" on all three recording boards.** The route title for a
recording is "Recording" (`Route::title`), and that is what the app will print. The board is wrong
on this one point; leave the code.

## 1. Regions

Window 1280 by 820. The body (722 high) is the rail (216) plus the main region, and the main region
is split into a left column (flex) and the **aside**, 330 wide on this tab, `bg_canvas`, 1 pt
`border_subtle` on its left edge, padding 14 by 16, gap 10. The aside is the shell's `Screen::aside`
(see section 4). The outputs and review tabs drag it to 540.

### 1.1 Left column: padding 14 top and bottom, 18 right, 22 left (off-scale), gap 12

**Header block (gap 6).**

| Element | Spec | Token |
| --- | --- | --- |
| Back link | "Back to library", 24 high, `sm` 12, chevron 12 by 12 stroke 1.6, left aligned | `accent` |
| Delete button | "Delete recording…", 26 high, padding 0 10, `sm` 12, right aligned on the same row as the back link | outline `status_danger`, text `status_danger` |
| Title row (gap 10) | `h1` "Weekly sync, platform team" at `xl` 19 weight 700; badge **✓ Transcribed** (`status_success` on `tint_success`); badge **✓ Local only** (same tones; tooltip "Local-only mode is on: this recording is never sent to a provider.") | `text_primary`, badges per `COMPONENTS.md` 8 |
| Facts grid | five columns 96, 150, 130, 130, 130 wide, column gap 12, row gap 1, `sm` 12 | labels `text_secondary`, values `text_primary` |
| Facts | Source: Watched folder; Original file: weekly-sync-1006.m4a; Content hash (SHA-256): `3f9a1c0e2b7d…` (mono, twelve characters then an ellipsis, as the provenance rule says); Recorded: 6 Oct 2026, 09:02 UTC; Imported: 6 Oct 2026, 11:40 UTC | |
| Summary line | `sm` 12: "Duration 48:12 · Codec aac · Engine whisper.cpp · Renaming is not part of this build, so the title is shown exactly as it was imported." The duration is mono; values in `text_primary`, the rest `text_secondary` | |

**Player card** (`aria-label="Playback"`): `bg_surface`, 1 pt `border_subtle`, `radius.large` 10,
padding 10 by 12, gap 6.

| Element | Spec | Token |
| --- | --- | --- |
| Ruler | 14 high, mono 10; ticks every 5 minutes: 0:00, 5:00 ... 45:00; each tick is a 1 pt left rule, 12 high, with 3 of padding before the label | rule `border_strong`, label `text_secondary` |
| Waveform | 64 high; columns 3 wide on a 5 pt pitch (so 2 of gap), radius 1, heights 4 to 60; `role=slider`, `aria-label="Position"`, `aria-valuetext="12:03 of 48:12"` | played `accent`, remaining `wave_remaining` |
| Playhead | 2 wide, 68 high, with a 10 pt round knob on top (shown keyboard-focused with the 2 + 2 ring) | `text_primary` |
| Speaker regions | 16 high strip under the waveform; each region a rect with radius 3, label `xs`-ish 10 weight 600, 4 of left padding, clipped to the region | A: `tint_running` with `status_running`; B: `tint_waiting` with `status_waiting` |
| Transport row (gap 8) | **−10** (32 by 28, secondary, text 10 weight 700, name "Back 10 s"), **Pause** (36 by 32, primary, two-bar icon, name "Pause"), **+10** (32 by 28, name "Forward 10 s"), the clock `12:03 / 48:12` (mono 13, the total in `text_secondary`), a spacer, **Speed** label and a 70 by 26 ComboBox "1×", **Volume** label and a 96 wide slider | |

Region labels on the board, in order: "Maya (confirmed)", "Speaker 2 (inferred cluster)", "Maya
(confirmed)", "Speaker 2 (inferred cluster)", "Maya (confirmed)", "Speaker 2". Regions alternate A, B
because the speakers differ **by name, not hue**: two tints only. Long labels truncate inside a
short region (they do in the picture); the transcript speaker chip has the full name.

**Transcript card** (`aria-label="Transcript"`): `bg_surface`, border, radius 10, flex 1, a column.

| Element | Spec | Token |
| --- | --- | --- |
| Header row (padding 8 by 12, gap 8) | "Transcript" at `lg` 15 weight 600; a 26 high ComboBox "Edited 6 Oct 2026, current" (the revision picker); spacer; "✓ All changes saved"; secondary **Undo edit** (26); secondary **Redo**, disabled | `text_primary`, `status_success`, disabled at 0.6 alpha |
| Find bar (padding 6 by 12, gap 8, wraps) | field 150 by 24 "release" (name "Find in transcript"); "2 of 3"; **Previous**, **Next** (24 high); checkboxes **Match case**, **Whole word** (14 pt boxes here); field 120 by 24 "Replace with"; **Replace**, **Replace all** | bar fill `bg_canvas`, 1 pt `border_subtle` underneath |
| Segment row | grid 62, 170, flexible, gap 10, padding 8 by 12, hairline `bg_hover` underneath, items top-aligned | |
| Time chip | button 22 high, radius 3, mono 12 weight 500, text like "11:02" | `status_running` on `tint_running` |
| Speaker chip | button 22 high, 1 pt `border_subtle`, transparent, `sm` 12, left aligned, padding 0 6, ellipsis | `text_primary` |
| Segment text | `md` 13, line height 1.5, padding 2 by 6, radius 6, 2 pt transparent border | `text_primary` |
| Find highlight | background behind the matched word, radius 2 | `tint_warning`; text stays `text_primary` (13.05:1 light) |
| Current match | highlight plus a 1 pt `focus_ring` outline | |
| Playing segment | whole row filled | `bg_selected` |
| Segment being edited | 2 pt `focus_ring` border on the text box, fill `bg_surface` | |

The five segments on the board: 11:02 Maya (confirmed), "Okay, so the cut-off for the release branch
is Thursday, not Friday."; 11:31 Speaker 2 (inferred cluster), "That works if the migration lands
first. I can pair on it tomorrow morning."; 12:01 Maya (confirmed), "Good. Then the release notes go
out with the build, and support gets them the day before." (playing and current match); 12:40
Speaker 2 (inferred cluster), "I will draft them tonight and send them round for a read." (being
edited); 13:12 Maya (confirmed), "One more thing about the dashboards: the numbers there are still from
last week."

### 1.2 Aside, Transcription tab

| Element | Spec | Token |
| --- | --- | --- |
| Title | "Outputs", `lg` 15 weight 600 | `text_primary` |
| Tab strip | six tabs, wraps to two rows at 330 (gap 2 vertical, 4 horizontal); current tab is "Transcription" | `COMPONENTS.md` 6 |
| Sub heading | "Transcribe this recording", `md` 13 weight 600 | |
| Engine option cards (gap 6) | each a radio row, padding 8, radius 6, 1 pt border; the chosen one has a `accent` border; unavailable ones draw at 0.85 opacity | |
| Facts | two columns: "Model" / "ggml-base.en"; "Estimated charge before dispatch" / "None, runs locally"; `sm` 12 | labels `text_secondary` |
| Primary button | **Run transcription**, full width, 30 high | `accent` |
| Note | `sm` 12, line height 1.5: "Only the stage you choose runs again. A model chosen here applies to this recording alone." | `text_secondary` |

The three engine cards:

| Engine | Line | Tone |
| --- | --- | --- |
| whisper.cpp (chosen) | "⌂ On this computer · Installed" | `status_success` |
| Apple Speech | "– Unavailable" | `status_neutral` |
| OpenRouter | "⊘ Blocked by Local-only Mode" and a second line "Through a cloud provider" | `status_warning`, then `text_secondary` |

Status bar: "Playing at 1×" left, then "Ready", then the Local-only badge.

## 2. egui mapping

| Part | Build with |
| --- | --- |
| Layout | `CentralPanel` holds header, player and transcript; the outputs column is the screen's aside: `Screen::has_aside`, drawn into the second pane by the shell with the vendored `ResizeHandle` (`shell::layout::vertical_divider`), width persisted in `ShellState.panels.aside_width` |
| Facts | `egui::Grid`, `sm` labels in `text_secondary` over `md` values; hash monospace, twelve characters |
| Waveform | one `egui::Mesh` of 1 px columns from the peak pyramid, as built. Before the playhead `accent`, after it `wave_remaining` (3.65:1 and 3.71:1) |
| Playhead | `painter.vline`, 2 pt, `text_primary`. The clock text also carries the position, so the two fills never stand alone |
| Speaker regions | `rect_filled` in `tint_running` and `tint_waiting` with the label painted in `text_primary`/`status_*`, from `speaker_turns` |
| Transport | buttons named Play or Pause, Back 10 s, Forward 10 s; icons are painter shapes (`convex_polygon`, two `rect_filled`); Speed is a `ComboBox` 0.5× to 2×; volume a `Slider` |
| Transcript | `ScrollArea::show_rows` over pages of 200; find highlight via `LayoutJob` sections with a `tint_warning` background; current match adds a 1 pt `focus_ring` outline; playing row fill `bg_selected` |

## 3. Keyboard map (as built; must not change)

Player: Space, J, K, L, comma and period (suppressed in text entry). Scrubber: arrow keys 5 s,
PageUp and PageDown 30 s, Home, End. Unknown duration hides the slider and says "Duration unknown".
Transcript: Enter or double-click edits a segment, Escape cancels, `Ctrl+Z` undo. Timestamp chips
appear only when the alignment permits seeking; derived timing reads "(approximate)".

## 4. Tagged lists

### EXISTS

- Container, back button, provenance (Source, Original file, Content hash, Recorded, Imported,
  Duration, Codec), the "Renaming is not part of this build..." sentence, the Transcribed and Local
  only badges, `Delete recording…` and its disclosure dialog, source artifacts panel
  (`artifacts.rs`).
- Player: peaks worker, ruler, waveform with scrub, zoom and pan, transport, keyboard commands.
- Transcript editor: paged load, find and replace with Match case and Whole word, undo and redo,
  autosave state, conflict frame, speaker chips with the honest "(confirmed)" and "(inferred
  cluster)" wording, timestamp chips with "(approximate)".
- Six tabs, per-engine models, the consent flow, "Estimated charge before dispatch".

### CHANGE

- **Layout.** `DetailLayout` in `screens/recording/mod.rs` lays the outputs column out inside the main
  pane (`Wide` at 900 pt or more, side column 32 percent clamped 260 to 420) and gives every panel an
  egui heading plus `Frame::group`. The board wants the shell aside. No production screen implements
  `has_aside` yet (only a test in `shell_kittest.rs`); this is the first real use. Width 330 on this
  tab and 540 on the Outputs and Review tabs, both inside `ASIDE_WIDTH_RANGE` (240 to 720); default
  is 340. How the width follows the tab is not specified.
- **Header.** The repo prints a heading, the rename sentence as small text under it, a badge row, then
  a seven-row vertical Grid (striped), and the Delete button below it. The board has Back and Delete
  on one row, title with badges inline, a five-column facts grid, and a summary line that carries
  Duration, Codec, Engine and the rename sentence. Duration and Codec are not in the five-column grid.
- **Player look.** `PlayerStyle::from_visuals` derives everything from egui `Visuals`: played =
  `selection.bg_fill` (the pale selection fill, not the accent), unplayed = weak text colour, speakers = six
  hard-coded hues (`player/style.rs`). Waveform default 96 high, ruler 20, speaker band 8 and unlabelled;
  the board is 64, 14 and 16 with a label in each region and two tints. Replace `PlayerStyle` with
  token-driven fields.
- **Transport.** Text buttons "Play"/"Pause", "Back 10 s", "Forward 10 s", six speed selectable labels
  and a labelled slider today; the board uses icon buttons (names unchanged), a Speed ComboBox and a
  96 wide Volume slider.
- **Transcript chrome.** The editor's status row (autosave text, Undo, Redo) is below the editor
  (`TranscriptEditor::status_bar`) and Redo is hidden when there is nothing to redo; the board puts them
  in the card header and shows Redo disabled. The revision switcher is a row of selectable labels
  ("Original (machine), current", "Edited, current", "Edited {created_at}"); the board has a ComboBox
  whose text is "Edited 6 Oct 2026, current". The saved-state text is `autosave_state().label()`; the
  board's "✓ All changes saved" is the same sentence as the baseline shows ("All changes saved").
- **Transcription panel** (`ai/transcription.rs`). The repo shows an `Engine` heading, one bordered
  group per engine with the radio, "Engine id", "Status" (printed with Rust's debug format,
  `format!("{:?}", engine.status)`, which is a defect), "Runs", unavailable reason, installed
  models, download size; then a model picker; then the dispatch controls ("Use this model for this
  recording only", **Transcribe this recording**). The board shows three compact cards with one
  status line each, then Model and Estimated charge facts, then **Run transcription**. The board's
  button label is not the app's: the app's label is "Transcribe this recording" and the board uses
  that phrase as the sub heading.
- Badges for "⊘ Blocked by Local-only Mode": the repo uses `Tone::Danger` and the glyph "⛔"; the board
  uses `status_warning`/`tint_warning` and "⊘". Tests assert the label only (`ai_workflows.rs`,
  `ai_summarize.rs`), so the glyph and tone can change.

### PROPOSED

- Playhead knob on the waveform.
- Region labels painted inside the speaker band, two-tint scheme.
- A single status line on each engine card; this does not drop information, since the details can go
  into a tooltip or an expander.

### NEW DEP

None. Icons are painter shapes.

## 5. Accessibility notes

- The waveform exposes a position slider with `aria-valuetext` "12:03 of 48:12". Keep a name and a
  value on it.
- Every region has a label, so the two-tint scheme never relies on hue.
- Seek is offered only when the alignment has real timing; unknown duration disables skip and says
  "Duration unknown".
- The segment being edited is marked by the 2 pt ring and also by being the focused text field.

## 6. Snapshots that move

`scale-recording-detail@1x.png`, `@1_5x.png`, `@2x.png` (`scale_kittest.rs`). The committed 1x image
shows the current layout problems clearly (a one-word-per-line transcript column), so expect it to
change completely. Related tests: `recording_detail.rs` (the stacked layout, deletion disclosure),
`player_panel.rs`, `player_interaction.rs`, `transcript_editor.rs`.
