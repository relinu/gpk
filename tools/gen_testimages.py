#!/usr/bin/env python3
"""Creates deterministic PNG and JPEG test images for the differential tests (needs Pillow).

  gen_testimages.py <png-dir> <jpeg-dir>

PNG: different color modes, bit depths, zlib levels and filters.
JPEG: baseline with different qualities, subsamplings, restart intervals, optimized Huffman
tables and grayscale; also progressive (must be rejected).
"""
import math
import os
import sys

from PIL import Image


def picture(w, h, seed):
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w):
            r = int(128 + 120 * math.sin((x + seed) * 0.05) * math.cos(y * 0.03))
            g = int((x * 3 + y * 5 + seed * 7) % 256)
            b = int(128 + 100 * math.sin(math.hypot(x - w / 2, y - h / 2) * 0.1 + seed))
            if (x // 16 + y // 16 + seed) % 7 == 0:
                r, g, b = 250, 250, 250
            px[x, y] = (r & 255, g, b & 255)
    return img


def main():
    png_dir, jpg_dir = sys.argv[1], sys.argv[2]
    os.makedirs(png_dir, exist_ok=True)
    os.makedirs(jpg_dir, exist_ok=True)
    k = 0
    for seed, (w, h) in enumerate([(64, 48), (173, 91), (256, 256)]):
        base = picture(w, h, seed)
        variants = [base, base.convert("L"), base.convert("RGBA"), base.convert("P"), base.convert("LA")]
        for img in variants:
            for level in (9, 6, 1):
                img.save(os.path.join(png_dir, f"t{k:03d}.png"), compress_level=level)
                k += 1
        for q in (50, 75, 90, 98):
            for sub in (0, 1, 2):
                base.save(os.path.join(jpg_dir, f"q{q}_s{sub}_{seed}.jpg"), quality=q, subsampling=sub)
        base.save(os.path.join(jpg_dir, f"opt_{seed}.jpg"), quality=85, optimize=True)
        base.convert("L").save(os.path.join(jpg_dir, f"gray_{seed}.jpg"), quality=80)
        base.save(os.path.join(jpg_dir, f"prog_{seed}.jpg"), quality=80, progressive=True)
        try:
            base.save(os.path.join(jpg_dir, f"rst_{seed}.jpg"), quality=80, restart_marker_blocks=4)
        except TypeError:
            pass
    print(f"{k} PNGs in {png_dir}, {len(os.listdir(jpg_dir))} JPEGs in {jpg_dir}")


if __name__ == "__main__":
    main()
