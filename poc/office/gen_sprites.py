#!/usr/bin/env python3
"""Workflow office POC sprite generator.

Every sprite is a hand-designed pixel matrix (ASCII rows + a char->RGBA map),
written out as an individual PNG via a stdlib-only encoder (zlib + struct).
Run: python3 gen_sprites.py  ->  sprites/*.png
"""
import os
import struct
import zlib

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "sprites")
os.makedirs(OUT, exist_ok=True)


def png(path, rows, palette):
    h = len(rows)
    w = max(len(r) for r in rows)
    raw = bytearray()
    for r in rows:
        raw.append(0)  # filter: none
        for x in range(w):
            ch = r[x] if x < len(r) else " "
            raw += bytes(palette.get(ch, (0, 0, 0, 0)))
    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    out = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")
    with open(os.path.join(OUT, path), "wb") as f:
        f.write(out)
    print("wrote", path, f"{w}x{h}")


def hexc(s, a=255):
    return (int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16), a)


# ---------------------------------------------------------------- floor tile
# Warm wood planks, LOW contrast — the previous dark grout grid read as
# basement brick. Horizontal planks only, soft seams, sparse grain.
floor_pal = {
    ".": hexc("2b241c"),
    ",": hexc("2e271e"),
    "=": hexc("251f18"),
    "+": hexc("332b21"),
    "|": hexc("272119"),
}
png("floor_tile.png", [
    "................",
    "....+...........",
    ".........,......",
    "================",
    ",,,,,,,|,,,,,,,,",
    ",,+,,,,|,,,,,,,,",
    ",,,,,,,,,,,+,,,,",
    "================",
    "....,...........",
    "...........+....",
    ".......,........",
    "================",
    ",,,,,,,,,,,,|,,,",
    ",,,+,,,,,,,,|,,,",
    ",,,,,,,+,,,,,,,,",
    "================",
], floor_pal)

# ---------------------------------------------------------------- desk 32x16
desk_pal = {
    "h": hexc("7d6142"),
    "w": hexc("64503a"),
    "g": hexc("584634"),
    "d": hexc("463726"),
    "l": hexc("32271a"),
    "s": (0, 0, 0, 70),
}
png("desk.png", [
    "                                ",
    "                                ",
    "                                ",
    "hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh",
    "wwwwwwwwgwwwwwwwwwwwwwgwwwwwwwww",
    "wwgwwwwwwwwwwwgwwwwwwwwwwwwwgwww",
    "wwwwwwwwwwwwgwwwwwwwwwwgwwwwwwww",
    "dddddddddddddddddddddddddddddddd",
    "  lll                      lll  ",
    "  lll                      lll  ",
    "  lll                      lll  ",
    "  lll                      lll  ",
    "  lll                      lll  ",
    "  lll                      lll  ",
    " sssss                    sssss ",
    "                                ",
], desk_pal)

# ---------------------------------------------------------------- chair 16x16
chair_pal = {
    "B": hexc("232a38"),
    "b": hexc("2e3648"),
    "c": hexc("39445c"),
    "C": hexc("42506c"),
    "l": hexc("1d222c"),
    "s": (0, 0, 0, 60),
}
png("chair.png", [
    "                ",
    "    BBBBBBBB    ",
    "   BbbbbbbbbB   ",
    "   BbbbbbbbbB   ",
    "   BbbbbbbbbB   ",
    "   BbbbbbbbbB   ",
    "   BbbbbbbbbB   ",
    "  BccccccccccB  ",
    "  BcCCCCCCCCcB  ",
    "  BccccccccccB  ",
    "   l        l   ",
    "   l        l   ",
    "  ll        ll  ",
    "  ssss    ssss  ",
    "                ",
    "                ",
], chair_pal)

# ---------------------------------------------------------------- monitors 16x12
mon_common = {
    "f": hexc("323a4d"),
    "F": hexc("3d4burning65"[:6]) if False else hexc("3d4763"),
    "m": hexc("2a3140"),
    "s": (0, 0, 0, 60),
}
mon_off = dict(mon_common)
mon_off.update({"o": hexc("0d1117")})
png("monitor_off.png", [
    " ffffffffffffff ",
    " foooooooooooof ",
    " foooooooooooof ",
    " foooooooooooof ",
    " foooooooooooof ",
    " foooooooooooof ",
    " foooooooooooof ",
    " foooooooooooof ",
    " ffffffffffffff ",
    "      mmmm      ",
    "     mmmmmm     ",
    "    ssssssss    ",
], mon_off)

mon_on = {
    "f": hexc("3d4763"),
    "m": hexc("2a3140"),
    "s": (0, 0, 0, 60),
    "S": hexc("0f2733"),
    "t": hexc("50c8ff"),
    "d": hexc("2b7a9e"),
    "C": hexc("a6e3ff"),
}
png("monitor_on_a.png", [
    " ffffffffffffff ",
    " fSSSSSSSSSSSSf ",
    " fSttttttdSSSSf ",
    " fSSSSSSSSSSSSf ",
    " fSttttdSSSSSSf ",
    " fSSSSSSSSSSSSf ",
    " fSttttttttdSSf ",
    " fSCSSSSSSSSSSf ",
    " ffffffffffffff ",
    "      mmmm      ",
    "     mmmmmm     ",
    "    ssssssss    ",
], mon_on)
png("monitor_on_b.png", [
    " ffffffffffffff ",
    " fSSSSSSSSSSSSf ",
    " fSttttttdSSSSf ",
    " fSSSSSSSSSSSSf ",
    " fSttttttttdSSf ",
    " fSSSSSSSSSSSSf ",
    " fSttttdSSSSSSf ",
    " fSSSCSSSSSSSSf ",
    " ffffffffffffff ",
    "      mmmm      ",
    "     mmmmmm     ",
    "    ssssssss    ",
], mon_on)

# ---------------------------------------------------------------- workers (back view, seated)
# personas: (body, body-shade, hair, hair-shade)
PERSONAS = {
    "nova":   ("4f89c2", "3f6e9e", "2b4a68", "223b53"),
    "mika":   ("c27a9e", "9e6181", "6e3d55", "583144"),
    "tetsuo": ("5fa06b", "4c8156", "2f5537", "26442c"),
    "bob":    ("c29455", "9e7844", "6e5228", "584220"),
}

def worker_rows(arms):
    # arms: 'up' (type A), 'mid' (type B), 'down' (idle)
    a_row_up   = "  aPPPPPPPPPPa  "
    a_row_mid  = " a PPPPPPPPPP a "
    base = [
        "                ",
        "     HHHHHH     ",
        "    HHHHHHHH    ",
        "   HHHHHHHHHH   ",
        "   HhhHHHHhhH   ",
        "   HHHHHHHHHH   ",
        "    HHHHHHHH    ",
        "     kkkkkk     ",
        "    PPPPPPPP    ",
        "   PPPPPPPPPP   ",  # 9  shoulders
        "   PPPPPPPPPP   ",  # 10
        "   pPPPPPPPPp   ",  # 11
        "   pPPPPPPPPp   ",  # 12
        "    pppppppp    ",
        "                ",
        "                ",
    ]
    if arms == "up":
        base[9] = a_row_up
    elif arms == "mid":
        base[10] = a_row_mid
    return base

for name, (body, shade, hair, hshade) in PERSONAS.items():
    pal = {
        "H": hexc(hair),
        "h": hexc(hshade),
        "k": hexc("d9b48f"),
        "P": hexc(body),
        "p": hexc(shade),
        "a": hexc("d9b48f"),
    }
    png(f"worker_{name}_idle.png", worker_rows("down"), pal)
    png(f"worker_{name}_type_a.png", worker_rows("up"), pal)
    png(f"worker_{name}_type_b.png", worker_rows("mid"), pal)

# ---------------------------------------------------------------- PM (front view, walking)
pm_pal = {
    "H": hexc("4a3c66"),
    "h": hexc("3a2f52"),
    "k": hexc("d9b48f"),
    "e": hexc("1a1420"),
    "P": hexc("8f79c8"),
    "p": hexc("745fa6"),
    "f": hexc("2a2438"),
}

def pm_rows(step):
    rows = [
        "                ",
        "     HHHHHH     ",
        "    HHHHHHHH    ",
        "   HHHHHHHHHH   ",
        "   Hkkkkkkkkh   ",
        "   HkekkkkekH   ",
        "   Hkkkkkkkkh   ",
        "    kkkkkkkk    ",
        "    PPPPPPPP    ",
        "   PPPPPPPPPP   ",
        "   pPPPPPPPPp   ",
        "    PPPPPPPP    ",
        "    pppppppp    ",
        "                ",
        "                ",
        "                ",
    ]
    if step == "a":
        rows[13] = "    ff    ff    "
        rows[14] = "    ff          "
    else:
        rows[13] = "    ff    ff    "
        rows[14] = "          ff    "
    return rows

png("pm_walk_a.png", pm_rows("a"), pm_pal)
png("pm_walk_b.png", pm_rows("b"), pm_pal)

# ---------------------------------------------------------------- reviewer (front view, gesturing)
rev_pal = {
    "H": hexc("7a3a3a"),
    "h": hexc("5e2c2c"),
    "k": hexc("d9b48f"),
    "e": hexc("1a1420"),
    "G": hexc("11151d"),  # glasses band
    "P": hexc("c26a52"),
    "p": hexc("9e5542"),
    "a": hexc("d9b48f"),
    "f": hexc("2a2438"),
}

def reviewer_rows(gesture):
    rows = [
        "                ",
        "     HHHHHH     ",
        "    HHHHHHHH    ",
        "   HHHHHHHHHH   ",
        "   HGGGGGGGGh   ",
        "   HkkkkkkkkH   ",
        "   Hkkkkkkkkh   ",
        "    kkkkkkkk    ",
        "    PPPPPPPP    ",
        "   PPPPPPPPPP   ",  # 9
        "   pPPPPPPPPp   ",  # 10
        "    PPPPPPPP    ",
        "    pppppppp    ",
        "    ff    ff    ",
        "                ",
        "                ",
    ]
    if gesture == "a":
        rows[8] = "   aPPPPPPPP    "   # left arm raised (pointing)
    else:
        rows[9] = "  a PPPPPPPPPP  "   # arm swung out
    return rows

png("reviewer_point_a.png", reviewer_rows("a"), rev_pal)
png("reviewer_point_b.png", reviewer_rows("b"), rev_pal)

# ---------------------------------------------------------------- debate bubbles 12x10
bub_pal = {
    "w": hexc("e8ecf8"),
    "o": hexc("b9c2dd"),
    "x": hexc("1a1f2b"),
}
png("bubble_excl.png", [
    " wwwwwwwwww ",
    "wwwwwwwwwwww",
    "wwwwwxxwwwww",
    "wwwwwxxwwwww",
    "wwwwwxxwwwww",
    "wwwwwwwwwwww",
    "wwwwwxxwwwww",
    "owwwwwwwwwwo",
    "  ow        ",
    "   w        ",
], bub_pal)
png("bubble_q.png", [
    " wwwwwwwwww ",
    "wwwwxxxwwwww",
    "wwwxwwwxwwww",
    "wwwwwwxxwwww",
    "wwwwwxxwwwww",
    "wwwwwxxwwwww",
    "wwwwwwwwwwww",
    "owwwwxxwwwwo",
    "        wo  ",
    "        w   ",
], bub_pal)

print("done")
