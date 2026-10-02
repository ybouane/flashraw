# Olympus / OM System ORF and ORI

Module: `crates/flashraw-formats/src/olympus.rs`, codec
`crates/flashraw-codecs/src/olympus.rs`.

ORF is TIFF with a vendor magic: `IIRO` (most bodies), `IIRS` (C-/SP-series),
`MMOR` (big-endian E-10/E-20). ORI files (the base frame written next to a
high-resolution composite) are ordinary ORFs. Makes: `OLYMPUS OPTICAL CO.,LTD`,
`OLYMPUS CORPORATION`, `OLYMPUS IMAGING CORP.`, `OM Digital Solutions`.

## Payloads

The sensor image is the largest uncompressed-TIFF IFD (IFD0). Its layout is
does:

| Mode | Detection | Layout |
|---|---|---|
| `compressed` | 16 bps, `first_strip * 7 > w*h` | Olympus lossless compression (below) |
| `12bit-packed` | 12 bps, `bytes = 1.5 * w*h` | 12-bit MSB-first within 32-bit little-endian words |
| `12bit-packed` | 12 bps otherwise | 12-bit MSB-first bytes |
| `12bit-packed-interlaced` | 12 bps, photometric 2 (C-/SP-series) | MSB-first rows; even rows first, odd rows from the next 2048-byte boundary after the even field |
| `12bit-packed-padded` | `bytes = 1.6 * w*h` (E-300/E-330/E-500, E-M5 II/PEN-F high-res) | 12-bit LSB-first, one pad byte after every 10 samples |
| `uncompressed` | 16 bps, multi-strip (E-1, E-10/20, E-400) | 16-bit words in file byte order, data in the top bits (`>> 4`, `>> 6` for 10-bit bodies) |

Frames with an odd height get one more buffer row, read from the bytes that follow

### Olympus lossless compression

One MSB-first bit stream codes all samples. Even and odd columns are two channels
with independent adaptation state; prediction uses same-colour neighbours two
pixels away (west, north, north-west) with an edge-sensitive median-like rule.
Each sample: a sign bit, 2 literal low bits, a unary "high" part (escape after 12
zeros to a fixed-width field) and an adaptive number of low bits that depends on
the magnitude of the previous same-channel code and on a run counter of small
codes; the code is combined with a smoothed residual (`3 * diff + carry >> 5`).
The makernote ImageProcessing tags 0x640–0x653 describe the coder parameters;
every body uses the same values, which FlashRaw folds into a specialised decoder
(any other parameter set takes a generic path; both are tested against a
bit-at-a-time reference). 14-bit OM System files use the same coder.

Performance: the bit stream parse never depends on reconstructed values, so the
entropy decoding of a band of rows (position-addressed 64-bit window, one load per
sample, branch-free width computation) overlaps on a second thread with the
branch-free prediction of the previous band (two rows at a time = four
independent dependency chains). About 3 ns per sample.

## Metadata (makernote)

Headers: `OLYMPUS\0II\x03\0` (offsets relative to the makernote), `OM SYSTEM\0\0\0II\x04\0`
(same, IFD 4 bytes later) and the older `OLYMP\0` (offsets relative to the file).
Sub-directories (0x2010 Equipment, 0x2040 ImageProcessing) are IFD pointers or
embedded blobs.

* White balance: ImageProcessing 0x100 (R, B levels / 256), old bodies 0x1017/0x1018.
* Black: ImageProcessing 0x600 (RGGB), old bodies 0x1012; ×4 for 14-bit files.
* Valid bits: 0x611 (old: 0x102C) → white level `2^bits - 1`; colour-table and
  per-body maxima (E-300/E-500 unpacked 0xFC3, E-330 unpacked 0xF79, Stylus 1 0xFFF).
* Crop: 0x612–0x615 → `FrameInfo::crop`.
  10400 → -12, 8200 → -30, 8180 → -10, 9280 → -6 x -6) and by body (E-300/E-500
  -20, E-330 -30, TG-4 -16, TG-5/6/7 -26, Stylus 1 -16).
* CFA from the EXIF `CFAPattern`; colour matrix from the colour table with

## Verification

All 112 corpus ORF/ORI files (71 body folders, 2002–2025, incl. 50/80 MP high-res
all metadata checks passing (`tests/regression/olympus.tsv`). Malformed-input
tests cover every layout (`sony::robustness::olympus_*`).

Not covered by samples: C-740UZ/C-770UZ headerless variants and ORFs with
non-standard coder parameters (generic path, unit-tested only).
