"""Generates assets/rcap.ico (stdlib only): `python assets/make_icon.py`.

The icon is the rcap window itself: dark frame and title bar, a see-through
hole drawn as a transparency checkerboard, and a red record dot trailing
onion-skin copies of itself (an animated GIF being captured). Sizes <= 24px
use a simplified design so they stay crisp.

Shapes are defined on a 256x256 canvas and rendered per size with 4x4
supersampling.
"""

import struct
import zlib
from pathlib import Path

SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
SS = 4

DARK = (0x2E, 0x2E, 0x34)
BODY = (0xF4, 0xF4, 0xF5)
CHECK_A = (0xFF, 0xFF, 0xFF)
CHECK_B = (0xD4, 0xD4, 0xDA)
RED = (0xEF, 0x44, 0x44)
WHITE = (0xFF, 0xFF, 0xFF)


def in_rrect(x, y, x0, y0, x1, y1, r):
    if not (x0 <= x < x1 and y0 <= y < y1):
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def in_circle(x, y, cx, cy, r):
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def design(size):
    if size <= 24:
        # Snapped to whole pixels: 1px outline, 4px title bar, no onion skins.
        p = 256 / size
        frame = (0, p, 256, 256 - p)
        radius, outline = 2.5 * p, p
        title_bottom = frame[1] + 4 * p
        hole = (2 * p, title_bottom + p, 256 - 2 * p, frame[3] - 2 * p)
        cols = 4
        ghosts = []
        dot = (128, (hole[1] + hole[3]) / 2, (hole[3] - hole[1]) / 2)
        ring = 0.75 * p
    else:
        frame = (10, 22, 246, 234)
        radius, outline = 34, 8
        title_bottom = 74
        hole = (34, 92, 222, 208)
        cols = 8
        ghosts = [(84, 150, 30, 0.22), (116, 150, 30, 0.45)]
        dot = (156, 150, 31)
        ring = 6
    cell = (hole[2] - hole[0]) / cols
    fx0, fy0, fx1, fy1 = frame

    def sample(x, y):
        """RGBA (straight alpha) at canvas point (x, y)."""
        if not in_rrect(x, y, fx0, fy0, fx1, fy1, radius):
            return (0, 0, 0, 0)
        inner = in_rrect(x, y, fx0 + outline, fy0 + outline, fx1 - outline, fy1 - outline, radius - outline)
        if not inner or y < title_bottom:
            return (*DARK, 1.0)
        hx0, hy0, hx1, hy1 = hole
        if not (hx0 <= x < hx1 and hy0 <= y < hy1):
            return (*BODY, 1.0)
        i, j = int((x - hx0) // cell), int((y - hy0) // cell)
        c = CHECK_A if (i + j) % 2 == 0 else CHECK_B
        for gx, gy, gr, a in ghosts:
            if in_circle(x, y, gx, gy, gr):
                c = tuple(round(c[k] * (1 - a) + RED[k] * a) for k in range(3))
        dx, dy, dr = dot
        if in_circle(x, y, dx, dy, dr):
            c = RED
        elif in_circle(x, y, dx, dy, dr + ring):
            c = WHITE
        return (*c, 1.0)

    return sample


def render(size):
    sample = design(size)
    scale = 256 / size
    px = bytearray()
    for py in range(size):
        px.append(0)  # PNG filter: none
        for qx in range(size):
            r = g = b = a = 0.0
            for sy in range(SS):
                for sx in range(SS):
                    x = (qx + (sx + 0.5) / SS) * scale
                    y = (py + (sy + 0.5) / SS) * scale
                    cr, cg, cb, ca = sample(x, y)
                    r += cr * ca
                    g += cg * ca
                    b += cb * ca
                    a += ca
            n = SS * SS
            if a:
                px += bytes((round(r / a), round(g / a), round(b / a), round(a / n * 255)))
            else:
                px += bytes(4)
    return png(size, bytes(px))


def png(size, raw):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(images):
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for size, data in images:
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return out + b"".join(data for _, data in images)


if __name__ == "__main__":
    here = Path(__file__).parent
    images = [(s, render(s)) for s in SIZES]
    (here / "rcap.ico").write_bytes(ico(images))
    (here / "rcap-256.png").write_bytes(dict(images)[256])
    print("wrote", here / "rcap.ico")
