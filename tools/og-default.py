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


def tracked_width(draw, text, font, tracking):
    """The width tracked() would draw text at."""
    return sum(draw.textlength(char, font=font) + tracking for char in text) - tracking


def centred(draw, y, parts, s):
    """Draw tracked runs one after another, the whole line centred on the card.

    Each part is (text, font, fill, tracking). Centred, the square a chat app
    crops the card to keeps the line whole."""
    total = sum(tracked_width(draw, text, font, tracking) for text, font, _, tracking in parts)
    total += sum(tracking for _, _, _, tracking in parts[:-1])
    x = W * s / 2 - total / 2
    for text, font, fill, tracking in parts:
        x = tracked(draw, (x, y * s), text, font, fill, tracking)


def rule(draw, y, s):
    draw.rectangle([MARGIN * s, y * s, (W - MARGIN) * s, (y + 1.5) * s], fill=INK)


def main():
    image = Image.new("RGB", (W * SCALE, H * SCALE), PAPER)
    draw = ImageDraw.Draw(image)
    s = SCALE

    # Mono-caps eyebrow, matching the label recipe used across the site.
    centred(draw, 96, [("THE AUSTRALIAN PARLIAMENTARY RECORD", mono(18 * s), FAINT, 2.2 * s)], s)
    rule(draw, 132, s)

    # Wordmark, ".au" carried in faint like the header.
    centred(
        draw,
        318,
        [
            ("pollywiki", serif(104 * s, weight=700), INK, -2 * s),
            (".au", serif(104 * s, weight=400), FAINT, -2 * s),
        ],
        s,
    )
    centred(draw, 412, [("Every parliament's record, unedited.", serif(34 * s, italic=True), MUTED, 0)], s)

    # The header's double rule, then the nine parliaments it keeps.
    rule(draw, 500, s)
    rule(draw, 504, s)
    centred(draw, 556, [("  ".join(PARLIAMENTS), mono(19 * s), FAINT, 0)], s)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    image.resize((W, H), Image.LANCZOS).save(OUT, "PNG", optimize=True)
    print(f"wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
