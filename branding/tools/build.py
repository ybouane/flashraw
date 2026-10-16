"""FlashRaw brand asset builder.

Writes every SVG in branding/ and renders the PNG/ICO files from them. All text is
outlined to paths, so the outputs need no fonts.

    pip install pillow fonttools uharfbuzz cairosvg
    python branding/tools/build.py --fetch-fonts   # once: downloads the two OFL fonts
    python branding/tools/build.py [OUT_DIR]       # default OUT_DIR: branding/
"""
import io, os, sys, math, random
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = os.path.join(HERE, "fonts")
FONT_URLS = {
    "PlusJakartaSans[wght].ttf": "https://raw.githubusercontent.com/google/fonts/main/ofl/plusjakartasans/PlusJakartaSans%5Bwght%5D.ttf",
    "JetBrainsMono[wght].ttf": "https://raw.githubusercontent.com/google/fonts/main/ofl/jetbrainsmono/JetBrainsMono%5Bwght%5D.ttf",
}
if "--fetch-fonts" in sys.argv:
    os.makedirs(FONTS, exist_ok=True)
    for name, url in FONT_URLS.items():
        urllib.request.urlretrieve(url, os.path.join(FONTS, name))
    sys.exit(0)

import cairosvg
from PIL import Image
sys.path.insert(0, HERE)
from textpath import text_path

OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(HERE)
os.makedirs(OUT, exist_ok=True)
SANS = os.path.join(FONTS, "PlusJakartaSans[wght].ttf")
MONO = os.path.join(FONTS, "JetBrainsMono[wght].ttf")

# ---- palette -------------------------------------------------------------
INDIGO = "#4733E6"      # primary
INDIGO_DK = "#3A26CF"   # mark: R/B photosites (darker)
INDIGO_LT = "#5A47F2"   # mark: G photosites (lighter, as green is the bright channel)
INDIGO_ON_DARK = "#A89BFF"
AMBER = "#FFB224"       # the flash
INK = "#15142B"
SLATE = "#5A5D78"
MIST = "#F5F4FD"
LINE = "#E4E2F3"
RED, GREEN, BLUE = "#F0505A", "#1DB07A", "#2F7BF2"

# ---- mark (64-unit grid) -------------------------------------------------
R = 14
TILE = f"M{R} 0H{64-R}A{R} {R} 0 0 1 64 {R}V{64-R}A{R} {R} 0 0 1 {64-R} 64H{R}A{R} {R} 0 0 1 0 {64-R}V{R}A{R} {R} 0 0 1 {R} 0Z"
CELL_TR = f"M32 0H{64-R}A{R} {R} 0 0 1 64 {R}V32H32Z"
CELL_BL = f"M0 32H32V64H{R}A{R} {R} 0 0 1 0 {64-R}Z"
BOLT_PTS = [(29.5, 4), (47.5, 4), (35.5, 28), (51, 28), (21, 60), (29, 36), (13.5, 36)]


def pts_d(pts, sx=1, sy=1, dx=0, dy=0):
    f = lambda v: f"{v:.2f}".rstrip("0").rstrip(".")
    return "M" + "L".join(f"{f(x*sx+dx)} {f(y*sy+dy)}" for x, y in pts) + "Z"


BOLT = pts_d(BOLT_PTS)
# favicon: fuller bolt, shelf and top edge on the 4-unit (16 px) grid
BOLT_SMALL = pts_d([(28, 4), (48, 4), (36, 28), (54, 28), (20, 62), (28, 36), (10, 36)])


def mark_body(variant="color"):
    if variant == "color":
        return (f'<path fill="{INDIGO_DK}" d="{TILE}"/>'
                f'<path fill="{INDIGO_LT}" d="{CELL_TR}{CELL_BL}"/>'
                f'<path fill="{AMBER}" d="{BOLT}"/>')
    if variant == "mono":  # single colour, bolt knocked out
        return f'<path fill="{INK}" fill-rule="evenodd" d="{TILE}{BOLT}"/>'
    if variant == "flat":  # small sizes: flat tile, no photosite checker
        return f'<path fill="{INDIGO}" d="{TILE}"/><path fill="{AMBER}" d="{BOLT_SMALL}"/>'
    raise ValueError(variant)


def svg_doc(w, h, body, title=None, vb=None):
    vb = vb or f"0 0 {w} {h}"
    t = f"<title>{title}</title>" if title else ""
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{vb}" '
            f'role="img"{" aria-label=" + chr(34) + title + chr(34) if title else ""}>{t}{body}</svg>\n')


# ---- wordmark --------------------------------------------------------------
WM_WGHT = 750
WM_TRACK = -0.015


def wordmark(x, baseline, size, c1, c2):
    d1, a1, b1 = text_path("Flash", SANS, size, x, baseline, WM_WGHT, WM_TRACK)
    gap = WM_TRACK * size
    d2, a2, b2 = text_path("Raw", SANS, size, x + a1 + gap, baseline, WM_WGHT, WM_TRACK)
    body = f'<path fill="{c1}" d="{d1}"/><path fill="{c2}" d="{d2}"/>'
    return body, b2[2]


CAP = 0.745  # Plus Jakarta Sans cap height / em


def lockup(dark=False, mark_size=64.0):
    """Horizontal logo. Returns (body, width, height)."""
    s = mark_size / 64
    size = mark_size * 0.50 / CAP          # cap height = 50% of mark
    gap = mark_size * 0.25
    base = mark_size / 2 + size * CAP / 2  # cap height centred on the mark
    c1 = "#FFFFFF" if dark else INK
    c2 = INDIGO_ON_DARK if dark else INDIGO
    wm, right = wordmark(mark_size + gap, base, size, c1, c2)
    body = f'<g transform="scale({s:g})">{mark_body()}</g>{wm}' if s != 1 else mark_body() + wm
    return body, right, mark_size


# ---- render helpers ----------------------------------------------------------
def write(name, text):
    with open(os.path.join(OUT, name), "w") as f:
        f.write(text)


def png(svg_text, w, h, bg=None):
    data = cairosvg.svg2png(bytestring=svg_text.encode(), output_width=w, output_height=h)
    im = Image.open(io.BytesIO(data)).convert("RGBA")
    if bg:
        b = Image.new("RGBA", im.size, bg); b.alpha_composite(im); im = b
    return im


def pad_icon(body, scale, bg):
    """Square 64-unit icon: body scaled about the centre on an opaque background."""
    off = 32 * (1 - scale)
    return svg_doc(64, 64, f'<rect width="64" height="64" fill="{bg}"/>'
                           f'<g transform="translate({off:g} {off:g}) scale({scale:g})">{body}</g>')


# ---- mosaic art ------------------------------------------------------------
def inside(px, py, poly):
    c = False
    n = len(poly)
    for i in range(n):
        x1, y1 = poly[i]; x2, y2 = poly[(i + 1) % n]
        if (y1 > py) != (y2 > py) and px < (x2 - x1) * (py - y1) / (y2 - y1) + x1:
            c = not c
    return c


def mosaic(x0, y0, w, h, pitch, bolt_box, fade_from, fade_to, seed=7, max_op=0.30):
    """Bayer RGGB photosite field sampling an amber bolt: each photosite's amber
    opacity is the fraction of it the bolt covers (an anti-aliased, 'sensed' bolt).
    bolt_box = (bx, by, bsize): the 64-unit bolt placed at bx,by scaled to bsize."""
    rnd = random.Random(seed)
    bx, by, bs = bolt_box
    poly = [(bx + x * bs / 64, by + y * bs / 64) for x, y in BOLT_PTS]
    ys = [p[1] for p in poly]; top, bot = min(ys), max(ys)
    gap = pitch * 0.18
    cell = pitch - gap
    rx = cell * 0.24
    out = []
    n = 6
    for j in range(int(math.ceil(h / pitch)) + 1):
        for i in range(int(math.ceil(w / pitch)) + 1):
            x = x0 + i * pitch; y = y0 + j * pitch
            if x > x0 + w or y > y0 + h:
                continue
            cx = x + cell / 2
            t = max(0.0, min(1.0, (cx - fade_from) / (fade_to - fade_from)))
            cov = sum(inside(x - gap / 2 + (a + .5) * pitch / n, y - gap / 2 + (b + .5) * pitch / n, poly)
                      for a in range(n) for b in range(n)) / (n * n)
            base = max_op * (t ** 1.5) * (0.6 + 0.4 * rnd.random()) * (cov <= 0.02)
            if base >= 0.02:
                col = [[RED, GREEN], [GREEN, BLUE]][j % 2][i % 2]
                out.append(f'<rect x="{x:.1f}" y="{y:.1f}" width="{cell:.1f}" height="{cell:.1f}" rx="{rx:.1f}" fill="{col}" fill-opacity="{base:.2f}"/>')
            if cov > 0.02:
                out.append(f'<rect x="{x:.1f}" y="{y:.1f}" width="{cell:.1f}" height="{cell:.1f}" rx="{rx:.1f}" fill="url(#amb)" fill-opacity="{cov:.2f}"/>')
    grad = (f'<linearGradient id="amb" gradientUnits="userSpaceOnUse" x1="0" y1="{top:.0f}" x2="0" y2="{bot:.0f}">'
            f'<stop offset="0" stop-color="#FFC94A"/><stop offset="1" stop-color="#FF9A1F"/></linearGradient>')
    return f"<defs>{grad}</defs>" + "".join(out)


# ---- banners ---------------------------------------------------------------
def text(s, x, y, size, color, wght=500, font=SANS, track=0.0):
    d, adv, bb = text_path(s, font, size, x, y, wght, track)
    return f'<path fill="{color}" d="{d}"/>', adv


def facts_list(x, y, items, size, lh):
    out = []
    for k, (col, s) in enumerate(items):
        yy = y + k * lh
        sq = size * 0.5
        out.append(f'<rect x="{x}" y="{yy - size*0.36 - sq/2:.1f}" width="{sq:.1f}" height="{sq:.1f}" rx="{sq*0.22:.1f}" fill="{col}"/>')
        out.append(text(s, x + sq + size * 0.6, yy, size, INK, 560)[0])
    return "".join(out)


def og_banner():
    W, H = 1200, 630
    lk, lw, lh = lockup(mark_size=60)
    body = [f'<rect width="{W}" height="{H}" fill="#FFFFFF"/>',
            '<defs><linearGradient id="g" x1="0" x2="1" y1="0" y2="0">'
            f'<stop offset="0.45" stop-color="#FFFFFF"/><stop offset="1" stop-color="{MIST}"/></linearGradient></defs>',
            f'<rect width="{W}" height="{H}" fill="url(#g)"/>',
            mosaic(700, -4, 520, 650, 24, (776, 48, 520), 730, 1180),
            f'<g transform="translate(80 70)">{lk}</g>',
            text("The fast, safe", 80, 258, 62, INK, 780, track=-0.025)[0],
            text("camera RAW decoder.", 80, 332, 62, INK, 780, track=-0.025)[0],
            facts_list(82, 424, [(RED, "8–15× faster than the alternatives"),
                                 (GREEN, "Memory-safe Rust · WASM · GPU"),
                                 (BLUE, "Every camera on raw.pixls.us")], 27, 46),
            text("flashraw.ybouane.com", 80, 568, 21, INDIGO, 600, font=MONO)[0]]
    return svg_doc(W, H, "".join(body), "FlashRaw: the fast, safe camera RAW decoder")


def readme_banner():
    W, H = 1600, 500
    lk, lw, lh = lockup(mark_size=84)
    body = [f'<rect width="{W}" height="{H}" fill="#FFFFFF"/>',
            '<defs><linearGradient id="g" x1="0" x2="1" y1="0" y2="0">'
            f'<stop offset="0.5" stop-color="#FFFFFF"/><stop offset="1" stop-color="{MIST}"/></linearGradient></defs>',
            f'<rect width="{W}" height="{H}" fill="url(#g)"/>',
            mosaic(880, -6, 740, 520, 24, (1080, 30, 440), 920, 1480),
            f'<g transform="translate(96 80)">{lk}</g>',
            text("The fast, safe camera RAW decoder.", 98, 252, 46, INK, 760, track=-0.022)[0],
            facts_list(100, 332, [(RED, "8–15× faster than the alternatives"),
                                  (GREEN, "Memory-safe Rust · WASM · GPU"),
                                  (BLUE, "Every camera on raw.pixls.us")], 27, 46)]
    return svg_doc(W, H, "".join(body), "FlashRaw: the fast, safe camera RAW decoder")


def main():
    # mark + favicon
    write("logo-mark.svg", svg_doc(64, 64, mark_body(), "FlashRaw"))
    write("logo-mark-mono.svg", svg_doc(64, 64, mark_body("mono"), "FlashRaw"))
    write("favicon.svg", svg_doc(64, 64, mark_body("flat"), "FlashRaw"))
    for dark, name in ((False, "logo.svg"), (True, "logo-dark.svg")):
        body, w, h = lockup(dark)
        w = math.ceil(w)
        write(name, svg_doc(w, 64, body, "FlashRaw"))
    og = og_banner(); write("og-banner.svg", og); png(og, 1200, 630).convert("RGB").save(os.path.join(OUT, "og-banner.png"), optimize=True)
    rb = readme_banner(); write("readme-banner.svg", rb); png(rb, 1600, 500).convert("RGB").save(os.path.join(OUT, "readme-banner.png"), optimize=True)

    fav = open(os.path.join(OUT, "favicon.svg")).read()
    mark = open(os.path.join(OUT, "logo-mark.svg")).read()
    png(fav, 32, 32).save(os.path.join(OUT, "favicon-32.png"), optimize=True)
    ico = [png(fav, s, s) for s in (16, 32, 48)]
    ico[2].save(os.path.join(OUT, "favicon.ico"), sizes=[(16, 16), (32, 32), (48, 48)], append_images=ico[:2])
    png(mark, 192, 192).save(os.path.join(OUT, "icon-192.png"), optimize=True)
    png(mark, 512, 512).save(os.path.join(OUT, "icon-512.png"), optimize=True)
    # apple touch: opaque, iOS rounds corners itself -> full-bleed square tile
    sq = (f'<rect width="64" height="64" fill="{INDIGO_DK}"/><path fill="{INDIGO_LT}" d="M32 0H64V32H32ZM0 32H32V64H0Z"/>')
    apple = svg_doc(64, 64, sq + f'<g transform="translate(6.4 6.4) scale(0.8)"><path fill="{AMBER}" d="{BOLT}"/></g>')
    png(apple, 180, 180).convert("RGB").save(os.path.join(OUT, "apple-touch-icon.png"), optimize=True)
    # maskable: full-bleed background, artwork inside the 80% safe circle
    mk = svg_doc(64, 64, sq + f'<g transform="translate(9.6 9.6) scale(0.7)"><path fill="{AMBER}" d="{BOLT}"/></g>')
    png(mk, 512, 512).convert("RGB").save(os.path.join(OUT, "icon-512-maskable.png"), optimize=True)


if __name__ == "__main__":
    main()
