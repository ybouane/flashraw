# Fujifilm RAF

FlashRaw decodes every RAF generation from the 2000 FinePix S1 Pro era to current
X-Trans V and GFX bodies: bare 16-bit SuperCCD buffers, packed 12/14-bit layouts,
16-bit X-Trans/GFX payloads and Fujifilm's lossless and lossy compression. The full
sensor buffer of all 151 corpus RAF files is covered by regression hashes
(`tests/regression/fuji.tsv`); per-mode status is in `docs/support/fuji.tsv`.

Code: `crates/flashraw-formats/src/fuji.rs` (container, metadata, geometry) and
`crates/flashraw-codecs/src/fuji_compressed.rs` (compressed codec).

## Container

| Offset | Field |
|---|---|
| 0x00 | `FUJIFILMCCD-RAW ` + format version |
| 0x1C | camera model (32 bytes) |
| 0x3C | RAF version |
| 0x54 / 0x58 | embedded JPEG offset / length (EXIF + Fujifilm maker note inside) |
| 0x5C / 0x60 | meta directory offset / length |
| 0x64 / 0x68 | CFA section offset / length |
| 0x78 | second frame pointer (dual-exposure SuperCCD SR bodies) |

**Meta directory** (big-endian `u32 count`, then `u16 tag, u16 len, data`):
0x100 raw size (h, w), 0x110/0x111 vendor crop (top/left, h/w), 0x115 aspect, 0x121
image size, 0x130 SuperCCD layout (`fuji_layout = byte0 >> 7`, `fuji_width = !(byte1 & 8)`),
0x131 X-Trans pattern (36 bytes, stored reversed), 0x2FF0 as-shot WB (G,R,G,B; two
sets on some bodies, averaged), 0x4000 per-channel black (G,R,G,B), 0xC000 RAFData
(little-endian; its generation-dependent header precedes the recommended image size).

**CFA section**: since ~2010 a small little-endian TIFF whose IFD0 has a single 0xF000
entry pointing at the raw IFD: 0xF001/0xF002 full width/height, 0xF003 bits, 0xF006
packing flag, 0xF007/0xF008 payload offset/size (relative to the section), 0xF00A
black levels (2x2 or 6x6 pattern), 0xF00E as-shot WB (G,R,B). Older bodies store bare
16-bit samples in the byte order of the JPEG's EXIF TIFF (big-endian on the S2 Pro and
the DBP back, little-endian otherwise); on dual-frame bodies (F700/S20Pro) both frames
are interleaved row by row and the section length covers only one frame.

EXIF/maker note: Make/Model, orientation, exposure; maker note 0x104D (crop mode:
full, sports finder / 1.25x, GFX 35mm crop) drives the active area.

## Payload layouts

| Layout | Detection | Bodies |
|---|---|---|
| 16-bit words | `w*h*2 == size` (or no TIFF) | X-Trans/GFX uncompressed, SuperCCD, older Bayer |
| 12-bit LSB packed | 0xF006 = 0 | EXR bodies (F550–F900EXR, HS20–HS50EXR), X10, X-S1, XF1, X100, X-Pro1/E1/M1/A1/A2, X20/X30/XQ1/XQ2 |
| 12-bit MSB in 32-bit LE words | 0xF006 != 0 | HS10, S1, SL1000 |
| 14-bit MSB in 32-bit LE words | `w*h*7/4 == size` | X-A3/A5/A7/A10, X-T100/T200, XF10 |
| Fuji compressed | stream header `IS` | X-Trans III+ and GFX (lossless and lossy) |
| 8 vertical tiles, BE words | raw width 5504 | DBP for GX680 back |

Uncompressed rows are unpacked in parallel with the shared SIMD kernels.

## Fuji compressed

Stream: 16-byte header (lossless flag, X-Trans/Bayer, 12/14/16 bits, size, 768-column
block size, block count, line count), one big-endian size per block, for lossy files a
quantisation base per block and 6-row line, then the blocks.

Each 768-column vertical stripe is an independent bit stream, so FlashRaw decodes all
stripes concurrently, writing straight into the output buffer (`GridWriter`). Within
a stripe, every 6-row line is split into per-colour line buffers (3 R, 6 G, 3 B plus
two context lines each); samples are predicted from their neighbours (edge-directed
for even positions, local-extremum test for odd ones) and residuals use an adaptive
Rice code whose parameter comes from statistics selected by a quantised gradient
(41 contexts; lossy streams add three coarse flat-area tables and a per-line
quantiser `2*q+1`). X-Trans streams spend no bits on positions that are pure
interpolation. The decoder tolerates a
one-byte over-read at the end of a stripe (anything further is reported as corruption).

Hot-path notes: branch-free residual sign/zig-zag and edge prediction, fused read of
the unary prefix and the Rice remainder from one 32-bit window, precomputed
quantisation tables for every line quantiser present in a lossy file.

## Geometry and calibration

- **Buffer**: TIFF raw size, replaced by the compressed header's size; dual-frame
  bodies keep the first frame.
- **Active area**: RAFData recommended size centred (margins rounded
  to even), per-raw-width overrides for modern bodies (X-Trans 6336/6384/7872, GFX
  8472/9696/11808...) and crop modes, vendor crop records for older bodies, the 3328-wide
  EXR special case; X-Trans margins rounded up to multiples of 6, Bayer margins to 2.
- **Crop**: vendor crop (0x110/0x111) within the active area.
- **CFA**: X-Trans 6x6 from 0x131 anchored at the buffer origin; Bayer RGGB at the
  visible origin, except BGGR bodies (X10/X-S1/XF1, HS10, F900EXR/HS50EXR).
- **Black**: 0xF00A pattern (2x2 or 6x6) or meta 0x4000; the camera database's colour-table black
  as fallback (e.g. S2 Pro 128, S100FS 514, S200EXR 512).
- **White**: 0x3E00 for Bayer non-"X-" uncompressed/compressed bodies (14-bit GFX
  included), `2^bits - 1` otherwise, 16-bit GFX 65535, per-model overrides (HS10 3944,
  F770EXR 4073, S200EXR 16383).
- **WB**: 0xF00E, else 0x2FF0 (with a fixed correction for S2 Pro/F700/S5000/S7000/DBP).
- **Colour**: the camera database's colour table with model-prefix matching (the X-T20
  uses the X-T2 entry).
- **Orientation**: EXIF; S2 Pro and the DBP back are mounted rotated (90°).

## SuperCCD

Bodies with `fuji_width` set (S2/S3/S5 Pro, S5000–S9600, S6000fd/S6500fd, S100FS, E550,
E900, F700) have diagonally arranged photosites. FlashRaw returns the stored samples and describes them as
stored, without the 45° rotation a viewer needs for a rectangular image: the active band
holds the photosites that the rotation reads (`fuji_width` columns, or twice that
for layout 0), and the CFA is the true stored pattern, a 2x4 (layout 1) or 4x2 (layout 0) pattern of
green rows/columns alternating with red/blue ones, derived from the diagonal photosite
mapping (`SensorLayout::OtherCfa`). Keeping the stored geometry leaves every sample untouched;
the rotation interpolates, so it belongs in post-processing. The DBP for GX680 back carries the same flag but
is a plain BGGR Bayer back and is reported as such.

## Verified vs not

- Verified (regression-hashed): every corpus file (151): compressed lossless/lossy X-Trans and
  Bayer, 14/16-bit (60), 16-bit words (58), 12-bit packed both orders (25), 14-bit
  packed (6), DBP (1), F700 dual frame (1).
- Not in the corpus: 12-bit compressed streams (decoded by the same code path),
  second-frame extraction of dual-exposure bodies (FlashRaw returns the main frame), Fujifilm DNGs (handled by the DNG decoder).
- Malformed input: truncated/corrupted RAF files (synthetic and corpus) return errors,
  never panic (`fuji::tests`, codec tests, mutation runs over the corpus).

## Performance (16 cores)

Standalone `flashraw benchmark` medians:

| File | FlashRaw |
|---|---:|
| X-T3 lossless, 26.7 MP | 63 ms |
| X-E4 lossy, 26.7 MP | 72 ms |
| X-T5 lossless, 40.9 MP | 88 ms |
| GFX 50S lossless, 57.2 MP | 95 ms |
| GFX100S 16-bit lossless, 103 MP | 175 ms |

Per thread the compressed decoder needs ~0.6 s CPU for the 40 MP X-T5;
stripe parallelism (8–16 independent 768-column stripes per image) provides the rest.
Region decoding only decodes the intersecting stripes down to the last needed line
(a 512×512 crop of the X-T5 decodes in ~12 ms).
