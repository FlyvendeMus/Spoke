#!/usr/bin/env python3
"""Generate Spoke's app icons with no third-party deps.

Draws a simple dark rounded square with a blue mic glyph, emits the PNG sizes
Tauri needs, an .ico (PNG-wrapped), and an .icns (via iconutil). Run from
anywhere; paths are resolved relative to this file.
"""
import os
import struct
import zlib
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ICONS = os.path.normpath(os.path.join(HERE, "..", "icons"))

BG = (16, 16, 22, 255)        # near-black
ACCENT = (91, 140, 255, 255)  # blue mic


def draw(size):
    """Return an RGBA bytearray (size*size*4) of the icon at `size` px."""
    px = bytearray(size * size * 4)

    def put(x, y, c):
        if 0 <= x < size and 0 <= y < size:
            i = (y * size + x) * 4
            px[i:i + 4] = bytes(c)

    # Rounded-square background.
    radius = size * 0.22
    inset = size * 0.06
    cx_c = inset + radius
    for y in range(size):
        for x in range(size):
            corner = (
                (x < cx_c and y < cx_c and (x - cx_c) ** 2 + (y - cx_c) ** 2 > radius ** 2)
                or (x > size - cx_c and y < cx_c and (x - (size - cx_c)) ** 2 + (y - cx_c) ** 2 > radius ** 2)
                or (x < cx_c and y > size - cx_c and (x - cx_c) ** 2 + (y - (size - cx_c)) ** 2 > radius ** 2)
                or (x > size - cx_c and y > size - cx_c and (x - (size - cx_c)) ** 2 + (y - (size - cx_c)) ** 2 > radius ** 2)
            )
            if not corner and inset <= x <= size - inset and inset <= y <= size - inset:
                put(x, y, BG)

    # Mic: capsule body + stand.
    cx = size / 2
    body_w = size * 0.16
    body_top = size * 0.26
    body_bot = size * 0.56
    cap_r = body_w / 2
    for y in range(size):
        for x in range(size):
            in_body = abs(x - cx) <= cap_r and body_top + cap_r <= y <= body_bot - cap_r
            in_top = (x - cx) ** 2 + (y - (body_top + cap_r)) ** 2 <= cap_r ** 2
            in_bot = (x - cx) ** 2 + (y - (body_bot - cap_r)) ** 2 <= cap_r ** 2
            if in_body or in_top or in_bot:
                put(x, y, ACCENT)

    # Stand arc (an open ring below the capsule) + post + base.
    arc_r = size * 0.16
    arc_cy = body_bot
    for y in range(size):
        for x in range(size):
            d = ((x - cx) ** 2 + (y - arc_cy) ** 2) ** 0.5
            if arc_r - size * 0.025 <= d <= arc_r and y >= arc_cy:
                put(x, y, ACCENT)
    post_top = arc_cy + arc_r
    post_bot = size * 0.80
    for y in range(int(post_top), int(post_bot)):
        for x in range(int(cx - size * 0.02), int(cx + size * 0.02)):
            put(x, y, ACCENT)
    for x in range(int(cx - size * 0.10), int(cx + size * 0.10)):
        for y in range(int(post_bot), int(post_bot + size * 0.025)):
            put(x, y, ACCENT)

    return px


def png_bytes(size):
    raw = draw(size)
    rows = bytearray()
    stride = size * 4
    for y in range(size):
        rows.append(0)  # filter type 0
        rows.extend(raw[y * stride:(y + 1) * stride])
    comp = zlib.compress(bytes(rows), 9)

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", comp) + chunk(b"IEND", b"")


def write_png(path, size):
    with open(path, "wb") as f:
        f.write(png_bytes(size))


def write_ico(path, size=256):
    png = png_bytes(size)
    # ICONDIR + one ICONDIRENTRY referencing a PNG-encoded image.
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack(
        "<BBBBHHII",
        size & 0xFF, size & 0xFF, 0, 0, 1, 32, len(png), 6 + 16,
    )
    with open(path, "wb") as f:
        f.write(header + entry + png)


def main():
    os.makedirs(ICONS, exist_ok=True)
    write_png(os.path.join(ICONS, "32x32.png"), 32)
    write_png(os.path.join(ICONS, "128x128.png"), 128)
    write_png(os.path.join(ICONS, "128x128@2x.png"), 256)
    write_png(os.path.join(ICONS, "icon.png"), 512)
    write_ico(os.path.join(ICONS, "icon.ico"), 256)

    # Build .icns via iconutil from a generated .iconset.
    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, "icon.iconset")
        os.makedirs(iconset)
        spec = [
            (16, "icon_16x16.png"), (32, "icon_16x16@2x.png"),
            (32, "icon_32x32.png"), (64, "icon_32x32@2x.png"),
            (128, "icon_128x128.png"), (256, "icon_128x128@2x.png"),
            (256, "icon_256x256.png"), (512, "icon_256x256@2x.png"),
            (512, "icon_512x512.png"), (1024, "icon_512x512@2x.png"),
        ]
        for sz, name in spec:
            write_png(os.path.join(iconset, name), sz)
        try:
            subprocess.run(
                ["iconutil", "-c", "icns", iconset,
                 "-o", os.path.join(ICONS, "icon.icns")],
                check=True,
            )
        except (FileNotFoundError, subprocess.CalledProcessError) as e:
            print(f"icns generation skipped: {e}")

    print(f"icons written to {ICONS}")


if __name__ == "__main__":
    main()
