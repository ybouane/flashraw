# Sigma / Foveon X3F

Module: `crates/flashraw-formats/src/sigma.rs`; codec: `crates/flashraw-codecs/src/x3f.rs`.
Sigma DNG files (fp, fp L, and DNGs written by Sigma Photo Pro) go through the DNG decoder.

## Container

| Part | Content |
|---|---|
| `FOVb` header | version (2.x/3.x classic, 4.x Quattro), rotation (0/90/180/270 → EXIF orientation) |
| directory | `SECd` block at the offset stored in the last 4 bytes; ≤ 50 entries of (offset, size, type) |
| `SECp` | UTF-16 property list: `CAMMANUF`, `CAMMODEL`, `ISO`, `SHUTTER`, `APERTURE`, `FLENGTH`, `FLEQ35MM`, `LENSMODEL`, `LENSFRANGE`, `LENSARANGE`, `TIME`, `CAMSERIAL`, `FIRMVERS`, `WB_DESC` |
| `SECi` | images: raw (see below), JPEG thumbnail (`2/12`), plain RGB thumbnail (`2/3`); Huffman thumbnails (`2/b`) and Quattro `2/19` thumbnails are not exposed |

`SIGMA dp<n>` / `sd Quattro (H)` strings in the first 2 KiB, falling back to the raster size.

## Raw image types

| type/format | Cameras (corpus) | Coding |
|---|---|---|
| 3/5 | Polaroid x530 | uncompressed: one LE u32 per pixel holding three 10-bit indices into a 1024-entry difference table; running sums per colour |
| 3/6 | SD9, SD10, SD14 | 10-bit Huffman: explicit (non-canonical) codes mapped through the difference table; per-row offsets → rows decode in parallel; 16-bit signed running sums, globally offset when negative (X3F tools behaviour) |
| 3/1e | DP1, DP1s, SD15 | TRUE I/II |
| 1/1e | DP1/2/3 Merrill, SD1 Merrill | TRUE II |
| 1/23, 1/25, 1/27, 1/29 | dp0/dp2 Quattro, sd Quattro, sd Quattro H | TRUE III with per-plane geometry |

TRUE: three independent planes (bottom, middle, top) at 16-byte aligned offsets, one shared prefix
code whose symbol is the bit length of a JPEG-style difference; prediction per 2x2 phase (first two
columns from two rows above, others from two columns left), seeds from the header. The planes decode
concurrently (3 threads); each plane is a strictly sequential stream.

## Output representation


- classic/Merrill: the three full-resolution layers interleaved (channel 0 = bottom … 2 = top);
- Quattro (1:1:4): channel 2 = full-resolution top layer; channels 0/1 = half-resolution bottom/middle
  the phase-detect AF pixel repair (`x3f_dpq_interpolate_af`/`_af_sd`, float arithmetic matched) and
  the replication of the lower layers over each 2x2 block (block rows `2..h/2-2`, block columns

## Metadata

Margins (active area) and white level come from the camera database (entries
with full-frame geometry, the colour-table black and white 0x3fff.

## Verification


