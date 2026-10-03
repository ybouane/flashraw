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
| `SECc` | CAMF calibration blob: not parsed (not needed for the decoded buffer, levels or white balance) |

Quattro files have no property list: the make is `SIGMA` and the model is derived from
`SIGMA dp<n>` / `sd Quattro (H)` strings in the first 2 KiB, falling back to the raster size.

## Raw image types

| type/format | Cameras (corpus) | Coding |
|---|---|---|
| 3/5 | Polaroid x530 | uncompressed: one LE u32 per pixel holding three 10-bit indices into a 1024-entry difference table; running sums per colour |
| 3/6 | SD9, SD10, SD14 | 10-bit Huffman: explicit (non-canonical) codes mapped through the difference table; per-row offsets → rows decode in parallel; 16-bit signed running sums, globally offset when negative |
| 3/1e | DP1, DP1s, SD15 | TRUE I/II |
| 1/1e | DP1/2/3 Merrill, SD1 Merrill | TRUE II |
| 1/23, 1/25, 1/27, 1/29 | dp0/dp2 Quattro, sd Quattro, sd Quattro H | TRUE III with per-plane geometry |

TRUE: three independent planes (bottom, middle, top) at 16-byte aligned offsets, one shared prefix
code whose symbol is the bit length of a JPEG-style difference; prediction per 2x2 phase (first two
columns from two rows above, others from two columns left), seeds from the header. The planes decode
concurrently (3 threads); each plane is a strictly sequential stream.

## Output representation

`width x height x 3` u16 (`SensorLayout::Foveon`, cpp 3):

- classic/Merrill: the three full-resolution layers interleaved (channel 0 = bottom … 2 = top);
- Quattro (1:1:4): channel 2 = full-resolution top layer; channels 0/1 = half-resolution bottom/middle
  layers placed on even 2x2 sites, then two post-steps:
  the phase-detect AF pixel repair (dp Quattro and sd Quattro patterns, float arithmetic) and
  the replication of the lower layers over each 2x2 block (block rows `2..h/2-2`, block columns
  `0..w/2-4`, borders stay zero);
- Polaroid x530: channels swapped to R,G,B order.

## Metadata

Margins (active area) and white level come from the camera database (entries
`mode = "x3f-<width>x<height>"` in `camera-db/cameras.json`), black level and colour matrix from the
camera database's colour table (prefix-matched). No as-shot white balance is reported. Unknown bodies/sizes still decode
with full-frame geometry, the colour-table black and white 0x3fff.

## Verification

All 18 corpus X3F files decode and are covered by regression hashes (`tests/regression/sigma.tsv`).

Performance (16 threads, decode only, `flashraw benchmark`): Merrill 4928x3264x3 ≈ 52 ms,
sd Quattro H ≈ 97 ms, SD9/SD10 ≈ 5–15 ms.
TRUE planes use a fused 14-bit table (prefix code + difference bits in one probe); the Quattro top
plane (a single sequential stream of up to 30 M samples) bounds the wall time.
