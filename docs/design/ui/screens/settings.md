# Settings

One scrolling page of sections with a proposed "On this page" index. The board draws the first three
sections (Privacy & network with Voice profiles, Appearance, AI key); the other eleven are named in
the index and not drawn. Source board `source/Settings.dc.html`. Pictures: [light](settings-light.png)
(1280 by 1100) and [dark](settings-dark.png) (1280 by 820).

Shared chrome: [`../COMPONENTS.md`](../COMPONENTS.md) section 13. Tokens:
[`../TOKENS.md`](../TOKENS.md).

Code: `crates/ui/src/settings/` (`mod.rs` `SettingsScreen`, `section.rs`, `theme.rs`, `privacy.rs`,
`appearance.rs`, `ai_key.rs`, `consent.rs`, `engines.rs`, `models.rs`, `notifications.rs`,
`storage.rs`, `backup.rs`, `export.rs`, `templates.rs`, `shortcuts.rs`, `updater.rs`, `links.rs`,
`destination.rs`, `shell_adapter.rs`). Route `Route::Settings`. `Ctrl+5`. The same destination hosts
Activity history and "PLAUD and sources" (`Route::SettingsActivity`, `Route::SettingsSources`).

State drawn: Local-only mode on and keyboard-focused, voice matching off, appearance Light (light
board) or Dark (dark board), contrast Standard, no key saved.

## 1. Regions

The main region is two columns: a 200 wide index (padding 20 top, 8 right, 16 bottom, 20 left) and
the content (flex; padding 20 top, 24 right, 16 bottom, 12 left; gap 12; clipped).

### 1.1 "On this page" index (PROPOSED)

A label "On this page" (`sm` 12, `text_secondary`) followed by 14 links, `sm` 12, padding 4 by 8,
radius 3, gap 2, no underline. The current section is `text_primary`, weight 600, on `bg_selected`;
the others `text_secondary`.

Entries, in the board's order: **Privacy & network**, **Appearance**, **AI key**, **Engines**, **Local
models**, **Cloud consent**, **Notifications**, **Storage and permissions**, **Backup and restore**,
**Export**, **Default templates**, **Keyboard shortcuts**, **Updates**, **Sources and activity**. Each
entry scrolls its section into view (`scroll_to_rect`). These 14 names are exactly the titles in the
repo.

### 1.2 Content

- `h1` "Settings", `xxl` 24 weight 700.

**Privacy & network card.** `bg_surface`, 1 pt `border_subtle`, `radius.large` 10, padding 14 by 16,
gap 8. The section title is **inside** the card: `h2` `lg` 15 weight 600.

1. Paragraph (`md` 13, line height 1.5, `text_secondary`): "Recordings, transcripts and summaries stay
   on this computer. Cloud AI is opt-in: Plaudon sends nothing to a provider until you choose one and
   consent for that purpose."
2. Row (gap 12, wraps): a checked, keyboard-focused checkbox "Local-only mode (refuse all outbound
   network access)" (`md` 13) and the badge **✓ Local only** (`status_success` on `tint_success`).
3. Policy box: padding 8 by 10, radius 6, fill `bg_canvas`, 1 pt `border_subtle`, `sm` 12, line height
   1.6. Line one "Enforced policy: [the gate's own description]" in `text_primary`; line two "No hosts are
   reachable." in `text_secondary`.
4. A 1 pt `border_subtle` divider with 4 margin.
5. `h3` **Voice profiles** (`md` 13 weight 600).
6. Paragraph (`sm` 12, line height 1.5, `text_secondary`): "Voice matching compares a voiceprint computed
   on this device against the people you have enrolled. A voiceprint is biometric data, it never leaves
   this machine, and matching stays off until you turn it on. A match is only ever a suggestion you
   confirm."
7. Row: an unticked checkbox "Match speakers against enrolled voice profiles" and the badge **– No
   speaker-embedding model in this build** (neutral).
8. Buttons (26 high, gap 8): secondary **Open the People list**, destructive **Forget all voice
   profiles…**.

**Appearance card** (left of a two-column grid, gap 12). Same card style. `h2` "Appearance".

- A `radiogroup` named "Appearance": three chips (28 high, padding 0 10, radius 6, 1 pt border, gap 6
  between chips), each with a 12 pt radio dot (selected: a 4 pt `accent` ring; otherwise 1 pt
  `border_strong`) and the label at `md` 13: **Follow the system**, **Light**, **Dark**. The selected
  chip fills `bg_selected`, border `border_strong`, weight 600.
- Label "Contrast" (`sm` 12, `text_secondary`) and a `radiogroup` named "Contrast": **Standard**
  (selected), **High contrast**.
- Note (`sm` 12, line height 1.5, `text_secondary`): "Follow the system flips light and dark with the
  OS, live. High contrast swaps in the high-contrast pair and a 3 pt focus ring."

**AI key card** (right of that grid). `h2` "AI key".

- Badge **– No key saved** (neutral), left aligned.
- Paragraph (`sm` 12, line height 1.5, `text_secondary`): "Plaudon never displays a saved key and never
  returns it from the backend. Having a key does not grant cloud processing on its own."
- Label "Set OpenRouter API key" (`sm` 12, `text_secondary`) over a 26 high password field.
- Primary **Set key** (26 high, `sm` 12 weight 600).

Status bar: "Ready", Local-only badge.

## 2. egui mapping

One `ScrollArea` of sections, each `settings::section()` drawn as a `Frame` (`bg_surface`,
`radius.large`). Appearance and AI key sit side by side via `ui.columns(2)` when the main width is over
820 pt, stacked otherwise. The index is built from `section_titles()` with a `scroll_to_rect` per entry.
Local-only mode is `egui::Checkbox`. Appearance is two radio groups (`radio_value`) drawn as chips. The
key field is `TextEdit::singleline(..).password(true)`.

## 3. Behaviour (as built; must not change)

- Local-only mode: ticking sends `set_local_only_mode` with `confirmed: true`; the toggle **is** the
  confirmation. It is disabled while the request is in flight. The badge and the "Enforced policy"
  lines render only after the gate answers. Before that the safe reading is on and **no policy sentence
  is composed**. (The board's sentence here is internally loose: it says the badge renders only after
  the gate answers *and* that the safe reading is on beforehand. Read it as: show "Local only" from the
  stored preference's safe default, but never an enforced-policy line until the gate has spoken.)
- Appearance and contrast: `ThemeState::apply` installs both styles with `set_style_of`; following
  the system flips live with the OS. Selecting writes `set_appearance`. **The board and
  `docs/EGUI_DESIGN_SYSTEM.md` section 2 say the choice persists in `shell-state.json` "until
  `set_appearance` exists". It exists now** (`AppCommand::SetAppearance`,
  `crates/ui/src/settings/appearance.rs`, `crates/app/src/commands/settings.rs`). Do not rebuild the
  workaround; the design system document needs a correction when you touch it.
- AI key: handed over once and cleared. Only presence is shown: "Key saved in the system keychain" or
  "No key saved". **Remove** asks first: "Remove the OpenRouter key?" with its disclosure
  (`OPENROUTER_REMOVE_DISCLOSURE`). Having a key never grants cloud processing.
- Keyboard: Tab through the index links, then each section. Radios: arrows move within a group.

## 4. Where the board and the repo disagree

1. **Section order.** Board and index: Privacy & network, Appearance, AI key, Engines, Local models,
   Cloud consent, Notifications, Storage and permissions, Backup and restore, Export, Default templates,
   Keyboard shortcuts, Updates, Sources and activity. Repo (`SettingsScreen::new`): Appearance, Privacy &
   network, AI key, Cloud consent, Notifications, Engines, Default templates, Export, Keyboard
   shortcuts, Storage and permissions, Sources and activity, Local models, Backup and restore, Updates.
   The test `every_section_is_listed_once_and_in_the_published_order` asserts 14 titles and that the
   **first is "Appearance"**, with the comment "appearance is what a first-time reader looks for". The
   board's order needs Appearance and AI key to be neighbours so they can share a row; the repo's
   places Privacy between them. Recommendation: **the board's order wins**. The test pins a taste, not
   a safety rule; the page now opens on the section that carries the product's central promise; and the
   side-by-side layout depends on it. Change the assertion in the same pull request and say why.
2. **Where the title lives.** The repo draws each section's heading as a label above its frame
   (`section::section`: `heading().strong()` then a `Frame` radius 6, margin 12). The board puts an
   `h2` inside the card with 14 by 16 padding and radius 10.
3. **Appearance control.** The repo draws two vertical `radio` columns side by side (Appearance, then
   Contrast, 24 apart). The board draws chips with a dot and puts Contrast under Appearance in one
   card.
4. **Section titles beyond three.** Eleven sections are not drawn. Keep their content; restyle them with
   the same card and the same shared primitives.

## 5. Tagged lists

### EXISTS

- All 14 sections and their content, the Local-only checkbox with its badge and enforced policy
  lines, voice profiles block with its three controls, the two radio groups, the key field with
  presence-only status, set and remove.
- Section headings as accessible headings (asserted in `settings_screen.rs`).

### CHANGE

- Section card style: title inside, `bg_surface`, `radius.large`, 14 by 16 padding.
- `settings::section` and `VisualsTokens`: inside the shell the sections already render through `Tokens`
  (`impl SettingsTokens for Tokens`, `settings/shell_adapter.rs`); rendered alone (tests) they use
  `VisualsTokens` with hard-coded reds, ambers and greens (`settings/theme.rs`). Keep the trait, drop
  the hard-coded fallbacks, or have the fallback read `Tokens::get`.
- Primary and destructive buttons: `section::primary_button` chooses black or white by luminance;
  switch to `accent_text`. `danger_button` is red text only; add the outline.
- Appearance as chips; two-column row for Appearance and AI key when the width allows.
- Badges become tinted frames (`section::status_badge` is text only).
- Section order (above).

### PROPOSED

- The "On this page" index (tagged PROPOSED on the board).

### NEW DEP

None.

## 6. Accessibility notes

- The index entries are links (or buttons) with names equal to the section titles; they move focus to
  the section's heading, not only the scroll position.
- The radiogroups are named ("Appearance", "Contrast"); every chip has a visible label.
- The key field has a visible label; a saved key is never exposed to the accessibility tree.
- The Local-only checkbox is keyboard-focused on the board with the 2 + 2 ring.

## 7. Snapshots that move

None committed. Tests: `settings_screen.rs`, `settings_privacy_group.rs`, `settings_engines_group.rs`,
`settings_backup.rs`, `shell_adapters.rs`. `docs/EGUI_DESIGN_SYSTEM.md` section 2 needs the
`set_appearance` correction.
