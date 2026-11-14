"""Outline text to SVG path data: harfbuzz shaping (kerning) + fontTools glyph outlines."""
import io, functools
import uharfbuzz as hb
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.pens.boundsPen import BoundsPen


@functools.lru_cache(None)
def load(path, axes):
    f = TTFont(path)
    if "fvar" in f:
        f = instancer.instantiateVariableFont(f, dict(axes))
    buf = io.BytesIO(); f.save(buf)
    data = buf.getvalue()
    return TTFont(io.BytesIO(data)), data


def fmt(v):
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


class RoundPen(SVGPathPen):
    def __init__(self, gs):
        super().__init__(gs, ntos=fmt)


KERN = {('t', ','): -0.07, ('t', '.'): -0.05, ('r', '.'): -0.04, ('r', ','): -0.04}


def text_path(text, font_path, size, x=0.0, y=0.0, wght=400, tracking=0.0, features=None, extra_axes=(), kern=KERN):
    """Return (path_d, advance_width, (xmin, ymin, xmax, ymax)) with baseline at y.
    tracking in em units (e.g. -0.02)."""
    axes = tuple(sorted({"wght": wght, **dict(extra_axes)}.items()))
    tt, data = load(font_path, axes)
    upem = tt["head"].unitsPerEm
    face = hb.Face(data); font = hb.Font(face)
    buf = hb.Buffer(); buf.add_str(text); buf.guess_segment_properties()
    hb.shape(font, buf, features or {"kern": True, "liga": True})
    gs = tt.getGlyphSet(); order = tt.getGlyphOrder()
    scale = size / upem
    pen = RoundPen(gs); bpen = BoundsPen(gs)
    cx = 0.0
    for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
        if info.codepoint == 0:
            raise ValueError(f"missing glyph in {font_path} for text {text!r} at cluster {info.cluster}")
        name = order[info.codepoint]
        t = (scale, 0, 0, -scale, x + (cx + pos.x_offset) * scale, y - pos.y_offset * scale)
        gs[name].draw(TransformPen(pen, t))
        gs[name].draw(TransformPen(bpen, t))
        cx += pos.x_advance + tracking * upem
        c = info.cluster
        if c + 1 < len(text):
            cx += kern.get((text[c], text[c + 1]), 0.0) * upem
    adv = (cx - tracking * upem) * scale
    return pen.getCommands(), adv, bpen.bounds


def metrics(font_path, wght=400):
    tt, _ = load(font_path, (("wght", wght),))
    os2 = tt["OS/2"]
    return dict(upem=tt["head"].unitsPerEm, cap=os2.sCapHeight, xh=os2.sxHeight)
