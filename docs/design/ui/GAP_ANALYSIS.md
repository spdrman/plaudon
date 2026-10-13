# Gap analysis: design against the code

What has to change in `crates/ui`, `crates/app` and `apps/plaudon-egui` to match the design boards,
screen by screen, with the contradictions called out and a work order at the end. I read the code, not
just the docs; every file and symbol named here was found by looking. Where I could not check
something (anything that needs egui 0.36's source, which is not on this machine) I say so.

**Size** is the effort for the change on that screen: **S** one file, hours; **M** a few files and a
test update, a day or two; **L** several modules, new structure, many tests. **Risk** is how likely the
change is to break behaviour or accessibility, not how hard it is: **Low**, **Med**, **High**.

Companion files: [`TOKENS.md`](TOKENS.md), [`COMPONENTS.md`](COMPONENTS.md), the screen specs in
[`screens/`](screens/), and [`REVISION_PLAYBOOK.md`](REVISION_PLAYBOOK.md).

## 1. Summary

| Screen | Spec | Size | Risk | Headline |
| --- | --- | --- | --- | --- |
| Tokens | [TOKENS.md](TOKENS.md) | M | Med | 14 values change, 8 new roles with no HC values, every baseline moves |
| Shared components | [COMPONENTS.md](COMPONENTS.md) | L | High | four badge renderers become one; buttons, tabs, progress, notice, toast |
| Shell chrome | COMPONENTS.md 7, 13 | M | Med | rail header and tiles, bell pill, Local-only badge, widths |
| Library | [library.md](screens/library.md) | M | Med | row and header heights, title block, selection group, status colours |
| Library empty and import | [library-empty.md](screens/library-empty.md) | S | Low | card, drop pill, toasts with progress |
| Recording, Transcription | [recording-transcript.md](screens/recording-transcript.md) | L | High | first real use of the shell aside; player restyle; transcript chrome |
| Recording, Outputs | [recording-outputs.md](screens/recording-outputs.md) | M | Med | collapsing sections, action table, field subset conflict |
| Recording, Review | [recording-review.md](screens/recording-review.md) | M | Med | card framing, segmented filter, anchor span, tests pin glyphs |
| Consent and egress | [consent.md](screens/consent.md) | S | Med | restyle; three board/code disagreements |
| Jobs | [jobs.md](screens/jobs.md) | M | Med | chips, two-column cards, "All: 14" conflict |
| Devices | [devices.md](screens/devices.md) | L | High | detail becomes an aside, route and selection model change |
| Settings | [settings.md](screens/settings.md) | M | Med | card style, section order, index, appearance chips |
| First-run setup | [onboarding.md](screens/onboarding.md) | M | Low | card with step list, new short step names |
| Error and offline | [error-states.md](screens/error-states.md) | S | Low | error notice with two controls, tones |
| Inbox, search, people, templates, activity, palette | [more-screens.md](screens/more-screens.md) | M | Med | Inbox table or list; palette surface |
| Brand | [brand.md](screens/brand.md) | M | Low | image loader, mark and lockup, icon swap |

## 2. Cross-cutting gaps

These are the findings that shape the rest. Several are bigger than any single screen.

### 2.1 Only eight modules read `Tokens`; everything else paints through egui `Visuals`

`Tokens::get` or `ScreenCtx::tokens()` is called from `settings/shell_adapter.rs`, `shell/bell.rs`,
`shell/nav.rs`, `shell/screen.rs`, `shell/statusbar.rs`, `theme/mod.rs`, `theme/tokens.rs` and
`widgets/toast.rs`. Nothing else in `crates/ui/src` does. The rest reads `ui.visuals()`
(`weak_text_color`, `warn_fg_color`, `error_fg_color`, `strong_text_color`, `extreme_bg_color`,
`faint_bg_color`, `selection.bg_fill`). Because `style_for` in `theme/apply.rs` already maps the token
values onto those visuals, **changing the 20 colour constants repaints most surfaces correctly with no
other edit**. What it cannot fix: anything that needs a new role (tints, `accent_hover`,
`wave_remaining`) or that resolves tones from visuals in a way that discards them (next item).

### 2.2 Status colour is wrong in four places, by construction

| Where | What it does | Effect |
| --- | --- | --- |
| `screens/library/status.rs` `StatusTone::color(&Visuals)` | `Neutral` and `Waiting` use `weak_text_color`; `Success` and `Running` use `text_color` | "Imported" and "Transcribing" are drawn in the plain text colour, not green or blue |
| `ai/badge.rs` `tone_color` | `Running` is `strong_text_color`; `Waiting` and `Warning` share `warn_fg_color`; `Success` is two hard-coded greens `(0x4a,0xc0,0x7a)` and `(0x12,0x6b,0x3f)` | hard-coded colour in a module that is supposed to have none; running is not blue |
| `screens/devices/badge.rs` `tone_color` | only `Warning` and `Danger` are coloured | "? Not evaluated" is plain text |
| `settings/theme.rs` `VisualsTokens::from_visuals` | hard-codes reds, ambers and greens for the fallback | only used when a section is rendered without the shell, but it is a second palette |
| `player/style.rs` `speaker_palette` | six hard-coded hues per theme | the board wants two tints |

There are also **four tone enums**: `plaudon_app::tone::Tone` (six variants),
`screens/library/status.rs::StatusTone` (six), `screens/devices/presentation.rs::Tone` (six) and
`settings/theme.rs::Tone` (four). The design needs one mapping from tone to (text colour, tint colour).
Recommendation: render everything through `plaudon_app::tone::Tone` (it is already the type the
presentation models in `crates/app` produce) and delete the other three. `StatusDescriptor`
(library), `Badge` (devices) and `LocalOnlyStatus` (settings) stay as data; only the colour mapping and
the frame are shared.

### 2.3 Glyphs render as empty boxes today

The committed baseline `crates/ui/tests/visual/library_100_rows.png` shows hollow squares where the
tick (✓), the clock (◷) and the cross (✕) should be, while ⟳ renders. The built-in fonts lack those
glyphs. The design proposes painter-drawn glyphs. The repo uses these characters in many places:
`✓ ✕ ◷ ⟳ ! – ⛓ ⛔ ⧗ ∅ ◆ ⚠ ⤓ ⌂ × ? ○ ●`. Job states use ASCII on purpose (`>`, `?`, `??`, `!r`, `…`,
`OK`, `x-`, `~`, `M`, `S`, `K`, `^`, `^^`, `=`, `+`, `!!`, `x`) and are fine. The painted set the
board names is four glyphs (✓ ✕ ◷ ⊘) plus the ⟳ and ⤓ that already render or are decorative; the others
need a decision, because `⛓ ⛔ ⧗ ∅ ⚠ ◆` are also at risk. I did not render them; check each in a snapshot.

### 2.4 The shell aside has never been used by a real screen

`Screen::has_aside` is implemented only by a test double (`crates/ui/tests/shell_kittest.rs`) and by
`SettingsDestinationScreen` delegating to its children, which all return the default `false`. The
shell machinery exists and is tested: a second `ResizeHandle` divider, `PanelLayout.aside_width` persisted
in `shell-state.json` (default 340, range 240 to 720). The board uses it three times (Recording 330 and
540, Devices 380). Recording currently lays its own side column out inside the main pane
(`DetailLayout` in `screens/recording/mod.rs`).

### 2.5 No image loader

`crates/ui` uses `egui_extras` for `TableBuilder` only; the workspace entry has
`default-features = false`; nothing calls `install_image_loaders` or `include_image!`. The mark, lockup
and monochrome mark (rail, empty state, error state, setup) need one (NEW DEP).

### 2.6 Primary buttons are mostly not primary

Only Settings and onboarding use a primary helper (`settings::section::primary_button`). The Library
empty-state "Import an audio file", the Jobs "Retry job", the review "Seek to this passage" and the
toolbar "Import audio…" are ordinary buttons. The toolbar import button on every board is primary.

### 2.7 Four more things the design assumes exist

- `window_shadow` and `popup_shadow` are never set (`theme/apply.rs`), and `Modal::backdrop_color` is
  never called anywhere. Both are in the boards. I could not confirm the egui 0.36 signatures here.
- The tab strip in the outputs column has no `Role::Tab` and no arrow-key model
  (`screens/recording/ai_panels.rs`). The nav rail has both (`shell/nav.rs`).
- `error_notice` offers one control. Two are drawn (Retry plus the envelope's action).
- `interact_size.y` is 24 (`MIN_TARGET_SIZE`); the boards' screen controls are 28.

## 3. Per screen

### 3.1 Tokens (`crates/ui/src/theme/`)

| | |
| --- | --- |
| Files | `tokens.rs`, `apply.rs`, docs `docs/EGUI_DESIGN_SYSTEM.md` |
| Exists | 20 roles in four themes, spacing, radius, type, motion, focus, contrast tests |
| Differs | 14 colour values change (see `TOKENS.md` 1), overlay colour, nothing else in the scales |
| Missing | 8 roles (`accent_hover`, `wave_remaining`, six `tint_*`) and their high-contrast values; shadow wiring; tint contrast tests |
| Size and risk | M, Med |

Changes: edit `LIGHT` and `DARK`; extend `ColorRoles`, `ColorRole`, `ColorRole::ALL` (typed `[Self; 20]`),
`css_name`, `get`, and both HC constants; add tests for the tint pairs in `TOKENS.md` 6 and for
`accent_hover`; set `window_shadow` and `popup_shadow` in `style_for`; correct
`docs/EGUI_DESIGN_SYSTEM.md` (table 1.1, "twenty roles", "six destinations" which is now seven,
section 2's `set_appearance` note).

The high-contrast themes need decisions: see `TOKENS.md` 1.1. `ThemeState`, `ThemeKind::ALL` and the
existing tests do not change.

### 3.2 Shared components (`crates/ui/src/widgets/` and the four badge modules)

| | |
| --- | --- |
| Files | `widgets/toast.rs`, `widgets/error_notice.rs`, new files in `widgets/` (badge, button helpers, tab strip, progress, glyph painter), `screens/library/status.rs`, `ai/badge.rs`, `screens/devices/badge.rs`, `settings/section.rs`, `settings/theme.rs` |
| Exists | toast behaviour, error notice, `status_badge` x4, nav-style roving focus in `nav.rs` |
| Differs | see 2.2, 2.3, 2.6, 2.7 |
| Missing | tint badge frame, painter glyphs, primary and destructive helpers, tab strip with `Role::Tab`, two-clip progress label, two-control notice, toast tile and progress |
| Size and risk | L, High |

The risk is in the tests, not the drawing: roughly a dozen tests assert the exact text of a badge,
including the glyph (`ai_review.rs`: `"⛓ Source linked"`, `"! Anchor did not resolve"`, `"! Unverified"`;
`ai_templates.rs`: `"◆ Built-in"`; plus status and activity strings). Painting the glyph breaks them
unless the accessible label keeps it. Decide once what the accessible name of a badge is (word, or glyph
and word) and update all of them in one pull request. The product rule is "glyph and word, never colour
alone"; keep a test that proves a badge exposes both somewhere, even if the glyph is a painted shape (for
example by asserting the glyph enum on the model, not the characters).

### 3.3 Shell chrome

| | |
| --- | --- |
| Files | `shell/nav.rs`, `shell/mod.rs` (`body`, `shell_ui`, `skip_link`), `shell/bell.rs`, `shell/statusbar.rs`, `shell/toolbar.rs`, `crates/app/src/shell_state.rs` (`DEFAULT_SIDEBAR_WIDTH` 208 to 216) |
| Exists | menu bar, toolbar with overflow, rail with roles and keys, compact breakpoint, bell with count in words, status bar with spinner and count, skip link |
| Differs | rail is bare `selectable_label`s (no header, tiles, hints); `COLLAPSED_WIDTH` 56 vs 52; bell count is coloured text, not a pill; the skip link sits above the panes, not in the rail; status bar has no Local-only badge; toolbar buttons are 24 high |
| Missing | tile painter, shortcut hint text, header mark (needs 2.5), enforced-policy value in `Shell`, Local-only badge |
| Size and risk | M, Med |

Notes: `Shell::route.title()` already returns "Recording", "Search", "PLAUD and sources" and so on; the
boards' "Library" title on recording screens is the board's mistake. The Local-only badge needs a source
of truth in `Shell`: `Capabilities.preferences.local_only_mode` is documented as "Whether local-only mode
is enforced" and the recording header already reads it that way (`DetailPanels::local_only`); the
stronger "what the gate answered" wording on the boards is satisfied if you read the same field the
recording header reads. If you want the live description sentence, `LocalOnlyModeResponse` only arrives
with `set_local_only_mode`.

Tests that need attention: `shell_kittest.rs` (`every_destination_is_a_tab_and_clicking_one_navigates`,
`a_narrow_window_collapses_the_rail_and_explains_itself`,
`every_interactive_element_is_reachable_by_keyboard`, `the_header_bell_*`), `a11y_contract.rs`, `scale_kittest.rs`.

### 3.4 Library

| | |
| --- | --- |
| Files | `screens/library/table.rs` (`ROW_HEIGHT`, `HEADER_HEIGHT`, `columns`, `header_cell`, `cell`, `footer`), `toolbar.rs`, `mod.rs` (`ui`, `empty_state`, `table`), `status.rs`, `format.rs` |
| Exists | virtualised grid, selection, sorting, filters, context menu, counts, load more, import |
| Differs | row 28 vs 32, header 24 vs 32, `striped(true)`, "↕" on unsorted headers, status colours, no title block, no filter card, selection actions only in the context menu |
| Missing | title block, selection group, thousands separator, filter card frame |
| Size and risk | M, Med |

Conflict: the board has nine columns; `every_column_of_the_spec_is_present_in_the_header` asserts ten
(adds "Last error"). **The repo wins**; see section 4.

### 3.5 Library empty and import

| | |
| --- | --- |
| Files | `screens/library/mod.rs` (`empty_state`), `import.rs` (`drop_overlay`, `import_results_panel`, `describe_batch`), `shell/mod.rs` (`report_import`), `widgets/toast.rs` |
| Exists | empty card with both buttons and the filtered variant, drop path, results panel |
| Differs | `Frame::group` + `heading()`; drop overlay is a framed heading and extension list; import progress is an inline panel, the toast says "Imported N recording(s)" |
| Missing | dashed accent border and pill on hover, mark (2.5), copy note, progress and action in a toast |
| Size and risk | S, Low |

Conflict: "Connect PLAUD" goes to Settings in the code (`LibraryIntent::OpenSettings`), to a setup wizard on
the board. See section 4.

### 3.6 Recording detail (all three tabs)

| | |
| --- | --- |
| Files | `screens/recording/mod.rs` (`RecordingDetailScreen::ui`, `DetailLayout`, `header`, `panel`, `privacy_badge`), `screens/library/screen.rs` (`Screen for LibraryScreen`: add `has_aside` and `aside`), `playback.rs`, `transcript_panel.rs` (`revision_bar`), `ai_panels.rs` (`AiBinding::ui`), `player/{style,transport,waveform,ruler,mod}.rs`, `transcript/{mod,find,chips,style}.rs`, `ai/{outputs,review,transcription,stage,summarize,models,job,badge}.rs` |
| Exists | container, provenance, deletion disclosure, source artifacts, player with scrub, zoom, speaker band, editor with find, undo, autosave, conflict, six tabs, review with anchors and seek |
| Differs | layout, header form, panel headings, player look and sizes, transport controls, speaker band, transcript chrome, tab strip, outputs sections and claim rows, review card framing |
| Missing | aside use, playhead knob, labelled two-tint speaker regions, cited-passage span, outputs footer buttons, segmented filter, compact player strip |
| Size and risk | L, High |

Hotspots:

- `DetailLayout::Wide` today puts the outputs column in the main pane at 32 percent (260 to 420 wide). The
  board's aside is 330 on one tab and 540 on two. How the width follows the tab is not specified.
  `ASIDE_WIDTH_RANGE` is 240 to 720 and the default 340.
- `LibraryScreen` hosts the detail in its `ui`; `has_aside` must be route-dependent (only for
  `Route::LibraryRecording`), and `Screen::aside` must reach `RecordingDetailScreen`'s outputs panels. The
  trait says `has_aside` is asked without a context (`settings/destination.rs` explains the same
  constraint), so the screen has to remember the route.
- `PlayerStyle::from_visuals` is built once in `playback.rs` (line 215 region); a theme change needs the
  tokens read per frame. Played colour is currently `selection.bg_fill`, the pale selection fill.
- Tests: `recording_detail.rs` (the stacked layout), `player_panel.rs`, `player_interaction.rs`,
  `transcript_editor.rs`, `ai_review.rs`, `ai_workflows.rs`, `ai_summarize.rs`, `scale_kittest.rs`.

Conflicts: 3.6 has the most (field subset, "not reported" vs "Unassigned"/"Not specified", seek fallback
copy, unverified tone, "Review the sources" button and the regenerate branch): section 4.

A defect worth fixing in passing: `TranscriptionPanel::engines` (`ai/transcription.rs`) prints the
engine status with `format!("{:?}", engine.status)`, which shows the Rust debug name, not words.

### 3.7 Consent and egress

| | |
| --- | --- |
| Files | `ai/consent.rs`, `ai/stage.rs`, `settings/consent.rs` (not on the board) |
| Exists | the whole sheet with all its words, the fail-closed refusal, scopes, cap, version |
| Differs | width and scroll, facts grid, selected-scope fill, overlay and shadow, heading size |
| Missing | none of substance; the board's three extra behaviours are disputed (section 4) |
| Size and risk | S, Med |

### 3.8 Jobs

| | |
| --- | --- |
| Files | `jobs/mod.rs` (`ui`, `filters`, `row`, `capacity`), `ai/job.rs` (`JobCard`), `jobs/shell_adapter.rs` |
| Exists | everything the board draws except the chips and the card frames |
| Differs | checkboxes vs chips, one column vs two, heading vs 13 weight 600 title, badge placement, plain vs destructive Cancel, extra boxes |
| Missing | pill chips with counts, two-column grid, tinted extra boxes, focused-card stroke |
| Size and risk | M, Med |

Conflict: "All: 14" (section 4). Tests: `jobs_screen.rs`.

### 3.9 Devices

| | |
| --- | --- |
| Files | `screens/devices/mod.rs` (`devices_screen`, `sections_ui`, `usb_section_ui`), `detail.rs` (`device_detail`), `shell_adapter.rs` (`DevicesShellScreen`: `follow_route`, `publish_selection`, add `has_aside` and `aside`), `badge.rs`, `presentation.rs` |
| Exists | all content: sections, USB card, wizard, detail, probe, authorise, sync, listing, transfers |
| Differs | detail replaces the section list ("All devices" button), five headings are "Name (count)", badges mostly uncoloured |
| Missing | aside for the detail with sections still visible, two-column facts, danger ✕ on capabilities |
| Size and risk | L, High |

The risk is the selection model: `DevicesState.selected` currently means "show the detail instead of the
list". With an aside, selection is independent of the list and the aside is shown or hidden by it. Routes
(`Route::DevicesDevice`) and the "Check the device again" flow must keep working. Test:
`devices_screen.rs`, `hardware_radio.rs`.

### 3.10 Settings

| | |
| --- | --- |
| Files | `settings/mod.rs` (`SettingsScreen::new`, `ui_with_tokens`, `section_titles`), `section.rs`, `appearance.rs`, `privacy.rs`, `ai_key.rs`, `theme.rs` |
| Exists | all 14 sections |
| Differs | section order, heading outside the card, radius 6 vs 10, radio columns vs chips, black or white primary text |
| Missing | the "On this page" index, a way to put two sections side by side |
| Size and risk | M, Med |

Conflict: section order and the test that pins "Appearance" first (section 4). Test:
`settings_screen.rs`.

### 3.11 First-run setup

| | |
| --- | --- |
| Files | `onboarding/mod.rs`, `crates/app/src/onboarding.rs` (short names), `settings/shell_adapter.rs` (`OnboardingShellScreen`), `shell/mod.rs` (`onboarding_ui`) |
| Exists | all six steps, skip rules, finish review, escape rule |
| Differs | no card, no left column, plain radios |
| Missing | the step list, short step names, lockup (2.5), green summary line, card scroll for 560 pt windows |
| Size and risk | M, Low |

### 3.12 Errors and offline

| | |
| --- | --- |
| Files | `widgets/error_notice.rs`, `shell/mod.rs` (`loading_ui`, `failed_ui`), `ai/models.rs`, `ai/transcription.rs`, `ai/stage.rs` (offline badge), `screens/devices/mod.rs` |
| Exists | every state on the board with the board's copy |
| Differs | notice frame and controls, offline tone and glyph |
| Missing | tinted frames, mark, a second control |
| Size and risk | S, Low |

### 3.13 Inbox, search, people, templates, activity, palette

| | |
| --- | --- |
| Files | `screens/inbox/mod.rs`, `screens/search/mod.rs`, `screens/people/{mod,roster}.rs`, `screens/templates.rs`, `ai/templates.rs`, `activity/mod.rs`, `shell/palette.rs` |
| Exists | all of them with the board's copy |
| Differs | Inbox is the ten-column table; palette is a Modal; row and card styling |
| Missing | Inbox card list (if chosen), highlight in search hits, avatars, palette columns |
| Size and risk | M, Med |

### 3.14 Brand

| | |
| --- | --- |
| Files | `apps/plaudon-egui/Cargo.toml` and `src/main.rs` (loaders and window icon), `apps/plaudon-egui/packaging/icon.svg`, `icon-1024.png`, `windows/plaudon.ico`, `scripts/make-app-icon.sh`, `docs/licensing/third-party-register.md` |
| Exists | procedural tray icon, five-bar packaging icon, nothing in-app |
| Missing | image loader, all brand assets in the UI, window icon |
| Size and risk | M, Low |

## 4. Where the design contradicts a product rule, a test or the code

For each: what the design says, what holds the other side, and which should win. "Repo wins" means the
design should change, not the code.

| # | Design | Other side | Winner and why |
| --- | --- | --- | --- |
| 1 | Jobs chip "All: 14" (a total) | `jobs/mod.rs` doc: "There is no total... so the screen does not show one"; test `the_state_counts_are_the_backend_s_and_there_is_no_total` | **Repo.** A total the backend never sent is a computed claim. Show "All" without a number |
| 2 | Outputs: missing owner or date reads "not reported" | `backend/outputs.rs` sends "Unassigned" and "Not specified"; `ai/mod.rs` doc: panels compose no user-facing claim | **Repo.** The backend owns the words. Keep `text_secondary` styling only |
| 3 | Outputs: one line per claim, a four-column action table | `claim_fields` renders every §9.2 field verbatim | **Repo.** Keep every field reachable; the board's compact row is the default view |
| 4 | Review: seek fallback copy is `NO_TIMESTAMPS_REASON` | `ReviewPanel::anchor` has a passage-specific sentence; `NO_TIMESTAMPS_REASON` is a caption-export reason | **Repo.** The design's string describes the wrong thing |
| 5 | Review badges "✓ Source linked", "✕ Anchor did not resolve", "– Unverified" (neutral) | tests `ai_review.rs` pin "⛓ Source linked", "! Anchor did not resolve", "! Unverified"; repo tone for Unverified is warning | **Design** on glyphs (the characters render as boxes) and on Unverified being neutral with Review required carrying the warning. Update the tests in the same pull request; keep a test that a badge exposes glyph and word |
| 6 | Offline badge "⊘ Blocked by Local-only Mode" in warning | repo `Tone::Danger` and "⛔" | **Design.** A refusal by the user's own setting is a guided condition; tests assert the label only |
| 7 | Consent: cost checkbox inside the sheet; Approve disabled until ticked | cost acknowledgement is in `StageDispatch` before the sheet; Approve disabled only by the zero-retention refusal | **Repo** unless you choose to move it; either way one place, not both |
| 8 | Consent footer shows "Save preference without sending" and "Approve and continue" together | one button, label follows the scope (`approve_label`); tests click both labels | **Repo.** A single truthful button; the contract defines no "save an authorising scope without sending" |
| 9 | Consent: zero-retention box ticked | defaults to unticked | **Repo** (safe default, pre-ticking is a behaviour change) |
| 10 | Cancel dialog body is a placeholder | `CANCEL_DISCLOSURE` is the §14.7 text | **Repo.** The board says so itself |
| 11 | Delete dialog: "the five DELETION_LIMITS sentences" | six in `app-core/src/deletion.rs`; `recording_detail.rs` asserts all | **Repo.** Also keep the "Audio files" media section the board omits |
| 12 | Jobs retry checkbox: "I understand this starts a new run that may be billed" | `JobCard::controls`: "I understand this request may already have been billed and that retrying may be billed again" | **Repo.** The design reused the Regenerate sentence |
| 13 | Library table has nine columns | `every_column_of_the_spec_is_present_in_the_header` asserts ten including "Last error" | **Repo.** A failed import needs its reason on screen |
| 14 | Settings order starts Privacy & network | `every_section_is_listed_once_and_in_the_published_order` asserts "Appearance" first | **Design.** The test pins a preference, not a rule; the side-by-side Appearance and AI key row needs them adjacent; the page then opens on the promise. Change the assertion with the reason |
| 15 | Devices primary button "Scan again" | `devices_screen.rs` tab-order test expects "Scan for nearby devices" | **Repo** for the empty state; use "Scan again" once a scan has run |
| 16 | Devices Identity "! Unverified" | DTO `identity_label`, test expects "Unverified identity" | **Repo.** The backend owns the words |
| 17 | Onboarding "Set up Plaudon · step 2 of 6" as one line | test finds the label "Step 2 of 6" | **Repo** (separate label, capital S); the look can still put them side by side |
| 18 | Onboarding step names and H1 ("Network policy") "as built" | `OnboardingStep::title()` has different words | **Neither is wrong.** The names are new copy; add short names, keep `title()` for the heading unless you decide otherwise |
| 19 | Outputs footer "Review the sources" "as built" switches tabs; Regenerate is a cloud/local branch | neither exists as drawn; Regenerate always confirms | **Repo** for behaviour (keep the confirmation); the button is new |
| 20 | Library empty: "Connect PLAUD opens the setup wizard" | goes to Settings | **Unresolved.** Pick one; the wizard lives in Devices |
| 21 | Palette is a Modal-free Area | repo uses `egui::Modal` | **Design**, low stakes; keep Escape and focus return |
| 22 | Progress label colours: `accent_text` over the fill (determinate), `text_primary` across the accent segment (indeterminate) | measured 2.32 and 2.23:1 where text crosses the fill; `accent_text` on the pale track about 1.1:1 | **Neither.** Use the two-clip rule in `COMPONENTS.md` 10 |
| 23 | Disabled alpha 0.6 (Tokens) vs 0.5 (components) | | Pick one; 0.5 for buttons, 0.6 for fields is what the boards draw |
| 24 | Header 32 vs 30; modal title 19 vs 15; rail mark 24 vs 26 | | Use 32, 19 (as the Tokens board says), 24 by 28 |
| 25 | Playing transcript row fill `bg_selected` | repo: `faint_bg_color` which `style_for` sets to `bg_hover` | **Design** (selected is the colour the rest of the app uses for "current") |
| 26 | Toolbar title "Library" on recording boards | `Route::title` returns "Recording" | **Repo** |
| 27 | Settings: "persists in shell-state.json until set_appearance exists" | `set_appearance` exists and the appearance section uses it | **Repo.** Stale statement; also fix `EGUI_DESIGN_SYSTEM.md` |

Also internal to the boards (harmless): "2 of 5 claims" on the Review board against seven claims on the
Outputs board; "Elapsed Elapsed time unknown" on a Jobs card; Playback's disabled Play button has no icon
on the Errors board; the toast stack hides the copy note on the empty-library board; the Consent footer
wraps at 620.

## 5. Stale committed state I found

- `crates/ui/tests/visual/shell-ready.png` lists six destinations. The shell has seven (People was
  added). Whatever last generated it ran before People existed, so the baseline would already fail a
  `PLAUDON_VISUAL=1` comparison. Refresh all baselines before the first design change so every later
  diff is only the design.
- `docs/EGUI_DESIGN_SYSTEM.md` and `docs/EGUI_ARCHITECTURE.md` still say "six destinations" (and "twelve
  route variants"); the code has seven destinations and thirteen `Route` variants.
- `apps/plaudon-egui/tests/visual/spike-empty-library.png` is the spike view, not the shell; it depends
  only on colour tokens.
- `apps/plaudon-egui/packaging/icon.svg` ties the icon colour to the accent in a comment.

## 6. Suggested work order

Each step is independently shippable, in this order because each one reduces the next one's risk. A
step regenerates the baselines it moves (container renderer only, see the playbook) and updates the tests
named. "All 13" means every committed baseline, because every one renders colour tokens.

| # | Step | Files | Tests and snapshots to update | Size, risk |
| --- | --- | --- | --- | --- |
| 0 | **Refresh baselines on unchanged code.** No source change | the 13 PNGs | all 13 regenerate (`shell-ready.png` gains People) | S, Low |
| 1 | **Tokens.** New values, 8 roles with HC values, shadows, docs | `theme/tokens.rs`, `theme/apply.rs`, `docs/EGUI_DESIGN_SYSTEM.md` | `tokens.rs` unit tests (add tint pairs); all 13 baselines | M, Med |
| 2 | **Badge and glyph.** One tone-to-colour mapping; tinted badge frame; painter glyphs; retire `StatusTone::color`, `ai::badge::tone_color`, `devices::badge::tone_color`, `settings::section::status_badge` | `widgets/` (new), `screens/library/status.rs`, `ai/badge.rs`, `screens/devices/badge.rs`, `screens/devices/presentation.rs`, `settings/section.rs`, `settings/theme.rs`, `activity/mod.rs`, `screens/recording/mod.rs` | `ai_review.rs`, `ai_templates.rs`, `ai_workflows.rs`, `devices_screen.rs`, `library_screen.rs`, `activity.rs`, `settings_*` tests that assert glyph text; `library_100_rows.png`, `scale-library-*` | L, High |
| 3 | **Buttons, focus ring, tabs.** Primary, secondary, destructive, link; ring 2 pt outside; 28 high; tab strip with `Role::Tab` | `widgets/` (new), `theme/apply.rs` (`interact_size`), `ai/*`, `jobs/mod.rs`, `settings/section.rs`, `screens/recording/ai_panels.rs` | `a11y_contract.rs`, `shell_kittest.rs` focus-order tests; all baselines that show buttons | M, Med |
| 4 | **Progress, error notice, toast.** Two-clip label, two controls, tile and progress | `widgets/error_notice.rs`, `widgets/toast.rs`, `ai/job.rs`, `screens/library/import.rs`, `shell/mod.rs` (`report_import`) | `library_screen.rs` (error and import tests), `jobs_screen.rs`, `shell_kittest.rs` | M, Med |
| 5 | **Shell chrome.** Rail tiles, hints, widths, skip link into the rail, bell pill, Local-only badge in the status bar | `shell/nav.rs`, `shell/mod.rs`, `shell/bell.rs`, `shell/statusbar.rs`, `shell/toolbar.rs`, `crates/app/src/shell_state.rs` | `shell_kittest.rs`, `a11y_contract.rs`, `scale_kittest.rs`; `shell-ready.png`, `scale-shell-ready@*` (the only baselines that render the shell; the library and detail cases draw the screen alone) | M, Med |
| 6 | **Library and Library empty.** Heights, columns kept, title block, selection group, filter card, empty card, drop state | `screens/library/{table,toolbar,mod,import,status}.rs` | `library_screen.rs`, `library_perf.rs` (budget), `scale_kittest.rs`; `library_100_rows.png`, `library_empty.png`, `scale-library-*` | M, Med |
| 7 | **Jobs.** Chips, cards, extra boxes | `jobs/mod.rs`, `ai/job.rs` | `jobs_screen.rs`; new Jobs baselines | M, Med |
| 8 | **Settings and onboarding.** Cards, order, index, chips, step list | `settings/{mod,section,appearance,privacy,ai_key}.rs`, `onboarding/mod.rs`, `crates/app/src/onboarding.rs` | `settings_screen.rs` (first title assertion), `settings_privacy_group.rs`, `onboarding.rs`, `shell_adapters.rs`; new baselines | M, Med |
| 9 | **Consent, dialogs, errors.** Sheet restyle, overlay, danger fill, focus start on the safe button | `ai/consent.rs`, `ai/stage.rs`, `screens/recording/deletion.rs`, `settings/section.rs` (`confirm_modal`), `screens/library/duplicate.rs` | `ai_workflows.rs`, `ai_tools.rs`, `recording_detail.rs`, `library_screen.rs` | S, Med |
| 10 | **Recording detail: aside, header, player.** Aside for outputs; header; player restyle; speaker regions | `screens/recording/mod.rs`, `screens/library/screen.rs`, `playback.rs`, `player/*` | `recording_detail.rs`, `player_*`, `scale_kittest.rs`; `scale-recording-detail@*` | L, High |
| 11 | **Recording detail: transcript, outputs, review.** Chrome, sections, action table, review cards, filter, anchor span | `transcript/*`, `screens/recording/transcript_panel.rs`, `ai/{outputs,review,transcription,stage}.rs` | `transcript_editor.rs`, `ai_review.rs`, `ai_workflows.rs`, `ai_summarize.rs` | L, High |
| 12 | **Devices.** Aside, section cards, facts | `screens/devices/*` | `devices_screen.rs`, `hardware_radio.rs` | L, High |
| 13 | **More screens.** Inbox list, search highlight, people rows, templates rows, activity rows, palette | `screens/{inbox,search,people,templates}`, `ai/templates.rs`, `activity/mod.rs`, `shell/palette.rs` | `people_screen.rs`, `ai_templates.rs`, `activity.rs`, `shell_kittest.rs` | M, Med |
| 14 | **Brand.** Image loader (NEW DEP), rail and empty-state marks, setup lockup, window icon, app icon swap, optional Inter | `apps/plaudon-egui/*`, `crates/ui` (loader use), `packaging/*`, licence register | `packaging.rs`; baselines with marks | M, Low |

Why this order: steps 1 to 4 are the shared substrate; every later screen step is then small. 5 and 6 give
the visible win early. 10 to 12 are the structural ones and go last because they depend on 2, 3 and 5.
Step 14 can move earlier if you want the marks sooner; nothing depends on it except 5's rail header and 6's
empty-state mark, which can ship as text first.

## 7. What I did not verify

- egui 0.36 API details the boards depend on: `Modal::backdrop_color`, `Shape::dashed_line` and
  `Shadow` field types. The repo does not call the first two. Check at compile time.
- How the boards' off-scale spacing (6, 10, 14, 18, 20, 22) should round; no board says.
- Whether the glyphs other than ✓ ✕ ◷ ⟳ render in egui's built-in fonts; check each in a snapshot.
- Anything that needs a build or a test run: I did not run any.
