# SPDX-License-Identifier: MIT
# Makes animated.webp, the animated sticker the still tests read: 4 x 4, lossless, looping forever.
# Frame 0: red with a transparent top-left pixel, 100 ms. Frame 1: green, 200 ms. Frame 2: blue, 300 ms.
# Run with the py launcher and Pillow: py make_animated_webp.py
from pathlib import Path

from PIL import Image

frames = []
for color in [(255, 0, 0, 255), (0, 255, 0, 255), (0, 0, 255, 255)]:
    frames.append(Image.new("RGBA", (4, 4), color))
frames[0].putpixel((0, 0), (0, 0, 0, 0))
target = Path(__file__).with_name("animated.webp")
frames[0].save(target, save_all=True, append_images=frames[1:], duration=[100, 200, 300], loop=0, lossless=True)
print(target)
