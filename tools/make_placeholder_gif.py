#!/usr/bin/env python3
"""Generate assets/movie.gif - a placeholder animation.

This exists only so the build.rs pipeline has input before a real movie is
dropped in. Replace assets/movie.gif with any GIF and rebuild; build.rs converts
whatever is there. Re-run this script to regenerate the placeholder.

Requires Pillow (Ubuntu: `sudo apt-get install python3-pil`).
"""
import math
from pathlib import Path

from PIL import Image, ImageDraw

WIDTH, HEIGHT = 128, 64
FRAME_COUNT = 36
DURATION_MS = 60
OUT = Path(__file__).resolve().parent.parent / "assets" / "movie.gif"


def main() -> None:
    frames = []
    for i in range(FRAME_COUNT):
        t = i / FRAME_COUNT
        img = Image.new("L", (WIDTH, HEIGHT), 0)
        draw = ImageDraw.Draw(img)

        # A dot tracing a Lissajous path.
        cx = WIDTH / 2 + (WIDTH / 2 - 10) * math.sin(2 * math.pi * t)
        cy = HEIGHT / 2 + (HEIGHT / 2 - 10) * math.sin(4 * math.pi * t)
        r = 6
        draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=255)

        # A pulsing ring at the centre.
        rr = 8 + 14 * (0.5 + 0.5 * math.sin(2 * math.pi * t))
        draw.ellipse(
            [WIDTH / 2 - rr, HEIGHT / 2 - rr, WIDTH / 2 + rr, HEIGHT / 2 + rr],
            outline=255,
            width=2,
        )
        frames.append(img)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    frames[0].save(
        OUT,
        save_all=True,
        append_images=frames[1:],
        duration=DURATION_MS,
        loop=0,
        optimize=False,
    )
    print(f"wrote {OUT} ({FRAME_COUNT} frames, {DURATION_MS} ms each)")


if __name__ == "__main__":
    main()
