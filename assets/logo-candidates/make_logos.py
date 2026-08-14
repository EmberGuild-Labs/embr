#!/usr/bin/env python3
"""Generate pixel-art flame logo candidates for EMBR.

Four concepts, drawn on a 16x16 grid so they stay legible at favicon size and
scale up to any resolution without blurring. Each is written twice:

  * SVG — one <rect> per pixel, `shape-rendering="crispEdges"`, so it stays
    sharp at any size. This is the master.
  * PNG — nearest-neighbour upscale, for previewing and for places that will
    not take an SVG.

Nothing here is wired into the project; these are candidates to choose from.
Edit a grid below and re-run to iterate.

    python3 assets/logo-candidates/make_logos.py
"""

from __future__ import annotations

import struct
import zlib
from pathlib import Path

OUT = Path(__file__).resolve().parent
SIZE = 16          # grid is SIZE x SIZE
SCALE = 32         # PNG pixel size, so 16 * 32 = 512px

# Ember palette. Kept deliberately short — pixel art reads better with few
# colours, and a logo has to survive being printed one colour anyway.
PALETTE = {
    ".": None,              # transparent
    "R": "#B3200A",         # deep red, outer edge
    "O": "#EF5F10",         # orange
    "A": "#FF9A1F",         # amber
    "Y": "#FFD23B",         # yellow
    "W": "#FFF4C9",         # white-hot core
}

# --------------------------------------------------------------------------
# 1. Classic — a single solid flame. The safe, obvious, hardest-to-get-wrong
#    option. Reads as "fire" instantly at any size.
# --------------------------------------------------------------------------
CLASSIC = [
    "................",
    ".......RR.......",
    "......ROOR......",
    "......ROOR......",
    ".....ROAAOR.....",
    ".....ROAAOR.....",
    "....ROAAAAOR....",
    "....ROAYYAOR....",
    "...ROAAYYAAOR...",
    "..ROAYYWWYYAOR..",
    "..ROAYWWWWYAOR..",
    "..ROAYWWWWYAOR..",
    "..ROAAYWWYAAOR..",
    "..ROAAYYYYAAOR..",
    "...ROOAAAAOOR...",
    ".....RROORR.....",
]

# --------------------------------------------------------------------------
# 2. Ember — a smaller flame with detached sparks rising off it. Puts the
#    product's actual name in the mark, and the loose pixels sell the
#    "pixelated" idea harder than a solid shape can.
# --------------------------------------------------------------------------
EMBER = [
    "..........A.....",
    "................",
    "....Y...........",
    ".........Y......",
    "................",
    ".......RR.......",
    "......ROOR......",
    "......ROOR......",
    ".....ROAAOR.....",
    ".....ROAAOR.....",
    "....ROAYYAOR....",
    "...ROAYWWYAOR...",
    "...ROAYWWYAOR...",
    "...ROAAYYAAOR...",
    "....ROOAAOOR....",
    ".....RROORR.....",
]

# --------------------------------------------------------------------------
# 3. Monogram — an "E" knocked out of the flame as negative space. The most
#    brandable of the four, and the only one that still says "EMBR" with the
#    wordmark removed. Also the most fragile at very small sizes.
# --------------------------------------------------------------------------
MONOGRAM = [
    "................",
    ".......RR.......",
    "......ROOR......",
    ".....ROAAOR.....",
    "....ROAAAAOR....",
    "...ROAAAAAAOR...",
    "..ROAAAAAAAAOR..",
    "..ROA.....AAOR..",
    "..ROA.AAAAAAOR..",
    "..ROA....AAAOR..",
    "..ROA.AAAAAAOR..",
    "..ROA.....AAOR..",
    "..ROAAAAAAAAOR..",
    "...ROAAAAAAOR...",
    "....ROAAAAOR....",
    ".....RROORR.....",
]

# --------------------------------------------------------------------------
# 4. Compress — a flame above three bars that shrink and cool as they descend.
#    Says "archive format", not just "fire", which none of the others do. The
#    bars double as a progress/compression metaphor.
# --------------------------------------------------------------------------
COMPRESS = [
    ".......RR.......",
    "......ROOR......",
    "......ROOR......",
    ".....ROAAOR.....",
    "....ROAYYAOR....",
    "...ROAYWWYAOR...",
    "...ROAYWWYAOR...",
    "...ROAAYYAAOR...",
    "....ROOAAOOR....",
    ".....RROORR.....",
    "................",
    "..AAAAAAAAAAAA..",
    "................",
    "....OOOOOOOO....",
    "................",
    "......RRRR......",
]

DESIGNS = {
    "1-classic": (CLASSIC, "Solid flame. Safest, reads instantly at any size."),
    "2-ember": (EMBER, "Flame with rising sparks. Literal 'ember', most pixel-forward."),
    "3-monogram": (MONOGRAM, "'E' knocked out of the flame. Most brandable."),
    "4-compress": (COMPRESS, "Flame over shrinking bars. Says archive, not just fire."),
}

# The design that shipped. Change this and re-run to swap the project's logo
# and the icon macOS shows on .embr files.
CHOSEN = "2-ember"

# Sizes macOS wants in an .iconset. Every one is an exact whole-number multiple
# of the 16x16 grid, so each is pixel-perfect with no resampling anywhere —
# which is the entire reason to draw a logo on a 16px grid in the first place.
ICONSET = [
    ("icon_16x16.png", 1),
    ("icon_16x16@2x.png", 2),
    ("icon_32x32.png", 2),
    ("icon_32x32@2x.png", 4),
    ("icon_128x128.png", 8),
    ("icon_128x128@2x.png", 16),
    ("icon_256x256.png", 16),
    ("icon_256x256@2x.png", 32),
    ("icon_512x512.png", 32),
    ("icon_512x512@2x.png", 64),
]


def check(grid: list[str], name: str) -> None:
    """Pixel art is unforgiving about alignment, so verify the grid is square
    and uses only known colours before rendering anything."""
    if len(grid) != SIZE:
        raise SystemExit(f"{name}: {len(grid)} rows, expected {SIZE}")
    for y, row in enumerate(grid):
        if len(row) != SIZE:
            raise SystemExit(f"{name} row {y}: {len(row)} cols, expected {SIZE}")
        for ch in row:
            if ch not in PALETTE:
                raise SystemExit(f"{name} row {y}: unknown colour '{ch}'")


def hex_to_rgb(h: str) -> tuple[int, int, int]:
    return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))  # type: ignore[return-value]


def write_png(path: Path, grid: list[str], scale: int) -> None:
    """Minimal RGBA PNG writer — no dependencies, and nearest-neighbour
    upscaling is exactly what pixel art wants anyway."""
    w = h = SIZE * scale
    raw = bytearray()
    for y in range(h):
        raw.append(0)  # filter type 0 (None) for each scanline
        row = grid[y // scale]
        for x in range(w):
            colour = PALETTE[row[x // scale]]
            if colour is None:
                raw += bytes((0, 0, 0, 0))
            else:
                raw += bytes(hex_to_rgb(colour)) + b"\xff"

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
           + chunk(b"IEND", b""))
    path.write_bytes(png)


def write_svg(path: Path, grid: list[str]) -> None:
    """One rect per opaque pixel. Runs of the same colour are merged
    horizontally so the file stays small and hand-editable."""
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {SIZE} {SIZE}" '
        f'width="512" height="512" shape-rendering="crispEdges">'
    ]
    for y, row in enumerate(grid):
        x = 0
        while x < SIZE:
            ch = row[x]
            run = 1
            while x + run < SIZE and row[x + run] == ch:
                run += 1
            if PALETTE[ch] is not None:
                parts.append(
                    f'<rect x="{x}" y="{y}" width="{run}" height="1" '
                    f'fill="{PALETTE[ch]}"/>'
                )
            x += run
    parts.append("</svg>")
    path.write_text("\n".join(parts) + "\n")


def write_sheet(path: Path, scale: int = 14, gap: int = 3) -> None:
    """All four side by side on a dark ground, for comparing at a glance."""
    cell = SIZE + gap * 2
    w = cell * len(DESIGNS) * scale
    h = cell * scale
    bg = (24, 20, 26)

    canvas = [[bg for _ in range(w)] for _ in range(h)]
    for i, (grid, _desc) in enumerate(DESIGNS.values()):
        ox = (i * cell + gap) * scale
        oy = gap * scale
        for gy in range(SIZE):
            for gx in range(SIZE):
                colour = PALETTE[grid[gy][gx]]
                if colour is None:
                    continue
                rgb = hex_to_rgb(colour)
                for sy in range(scale):
                    for sx in range(scale):
                        canvas[oy + gy * scale + sy][ox + gx * scale + sx] = rgb

    raw = bytearray()
    for row in canvas:
        raw.append(0)
        for r, g, b in row:
            raw += bytes((r, g, b))

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
           + chunk(b"IEND", b""))
    path.write_bytes(png)


def write_chosen(grid: list[str]) -> None:
    """Write the shipping logo one level up, plus the .iconset macOS needs."""
    assets = OUT.parent
    write_svg(assets / "logo.svg", grid)
    write_png(assets / "logo.png", grid, 32)        # 512px
    write_png(assets / "logo-1024.png", grid, 64)   # for README / stores
    write_png(assets / "logo-32.png", grid, 2)      # favicon

    iconset = assets / "embr.iconset"
    iconset.mkdir(exist_ok=True)
    for filename, scale in ICONSET:
        write_png(iconset / filename, grid, scale)
    print(f"\nchose {CHOSEN} -> assets/logo.svg, logo.png, embr.iconset/")
    print("run macos/build-app.sh to turn the iconset into the Finder icon")


def main() -> None:
    for name, (grid, desc) in DESIGNS.items():
        check(grid, name)
        write_png(OUT / f"{name}.png", grid, SCALE)
        write_png(OUT / f"{name}-32.png", grid, 2)   # favicon-scale preview
        write_svg(OUT / f"{name}.svg", grid)
        marker = "  <-- chosen" if name == CHOSEN else ""
        print(f"{name:<12} {desc}{marker}")
    write_sheet(OUT / "contact-sheet.png")
    print("\ncontact-sheet.png — all four side by side")
    write_chosen(DESIGNS[CHOSEN][0])


if __name__ == "__main__":
    main()
