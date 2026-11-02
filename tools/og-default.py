#!/usr/bin/env python3
"""Render the site-wide share card, crates/site/assets/public/og-default.png.

1200x630, drawn from the same tokens and vendored fonts the pages use, and
laid out as the build's own renderer draws it (crates/site/src/og.rs), so the
card and the site cannot drift apart. Run from the repo root:

    python3 tools/og-default.py

Requires Pillow with WOFF2 support (FreeType built with brotli).
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
FONTS = ROOT / "crates/site/assets/fonts"
OUT = ROOT / "crates/site/assets/public/og-default.png"

# global.css :root
PAPER = "#f6f3ec"
INK = "#161513"
MUTED = "#4d4a43"
FAINT = "#6b675e"

W, H = 1200, 630
MARGIN = 84
SCALE = 2  # supersample, then downscale, so the hairlines stay crisp

PARLIAMENTS = ["Federal", "NSW", "Vic", "Qld", "WA", "SA", "Tas", "ACT", "NT"]


def variable(name, size, **axes):
    """A variable face at the given axis values; axes the face lacks are skipped."""
    font = ImageFont.truetype(str(FONTS / name), size)
    values = []
    for axis in font.get_variation_axes():
        tag = axis["name"].decode() if isinstance(axis["name"], bytes) else axis["name"]
        key = {"Weight": "wght", "Optical size": "opsz", "Optical Size": "opsz"}.get(tag, tag)
        values.append(axes.get(key, axis["default"]))
    font.set_variation_by_axes(values)
    return font


def serif(size, weight=400, italic=False):
    if italic:
        return variable("source-serif-4-latin-wght-italic.woff2", size, wght=weight)
    # The display cut, as a browser picks it for a headline this size.
    return variable("source-serif-4-latin-opsz-normal.woff2", size, wght=weight, opsz=60)


def mono(size):
    return variable("spline-sans-mono-latin-wght-normal.woff2", size, wght=400)


def tracked(draw, xy, text, font, fill, tracking):
    """Draw text on a baseline with letter-spacing, which Pillow has no setting for."""
    x, y = xy
    for char in text:
        draw.text((x, y), char, font=font, fill=fill, anchor="ls")
        x += draw.textlength(char, font=font) + tracking
    return x


def rule(draw, y, s):
    draw.rectangle([MARGIN * s, y * s, (W - MARGIN) * s, (y + 1.5) * s], fill=INK)


def main():
    image = Image.new("RGB", (W * SCALE, H * SCALE), PAPER)
    draw = ImageDraw.Draw(image)
    s = SCALE
    left = MARGIN * s

    # Mono-caps eyebrow, matching the label recipe used across the site.
    tracked(draw, (left, 96 * s), "THE AUSTRALIAN PARLIAMENTARY RECORD", mono(22 * s), FAINT, 2.6 * s)
    rule(draw, 132, s)

    # Wordmark, ".au" carried in faint like the header.
    x = tracked(draw, (left, 312 * s), "pollywiki", serif(156 * s, weight=700), INK, -3 * s)
    tracked(draw, (x, 312 * s), ".au", serif(156 * s, weight=400), FAINT, -3 * s)

    draw.text(
        (left, 428 * s),
        "Every parliament's record, unedited.",
        font=serif(48 * s, italic=True),
        fill=MUTED,
        anchor="ls",
    )

    # The header's double rule, then the nine parliaments it keeps.
    rule(draw, 500, s)
    rule(draw, 504, s)
    draw.text((left, 556 * s), "  ".join(PARLIAMENTS), font=mono(24 * s), fill=FAINT, anchor="ls")

    OUT.parent.mkdir(parents=True, exist_ok=True)
    image.resize((W, H), Image.LANCZOS).save(OUT, "PNG", optimize=True)
    print(f"wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
