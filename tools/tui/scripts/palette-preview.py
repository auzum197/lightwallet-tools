#!/usr/bin/env python3
"""Render the lwtui color tokens as SVG swatch strips for DESIGN.md.

Dependency-free. Emits one SVG per token group into docs/assets/. Not part of
the build; run by hand when the palette changes:

    python3 tools/tui/scripts/palette-preview.py
"""

from pathlib import Path

GROUND = "#181818"
RULE = "#2c2c28"
INK = "#e8e3d3"
DIM = "#8f8a7c"

CHROME = [
    ("bg", "#181818"),
    ("surface", "#1f1f1d"),
    ("ink", "#e8e3d3"),
    ("dim", "#8f8a7c"),
    ("rule", "#2c2c28"),
    ("accent-cyan", "#a8d5d5"),
    ("accent-gold", "#d3a050"),
    ("orange", "#c98443"),
    ("olive", "#a0a344"),
    ("coral", "#c98a72"),
]

GRADIENT = [
    ("tan-dim", "#624e34"),
    ("tan-faint", "#413625"),
    ("sand", "#8b6e52"),
    ("rust", "#7b4331"),
    ("grad-olive", "#676638"),
    ("greige", "#776b58"),
    ("teal-deep", "#39554e"),
    ("teal", "#435c54"),
]

PAD = 16
SW = 104
GAP = 12
RECT_H = 56
LABEL_H = 34
FONT = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"


def strip(tokens):
    n = len(tokens)
    width = PAD * 2 + n * SW + (n - 1) * GAP
    height = PAD * 2 + RECT_H + LABEL_H
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" font-family="{FONT}">',
        f'<rect width="{width}" height="{height}" fill="{GROUND}"/>',
    ]
    for i, (name, hexv) in enumerate(tokens):
        x = PAD + i * (SW + GAP)
        cx = x + SW / 2
        parts.append(
            f'<rect x="{x}" y="{PAD}" width="{SW}" height="{RECT_H}" rx="6" '
            f'fill="{hexv}" stroke="{RULE}" stroke-width="1"/>'
        )
        parts.append(
            f'<text x="{cx:.0f}" y="{PAD + RECT_H + 16}" fill="{INK}" '
            f'font-size="12" text-anchor="middle">{name}</text>'
        )
        parts.append(
            f'<text x="{cx:.0f}" y="{PAD + RECT_H + 30}" fill="{DIM}" '
            f'font-size="11" text-anchor="middle">{hexv}</text>'
        )
    parts.append("</svg>\n")
    return "\n".join(parts)


def main():
    out = Path(__file__).resolve().parent.parent / "docs" / "assets"
    out.mkdir(parents=True, exist_ok=True)
    (out / "palette-chrome.svg").write_text(strip(CHROME))
    (out / "palette-gradient.svg").write_text(strip(GRADIENT))
    print("wrote palette-chrome.svg, palette-gradient.svg")


if __name__ == "__main__":
    main()
