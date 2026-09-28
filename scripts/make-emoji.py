#!/usr/bin/env python3
"""Build kernel/assets/emoji/: the emoji pictures Telegram's window draws.

It reads the Twemoji pictures from the emoji-datasource-twitter npm
package and writes one picture with all of them in a grid (emoji.png,
SIZE pixels each, COLUMNS a row) and emoji.txt: one line per picture,
in the same order, with its code points in hex (without FE0F) and its
group, for the picker.

    curl -sSLO https://registry.npmjs.org/emoji-datasource-twitter/-/emoji-datasource-twitter-16.0.0.tgz
    tar xzf emoji-datasource-twitter-16.0.0.tgz
    python3 scripts/make-emoji.py package

The pictures are Twemoji by Twitter, CC-BY 4.0.
"""

import json
import os
import sys

from PIL import Image

SIZE = 20
COLUMNS = 64
# the picker's tabs, in order, and which of the package's groups go in each
GROUPS = [
    ("smileys", ["Smileys & Emotion"]),
    ("people", ["People & Body"]),
    ("nature", ["Animals & Nature"]),
    ("food", ["Food & Drink"]),
    ("travel", ["Travel & Places"]),
    ("activities", ["Activities"]),
    ("objects", ["Objects"]),
    ("symbols", ["Symbols"]),
    ("flags", ["Flags"]),
]


def main():
    pkg = sys.argv[1] if len(sys.argv) > 1 else "package"
    data = json.load(open(os.path.join(pkg, "emoji.json")))
    group_of = {src: name for name, srcs in GROUPS for src in srcs}
    chosen = [e for e in data if e.get("has_img_twitter") and e["category"] in group_of]
    order = [name for name, _ in GROUPS]
    chosen.sort(key=lambda e: (order.index(group_of[e["category"]]), e["sort_order"]))

    rows = (len(chosen) + COLUMNS - 1) // COLUMNS
    sheet = Image.new("RGBA", (COLUMNS * SIZE, rows * SIZE), (0, 0, 0, 0))
    lines = []
    for i, e in enumerate(chosen):
        pic = Image.open(os.path.join(pkg, "img", "twitter", "64", e["image"])).convert("RGBA")
        pic = pic.resize((SIZE, SIZE), Image.LANCZOS)
        sheet.paste(pic, ((i % COLUMNS) * SIZE, (i // COLUMNS) * SIZE))
        points = [p for p in e["unified"].lower().split("-") if p != "fe0f"]
        lines.append("%s %s" % ("-".join(points), group_of[e["category"]]))

    out = os.path.join(os.path.dirname(__file__), "..", "kernel", "assets", "emoji")
    os.makedirs(out, exist_ok=True)
    # 256 colors keep the kernel small: a fifth of the size, and at this
    # size the difference hardly shows
    sheet = sheet.quantize(colors=256, method=Image.Quantize.FASTOCTREE, dither=Image.Dither.NONE)
    sheet.save(os.path.join(out, "emoji.png"), optimize=True)
    with open(os.path.join(out, "emoji.txt"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print(len(chosen), "emoji,", os.path.getsize(os.path.join(out, "emoji.png")), "bytes")


if __name__ == "__main__":
    main()
