#!/usr/bin/env python3
"""Regenerate the README status chips in docs/assets/badges/.

The chips are served from this repository rather than from shields.io, for the
same reason the application ships with Local-only Mode on: reading the README
should not make a request to a third party that can count the reader. The files
are plain SVG, drawn here from the text and the measured glyph widths, with no
script, no external font and no network reference inside them.

Every chip states something the repository can back up, and the README links
each one to the document that backs it. A chip that would need a measurement we
do not have (speed, accuracy, "production ready") does not exist.

Usage:
    python3 scripts/make-readme-badges.py          # write the files
    python3 scripts/make-readme-badges.py --check  # fail if a file is stale

Rendering note. The chips are drawn as <img> by GitHub, so the SVG is rendered
in isolation: no page CSS and no web font. The text therefore names a stack of
fonts that every platform resolves to something, and every run of text carries
`textLength`/`lengthAdjust`, which pins it to the width the box was sized for.
Whatever sans-serif the reader's browser picks, the text cannot spill out of
its box or leave a gap at the end of it.

The widths below are Verdana 11px advances (the metric the layout here is
sized against), measured once from the system font; they are baked in so this
script needs nothing but the standard library.
"""

from __future__ import annotations

import sys
from pathlib import Path
from xml.sax.saxutils import escape

# Verdana 11px advance widths, in pixels.
WIDTHS = {
    "a": 6.6, "b": 6.9, "c": 5.7, "d": 6.9, "e": 6.6, "f": 3.9, "g": 6.9,
    "h": 7.0, "i": 3.0, "j": 3.8, "k": 6.5, "l": 3.0, "m": 10.7, "n": 7.0,
    "o": 6.7, "p": 6.9, "q": 6.9, "r": 4.7, "s": 5.7, "t": 4.3, "u": 7.0,
    "v": 6.5, "w": 9.0, "x": 6.5, "y": 6.5, "z": 5.8,
    "A": 7.5, "B": 7.5, "C": 7.7, "D": 8.5, "E": 7.0, "F": 6.3, "G": 8.5,
    "H": 8.3, "I": 4.6, "J": 5.0, "K": 7.6, "L": 6.1, "M": 9.3, "N": 8.2,
    "O": 8.7, "P": 6.6, "Q": 8.7, "R": 7.6, "S": 7.4, "T": 6.4, "U": 8.1,
    "V": 7.5, "W": 10.9, "X": 7.5, "Y": 6.8, "Z": 7.4,
    "0": 7.0, "1": 7.0, "2": 7.0, "3": 7.0, "4": 7.0, "5": 7.0, "6": 7.0,
    "7": 7.0, "8": 7.0, "9": 7.0,
    " ": 3.9, ".": 4.0, ",": 4.0, ":": 5.0, "(": 5.0, ")": 5.0, "-": 5.0,
    "+": 9.0, "/": 5.0, "|": 5.0, "'": 3.0, "\u00b7": 4.0, "\u2014": 11.0,
}
FALLBACK_WIDTH = 7.0

# The label half of every chip. Dark slate reads on a white page and on a dark
# one, so no chip needs a light-theme and a dark-theme variant.
LABEL_FILL = "#30363d"
TEXT_FILL = "#ffffff"

# Value fills. The design-system accent (crates/ui/src/theme/tokens.rs) for a
# plain fact, green for a property the test suite enforces, amber and rust for
# the two chips that carry a caveat, slate for the build gate. Each one is at
# least 4.5:1 against the white text it carries.
ACCENT = "#1a4fa0"
ENFORCED = "#1a6b3f"
CAVEAT = "#8a5a00"
WARNING = "#8a3b12"
NEUTRAL = "#4d5359"

PAD = 6.0  # horizontal padding either side of a run of text
HEIGHT = 20

# name | label | value | fill
CHIPS = [
    ("licence-mit", "licence", "MIT", ACCENT),
    ("version", "version", "0.1.0 (release candidate)", CAVEAT),
    ("platforms", "platforms", "macOS arm64 \u00b7 Windows 11 x64 untested", CAVEAT),
    ("rust", "rust", "stable \u00b7 1.90+", ACCENT),
    ("privacy", "privacy", "local-first \u00b7 no telemetry", ENFORCED),
    ("build-gate", "build gate", "pre-commit (CI disabled)", NEUTRAL),
    ("status", "status", "pre-release \u00b7 unsigned", WARNING),
]


def text_width(text: str) -> float:
    """Advance width of `text` in Verdana 11px, rounded to a tenth."""
    return round(sum(WIDTHS.get(char, FALLBACK_WIDTH) for char in text), 1)


def chip_svg(label: str, value: str, fill: str) -> str:
    label_text = text_width(label)
    value_text = text_width(value)
    label_box = round(label_text + 2 * PAD, 1)
    value_box = round(value_text + 2 * PAD, 1)
    total = round(label_box + value_box, 1)
    alt = f"{label}: {value}"
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<!-- GENERATED FILE: run `python3 scripts/make-readme-badges.py` to regenerate. -->
<svg xmlns="http://www.w3.org/2000/svg" width="{total:g}" height="{HEIGHT}" \
viewBox="0 0 {total:g} {HEIGHT}" role="img" aria-label="{escape(alt)}">
  <title>{escape(alt)}</title>
  <clipPath id="chip">
    <rect width="{total:g}" height="{HEIGHT}" rx="3"/>
  </clipPath>
  <g clip-path="url(#chip)">
    <rect width="{label_box:g}" height="{HEIGHT}" fill="{LABEL_FILL}"/>
    <rect x="{label_box:g}" width="{value_box:g}" height="{HEIGHT}" fill="{fill}"/>
  </g>
  <g fill="{TEXT_FILL}" font-family="Verdana,DejaVu Sans,Geneva,sans-serif" font-size="11">
    <text x="{PAD:g}" y="14" textLength="{label_text:g}" \
lengthAdjust="spacingAndGlyphs">{escape(label)}</text>
    <text x="{label_box + PAD:g}" y="14" textLength="{value_text:g}" \
lengthAdjust="spacingAndGlyphs">{escape(value)}</text>
  </g>
</svg>
"""


def main(argv: list[str]) -> int:
    check = "--check" in argv[1:]
    out_dir = Path(__file__).resolve().parent.parent / "docs" / "assets" / "badges"
    out_dir.mkdir(parents=True, exist_ok=True)
    stale: list[str] = []
    for name, label, value, fill in CHIPS:
        path = out_dir / f"{name}.svg"
        svg = chip_svg(label, value, fill)
        if check:
            current = path.read_text(encoding="utf-8") if path.exists() else ""
            if current != svg:
                stale.append(str(path))
            continue
        path.write_text(svg, encoding="utf-8")
        print(path)
    if stale:
        for path in stale:
            print(f"stale: {path}", file=sys.stderr)
        print("run: python3 scripts/make-readme-badges.py", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
