# FlashRaw brand

<img src="logo.svg" alt="FlashRaw" height="64">

## The mark: Aperture

Four blades on a 3 × 3 grid close like a square iris. The light that passes through the
opening lands on one RGGB quad of a Bayer colour filter array: the only colour in the
identity. Everything else is black on white, or white on black.

The mark is drawn on a 16-unit grid (one unit is one pixel at 16 px): blades 4 units thick,
2-unit gaps, a 4 × 4 quad in the centre. Every edge falls on a whole pixel at 16, 32, 48 px
and their multiples, so the favicon needs no separate drawing.

The wordmark is **Flash** in Newsreader and ***Raw*** in Newsreader Italic, weight 540,
optical size 60, −0.6 % tracking. Its cap height is 58 % of the mark, centred on it; the gap
between mark and wordmark is 34 % of the mark.

## Colours

| Name | Hex | Use |
|---|---|---|
| Ink | `#0E0E0E` | Type, the mark, rules that matter, dark backgrounds |
| Paper | `#FFFFFF` | Page background, type and mark on dark |
| Bone | `#F6F4EF` | Warm off-white for quiet panels (code, the image stage) |
| Graphite | `#3A3A3A` | Secondary text |
| Stone | `#6B6B6B` | Muted text, captions, labels |
| Ash | `#A3A3A3` | Non-text grey: chart series, rules on dark |
| Hairline | `#E2E0DA` | Rules and borders on light backgrounds |
| Sensor Red | `#D9473B` | The quad; tiny accents |
| Sensor Green | `#2E9A5C` | The quad; tiny accents |
| Sensor Blue | `#2F63D6` | The quad; tiny accents |

Contrast on Paper: Ink 19.3:1, Graphite 11.4:1, Stone 5.3:1 (all pass WCAG AA for body
text; Stone also passes on Bone, 4.85:1). Ash is for graphics only (2.5:1).

The three sensor tints appear inside the quad and, at most, as small accents that mean
"channel": a histogram, a CFA diagram, a white-balance bar. They are never text colours,
never backgrounds, never decoration.

## Typography

| Role | Font | Fallbacks |
|---|---|---|
| Wordmark, headlines, body | [Newsreader](https://fonts.google.com/specimen/Newsreader) and Newsreader Italic, variable (opsz 6–72, wght 200–800), SIL OFL 1.1 | `Georgia, 'Times New Roman', serif` |
| Labels, numbers, code, URLs | [JetBrains Mono](https://fonts.google.com/specimen/JetBrains+Mono), variable, SIL OFL 1.1 | `ui-monospace, SFMono-Regular, Menlo, Consolas, monospace` |

- Headlines: Newsreader at a display optical size, weight 400 to 450, tracking −1 % to −2 %.
  Italic is for the one word that carries the meaning ("RAW", "Raw"), not for emphasis
  in general.
- Body: Newsreader at text sizes (17 to 19 px on screen), weight 400, line height 1.5 to 1.6.
- Labels: JetBrains Mono, 11 to 13 px, upper case, +8 to +12 % tracking, in Stone.
  Numbers in tables and timings: JetBrains Mono, tabular.

Every SVG here has its text outlined to paths, so none of them needs a font installed.

## Usage

- Black on white, white on black. No gradients, shadows or tinted backgrounds.
- Square corners on the mark and on everything built around it (tiles, buttons, panels:
  0 to 2 px radius).
- Clear space around the logo: at least half the mark's height on every side.
- Minimum size: the horizontal logo 24 px tall; the mark alone 16 px.
- Use `logo.svg` on white or light backgrounds and `logo-dark.svg` on dark ones. On photos
  or busy backgrounds, use the mark alone on a Paper or Ink tile.
- One colour (print, embossing, watermarks): `logo-mark-mono.svg`, the quad filled in the
  same colour as the blades; recolour the single fill as needed.
- Don't recolour the blades, rotate or mirror the mark (the blades turn clockwise), round
  its corners, change the quad's order (R G / G B), or set "FlashRaw" in another font next
  to it.
- Write the name as **FlashRaw**: one word, capital F and R.

## Files

| File | What |
|---|---|
| `logo.svg` | Mark + wordmark, horizontal, for light backgrounds |
| `logo-dark.svg` | Same, for dark backgrounds |
| `logo-mark.svg` | The mark alone, square, 16 px and up |
| `logo-mark-mono.svg` | One-colour mark |
| `favicon.svg` | The mark at 16 units; switches to white blades in dark browser themes |
| `favicon.ico` | 16, 32 and 48 px |
| `favicon-32.png` | 32 × 32 |
| `apple-touch-icon.png` | 180 × 180, opaque Paper, mark at 7 px per unit (iOS rounds the corners) |
| `icon-192.png`, `icon-512.png` | Web app manifest icons (mark on a Paper tile) |
| `icon-512-maskable.png` | Maskable manifest icon, mark inside the 80 % safe zone |
| `og-banner.png` / `.svg` | 1200 × 630 social and Open Graph image |
| `readme-banner.png` / `.svg` | 1600 × 500 banner for the top of the README |
| `tools/build.py` | Regenerates all of the above |
| `options/`, `tools/options.py` | The four logo directions that were reviewed (history) |

HTML head:

```html
<link rel="icon" href="/favicon.ico" sizes="48x48">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/apple-touch-icon.png">
<meta property="og:image" content="https://flashraw.ybouane.com/og-banner.png">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta name="twitter:card" content="summary_large_image">
<meta name="theme-color" content="#FFFFFF">
```

Manifest icons:

```json
"icons": [
  { "src": "/icon-192.png", "sizes": "192x192", "type": "image/png" },
  { "src": "/icon-512.png", "sizes": "512x512", "type": "image/png" },
  { "src": "/icon-512-maskable.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable" }
]
```

## Rebuilding

The SVGs are generated, not hand-edited: change `tools/build.py` and rerun it.

```sh
pip install pillow fonttools uharfbuzz cairosvg
python branding/tools/build.py --fetch-fonts   # once; fonts go to tools/fonts/ (ignored)
python branding/tools/build.py
```

The figures on the banners come from the headline table in the top-level README; update
both together.
