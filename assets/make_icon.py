#!/usr/bin/env python3
"""Generate the Oxide app icon (1024x1024 PNG) with the Python stdlib only.

The name is a pun: written in Rust, and rust *is* iron oxide. So the prompt
glyph is a corroded iron chevron — fbm-noise oxidation in ochre/rust/umber,
pitted edges, a faint bloom of rust staining the plate around it.
"""
import math
import struct
import sys
import zlib

SIZE = 1024

PLATE_TOP = (0x1C, 0x18, 0x16)
PLATE_BOTTOM = (0x0E, 0x0B, 0x0A)
PLATE_EDGE = (0x3A, 0x2E, 0x28)

# Oxidation palette, deep crust -> bright fresh rust.
RUST_CRUST = (0x5A, 0x24, 0x10)
RUST_DEEP = (0x8B, 0x31, 0x03)
RUST_MID = (0xB7, 0x41, 0x0E)
RUST_LIGHT = (0xE2, 0x72, 0x5B)
RUST_BRIGHT = (0xFA, 0xB3, 0x87)


def hash2(ix, iy):
    h = (ix * 374761393 + iy * 668265263) & 0xFFFFFFFF
    h = (h ^ (h >> 13)) * 1274126177 & 0xFFFFFFFF
    return ((h ^ (h >> 16)) & 0xFFFF) / 65535.0


def smooth(t):
    return t * t * (3.0 - 2.0 * t)


def vnoise(x, y):
    ix, iy = math.floor(x), math.floor(y)
    fx, fy = x - ix, y - iy
    a = hash2(ix, iy)
    b = hash2(ix + 1, iy)
    c = hash2(ix, iy + 1)
    d = hash2(ix + 1, iy + 1)
    ux, uy = smooth(fx), smooth(fy)
    return a + (b - a) * ux + (c - a) * uy + (a - b - c + d) * ux * uy


def fbm(x, y, octaves=4):
    total, amp, freq, norm = 0.0, 1.0, 1.0, 0.0
    for _ in range(octaves):
        total += amp * vnoise(x * freq, y * freq)
        norm += amp
        amp *= 0.5
        freq *= 2.1
    return total / norm


def sd_rounded_rect(px_, py, cx, cy, hw, hh, r):
    qx = abs(px_ - cx) - (hw - r)
    qy = abs(py - cy) - (hh - r)
    ox = max(qx, 0.0)
    oy = max(qy, 0.0)
    return math.hypot(ox, oy) + min(max(qx, qy), 0.0) - r


def sd_segment(px_, py, ax, ay, bx, by):
    abx, aby = bx - ax, by - ay
    apx, apy = px_ - ax, py - ay
    t = max(0.0, min(1.0, (apx * abx + apy * aby) / (abx * abx + aby * aby)))
    return math.hypot(apx - t * abx, apy - t * aby)


def coverage(d):
    return max(0.0, min(1.0, 0.5 - d))


def lerp3(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def blend(dst, src, alpha):
    return tuple(dst[i] * (1 - alpha) + src[i] * alpha for i in range(3))


def oxide_color(px_, py):
    """Layered rust: fbm picks the oxidation stage, speckle noise adds pits."""
    n = fbm(px_ / 92.0, py / 92.0, 4)
    fine = fbm(px_ / 23.0 + 41.7, py / 23.0 + 17.3, 3)
    n = 0.72 * n + 0.28 * fine
    # Stretch contrast so both deep crust and bright fresh rust show up.
    n = max(0.0, min(1.0, (n - 0.30) / 0.42))
    if n < 0.30:
        c = lerp3(RUST_CRUST, RUST_DEEP, n / 0.30)
    elif n < 0.55:
        c = lerp3(RUST_DEEP, RUST_MID, (n - 0.30) / 0.25)
    elif n < 0.80:
        c = lerp3(RUST_MID, RUST_LIGHT, (n - 0.55) / 0.25)
    else:
        c = lerp3(RUST_LIGHT, RUST_BRIGHT, (n - 0.80) / 0.20)
    # Dark pits where the speckle field spikes.
    pit = fbm(px_ / 9.0 + 99.1, py / 9.0 + 7.7, 2)
    if pit > 0.72:
        c = lerp3(c, RUST_CRUST, min(1.0, (pit - 0.72) * 4.0) * 0.7)
    return c


def main(out_path):
    rows = []
    plate_c, plate_hw, plate_r = SIZE / 2, 412.0, 232.0
    stroke = 46.0
    ch = [(300, 372, 476, 512), (476, 512, 300, 652)]
    cur_cx, cur_cy, cur_hw, cur_hh = 622.0, 630.0, 92.0, 24.0

    for y in range(SIZE):
        row = bytearray([0])
        for x in range(SIZE):
            px_, py = x + 0.5, y + 0.5
            d_plate = sd_rounded_rect(px_, py, plate_c, plate_c, plate_hw, plate_hw, plate_r)
            a_plate = coverage(d_plate)
            if a_plate <= 0.0:
                row += bytes((0, 0, 0, 0))
                continue

            t = y / SIZE
            color = lerp3(PLATE_TOP, PLATE_BOTTOM, t)
            a_edge = coverage(abs(d_plate + 5.0) - 5.0)
            color = blend(color, PLATE_EDGE, a_edge * 0.9)

            # Corroded edges: perturb the glyph SDF with noise so the outline
            # is eaten away rather than machine-clean. High frequency, low
            # amplitude — pitting, not melting.
            wobble = (fbm(px_ / 16.0 + 7.7, py / 16.0 + 3.1, 3) - 0.5) * 11.0
            d_ch = min(
                sd_segment(px_, py, *ch[0]) - stroke,
                sd_segment(px_, py, *ch[1]) - stroke,
            ) + wobble
            d_cur = sd_rounded_rect(px_, py, cur_cx, cur_cy, cur_hw, cur_hh, 12.0) + wobble * 0.5
            d_glyph = min(d_ch, d_cur)

            # Rust bloom: staining that creeps outward from the metal.
            if 0.0 < d_glyph < 34.0:
                stain = fbm(px_ / 27.0 + 55.5, py / 27.0 + 21.2, 3)
                creep = (1.0 - d_glyph / 34.0) ** 2 * max(0.0, stain - 0.46) * 1.0
                if creep > 0.0:
                    color = blend(color, RUST_DEEP, min(0.45, creep))

            a_glyph = coverage(d_glyph)
            if a_glyph > 0.0:
                glyph = oxide_color(px_, py)
                # Top-light the glyph slightly for depth.
                shade = 1.0 + (0.10 - 0.20 * max(0.0, min(1.0, (py - 340.0) / 340.0)))
                glyph = tuple(min(255.0, c * shade) for c in glyph)
                color = blend(color, glyph, a_glyph)

            row += bytes(
                (
                    int(round(color[0])),
                    int(round(color[1])),
                    int(round(color[2])),
                    int(round(255 * a_plate)),
                )
            )
        rows.append(bytes(row))

    raw = b"".join(rows)

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    with open(out_path, "wb") as f:
        f.write(png)
    print(f"wrote {out_path}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "icon_1024.png")
