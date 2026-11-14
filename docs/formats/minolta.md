# Minolta / Konica Minolta MRW, DiMAGE Z2 and RD-175

Module: `crates/flashraw-formats/src/minolta.rs`.

## MRW

Signature `\0MRM`, a 4-byte length, then tagged blocks; image data follows the
header:

| Block | Content |
|---|---|
| `PRD` | sensor height/width, image size, total/active bits, storage method (0x52 unpacked, 0x59 packed), Bayer pattern (1 = RGGB, 4 = G2BRG1) |
| `WBG` | as-shot white-balance gains in sensor Bayer order |
| `RIF` | presets, ISO, colour mode (not needed for decoding) |
| `TTW` | embedded TIFF: Make/Model/EXIF, thumbnail |

* Packed storage: 12-bit MSB-first rows (row-parallel unpack).
* Unpacked storage: 16-bit big-endian words with 12 active bits.
* Black is 0; white from the colour table (DiMAGE 7 0xF7D, A1 0xF8B, 5D/7D
  0xFFB, ...) or `2^active_bits - 1`.
* Model aliases (`DYNAX 5D`/`MAXXUM 5D`/`ALPHA SWEET DIGITAL` →
  `DG-5D`, `DYNAX 7D`/`MAXXUM 7D`/`ALPHA-7 DIGITAL` → `DG-7D`) for colour lookup.

The same block structure is embedded in Sony DSLR-A100 ARW files; `parse_blocks`
is shared with the Sony decoder.

## DiMAGE Z2

Headerless 5 869 568-byte dumps (2288 x 1710, BGGR), identified by size and a
non-zero tail (to distinguish same-size Nikon dumps). 12-bit samples MSB-first
inside 32-bit little-endian words; even rows in the first half of the file, odd
rows from the middle.

## RD-175 (`.MDC`)

Headerless 1 138 688-byte files from the 1995 three-CCD camera: 1481 lines of
768 8-bit samples from three sensors, re-interleaved into a 1534 x 986 GRBG
mosaic (green lines carry both quincunx phases, missing ones are interpolated,
values doubled; white 0x1FE).

## Konica Minolta TIFF files

TIFFs with a Minolta/Konica Minolta make carry no raw data; `open_tiff` rejects
them. Konica KD-400Z/KD-510Z and DiMAGE G-series JPEG-embedded raws are not
supported.

## Verification

All 15 corpus files (14 MRW in packed and unpacked storage across 12 bodies, the
Z2 dump and the RD-175 file) decode and are covered by regression hashes
(`tests/regression/minolta.tsv`). Unit tests: synthetic MRW round trips
(packed/unpacked, both Bayer orders), garbage headers; corpus malformed-input
tests in `sony::robustness`.
