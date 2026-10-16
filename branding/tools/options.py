"""FlashRaw logo options: four monochrome directions for review.

Writes branding/options/option-N.svg (horizontal lockup, for light backgrounds),
option-N-dark.svg (for dark backgrounds), option-N-mark.svg (square mark) and the
comparison sheet options-sheet.svg / .png. All text is outlined to paths.

    pip install pillow fonttools uharfbuzz cairosvg
    python branding/tools/options.py --fetch-fonts   # once; fonts go to tools/fonts/ (ignored)
    python branding/tools/options.py [OUT_DIR]       # default OUT_DIR: branding/options/
"""
import io, os, sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = os.path.join(HERE, "fonts")
GF = "https://raw.githubusercontent.com/google/fonts/main/ofl/"
FONT_URLS = {  # all SIL Open Font License 1.1
    "SourceSerif4[opsz,wght].ttf": GF + "sourceserif4/SourceSerif4%5Bopsz,wght%5D.ttf",
    "IBMPlexSerif-SemiBold.ttf": GF + "ibmplexserif/IBMPlexSerif-SemiBold.ttf",
    "Newsreader[opsz,wght].ttf": GF + "newsreader/Newsreader%5Bopsz,wght%5D.ttf",
    "Newsreader-Italic[opsz,wght].ttf": GF + "newsreader/Newsreader-Italic%5Bopsz,wght%5D.ttf",
    "Fraunces[SOFT,WONK,opsz,wght].ttf": GF + "fraunces/Fraunces%5BSOFT,WONK,opsz,wght%5D.ttf",
    "PlusJakartaSans[wght].ttf": GF + "plusjakartasans/PlusJakartaSans%5Bwght%5D.ttf",
}
if "--fetch-fonts" in sys.argv:
    os.makedirs(FONTS, exist_ok=True)
    for name, url in FONT_URLS.items():
        if not os.path.exists(os.path.join(FONTS, name)):
            urllib.request.urlretrieve(url, os.path.join(FONTS, name))
    sys.exit(0)

import cairosvg
from PIL import Image
sys.path.insert(0, HERE)
from textpath import text_path

OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(HERE), "options")
os.makedirs(OUT, exist_ok=True)


def font(name):
    return os.path.join(FONTS, name)


INK = "#111111"
PAPER = "#FFFFFF"
NIGHT = "#0B0B0C"
# Sensor tints: used only inside a photosite quad, never for type.
RED, GREEN, BLUE = "#D9473B", "#2E9A5C", "#2F63D6"


def num(v):
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def poly(pts):
    return "M" + "L".join(f"{num(x)} {num(y)}" for x, y in pts) + "Z"


def rect(x, y, w, h):
    return poly([(x, y), (x + w, y), (x + w, y + h), (x, y + h)])


def rot180(pts, c=16):
    return [(c - x, c - y) for x, y in pts]


# ---- marks ---------------------------------------------------------------
# Every mark is drawn on a 16-unit grid (one unit = one pixel at 16 px) and
# returned as a list of (fill_role, path_d); fill_role is "fg", "paper" (knock-out
# colour, only where a true hole is impossible) or a tint colour.

def mark_quad(small=False):
    """1. A sensor quad split by a flash: the vertical gutter is a lightning bolt,
    the horizontal one a hairline. small: the bolt redrawn in whole pixels for 16 px."""
    if small:
        # white pixel runs per row (x0, x1 inclusive); row 8 is the hairline
        runs = {0: (9, 10), 1: (9, 10), 2: (8, 9), 3: (8, 9), 4: (7, 8), 5: (7, 8), 6: (6, 7),
                7: (6, 10), 8: (0, 15), 9: (9, 10), 10: (8, 9), 11: (8, 9), 12: (7, 8), 13: (7, 8),
                14: (6, 7), 15: (6, 7)}
        d = ""
        for y in range(16):
            x0, x1 = runs[y]
            if x0 > 0:
                d += rect(0, y, x0, 1)
            if x1 < 15:
                d += rect(x1 + 1, y, 15 - x1, 1)
        return [("fg", d)]
    s = 3 / 8          # bolt slant, dx per dy
    J = 3.0            # jog of the bolt at the centre line
    w = 2.0            # bolt width (horizontal)
    ct8 = 8 - J / 2    # top band centre at y = 8
    a = ct8 + s * 8 - w / 2  # top band left edge at y = 0
    L = lambda y: a - s * y               # top band left edge
    Rb = lambda y: 16 - L(16 - y)         # bottom band right edge
    h0 = 7.5                              # hairline 7.5 to 8.5

    def top(h0):
        tl = [(0, 0), (a, 0), (L(h0), h0), (0, h0)]
        tr = [(a + w, 0), (16, 0), (16, h0), (Rb(h0), h0), (Rb(7), 7), (L(7) + w, 7)]
        return tl, tr
    tl, tr = top(h0)
    return [("fg", poly(tl) + poly(tr) + poly(rot180(tl)) + poly(rot180(tr)))]


F_CELLS = [(0, 0), (0, 1), (0, 2), (0, 3), (1, 0), (2, 0), (3, 0), (1, 2), (2, 2)]


def mark_monogram(small=False):
    """2. An F lit on a 4 x 4 photosite grid; the unlit photosites remain as points.
    small: points snapped to whole pixels at 16 px."""
    lit = "".join(rect(i * 4, j * 4, 4, 4) for i, j in F_CELLS)
    o = 2 if small else 1.5
    dots = "".join(rect(i * 4 + o, j * 4 + o, 1, 1)
                   for i in range(4) for j in range(4) if (i, j) not in F_CELLS)
    return [("fg", lit + dots)]


def mark_aperture(small=False):
    """3. Four blades on a 3 x 3 grid close like a square iris; the light that passes
    lands on one RGGB quad."""
    blades = rect(0, 0, 10, 4) + rect(12, 0, 4, 10) + rect(6, 12, 10, 4) + rect(0, 6, 4, 10)
    return [("fg", blades), (RED, rect(6, 6, 2, 2)), (GREEN, rect(8, 6, 2, 2) + rect(6, 8, 2, 2)),
            (BLUE, rect(8, 8, 2, 2))]


FRA = ("Fraunces[SOFT,WONK,opsz,wght].ttf", 640, (("opsz", 144), ("SOFT", 0), ("WONK", 1)))


def stop_quad(x, y, c):
    """A full stop made of one RGGB photosite quad, c x c cells."""
    return [(RED, rect(x, y, c, c)), (GREEN, rect(x + c, y, c, c) + rect(x, y + c, c, c)),
            (BLUE, rect(x + c, y + c, c, c))]


def mark_letter(small=False):
    """4. A serif F with a photosite full stop, cut from a square.
    small: the F redrawn on the 16 px grid (stem 2 px, arms and serifs 1 px)."""
    if small:
        F = (rect(4, 3, 2, 10) + rect(3, 3, 9, 1) + rect(11, 4, 1, 2) + rect(6, 7, 3, 1) +
             rect(9, 6, 1, 3) + rect(3, 12, 5, 1))
        return [("fg", rect(0, 0, 16, 16) + F, "evenodd")] + stop_quad(11, 11, 1)
    f, w, ax = FRA
    w = 760                            # heavier cut, so the hairlines survive at 32 px
    d, adv, bb = text_path("F", font(f), 100, 0, 0, wght=w, extra_axes=ax)
    cap = -bb[1]
    k = 10.5 / cap                     # cap height 10.5 units
    gw = (bb[2] - bb[0]) * k
    c = 1.25                           # quad cell
    gap = 0.75
    total = gw + gap + 2 * c
    x0 = (16 - total) / 2 - bb[0] * k
    base = 13.25
    d, _, _ = text_path("F", font(f), 100 * k, x0, base, wght=w, extra_axes=ax)
    qx = x0 + bb[2] * k + gap
    return [("fg", rect(0, 0, 16, 16) + d, "evenodd")] + stop_quad(qx, base - 2 * c, c)


# ---- wordmarks -----------------------------------------------------------
OPTIONS = [
    dict(n=1, name="Quad", mark=mark_quad,
         font=("SourceSerif4[opsz,wght].ttf", 690, (("opsz", 60),)), cap=0.58, gap=0.34, track=0.004,
         why="A Bayer quad split by its own flash: the vertical gutter is a lightning bolt. Source Serif 4."),
    dict(n=2, name="Photosite F", mark=mark_monogram,
         font=("IBMPlexSerif-SemiBold.ttf", 600, ()), cap=0.54, gap=0.34, track=-0.01,
         why="The initial lit on a 4 × 4 sensor grid; the unlit photosites stay as points. IBM Plex Serif."),
    dict(n=3, name="Aperture", mark=mark_aperture,
         font=("Newsreader[opsz,wght].ttf", 540, (("opsz", 60),)), cap=0.58, gap=0.34, track=-0.006,
         raw=("Newsreader-Italic[opsz,wght].ttf", 540, (("opsz", 60),), 0.0),
         why="Four mosaic blades close like a square iris; the light that passes lands on one RGGB quad. Newsreader, Raw in italic."),
    dict(n=4, name="Full stop", mark=mark_letter,
         font=FRA, cap=0.58, gap=0.34, track=0.004,
         why="Typographic: a Fraunces F whose full stop is a single RGGB photosite quad. Fraunces."),
]


def cap_height(fnt):
    f, w, ax = fnt
    _, _, bb = text_path("H", font(f), 1000, 0, 0, wght=w, extra_axes=ax)
    return -bb[1] / 1000


def parts(opt, px=64):
    """Mark geometry for a given render size: the grid-snapped variant at 16 px and below."""
    return opt["mark"](small=px <= 16)


def paint(parts, fg, paper):
    out = []
    for p in parts:
        role, d = p[0], p[1]
        rule = f' fill-rule="{p[2]}"' if len(p) > 2 else ""
        col = fg if role == "fg" else paper if role == "paper" else role
        out.append(f'<path fill="{col}"{rule} d="{d}"/>')
    return "".join(out)


def mark_svg_body(opt, fg=INK, paper=PAPER, size=64):
    return f'<g transform="scale({num(size / 16)})">{paint(parts(opt), fg, paper)}</g>'


def lockup(opt, fg=INK, paper=PAPER, M=64.0):
    """Horizontal lockup at mark height M. Returns (body, width, height)."""
    f, w, ax = opt["font"]
    ch = cap_height(opt["font"])
    size = opt["cap"] * M / ch
    base = M / 2 + opt["cap"] * M / 2           # cap height centred on the mark
    x = M + opt["gap"] * M
    if opt.get("raw"):  # "Raw" set in a second style
        d, adv, bb = text_path("Flash", font(f), size, x, base, wght=w, extra_axes=ax, tracking=opt["track"])
        f2, w2, ax2, dx = opt["raw"]
        d2, adv2, bb2 = text_path("Raw", font(f2), size, x + adv + dx * size, base, wght=w2, extra_axes=ax2, tracking=opt["track"])
        d += d2
        bb = (bb[0], min(bb[1], bb2[1]), bb2[2], max(bb[3], bb2[3]))
    else:
        d, adv, bb = text_path("FlashRaw", font(f), size, x, base, wght=w, extra_axes=ax, tracking=opt["track"])
    body = f'<g transform="scale({num(M / 16)})">{paint(parts(opt), fg, paper)}</g><path fill="{fg}" d="{d}"/>'
    right = bb[2]
    if opt.get("stop"):
        c = size * ch * 0.105           # quad cell: the stop is ~ the stem weight
        qx = bb[2] + size * 0.07
        body += paint(stop_quad(qx, base - 2 * c, c), fg, paper)
        right = qx + 2 * c
    top = min(0.0, bb[1])
    return body, right, M, top


def svg_doc(w, h, body, vb=None, title="FlashRaw"):
    vb = vb or f"0 0 {num(w)} {num(h)}"
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{num(w)}" height="{num(h)}" viewBox="{vb}" '
            f'role="img" aria-label="{title}"><title>{title}</title>{body}</svg>\n')


def write(name, text):
    with open(os.path.join(OUT, name), "w") as fh:
        fh.write(text)


def raster(svg_text, px):
    data = cairosvg.svg2png(bytestring=svg_text.encode(), output_width=px, output_height=px)
    return Image.open(io.BytesIO(data)).convert("RGBA")


def pixels(im, x, y, scale, bg):
    """Vector copy of a raster (one square per pixel, merged into one path per colour)
    so the sheet shows true pixels at any zoom."""
    bgc = tuple(int(bg[i:i + 2], 16) for i in (1, 3, 5))
    by = {}
    W, H = im.size
    for j in range(H):
        for i in range(W):
            r, g, b, a = im.getpixel((i, j))
            al = a / 255
            c = tuple(round(v * al + bv * (1 - al)) for v, bv in zip((r, g, b), bgc))
            if c != bgc:
                by.setdefault("#%02x%02x%02x" % c, []).append(
                    f"M{num(x + i * scale)} {num(y + j * scale)}h{scale}v{scale}h-{scale}Z")
    return "".join(f'<path fill="{c}" d="{"".join(ds)}"/>' for c, ds in by.items())


# ---- files ---------------------------------------------------------------
for opt in OPTIONS:
    n = opt["n"]
    write(f"option-{n}-mark.svg", svg_doc(64, 64, mark_svg_body(opt)))
    write(f"option-{n}-favicon.svg", svg_doc(16, 16, paint(parts(opt, 16), INK, PAPER)))
    for dark in (False, True):
        fg, paper = (PAPER, NIGHT) if dark else (INK, PAPER)
        body, w, h, top = lockup(opt, fg, paper)
        write(f"option-{n}{'-dark' if dark else ''}.svg", svg_doc(w, h - top, body, vb=f"0 {num(top)} {num(w)} {num(h - top)}"))


# ---- comparison sheet ----------------------------------------------------
SANS = font("PlusJakartaSans[wght].ttf")
GREY = "#6B6B70"
LINE = "#E3E3E6"


def label(s, x, y, size, color=INK, wght=500, anchor="start"):
    d, adv, bb = text_path(s, SANS, size, x, y, wght=wght)
    if anchor == "end":
        d, adv, bb = text_path(s, SANS, size, x - adv, y, wght=wght)
    return f'<path fill="{color}" d="{d}"/>'


SW = 1600
PAD = 48
ROW = 470
HEAD = 150
SH = HEAD + ROW * len(OPTIONS) + 24
out = [f'<rect width="{SW}" height="{SH}" fill="#F4F4F5"/>',
       label("FlashRaw — logo options", PAD, 70, 34, INK, 650),
       label("Black on white, white on black. Sensor tints (R, G, B) appear only inside a photosite quad. "
             "Small sizes are rendered at true pixels; the enlargements show those same pixels.", PAD, 106, 15, GREY, 500)]

def small_mark(opt, px, x, y, scale, fg, bg):
    msvg = svg_doc(px, px, paint(parts(opt, px), fg, bg), vb="0 0 16 16")
    return pixels(raster(msvg, px), x, y, scale, bg)


def header(opt, x, y, w, dark):
    bg, fg, navc = (NIGHT, PAPER, "#C9C9CE") if dark else (PAPER, INK, "#3A3A3F")
    o = [f'<rect x="{x}" y="{y}" width="{w}" height="64" fill="{bg}" stroke="{LINE if not dark else bg}"/>']
    body, lw, lh, top = lockup(opt, fg, bg, 28)
    o.append(f'<g transform="translate({x + 20} {y + 18})">{body}</g>')
    nx = x + w - 20
    for item in reversed(["Docs", "Demo", "Benchmarks", "GitHub"]):
        nx -= text_path(item, SANS, 14, 0, 0, wght=500)[1]
        o.append(label(item, nx, y + 37, 14, navc))
        nx -= 22
    return "".join(o)


def tab(opt, x, y, w, dark):
    chrome, tabc, txt, fg, bg = (("#202124", "#35363A", "#E8EAED", PAPER, "#35363A") if dark else
                                 ("#DEE1E6", "#FFFFFF", "#202124", INK, PAPER))
    o = [f'<rect x="{x}" y="{y}" width="{w}" height="64" fill="{chrome}"/>',
         f'<path fill="{tabc}" d="M{x + 12} {y + 64}V{y + 20}Q{x + 12} {y + 12} {x + 20} {y + 12}H{x + 252}'
         f'Q{x + 260} {y + 12} {x + 260} {y + 20}V{y + 64}Z"/>',
         small_mark(opt, 16, x + 26, y + 30, 1, fg, bg),
         label("FlashRaw · RAW decoding", x + 52, y + 43, 13, txt, 500),
         label("×", x + 236, y + 43, 14, txt, 500), label("+", x + 276, y + 44, 18, txt, 400)]
    return "".join(o)


for k, opt in enumerate(OPTIONS):
    y0 = HEAD + k * ROW
    X = PAD + 24
    out.append(f'<rect x="{PAD}" y="{y0}" width="{SW - 2 * PAD}" height="{ROW - 24}" fill="#FFFFFF" stroke="{LINE}"/>')
    out.append(label(str(opt["n"]), X, y0 + 44, 28, INK, 700))
    out.append(label(opt["name"], X + 36, y0 + 42, 20, INK, 650))
    nw = text_path(opt["name"], SANS, 20, 0, 0, wght=650)[1]
    out.append(label(opt["why"], X + 36 + nw + 18, y0 + 42, 15, GREY, 500))
    py = y0 + 70
    pw, ph = 500, 200
    for j, dark in enumerate((False, True)):
        px = X + j * (pw + 16)
        bg = NIGHT if dark else PAPER
        out.append(f'<rect x="{px}" y="{py}" width="{pw}" height="{ph}" fill="{bg}" stroke="{LINE}"/>')
        body, w, h, top = lockup(opt, PAPER if dark else INK, bg, 64)
        out.append(f'<g transform="translate({num(px + (pw - w) / 2)} {num(py + (ph - h) / 2)})">{body}</g>')
    mx = X + 2 * (pw + 16)
    for j, dark in enumerate((False, True)):
        bg, fg = (NIGHT, PAPER) if dark else (PAPER, INK)
        bx = mx + j * 216
        out.append(f'<rect x="{bx}" y="{py}" width="200" height="{ph}" fill="{bg}" stroke="{LINE}"/>')
        out.append(f'<g transform="translate({bx + 10} {py + 10}) scale({num(180 / 16)})">{paint(parts(opt, 180), fg, bg)}</g>')
    ry = py + ph + 20
    out.append(f'<rect x="{X}" y="{ry}" width="{pw}" height="150" fill="#FFFFFF" stroke="{LINE}"/>')
    sx = X + 24
    for px_ in (64, 32, 16):
        out.append(small_mark(opt, px_, sx, ry + 65 - px_ / 2, 1, INK, PAPER))
        out.append(label(f"{px_} px", sx + px_ / 2 - 12, ry + 132, 11, GREY))
        sx += px_ + 26
    sx += 10
    for px_, sc in ((32, 3), (16, 6)):
        out.append(small_mark(opt, px_, sx, ry + 17, sc, INK, PAPER))
        out.append(label(f"{px_} px ×{sc}", sx + 26, ry + 132, 11, GREY))
        sx += px_ * sc + 26
    ux = X + pw + 16
    out.append(header(opt, ux, ry, pw, False) + header(opt, ux, ry + 86, pw, True))
    tx = ux + pw + 16
    out.append(tab(opt, tx, ry, 416, False) + tab(opt, tx, ry + 86, 416, True))

sheet = svg_doc(SW, SH, "".join(out), title="FlashRaw logo options")
write("options-sheet.svg", sheet)
cairosvg.svg2png(bytestring=sheet.encode(), write_to=os.path.join(OUT, "options-sheet.png"))
print("wrote", OUT)
