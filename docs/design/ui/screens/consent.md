# Cloud consent and egress

The sheet a user sees before any content leaves the computer, drawn over a dimmed shell with Local-only
mode off. Source board `source/Consent.dc.html`. Pictures: [light](consent-light.png) (1280 by
1100) and [dark](consent-dark.png) (1280 by 820).

The board's brief title in the overview is "consent sheet, egress refusal". The refusal itself
("Zero retention cannot be confirmed") is described in the mapping strip but not drawn. Its tones
and copy are in section 4.

Shared chrome: [`../COMPONENTS.md`](../COMPONENTS.md) sections 12 and 13. Tokens:
[`../TOKENS.md`](../TOKENS.md).

Code: `crates/ui/src/ai/consent.rs` (`ConsentSheet`), launched from `ai/stage.rs` (`StageDispatch::ui`)
and `ai/models.rs`; copy from `crates/app/src/cloud_presentation.rs` (`ZDR_CAVEAT`,
`CONSENT_DISCLOSURE_VERSION`, `ConsentScopeKind::{label, description}`). Settings has its own consent
list (`crates/ui/src/settings/consent.rs`), not drawn here.

## 1. Regions

### 1.1 Behind the sheet

A skeleton of the Library with the Recording aside. Nothing in it is specified: grey blocks for the
toolbar buttons, the rail and two cards, and an "Outputs" label over a card. The overlay covers the
whole 1280 by 820 window: `overlay` (light `rgba(11,18,32,.45)`, dark `rgba(0,0,0,.6)`). The status bar
underneath shows the warning badge **! Network allowed for providers you enable** (`status_warning` on
`tint_warning`), which is how the user sees that Local-only is off. The toolbar title reads "Library".

### 1.2 The sheet

Position: left 330, top 52, width **620**, `box-sizing: border-box`. Fill `bg_raised`, 1 pt
`border_subtle`, `radius.large` 10, popup shadow, padding 20 by 22, column gap 12. The content scrolls
inside a `ScrollArea`, so the sheet survives the 560 pt minimum window height.

| Element | Spec | Token |
| --- | --- | --- |
| Title (`h2`, id `consent-title`) | "Send this content to a cloud provider?" `xl` 19 weight 700 | `text_primary` |
| Lead paragraph | `md` 13, line height 1.5: "Summarization would be performed by a model reached through OpenRouter. Only what is listed under Data transferred leaves this computer, for this request." | `text_secondary` |
| Facts panel | grid 170 and flexible, row gap 6, column gap 14, `md` 13; padding 10 by 12, radius 6, fill `bg_canvas`, 1 pt `border_subtle` | labels `text_secondary` |
| Checkbox 1 | ticked: "Require documented zero retention. If it cannot be confirmed for this exact route, nothing is sent." | `accent` fill |
| Checkbox 2 | ticked and keyboard-focused (2 + 2 ring): "I accept that the charge for this run cannot be estimated in advance" | |
| Scope group | `fieldset`, legend "How long should this apply?" (`md` 13 weight 600), then three radio rows | |
| Spending cap | label `sm` 12 "Spending cap for a single run (USD)" with a 140 by 26 field whose hint is "Leave empty to keep the current limit" | `text_secondary` |
| ZDR caveat | `sm` 12, line height 1.5: "Zero retention still processes your content outside this device, and it does not retroactively delete provider data, exports, logs or backups created earlier." | `text_secondary` |
| Footer | top border `border_subtle`, padding-top 12, gap 8: "Disclosure version 2026-02-01" (`xs` 11, left), spacer, **Cancel** (30 high, secondary), **Save preference without sending** (30, secondary), **Approve and continue** (30, primary, padding 0 14, weight 600) | |

**Facts rows**, in order (labels in `text_secondary`):

| Label | Value on the board | Tone |
| --- | --- | --- |
| Model | "[model id from the verified list]" | |
| Provider route | "OpenRouter, [resolved provider route]" | |
| Data transferred | "Transcript text of the current revision. Never audio, file paths or your key." | |
| Retention posture | "✓ Zero retention documented for this route" | `status_success`, weight 600 |
| Estimated charge | "! Unknown: no price is advertised for this route" | `status_warning`, weight 600 |

Bracketed text stands for what the backend returns at run time. The UI composes no provider or price
string of its own. An unknown price is "Unknown", never `$0`.

**Scope rows.** Each row: a 16 pt radio, then a label (`md` 13 weight 600) over a description (`sm`
12, `text_secondary`); padding 6 by 8, radius 6, gap 10. The selected row also fills `bg_selected`
so the choice is not only a dot (the board selects the second row).

| Label | Description (exact, from `ConsentScopeKind`) |
| --- | --- |
| Ask every time | Records the preference only. It does not authorise this run, so nothing will be sent now. |
| Allow for this recording | Covers further cloud runs for this recording with this model. |
| Allow for this model policy | Covers any recording processed with this model until you revoke it. |

## 2. egui mapping

`egui::Modal::new(Id::new("cloud-consent-sheet"))` with `backdrop_color(overlay)`, `Frame` fill
`bg_raised`, `radius.large`, popup shadow. Width 620 pt, content in a `ScrollArea`. Facts are an
`egui::Grid` on a `bg_canvas` `Frame`. Scopes are `radio_value` rows; the selected row also fills
`bg_selected`. Whether `Modal::backdrop_color` exists in egui 0.36 is not verified here (the repo does not call it
anywhere); confirm when you wire it.

## 3. Behaviour and keys

- Built-in refusals, as built: the sheet opens on **Ask every time**, which authorises nothing. If
  zero retention is required and the route cannot confirm it, Approve is replaced by the refusal "Zero
  retention cannot be confirmed" and nothing is sent. In the code the Approve button is disabled and a
  Danger badge "Zero retention cannot be confirmed" explains it (`fail_closed` in `ConsentSheet::ui`).
- Focus lands on the first control and stays inside the Modal. Escape closes it as Cancel
  (`close_overlay`, not rebindable). Tab order follows reading order; the primary sits last, on the right.
- Typing wins over shortcuts inside the cap field.

## 4. Where the board and the repo disagree

1. **Cost checkbox.** The board's strip says "an unknown price is 'Unknown', never $0, and Approve
   stays disabled until the cost checkbox is ticked", and draws the checkbox inside the sheet. In the code
   the checkbox ("I accept that the charge for this run cannot be estimated in advance") belongs to
   the dispatch controls in `ai/stage.rs`, **before** the sheet opens; the Start button is disabled until it
   is ticked, and the sheet's Approve is disabled only by the zero-retention refusal. The words are
   identical. Decide whether to move the checkbox into the sheet. If you do, the Start button no longer
   needs it, and the assertion in `ai_tools.rs` that looks for the label moves too. If you do not, drop
   it from the board's sheet.
2. **Footer buttons.** The board shows three buttons at once. The repo has two: **Cancel** and one
   button whose label follows the chosen scope, "Approve and continue" when the scope authorises
   and "Save preference without sending" when it is **Ask every time**
   (`approve_label` in `ConsentSheet::ui`). Showing both together invites saving a scope that authorises
   without sending, which the contract does not define. Keep the single button.
3. **Zero-retention checkbox default.** The board shows it ticked. In the code
   `require_zero_retention` defaults to `false`, so it opens unticked. A pre-ticked box is consent by
   inertia in the safe direction, but it is still a change; I would keep it unticked unless you decide
   otherwise.
4. **Lead paragraph.** The board says the second sentence "is my placeholder for the route's own
   description". The real text is built from `consent_purpose_label`, `model.privacy.relay` and
   `content_type_label`: "{purpose} would be performed by a model reached through {relay}. {content type}". Keep the code's words.
5. **Checkbox wording.** Board: "...nothing is sent." Code: "...the request is refused rather than
   sent." Same meaning; keep the code's.
6. **Extra line.** The code prints "Saved as your per-job budget." under the cap field and the cost's
   detail sentence after Estimated charge. Keep both.
7. **Model line.** The code prints "Model: {display name} ({model id})".
8. **Footer wrapping.** In the picture the footer buttons and the version label wrap onto two lines
   ("Approve and continue" is cut). That is a flaw of the mock at this width; in egui make buttons
   non-wrapping and let the version label take the leftover space.

## 4a. The refusal state (not drawn)

When zero retention is required and the route does not document it: the Approve control is withdrawn
(disabled) and a `status_danger` badge on `tint_danger` reads "! Zero retention cannot be confirmed" with
the sentence "This route does not document zero retention, so the request is refused. Nothing has been
sent." (strings from `ConsentSheet::ui`). The board does not set the tone; the code uses `Tone::Danger`.

## 5. Tagged lists

### EXISTS

- The sheet, all of its copy listed above, the three scopes with `Ask every time` as the default, the
  per-run cap that sends `SetCloudBudget`, the fail-closed refusal, the disclosure version, and the
  decision record sent with `GrantCloudConsent`.

### CHANGE

- Width 620, `ScrollArea`, and the tokens; facts grid with a canvas panel; selected scope row fill;
  larger heading; the Local-only warning visible in the status bar behind it.
- Overlay colour and shadow (the repo uses egui's defaults).

### PROPOSED

- The status-bar Local-only badge (so the warning tone appears while the sheet is open).
- Moving the cost checkbox into the sheet (only if you pick option one above).

### NEW DEP

None.

## 6. Accessibility notes

- `role=dialog`, `aria-modal`, labelled by the title. Keep a focus trap and return focus to the opener.
- The checked state of a checkbox is the tick; the focused one also has the ring. Do not rely on
  colour.
- Retention posture and estimated charge carry glyph and word; the tone only reinforces.
- Text on the sheet: `text_primary` on `bg_raised` 15.17:1 light, 14.48:1 dark; `text_secondary` on
  `bg_canvas` 6.56:1 and 9.03:1.

## 7. Snapshots that move

None committed. Tests that click the footer buttons by label: `ai_workflows.rs` and `ai_tools.rs`
("Approve and continue", "Save preference without sending"); a two-button footer keeps them passing.
