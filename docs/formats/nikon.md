# Nikon NEF / NRW

Module: `crates/flashraw-formats/src/nikon/` (container, makernote, decryption, shared
conventions in `conventions.rs`), codecs `flashraw_codecs::{nikon_huffman, nikon_packed, packed}`.

NEF and NRW are TIFF files with make `NIKON` / `NIKON CORPORATION`. The raw image is the
largest non-JPEG IFD (usually SubIFD 1). Coolpix models write NRW (and early E-series
Coolpix wrote NEF); Nikon Scan writes Coolscan NEFs (RGB scans).

## Payload variants

The payload is chosen from the raw IFD compression, its byte count and the makernote,
not from the compression tag alone, so files that are mislabelled by the camera firmware
still decode correctly.

| variant | detection | storage | codec |
|---|---|---|---|
| Huffman, lossy/lossless, 12/14-bit | compression 34713, no other rule matches | single sequential bit stream | `nikon_huffman` |
| 12-bit packed, MSB first | compression 1, 12 bps (D1..D4 era, multi-strip) | rows of `w*12/8` bytes | `packed` (SIMD) |
| 12-bit packed, LSB first | compression 34713 with `w*h*3/2` bytes and a `NIKON …` model (D4S, D810, D500, D850, Z 7 12-bit uncompressed) | | `packed` (SIMD) |
| 12-bit packed + pad byte every 10 samples | compression 34713 with `ceil(w/10)*16*h` bytes (D100 "compressed") | buffer widened to 3040 | `nikon_packed` |
| interlaced fields | Coolpix E-series NEF (even rows, then odd rows) | | `nikon_packed` (exact) |
| 12-bit packed in LE 32-bit words | Coolpix P (non-4032 wide) and B700 NRW | | `packed` Msb32 |
| 16-bit words | compression 1 with 14/16 bps, A1000, or `w*h*2` bytes (P7700/P7800/P330 NRW; P330 is shifted right by 4) | file byte order | `packed` |
| 14-bit LSB packed, rows padded to 16 bytes | Z-series / D6 14-bit uncompressed (`ceil(w*7/4/16)*16*h` bytes) | | `packed` (word kernel) |
| 12-bit LSB packed, rows padded to 16 bytes | Z-series / D6 12-bit uncompressed | | `packed` (SIMD) |
| small/medium NEF (sNEF) | `w*h*3` bytes | YCbCr 4:2:2, 12-bit, little-endian packed (Y0 Y1 Cb Cr per pixel pair) | YCbCr → RGB, 3 samples/pixel |
| Coolscan scans | software `Nikon Scan…` or model `COOLSCAN` | 8/16-bit RGB or grey | 16-bit RGB/grey |
| high efficiency (HE / HE*) | JPEG XS SOC+CAP markers (`FF10 FF50`) at the payload start | TicoRAW (intoPIX) | **unsupported** |

### Huffman

One of six trees (12/14-bit × lossy / lossy-after-split / lossless) selected from the
version bytes of makernote tag `0x0096` (`0x46` = lossless; `0x44 0x20`/`0x44 0x40` =
lossy with a split row read at offset 562, after which the "after split" tree with
`shl` low-order bits not transmitted takes over). The same tag holds the four initial
vertical predictors and the linearisation curve (`csize` anchors interpolated, or a
direct table). Sample values are `curve[clamp(pred as i16, 0, 0x3fff)]`.

Decoding is inherently sequential. The hot loop (shared with Pentax and Samsung V1)
keeps a 64-bit bit cache with branchless refills and resolves code + difference with
one 13-bit table lookup for most samples. Rows close to the end of the data use an
exact slow path with defined end-of-file behaviour (zero-padded
reads, then frozen predictors), so truncated files decode deterministically.

Rows beyond the visible height (two rows on Nikon 1 bodies, three on the D40X/D60/D80/
D3000) are not decoded and stay zero.

### sNEF representation

FlashRaw converts sNEF YCbCr to RGB with a fixed matrix, horizontal chroma
interpolation and a tone curve (14-bit output, black 0, white 16383), and outputs
the result as a 3-sample `Linear` image (verified on the D4S and D810 samples).

### High efficiency (TicoRAW)

Z 8, Z 9, Z f, Z6III, Z50II and Z5II "High Efficiency" / "High Efficiency★" files use
intoPIX TicoRAW, a JPEG XS-based proprietary codec with no public specification or
licensed open implementation. FlashRaw recognises the payload and returns
`UnsupportedCompression` from `decode`; metadata and embedded previews are available.

## Metadata

| item | source |
|---|---|
| makernote layout | type 2 (`Nikon\0\2` + embedded TIFF header, offsets relative to it), type 3 (D1: plain IFD), type 1 ignored |
| black level | `0x003D` (RGGB, 14-bit units; divided by 4 for 12-bit Huffman/packed data except Coolpix), Coolpix NRW block `0x0014`, camera-database colour table, P7000/P7100 255 rule |
| white level | `(1 << bps) - 1`, camera-database colour-table maximum when present, 4095 for 12-bit Huffman/packed DSLR data, 65504 for P7700/P7800, 16383 for sNEF |
| white balance | `0x000C` WB_RBLevels; `0x0097` colour balance 0100 (D100), 0102 (D2H), 0103 (D70); versions 200–217 decrypted with the serial (`0x001D`) and shutter count (`0x00A7`) keystream; NRW `0x0014`; E-series `0x0014`. D1 multipliers are analog balance (reported as 1) |
| CFA | raw IFD CFA pattern (TIFF-EP); D200 and Coolpix P overrides; camera database for interlaced E-series (the file pattern describes field order) |
| visible area | per-model trims (D1X, D3/D3S/D700, D40X…, D3100, D5000/D90, D5100/D7000/Coolpix A, D3200/D600/D610/D800, D4/Df, D2H, D2X, D300, D200, Nikon 1), then odd origins moved to even |
| colour matrix | camera-database colour table (`find_matrix`), other camera-database entries otherwise |
| previews | TIFF thumbnails, JpgFromRaw SubIFD, makernote preview IFD `0x0011` |

## Verification

The CC0 corpus holds 289 NEF/NRW files. Every file with plain sensor data decodes and is
covered by regression hashes in `tests/regression/nikon.tsv` (12/14-bit lossy and
lossless Huffman across D1X…Z 9, all uncompressed variants, NRW, sNEF, Coolscan);
High Efficiency files are reported as unsupported.
