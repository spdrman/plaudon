# Plaudon desktop UI design: handoff

This folder is the design for the egui shell (`crates/ui`, `apps/plaudon-egui`, `crates/app`) and the
written spec for turning it into code. It is meant to be read cold by whoever revises the UI. The
design boards say what the screens look like; these Markdown files say what every number and word on them
is, how it compares with the code today, and what order to change things in.

Nothing in this folder is compiled or run. Nothing in it changes the app.

## 1. What is here

```
docs/design/ui/
  README.md               this file: orientation, index, legend, order of work
  TOKENS.md, tokens.json  every colour, size, type and motion value, with measured contrast
  COMPONENTS.md           every component on the component sheet, plus the shell chrome
  GAP_ANALYSIS.md         design against code, screen by screen, contradictions, work order
  REVISION_PLAYBOOK.md    how to change this repo safely: tests, snapshots, GIF, contrast, conventions
  screens/
    *.md                  one spec per screen group (index below)
    *.png                 a render of every board, light and dark
  source/
    *.dc.html             the 29 design boards
    canvas.json           index: board titles, positions and sizes
    assets/               logos, marks, app icons
```

### Which file for what

| You want to know | Read |
| --- | --- |
| The exact hex of a colour, light and dark; spacing; type; motion; contrast numbers | [`TOKENS.md`](TOKENS.md), or [`tokens.json`](tokens.json) for a program |
| How a button, field, table row, tab, badge, toast, dialog or the rail is built and sized | [`COMPONENTS.md`](COMPONENTS.md) |
| What one screen shows, region by region, with sizes, copy, tokens and egui calls | the matching file in [`screens/`](screens/) |
| What exists in the repo, what differs, what is missing, how big, how risky | [`GAP_ANALYSIS.md`](GAP_ANALYSIS.md) |
| Where the design and a product rule, a test or the code disagree, and who should win | [`GAP_ANALYSIS.md`](GAP_ANALYSIS.md) section 4 and the "Where the board and the repo disagree" section of each screen file |
| How to build, test, regenerate snapshots and the demo GIF, and what to check per screen | [`REVISION_PLAYBOOK.md`](REVISION_PLAYBOOK.md) |

## 2. The boards and the pictures

**The `.dc.html` files need the design canvas runtime to render.** Each one loads `./support.js`, which is
not in this folder, and uses custom elements (`x-dc`, `sc-for`, `sc-if`, `dc-import`) that only that runtime
understands. Opened in a browser on their own they will not draw. They are still useful as text: the
styles are inline, each light board has a `THEMES` object in its script holding the exact light and dark
colour values it uses, and the egui mapping notes are plain text in the strip at the bottom of the board.
`source/canvas.json` lists every board with its size and its position on the canvas.

**The PNGs in `screens/` are the reviewable copy.** They are 1x renders at 1280 pt wide (one image pixel
is one point), so a measurement in the picture is a measurement in points.

| Picture | Size | What it is |
| --- | --- | --- |
| `<name>-light.png` for a screen | 1280 by 1100 | the window (the top 820) plus the board's own egui mapping strip (the bottom 280) |
| `<name>-dark.png` for a screen | 1280 by 820 | the same screen in the dark theme, window only. The dark boards import the light board with `theme="dark"` and the strip switched off |
| `components-light.png`, `components-dark.png` | 1280 by 2000 | the component sheet |
| `tokens.png` | 1280 by 1800 | the token table and measured contrast |
| `brand.png` | 1280 by 960 | logo and app icon usage |
| `overview.png` | 1280 by 820 | the overview board |

Note the name `recording-light.png` is the Recording board's **Transcription tab**; the Outputs and Review
tabs are `recording-outputs-*` and `recording-review-*`.

Some boards annotate with the words **PROPOSED** and **NEW TO THE REPO**. Those are drawn on the board as
small outlined tags and are not part of the UI.

## 3. The product rules the UI has to show

From the overview board; every spec in this folder assumes them.

- **Local-first.** Local-only mode is on from first launch. The badge renders what the egress gate
  answered, never a stored preference alone.
- **No account.** Nothing in the shell asks you to sign in. The PLAUD account route lives behind Devices
  and says it is PLAUD Cloud.
- **Bring your own key.** The OpenRouter key's presence is shown, never its value. A key is never
  consent.
- **Claims are source-anchored.** Every claim says whether its anchor resolved, and seeks only when the
  transcript has real timing.
- **Unverified means unverified.** Hardware reads "Unsupported" or "Not evaluated". Unknown is "Unknown",
  never 0:00 or $0.
- **Never colour alone.** Every status has a glyph and a word.

And what egui is allowed to draw here: panels, `Frame` with `CornerRadius` and `Stroke`, `Grid`,
`ScrollArea`, `egui_extras::TableBuilder`, `Modal`, `Area`, `ComboBox`, `Checkbox`, `Slider`,
`ProgressBar`, `TextEdit`, `selectable_label`; painter shapes for the waveform, ruler, playhead, status
glyphs and transport icons; shadow only on Modal and popups; no blur, no backdrop filter; motion only
through `animate_bool` at the 90 and 160 ms tokens, and a spinner while work runs. The boards target
egui 0.36 and eframe 0.36, which is what `Cargo.toml` pins.

## 4. Screen index

Paths are relative to this folder. "Source board" is the file in `source/`; every dark picture comes
from the board of the same name plus `Dark` (for example `LibraryDark.dc.html` imports `Library.dc.html`).

| Screen | Light PNG | Dark PNG | Source board | Spec |
| --- | --- | --- | --- | --- |
| Overview | [overview.png](screens/overview.png) | not drawn | `Main.dc.html` | this file, section 3 |
| Logo and app icon | [brand.png](screens/brand.png) | one board draws both | `Brand.dc.html` | [screens/brand.md](screens/brand.md) |
| Tokens and measured contrast | [tokens.png](screens/tokens.png) | not drawn | `Tokens.dc.html` | [TOKENS.md](TOKENS.md), [tokens.json](tokens.json) |
| Components | [components-light.png](screens/components-light.png) | [components-dark.png](screens/components-dark.png) | `Components.dc.html` | [COMPONENTS.md](COMPONENTS.md) |
| Library | [library-light.png](screens/library-light.png) | [library-dark.png](screens/library-dark.png) | `Library.dc.html` | [screens/library.md](screens/library.md) |
| Library, empty and importing | [library-empty-light.png](screens/library-empty-light.png) | [library-empty-dark.png](screens/library-empty-dark.png) | `LibraryEmpty.dc.html` | [screens/library-empty.md](screens/library-empty.md) |
| Recording, Transcription tab | [recording-light.png](screens/recording-light.png) | [recording-dark.png](screens/recording-dark.png) | `Recording.dc.html` | [screens/recording-transcript.md](screens/recording-transcript.md) |
| Recording, Outputs tab | [recording-outputs-light.png](screens/recording-outputs-light.png) | [recording-outputs-dark.png](screens/recording-outputs-dark.png) | `RecordingOutputs.dc.html` | [screens/recording-outputs.md](screens/recording-outputs.md) |
| Recording, Review tab | [recording-review-light.png](screens/recording-review-light.png) | [recording-review-dark.png](screens/recording-review-dark.png) | `RecordingReview.dc.html` | [screens/recording-review.md](screens/recording-review.md) |
| Cloud consent and egress | [consent-light.png](screens/consent-light.png) | [consent-dark.png](screens/consent-dark.png) | `Consent.dc.html` | [screens/consent.md](screens/consent.md) |
| Jobs | [jobs-light.png](screens/jobs-light.png) | [jobs-dark.png](screens/jobs-dark.png) | `Jobs.dc.html` | [screens/jobs.md](screens/jobs.md) |
| Bluetooth devices | [devices-light.png](screens/devices-light.png) | [devices-dark.png](screens/devices-dark.png) | `Devices.dc.html` | [screens/devices.md](screens/devices.md) |
| Settings | [settings-light.png](screens/settings-light.png) | [settings-dark.png](screens/settings-dark.png) | `Settings.dc.html` | [screens/settings.md](screens/settings.md) |
| First-run setup | [onboarding-light.png](screens/onboarding-light.png) | [onboarding-dark.png](screens/onboarding-dark.png) | `Onboarding.dc.html` | [screens/onboarding.md](screens/onboarding.md) |
| Error, offline and unavailable states | [error-states-light.png](screens/error-states-light.png) | [error-states-dark.png](screens/error-states-dark.png) | `ErrorStates.dc.html` | [screens/error-states.md](screens/error-states.md) |
| Inbox, search, people, templates, activity, command palette | [more-screens-light.png](screens/more-screens-light.png) | [more-screens-dark.png](screens/more-screens-dark.png) | `MoreScreens.dc.html` | [screens/more-screens.md](screens/more-screens.md) |

Shell chrome (menu bar, toolbar, rail, status bar, activity bell) appears on every screen board and is
specified once, in [`COMPONENTS.md`](COMPONENTS.md) sections 7 and 13.

## 5. Legend for the tags in the specs

Every list of changes in a spec uses these four tags.

| Tag | Meaning |
| --- | --- |
| **EXISTS** | the app does this today. Keep it; the spec says so wherever a restyle could lose it |
| **CHANGE** | existing behaviour, restyled or rearranged. The behaviour stays unless the spec says otherwise |
| **PROPOSED** | new behaviour or content the design suggests, which the app does not do today. The boards mark some of these with a PROPOSED tag; the specs also apply the tag to things the boards forgot to mark, because I checked each against the code |
| **NEW DEP** | needs an egui feature or crate the repo does not use yet: an image loader (`egui_extras` `svg` or `png`, `install_image_loaders`) and the Inter font. The boards call this "NEW TO THE REPO" |

Other conventions: "not specified" means the boards do not say and I did not guess; "(off-scale)" marks a
measurement that is not on the spacing scale; bracketed text such as "[the envelope's message]" on a board
stands for what the backend returns at run time.

## 6. Order of work

The full list, with files and the tests and snapshots each step moves, is in
[`GAP_ANALYSIS.md`](GAP_ANALYSIS.md) section 6. The shape of it:

1. Refresh the committed visual baselines on unchanged code (one is already stale).
2. Tokens: new values, eight new roles, shadows.
3. Shared components: one badge and glyph set, buttons and focus ring, tab strip, progress, error notice,
   toast.
4. Shell chrome: rail, bell, status bar.
5. Screens, in this order: Library and empty state, Jobs, Settings and first run, consent and dialogs,
   Recording (aside, header and player first, then transcript, outputs and review), Devices, the
   smaller destinations.
6. Brand: image loader, marks, app icon.

One pull request per step. Run builds and tests in a container as `REVISION_PLAYBOOK.md` describes.

## 7. Things to know before you start

- **Where the design is wrong, the code usually wins.** The boards' mapping strips claim some things are
  "as built" that the code does not do, and quote a few strings that are not the app's. There are 27
  points where the design and a rule, a test or the code disagree, each with a recommendation, in
  `GAP_ANALYSIS.md` section 4. The biggest are the Jobs
  total chip, the Outputs field subset and "not reported" wording, the consent footer, the deletion
  limits count (five on the board, six in the code), and the section order test.
- **Colour changes are global.** The 20 existing colour values change, so every committed snapshot moves
  in the tokens step. Only eight modules read `Tokens` directly; most surfaces repaint through egui
  `Visuals`, which already follow the tokens.
- **The glyphs ✓ ✕ ◷ render as empty boxes today** (see the committed baseline
  `crates/ui/tests/visual/library_100_rows.png`). The design proposes drawing them with the painter.
- **I did not run anything.** No build, no test, no snapshot. Where a statement needs egui 0.36's source
  (for example `Modal::backdrop_color`) I say it is unverified.
