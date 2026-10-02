# Panasonic RW2 / Leica RWL

FlashRaw decodes Panasonic RawFormat 4–8 (every RW2/RWL in the corpus, 2008–2025
bodies) plus the early unversioned layouts. The full sensor buffer is bit-identical
by FlashRaw as well. Per-mode status: `docs/support/panasonic.tsv`.

Code: `crates/flashraw-formats/src/panasonic.rs` (container, metadata) and
`crates/flashraw-codecs/src/panasonic.rs` (all payload codecs).

## Container

TIFF variant with magic `IIU\0` (Leica RWL uses the same structure). IFD0 uses
Panasonic tag numbers:

| Tag | Meaning |
|---|---|
| 0x02/0x03 | raw width/height |
| 0x04–0x07 | sensor top/left/bottom/right border |
| 0x09 | CFA (1 RGGB, 2 GRBG, 3 GBRG, 4 BGGR at the buffer origin) |
| 0x0A | bits per sample |
| 0x0E–0x10 | linearity limits (white level for formats 6–8) |
| 0x11/0x12 | old red/blue balance (×256) |
| 0x17/0x37 | ISO |
| 0x1C–0x1E | black level R/G/B |
| 0x24–0x26 | as-shot WB R/G/B |
| 0x2D | RawFormat (payload version) |
| 0x2E | embedded full-size JPEG |
| 0x39–0x48 | format-8 tone curve, initial prediction, Huffman table, strip table |
| 0x112 | orientation |
| 0x118 | payload offset (0x111 on early files) |

The EXIF IFD (0x8769) supplies exposure data.

## Payload formats

| RawFormat | Bodies | Coding | FlashRaw parallel unit |
|---|---|---|---|
| 4 | most bodies up to 2019 | 14 pixels per 16-byte packet: 2-bit shift selectors, 8-bit deltas, 4-bit refinements | packet (rows) |
| 5 | GH5S, G9 high-res | 10×12-bit / 9×14-bit LSB packing per 16 bytes | rows |
| 6 | S1/S1R/S1H/S5, GH5 II | 11×14-bit / 14×12-bit delta-coded pixels per 16 bytes | rows |
| 7 | S1R/S5 high-res | 9×14-bit LSB packing per 16 bytes | rows |
| 8 | GH6, GH7, G9 II, S5 II/IIX, S9, S1 II... | Huffman-coded differences of 2×2 blocks | strips/tiles |

**Blocked stream (formats 4, 5).** The stream is stored in 0x4000-byte blocks whose
a short final block (unread bytes keep the previous block's contents).

(`pana_data`), but each 16-byte packet is consumed as a 128-bit little-endian integer
from its top bit down and always carries exactly 128 bits for its 14 pixels (four
2-bit shifts + 14×8 delta bits + one 4-bit refinement per parity). So every packet —
and every row, since widths are multiples of 14 — decodes independently. A sequential

whole 16-row bands are decoded and columns that do not fill a packet stay zero.

**Format 8.** The image is split into strips (tags 0x42 columns × 0x43 rows; 0x44
offsets, 0x45 x | y<<16 positions, 0x46 sizes in bits, 0x47/0x48 sizes). Each strip
is decoded independently and in parallel, straight into its band of the output: the
bit stream is LSB-first per byte, codes come from tag 0x40 (matched in table order;
the symbol index is the difference bit length, JPEG-style sign extension), and each
2×2 block is predicted from the previous block (first block of a row pair from the
previous row pair's first block), clipped to tag 0x3B and passed through the 0x39/0x3A
tone curve when it is not the identity (it is in every known file). The rarely used
of at most five strips; FlashRaw also decodes the 2-D strip grids of pixel-shift
high-resolution shots (seams verified continuous).


  `(0, 0, left + right border, bottom border + 12)` clipped to the buffer, adjusted by
  margins rounded to even). This is generous; the precise image area is the sensor
  border rectangle, which FlashRaw reports as `crop`.
- **Black**: formats 6–8 use tags 0x1C–0x1E as is; formats 4/5 add 15 (format 4) when
  bodies, 2048 for GH6-class 16-bit files).
- **White**: formats 6–8: maximum linearity limit, with 16383 → 16319 and 4095 → 4079
- **WB**: 0x24–0x26, else 0x11/0x12 with green 256.
  prefix matching), then the camera database.
  `3:2`, `16:9`, `1:1`) for camera-database lookups; also exposed as metadata `aspect`.
- **Previews**: tag 0x2E JPEG plus TIFF thumbnails.

## Verified vs not

  (8), 7 (3), 8 (35, 12/14/16-bit, 2–3 strips).
  by seam continuity only.
- Not in the corpus: unversioned early files (0x111 payloads: packed format-4 stream
  with no block split or 16-bit words), 12-bit format 7, non-identity format-8 tone
  on real files.
- Malformed input: truncated/corrupted RW2 files (synthetic and corpus) return errors,
  never panic (`panasonic::tests`, codec tests, mutation runs over the corpus).



| File | FlashRaw | reference | Speed-up |
|---|---:|---:|---:|
| D-LUX (Typ 109) 4:3, format 4, 12.9 MP | 4.3 ms | 153 ms | 36x |
| DC-S1R, format 6, 47.6 MP | 12.8 ms | 319 ms | 25x |
| DC-GH6 4:3, format 8, 25.2 MP | 74 ms | 256 ms | 3.5x |
| DC-G9M2 high-res, format 8, 100 MP | 194 ms | 1226 ms | 6.3x |

6 10 vs 89 ms, 7 8 vs 38 ms, 8 129 vs 405 ms.

Formats 4–7 are row-parallel and memory bound. Format 8 is limited by its strip count
(2–3 strips per image on most bodies; each strip is one sequential bit stream with a
row-to-row prediction chain); per thread it decodes ~6 ns/pixel (L1-sized Huffman
table, one bit-buffer refill per sample).
