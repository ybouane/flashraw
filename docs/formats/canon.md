# Canon raw formats

FlashRaw decodes every Canon raw container: CRW (CIFF, 1998–2004), the TIFF raws of
the EOS-1D/1Ds/D2000, CR2 (2004–2018, incl. sRAW/mRAW), and CR3 (ISO BMFF + CRX,
2018–). The whole sensor buffer of every Canon corpus file is covered by regression hashes
(see `tests/regression/canon.tsv`, and `docs/support/canon.tsv` for the per-mode status).

Code: `crates/flashraw-formats/src/canon/` (`cr2.rs`, `cr3/`, `crw.rs`, shared
`makernote.rs` and `geometry.rs`) and codecs `crates/flashraw-codecs/src/canon_{cr2,sraw,crx,crw}`.

## Shared Canon knowledge

- **Maker note** (`makernote.rs`): ModelID (0x10), firmware (0x07), CameraSettings,
  ShotInfo white-balance index, SensorInfo (0xE0: sensor size, default crop, left
  optical black), ColorData (0x4001; layouts v1–v12 selected by tag length — note
  PowerShots store it as UNDEFINED bytes, so the *byte* count selects the layout — and,
  for the 3778-word block, by sub-version: 64 and later (R6 Mark III, R50 V, PowerShot
  V1) use the v12 offsets):
  as-shot WB, sRAW WB, per-channel black, normal/specular white. Also the EOS-1D
  (0x00A4) and PowerShot G9 (0x0029) WB tables and the multi-shot flag (0x4021, already
  white-balanced composites → unity WB).
- **Sensor geometry** (`geometry.rs`): Canon files do not describe their optical-black
  borders, so margins come from a table keyed by raw dimensions. Sensors missing from it use the maker-note SensorInfo crop and per-channel
  black when the sensor size equals the raw size.
- **Black level**: measured after decoding from the left/right optical-black column
  bands (2-pixel guard bands, rows of the visible area) per CFA position
  (`black_from_masked`); otherwise the maker-note
  per-channel black.
- **Odd margins** are rounded up to even coordinates (width/height shrink by one), and
  the CFA is anchored relative to the original visible origin.

## CR2

The raw is a single lossless-JPEG stream (usually IFD3) cut into vertical slices
(tag 0xC640 `[count, width, last_width]`). A JPEG frame of `W` columns and `C`
components covers `W*C` sensor columns; frames more than 4x wider than tall carry two
sensor rows per JPEG row (5D Mark IV). `canon_cr2` streams decoded JPEG rows directly
into their slice position (no intermediate frame). Quirks: the EOS-1D Mark III
(3984-wide) stream is offset by two samples; dual-pixel files contain a second raw
stream (the first, largest one is decoded).

**sRAW/mRAW** frames are subsampled YCbCr (4:2:2 / 4:2:0). FlashRaw outputs linear
camera RGB with `cpp = 3` (layout `Linear`, white 16383): chroma is upsampled with
Canon's rounding, converted with the per-generation formula (matrix form for 5D
Mark II/7D/50D/1D Mark IV/60D with a firmware-dependent hue offset, simple form
otherwise, with the 512 luma offset for pre-5D II bodies) and scaled by the sRAW WB
from ColorData. Geometry handles the sRAW quirks (portrait-stored frames, 5DS sRAW1,
SensorInfo-based layouts for frames with implausible aspect ratios).

**TIFF raws**: EOS-1D/1Ds point at the stream through maker note 0x0081 (GRBG CFA).
The Kodak-built EOS D2000C has a linearisation curve (tag 0x0123), TIFF-EP CFA and
Kodak-IFD (0x8290) black/white balance.

White level is the per-model value from the camera database, else the bit depth.

## CR3

ISO BMFF (`ftyp crx `). Metadata: moov/uuid 85c0b687… with CMT1 (IFD0), CMT2 (EXIF),
CMT3 (maker note), CMT4 (GPS), THMB (thumbnail); traks with CRAW sample entries
(CMP1 header), JPEG/HEVC previews and CTMD timed metadata (ColorData lives in the
CTMD maker notes, not in CMT3); PRVW preview in uuid eaf42b5e…. The decoded track is
the first image of dual-pixel files and the first frame of bursts.

CRX splits the sensor into 4 CFA planes and a tile grid; every (tile, plane) is coded
independently and decoded in parallel. Lossless: line-predictive median predictor
with adaptive Golomb–Rice codes and run mode. C-RAW: integer 5/3 lifting wavelet
(3 levels, tile-edge overlap), with per-line QP update (CMP1 0x100) or per-tile QP
maps (0x200). White = (1<<nBits)-1. Encoding type 3 and the rounded near-lossless
mode are implemented but have no samples.

## CRW

CIFF heap (sub-heaps 0x28xx/0x30xx), raw in record 0x2005. Canon's block Huffman
codec: 64-sample blocks, three table sets chosen by record 0x1835, even/odd column
predictors reset to 512 per row; later models store the two low bits separately
(12-bit data). PowerShot A5/A50/Pro70 store packed 10-bit CMYG data; the PowerShot 600
has its own packing. WB from ColorInfo2 (0x102C), ColorBalance (0x10A9) or the
ColorInfo1 tables (0x0032) indexed by the shot-info WB setting; previews from
0x2007/0x2008.

## Not supported

- Headerless CHDK dumps (`.CRW`/`.CR2` without a header, identified only by file
  size) are not handled by the Canon decoders; they go through the generic file-size path
  (see `misc.md`).
