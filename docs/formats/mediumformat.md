# Epson ERF, Mamiya MEF, Sinar

Modules: `crates/flashraw-formats/src/medium/{epson,mamiya,sinar}.rs` (shared helpers in

## Epson ERF (R-D1, R-D1s, R-D1x)

TIFF, raw image in a SubIFD with compression 32769: 12-bit samples MSB-first, ten samples in
15 bytes plus one padding byte (16-byte groups). The Epson maker note (`EPSON\0`, IFD in file
byte order, file-relative offsets) gives the per-channel black (0x0401, RGGB order) and white
balance (0x0E80: `R = v·567/65536`, `B = v·431/65536`). Rows decode in parallel. Dispatched
from `lib.rs` for Make "Epson"/"SEIKO EPSON".

## Mamiya MEF (ZD)

TIFF SubIFD with 12-bit MSB-packed samples (compression 1, no row padding); exposed as a
`PackedPayload`. Mamiya-branded Leaf backs (DM series) write Leaf MOS and use the MOS
decoder.

## Sinar (CaptureShop `.sti`)

One 16-bit big-endian uncompressed tile; WB from the "Neutral r g b" line of the Sinar text
tags 0xC516/0xC517; white 0x3FFF. The CFA repeat-dimension tag is malformed (`2, 0`) in
these files, so a 4-byte pattern is read as 2×2. Dispatched for Make "Sinar".

## Unsupported

* Sinar IA (`parse_sinar_ia`, non-TIFF container with META/RAW0 records): no sample.
