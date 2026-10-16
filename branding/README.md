# FlashRaw brand

<img src="logo.svg" alt="FlashRaw" height="64">

## The mark

A rounded-square tile split into a 2×2 checker, the RGGB quad of a Bayer colour filter
array: the two lighter cells sit on the green diagonal, since green is the bright channel.
An amber lightning bolt, the flash, crosses the quad, with its horizontal jog on the
quad's centre line. Sensor data, decoded fast.

The banners carry the same idea at a larger scale: a field of red, green and blue
photosites, with the bolt lit up in amber where it covers them, as a sensor would sample it.

## Colours

| Name | Hex | Use |
|---|---|---|
| Flash Indigo | `#4733E6` | Primary. Links, buttons, the "Raw" half of the wordmark, flat mark tile |
| Indigo Deep | `#3A26CF` | Mark: darker (R, B) photosites |
| Indigo Bright | `#5A47F2` | Mark: lighter (G) photosites |
| Indigo on dark | `#A89BFF` | "Raw" in the wordmark and indigo text on dark backgrounds |
| Flash Amber | `#FFB224` | The bolt. Highlights; never for text on white |
| Ink | `#15142B` | Headings, body text, "Flash" in the wordmark, monochrome mark |
| Slate | `#5A5D78` | Secondary text |
| Mist | `#F5F4FD` | Tinted panels and section backgrounds |
| Line | `#E4E2F3` | Borders and dividers |
| Sensor Red | `#F0505A` | Accent |
| Sensor Green | `#1DB07A` | Accent |
| Sensor Blue | `#2F7BF2` | Accent |

Contrast: Flash Indigo on white 7.3:1, Ink on white 18:1, Slate on white 6.4:1,
Indigo on dark on `#0F1020` 7.9:1 (all pass WCAG AA for body text).

Use the three sensor colours sparingly, and always as a set: small bullets, chart series,
the photosite mosaic. They are not text colours.

## Typography

| Role | Font | Fallbacks |
|---|---|---|
| Wordmark, headings, UI | [Plus Jakarta Sans](https://fonts.google.com/specimen/Plus+Jakarta+Sans) (SIL OFL 1.1) | `Inter, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif` |
| Code, numbers, URLs | [JetBrains Mono](https://fonts.google.com/specimen/JetBrains+Mono) (SIL OFL 1.1) | `ui-monospace, SFMono-Regular, Menlo, Consolas, monospace` |

The wordmark is Plus Jakarta Sans at weight 750 with −1.5 % tracking: "Flash" in Ink,
"Raw" in Flash Indigo (white and Indigo on dark on dark backgrounds). Headings use
weight 700 to 800 with slight negative tracking; body text uses 400 to 500.

Every SVG here has its text outlined to paths, so none of them needs a font installed.

## Usage

- Clear space around the logo: at least half the mark's height on every side.
- Minimum size: the horizontal logo 24 px tall; the mark alone 16 px (use `favicon.svg`
  below 32 px).
- Use `logo.svg` on white or light backgrounds and `logo-dark.svg` on dark ones. On
  photos or busy backgrounds, use the mark alone.
- One colour (print, embossing, watermarks): `logo-mark-mono.svg`, Ink with the bolt
  knocked out; recolour the single fill as needed (white on dark).
- Don't recolour, rotate, skew, outline or add shadows to the mark, don't rearrange the
  checker or the bolt, and don't set "FlashRaw" in another font next to the mark.
- Write the name as **FlashRaw**: one word, capital F and R.

## Files

| File | What |
|---|---|
| `logo.svg` | Mark + wordmark, horizontal, for light backgrounds |
| `logo-dark.svg` | Same, for dark backgrounds |
| `logo-mark.svg` | The mark alone, square, 16 to 512 px and up |
| `logo-mark-mono.svg` | One-colour mark (bolt knocked out) |
| `favicon.svg` | The mark tuned for 16 to 32 px: flat tile, fuller bolt on the pixel grid |
| `favicon.ico` | 16, 32 and 48 px |
| `favicon-32.png` | 32 × 32 |
| `apple-touch-icon.png` | 180 × 180, opaque, full bleed (iOS rounds the corners) |
| `icon-192.png`, `icon-512.png` | Web app manifest icons (rounded tile, transparent corners) |
| `icon-512-maskable.png` | Maskable manifest icon, artwork inside the 80 % safe zone |
| `og-banner.png` / `.svg` | 1200 × 630 social and Open Graph image |
| `readme-banner.png` / `.svg` | 1600 × 500 banner for the top of the README |
| `tools/build.py` | Regenerates all of the above |

HTML head:

```html
<link rel="icon" href="/favicon.ico" sizes="48x48">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/apple-touch-icon.png">
<meta property="og:image" content="https://flashraw.ybouane.com/og-banner.png">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta name="twitter:card" content="summary_large_image">
<meta name="theme-color" content="#4733E6">
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
