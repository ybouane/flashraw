# Long-tail formats (`misc`)

Module: `crates/flashraw-formats/src/misc/` — `naked.rs` (file-size identified dumps), `rpi.rs`
(Raspberry Pi), `arri.rs` (ARRI), `tiff_ep.rs` (TIFF/EP fallback for unknown makes).
Codecs: `crates/flashraw-codecs/src/misc_packed.rs`, `crates/flashraw-codecs/src/vc5.rs` (GoPro).

Dispatch (`flashraw-formats/src/lib.rs`): `misc::probe`/`open` for magic-identified files (ARRI,
Light LRI), `misc::open_tiff` for TIFFs whose make has no module, `misc::open_headerless` for files
without a TIFF header (Raspberry Pi JPEG+BRCM first, then the file-size table).

## Headerless ("naked") dumps — `naked.rs`

Identified only by exact file size, after every signature-based format declined the file. Two kinds
of camera-database entries carry a `filesize` hint:

1. **File-size table** — `camera-db/cameras.json`, `mode = "naked-<size>"` (138 sizes; the
   Nikon/Olympus "diag raw" dumps with CMYG-type layouts are omitted). Hints:
   `full_width`/`full_height` (stored raster), `offset` (or `reversed` for bottom-up rows),
   `load_flags` (packing flags), `max_shift` (white = 2^bits − 2^max_shift), `flip`, `zero_is_bad`,
   `monochrome`, `right_mask_trim`. The bit depth is computed from the size and picks the loader:

   | bits | loader |
   |---|---|
   | 6 | Minolta RD-175 three-CCD interleave (1534x986 GRBG) |
   | 8 | bytes through a tone curve (identity, Kodak's curve, or the fixed `gamma_curve(0, 3.875, 255)`) |
   | 10 | Android "loose" (6 samples / 64-bit word), MIPI "tight" (load flag 1), else generic packed |
   | 12 | generic packed: MSB-first in 8/16/24/32-bit LE words (`flags & 24`), even row padding, pad byte per 10 samples (`& 1`), pair swap (`& 64`), interlaced fields (`& 2`, `& 4`) |
   | 16 | 16-bit words, big-endian if `flags & 1`, right shift `(flags >> 1) & 7` |

   Geometry and level rules: odd Bayer margins are moved to the next even position; the CFA of a
   table entry is anchored at the (unadjusted) visible origin and re-anchored at the buffer origin;
   CHDK dumps (`flags & 32`) and 8-bit dumps measure black from the left/right optical-black columns
   (SX220 HS: last 4 columns excluded); the PowerShot A610 size is renamed S2 IS when a data test on
   the file says so; Samsung WB550 words are little-endian; Kodak C330/C603/"12MP" YCbCr dumps decode
   to a 3-sample RGB image (cpp 3).
2. **CHDK entries** (`mode = "chdk*"`, all vendor files): `full_width`, `full_height`,
   `offset`, `bits`, `order` (`plain` = LSB, `jpeg` = MSB, `jpeg16`/default = MSB in 16-bit words,
   `jpeg32`), crop/black/white/CFA from the entry. Used when no file-size table entry has the size.

Additional curated entries (in `generic.toml`): Xiro Xplorer V (LSB-packed 10-bit) and
PowerShot G7 X CHDK (experimental).

The generic packed decoder has a parallel fast path (rows starting on word boundaries, unpacked with
the SIMD kernels in `packed.rs`) and a bit-exact sequential reference used for the other flag
combinations; tests check both agree.

## Raspberry Pi — `rpi.rs`

`raspistill --raw` / picamera captures: JPEG + `BRCM` block at a mode-specific distance from the end
of the file (32 KiB header, then rows padded to 32 bytes). Header at `BRCM+0xB0`: width, height,
right padding, Bayer order, packing (8/10/12/14/16 bits). Names and black levels:
`RPi IMX219` (66), `RPi OV5647 v.1/v.2` (16), `RP_imx477` (256, 64 in 10-bit mode). White balance
from the maker-note text (`gain_r=…`, `gain_b=…`). OV5647 rows are decoded 2611 samples
wide, reading past the stride into the start of the row.

## ARRI — `arri.rs`

`ARRI\x12\x34\x56\x78`, payload offset (8), width (20), height (24), WB multipliers (100..112, f32),
model (668). 12-bit samples MSB-first in 32-bit LE words with pairs swapped,
GRBG, white 4095.

## TIFF/EP fallback — `tiff_ep.rs`

For TIFF files from makes without a module: the largest full-resolution IFD with
PhotometricInterpretation 32803, uncompressed (MSB-first packed rows or 16-bit words in file byte
order, strips contiguous or not) or lossless JPEG (one stream per strip), CFA from
`CFARepeatPatternDim`/`CFAPattern` (or EXIF `CFAPattern`), white from `BitsPerSample`; the camera
database refines it. Files without a CFA IFD or pattern are rejected, never guessed (the Leaf `.mos`
and Sinar `.sti` files of the corpus that reach this path because their makes are not dispatched are
declined cleanly and belong to the medium-format module).

## GoPro GPR (VC-5) — `flashraw-codecs/src/vc5.rs`

GPR files are DNGs with TIFF compression 9: one VC-5 (SMPTE ST 2073) bitstream per image. The codec
parses the big-endian tag stream, decodes the 40 coded bands in parallel (16-bit lowpass; run-length
VLC highpass with GoPro's table 17, decompanding and dequantisation), runs the 3-level inverse wavelet
per channel (prescale, 14-bit clamp at the finest level) and recombines G-sum / R−G / B−G / G-diff
into the Bayer mosaic through the inverse-log curve scaled to the DNG white level.

API: `vc5::decode(data, width, height, white_level, phase, out, parallel)` with
`vc5::CfaPhase::from_pattern(&CfaPattern)` for the phase. The DNG decoder (`dng.rs`) dispatches
compression 9 to it and decodes the bitstream as one whole image (regions are cropped from it).

## Verification

All corpus files listed in `docs/support/misc.tsv` as verified decode and are covered by regression
hashes in `tests/regression/misc.tsv` (GoPro GPR: `tests/regression/gpr.tsv`).

## Unsupported (corpus)

- **Light L16 `.lri`**: multi-module container (16 camera modules, protobuf metadata, per-module
  proprietary packing). Reported as unsupported.
- **Casio QV-11 / QV-5000SX `.cam`**: Casio's processed CAM image format (not sensor data).
- **Canon A620 `Portulaca_1216.CRW`**: the file is a bzip2 archive (`BZh9`), not a raw.
