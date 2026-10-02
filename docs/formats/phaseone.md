# Phase One IIQ and Leaf MOS

Module: `crates/flashraw-formats/src/phaseone.rs`; codecs:
`crates/flashraw-codecs/src/phaseone.rs` (IIQ L / IIQ S), `crates/flashraw-codecs/src/leaf.rs`
(restart-parallel lossless JPEG).

## IIQ container

IIQ files are TIFFs (previews, EXIF) whose maker note is the Phase One directory. Detection
is by magic: `IIII`/`MMMM` within the first 32 bytes followed by a `?Raw` word (`probe`), so
both bare and TIFF-wrapped files (`.IIQ`, Phase One `.TIF`, Leaf Credo `.IIQ`) are handled
before generic TIFF dispatch. All directory offsets are relative to the `IIII` header.

| Tag | Meaning |
|---|---|
| 0x100 | rotation (0..3 → none / 90° CW / 270° / 180°) |
| 0x107 | as-shot multipliers (3 floats) |
| 0x108/0x109 | raw width/height; 0x10A..0x10D visible area (margins may be odd) |
| 0x10E | payload format; 0x10F payload offset/size; 0x110 calibration block |
| 0x112 | scrambling keys (formats 1/2) |
| 0x21C | row-offset table; 0x21D sensor black (`t_black`) |
| 0x222/0x224 | split column/row; 0x223/0x225 per-row/per-column black references |
| 0x301 | firmware string (model, "Achromatic") |

Payload formats:

| Format | Name | Coding |
|---|---|---|
| 0, 1, 2 | uncompressed | 16-bit words; 1/2 XOR-scrambled with a key pair (mask 0x5555 / 0x1354) |
| 3, 5 | IIQ L | per 8-pixel block and column parity a prefix code picks a difference width from `{8,7,6,9,11,10,5,12,14,13}`; 14 escapes to a 16-bit literal; 14-bit samples ×4. Format 5 maps values < 256 through `round(v²/3.969)` |
| 8 | IIQ L 16-bit | as format 3 without the ×4 scaling (IQ3/IQ4 "16-bit" captures) |
| 6 | IIQ S | per block, each parity carries a quantisation level (delta code or table reset); level 9 = 14-bit literal; output ×4, clamped |

Every row starts at an offset from the row table and is an independent bit stream
(MSB-first in 32-bit little-endian words), so rows decode **in parallel**, and region
decodes touch only the rows they need (`RandomAccess::Rows`, one `DecodeUnit` per row).

### What FlashRaw reports

  shrinks by one. The CFA (RGGB at the vendor's visible origin) is re-anchored at the
  buffer origin, so e.g. P25/P45 report GBRG/GRBG at (0, 0) of the visible area.
* Black: `t_black` plus the row/column black references as `BlackLevel` row/column deltas
  (each reference has two halves split at 0x222/0x224; FlashRaw uses their sample-weighted
  mean). White: 0xfffc for 14-bit L/S data, 0xffff for 16-bit L and uncompressed data.
* Achromatic backs (firmware string contains "Achromatic") are reported as monochrome.
* **Not applied:** the vendor calibration in the 0x110 block (defect columns/pixels, flat
  only in `raw2image`/post-processing (`phase_one_correct`), not to `raw_image`; FlashRaw
  exposes the uncorrected sensor data and the calibration size in metadata
  (`calibration_bytes`).

## Leaf MOS

TIFF without a Make tag, identified by the `PKTS` metadata tag 0x8606 (dispatch added in
`flashraw-formats/src/lib.rs`). The packet tree supplies the back model
(`ShootObj_back_type`), neutrals (WB), plane count, mosaic pattern and rotation. The CFA is
evaluated at the end of the packet level that describes the mosaic, with the rotation known

Payloads: one lossless-JPEG tile (compression 99/7), uncompressed big-endian 16-bit (single
strip/tile or horizontal bands). Leaf JPEG streams carry a DRI interval of whole rows; the
scan is split at `RSTn` markers and the intervals decode in parallel. When the `Rows_data`
packet has bit 0 set, JPEG rows are interleaved (even rows top-down, odd rows bottom-up) and
are scattered back to sensor order. White is 0x3fff (0x1fff for 15-bit streams).

## Unsupported

  a 4-channel image from the shots; no corpus sample to validate — returns
  `UnsupportedCompression`.
* IIQ payload formats other than 0/1/2/3/5/6/8 (none known).
* Leaf tiles narrower than the image.
