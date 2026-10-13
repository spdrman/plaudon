# Devices

PLAUD devices found over Bluetooth, grouped into five sections, with the compatibility status recorded
for each exact model and firmware, and a facts pane for the selected device. Source board
`source/Devices.dc.html`. Pictures: [light](devices-light.png) (1280 by 1100) and
[dark](devices-dark.png) (1280 by 820).

Shared chrome: [`../COMPONENTS.md`](../COMPONENTS.md) section 13. Tokens:
[`../TOKENS.md`](../TOKENS.md). The error variants (no radio in this build, permission denied) are on
the Errors board: [`error-states.md`](error-states.md).

Code: `crates/ui/src/screens/devices/` (`mod.rs` `devices_screen`, `sections_ui`, `usb_section_ui`;
`detail.rs` `device_detail`; `presentation.rs` `SECTION_ORDER`, `section_label`, `section_empty`,
`SECTION_NOT_CHECKED`, `support_badge`; `badge.rs`; `wizard.rs`; `transfers.rs`; `shell_adapter.rs`).
Routes `Route::Devices` and `Route::DevicesDevice { peripheral_id }`. `Ctrl+6`.

State drawn: after a scan with two devices heard, one selected, Activity with no unread, status bar
"Scan finished: 2 devices heard". Title "Devices".

## 1. Regions

The main region is a left column (flex, padding 20 top, 20 right, 16 bottom, 24 left; gap 12;
clipped) and a 380 wide **aside** (padding 18, gap 10, left border `border_subtle`, clipped).

### 1.1 Left column

| Element | Spec | Token |
| --- | --- | --- |
| Header row (gap 16) | left: `h1` "PLAUD devices" `xxl` 24 weight 700, then a paragraph; right: primary **Scan again** (28, no wrap) and secondary **Add PLAUD device** (28) | |
| Paragraph | `md` 13, line height 1.5: "Each device is shown with the compatibility status recorded for its exact model and firmware. Scanning asks for Bluetooth permission only when you start a scan." | `text_secondary` |
| Section cards (five) | `bg_surface`, 1 pt `border_subtle`, `radius.large` 10, padding 10 by 12, gap 6; each is a labelled region | |
| Section header | `h2` `md` 13 weight 600 and the count (`sm` 12, `text_secondary`) on one baseline: "1 device", "0 devices" | |
| Empty text | `sm` 12, `text_secondary` | |
| Device row | a full-width button: padding 8 by 10, radius 6, 1 pt `border_subtle`; two lines (name `md` 13 weight 600; facts `sm` 12 `text_secondary`); a badge at the right (padding 0 7, radius 3, `sm` 12 weight 600, line height 20) | |
| Selected row | fill `bg_selected`, **2 pt `focus_ring`** border | |
| USB section | dashed 1 pt `border_strong`, `radius.large` 10, padding 10 by 12, a sentence and a secondary 26 high button at the right | |

Section order is `SECTION_ORDER` and must stay: **Nearby**, **Connected**, **Needs authorization**,
**Unsupported firmware**, **Previously linked**, then the USB card.

On the board:

| Section | Count | Content |
| --- | --- | --- |
| Nearby | 1 device | **PLAUD NOTE** / "Model: PLAUD Note · Firmware: Unknown · Signal: -61 dBm" / badge **? Not evaluated** (neutral). Selected |
| Connected | 0 devices | "No device is connected." |
| Needs authorization | 0 devices | "No scanned device is waiting for authorization." |
| Unsupported firmware | 1 device | **PLAUD NOTEPIN** / "Model: PLAUD NotePin · Firmware: [reported version] · Signal: -74 dBm" / badge **× Unsupported** (danger) |
| Previously linked | 0 devices | "No device has been linked on this computer." |

Empty sentences are `section_empty` verbatim. After a scan that ran, an empty Nearby section says
"No PLAUD devices were heard in this scan."

**USB card.** "**USB (legacy PLAUD Note volume).** This build has no USB device command, so the
recordings are brought in with the ordinary file import: Plaudon only reads the files you choose and
writes nothing to the volume." (the first words in `text_primary` bold, the rest `text_secondary`,
`sm` 12, line height 1.5), and the button **Choose files from the mounted volume** (secondary, 26).

### 1.2 Aside (380): the selected device

| Element | Spec | Token |
| --- | --- | --- |
| Header row | `h2` "PLAUD NOTE" (`lg` 15 weight 600) and the badge "? Not evaluated" (neutral) at the right | |
| Facts | two columns 120 and flexible, `sm` 12, gap 4 by 10: Model **PLAUD Note**; Firmware **Unknown**; Transport **Bluetooth LE**; Advertised name **PLAUD NOTE**; Signal strength **-61 dBm**; Battery **Unknown**; Identity **! Unverified** (`status_warning`, weight 600) | labels `text_secondary` |
| Explanation | `sm` 12, line height 1.5: "An advertisement can be copied, so a name or a signal does not identify this device. It stays unverified until the device accepts its authorization." | `text_secondary` |
| **Capabilities** | `md` 13 weight 600, then two lines (`sm` 12): "✕ Read-only listing of recordings: Not offered. [reason from the claim register]" and "✕ Read-only transfer: Not offered. [reason from the claim register]". The ✕ is `status_danger` weight 600 | |
| **Known limitations** | heading and "[verbatim limitations from the compatibility register]" | `text_secondary` |
| **Radio disclosures** | heading and the paragraph "The legacy protocol may exchange a static per-device credential over a link that is not encrypted at the radio layer. Turning off every internet connection does not make that link private." (the board says it paraphrases the README for the mock; the real text arrives in the DTO) | `text_secondary` |
| **Experimental probe** | heading, a 14 pt checkbox "Allow a read-only experimental probe of this device (listing only, no transfer)", then a secondary 28 button **Check the device again** | |

Bracketed text marks where the register's own words go. Unknown firmware, battery and signal print
"Unknown".

Status bar: "Scan finished: 2 devices heard" left, Local-only badge right.

## 2. egui mapping

Five sections in `SECTION_ORDER`, each a `Frame` with a heading and a count, exposed as a labelled
region. Rows are full-width `Button`s with a two-line label and a badge on the right. Detail is the
screen's aside (`Screen::has_aside`, 380 pt, drawn by the shell's second pane). The badge word is the
register's `status_label`: "Not evaluated" (`?`, neutral), "Unsupported" (`×`, danger), "Experimental"
(`!`, warning). Nothing reads "should work".

## 3. Behaviour (as built; must not change)

- Capabilities, limitations, disclosures and transfer labels arrive in the DTO; the UI composes none of
  them. An empty section after a real scan uses its own sentence; with no usable radio every radio-dependent
  section reads "Not checked: the radio could not be used for this scan." and Previously linked still lists
  what is stored.
- Without the `hardware-scan` feature the screen shows the `UnavailableInBuild` state (see
  `error-states.md`). Authorization, transfer rows with an estimate labelled as one, and disconnect keep
  their current layouts and copy.
- Keyboard: Tab through Scan, Add PLAUD device, each section's rows, the USB button, then the aside.
  Rows are buttons: Enter opens. Opening a row updates `Route::DevicesDevice` as today.

## 4. Where the board and the repo disagree

1. **Detail is drilled-in today, an aside on the board.** In the repo, choosing a row replaces the
   section list with the detail and an "All devices" button (`devices_screen`: `state.selected`
   switches between `detail::device_detail` and `sections_ui`). The board keeps the sections visible
   and puts the detail beside them. This is a structural change, not a restyle, and it needs the
   shell aside (unused by any real screen today). It also changes `shell_adapter.rs`
   (`publish_selection`, `follow_route`).
2. **Detail sections the board omits.** The repo's detail also has "Automatic sync", "Authorize this
   device" and "Recordings on the device" (listing, transfer rows with progress and cancel and retry,
   completeness badge). The board's strip says these "keep their current layouts and copy" without drawing
   them. Keep them and decide where they sit in a 380 pt pane (they will scroll).
3. **Scan label.** The board's primary button reads **Scan again**. In the repo the first-scan state
   is **Scan for nearby devices** (`devices_screen`), and `devices_screen.rs` asserts that label in the
   tab order of the empty screen. "Scan again" exists as a button in the radio-problem and not-found
   states. Use "Scan for nearby devices" before a scan has run and "Scan again" after, which matches the
   board's drawn state.
4. **Heading format.** The repo prints "Nearby (1)" in one heading; the board splits the title and a
   "1 device" count. Both announce the count; the board's form is clearer.
5. **Identity label.** The board prints "Identity: ! Unverified". The repo prints the DTO's
   `identity_label` (`DeviceIdentityAssurance::ui_label()`), and the test says `"Unverified identity"`
   (`devices_screen.rs`). The backend owns the words; use the DTO label.
6. **"Add PLAUD device".** Exists ("Close setup" when open); opens `wizard::setup_wizard`. The board
   only shows the button.
7. **Badge colour.** `badge_ui` and `badge_inline` colour only Warning and Danger and draw
   Success, Neutral, Running and Waiting in the ordinary text colour, so "? Not evaluated" is plain text
   today.

## 5. Tagged lists

### EXISTS

- Five sections in order with their empty and not-checked sentences, the USB card, row buttons, the
  scan, the wizard, the detail content (status, identity, facts, capabilities, limitations,
  disclosures, experimental probe, authorize, sync, listing, transfers).
- Badge words from the register, glyphs `✓ ! ? ×`.

### CHANGE

- Detail into an aside and the sections stay visible (largest item).
- Cards, rows, badges, selected state (2 pt focus ring plus `bg_selected`) restyled to tokens.
- Section headings and counts; the scan button label rule above.
- Capability lines with a danger-coloured ✕ (today plain text), the Capabilities and facts layout in a
  two-column grid.

### PROPOSED

None tagged on the board.

### NEW DEP

None. (The feature `hardware-scan` already exists.)

## 6. Accessibility notes

- Every section is a labelled region and its count is in the heading text.
- A device is never described by colour alone: glyph, word, and an Identity line state its assurance.
- Never "No devices nearby" when the radio could not be used; use the not-checked sentence.
- The aside is a labelled landmark ("Device facts" on the board).
- Contrast of the badge pairs: `status_neutral` on `tint_neutral` 6.12 and 6.11, `status_danger` on
  `tint_danger` 6.68 and 5.55.

## 7. Snapshots that move

None committed. Tests: `crates/ui/tests/devices_screen.rs`, `apps/plaudon-egui/tests/hardware_radio.rs`,
and the a11y sweep `a11y_contract.rs`.
