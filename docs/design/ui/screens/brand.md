# Logo and app icon

How the mark, the lockup and the app icon are used in the interface, and which brand colours the UI
may use. Source board `source/Brand.dc.html`, picture [`brand.png`](brand.png) (1280 by 960, one
board, no separate dark picture; the board draws both palettes itself). Assets are in
`source/assets/`.

Tokens: [`../TOKENS.md`](../TOKENS.md) section 8 has the colour table and contrast numbers.

## 1. Assets

| Asset | File | Use |
| --- | --- | --- |
| Mark, light palette | `source/assets/plaudon-mark-light.svg` | rail header on the light theme, About |
| Mark, dark palette | `source/assets/plaudon-mark-dark.svg` | rail header on the dark theme |
| Monochrome blue, light | `source/assets/plaudon-mono-blue-light.svg` | empty and error states (56 and 40 high), below 24 pt, the tray |
| Monochrome blue, dark | `source/assets/plaudon-mono-blue-dark.svg` | same, dark theme |
| Monochrome white | `source/assets/plaudon-mono-white.svg` | on a royal `#0b57ff` tile (96 by 112, radius 10, mark 72 high) |
| Lockup, light | `source/assets/plaudon-lockup-light.svg` | first-run setup (190 wide), About |
| Lockup, dark | `source/assets/plaudon-lockup-dark.svg` | same |
| Desktop icon exports | `source/assets/app-icon-256.png`, `-32.png`, `-16.png` | the brand kit's desktop icons |
| "Ships today" | `source/assets/packaging-icon-today.png` | the current packaging icon, for comparison only |

The repo's own copy of the kit is `brand/kit/` (`logo/`, `icons/desktop/` with PNGs 16 to 1024,
`plaudon.icns` and `plaudon.ico`). The board says "Ribbon Pulse, from brand/kit/logo. Geometry
untouched; the palette is chosen per theme." Do not edit the SVG geometry. The SVGs in
`brand/kit/logo/` switch palette with `prefers-color-scheme` inside the file; egui cannot read that
from inside an SVG, so ship the two pinned variants in `source/assets/` and choose by theme.

Mark palettes (from the board): light body `#0585fc` to `#0029b0`, highlight `#46c9fc`, fold `#04185a`;
dark body `#5cb4ff` to `#5470ee`, fold `#3a58cc` (so the fold does not sink into the canvas). Monochrome
blue `#0b57ff` (5.50:1 on white) or, dark, `#6ea8ff` (7.76:1 on the dark canvas); white on royal 5.50:1.

## 2. Where the logo appears in the app

| Place | Asset | Size |
| --- | --- | --- |
| Nav rail header | mark (theme-pinned) with the word "Plaudon" (`lg` 15, weight 600 to 700) | board text says 24 pt; the screen boards draw it 26 high; the egui mapping says `fit_to_exact_size(vec2(24.0, 28.0))`. Use 24 by 28 (the aspect ratio of the SVG viewBox, 650 by 763) |
| Collapsed rail | mark alone | 20 pt |
| First-run setup, About | lockup | 190 wide in setup (40 high in the board's "where it appears" row) |
| Empty library, startup failure | monochrome blue mark | 56 and 40 high |
| Tray | monochrome | the board says "Use these below 24 pt and in the tray" |

Clear space is a quarter of the mark's height. Minimum 16 px for the mark and 96 px wide for the lockup,
"as the kit says".

## 3. App icon

The board shows the kit's desktop icon at 256 (drawn 96), 32 and 16, and beside them the icon that ships
today.

**PROPOSED.** `apps/plaudon-egui/packaging/icon.svg` is still the earlier five-bar tile in `#1a4fa0`. The
board proposes swapping the bundle, the `.icns`, the `.ico` and the eframe window icon to the kit's
desktop exports so the dock matches the rail.

What that touches in the repo (checked):

- `apps/plaudon-egui/packaging/icon.svg` (source), `icon-1024.png` (master raster) and
  `windows/plaudon.ico`, rebuilt with `scripts/make-app-icon.sh`; the macOS `.icns` is built from the
  master by `scripts/package-macos-egui.sh` at package time.
- `apps/plaudon-egui/tests/packaging.rs` asserts the master is a 1024 square and the `.ico` carries 16,
  32, 48 and 256 pixel images. The kit's `plaudon.ico` and 1024 PNG are expected to satisfy both;
  check, do not assume.
- The comment in `icon.svg` says the icon uses "the design-system light-theme accent #1a4fa0", so the
  icon cannot drift from the palette. After the token change that sentence is wrong either way: either
  the new icon is the brand mark (and the comment goes) or the file is kept and re-coloured to `#0a4fe0`.
- The window icon: `viewport()` in `apps/plaudon-egui/src/lib.rs` sets no icon today (no `with_icon`).
  The tray icon is drawn by `status_icon_rgba()` in `tray.rs`, not from an image file.

## 4. Brand colours in the UI

From the board:

| Colour | Hex | On white | Verdict |
| --- | --- | --- | --- |
| Highlight | `#46c9fc` | 1.91:1 | fail |
| Sky | `#56c9ff` | 1.88:1 | fail |
| Teal | `#17c7c7` | 2.09:1 | fail |
| Azure | `#2f86ff` | 3.51:1 | UI only |
| Royal | `#0b57ff` | 5.50:1 | pass |
| Ink | `#172544` | 15.17:1 | pass |

The cyan family lives inside the mark only. It never carries text, a status, a focus ring or a waveform.
Royal becomes the focus ring; the accent is a slightly deeper `#0a4fe0` so white text on it clears 6.5:1
(measured 6.55). Ink becomes `text_primary`.

I recomputed all six ratios; they match to two decimals.

## 5. egui mapping

egui cannot read `prefers-color-scheme` inside an SVG, so ship both pinned variants and choose by
`Tokens::get(ctx).theme.is_dark()`. Paint with
`egui::Image::new(include_image!("…light.svg")).fit_to_exact_size(vec2(24.0, 28.0))`.

**NEW DEP.** `include_image!` needs an image loader: `egui_extras` with the `svg` feature (resvg) and
`egui_extras::install_image_loaders`. `crates/ui` uses `egui_extras` for tables only today, and the
workspace entry is `egui_extras = { version = "0.36", default-features = false }`. Pre-rasterised PNGs
at 1x and 2x with the `png` feature are the lighter alternative (no SVG renderer in the dependency tree).
Either choice needs a licence-register row if it adds a crate (`docs/licensing/third-party-register.md`);
`cargo deny` runs in the pre-commit gate.

Where to install the loaders: once at startup in `apps/plaudon-egui` (the binary owns the context
setup), because `plaudon-ui` has no operating-system or setup responsibilities. Widgets then use
`egui::Image`.

## 6. Tagged lists

### EXISTS

- Nothing from this board is in the shell yet: the rail header has no mark, the empty and error
  states have no image, the first-run screen has no lockup.

### CHANGE

- App icon assets as in section 3.

### PROPOSED

- Everything in sections 2 and 3 (the tagged PROPOSED item on the board is the icon swap).

### NEW DEP

- Image loading for SVG or PNG (section 5).

## 7. Notes

- The board draws the dark lockup on `#0b1220` and the light one on white. Both rely on the lockup's own
  wordmark colours (ink and white); do not recolour them.
- The board's "ships today" icon is not an endorsement; it is what the packaging produces now.
- Brand colours are not tokens. The only brand-derived tokens are `text_primary` (light, ink),
  `focus_ring` (light, royal) and `accent` (the deeper blue).
