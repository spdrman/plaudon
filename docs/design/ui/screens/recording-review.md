# Recording detail, Review tab

Claims checked against the transcript, one card per claim, with the source anchor and the actions
that confirm or reject it. Source board `source/RecordingReview.dc.html`. Pictures:
[light](recording-review-light.png) (1280 by 1100) and [dark](recording-review-dark.png) (1280 by
820).

Shared chrome: [`../COMPONENTS.md`](../COMPONENTS.md) section 13. The container, header and player
come from [`recording-transcript.md`](recording-transcript.md) and the aside from
[`recording-outputs.md`](recording-outputs.md) (540 wide here too). Tokens:
[`../TOKENS.md`](../TOKENS.md).

Code: `crates/ui/src/ai/review.rs` (`ReviewPanel`, `claim`, `anchor`, `regenerate`,
`claim_state_badge`, `stale_badge`, `ClaimFilter`), `ai/mod.rs` (`SeekRequest`, `NO_TIMESTAMPS_REASON`),
`screens/recording/ai_panels.rs` (routes `AiIntent::Seek` to the player).

State drawn: Review tab, filter set to "Review required", the first claim keyboard-focused, the
player seeked to 22:05, status bar "Moved to 22:05, where the cited passage begins".

## 1. Regions

### 1.1 Left column

Same compact layout as the Outputs board (no badges under the title on this board; the Delete button
is not drawn). Differences:

| Element | Spec | Token |
| --- | --- | --- |
| Waveform | 40 high, 96 columns, the first 44 played; playhead 2 by 42 at 219 | `accent`, `wave_remaining`, `text_primary` |
| **Cited passage span (PROPOSED)** | a soft rectangle on the waveform, 18 wide, 40 high, at 221, fill `tint_running` at 0.8 opacity, with a 2 pt `accent` base line. Marks where the cited passage sits in time | `tint_running`, `accent` |
| Clock | `22:05 / 48:12` | |
| Transcript rows | 21:40 Speaker 2 (inferred cluster): "Before we close, can we talk about the dashboards?"; **22:05** Maya (confirmed) **· Segment 41**: "So for the dashboards, <u>the numbers are a week old because the export job stalled.</u>"; 22:40 Speaker 2 (inferred cluster): "I can rerun it today, but someone needs to own the alert that should have caught it."; 23:18 Maya (confirmed): "Put my name on that. Let us also check whether the retry limit is the culprit." | |
| Anchored row | row fill `bg_selected`; the time chip is **filled** `accent` with `accent_text` (the seek landed here); the speaker line gains " · Segment 41"; the anchored text is underlined 2 pt `accent` on a `bg_surface` run | `accent`, `bg_selected` |

### 1.2 Aside (540), Review tab

| Element | Spec | Token |
| --- | --- | --- |
| Title and tab strip | "Outputs" and the six tabs, current **Review** | |
| Summary line | "2 of 5 claims have to be checked against the transcript." (`md` 13) | `text_primary` |
| Filter | segmented control at the right of that line (`role=radiogroup`, name "Filter claims"): three segments, 1 pt `border_strong`, radius 6, `sm` 12, padding 3 by 8; the current segment has fill `bg_selected`, weight 600 and 1 pt `border_strong` rules at its sides | |
| Segments, in board order | **All claims**, **Review required** (current), **Verified only** | |

**Claim card, focused (first).** `bg_surface`, **2 pt `focus_ring`** border, radius 10, padding 12 by 14, gap 8.

1. Header row: badge **! Review required** (small badge, warning) and the section name "Summary"
   (`sm` 12, `text_secondary`).
2. Claim text, `md` 13, line height 1.5, weight 600: "Dashboard figures are a week out of date
   because the export job stalled."
3. Anchor box: fill `bg_canvas`, 1 pt `border_subtle`, radius 6, padding 8 by 10, gap 6.
   - Resolution line first, `sm` 12 weight 600, `status_success`: "✓ Anchor resolved in the
     transcript".
   - The cited text, italic `md` 13: "“the numbers are a week old because the export job stalled.”"
   - A four-column facts grid (columns sized to content, column gap 16, row gap 2, `sm` 12; labels
     `text_secondary`): Segment **41**, Characters **22–80**, Alignment origin **Engine reported**,
     Entailment check **✓ Supported by the cited passage** (in `status_success`).
4. "Review note (optional)" label (`sm` 12, `text_secondary`) over a 26 high text field.
5. Buttons (gap 8, 28 high): primary **Seek to this passage**, secondary **Confirm this claim**,
   destructive outline **Reject this claim**.

**Claim card, plain (second).** `bg_surface`, 1 pt `border_subtle`, radius 10, padding 10 by 14, gap
6: badge **– Unverified** (neutral) and "Summary"; text "Support will receive the release notes a day
before the build ships."; "✕ Anchor did not resolve" in `status_danger` `sm` weight 600; then "This claim
is not backed by a resolved source anchor." in `text_secondary`.

**Claim card, plain (third).** Badge **✓ Confirmed by a person** and "Key decisions"; text "The
release branch cut-off moves to Thursday."; "A reviewer accepted this claim against the transcript."
in `text_secondary`.

**Footnote.** `sm` 12, line height 1.5, `text_secondary`: "On a transcript without engine timing the
seek control is replaced by: This transcript has no real timestamps; we do not invent seek times."

Status bar: "Moved to 22:05, where the cited passage begins" left, Local-only badge right. PROPOSED (see
below).

## 2. egui mapping

`ScrollArea::vertical` in the aside. Each claim is a `Frame` (`bg_surface`, `border_subtle`,
`radius.large`); the claim with keyboard focus swaps its stroke to `focus_ring` at 2 pt. The filter is
three `selectable_value` buttons in one horizontal group. Anchors: resolution stated first (glyph and
word), the matched text in italics, then a `Grid` of Segment, Characters, Alignment origin,
Entailment check. In the transcript the anchored range is a `LayoutJob` section with a 2 pt accent
underline and its row gets `bg_selected`. The waveform span is `painter.rect_filled` in `tint_running`
with a 2 pt `accent` base line.

## 3. Behaviour

- **Seek to this passage** exists only when the anchor has a seek time (`anchor.seek_ms`, which
  follows `AlignmentOrigin`). It moves the engine clock to the segment start and scrolls the transcript.
  An anchor proves traceability, not truth (ADR-0041), so Confirm and Reject stay separate actions.
- Status line after a seek: "Moved to 22:05, where the cited passage begins". Not in the app today
  (the board tags it PROPOSED).
- Keyboard: Tab through the claim cards and their controls; the focused card is outlined 2 pt. Arrow
  keys on the filter segments. Confirm and Reject never share a default.

## 4. Where the board and the repo disagree

1. **Seek fallback copy.** The board says that without engine timing "the seek control is replaced
   by: This transcript has no real timestamps; we do not invent seek times." That sentence is
   `NO_TIMESTAMPS_REASON`, the tooltip for an unavailable caption export format (`ai/mod.rs`,
   `format_available`). The review panel's real fallback is: "This passage has no playback position, so
   it cannot be played from here." (`ReviewPanel::anchor`). Keep the real one; it describes a
   passage, not an export.
2. **Segment order.** Repo (`ReviewPanel::ui`): All claims, Verified only, Review required. Board: All
   claims, Review required, Verified only. The board order puts the narrower question in the middle;
   either works. Use the board order, it is a free change.
3. **Fields per claim.** The repo's card shows the claim text, the §9.2 claim fields, the state badge,
   "Review required" with its reasons (each as "• reason"), a Category fact, an Entailment check fact,
   then each anchor. The board drops Category and the review reasons and moves Entailment check inside the
   anchor grid. Keep the reasons: they say why a claim needs review. Entailment is per claim in the data
   today (`claim.entailment`); where it lives visually is the board's call, but do not lose it.
4. **Badges.** Board: "✓ Source linked", "✕ Anchor did not resolve" (danger), "– Unverified"
   (neutral), "✓ Confirmed by a person". Repo (and its tests, `ai_review.rs`): "⛓ Source linked",
   "! Anchor did not resolve" (danger), "! Unverified" (warning), "✓ Confirmed by a person". Same
   words, different glyphs and one different tone. See `GAP_ANALYSIS.md`.
5. **Count line.** "2 of 5 claims" here, but the Outputs board lists seven claims for what is
   plausibly the same output (three summary, one decision, three action items). The fixtures disagree.
   Harmless; the count is the backend's.
6. **"Review the sources" navigation** from the Outputs footer is new; see
   [`recording-outputs.md`](recording-outputs.md) section 3.

## 5. Tagged lists

### EXISTS

- The claim list with a filter, the review note field, Confirm and Reject (`ReviewClaim`), anchors
  with excerpt, Segment, Characters, Alignment origin and the resolved or unresolved badge, Seek to
  this passage when a time exists, the regenerate confirmation, the stale badge, and the
  "N of M claims have to be checked against the transcript." sentence (it is the description of the
  Review required badge).

### CHANGE

- Card framing: `ui.group` today; a `Frame` with the tokens, 2 pt focus stroke on the focused card.
- Filter: `radio_value` buttons today; a segmented control.
- Badges: glyph set, tone for Unverified, tinted frame (see `COMPONENTS.md` 8).
- Anchor box fill `bg_canvas` and the four-column grid; resolution line first.
- Anchored range in the transcript underlined 2 pt in `accent` and the time chip filled. Today the
  transcript fills the playing segment with `faint_bg_color` (`TranscriptStyle::playback_background`,
  which `style_for` sets to `bg_hover`; the board wants `bg_selected`) and draws no anchored range.
- Header counts: the Review required badge becomes the summary line.

### PROPOSED

- The cited-passage span on the waveform (tagged PROPOSED on the board: "today the playhead moves but
  the passage isn't drawn on the waveform").
- The status-line sentence after a seek (tagged PROPOSED).

### NEW DEP

None.

## 6. Accessibility notes

- Resolution is stated in words first ("Anchor resolved in the transcript" or "Anchor did not
  resolve"), glyph second. Colour never decides it.
- The focused card carries both the 2 pt ring and keyboard focus; measure the ring against
  `bg_surface`: 5.50:1 light, 9.38:1 dark.
- Confirm and Reject have distinct names and no shared default; the note field is labelled "Review
  note (optional)".
- `focus_ring` on the filter's `bg_selected` segment: 4.60:1 light, 6.61:1 dark.

## 7. Snapshots that move

None committed for this tab. Tests to update: `crates/ui/tests/ai_review.rs` (glyph strings),
`ai_workflows.rs`.
