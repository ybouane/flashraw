# Sony ARW / SR2 / SRF / ARQ

Module: `crates/flashraw-formats/src/sony/` (container, metadata, geometry),
codecs `crates/flashraw-codecs/src/sony_*.rs`.

All Sony raws are TIFF containers (`II*\0`, make `SONY`). The sensor image is the
largest non-preview IFD (a SubIFD of IFD0 for ARW/SR2, IFD0 itself for SRF).

## Payloads

| Mode (`mode` extra) | Detection (raw IFD) | Layout | Decoder |
|---|---|---|---|
| `arw2` | compression 32767, `bytes == w*h` | 16-byte blocks of 16 same-colour samples: 11-bit max/min + their indices, 14 x 7-bit deltas scaled by `1 << sh`; even and odd columns alternate blocks; 11-bit values doubled through the 4096-entry `SonyRawCurve` (0x7010) | `sony_arw2`, row-parallel |
| `arw1` | compression 32767, `bytes*8 != w*h*bps` (A100–A390) | single MSB-first Huffman stream (fixed 18-symbol prefix code, JPEG-style diffs), one running sum, columns right to left, even rows then odd rows; 8 extra coded rows | `sony_arw1`, sequential |
| `arw1` (DSLR-A100) | model `DSLR-A100` | as above; data starts at the SubIFD pointer, geometry and WB in an embedded Minolta MRW block (`DNGPrivateData`) | `sony_arw1` |
| `12bit-packed` | compression 32767, 12 bps, `bytes*8 == w*h*12` (A850/A900) | 12-bit LSB-first packing (pairs in 3 bytes) | `packed`, row-parallel |
| `uncompressed` | compression 1 or 32767, `bytes == w*h*2` | 16-bit little-endian containers holding 12- or 14-bit data (big-endian for the DSC-R1 SR2) | `packed`, row-parallel |
| `arq` | compression 1, 4 samples, `bytes == w*h*8` | 4-shot pixel-shift composite, per pixel R, G, G2, B (output reordered to R, G, B, G2, 4 samples per pixel) | row-parallel |
| `lossless` | compression 7 (or 6), 1 sample | lossless JPEG tiles (512x512); each JPEG MCU (4 components) is one 2x2 Bayer quad | `ljpeg`, tile-parallel |
| `lossless-ycbcr` | compression 7/6, photometric 6, 3 samples (lossless S/M sizes) | 3-component SOF3 tiles with 2x2 (4:2:0) or 2x1 (4:2:2) luma sampling and Sony's own chained prediction; converted to RGB (4 samples per pixel, 4th = 0) with the JFIF equations around a chroma offset of 16383 | `sony_ycc`, tile-parallel |
| `arw6` | compression 32766 (ILCE-7M5 "compressed"/"compressed HQ") | tile streams of a 3-level 5/3 integer wavelet over green average / red / blue residual planes plus a final green level; coefficient packets with adaptive bit widths and zero runs; 12-bit codes through a fixed curve (max 39002) | `sony_arw6`, tile/record/plane/row-parallel |
| `srf` | model DSC-F828 / DSC-V3 | big-endian 16-bit samples at a fixed offset, XORed with the Sony key stream; the data key is itself encrypted | `sony::srf`, key stream generated in bulk, rows parallel |

## Metadata

* **Black level**: `0x7310` (RGGB, raw IFD on recent bodies or the SR2 private
  directory) or `0x7300` (per-channel offsets); defaults when absent: 512 for ARW2,
  `128 << (bits - 12)` otherwise (wide sensors only), zero for 4600-pixel bodies;
  then the colour-table black (`black > 0` overrides, `< 0` applies only without
  file black). 12-bit packed bodies store levels in 14-bit units (divided by 4).
  ARW6 uses fixed levels (black 1024, white 39002); YCbCr output black 1024,
  white 17536 (nominal levels for that mode).
* **White level**: `WhiteLevel` (0xC61D) in the raw IFD, else `2^bits - 1`, then the
  colour-table maximum.
* **White balance**: `0x7303` (GRBG) / `0x7313` (RGGB) from the raw IFD or the SR2
  private directory (the latter wins); A100 from the MRW `WBG` block. Rejected when a
  multiplier is zero or the spread exceeds 100x. Lossless YCbCr (S/M) images are
  white-balanced by the camera before the YCbCr conversion: `white_balance_applied` is
  set, the as-shot multipliers are still reported, and RGB outputs develop with unit
  multipliers.
* **SR2 private data**: IFD0 `DNGPrivateData` (0xC634) points to an IFD with
  `0x7200` (offset), `0x7201` (length) and `0x7221` (key, 4 UNDEFINED bytes). The
  block is decrypted with the key stream (`sony::crypt`) and parsed as an IFD whose
  value offsets are absolute file offsets.
* **Geometry**: the full IFD size is the buffer (`+8` rows for ARW1, `+1` column for
  the A100). The visible area follows per-body rules (`visible_area`):
  fixed trims by raw width (e.g. 6048 → `-24`, 8000/9600 → `-32`), per-model tables
  for the lossless/S/M sizes of the ILCE-1/1M2, 7M4/7CM2/FX2/7M5, 7RM5/7CR/LR1,
  9M3, ZV-E1, 6700/ZV-E10M2/FX30, and APS-C crop modes of full-frame bodies.
  The recommended crop comes from `0x74C7/0x74C8` (or DNG `DefaultCrop*`).
* **CFA**: TIFF-EP `CFAPattern` of the raw IFD (GRBG for the A100, RGBE for the
  F828, measured black from the optical-black columns for SRF).
* **Colour matrix**: the camera-db colour table in table order (first matching
  model prefix), with a few model aliases (UMC-R10C → ILCE-QX1, ...).
* **Previews**: JPEGs referenced from IFD0/IFD1 (`JPEGInterchangeFormat`).
* **Orientation**: first non-default `Orientation` tag.

## Verification

208 corpus files (108 body folders) decode and their full buffers are covered by
regression hashes (`tests/regression/sony.tsv`). Malformed-input tests truncate and corrupt
samples of every mode (`sony::robustness`). Codec unit tests: ARW2 block
round-trip and row interleave, ARW1 encoder round-trip, YCbCr 4:2:0 synthetic
stream, ARW6 bit reader against a bit-at-a-time reference and inverse lifting
against a forward 5/3 transform, key stream bulk generator against the cipher.

Not covered by samples: DSC-V3 SRF (implemented from the format description),
ARW1 bodies other than the A100–A390 family, 8-bit ARW (rejected),
multi-tile ARW6 strips (supported by the stream table, untested).

## Quirks

* The last row/column of lossless tiled frames is not written by the tile decoder and
  stays zero.
* The Hasselblad Lunar (rebadged NEX-7, make `HASSELBLAD`) is dispatched to the
  Hasselblad module; it is an ordinary ARW2 file (`sony::open_tiff` handles it).
