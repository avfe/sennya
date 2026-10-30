"""Render the 1024x1024 app icon to assets/icon-1024.png (needs numpy).

A black squircle on the macOS icon grid (824 px body) with one white `>_` mark,
centred by its bounding box. Nothing else.
"""
import struct
import sys
import zlib

import numpy as np

N = 1024
OUT = sys.argv[1] if len(sys.argv) > 1 else "assets/icon-1024.png"
y, x = np.mgrid[0:N, 0:N].astype(np.float64) + 0.5


def hex_rgb(h):
    h = h.lstrip("#")
    return np.array([int(h[i:i + 2], 16) / 255 for i in (0, 2, 4)])


def rrect(cx, cy, hw, hh, r):
    """Signed distance to a rounded rectangle."""
    qx = np.abs(x - cx) - (hw - r)
    qy = np.abs(y - cy) - (hh - r)
    outside = np.sqrt(np.maximum(qx, 0) ** 2 + np.maximum(qy, 0) ** 2)
    return outside + np.minimum(np.maximum(qx, qy), 0) - r


def capsule(ax, ay, bx, by, w):
    pax, pay, bax, bay = x - ax, y - ay, bx - ax, by - ay
    h = np.clip((pax * bax + pay * bay) / (bax * bax + bay * bay), 0, 1)
    return np.hypot(pax - bax * h, pay - bay * h) - w


def circle(cx, cy, r):
    return np.hypot(x - cx, y - cy) - r


def coverage(d, soft=1.0):
    return np.clip(0.5 - d / soft, 0, 1)


rgb = np.zeros((N, N, 3))
alpha = np.zeros((N, N))


def paint(color, a):
    """Composite `color` (H,W,3 or 3) with alpha `a` over the canvas."""
    global rgb, alpha
    a = a[..., None] if a.ndim == 2 else a
    out_a = a[..., 0] + alpha * (1 - a[..., 0])
    safe = np.where(out_a > 0, out_a, 1)[..., None]
    rgb = (color * a + rgb * alpha[..., None] * (1 - a)) / safe
    alpha = out_a


def lerp(c1, c2, t):
    t = np.clip(t, 0, 1)[..., None]
    return c1 * (1 - t) + c2 * t


# Body on Apple's grid: 824 px wide.
CX, CY, HALF, R = 512, 512, 412, 186

d = rrect(CX, CY + 12, HALF, HALF, R)
paint(np.zeros(3), 0.18 * np.clip(1 - (d + 6) / 48, 0, 1) ** 2 * (d > -40))

body = coverage(rrect(CX, CY, HALF, HALF, R))
t = (y - 100) / 824
paint(lerp(hex_rgb("#1C1C1E"), hex_rgb("#000000"), t), body)
# A hairline rim so the black body still reads on dark backgrounds and in the Dock.
rim = coverage(np.abs(rrect(CX, CY, HALF - 1.5, HALF - 1.5, R - 1.5)) - 1.2)
paint(np.ones(3), rim * 0.16)

# The mark: a chevron and an underscore, stroke half-width T.
T = 34          # half the stroke width
W = 128         # chevron depth (tip x minus arm x)
H = 128         # chevron half height
GAP = 46        # space between the chevron tip and the underscore
UL = 150        # underscore length
xmin_rel = -T                      # left of the arm caps
xmax_rel = W + T + GAP + UL        # right end of the underscore
x0 = CX - (xmin_rel + xmax_rel) / 2    # arms' x so the group is centred
y0 = CY                                 # chevron tip height (group is symmetric)
white = np.ones(3)
chev = np.minimum(capsule(x0, y0 - H, x0 + W, y0, T), capsule(x0 + W, y0, x0, y0 + H, T))
u_left = x0 + W + T + GAP
under = capsule(u_left + T, y0 + H, u_left + UL - T, y0 + H, T)
paint(white, coverage(np.minimum(chev, under)))

pixels = (np.dstack([rgb, alpha]) * 255).round().clip(0, 255).astype(np.uint8)
raw = b"".join(b"\0" + pixels[row].tobytes() for row in range(N))


def chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)


png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
with open(OUT, "wb") as f:
    f.write(png)
print(OUT)
