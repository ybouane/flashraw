#!/usr/bin/env python3
"""Comparison charts (SVG) and their summary data (JSON) from the compare.py results.

    tools/bench/charts.py                     # defaults: benches/results/*-compare.jsonl -> docs/charts/

Reads benches/results/linux-x86_64-compare.jsonl and macos-arm64-compare.jsonl (written by
`tools/bench/compare.py merge`), the camera list benches/popular-cameras.tsv, and writes:

  docs/charts/decode-speed.svg   decode time of each library relative to FlashRaw (median)
  docs/charts/rgb-speed.svg      RAW -> RGB time relative to FlashRaw, incl. LibRaw + Little CMS
  docs/charts/memory.svg         peak memory relative to FlashRaw (median)
  docs/charts/cameras.svg        per-camera times, 12 popular bodies (Apple M1 Ultra)
  docs/charts/data.json          every number the charts show, plus per-camera values

Pure Python, no dependencies. Colours, fonts, line weights and corner radii are in THEMES
below (`--theme`). Text is SVG <text> with the theme's font stack (nothing embedded);
`--outline FONT.ttf` outlines it to paths with that font instead (needs fontTools and
uharfbuzz, see branding/tools/textpath.py). `--png DIR` also renders PNG previews (cairosvg).
"""
import argparse
import json
import math
import os
import statistics
import sys
import textwrap

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import compare  # noqa: E402  (Results, LIBS, gmean)

# ---------------------------------------------------------------- style (change it here)
# Every colour, font, line weight and corner radius of the charts; `--theme NAME` picks one.
# The default follows the brand (branding/README.md): monochrome ink on white, greys,
# hairlines, sharp corners, serif type with tabular figures. FlashRaw is drawn in ink, the
# other libraries in greys; marker shapes carry library identity in the dot plot, so no
# chart relies on colour.
SERIF = "Newsreader, Georgia, 'Times New Roman', serif"
THEMES = {
    "aperture": {
        "ink": "#0E0E0E", "text2": "#4A4A4A", "muted": "#7A7A7A", "line": "#E2E2E2", "band": "#F6F6F6",
        "card": "#FFFFFF", "fr": "#0E0E0E", "fr_light": "#6E6E6E", "other": "#A3A3A3", "other_light": "#D6D6D6",
        "baseline": "#0E0E0E", "hairline": 0.75,
        "series": {"flashraw": ("#0E0E0E", "circle"), "libraw": ("#8C8C8C", "square"),
                   "rawspeed": ("#B5B5B5", "diamond"), "rawler": ("#5C5C5C", "triangle")},
        "radius": 0, "card_radius": 0, "bold": 600, "regular": 400, "em": 0.47,
        "sans": SERIF, "mono": SERIF,
    },
    "indigo": {  # the first brand palette (Flash Indigo), sans-serif
        "ink": "#15142B", "text2": "#5A5D78", "muted": "#8C8FA8", "line": "#E4E2F3", "band": "#F5F4FD",
        "card": "#FFFFFF", "fr": "#4733E6", "fr_light": "#A89BFF", "other": "#5A5D78", "other_light": "#BDBFD3",
        "baseline": "#4733E6", "hairline": 1,
        # validated for colour-vision deficiency with the shapes as secondary encoding
        "series": {"flashraw": ("#4733E6", "circle"), "libraw": ("#E5484D", "square"),
                   "rawspeed": ("#C98500", "diamond"), "rawler": ("#0F9AA8", "triangle")},
        "radius": 4, "card_radius": 14, "bold": 700, "regular": 500, "em": 0.55,
        "sans": "'Plus Jakarta Sans', Inter, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
        "mono": "'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
    },
}


def use_theme(name):
    global INK, SLATE, MUTED, LINE, MIST, CARD, INDIGO, INDIGO_LIGHT, OTHER, OTHER_LIGHT, BASELINE, SERIES
    global RADIUS, CARD_RADIUS, FONTS, HAIR, THEME
    THEME = t = THEMES[name]
    INK, SLATE, MUTED, LINE, MIST, CARD = t["ink"], t["text2"], t["muted"], t["line"], t["band"], t["card"]
    INDIGO, INDIGO_LIGHT, OTHER, OTHER_LIGHT = t["fr"], t["fr_light"], t["other"], t["other_light"]
    BASELINE, SERIES, RADIUS, CARD_RADIUS = t["baseline"], t["series"], t["radius"], t["card_radius"]
    FONTS, HAIR = {"sans": t["sans"], "mono": t["mono"]}, t["hairline"]


use_theme("aperture")
# Machines: the first series is drawn dark, the second light.
MACHINES = [("linux", "Linux · Ryzen 7 9700X, 16 threads"), ("macos", "macOS · Apple M1 Ultra, 20 cores")]
W = 960                 # viewBox width; the README scales it to its column
PAD = 32

# ---------------------------------------------------------------- text


class Typesetter:
    """SVG <text> with the theme's font stack, or text outlined with one font file."""

    def __init__(self, outline_font=None):
        self.tp = None
        if outline_font:
            sys.path.insert(0, os.path.join(ROOT, "branding", "tools"))
            from textpath import text_path
            self.tp, self.font = text_path, outline_font

    @staticmethod
    def weight(w):
        return THEME["bold"] if w >= 650 else THEME["regular"] if w <= 500 else w

    def width(self, s, size, wght=500, font="sans"):
        if self.tp:
            return self.tp(s, self.font, size, 0, 0, self.weight(wght))[1]
        return len(s) * size * THEME["em"]

    def text(self, s, x, y, size, color, wght=500, font="sans", anchor="start", halo=False, halo_color=None):
        """halo: a card-coloured outline under the glyphs, so a label stays legible on gridlines."""
        s, wght = str(s), self.weight(wght)
        h = f'stroke="{halo_color or CARD}" stroke-width="6" stroke-linejoin="round"'
        if self.tp:
            w = self.width(s, size, wght, font)
            x0 = x - (w if anchor == "end" else w / 2 if anchor == "middle" else 0)
            d = self.tp(s, self.font, size, x0, y, wght)[0]
            return (f'<path d="{d}" fill="none" {h}/>' if halo else "") + f'<path d="{d}" fill="{color}"/>'
        fam = FONTS[font]
        t = (f'<text x="{x:.1f}" y="{y:.1f}" font-family="{fam}" font-size="{size}" font-weight="{wght}" '
             f'font-variant-numeric="tabular-nums lining-nums" '
             f'text-anchor="{anchor}" fill="{{}}"{{}}>{esc(s)}</text>')
        return (t.format("none", " " + h) if halo else "") + t.format(color, "")


def esc(s):
    return str(s).replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def svg_doc(h, body, title, desc):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {h:.0f}" width="{W}" height="{h:.0f}" '
            f'role="img" aria-labelledby="t d">\n<title id="t">{esc(title)}</title>\n<desc id="d">{esc(desc)}</desc>\n'
            f'<rect x="0.5" y="0.5" width="{W - 1}" height="{h - 1:.0f}" rx="{CARD_RADIUS}" fill="{CARD}" stroke="{LINE}" stroke-width="{HAIR}"/>\n'
            + "\n".join(body) + "\n</svg>\n")


def fmt_x(v):
    return "%.1f×" % v if v >= 9.95 else "%.2f×" % v


def fmt_ms(v):
    return "%.0f" % v if v >= 100 else "%.1f" % v if v >= 10 else "%.2f" % v


def nice_step(vmax, n=5):
    raw = vmax / n
    p = 10 ** math.floor(math.log10(raw))
    return next(m * p for m in (1, 2, 2.5, 5, 10) if m * p >= raw)


def header(T, title, subtitle, legend=True):
    """Title, subtitle and the machine legend; returns (svg parts, y below)."""
    b = [T.text(title, PAD, PAD + 22, 27, INK, 750),
         T.text(subtitle, PAD, PAD + 48, 15.5, SLATE, 500)]
    y = PAD + 74
    if legend:
        x = PAD
        for (key, lab), dark in zip(MACHINES, (True, False)):
            b.append(f'<rect x="{x}" y="{y - 10}" width="12" height="12" rx="{min(RADIUS, 3)}" fill="{OTHER if dark else OTHER_LIGHT}"/>')
            b.append(f'<rect x="{x + 14}" y="{y - 10}" width="12" height="12" rx="{min(RADIUS, 3)}" fill="{INDIGO if dark else INDIGO_LIGHT}"/>')
            b.append(T.text(lab, x + 34, y, 14.5, SLATE, 500))
            x += 34 + T.width(lab, 14.5) + 36
        y += 22
    return b, y


def bar_chart(T, title, subtitle, rows, footnotes, unit_label, baseline=1.0):
    """Horizontal paired bars. rows: [(label, sublabel, is_flashraw, [v_linux, v_macos])], or
    (heading,) for a group heading; a None value is drawn as 'n/a'. Values are ratios to
    FlashRaw (FlashRaw = baseline)."""
    body, y = header(T, title, subtitle)
    label_w = 250
    x0, x1 = PAD + label_w, W - PAD - 70
    vmax = max(v for r in rows if len(r) > 1 for v in r[3] if v is not None)
    step = nice_step(vmax * 1.04)
    top = step * math.ceil(vmax * 1.04 / step)
    sx = lambda v: x0 + (x1 - x0) * v / top  # noqa: E731
    bar_h, gap, row_pad = 15, 3, 20
    row_h = 2 * bar_h + gap + row_pad
    group_h = 34
    y_plot = y + 14 + (group_h - 14 if len(rows[0]) == 1 else 0)
    ys, yy = [], y_plot
    for r in rows:
        if len(r) == 1 and ys:
            yy += group_h - 14
        ys.append(yy)
        yy += row_h if len(r) > 1 else 14
    h_plot = yy - y_plot
    # grid + ticks
    t = 0.0
    while t <= top + 1e-9:
        gx = sx(t)
        body.append(f'<line x1="{gx:.1f}" y1="{y_plot - 6}" x2="{gx:.1f}" y2="{y_plot + h_plot - row_pad + 6}" '
                    f'stroke="{LINE}" stroke-width="{HAIR}"/>')
        body.append(T.text(("%g" % t) + "×", gx, y_plot + h_plot - row_pad + 24, 13.5, MUTED, 500, "mono", "middle"))
        t += step
    if baseline:
        bx = sx(baseline)
        body.append(f'<line x1="{bx:.1f}" y1="{y_plot - 6}" x2="{bx:.1f}" y2="{y_plot + h_plot - row_pad + 6}" '
                    f'stroke="{BASELINE}" stroke-width="{HAIR}" stroke-dasharray="3 3"/>')
    for row, ry in zip(rows, ys):
        if len(row) == 1:
            body.append(f'<rect x="{PAD}" y="{ry - group_h + 4}" width="{W - 2 * PAD}" height="{group_h - 10}" fill="{CARD}"/>')
            body.append(T.text(row[0], PAD, ry - 10, 14.5, INK, 750))
            body.append(f'<line x1="{PAD}" y1="{ry - 4}" x2="{W - PAD}" y2="{ry - 4}" stroke="{LINE}"/>')
            continue
        lab, sub, fr, vals = row
        if fr:
            body.append(f'<rect x="{PAD - 12}" y="{ry - 9}" width="{W - 2 * PAD + 24}" height="{row_h - row_pad + 18}" '
                        f'rx="{RADIUS * 2}" fill="{MIST}"/>')
        mid = ry + bar_h + gap / 2
        body.append(T.text(lab, x0 - 16, mid + (0 if sub else 5), 16.5, INDIGO if fr else INK, 750 if fr else 650,
                           anchor="end"))
        if sub:
            body.append(T.text(sub, x0 - 16, mid + 16, 13, SLATE, 500, anchor="end"))
        for j, v in enumerate(vals):
            by = ry + j * (bar_h + gap)
            color = (INDIGO if j == 0 else INDIGO_LIGHT) if fr else (OTHER if j == 0 else OTHER_LIGHT)
            if v is None:
                body.append(T.text("not measured" if fr else "n/a", x0 + 6, by + 12, 13, MUTED, 500, "mono"))
                continue
            wbar = max(3.0, sx(v) - x0)
            r = min(RADIUS, wbar / 2)
            # Rounded data end, square at the baseline.
            body.append(f'<path d="M{x0:.1f},{by:.1f} h{wbar - r:.1f} a{r},{r} 0 0 1 {r},{r} v{bar_h - 2 * r} a{r},{r} 0 0 1 {-r},{r} '
                        f'h{-(wbar - r):.1f} z" fill="{color}"/>')
            txt = ("1× baseline" if j == 0 else "1×") if fr and v == 1.0 else fmt_x(v)
            body.append(T.text(txt, x0 + wbar + 8, by + 12, 14.5, INK, 700 if fr else 600, "mono", halo=True,
                               halo_color=MIST if fr else None))
    y = y_plot + h_plot - row_pad + 34
    body.append(T.text(unit_label, x0, y + 6, 13, MUTED, 500))
    y += 16
    width = int((W - 2 * PAD) / (13 * THEME["em"] * 1.04))  # characters per line
    for fn in textwrap.wrap(" ".join(footnotes), width):
        y += 19
        body.append(T.text(fn, PAD, y, 13, MUTED, 500))
    return body, y + PAD - 6


def dot_panel(T, body, x0, x1, y0, rows, values, libs, title, ticks):
    """One panel of the per-camera dot plot on a log time axis."""
    lo, hi = math.log10(ticks[0]), math.log10(ticks[-1])
    sx = lambda v: x0 + (x1 - x0) * (math.log10(v) - lo) / (hi - lo)  # noqa: E731
    row_h = 30
    body.append(T.text(title, x0, y0 - 14, 15.5, INK, 700))
    for t in ticks:
        gx = sx(t)
        body.append(f'<line x1="{gx:.1f}" y1="{y0}" x2="{gx:.1f}" y2="{y0 + row_h * len(rows)}" stroke="{LINE}" stroke-width="{HAIR}"/>')
        body.append(T.text(("%g" % t), gx, y0 + row_h * len(rows) + 18, 13, MUTED, 500, "mono", "middle"))
    for i, cam in enumerate(rows):
        cy = y0 + i * row_h + row_h / 2
        vs = [(lib, values.get((cam, lib))) for lib in libs]
        pts = [sx(v) for _, v in vs if v]
        if pts:
            body.append(f'<line x1="{min(pts):.1f}" y1="{cy}" x2="{max(pts):.1f}" y2="{cy}" stroke="{LINE}" stroke-width="3" '
                        f'stroke-linecap="round"/>')
        for lib, v in sorted(vs, key=lambda lv: ("libraw", "rawspeed", "rawler", "flashraw").index(lv[0])):
            if v:
                dy = {"libraw": 4.5, "rawspeed": -4.5}.get(lib, 0)  # keep coinciding markers visible
                body.append(marker(SERIES[lib], sx(v), cy + dy, 6.5 if lib == "flashraw" else 5.5))
        fr = values.get((cam, "flashraw"))
        if fr:
            body.append(T.text(fmt_ms(fr), min(pts) - 11, cy + 4, 12.5, INDIGO, 700, "mono", "end", halo=True))


def marker(series, x, y, r):
    color, shape = series
    ring = f'stroke="{CARD}" stroke-width="2"'
    if shape == "circle":
        return f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r}" fill="{color}" {ring}/>'
    if shape == "square":
        s = r * 0.9
        return f'<rect x="{x - s:.1f}" y="{y - s:.1f}" width="{2 * s:.1f}" height="{2 * s:.1f}" rx="{min(RADIUS, 1.5)}" fill="{color}" {ring}/>'
    if shape == "diamond":
        s = r * 1.25
        return f'<path d="M{x:.1f},{y - s:.1f} L{x + s:.1f},{y:.1f} L{x:.1f},{y + s:.1f} L{x - s:.1f},{y:.1f} Z" fill="{color}" {ring}/>'
    s = r * 1.2
    return (f'<path d="M{x:.1f},{y - s:.1f} L{x + s * 0.95:.1f},{y + s * 0.7:.1f} L{x - s * 0.95:.1f},{y + s * 0.7:.1f} Z" '
            f'fill="{color}" {ring}/>')


# ---------------------------------------------------------------- data


CAMERAS = [  # (popular-cameras.tsv camera, note) of the per-camera chart
    ("Canon EOS R5", "RAW"), ("Canon EOS 5D Mark IV", "RAW"), ("Nikon Z8", "14-bit lossless"),
    ("Nikon Z6III", "lossless"), ("Sony A7 IV", "compressed"), ("Sony A7R V", "lossless"),
    ("Fujifilm X-T5", "lossless compressed"), ("Panasonic Lumix S5II", ""), ("OM System OM-1 Mark II", ""),
    ("Leica M10-R", ""), ("Hasselblad X2D 100C", "compressed"), ("Google Pixel 8 Pro", ""),
]


def summarize(R, files, th="all"):
    """{mode: {lib: {...}}}: median/geomean speedups, wins and median memory ratios."""
    out = {}
    for mode, libs in (("decode", ("libraw", "rawspeed", "rawler", "rawpy", "dcraw")),
                       ("rgb", ("libraw", "lcms", "rawler", "rawpy", "dcraw"))):
        out[mode] = {}
        for lib in libs:
            sp, mem = R.compare(files, lib, mode, th)
            if sp:
                out[mode][lib] = {
                    "files": len(sp), "median_speedup": round(statistics.median(sp), 3),
                    "geomean_speedup": round(compare.gmean(sp), 3), "flashraw_faster": sum(1 for s in sp if s > 1),
                    "median_memory_ratio": round(statistics.median(mem), 3) if mem else None,
                    "geomean_memory_ratio": round(compare.gmean(mem), 3) if mem else None,
                    "flashraw_less_memory": sum(1 for m in mem if m > 1),
                }
    if any(R.ms(f, "flashraw", "rgb-gpu", "all") for f in files):
        sp, mem = R.compare(files, "libraw", "rgb", "all", "rgb-gpu")
        cpu = [R.ms(f, "flashraw", "rgb", "all") / R.ms(f, "flashraw", "rgb-gpu", "all") for f in files
               if R.ms(f, "flashraw", "rgb", "all") and R.ms(f, "flashraw", "rgb-gpu", "all")]
        out["rgb-gpu"] = {"libraw": {"files": len(sp), "median_speedup": round(statistics.median(sp), 3),
                                     "geomean_speedup": round(compare.gmean(sp), 3),
                                     "median_memory_ratio": round(statistics.median(mem), 3) if mem else None},
                          "flashraw_cpu": {"files": len(cpu), "median_speedup": round(statistics.median(cpu), 3),
                                           "geomean_speedup": round(compare.gmean(cpu), 3)}}
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--linux", default=os.path.join(ROOT, "benches/results/linux-x86_64-compare.jsonl"))
    ap.add_argument("--macos", default=os.path.join(ROOT, "benches/results/macos-arm64-compare.jsonl"))
    ap.add_argument("--popular", default=os.path.join(ROOT, "benches/popular-cameras.tsv"))
    ap.add_argument("--out", default=os.path.join(ROOT, "docs/charts"))
    ap.add_argument("--theme", default="aperture", choices=list(THEMES))
    ap.add_argument("--outline", metavar="FONT.ttf", help="outline the text to paths with this font")
    ap.add_argument("--png", help="also write PNG previews (2x) into this directory (needs cairosvg)")
    a = ap.parse_args()
    use_theme(a.theme)
    T = Typesetter(a.outline)
    popular = compare.read_popular(a.popular)
    hosts = {"linux": compare.Results([a.linux]), "macos": compare.Results([a.macos])}
    pop = {h: [p[0] for p in popular if p[0] in R.meta] for h, R in hosts.items()}
    summ = {h: {"all": summarize(R, pop[h], "all"), "1": summarize(R, pop[h], "1")} for h, R in hosts.items()}
    n_pop = len(popular)
    S = lambda h, mode, lib, key="median_speedup": summ[h]["all"].get(mode, {}).get(lib, {}).get(key)  # noqa: E731
    files_note = lambda mode, lib: "/".join(str(S(h, mode, lib, "files") or 0) for h, _ in MACHINES)  # noqa: E731
    os.makedirs(a.out, exist_ok=True)
    charts = {}

    # Decode speed
    order = [("libraw", "LibRaw", "unpack()"), ("rawpy", "rawpy", "Python, LibRaw inside"),
             ("dcraw", "dcraw", "one thread only"), ("rawler", "rawler", "Rust, dnglab"),
             ("rawspeed", "RawSpeed", "darktable")]
    rows = [(name, sub, False, [S(h, "decode", lib) for h, _ in MACHINES]) for lib, name, sub in order]
    rows.sort(key=lambda r: -(r[3][0] or 0))
    rows.append(("FlashRaw", "CPU, all cores", True, [1.0, 1.0]))
    body, h = bar_chart(
        T, "RAW decode time, relative to FlashRaw",
        f"Time to decode the sensor data, relative to FlashRaw: median over the {n_pop} popular cameras (lower is better)",
        rows,
        ["Each library's time ÷ FlashRaw's on the same file, same machine, all cores, median of 7 runs; files the library "
         "does not decode are left out.",
         "Files compared (Linux/macOS): " + ", ".join(f"{n} {files_note('decode', l)}" for l, n, _ in order) + "."],
        "× FlashRaw's time")
    charts["decode-speed"] = svg_doc(h, body, "RAW decode time relative to FlashRaw",
                                     "; ".join(f"{r[0]}: Linux {r[3][0]}, macOS {r[3][1]} times FlashRaw's time" for r in rows))

    # RAW -> RGB speed
    order = [("lcms", "LibRaw + Little CMS", "linear ProPhoto → sRGB"), ("libraw", "LibRaw", "dcraw_process()"),
             ("rawpy", "rawpy", "postprocess()"), ("dcraw", "dcraw", "one thread only"),
             ("rawler", "rawler", "RawDevelop")]
    rows = [(name, sub, False, [S(h, "rgb", lib) for h, _ in MACHINES]) for lib, name, sub in order]
    rows.sort(key=lambda r: -(r[3][0] or 0))
    gpu = summ["macos"]["all"].get("rgb-gpu")
    if gpu:
        rows.append(("FlashRaw", "CPU, all cores", True, [1.0, 1.0]))
        g = 1 / gpu["flashraw_cpu"]["median_speedup"]
        rows.append(("FlashRaw GPU", "Metal (macOS)", True, [None, g]))
    else:
        rows.append(("FlashRaw", "CPU, all cores", True, [1.0, 1.0]))
    body, h = bar_chart(
        T, "RAW → sRGB image time, relative to FlashRaw",
        f"Time from RAW file to an 8-bit sRGB image, relative to FlashRaw: median over the {n_pop} popular cameras (lower is better)",
        rows,
        ["Same settings everywhere: bilinear demosaic (rawler: PPG), camera white balance, camera matrix, 8-bit sRGB. "
         "LibRaw + Little CMS: LibRaw develops",
         "to 16-bit linear ProPhoto RGB, Little CMS 2.19 converts it to sRGB on all cores. FlashRaw GPU: GPU develop, "
         "image downloaded to host memory."],
        "× FlashRaw's time (CPU)")
    charts["rgb-speed"] = svg_doc(h, body, "RAW to RGB time relative to FlashRaw",
                                  "; ".join(f"{r[0]}: Linux {r[3][0]}, macOS {r[3][1]} times FlashRaw's time" for r in rows))

    # Memory (RAW -> RGB and decode)
    subs = {"lcms": "linear ProPhoto → sRGB", "libraw": "dcraw_process()", "rawpy": "postprocess()",
            "dcraw": "one thread only", "rawler": "RawDevelop", "rawspeed": "darktable"}
    rows = []
    for lib, name in (("lcms", "LibRaw + Little CMS"), ("rawler", "rawler"), ("libraw", "LibRaw"), ("rawpy", "rawpy"),
                      ("dcraw", "dcraw")):
        rows.append((name, subs[lib], False, [S(h, "rgb", lib, "median_memory_ratio") for h, _ in MACHINES]))
    rows.sort(key=lambda r: -(r[3][0] or 0))
    drows = []
    for lib, name in (("rawler", "rawler"), ("dcraw", "dcraw"), ("rawspeed", "RawSpeed"), ("rawpy", "rawpy"),
                      ("libraw", "LibRaw")):
        dsub = {"rawler": "rawler::decode()", "dcraw": "one thread only", "rawspeed": "darktable",
                "rawpy": "raw_image", "libraw": "unpack()"}[lib]
        drows.append((name, dsub, False,
                      [S(h, "decode", lib, "median_memory_ratio") for h, _ in MACHINES]))
    drows.sort(key=lambda r: -(r[3][0] or 0))
    rows = ([("RAW → sRGB image",)] + rows + [("FlashRaw", "RAW → RGBA8", True, [1.0, 1.0])]
            + [("Decode only",)] + drows + [("FlashRaw", "raw u16", True, [1.0, 1.0])])
    body, h = bar_chart(
        T, "Peak memory, relative to FlashRaw",
        f"Peak memory (RSS) relative to FlashRaw: median over the {n_pop} popular cameras (lower is better)",
        rows,
        ["Peak resident memory of a fresh process decoding one file, input file included. rawpy includes the Python "
         "interpreter (~27 MB).",
         "Decoding alone needs about the same memory in every library (input + 16-bit image); FlashRaw's parallel "
         "decoders add per-thread buffers."],
        "× FlashRaw's peak memory")
    charts["memory"] = svg_doc(h, body, "Peak memory relative to FlashRaw",
                               "; ".join(f"{r[0]} ({r[1]}): Linux {r[3][0]}, macOS {r[3][1]} times FlashRaw's peak"
                                         for r in rows if len(r) > 1))

    # Per camera (macOS, the quiet machine)
    R = hosts["macos"]
    cams = []
    for c, n in CAMERAS:
        f = next((p[0] for p in popular if p[1] == c and (not n or p[2] == n)), None)
        if f in R.meta:
            cams.append((c, n, f))
    vals_d, vals_r, labels = {}, {}, []
    for c, n, f in cams:
        lab = c + (f" ({n})" if n else "")
        labels.append(lab)
        for lib in ("flashraw", "libraw", "rawspeed", "rawler"):
            vals_d[(lab, lib)] = R.ms(f, lib, "decode", "all")
            vals_r[(lab, lib)] = R.ms(f, lib, "rgb", "all") if lib != "rawspeed" else None
    body, y = header(T, "Per camera: milliseconds per photo",
                     "Apple M1 Ultra, all cores, median of 7 runs · log scale, left is faster", legend=False)
    lx = PAD
    for lib in ("flashraw", "libraw", "rawspeed", "rawler"):
        body.append(marker(SERIES[lib], lx + 6, y - 5, 6))
        lab = compare.LIBS[lib] + (" (no RGB pipeline)" if lib == "rawspeed" else "")
        body.append(T.text(lab, lx + 18, y, 14.5, SLATE, 600 if lib == "flashraw" else 500))
        lx += 18 + T.width(lab, 14.5) + 34
    y += 44
    lab_w = 260
    p0, p1 = PAD + lab_w, (W + PAD + lab_w) / 2 - 12
    q0, q1 = p1 + 60, W - PAD - 8
    for i, lab in enumerate(labels):
        cy = y + i * 30 + 15
        if i % 2 == 0:
            body.append(f'<rect x="{PAD - 12}" y="{cy - 15}" width="{W - 2 * PAD + 24}" height="30" rx="{RADIUS}" fill="{MIST}" opacity="0.6"/>')
        body.append(T.text(lab, p0 - 14, cy + 5, 14.5, INK, 600, anchor="end"))
    dot_panel(T, body, p0, p1, y, labels, vals_d, ["flashraw", "libraw", "rawspeed", "rawler"], "Decode (ms)",
              [1, 3, 10, 30, 100, 300, 1000])
    dot_panel(T, body, q0, q1, y, labels, vals_r, ["flashraw", "libraw", "rawler"], "RAW → RGB (ms)",
              [10, 30, 100, 300, 1000, 3000])
    yb = y + 30 * len(labels) + 46
    body.append(T.text("Missing marker: that library does not decode the file (RawSpeed: Canon CR3; rawler: Hasselblad X2D). "
                       "Full tables: docs/benchmarks.md.", PAD, yb, 13, MUTED, 500))
    desc = "; ".join(f"{lab}: decode " + ", ".join(f"{compare.LIBS[l]} {fmt_ms(vals_d[(lab, l)])} ms" for l in SERIES
                                                    if vals_d.get((lab, l))) for lab in labels)
    charts["cameras"] = svg_doc(yb + PAD - 8, body, "Per-camera decode and RAW to RGB times", desc)

    for name, svg in charts.items():
        with open(os.path.join(a.out, name + ".svg"), "w") as o:
            o.write(svg)
    if a.png:
        import cairosvg
        os.makedirs(a.png, exist_ok=True)
        for name, svg in charts.items():
            cairosvg.svg2png(bytestring=svg.encode(), write_to=os.path.join(a.png, name + ".png"), scale=2)

    # Data for the demo site.
    per_camera = []
    for f, cam, note in popular:
        row = {"file": f, "camera": cam, "note": note}
        for h, R in hosts.items():
            if f not in R.meta:
                continue
            m = R.meta[f]
            row.setdefault("format", m.get("format"))
            row.setdefault("compression", m.get("compression"))
            row.setdefault("megapixels", round(m.get("mp") or 0, 2))
            row[h] = {
                key: {lib: {"ms": round(R.ms(f, lib, mode, th), 3), "rss_mb": round(R.rss(f, lib, mode, th) or 0, 1)}
                      for lib in compare.LIBS if R.ms(f, lib, mode, th)}
                for key, mode, th in (("decode", "decode", "all"), ("rgb", "rgb", "all"), ("rgb_gpu", "rgb-gpu", "all"),
                                      ("decode_1_thread", "decode", "1"), ("rgb_1_thread", "rgb", "1"))}
        per_camera.append(row)
    data = {
        "generated_by": "tools/bench/charts.py",
        "method": "docs/benchmarks.md",
        "libraries": compare.LIBS,
        "machines": {"linux": "AMD Ryzen 7 9700X (8 cores / 16 threads), Linux", "macos": "Apple M1 Ultra (20 cores), macOS"},
        "popular_cameras": n_pop,
        "summary": summ,
        "per_camera": per_camera,
        "notes": {
            "speedup": "other library's time / FlashRaw's time on the same file (median over files both decode)",
            "memory_ratio": "other library's peak RSS / FlashRaw's peak RSS (fresh process per file)",
            "single_threaded": sorted(compare.SINGLE_THREADED),
        },
    }
    with open(os.path.join(a.out, "data.json"), "w") as o:
        json.dump(data, o, indent=1)
        o.write("\n")
    print(f"wrote {', '.join(charts)} and data.json to {a.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
