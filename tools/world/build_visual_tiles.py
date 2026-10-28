"""Build original pixel-art materials for the Three.js world view.

These images are render-only assets. They do not change block identities or
any wallet/ledger data. Run: python3 tools/world/build_visual_tiles.py
"""
from pathlib import Path
from PIL import Image, ImageDraw
import random

OUT = Path(__file__).resolve().parents[2] / "desktop-wallet/ui/assets/world-tiles"
OUT.mkdir(parents=True, exist_ok=True)
S = 64


def canvas(color):
    return Image.new("RGBA", (S, S), (*color, 255))


def patches(im, seed, colors, count, lo=2, hi=7):
    r = random.Random(seed)
    d = ImageDraw.Draw(im)
    for _ in range(count):
        x, y = r.randrange(S), r.randrange(S)
        w, h = r.randint(lo, hi), r.randint(lo, hi)
        d.rectangle((x, y, x + w, y + h), fill=(*r.choice(colors), 255))


def save(name, im):
    im.save(OUT / f"{name}.png", optimize=True)


grass = canvas((93, 127, 64))
patches(grass, 1, [(75, 111, 52), (111, 148, 75), (129, 157, 81), (66, 104, 51)], 310, 1, 5)
patches(grass, 2, [(51, 91, 44), (145, 164, 91)], 42, 1, 2)
save("grass", grass)

dirt = canvas((117, 91, 67))
patches(dirt, 3, [(95, 70, 52), (138, 109, 78), (155, 118, 82), (79, 62, 50)], 290, 1, 6)
save("dirt", dirt)

grass_side = dirt.copy()
d = ImageDraw.Draw(grass_side)
d.rectangle((0, 0, 63, 12), fill=(87, 128, 58, 255))
r = random.Random(4)
for x in range(0, 64, 2):
    d.rectangle((x, 7, x + r.randrange(1, 4), 11 + r.randrange(2, 11)), fill=r.choice([(75, 117, 52, 255), (112, 152, 73, 255), (65, 108, 48, 255)]))
save("grass_side", grass_side)

stone = canvas((126, 130, 130))
patches(stone, 5, [(99, 105, 108), (147, 151, 148), (163, 166, 159), (112, 119, 122)], 280, 2, 8)
save("stone", stone)

deep = canvas((75, 79, 86))
patches(deep, 6, [(52, 58, 67), (96, 102, 106), (112, 112, 110)], 250, 2, 8)
save("deep", deep)

brick = stone.copy()
d = ImageDraw.Draw(brick)
for row, y in enumerate(range(0, 64, 16)):
    d.rectangle((0, y, 63, y + 2), fill=(68, 71, 73, 255))
    for x in range((-16 if row % 2 else 0), 65, 32):
        d.rectangle((x, y, x + 2, y + 16), fill=(66, 70, 73, 255))
        d.line((x + 3, y + 4, x + 29, y + 4), fill=(172, 175, 170, 255), width=1)
save("brick", brick)

planks = canvas((151, 112, 70))
d = ImageDraw.Draw(planks)
r = random.Random(7)
for y in range(0, 64, 16):
    d.rectangle((0, y, 63, y + 2), fill=(77, 53, 38, 255))
    d.line((0, y + 4, 63, y + 4), fill=(194, 151, 94, 255), width=2)
    d.line((0, y + 12, 63, y + 12), fill=(106, 75, 49, 255), width=1)
    for _ in range(12):
        x = r.randrange(64)
        yy = y + r.randrange(5, 12)
        d.line((x, yy, min(63, x + r.randrange(3, 12)), yy), fill=r.choice([(117, 82, 52, 255), (179, 132, 79, 255)]), width=1)
    d.rectangle(((8 if y % 32 else 37), y + 2, (10 if y % 32 else 39), y + 15), fill=(94, 65, 43, 255))
save("planks", planks)

bark = canvas((99, 70, 46))
d = ImageDraw.Draw(bark)
r = random.Random(8)
for x in range(0, 64, 6):
    d.line((x, 0, x + r.randrange(-2, 3), 63), fill=(60, 45, 36, 255), width=r.randrange(1, 3))
    d.line((x + 3, 0, x + 3 + r.randrange(-2, 3), 63), fill=(145, 103, 59, 255), width=1)
for _ in range(65):
    x, y = r.randrange(64), r.randrange(64)
    d.line((x, y, x + r.randrange(-2, 3), y + r.randrange(2, 8)), fill=(58, 44, 35, 255), width=1)
save("bark", bark)

leaves = canvas((57, 105, 51))
patches(leaves, 9, [(35, 78, 39), (77, 132, 57), (108, 153, 68), (40, 90, 49), (135, 164, 77)], 420, 1, 6)
save("leaves", leaves)

roof = canvas((98, 60, 47))
d = ImageDraw.Draw(roof)
for y in range(0, 64, 12):
    d.rectangle((0, y, 63, y + 2), fill=(55, 34, 35, 255))
    for x in range((-12 if (y // 12) % 2 else 0), 64, 24):
        d.rectangle((x, y + 2, x + 1, y + 11), fill=(63, 39, 37, 255))
        d.line((x + 3, y + 4, x + 18, y + 4), fill=(142, 88, 59, 255), width=2)
save("roof", roof)

sand = canvas((204, 183, 136))
patches(sand, 10, [(180, 158, 111), (223, 202, 156), (238, 218, 178)], 220, 1, 5)
save("sand", sand)

snow = canvas((225, 233, 238))
patches(snow, 11, [(200, 215, 227), (244, 246, 245), (187, 206, 219)], 135, 2, 8)
save("snow", snow)

water = canvas((42, 104, 153))
d = ImageDraw.Draw(water)
r = random.Random(12)
for _ in range(90):
    x, y = r.randrange(64), r.randrange(64)
    d.line((x, y, min(63, x + r.randrange(3, 17)), y), fill=r.choice([(65, 143, 181, 255), (96, 163, 192, 255), (28, 79, 136, 255)]), width=r.randrange(1, 3))
save("water", water)
