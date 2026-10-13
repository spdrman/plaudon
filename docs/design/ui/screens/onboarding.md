# First-run setup

The six-step flow that gates the shell until it is finished. The board draws step two, "Network
policy". Source board `source/Onboarding.dc.html`. Pictures: [light](onboarding-light.png) (1280 by
1100) and [dark](onboarding-dark.png) (1280 by 820).

There is no menu bar, toolbar, rail or status bar here: the onboarding gate replaces the shell until
`onboarding_completed`. Tokens: [`../TOKENS.md`](../TOKENS.md); components:
[`../COMPONENTS.md`](../COMPONENTS.md).

Code: `crates/ui/src/onboarding/mod.rs` (`OnboardingScreen`: `ui_with_tokens`, `navigation`, one fn per
step), state and step names in `crates/app/src/onboarding.rs` (`OnboardingStep`, `OnboardingState`,
`progress_label`), shell wiring in `crates/ui/src/shell/mod.rs` (`onboarding_ui`) and
`settings/shell_adapter.rs` (`OnboardingShellScreen`).

## 1. Regions

The window background is `bg_canvas`. A card 880 by 640, centred (the window is 1280 by 820): fill
`bg_surface`, 1 pt `border_subtle`, `radius.large` 10, clipped; a left column and a right pane.

### 1.1 Left column (250 wide)

Fill `bg_canvas`, 1 pt `border_subtle` on its right edge, padding 24 by 20, gap 18.

| Element | Spec | Token |
| --- | --- | --- |
| Lockup | `plaudon-lockup-light.svg` / `-dark.svg`, 190 wide, name "Plaudon: private audio, searchable knowledge" | brand |
| "Steps" label | `sm` 12, with the PROPOSED tag on the board | `text_secondary` |
| Step list | an ordered list, gap 4; each step is a row (padding 6 by 8, radius 6, gap 10) with a 22 pt round marker (`xs` 11 weight 700) and a label (`md` 13) | |
| Done step | marker is a ✓ on `tint_success` in `status_success`; label `text_primary` | |
| Current step | row fill `bg_selected`; marker filled `accent` with the number in `accent_text`; label weight 600 | |
| Future step | marker 1 pt `border_strong`, number `text_secondary`; label `text_secondary` | |
| Footer note | pinned to the bottom; `sm` 12, line height 1.5: "Plaudon asks for no network access and no operating-system permission during setup." | `text_secondary` |

Step labels on the board, in order: **Privacy** (done), **Network policy** (current), **Speech to
text**, **OpenRouter key**, **Sources**, **Finish**.

### 1.2 Right pane (padding 28 by 32, gap 14)

| Element | Spec | Token |
| --- | --- | --- |
| Progress line | "Set up Plaudon · step 2 of 6", `sm` 12 | `text_secondary` |
| `h1` | "Network policy", `xxl` 24 weight 700 | `text_primary` |
| Paragraph | `md` 13, line height 1.5: "Your recordings, transcripts and summaries stay on this computer unless you explicitly choose a cloud AI provider. Cloud processing is opt-in, per purpose, and never turned on for you." | `text_secondary` |
| Group | `fieldset`, legend "Network policy" (`md` 13 weight 600, 8 below), two option cards, gap 8 | |
| Option 1, selected | card padding 12 by 14, radius 10, **2 pt `focus_ring`** border, fill `bg_selected`, gap 12; a 16 pt radio with a 5 pt `accent` ring; label `md` 13 weight 600 "Local only (recommended) — all outbound network access is refused by the backend." and, under it, `sm` 12 weight 600 `status_success`: "✓ No update check, no model download, no telemetry, no cloud inference" | |
| Option 2 | same card, 1 pt `border_subtle`, fill `bg_surface`, radio 1 pt `border_strong`; text `md` 13, line height 1.5: "Allow cloud AI later — choosing this now grants nothing by itself. No request is ever sent until you pick a provider and consent for that purpose." | |
| Disclosure | card padding 12 by 14, radius 10, 1 pt `border_subtle`, fill `bg_canvas`; header: a down chevron (10 pt) and "What would leave this machine if you opt in later" (`md` 13 weight 600); three bullets at `sm` 12, line height 1.6, `text_primary` (indent 34): "Transcript text you send to a cloud text model, for that request only.", "The model and provider you chose, so the request can be routed.", "Never: audio files, file paths, your API key, or anything you did not select." | |
| Footer | pinned to the bottom, top border `border_subtle`, padding-top 14: secondary **Back** (32 high, padding 0 14), spacer, primary **Next** (32 high, padding 0 18, weight 600) | |

Local only is preselected, matching the gate's first-launch state.

## 2. egui mapping

`CentralPanel` with a centred `Frame` 880 by 640 (`radius.large`, `bg_surface`); inside it a fixed-width
vertical strip (`ui.horizontal` with a fixed width column) for the left side. The lockup image at 190 pt
wide. Network policy is two `radio_value` rows drawn as selectable `Frame`s: the chosen one gets
`bg_selected` plus the 2 pt focus stroke while focused. The disclosure is a `CollapsingHeader` with
three bullets.

## 3. Keys (as built; must not change)

Tab walks the radio rows, the disclosure, Back and Next. Enter on Next advances. **Escape does
nothing** here, because there is no overlay to close (the code consumes it deliberately so a stray press
cannot discard answers). The key step's field is `TextEdit::password` and shows "Key saved to the system
keychain." after hand-over. Focus moves to the new step's heading on each Next, Back and Skip.

## 4. Where the board and the repo disagree

1. **Step names and the step heading are not the app's.** The board claims "Steps, as built: Privacy,
   Network policy, Speech to text, OpenRouter key, Sources, Finish". The repo's `OnboardingStep::title()`
   values, which are the headings shown today, are: "Your recordings stay on this computer", "Local only,
   or allow cloud AI later", "Transcription engines on this machine", "Optional: add an OpenRouter key",
   "Where your audio comes from", "Finish setup". The board's list labels and its step-two heading
   ("Network policy", which is the repo's *legend*) are new, short copy. PROPOSED: add a short name per
   step to `OnboardingStep` for the list, and decide what the pane heading is. Neither the heading text
   nor the list labels are asserted by a test today (`onboarding.rs` asserts only the progress label).
2. **Progress line.** The repo prints "Set up Plaudon" as a heading and, separately, "Step 2 of 6" (capital
   S) as a note, and a test finds it with `get_by_label("Step {n} of 6")`. The board has one line,
   "Set up Plaudon · step 2 of 6". Keep the repo's separate label (the test and the screen reader read it
   as its own item) and place the two beside each other if you want the look.
3. **Where the content lives.** The board's step-two pane carries a paragraph and a disclosure that the
   code shows on step one (`PRIVACY_PROSE_1`, `PRIVACY_DISCLOSURE` and the three bullets are in
   `privacy_step`; step two, `mode_step`, has the legend and the two radios). The footer note in the left
   column is the first sentence of `PRIVACY_PROSE_2`, also step one. What step one should show in the
   redesign is not drawn. Not specified; keep each piece with its code step.
4. **Green line under Local only** and the one-line summary words are new copy (PROPOSED on the board).
5. **Card height.** 640 does not fit the minimum window height (560). Not specified how the card
   behaves there; a `ScrollArea` inside the card is the obvious answer.
6. **Radios.** The repo draws plain `ui.radio` rows. The board draws them as selectable cards.

## 5. Tagged lists

### EXISTS

- The six steps in this order (Privacy, Mode, Engines, Key, Sources, Finish), Back, Next, Skip this
  step on the Key and Sources steps, Finish setup, the Finish review ("Steps you skipped"), no model
  download in Local-only mode, the disclosure with three bullets, the radios with Local only preselected,
  every string quoted above (the paragraph, both radio labels, the bullets, the disclosure title).

### CHANGE

- Centred 880 by 640 card with a left column; the lockup.
- Radios as selectable cards with the selected fill and focus stroke.
- Primary button via `Tokens` and `accent_text`.

### PROPOSED

- The visible step list, with done, current and future markers (tagged PROPOSED on the board).
- The "step 2 of 6" line placement and the green summary line under Local only (tagged PROPOSED).
- Short step names.

### NEW DEP

- The lockup SVG (or a PNG) needs an image loader (`egui_extras` `svg` and `install_image_loaders`,
  or pre-rasterised PNGs). Not used anywhere today.

## 6. Accessibility notes

- Progress is text ("Step 2 of 6"), never only a bar or only the list.
- The step list is an ordered list; the current step is marked in the AccessKit tree, and each
  marker's glyph is not the only indicator (the label weight and the fill change as well).
- The radio group is a `fieldset` with a legend; the chosen card carries both the selected fill and
  the 2 pt ring, so selection is not colour alone.
- Setup asks for no network access and no operating-system permission; keep it that way.

## 7. Snapshots that move

None committed. Tests: `crates/ui/tests/onboarding.rs`, `shell_kittest.rs`
(`onboarding_gates_the_shell_until_setup_is_complete`), `a11y_contract.rs` (the first-run flow).
