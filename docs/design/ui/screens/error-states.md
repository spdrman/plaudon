# Error, offline and unavailable states

Six panels on one board, each showing what the main region says in that state. Source board
`source/ErrorStates.dc.html`. Pictures: [light](error-states-light.png) (1280 by 1100) and
[dark](error-states-dark.png) (1280 by 820).

Unlike the screen boards, this one has no shell around it. The page is 1280 by 820, padded 24 by 28,
with a title and a 3 by 2 grid of panels (gap 16). Each panel: `bg_surface`, 1 pt `border_subtle`,
`radius.large` 10, clipped, with a header strip (`xs` 11 weight 600, `text_secondary`, padding 8 by 12,
`bg_canvas`, bottom hairline) naming the case in capitals. The header strips are board annotations, not
UI.

Components: [`../COMPONENTS.md`](../COMPONENTS.md) sections 10, 11 and 12. Tokens:
[`../TOKENS.md`](../TOKENS.md).

Code: `crates/ui/src/widgets/error_notice.rs` (`error_notice`, `control_for`),
`crates/ui/src/shell/mod.rs` (`loading_ui`, `failed_ui`), `crates/app/src/job_presentation.rs`
(`error_guidance`), `crates/ui/src/settings/engines.rs` (offline sentence),
`crates/ui/src/screens/devices/` (`radio_heading`, `SECTION_NOT_CHECKED`),
`crates/ui/src/screens/recording/playback.rs` (`no_audio_output`).

## 1. The six panels

### 1.1 STARTUP, capabilities failed

Shown before the shell paints when `get_capabilities` fails. Centred column, gap 10, padding 16, no
rail.

| Element | Spec | Token |
| --- | --- | --- |
| Mark | monochrome blue mark, 40 high | brand |
| `h2` | "Plaudon could not start", `xl` 19 weight 600 | `text_primary` |
| Message | "[the envelope's message, as the backend wrote it]", `md` 13, line height 1.5 | `text_secondary` |
| Id | "Diagnostic id [id]", mono 12 | `text_secondary` |
| Button | primary **Try again** (30 high, padding 0 14, weight 600) | `accent` |

EXISTS: `failed_ui` prints a heading with the same words, the message, "Diagnostic id {id}" and **Try
again**. CHANGE: the mark and the primary style.

### 1.2 STARTUP, loading

A spinner (28), "Loading your library…" (`role=status`, `md` 13), and the note "The spinner is the only
continuous animation and stops with the work." (`sm` 12, `text_secondary`; this line is a board
annotation, not UI). EXISTS: `loading_ui` has a `Spinner` and the label.

### 1.3 LIBRARY, load failed (error notice)

`h2` "Recordings" (`xl` 19 weight 700), then an error notice (`role=alert`): fill `tint_danger`, radius 6,
padding 12, gap 8.

1. "✕ Recordings could not be loaded." (`md` 13 weight 600, `status_danger`).
2. "[message from the error envelope]" (`md` 13, line height 1.5, `text_primary`).
3. "Diagnostic id: [id]" (mono 12, `text_secondary`).
4. Buttons (gap 8, 28 high, `sm` 12): primary **Retry**, secondary **Open diagnostics**.

Note under it (annotation): "The second control comes from the envelope's action: Open settings, Open
storage settings, Review cloud consent and so on."

The repo draws `Frame::group` with a red "✕ {context} {message}" label, **one** control (Retry or the
settings button, never both), and the id in small weak text. The board's two controls (Retry plus the
action) are a change in `ErrorControl` (a single value today). See `COMPONENTS.md` 11.

### 1.4 OFFLINE, cloud run refused by Local-only mode

| Element | Spec | Token |
| --- | --- | --- |
| Row | "Summarize" (`md` 13 weight 600) and the badge "⊘ Blocked by Local-only Mode" | `status_warning` on `tint_warning` |
| Notice (`role=alert`) | fill `tint_warning`, radius 6, padding 12, gap 8 | |
| Title | "! You are offline" (`md` 13 weight 600) | `status_warning` |
| Body | "Local-only mode is on, so choosing it will not make a request: Plaudon refuses outbound network access until you turn Local-only mode off." (`md` 13, line height 1.5) | `text_primary` |
| Buttons | secondary **Review Local-only Mode**, secondary **Choose local engine** (28) | |

Annotation: "Nothing was sent. The gate refused the request in Rust; this notice reports it."

The tone is the warning tone, not danger: a refusal by the user's own setting is a guided condition, not
a failure. In the repo the badge is `Tone::Danger` with the glyph "⛔" (`ai/models.rs`,
`ai/transcription.rs`) and the guidance title "You are offline" appears as a strong label above the
control buttons (`JobCard::failure`, via `error_guidance`). Tests assert the label text only, so the
tone and glyph change freely.

### 1.5 DEVICES, no radio in this build

A neutral box (`tint_neutral`, radius 6, padding 10 by 12, `md` 13 weight 600, `status_neutral`): "– This
build cannot scan for Bluetooth devices". Then four rows (gap 8; each padding 4 by 0 with a hairline):
the section name (`sm` 12 weight 600) over "Not checked: the radio could not be used for this scan."
(`sm` 12, `text_secondary`): Nearby, Connected, Needs authorization, Unsupported firmware. Annotation:
"Never 'No devices nearby'. Previously linked still lists what is stored."

EXISTS: `radio_heading(RadioState::UnavailableInBuild)`, `SECTION_NOT_CHECKED`, and Previously linked
that still lists what is stored. CHANGE: the neutral box and the row styling.

### 1.6 DEVICES, permission denied; PLAYER, no output

Two stacked blocks (gap 10).

- Error notice (`role=alert`, `tint_danger`, padding 10 by 12): "✕ Bluetooth access is not permitted"
  (`md` 13 weight 600, `status_danger`); "Check that Bluetooth permission is allowed for Plaudon in your
  system settings." (`sm` 12, line height 1.5); secondary **Open permission settings** (26).
- A bordered card (1 pt `border_subtle`, radius 6, padding 10 by 12, gap 6): "Playback" (`sm` 12 weight
  600); a **disabled** Play button (30 by 26, 0.5 opacity, named "Play" on the board; the mock draws no
  icon in it, which is an omission, give it the triangle) beside "Duration unknown"; then "this build
  was started without an audio output device" (`sm` 12, `text_secondary`).

## 2. egui mapping

`widgets::error_notice` is one `Frame`: `tint_danger` fill (`tint_warning` for a guided condition),
the status glyph and title in the tone colour, the envelope message in `text_primary`, the
diagnostic id in monospace, then the controls `ErrorAction` implies. It renders the envelope; it never
classifies or rewrites the failure.

Startup: a centred `Spinner` with "Loading your library…", or "Plaudon could not start" with **Try
again**. Both use `ui.vertical_centered` in a `CentralPanel`, no rail.

Offline guidance: `job_presentation::error_guidance` maps `LocalOnlyMode` to "You are offline" with
**Review Local-only Mode**, `StorageFull` to "Not enough space", `BudgetExceeded` to "Provider credits
exhausted" and `OutcomeUnknown` to "Cloud request outcome unknown". The body sentence on the offline
panel is the Engines section's own copy (`settings/engines.rs`).

Unavailable, not empty: `RadioState::UnavailableInBuild` and a denied radio each get their own
sentence; neither looks like an empty scan. A missing audio device hides the slider and says so in the
backend's words. Disabled controls draw at 0.5 alpha and stay in the AccessKit tree with their names.

## 3. Behaviour and keys

All unchanged. The notices are `Role::Alert`. The controls are normal buttons in the Tab order. Disabled
controls remain focusable for a screen reader (named, disabled).

## 4. Tagged lists

### EXISTS

- Startup loading and failure with retry; the library load-failed notice with Retry or a settings
  control; the offline guidance and its two controls; the radio states ("Not checked", the
  unavailable-in-build heading, permission denied); the no-audio-output state with "Duration unknown".

### CHANGE

- Error notice frame, tones, monospace id, two controls (see above).
- Offline badge tone and glyph (`⊘`, warning).
- Startup panels get the mark and primary button.

### PROPOSED

None tagged on this board beyond the shared glyph drawing.

### NEW DEP

- The monochrome mark image (same loader need as the empty library).

## 5. Accessibility notes

- Every notice starts with a glyph and a word ("✕", "!" and "–" with the sentence); colour is only a
  reinforcement.
- `role=alert` on the error and offline notices so a screen reader announces them when they appear.
- Disabled Play stays in the tree with its name; "Duration unknown" is the reason, in words.
- Text on `tint_danger`: `status_danger` 6.68 light and 5.55 dark; on `tint_warning`: 5.85 and 6.09;
  on `tint_neutral`: 6.12 and 6.11.

## 6. Disagreement worth knowing

The mapping strip says disabled controls draw at 0.5 alpha; the Tokens board says 0.6 for disabled text.
See `TOKENS.md` section 7.

## 7. Snapshots that move

None committed. Tests: `crates/ui/tests/settings_engines_group.rs` (the offline sentence),
`devices_screen.rs`, `hardware_radio.rs`, `library_screen.rs`.
