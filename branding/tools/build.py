"""FlashRaw brand asset builder.

Writes every SVG in branding/ and renders the PNG/ICO files from them. All text is
outlined to paths, so the outputs need no fonts.

    pip install pillow fonttools uharfbuzz cairosvg
    python branding/tools/build.py --fetch-fonts   # once: downloads the OFL fonts to tools/fonts/
    python branding/tools/build.py [OUT_DIR]       # default OUT_DIR: branding/
"""
import io, os, sys, math
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = os.path.join(HERE, "fonts")
GF = "https://raw.githubusercontent.com/google/fonts/main/ofl/"
FONT_URLS = {  # all SIL Open Font License 1.1
    "Newsreader[opsz,wght].ttf": GF + "newsreader/Newsreader%5Bopsz,wght%5D.ttf",
    "Newsreader-Italic[opsz,wght].ttf": GF + "newsreader/Newsreader-Italic%5Bopsz,wght%5D.ttf",
    "JetBrainsMono[wght].ttf": GF + "jetbrainsmono/JetBrainsMono%5Bwght%5D.ttf",
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

OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(HERE)
os.makedirs(OUT, exist_ok=True)
SERIF = os.path.join(FONTS, "Newsreader[opsz,wght].ttf")
ITALIC = os.path.join(FONTS, "Newsreader-Italic[opsz,wght].ttf")
MONO = os.path.join(FONTS, "JetBrainsMono[wght].ttf")

# ---- palette (branding/README.md) ------------------------------------------
INK = "#0E0E0E"       # type, the mark, dark backgrounds
PAPER = "#FFFFFF"
BONE = "#F6F4EF"      # warm off-white: quiet panels
GRAPHITE = "#3A3A3A"  # secondary text
STONE = "#6B6B6B"     # muted text (5.3:1 on Paper)
ASH = "#A3A3A3"       # non-text greys: rules on dark, chart series
HAIRLINE = "#E2E0DA"  # rules and borders on light backgrounds
SMOKE = "#262626"     # rules on dark backgrounds
# Sensor tints: only inside the RGGB quad and as tiny accents, never for type.
RED, GREEN, BLUE = "#D9473B", "#2E9A5C", "#2F63D6"


def num(v):
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def rect(x, y, w, h):
    return f"M{num(x)} {num(y)}h{num(w)}v{num(h)}h{num(-w)}Z"


# ---- the mark: "Aperture" (16-unit grid, one unit = one pixel at 16 px) -----
# Four blades on a 3 x 3 grid close like a square iris; the light that passes
# lands on one RGGB quad, the only colour in the identity.
BLADES = rect(0, 0, 10, 4) + rect(12, 0, 4, 10) + rect(6, 12, 10, 4) + rect(0, 6, 4, 10)
QUAD = [(RED, rect(6, 6, 2, 2)), (GREEN, rect(8, 6, 2, 2) + rect(6, 8, 2, 2)), (BLUE, rect(8, 8, 2, 2))]


def mark(fg=INK, mono=False, cls=False):
    """Mark paths on the 16-unit grid. mono: the quad in the foreground colour."""
    c = ' class="fg"' if cls else ""
    if mono:
        return f'<path{c} fill="{fg}" d="{BLADES}{rect(6, 6, 4, 4)}"/>'
    return f'<path{c} fill="{fg}" d="{BLADES}"/>' + "".join(f'<path fill="{col}" d="{d}"/>' for col, d in QUAD)


def mark_at(x, y, size, fg=INK, mono=False):
    return f'<g transform="translate({num(x)} {num(y)}) scale({num(size / 16)})">{mark(fg, mono)}</g>'


# ---- type ------------------------------------------------------------------
def text(s, x, y, size, fill=INK, font=SERIF, wght=400, opsz=None, track=0.0, anchor="start"):
    """Outlined text; returns (svg, advance)."""
    axes = (("opsz", opsz),) if opsz is not None and font != MONO else ()
    d, adv, bb = text_path(s, font, size, 0, 0, wght=wght, tracking=track, extra_axes=axes)
    if anchor == "end":
        x -= adv
    elif anchor == "middle":
        x -= adv / 2
    d, adv, bb = text_path(s, font, size, x, y, wght=wght, tracking=track, extra_axes=axes)
    return f'<path fill="{fill}" d="{d}"/>', adv


def runs(parts, x, y, size, fill=INK, wght=400, opsz=72, track=0.0):
    """A line set in roman and italic runs: parts = [(text, italic), ...]."""
    out, cx = [], x
    for s, it in parts:
        svg, adv = text(s, cx, y, size, fill, ITALIC if it else SERIF, wght, opsz, track)
        out.append(svg)
        cx += adv + track * size
    return "".join(out), cx - x


# ---- wordmark and lockup ---------------------------------------------------
WM = dict(wght=540, opsz=60, track=-0.006)
WM_CAP = 0.58     # cap height as a fraction of the mark
WM_GAP = 0.34     # mark to wordmark, fraction of the mark
WM_RAW_DX = 0.0   # extra space before the italic "Raw", in em


def cap_height(font=SERIF, wght=WM["wght"], opsz=WM["opsz"]):
    _, _, bb = text_path("H", font, 1000, 0, 0, wght=wght, extra_axes=(("opsz", opsz),))
    return -bb[1] / 1000


def lockup(fg=INK, M=64.0, mono=False):
    """Horizontal logo at mark height M. Returns (body, width, height)."""
    size = WM_CAP * M / cap_height()
    base = M / 2 + WM_CAP * M / 2            # cap height centred on the mark
    x = M + WM_GAP * M
    ax = (("opsz", WM["opsz"]),)
    d1, a1, _ = text_path("Flash", SERIF, size, x, base, wght=WM["wght"], extra_axes=ax, tracking=WM["track"])
    d2, _, bb2 = text_path("Raw", ITALIC, size, x + a1 + (WM["track"] + WM_RAW_DX) * size, base,
                           wght=WM["wght"], extra_axes=ax, tracking=WM["track"])
    body = mark_at(0, 0, M, fg, mono) + f'<path fill="{fg}" d="{d1}{d2}"/>'
    return body, bb2[2], M


def svg_doc(w, h, body, title="FlashRaw", vb=None, extra=""):
    vb = vb or f"0 0 {num(w)} {num(h)}"
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{num(w)}" height="{num(h)}" viewBox="{vb}" '
            f'role="img" aria-label="{title}"><title>{title}</title>{extra}{body}</svg>\n')


# ---- render helpers ----------------------------------------------------------
def write(name, txt):
    with open(os.path.join(OUT, name), "w") as f:
        f.write(txt)


def png(svg_text, w, h):
    data = cairosvg.svg2png(bytestring=svg_text.encode(), output_width=w, output_height=h)
    return Image.open(io.BytesIO(data)).convert("RGBA")


def icon(S, k, bg=PAPER, fg=INK):
    """S px square icon on an opaque background, the mark centred at k px per grid unit
    (whole pixels, so every edge stays crisp)."""
    off = (S - 16 * k) / 2
    return svg_doc(S, S, f'<rect width="{S}" height="{S}" fill="{bg}"/>' + mark_at(off, off, 16 * k, fg))


# ---- banner art ------------------------------------------------------------
def iris_panel(x0, y0, w, h, msize, pitch, clear=()):
    """Black panel: a faint photosite lattice behind a large white mark. The lattice is
    the sensor; only the quad behind the aperture is lit."""
    out = [f'<rect x="{num(x0)}" y="{num(y0)}" width="{num(w)}" height="{num(h)}" fill="{INK}"/>']
    mx = x0 + (w - msize) / 2
    my = y0 + (h - msize) / 2
    u = msize / 16
    # lattice aligned to the mark's unit grid, faint photosite points
    dots = []
    i0 = math.floor((x0 - mx) / pitch) - 1
    j0 = math.floor((y0 - my) / pitch) - 1
    p = pitch
    for j in range(j0, j0 + int(h / p) + 3):
        for i in range(i0, i0 + int(w / p) + 3):
            cx, cy = mx + i * p, my + j * p
            if not (x0 <= cx - 1.5 and cx + 1.5 <= x0 + w and y0 <= cy - 1.5 and cy + 1.5 <= y0 + h):
                continue
            if mx - u <= cx <= mx + msize + u and my - u <= cy <= my + msize + u:
                continue  # clear zone around the mark
            if any(a <= cx <= c and b <= cy <= d for a, b, c, d in clear):
                continue
            dots.append(rect(cx - 1.5, cy - 1.5, 3, 3))
    out.append(f'<path fill="#3A3A3A" d="{"".join(dots)}"/>')
    out.append(mark_at(mx, my, msize, PAPER))
    return "".join(out)


def kicker(s, x, y, size=13, fill=STONE):
    return text(s.upper(), x, y, size, fill, MONO, 500, track=0.12)[0]


FACTS = [("01", "8.8–15× faster than LibRaw"),
         ("02", "Memory-safe Rust · WASM · GPU"),
         ("03", "~40 RAW formats · 1,600+ cameras")]
TITLE = "FlashRaw: the fast, safe camera RAW decoder"


def og_banner():
    W, H = 1200, 630
    L = 72
    PW = 400                                   # black panel width
    R = W - PW - 64                            # right edge of the text column
    lk, _, _ = lockup(INK, 46)
    b = [f'<rect width="{W}" height="{H}" fill="{PAPER}"/>',
         f'<g transform="translate({L} 64)">{lk}</g>',
         kicker("Open source · Rust · MIT / Apache-2.0", L, 200),
         runs([("The fast, safe", False)], L - 3, 276, 70, INK, 420, 72, -0.012)[0],
         runs([("camera ", False), ("RAW", True), (" decoder.", False)], L - 3, 350, 70, INK, 420, 72, -0.012)[0],
         f'<path fill="{INK}" d="{rect(L, 404, R - L, 1)}"/>']
    y = 452
    for n, s in FACTS:
        b.append(text(n, L, y, 14, STONE, MONO, 500)[0])
        b.append(text(s, L + 42, y + 1, 25, INK, SERIF, 420, 18)[0])
        y += 44
    b.append(iris_panel(W - PW, 0, PW, H, 208, 208 / 8, clear=[(W - PW + 80, H - 76, W - 80, H - 30)]))
    b.append(text("flashraw.ybouane.com", W - PW / 2, H - 46, 16, "#BDBDBD", MONO, 500, anchor="middle")[0])
    return svg_doc(W, H, "".join(b), TITLE)


def readme_banner():
    W, H = 1600, 500
    L = 88
    PW = 400
    R = W - PW - 72
    lk, _, _ = lockup(INK, 48)
    b = [f'<rect width="{W}" height="{H}" fill="{PAPER}"/>',
         f'<g transform="translate({L} 80)">{lk}</g>',
         runs([("The fast, safe camera ", False), ("RAW", True), (" decoder.", False)], L - 3, 262, 66, INK, 420, 72, -0.012)[0]]
    gutter = 32
    colw = (R - L - 2 * gutter) / 3
    for k, (n, s) in enumerate(FACTS):
        x = L + k * (colw + gutter)
        b.append(f'<path fill="{INK}" d="{rect(x, 324, colw, 1)}"/>')
        b.append(text(n, x, 356, 13, STONE, MONO, 500)[0])
        b.append(text(s, x, 394, 22, INK, SERIF, 420, 18)[0])
    b.append(iris_panel(W - PW, 0, PW, H, 176, 176 / 8))
    return svg_doc(W, H, "".join(b), TITLE)


def main():
    # marks
    write("logo-mark.svg", svg_doc(64, 64, mark(), vb="0 0 16 16"))
    write("logo-mark-mono.svg", svg_doc(64, 64, mark(mono=True), vb="0 0 16 16"))
    # favicon: the mark is already drawn in whole pixels at 16 px; follow the browser theme
    fav_style = ('<style>@media (prefers-color-scheme: dark){.fg{fill:#FFFFFF}}</style>')
    write("favicon.svg", svg_doc(16, 16, mark(cls=True), extra=fav_style))
    for fg, name in ((INK, "logo.svg"), (PAPER, "logo-dark.svg")):
        body, w, h = lockup(fg)
        write(name, svg_doc(math.ceil(w), h, body))

    og = og_banner(); write("og-banner.svg", og)
    png(og, 1200, 630).convert("RGB").save(os.path.join(OUT, "og-banner.png"), optimize=True)
    rb = readme_banner(); write("readme-banner.svg", rb)
    png(rb, 1600, 500).convert("RGB").save(os.path.join(OUT, "readme-banner.png"), optimize=True)

    # raster icons. Small sizes: the 16-unit mark rendered on whole pixels (16, 32, 48 are
    # exact multiples, so every edge is crisp).
    flat = svg_doc(16, 16, mark())
    png(flat, 32, 32).save(os.path.join(OUT, "favicon-32.png"), optimize=True)
    ico = [png(flat, s, s) for s in (16, 32, 48)]
    ico[2].save(os.path.join(OUT, "favicon.ico"), sizes=[(16, 16), (32, 32), (48, 48)], append_images=ico[:2])
    # manifest icons: the mark on a Paper tile, square corners (platforms mask as they like)
    png(icon(192, 9), 192, 192).save(os.path.join(OUT, "icon-192.png"), optimize=True)
    png(icon(512, 24), 512, 512).save(os.path.join(OUT, "icon-512.png"), optimize=True)
    # maskable: artwork inside the 80 % safe circle (the mark at half size: diagonal 0.71)
    png(icon(512, 16), 512, 512).convert("RGB").save(os.path.join(OUT, "icon-512-maskable.png"), optimize=True)
    # apple touch: opaque, full bleed (iOS rounds the corners itself)
    png(icon(180, 7), 180, 180).convert("RGB").save(os.path.join(OUT, "apple-touch-icon.png"), optimize=True)


if __name__ == "__main__":
    main()
