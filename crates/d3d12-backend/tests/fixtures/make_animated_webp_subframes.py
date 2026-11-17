# SPDX-License-Identifier: MIT
# Makes animated_subframes.webp: an 8 x 8 animation whose frames cover parts of the canvas, mixing
# blending, no blending and disposing to the background, with an opaque white background colour in
# ANIM, as the Noto animated emoji have. Each frame is a lossless still from Pillow, wrapped by hand
# in an ANMF chunk so the flags and offsets are exactly these. A player that follows the WebP
# container spec (libwebp, browsers) disposes to transparent and ignores the background colour.
# Run with the py launcher and Pillow: py make_animated_webp_subframes.py
import io
import struct
from pathlib import Path

from PIL import Image


def chunk(fourcc, data):
    return fourcc + struct.pack("<I", len(data)) + data + (b"\0" if len(data) % 2 else b"")


def vp8l_of(image):
    buffer = io.BytesIO()
    image.save(buffer, "WEBP", lossless=True, exact=True)
    data = buffer.getvalue()
    at = 12
    while at < len(data):
        fourcc, size = data[at:at + 4], struct.unpack("<I", data[at + 4:at + 8])[0]
        if fourcc == b"VP8L":
            return chunk(fourcc, data[at + 8:at + 8 + size])
        at += 8 + size + (size & 1)
    raise ValueError("no VP8L chunk")


def u24(value):
    return value.to_bytes(3, "little")


def anmf(image, x, y, duration, blend, dispose):
    flags = (0 if blend else 2) | (1 if dispose else 0)
    header = u24(x // 2) + u24(y // 2) + u24(image.width - 1) + u24(image.height - 1) + u24(duration) + bytes([flags])
    return chunk(b"ANMF", header + vp8l_of(image))


# Frame 0: the whole canvas red, no blending, kept.
red = Image.new("RGBA", (8, 8), (255, 0, 0, 255))
# Frame 1: a half-transparent blue square blended over the middle, then disposed to the background.
blue = Image.new("RGBA", (4, 4), (0, 0, 255, 128))
# Frame 2: a green square at the bottom right with a transparent top-left pixel, blended, kept.
green = Image.new("RGBA", (4, 4), (0, 255, 0, 255))
green.putpixel((0, 0), (0, 0, 0, 0))
# Frame 3: a half-transparent yellow square at the top left, not blended: it replaces what is there.
yellow = Image.new("RGBA", (2, 2), (255, 255, 0, 128))

frames = [anmf(red, 0, 0, 100, False, False), anmf(blue, 2, 2, 100, True, True), anmf(green, 4, 4, 100, True, False), anmf(yellow, 0, 0, 100, False, False)]
vp8x = chunk(b"VP8X", bytes([0b00010010, 0, 0, 0]) + u24(7) + u24(7))
anim = chunk(b"ANIM", bytes([255, 255, 255, 255]) + struct.pack("<H", 0))
body = b"WEBP" + vp8x + anim + b"".join(frames)
target = Path(__file__).with_name("animated_subframes.webp")
target.write_bytes(b"RIFF" + struct.pack("<I", len(body)) + body)
print(target)
# What libwebp, through Pillow, shows: the pixels the tests check.
with Image.open(target) as decoded:
    for index in range(decoded.n_frames):
        decoded.seek(index)
        rgba = decoded.convert("RGBA")
        print(index, [rgba.getpixel(point) for point in [(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (7, 7), (6, 1)]])
