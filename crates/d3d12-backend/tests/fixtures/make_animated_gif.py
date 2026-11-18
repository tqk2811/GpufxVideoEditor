# SPDX-License-Identifier: MIT
# Makes animated_subframes.gif: an 8 x 8 GIF whose frames cover parts of the canvas, with disposal
# "previous" and "background", a transparent index, a frame delay of 0 and of 1/100 s (which a
# browser shows for 100 ms) and a NETSCAPE2.0 loop count of 2. Written byte by byte with the
# standard library only, so every field is exactly this. Run with: py make_animated_gif.py
#
# Palette: 0 transparent (black), 1 red, 2 green, 3 blue, 4 yellow; background index 0.
# Frame 0: the whole canvas red, keep, 10/100 s.
# Frame 1: green 4 x 4 at (2, 2), restore to previous, 0/100 s.
# Frame 2: blue 4 x 4 at (4, 4) whose top left pixel is transparent, restore to background, 20/100 s.
# Frame 3: yellow 2 x 2 at (0, 0), keep, 1/100 s.
# What a browser shows:
#   0: all red.  1: red with a green square at (2..6, 2..6).
#   2: red with blue at (4..8, 4..8), red at (4, 4).  3: yellow at (0..2, 0..2), clear at (4..8, 4..8), red elsewhere.
import struct
from pathlib import Path


def lzw(indices, min_code_size=3):
    # A valid LZW stream that never grows the table: a clear code before every code would also do,
    # but resetting each few codes keeps the code size fixed at min_code_size + 1 bits.
    clear, end = 1 << min_code_size, (1 << min_code_size) + 1
    size = min_code_size + 1
    codes = []
    room = (1 << size) - (end + 1)  # entries that fit before the code size grows
    for start in range(0, len(indices), max(room - 1, 1)):
        codes.append(clear)
        codes.extend(indices[start:start + max(room - 1, 1)])
    codes.append(end)
    bits, count, out = 0, 0, bytearray()
    for code in codes:
        bits |= code << count
        count += size
        while count >= 8:
            out.append(bits & 0xFF)
            bits >>= 8
            count -= 8
    if count:
        out.append(bits & 0xFF)
    blocks = bytearray([min_code_size])
    for at in range(0, len(out), 255):
        piece = out[at:at + 255]
        blocks += bytes([len(piece)]) + piece
    return bytes(blocks + b"\0")


def frame(x, y, w, h, indices, delay, dispose, transparent=None):
    flags = (dispose << 2) | (1 if transparent is not None else 0)
    gce = b"\x21\xF9\x04" + struct.pack("<BHB", flags, delay, transparent or 0) + b"\0"
    descriptor = b"\x2C" + struct.pack("<HHHHB", x, y, w, h, 0)
    return gce + descriptor + lzw(indices)


palette = bytes([0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
header = b"GIF89a" + struct.pack("<HHBBB", 8, 8, 0xF2, 0, 0) + palette
netscape = b"\x21\xFF\x0BNETSCAPE2.0\x03\x01" + struct.pack("<H", 2) + b"\0"
blue = [0] + [3] * 15
data = (
    header
    + netscape
    + frame(0, 0, 8, 8, [1] * 64, 10, 1)
    + frame(2, 2, 4, 4, [2] * 16, 0, 3)
    + frame(4, 4, 4, 4, blue, 20, 2, transparent=0)
    + frame(0, 0, 2, 2, [4] * 4, 1, 1)
    + b"\x3B"
)
Path(__file__).with_name("animated_subframes.gif").write_bytes(data)
