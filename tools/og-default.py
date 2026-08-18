#!/usr/bin/env python3
"""Render the site-wide share card, crates/site/assets/public/og-default.png.

1200x630, drawn from the same tokens and vendored fonts the pages use, so the
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
PAPER = "#fafaf7"
INK = "#1c2321"
MUTED = "#5b655f"
FAINT = "#6f6e65"
HAIR_STRONG = "#c9ccbf"

W, H = 1200, 630
MARGIN = 84
SCALE = 2  # supersample, then downscale, so the hairlines stay crisp


def serif(size, weight=400, italic=False):
    name = "newsreader-latin-wght-italic.woff2" if italic else "newsreader-latin-wght-normal.woff2"
    font = ImageFont.truetype(str(FONTS / name), size)
    font.set_variation_by_axes([weight])
    return font


def mono(size):
    return ImageFont.truetype(str(FONTS / "ibm-plex-mono-latin-400-normal.woff2"), size)


def tracked(draw, xy, text, font, fill, tracking):
    """Draw text with letter-spacing, which Pillow has no setting for."""
    x, y = xy
    for char in text:
        draw.text((x, y), char, font=font, fill=fill)
        x += draw.textlength(char, font=font) + tracking
    return x


def main():
    image = Image.new("RGB", (W * SCALE, H * SCALE), PAPER)
    draw = ImageDraw.Draw(image)
    s = SCALE
    left = MARGIN * s

    # Mono-caps eyebrow, matching the label recipe used across the site.
    tracked(
        draw,
        (left, 74 * s),
        "THE AUSTRALIAN FEDERAL RECORD",
        mono(20 * s),
        FAINT,
        2.2 * s,
    )

    # Hairlines top and bottom: the ledger frame.
    for y in (132, 500):
        draw.rectangle([left, y * s, (W - MARGIN) * s, y * s + s], fill=HAIR_STRONG)

    # Wordmark, ".au" carried in muted like the header.
    word = serif(150 * s, weight=700)
    x = left
    draw.text((x, 196 * s), "pollywiki", font=word, fill=INK)
    x += draw.textlength("pollywiki", font=word)
    draw.text((x, 196 * s), ".au", font=serif(150 * s, weight=400), fill=MUTED)

    # Motto in the true italic the site now ships.
    draw.text(
        (left, 392 * s),
        "The federal record, unedited.",
        font=serif(50 * s, weight=400, italic=True),
        fill=MUTED,
    )

    # Footer line: what is actually on the record.
    tracked(
        draw,
        (left, 532 * s),
        "DIVISIONS  ·  BILLS  ·  PEOPLE  ·  ELECTORATES  ·  ELECTIONS",
        mono(20 * s),
        FAINT,
        2.2 * s,
    )

    OUT.parent.mkdir(parents=True, exist_ok=True)
    image.resize((W, H), Image.LANCZOS).save(OUT, "PNG", optimize=True)
    print(f"wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
