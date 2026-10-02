# Kodak (DCR, KDC, DCS TIFF, compact-camera dumps)

Module: `crates/flashraw-formats/src/kodak.rs`; codec: `crates/flashraw-codecs/src/kodak.rs`.

## Variants

| Files | Payload | Notes |
|---|---|---|
| DCR: DCS Pro 14N/14nx/SLR, DCS 7x0 | compression 65000, CFA | blocks of ≤256 samples: 4-bit lengths, then differences LSB-first in big-endian 16-bit words; per-block predictors; nibble > 12 = raw 12-bit block. Rows decode **in parallel** after a header-only scan locates every row (block byte sizes follow from the length nibbles). Values go through the Kodak IFD linearisation curve (tag 0x090D). |
| DCS 5x0/6x0 TIFF | lossless JPEG + curve | curve from TIFF tag 0x0123 |
| KDC DC120 | 8-bit rows rotated per row | non-square pixels (aspect stored) |
| KDC DC40/DC50 | RADC | adaptive Huffman, 4-row bands, output curve |
| C330 / C603 / "12MP" / KAI-0340 `.RAW` | header-less (size table) | CFA dumps: 8-bit through a stored or gamma curve; "YRGB" dumps: YCbCr → RGB (3 samples/pixel) |
| DCR colour (photometric 2/6, 65000) | Kodak RGB / YCbCr | decoded to 3-sample RGB; no corpus sample (experimental) |

Header-less dumps are identified by file size before the generic header-less table (dispatch
added in `lib.rs`); the C603 "KodakCommonInfo" header is a magic probe.

## Metadata

White balance comes from the Kodak IFD (tag 0x8290): DCR presets 0x0848..0x084D selected by
0x03FC (as `mul1²/mul0, mul1, mul1²/mul2`), "set in software" 0x03FD, KDC presets
match). White is the curve value at 12 bits when a curve is stored, 0xFF for DC120, 0x3FFF for
RADC. CFA: the raw IFD's TIFF-EP pattern, else the camera database, else GRBG.


## Unsupported

  itself rejects the corpus sample.
* Compression 262 (kodak 262 loader, early DCS Pro 14 firmware): no sample.
* DC20/DC25/DCS200 header-less dumps (4-colour 8-bit, size-identified): no sample.
