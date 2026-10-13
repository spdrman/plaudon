# Recording detail, Outputs tab

The same recording screen with the aside dragged to 540 and the **Outputs** tab open: the generated
summary, key decisions, action items and the other claim sections. Source board
`source/RecordingOutputs.dc.html`. Pictures: [light](recording-outputs-light.png) (1280 by 1100) and
[dark](recording-outputs-dark.png) (1280 by 820).

Shared chrome: [`../COMPONENTS.md`](../COMPONENTS.md) section 13. The header, player and transcript
rules from [`recording-transcript.md`](recording-transcript.md) apply with the compact changes below.
Tokens: [`../TOKENS.md`](../TOKENS.md).

Code: `crates/ui/src/ai/outputs.rs` (`OutputsViewer`, `output_header`, `claims_section`, `section`,
`claim_fields`), `ai/review.rs` (`claim_state_badge`, `stale_badge`), `ai/speakers.rs`,
`screens/recording/ai_panels.rs` (`AiBinding::ui`, `OutputsTab`).

State drawn: the Outputs tab, the output "needs review", Activity with 1 unread. The toolbar title
reads "Library" on the board; the app's route title is "Recording".

## 1. Regions

### 1.1 Left column (flex, padding 14 top and bottom, 16 right, 22 left, gap 10)

The player is compact here and the header is reduced:

| Element | Spec | Token |
| --- | --- | --- |
| Back link | "Back to library", 24 high, `sm` 12, left aligned (the Delete button is not drawn on this board) | `accent` |
| Title | `h1` "Weekly sync, platform team", `xl` 19 weight 700 | `text_primary` |
| Badges | **✓ Transcribed**, **✓ Local only**, gap 6 | success on `tint_success` |
| Player strip (`aria-label="Playback"`) | card, padding 8 by 10, gap 6; waveform 40 high (96 columns, 3 wide on a 5 pitch, the first 46 are played), 2 wide 42 high playhead; below it a Play button (32 by 28, primary, triangle) and the clock `22:40 / 48:12` (mono 13) | `accent`, `wave_remaining`, `text_primary` |
| Transcript card | header "Transcript" (`lg` 15 weight 600, padding 8 by 12, bottom hairline); rows grid 54 and flexible, gap 8, padding 8 by 12 | |
| Row | a time chip (22 high, radius 3, mono 12, centred, **not a button on this board**) and, to its right, the speaker line (`sm` 12 `text_secondary`) over the text (`md` 13, line height 1.5) | `status_running` on `tint_running` |

Rows: 22:05 Maya (confirmed): "So for the dashboards, the numbers are a week old because the export
job stalled."; 22:40 Speaker 2 (inferred cluster): "I can rerun it today, but someone needs to own
the alert that should have caught it." (playing, `bg_selected`); 23:18 Maya (confirmed): "Put my name
on that. Let us also check whether the retry limit is the culprit."; 23:51 Speaker 2 (inferred
cluster): "Fine. I will look at the logs first thing."

The compact strip has no ruler, no speaker band, no speed or volume. The board does not say whether
that is a "compact mode" of the same player or a different panel; the mapping says "the player keeps a
compact strip". It does not say what triggers it. Not specified.

### 1.2 Aside, 540 wide (padding 14 by 18, gap 10, left border `border_subtle`)

| Element | Spec | Token |
| --- | --- | --- |
| Title | "Outputs", `lg` 15 weight 600 | |
| Tab strip | one row (540 is wide enough); current tab **Outputs** | `COMPONENTS.md` 6 |
| Output facts | four columns, `sm` 12, label over value: Template **Meeting summary**; Template version **3**; Transcript revision **Edited, current**; Generated **6 Oct 2026, 12:05 UTC** | labels `text_secondary` |
| Needs review strip | padding 8 by 10, radius 6, fill `tint_warning`; "! Needs review" in `status_warning` weight 600 then, in `text_primary` regular, "At least one claim in this output has to be reviewed by a person." | `status_warning` on `tint_warning` (5.85 light, 6.09 dark) |
| Sections | a scrolling column, gap 10 | |
| Footer | top border, padding-top 10: primary **Review the sources** (28), secondary **Regenerate this output** (28), and at the far right "Pipeline version 2" (`sm` 12, `text_secondary`) | |

**Sections.** Each section is a header (a 10 by 10 chevron, down when open, right when collapsed, in
`text_secondary`, then the title at `md` 13 weight 600) and a body. Claims in a section are rows
(padding-left 16, gap 8): a small badge (`xs` 11 weight 600, line height 18, padding 0 6, radius 3,
never wraps) then the claim text (`md` 13, line height 1.5).

| Section | State | Content on the board |
| --- | --- | --- |
| Summary | open | ✓ Source linked: "The team agreed to cut the release branch on Thursday so the migration can land first."; ! Review required: "Dashboard figures are a week out of date because the export job stalled."; – Unverified: "Support will receive the release notes a day before the build ships." |
| Key decisions | open | ✓ Source linked: "The release branch cut-off moves to Thursday." |
| Action items | open | a table, below |
| Risks | collapsed | count 1 |
| Open questions | collapsed | count 2 |
| Topics | collapsed | count 4 |

Collapsed headers print the count after the title in `text_secondary` regular weight.

**Action items table** (left margin 16, 1 pt `border_subtle`, radius 6, clipped, `sm` 12): header
fill `bg_canvas`, weight 600, `text_secondary`; columns `1.6fr 0.8fr 0.8fr 1fr`: Action, Owner, Due
date, Status; cells padding 5 by 8; row top border `bg_hover`.

| Action | Owner | Due date | Status |
| --- | --- | --- | --- |
| Draft the release notes | Speaker 2 | not reported | ✓ Source linked |
| Own the stalled-export alert | Maya | not reported | ✓ Source linked |
| Rerun the export job | not reported | Today | ! Review required |

A missing owner or date is `text_secondary`; a present one is `text_primary`. The status cell is the
claim badge's glyph and word in `status_*`, weight 600.

### Badges used here

✓ Source linked (success), ! Review required (warning), – Unverified (neutral), ✓ Confirmed by a person
(success), ! Stale (warning). Glyph and word in every theme.

## 2. egui mapping

The aside is dragged wider here (540 pt) with the same `ResizeHandle`; the player keeps a compact
strip. Tabs are `selectable_label` in a horizontal row, `Role::Tab`, with a 2 pt `accent` rule under
the current one. Each section is a `CollapsingHeader` (Summary, Key decisions, Action items, Risks,
Open questions, Topics). A claim is a horizontal row: a badge `Frame` (tint fill, status text,
`radius.small`) then a wrapping `Label`. Action items are an `egui::Grid`. The Needs review strip
is a `Frame` in `tint_warning` with `status_warning` text.

## 3. Behaviour

What the board's strip says is "as built", checked against the code:

- **Review the sources** "switches to the Review tab". There is no such button on the Outputs tab
  today. The phrase is the heading of the Review tab (`ReviewPanel::ui`:
  `RichText::new("Review the sources").heading()`). The button is new (PROPOSED); wire it to
  `OutputsTab::Review`.
- **Regenerate this output** "opens the confirm ... when the route is cloud; locally it runs as a
  job". Today the button lives at the foot of the Review tab and always opens the modal "Regenerate
  this output?" with the checkbox "I understand this starts a new run that may be billed"; there is
  no local or cloud branch in `ReviewPanel::regenerate`. Do not remove the confirmation when
  moving the button. If you want the local shortcut, that is a behaviour change to propose
  separately.
- An output that has not been read yet says "Outputs have not been read yet." rather than showing an
  empty list. No output at all says "No output has been generated for this recording yet." Both
  exist.
- Keyboard: tab strip arrows (once the shared tab helper exists), Tab through section headers, Enter
  or Space toggles a section, Tab reaches both footer buttons.

## 4. Where the board and the repo disagree

These matter more than the styling, so I list them before the tags.

1. **Field subset.** The repo renders every §9.2 field of every claim verbatim (`claim_fields`:
   Decision, Participants, Time reference, Assertion; Action, Detail, Owner, Due date, Status,
   Assertion; Risk, Consequence, Mitigation; Question, Owner; Topic, Segments), each as a "Label:
   value" line. The board shows one line per claim and a four-column action table, which drops Detail,
   Assertion, Participants, Time reference, Consequence, Mitigation and the rest. Dropping them would
   remove information the backend deliberately sends. Keep them: show the board's compact row by
   default and put the rest behind the row (an expander or the claim's detail), reachable by keyboard.
2. **"not reported" is not the app's word.** The backend sends the literal "Unassigned" for a missing
   owner and "Not specified" for a missing due date (`crates/app/src/backend/outputs.rs`). The
   board's mapping says a missing owner or date "prints 'not reported' in `text_secondary`". The UI
   composes no words of its own, so print what the DTO says and use `text_secondary` for those two
   values. The board's sample strings are its own.
3. **The speaker panel is missing.** `OutputsViewer::ui` renders a speaker panel (`SpeakerPanel`,
   rename, confirm, reset, people picker, voice suggestions) under a separator after the claims. The
   board shows nothing there. Keep it; place it under the sections.
4. **Header facts.** The repo prints Output, Template (the template id), Template version, Pipeline
   version, Generated and Transcript revision (a raw id) as separate lines, and adds a "Partial
   coverage" badge and the "Stale" badge when they apply. The board prints four facts in a row,
   moves Pipeline version to the footer, and shows "Edited, current" for the revision (a human
   label, not an id). Keep the Partial coverage and Stale badges. The revision label needs a
   lookup from `detail.revisions`.
5. **Unverified.** The repo draws "! Unverified" in the warning tone; the board draws "– Unverified"
   neutral and separates it from "! Review required". Tests assert the repo's glyph text
   (`ai_review.rs`: `"! Unverified"`, `"⛓ Source linked"`). See `GAP_ANALYSIS.md`.

## 5. Tagged lists

### EXISTS

- Six tabs and their panels; the Outputs view reads `list_outputs`; per-claim state badge; the
  Needs review badge with its sentence; stale and coverage badges; the speaker panel.
- The regenerate confirmation dialog (`ReviewPanel::regenerate`), reachable from the Review tab.

### CHANGE

- Sections become collapsing headers with counts (today a bold label per section, claims as bordered
  groups, empty sections omitted entirely rather than shown with a zero).
- Claim rows become badge-plus-text rows; action items become a table.
- The Needs review badge becomes a full-width tinted strip.
- Facts become a four-column row; the footer carries Pipeline version.
- Header buttons: **Review the sources** is a heading plus content on the Review tab today and
  **Regenerate this output** lives at the foot of the Review tab (`ReviewPanel::regenerate`). The board
  puts both in a footer on the Outputs tab. Review tab behaviour for Regenerate must keep working
  after the move.
- Claim badges: `Badge` in `ai/badge.rs` colours text only, with `Tone::Running` and `Waiting`
  collapsed; the tinted frame replaces it (see `COMPONENTS.md` 8).

### PROPOSED

- Output facts in one row and "Edited, current" as a resolved label.
- Collapsed-section counts.

### NEW DEP

None.

## 6. Snapshots that move

None of the 13 committed baselines shows this tab. Add one per tab in light and dark (see the
playbook). Tests to update: `ai_workflows.rs`, `ai_review.rs`, `ai_summarize.rs`, `recording_detail.rs`.
