"""BuildCast icon generator.

Blue rounded square, white SD card (notched corner, contact pins), cast arcs
in the card's bottom-left corner. Regenerates all PNG sizes, the .ico and the
raw RGBA used for the runtime window icon.

Usage: python gen_icon.py   (requires: pip install pillow)
"""

import os

from PIL import Image, ImageDraw

OUT = os.path.dirname(os.path.abspath(__file__))
S = 1024
SS = 4  # supersampling factor

ACCENT_TOP = (0x4A, 0x86, 0xF7)
ACCENT_BOT = (0x1F, 0x53, 0xC4)
ACCENT_MID = (0x2F, 0x6F, 0xED)
WHITE = (255, 255, 255, 255)


def rounded_rect_mask(size, radius):
    m = Image.new("L", (size, size), 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=255)
    return m


def vertical_gradient(size, top, bot):
    g = Image.new("RGB", (1, size))
    for y in range(size):
        t = y / (size - 1)
        g.putpixel((0, y), tuple(int(a + (b - a) * t) for a, b in zip(top, bot)))
    return g.resize((size, size))


def main():
    big = S * SS
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    bg = vertical_gradient(big, ACCENT_TOP, ACCENT_BOT).convert("RGBA")
    img.paste(bg, (0, 0), rounded_rect_mask(big, int(big * 0.22)))

    d = ImageDraw.Draw(img)
    u = big / 1024.0

    # centered SD card, notched top-right corner
    cx0, cy0 = 300 * u, 200 * u
    cx1, cy1 = 724 * u, 824 * u
    notch = 110 * u
    r = 42 * u
    d.rounded_rectangle([cx0, cy0, cx1, cy1], radius=r, fill=WHITE)
    notch_poly = [(cx1 - notch, cy0 - 2), (cx1 + 2, cy0 - 2), (cx1 + 2, cy0 + notch)]
    notch_mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(notch_mask).polygon(notch_poly, fill=255)
    img.paste(bg, (0, 0), notch_mask)
    d = ImageDraw.Draw(img)

    # exactly 4 contact pins, evenly spaced along the card top
    n_pins = 4
    pin_w, pin_h = 42 * u, 68 * u
    rx0, rx1 = cx0 + 76 * u, cx1 - notch - 62 * u
    gap = (rx1 - rx0 - n_pins * pin_w) / (n_pins - 1)
    top = cy0 + 56 * u
    for i in range(n_pins):
        px = rx0 + i * (pin_w + gap)
        d.rounded_rectangle(
            [px, top, px + pin_w, top + pin_h], radius=12 * u, fill=(*ACCENT_MID, 255)
        )

    # cast signal in the card's bottom-left corner: solid dot + 3 arcs,
    # 4 blue bands total, separated by white gaps
    acx, acy = cx0 + 30 * u, cy1 - 30 * u
    for radius in (150 * u, 245 * u, 340 * u):
        d.arc(
            [acx - radius, acy - radius, acx + radius, acy + radius],
            start=270,
            end=360,
            fill=(*ACCENT_MID, 255),
            width=int(60 * u),
        )
    d.pieslice(
        [acx - 72 * u, acy - 72 * u, acx + 72 * u, acy + 72 * u],
        start=270,
        end=360,
        fill=(*ACCENT_MID, 255),
    )
    # clip arc overshoot back to the background outside the card
    erase = Image.new("L", (big, big), 0)
    de = ImageDraw.Draw(erase)
    de.rectangle([0, 0, cx0 - 1, big], fill=255)
    de.rectangle([0, cy1 + 1, big, big], fill=255)
    img.paste(
        bg,
        (0, 0),
        Image.composite(erase, Image.new("L", (big, big), 0), rounded_rect_mask(big, int(big * 0.22))),
    )

    master = img.resize((S, S), Image.LANCZOS)
    master.save(os.path.join(OUT, "buildcast-1024.png"))
    for size in [256, 128, 64, 48, 32, 16]:
        master.resize((size, size), Image.LANCZOS).save(os.path.join(OUT, f"buildcast-{size}.png"))
    master.resize((256, 256), Image.LANCZOS).save(
        os.path.join(OUT, "buildcast.ico"),
        sizes=[(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (16, 16)],
    )
    icon64 = master.resize((64, 64), Image.LANCZOS)
    with open(os.path.join(OUT, "buildcast-64.rgba"), "wb") as f:
        f.write(icon64.tobytes())
    print("written to", OUT)


if __name__ == "__main__":
    main()
