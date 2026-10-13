# Jobs

Every job in the archive as a card, filtered by state and kind, with progress, the corrective
controls each state allows, and the guards that keep a cloud retry from billing twice. Source board
`source/Jobs.dc.html`. Pictures: [light](jobs-light.png) (1280 by 1100) and [dark](jobs-dark.png)
(1280 by 820).

Shared chrome: [`../COMPONENTS.md`](../COMPONENTS.md) section 13. Tokens:
[`../TOKENS.md`](../TOKENS.md).

Code: `crates/ui/src/jobs/mod.rs` (`JobsScreen`: `ui`, `filters`, `row`, `capacity`),
`jobs/shell_adapter.rs`, `crates/ui/src/ai/job.rs` (`JobCard`), copy and rules in
`crates/app/src/job_presentation.rs` (`job_state_presentation`, `error_guidance`,
`CANCEL_DISCLOSURE`, `job_progress`, `job_elapsed_label`). Route `Route::Jobs`, `Route::JobsJob`.
`Ctrl+3`.

State drawn: toolbar title "Jobs", Activity 2, six cards covering six states, the first card
focused as if opened from a link, warning badge in the status bar (Local-only off).

## 1. Regions

Main region: padding 20 by 24 (20 is off-scale), gap 12, content clipped.

| Element | Spec | Token |
| --- | --- | --- |
| `h1` "Jobs" | `xxl` 24 weight 700 | `text_primary` |
| State filter row | label "Filter by state" (`sm` 12, `text_secondary`, 96 wide), then chips, gap 6, wraps | |
| Kind filter row | label "Filter by kind" (same), then chips | |
| Chip | 26 high, padding 0 10, radius 13 (full pill), `sm` 12, 1 pt border. Selected: fill `bg_selected`, border `border_strong`, weight 600. Unselected: fill `bg_surface`, border `border_subtle` | |
| Cards | two columns, gap 12 | |
| Footer button | secondary **Show more jobs** (28), centred | |

State chips on the board: **All: 14** (selected), Running locally: 1, Preflight: 1, Queued: 1, Cloud
outcome unknown: 1, Failed — can retry: 1, Completed: 9. Kind chips: **All** (selected), Import,
Transcription, Summary, Export, Model install, Source sync, Speaker matching.

### 1.1 Card anatomy

`bg_surface`, 1 pt `border_subtle` (2 pt `focus_ring` for the focused card), `radius.large` 10,
padding 12 by 14, column gap 8.

1. Header row (gap 8): kind (`sm` 12, `text_secondary`), title (`md` 13 weight 600, truncates), state
   badge on the right (padding 0 7, radius 3, `sm` 12 weight 600, line height 20): a mono glyph then the
   label, in the state's tone on its tint.
2. Description (`sm` 12, line height 1.5, `text_secondary`).
3. Progress bar when the state has one (18 high; see `COMPONENTS.md` 10).
4. Facts row (`sm` 12, gap 14): "Stage" and "Elapsed", values in `text_primary`.
5. Extra box when there is a condition to explain (padding 8 by 10, radius 6, fill = the tone's tint,
   `sm` 12, gap 6): a weight 600 title in the tone colour, a line in `text_primary`, and sometimes a
   checkbox.
6. Action row (gap 8): buttons 26 high, padding 0 10, radius 6, `sm` 12.

### 1.2 The six cards

| # | Kind and title | Glyph and state | Tone | Description | Bar | Stage, Elapsed | Extra box | Actions |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Transcription, "Sync notes, design review" | `>` Running locally | running | Processing on this computer; nothing leaves the device. | 62% "Running locally: 62% complete", label in `accent_text` | Transcribing, 4 min 12 s | none; this card is focused | destructive **Cancel job** |
| 2 | Import, "Podcast draft, episode 12" | `?` Preflight | running | Checking inputs, space and engine availability before starting. | indeterminate, 24% segment at 30%, "Preflight: progress not reported" in `text_primary` | Preflight, 3 s | none | **Cancel job** |
| 3 | Summary, "Weekly sync, platform team" | `??` Cloud outcome unknown | warning | The provider response was lost. The request may already have been accepted and billed, so it is never retried automatically. | none | Sending to provider, 1 min 40 s | `tint_warning`: title "Cloud request outcome unknown", line "Provider request id: [request id from the provider]", checkbox (14 pt box) | primary **Retry job**, disabled at 0.5 opacity until the checkbox is ticked |
| 4 | Import, "Kitchen renovation call" | `!r` Failed — can retry | danger | The job stopped on an error that may succeed on another attempt. | none | Copying into the library, 12 s | `tint_danger`: title "Not enough space", line "Library volume. Free space: 1,288,490,188 bytes of 256,060,514,304 bytes" | primary **Retry job**, secondary **Open storage settings for the library volume** |
| 5 | Transcription, "Lecture 7, signal processing" | `…` Queued | neutral | Waiting for a free slot. | none | not started, Elapsed time unknown | none | **Cancel job** |
| 6 | Model install, "ggml-base.en" | `OK` Completed | success | The output and its metadata are durably committed. | none | Committing, 2 min 03 s | none | none |

The glyphs (`>`, `?`, `??`, `!r`, `…`, `OK`) are the ones `job_state_presentation` ships. They are
ASCII on purpose so they render in any font; keep them.

Cancel buttons are destructive (outline `status_danger`). "Retry job" is primary.

## 2. egui mapping

Filters are `selectable_label` chips with `CornerRadius` 13 (a full pill), selected = `bg_selected` plus
`border_strong`. Counts come from the page's per-state counts; when none are reported the row says so
("No per-state counts were reported for this page.") instead of showing zeros. Cards sit in a
two-column `egui::Grid` inside a `ScrollArea`, 50 jobs per page, **Show more jobs** at the end.
Every `JobState` has its own glyph and label from `job_presentation`, painted in its tone on the
matching tint. `ProgressBar::new(fraction).text("Running locally: 62% complete")` when a fraction
exists, otherwise an indeterminate bar with "progress not reported". Completion is never invented.

## 3. Behaviour (as built; must not change)

- Cloud outcome unknown is never retried automatically. **Retry job** stays disabled until the cost
  acknowledgement is ticked.
- **Cancel job** opens the irreversibility disclosure (`CANCEL_DISCLOSURE`, see `COMPONENTS.md` 12).
- Not enough space shows the library volume's capacity and a storage-settings control. If the
  capacity is unknown it says so.
- A job opened from a link (notification, activity) scrolls into view and takes focus. Its frame stroke
  becomes `focus_ring` at 2 pt, as on card one, and it is announced "Job: {label} — opened from a link".
- A dropped `job.updated` event refetches the page under the current filters.

Keyboard: Tab through the filters, then each card's controls; Space toggles a chip; the checkbox and
the buttons follow the usual keys.

## 4. Where the board and the repo disagree

1. **"All: 14" contradicts a documented rule.** The jobs module says "There is no total: `list_jobs`
   does not send one, so the screen does not show one", and the test
   `the_state_counts_are_the_backend_s_and_there_is_no_total` (`jobs_screen.rs`) enforces it. 14 is
   the sum of the other six counts. Do not sum them in the UI. Draw an **All** chip with no number
   unless the backend starts reporting a total. The other chips keep "label: count" from
   `state_counts`.
2. **Filter model.** The repo's filters are multi-select checkboxes (`states: Vec<JobState>`, `kinds:
   Vec<JobKind>`) and show only the states that have rows or are already selected. The board draws
   chips with a single selected "All". Keep the multi-select behaviour. Reconcile with: chips toggle;
   **All** is shown selected when nothing else is, and pressing it clears the set. That is my reading,
   not the board's.
3. **Retry acknowledgement copy.** Board: "I understand this starts a new run that may be billed" (that
   is the Regenerate sentence). Code: "I understand this request may already have been billed and that
   retrying may be billed again" (`JobCard::controls`). Keep the code's.
4. **Elapsed.** The board prints "Elapsed Elapsed time unknown" on card five (the label and the
   already-labelled value are concatenated). `job_elapsed_label` returns "Elapsed time unknown" and
   `JobCard` strips the leading "Elapsed" once. The picture is a mock bug.
5. **Cost and request id.** The repo shows a cost badge (`cost_presentation`) on every card and a
   "Provider request id" fact whenever one exists. The board has no cost badge anywhere and shows the
   request id inside card three's extra box. Keep the cost badge; an unknown cost must read "Unknown".
6. **Capacity wording.** The repo prints "Library volume: {path}" and "Free space: N bytes of M bytes"
   as two lines plus "Compare the space this job needs with the space the library volume has left in
   storage settings.". The board prints "Library volume. Free space: ...". Keep the verbatim byte
   counts (a rounded figure is one the backend never sent).
7. **Single column vs two.** The repo lays one row per job in a single column.

## 5. Tagged lists

### EXISTS

- The list with state and kind filters, state counts, 50 per page with Show more jobs, deep-link
  focus, resync.
- Cards with kind and label, state badge with glyph, description, stage, elapsed, cost, progress rules,
  failure message and guidance, corrective controls, cancel disclosure, cost-acknowledged retry,
  capacity comparison with the storage-settings control.

### CHANGE

- Filters from checkboxes to pills (keep multi-select).
- Cards from a single column of grouped rows to a two-column grid of framed cards; the title from an
  egui `heading()` to `md` 13 weight 600; the state badge from text lines to a tinted frame at the
  right of the header; stage and elapsed on one row.
- Cancel buttons destructive; Retry primary; Retry disabled at 0.5 alpha.
- Extra boxes tinted per tone; the focused card stroke to `focus_ring` 2 pt (today a label says "opened
  from a link" and takes focus).
- Progress label colour (see the label-colour rule in `COMPONENTS.md` 10; the board's Preflight
  bar fails 4.5:1 where its label crosses the fill).

### PROPOSED

- Per-state counts shown inside the chips (the counts exist, drawn as lines today).

### NEW DEP

None.

## 6. Accessibility notes

- State is a glyph and a word in the badge. The extra boxes say what they mean in words ("Cloud request
  outcome unknown", "Not enough space").
- The checkbox gating Retry is a normal checkbox with a visible label; the disabled button keeps its name.
- `status_waiting`, `status_warning` and `status_neutral` are not distinguishable by hue; the words are
  distinct, so this is fine.
- Contrast of `status_*` on `tint_*`: 5.41 to 6.70:1 (`TOKENS.md` 6).

## 7. Snapshots that move

None committed for Jobs. Tests: `crates/ui/tests/jobs_screen.rs` (filters are asserted by label and
count; the keyboard test `every_control_of_the_jobs_screen_is_reachable_by_keyboard`).
