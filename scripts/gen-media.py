#!/usr/bin/env python3
"""Make the sample photos and the demo video that go on the RyzikOS disc.

The video is an AVI with Motion JPEG frames, the format Video Player
plays. Like many cameras, the frames leave out their Huffman tables, so
the player's fallback to the standard tables is tested too.

Needs Pillow: python3 -m pip install pillow
Writes iso/media/Pictures/*.jpg and iso/media/Videos/*.avi.
"""
import math
import os
import struct
import sys
from io import BytesIO

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "iso", "media")
LOGO = os.path.join(ROOT, "kernel", "assets", "ryzikos-logo.png")
FONT = "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf"


def font(size):
    try:
        return ImageFont.truetype(FONT, size)
    except OSError:
        return ImageFont.load_default()


def lerp(a, b, t):
    return tuple(int(x + (y - x) * t) for x, y in zip(a, b))


def sky(w, h, top, bottom):
    img = Image.new("RGB", (w, h))
    d = ImageDraw.Draw(img)
    for y in range(h):
        d.line([(0, y), (w, y)], fill=lerp(top, bottom, y / max(1, h - 1)))
    return img


def ridge(d, w, h, base, amp, freq, phase, color):
    pts = [(0, h)]
    for x in range(0, w + 8, 8):
        y = base + amp * math.sin(x * freq + phase) + amp * 0.4 * math.sin(x * freq * 2.7 + phase * 1.7)
        pts.append((x, y))
    pts.append((w, h))
    d.polygon(pts, fill=color)


def landscape(w, h, t, sun_t):
    """A scene: sky, sun and three rows of hills. t moves the hills."""
    top = lerp((40, 110, 220), (40, 30, 90), sun_t)
    bottom = lerp((170, 215, 255), (255, 140, 90), sun_t)
    img = sky(w, h, top, bottom)
    d = ImageDraw.Draw(img)
    sx = w * (0.15 + 0.7 * sun_t)
    sy = h * (0.18 + 0.45 * sun_t ** 1.5)
    r = h * 0.07
    glow = Image.new("RGB", (w, h), (0, 0, 0))
    ImageDraw.Draw(glow).ellipse([sx - r * 2.4, sy - r * 2.4, sx + r * 2.4, sy + r * 2.4], fill=(255, 200, 120))
    glow = glow.filter(ImageFilter.GaussianBlur(r))
    img = Image.blend(img, Image.composite(glow, img, glow.convert("L")), 0.35)
    d = ImageDraw.Draw(img)
    d.ellipse([sx - r, sy - r, sx + r, sy + r], fill=lerp((255, 240, 170), (255, 170, 80), sun_t))
    ridge(d, w, h, h * 0.62, h * 0.06, 0.006, t * 0.3, lerp((90, 140, 190), (120, 70, 110), sun_t))
    ridge(d, w, h, h * 0.72, h * 0.05, 0.011, t * 0.7 + 1, lerp((50, 130, 90), (70, 50, 80), sun_t))
    ridge(d, w, h, h * 0.84, h * 0.04, 0.017, t * 1.4 + 2, lerp((30, 90, 60), (40, 30, 50), sun_t))
    return img


def photos():
    os.makedirs(os.path.join(OUT, "Pictures"), exist_ok=True)
    scenes = [
        ("Morning Hills.jpg", 0.05, 0.0),
        ("Midday.jpg", 0.35, 1.3),
        ("Sunset.jpg", 0.95, 2.1),
    ]
    for name, sun_t, t in scenes:
        img = landscape(1280, 800, t, sun_t)
        img.save(os.path.join(OUT, "Pictures", name), quality=86)
    # the logo on a soft background
    img = sky(1280, 800, (250, 244, 232), (236, 222, 200))
    logo = Image.open(LOGO).convert("RGBA").resize((360, 360), Image.LANCZOS)
    img.paste(logo, (460, 150), logo)
    d = ImageDraw.Draw(img)
    text = "RyzikOS 1.0"
    f = font(72)
    tw = d.textlength(text, font=f)
    d.text(((1280 - tw) / 2, 560), text, font=f, fill=(60, 50, 40))
    img.save(os.path.join(OUT, "Pictures", "RyzikOS Logo.jpg"), quality=88)


def strip_dht(jpeg):
    """Remove the DHT segments, as Motion JPEG cameras do."""
    out = bytearray(jpeg[:2])
    i = 2
    while i < len(jpeg):
        marker = jpeg[i + 1]
        if marker == 0xDA:
            out += jpeg[i:]
            break
        n = struct.unpack(">H", jpeg[i + 2:i + 4])[0]
        if marker != 0xC4:
            out += jpeg[i:i + 2 + n]
        i += 2 + n
    return bytes(out)


def chunk(fourcc, data):
    pad = b"\0" if len(data) % 2 else b""
    return fourcc + struct.pack("<I", len(data)) + data + pad


def lst(kind, *parts):
    body = kind + b"".join(parts)
    return b"LIST" + struct.pack("<I", len(body)) + body


def write_avi(path, frames, w, h, fps):
    n = len(frames)
    biggest = max(len(f) for f in frames)
    avih = struct.pack("<14I", 1000000 // fps, biggest * fps, 0, 0x10, n, 0, 1, biggest, w, h, 0, 0, 0, 0)
    strh = b"vids" + b"MJPG" + struct.pack("<IHHIIIIIIIi4h", 0, 0, 0, 0, 1, fps, 0, n, biggest, 0xFFFFFFFF, 0, 0, 0, w, h)
    strf = struct.pack("<IiiHH4sIiiII", 40, w, h, 1, 24, b"MJPG", w * h * 3, 0, 0, 0, 0)
    hdrl = lst(b"hdrl", chunk(b"avih", avih), lst(b"strl", chunk(b"strh", strh), chunk(b"strf", strf)))
    movi_parts = []
    index = []
    offset = 4
    for f in frames:
        c = chunk(b"00dc", f)
        index.append(struct.pack("<4sIII", b"00dc", 0x10, offset, len(f)))
        movi_parts.append(c)
        offset += len(c)
    movi = lst(b"movi", *movi_parts)
    idx1 = chunk(b"idx1", b"".join(index))
    body = b"AVI " + hdrl + movi + idx1
    with open(path, "wb") as out:
        out.write(b"RIFF" + struct.pack("<I", len(body)) + body)


def video():
    os.makedirs(os.path.join(OUT, "Videos"), exist_ok=True)
    w, h, fps, seconds = 640, 360, 25, 10
    logo = Image.open(LOGO).convert("RGBA").resize((96, 96), Image.LANCZOS)
    title = font(40)
    small = font(18)
    frames = []
    total = fps * seconds
    for i in range(total):
        t = i / total
        img = landscape(w, h, i / fps, t)
        d = ImageDraw.Draw(img)
        # the logo bounces along the hills
        x = int(40 + (w - 176) * (0.5 - 0.5 * math.cos(t * math.pi * 2)))
        y = int(h * 0.5 - abs(math.sin(i / fps * math.pi * 1.6)) * 90)
        img.paste(logo, (x, y), logo)
        a = min(1.0, max(0.0, (i / fps - 1.0) / 1.5))
        text = "RyzikOS 1.0"
        tw = d.textlength(text, font=title)
        if a > 0.02:
            shade = lerp(lerp((170, 215, 255), (255, 140, 90), t), (255, 255, 255), a)
            d.text(((w - tw) / 2, 28), text, font=title, fill=shade)
        label = "Video Player  %d:%02d  frame %d" % (i // fps // 60, i // fps % 60, i + 1)
        d.text((14, h - 30), label, font=small, fill=(255, 255, 255))
        buf = BytesIO()
        img.save(buf, "JPEG", quality=72)
        frames.append(strip_dht(buf.getvalue()))
        if i % 50 == 0:
            print("frame", i, file=sys.stderr)
    write_avi(os.path.join(OUT, "Videos", "RyzikOS Demo.avi"), frames, w, h, fps)


if __name__ == "__main__":
    photos()
    video()
    print("wrote", OUT)
