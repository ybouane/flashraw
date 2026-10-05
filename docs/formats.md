# Supported formats

Support is tracked per **format and mode** (bit depth, compression variant, size/crop
mode), not per extension. A mode is `verified` when representative corpus files decode
with full-buffer hashes pinned by the regression suite, their metadata (geometry, CFA,
levels, white balance, colour matrix, orientation) has been checked, tests exist and
corruption handling is in place. `partial` and `experimental` modes are implemented but
lack that evidence; `unsupported` modes are detected and rejected with a clear error.

## Coverage of the CC0 corpus

2,016 files from raw.pixls.us (all makes and models available):

| Format | Files | Decoded |
|---|---:|---:|
| 3FR | 11 | 11 |
| ARI | 1 | 1 |
| ARW / SR2 / SRF / ARQ | 209 | 209 |
| CR2 | 141 | 141 |
| CR3 | 110 | 110 |
| CRW | 22 | 21 |
| DCR / DCS | 10 | 10 |
| DNG (incl. GPR) | 281 | 278 |
| ERF | 3 | 3 |
| FFF | 11 | 11 |
| IIQ | 47 | 47 |
| KDC | 9 | 8 |
| MEF | 1 | 1 |
| MOS | 4 | 4 |
| MRW | 14 | 14 |
| NEF | 277 | 249 |
| NRW | 12 | 12 |
| ORF / ORI | 112 | 112 |
| PEF | 42 | 42 |
| RAF | 151 | 151 |
| RAW (Panasonic, Kodak, CHDK, headerless) | 46 | 46 |
| RW2 / RWL | 432 | 432 |
| SRW | 27 | 27 |
| X3F | 18 | 18 |
| other (TIF raws, CAM, LRI, misc) | 25 | 22 |
| **Total** | **2,016** | **1,980** |

Files not decoded (36): 28 Nikon "High Efficiency" NEFs (intoPIX TicoRAW, a
proprietary JPEG XS-based codec — metadata and previews still work), 2 smartphone
"DNG"s that are actually JPEGs, 2 Casio `.cam` processed images, a bzip2 archive named
`.CRW`, Kodak DC120 JPEG raw, Light L16 `.lri` (proprietary multi-module container),
and a JPEG XL DNG (decodes with the `jpegxl` feature).

## Vendor documentation

Each page documents container structure, compression variants, maker-note sources
for levels/WB/crop, camera quirks, firmware differences, verification evidence and
known limitations:

[Canon](formats/canon.md) · [Nikon](formats/nikon.md) · [Sony](formats/sony.md) ·
[Fujifilm](formats/fuji.md) · [Panasonic / Leica RWL](formats/panasonic.md) ·
[Olympus / OM System](formats/olympus.md) · [Pentax / Ricoh](formats/pentax.md) ·
[Samsung](formats/samsung.md) · [Minolta](formats/minolta.md) ·
[Phase One / Leaf](formats/phaseone.md) · [Hasselblad](formats/hasselblad.md) ·
[Kodak](formats/kodak.md) · [Epson / Mamiya / Sinar](formats/mediumformat.md) ·
[Sigma X3F](formats/sigma.md) · [GoPro, CHDK, headerless and misc](formats/misc.md)

DNG is described in [architecture.md](architecture.md) and the table below:
strips/tiles, uncompressed 8–16 bit, lossless JPEG, deflate (integer and float
predictors), lossy JPEG, JPEG XL, VC-5, Blackmagic DCT tiles, linearization tables,
black-level patterns with H/V deltas, ActiveArea/DefaultCrop, CFA re-anchoring,
colour/forward/calibration matrices, masked areas, multi-frame selection.

## Support matrix by mode

Generated from `docs/support/*.tsv` by `tools/support_matrix.py` (also printed by
`flashraw supported`).

<!-- BEGIN GENERATED SUPPORT TABLE (tools/support_matrix.py) -->
Status counts: experimental: 17, partial: 7, supported: 1, unsupported: 20, verified: 110

| Format | Mode | Compression | Status | Notes |
|---|---|---|---|---|
| 3FR | Hasselblad lossless JPEG | hasselblad | verified | H3D, H4D-60, CFV-16, CF-22, CFV-50c: decode |
| 3FR | uncompressed 16-bit | uncompressed | verified | X1D, X1D II, CFV II 50C, X2D, CFV 100C: decode; masked-area black |
| ARI | ARRI ALEXA (Mini) | 12-bit msb32 packed | verified | decodes |
| ARQ | arq | uncompressed | verified | 4-shot pixel shift (4 samples/pixel); 16-shot layout follows the same path, no sample |
| ARW | 12bit-packed | uncompressed | verified | A850/A900 12-bit LSB packing; 14-bit levels scaled |
| ARW | 8-bit | other | unsupported | 8-bit ARW variants are rejected with an error |
| ARW | Hasselblad Lunar/Stellar/HV | sony | partial | delegated to the Sony decoder |
| ARW | arw1 | sony-arw1 | verified | 9 corpus files (A100–A390 Huffman column stream), sequential |
| ARW | arw2 | sony-arw2 | verified | 114 corpus files (cooked 8-bit delta blocks + tone curve), row-parallel |
| ARW | arw6 | other | verified | 4 corpus files (ILCE-7M5 compressed / compressed HQ), wavelet codec, parallel |
| ARW | lossless | sony-lossless | verified | 19 corpus files, lossless JPEG tiles (L sizes, crop modes), tile-parallel |
| ARW | lossless-ycbcr | sony-lossless | verified | 19 corpus files, S/M sizes: 4:2:0 and 4:2:2 YCbCr tiles converted to RGB (4 samples/pixel) |
| ARW | uncompressed | uncompressed | verified | 38 corpus files, 12/14-bit data in 16-bit containers incl. APS-C and S35 crops |
| CAM | Casio QV-11 / QV-5000SX | - | unsupported | processed CAM image format, not sensor data |
| CR2 | raw | lossless-jpeg | verified | All CR2 generations (20D..5DS/1D X II/M100/G1 X III), sliced and unsliced; 108 corpus files decode, regression-hashed; corruption tests |
| CR2 | sRaw1/sRaw2 (sRAW/mRAW) | canon-sraw | verified | YCbCr 4:2:0/4:2:2 -> linear RGB (cpp=3), both colour-conversion generations; 31 corpus files decode, regression-hashed |
| CR3 | burst/dual-pixel | canon-crx | partial | First frame / first image only |
| CR3 | craw | canon-crx | verified | C-RAW wavelet (CMP1 0x100 per-line QP, 0x200 QP maps), multi-tile; 51 files decode |
| CR3 | enc3 | canon-crx | experimental | Encoding type 3 colour planes / rounded near-lossless lines: implemented from the format structure, no samples |
| CR3 | raw | canon-crx | verified | Lossless CRX; 59 corpus files decode incl. dual-pixel/crop, regression-hashed |
| CRW | Canon A620 Portulaca_1216.CRW | - | unsupported | file is a bzip2 archive, not a raw |
| CRW | canon600-10bit | uncompressed | experimental | PowerShot 600; no sample |
| CRW | compressed-10bit | canon-crw | verified | G1, G2, S30, S40; regression-hashed |
| CRW | compressed-12bit | canon-crw | verified | EOS D30/D60/10D/300D, G3/G5/G6, Pro1, S45-S70 (low-bit plane); regression-hashed |
| CRW | packed-10bit | uncompressed | verified | PowerShot Pro70 decodes (CMYG 8x2 CFA); A5/A50 share the path but are untested |
| CRW/CR2 | CHDK headerless | uncompressed | unsupported | Headerless CHDK dumps go through the generic file-size path (misc), not the Canon decoders |
| DCR | 65000 CFA | kodak-65000 | verified | DCS Pro 14N/14nx/SLR/c/SLR/n, DCS760C: decode; linearisation curve applied; parallel rows |
| DCR | 65000 RGB / YCbCr | kodak-rgb, kodak-ycbcr | experimental | decoded to 3-sample RGB; no corpus sample |
| DCR | compression 262 | - | unsupported | no sample |
| DCS | 8-bit + curve | uncompressed | verified | DCS460D, EOS DCS 1, EOS DCS 3 |
| DCS | lossless JPEG + curve | lossless-jpeg | verified | DCS520C, DCS560C |
| DNG | CFA | GoPro VC-5 (compression 9, GPR) | verified | 17/17 corpus files decode, regression-hashed |
| DNG | CFA / LinearRaw / mono, 8-16 bit | uncompressed (strips, tiles; MSB-packed 10/12/14) | verified | all corpus files decode (Leica, Pentax, Ricoh, Sigma fp, smartphones, converter-produced DNGs); StripByteCounts understatement tolerated (DJI) |
| DNG | CFA / LinearRaw, 1-4 components | lossless JPEG (predictors 1-7, restart markers, 2-component tile rows) | verified | all corpus files decode incl. smartphones with per-row restarts and Pentax Pixel Shift (frame 0) |
| DNG | CFA, 12-bit | SOF1 12-bit DCT tiles (Blackmagic) | verified | bit-exact with the scalar float IDCT |
| DNG | CFA, float 16/24/32 | deflate + floating-point predictors 3/34894/34895 | verified | merged-HDR files; RawU16 output: when max(white, data maximum) lies outside 4096..32767 it is scaled to 16383, then clamped at 0 and truncated; big-endian files whose WhiteLevel LONG holds the bits of 1.0f and whose samples are little-endian (one HDR-merge writer) are read little-endian with white 1.0; float data whose integer Black/WhiteLevel tags are in another unit than the samples (e.g. samples below 0.08 with white 1023) decode as stored and render dark |
| DNG | CFALayout != 1 | staggered layouts | unsupported | no corpus samples; explicit error |
| DNG | LinearRaw | lossy JPEG (34892) | verified | converter-produced lossy files |
| DNG | any | deflate + integer predictors 1/2/34892/34893 | verified |  |
| DNG | any | JPEG XL (52546, DNG 1.7) | experimental | `jpegxl` feature; corpus sample verified visually only |
| DNG | opcode lists | OpcodeList1/2/3 | supported | applied by default, each list in its stage (1: stored values; 2: normalised active area; 3: demosaiced camera RGB): GainMap, MapTable, MapPolynomial, per-row/column delta and scale, FixBadPixelsConstant/List, FixVignetteRadial; geometric opcodes (WarpRectilinear, WarpFisheye, TrimBounds) are reported, not applied; `DecodeOptions::apply_opcodes = false` / `--no-opcodes` disables |
| ERF | 12-bit padded packed (32769) | uncompressed | verified | R-D1, R-D1s, R-D1x: decode; maker-note black and WB |
| FFF | Hasselblad lossless JPEG | hasselblad | verified | H4D-40/50, H5D-40/50c, H6D-100c MS (1-shot), CFV-50/50c, X1D II, X2D: decode |
| FFF | Imacon/Flexcolor (Imacon tags 0xB4C3) | - | unsupported | Ixpress Imacon-tag container; no sample |
| FFF | multi-shot (4/6-shot) | - | unsupported | several samples per pixel; no sample |
| GPR | GoPro HERO5-HERO11, FUSION (DNG compression 9) | vc5 | verified | 17/17 corpus files decode |
| IA | Sinar IA container | - | unsupported | no sample |
| IIQ | IIQ L (formats 3, 5) | phaseone | verified | 14-bit; 33 corpus files (P20+..IQ4, Leaf Credo) decode; parallel rows |
| IIQ | IIQ L 16-bit (format 8) | phaseone | verified | IQ3/IQ4 16-bit captures decode |
| IIQ | IIQ S (format 6) | phaseone-s | verified | 9 corpus files decode; parallel rows |
| IIQ | achromatic backs | phaseone | experimental | monochrome layout from firmware string; no corpus sample |
| IIQ | all | - | partial | vendor sensor calibration (tag 0x110 block) not applied to the buffer; black reported as row/column deltas, not subtracted |
| IIQ | uncompressed (format 0) | uncompressed | verified | H 25 TIFF sample decodes |
| IIQ | uncompressed scrambled (formats 1, 2) | uncompressed | experimental | key unscrambling implemented; no corpus sample |
| JPEG+BRCM | Raspberry Pi IMX219 (RAW10) | mipi raw10 | verified | decodes; WB from maker note |
| JPEG+BRCM | Raspberry Pi IMX477 (RAW12) | raw12 | verified | decodes |
| JPEG+BRCM | Raspberry Pi OV5647 (RAW10) | mipi raw10 | verified | decodes (2611-sample rows) |
| JPEG+BRCM | Raspberry Pi RAW8/RAW14/RAW16 modes | raw8/14/16 | experimental | no corpus sample |
| KDC | 12-bit packed | uncompressed | verified | EasyShare Z981/Z990/Z1015, P712, P880 |
| KDC | DC120 | kodak-dc120 | verified | 2 samples |
| KDC | DC120 JPEG raw | - | unsupported | JPEG-compressed raw data not decoded |
| KDC | RADC (DC40/DC50) | other | verified | DC50 sample decodes |
| LRI | Light L16 | - | unsupported | multi-module container with proprietary packing |
| MDC | Minolta RD-175 | rd175 interleave | verified | decodes |
| MDC | rd175 | uncompressed | verified | RD-175 three-CCD dump |
| MEF | 12-bit packed | uncompressed | verified | Mamiya ZD sample decodes |
| MOS | Leaf banded uncompressed (leaf_hdr) | uncompressed | experimental | no corpus sample |
| MOS | Leaf lossless JPEG | leaf | verified | Aptus 22/75, AFi-II 12 (incl. interleaved rows); restart intervals decoded in parallel |
| MOS | Leaf uncompressed 16-bit | uncompressed | verified | Aptus 22 |
| MOS | multi-shot (Sinar/Leaf 4-shot) | - | unsupported | several samples per pixel; no sample to validate |
| MRW | DiMAGE G / Konica KD JPEG-embedded | other | unsupported | not implemented |
| MRW | packed | minolta-packed | verified | 9 corpus files (DiMAGE A1/A2/A200, Dynax/Maxxum/Alpha 5D and 7D) |
| MRW | unpacked | uncompressed | verified | 4 corpus files (DiMAGE 5, 7, 7i, 7Hi) |
| MRW | z2-headerless | minolta-packed | verified | DiMAGE Z2 headerless dump (identified by size and tail) |
| NEF | 12bit-compressed | nikon-huffman (lossy, lossless, split trees, curves) | verified | 108/108 corpus files decode (D1X..Z 6_2, Nikon 1); truncated files decode with missing data zero-filled |
| NEF | 12bit-uncompressed | packed MSB / LSB / padded rows / D100 pad bytes / E-series fields | verified | 31/31 decode |
| NEF | 14bit-compressed | nikon-huffman (lossy, lossless, split trees, curves) | verified | 84/84 decode (D3..Z 9, Z6III, Z50II, Z5II lossless) |
| NEF | 14bit-uncompressed | 16-bit words / Z+D6 14-bit LSB padded rows | verified | 22/22 decode |
| NEF | coolscan | uncompressed RGB 8/16-bit | verified | 3 samples/pixel |
| NEF | high-efficiency | TicoRAW (JPEG XS based) | unsupported | detected; decode returns UnsupportedCompression (proprietary codec) |
| NEF | sNEF | YCbCr 4:2:2 → RGB | verified | converted to RGB, 3 samples/pixel |
| NRW | 12bit-uncompressed | packed Msb32 / 16-bit words | verified | 12/12 decode (Coolpix P/B/A1000) |
| ORF | 12bit-packed | uncompressed | verified | 12-bit in 32-bit LE words and MSB-first bytes |
| ORF | 12bit-packed-interlaced | uncompressed | verified | C-/SP-series even/odd row fields |
| ORF | 12bit-packed-padded | uncompressed | verified | E-300/E-330/E-500 and E-M5 II / PEN-F high-res (pad byte per 10 samples) |
| ORF | C-740UZ/C-770UZ headerless | other | unsupported | size-identified dumps without TIFF structure |
| ORF | compressed | olympus | verified | 90 corpus files incl. 14-bit and 50/80 MP high-res; entropy/prediction pipelined on two threads |
| ORF | compressed (non-standard coder tags) | olympus | experimental | generic coder path, unit-tested against a bit-at-a-time scalar decoder only |
| ORF | uncompressed | uncompressed | verified | E-1, E-10/E-20 (big-endian, 10-bit), E-400 |
| ORI | compressed | olympus | verified | ORI base frames of high-res shots (same decoder) |
| PEF | compressed | pentax-huffman (makernote 0x220 table / default table) | verified | 34/34 corpus files decode (K10D..K-3 III, 645D/645Z, K-1, monochrome) |
| PEF | pixel-shift | pentax-huffman, first frame | partial | first of the four frames is decoded; frame merging is not provided |
| PEF | uncompressed | 12-bit packed (32773/1), 16-bit words | verified | 8/8 decode (*ist D family, K100D, K110D, Samsung GX-1L) |
| RAF | 12-bit compressed | fuji-compressed | experimental | Same code path as 14/16-bit; no corpus sample |
| RAF | 12-bit packed LSB | uncompressed | verified | EXR bodies, X10/X-S1/XF1, X100, X-Pro1/X-E1/X-M1/X-A1/X-A2, X20/X30/XQ1/XQ2; corpus files decode |
| RAF | 12-bit packed MSB32 | uncompressed | verified | HS10, S1, SL1000; corpus files decode |
| RAF | 14-bit packed MSB32 | uncompressed | verified | X-A3/A5/A7/A10, X-T100/T200, XF10; 6 files decode |
| RAF | Bayer lossless compressed (GFX) | fuji-compressed | verified | 14/16-bit GFX 50/100 series; 13 files decode |
| RAF | Bayer lossy compressed (GFX) | fuji-compressed | verified | 14/16-bit; 9 files decode |
| RAF | DBP for GX680 tiled | uncompressed | verified | 8 vertical big-endian tiles; 1 file decodes; reported unrotated as Bayer BGGR |
| RAF | SuperCCD 16-bit words | uncompressed | partial | S2/S3/S5 Pro, S5000-S9600, S6000/S6500fd, S100FS, E550/E900: 14 files decode; stored (unrotated) buffer with true 2x4/4x2 CFA; no 45-degree rotation applied |
| RAF | SuperCCD SR dual frame (F700/S20Pro) | uncompressed | partial | Main frame decoded (1 file); second (dark) frame not exposed |
| RAF | X-Trans lossless compressed | fuji-compressed | verified | 14-bit X-Trans III-V; 25 corpus files decode, regression-hashed; stripes decoded in parallel; truncation/corruption tests |
| RAF | X-Trans lossy compressed | fuji-compressed | verified | Per-line quantiser + flat-area tables; 13 files decode |
| RAF | second frame | uncompressed | unsupported | Dual-exposure SuperCCD SR second frame not exposed; the main frame is decoded |
| RAF | uncompressed 16-bit words | uncompressed | verified | X-Trans/GFX uncompressed and older Bayer bodies; 44 files decode |
| RAW | 12MP / KAI-0340 dumps | uncompressed | experimental | size-identified; no sample |
| RAW | C330/C603 CFA dumps | uncompressed | verified | 8-bit through stored or gamma curve |
| RAW | C330/C603 YRGB dumps | kodak-ycbcr | verified | YCbCr converted to RGB (cpp 3) |
| RAW | Canon CHDK headerless CRW (CHDK sizes, e.g. IXY 220F) | uncompressed packed | verified | decodes |
| RAW | Canon CHDK headerless CRW (file-size table sizes) | packed 10/12-bit | verified | 26 corpus files decode; black from optical-black columns |
| RAW | Canon PowerShot G7 X CHDK | packed 12-bit | experimental | no verified sample; geometry/black from the camera's CHDK DNG |
| RAW | DC20/DC25/DCS200 dumps | - | unsupported | no sample |
| RAW | GITUP GIT2/GIT2P, SJCAM M20/SJ6, Xiaomi Yi (16-bit) | uncompressed 16-bit | verified | decodes |
| RAW | Gitup G3 Duo 24000000 bytes (same size as a Samsung WB550 entry) | uncompressed 16-bit | verified | decodes |
| RAW | ImBack ImB35mm | uncompressed 8-bit | verified | decodes |
| RAW | Kodak C330 YRGB (4:2:2, optional 32-row gaps) | kodak-ycbcr | verified | decodes |
| RAW | Kodak C603/C643 CFAI/CCDI 8-bit | uncompressed 8-bit + curve | verified | file curve or a fixed gamma curve |
| RAW | Kodak C603/C643 YRGB | kodak-ycbcr | verified | decodes |
| RAW | Paralenz Dive Camera / + | uncompressed 16-bit | verified | decodes |
| RAW | Pentax Optio S4 | packed 12-bit | verified | decodes |
| RAW | SJCAM SJ6 LEGEND 12-bit (18325296 / 15972480 bytes) | uncompressed LSB packed | verified | decodes |
| RAW | SJCam 5000X Elite ("Kodak 12MP" size) | kodak-ycbcr | verified | YCbCr 4:2:0 → RGB (cpp 3) |
| RAW | Xiro Xplorer V | uncompressed LSB packed 10-bit | verified | decodes |
| RAW | other file-size table entries (AVT, Agfa, Android dumps, Casio, Nikon, Sony, ...) | per table | experimental | same loaders; no corpus sample |
| RW2 | RawFormat 4 (4:3/3:2/16:9/1:1) | panasonic-v4 | verified | 341 RW2 + 20 Leica RWL decode, regression-hashed; packet-parallel decoding; truncation/corruption tests |
| RW2 | RawFormat 5 (12/14-bit) | panasonic-v5 | verified | GH5S, G9 high-res; 10 files decode |
| RW2 | RawFormat 6 (12/14-bit) | panasonic-v6 | verified | S1/S1R/S1H/S5, GH5 II; 8 files decode |
| RW2 | RawFormat 7 (14-bit) | panasonic-v7 | verified | S1R/S5 high-res; 3 files decode; 12-bit variant implemented, no sample |
| RW2 | RawFormat 8 (12/14/16-bit) | panasonic-v8 | verified | GH6/GH7/G9 II/S5 II/S9/S1 II/S1R II...; 35 files decode; strips decoded in parallel |
| RW2 | RawFormat 8 high-res tile grid | panasonic-v8 | partial | 2-D strip grids of pixel-shift shots; 1 file decoded, seams verified continuous |
| RW2 | unversioned (0x111 payload) | panasonic-v4/uncompressed | experimental | Early layouts (packed stream without block split, or 16-bit words >> 4); no corpus sample |
| RWL | Leica (D-LUX, V-LUX, C-Lux, C Typ 112) | panasonic-v4 | verified | Same container; Leica models aliased to their Panasonic twins for colour/black tables; 20 files decode |
| SR2 | uncompressed | uncompressed | verified | DSC-R1 big-endian samples; SR2Private decryption for black/WB |
| SRF | srf | uncompressed | verified | DSC-F828 (RGBE CFA, measured black); DSC-V3 implemented, no sample |
| SRW | compressed | samsung-v0 (32770 + row offsets) | verified | 5/5 corpus files decode (NX300/NX300M/NX30/NX2000/EK-GN120) |
| SRW | compressed | samsung-v1 (32772) | verified | 3/3 decode (NX mini, NX3000, NX3300) |
| SRW | compressed | samsung-v2 (32773) | verified | 5/5 decode (NX1, NX500 12/14-bit) |
| SRW | uncompressed | 12-bit packed MSB/LSB (32770), 16-bit words (32769) | verified | 14/14 decode (NX5/10/11/100/20/200/210/1000/1100, EX1, EX2F, WB2000) |
| STI | Sinar 16-bit | uncompressed | verified | Sinarback eVolution 75 sample decodes |
| STI | Sinar four-shot | - | unsupported | no sample |
| TIF | EOS D2000C | lossless-jpeg | verified | Kodak-built: linearisation curve, Kodak-IFD black/WB; 1 file decodes |
| TIF | EOS-1D/1Ds raw | lossless-jpeg | verified | Raw located via maker note 0x0081; 2 files decode |
| TIFF | TIFF/EP CFA from unknown makes | uncompressed / lossless JPEG | experimental | standards-based fallback; no corpus sample reaches it with a CFA pattern |
| X3F | 1/1e TRUE II Merrill (DP1/2/3 Merrill, SD1 Merrill) | sigma-x3f | verified | corpus files decode; planes decode in parallel |
| X3F | 1/23 TRUE III Quattro (dp0..dp3 Quattro) | sigma-x3f | verified | 3-sample layout incl. AF-pixel repair and 2x2 replication of the lower layers |
| X3F | 1/25 TRUE III sd Quattro | sigma-x3f | verified | as above |
| X3F | 1/27 TRUE III sd Quattro H | sigma-x3f | verified | as above |
| X3F | 1/29 TRUE III sd Quattro H (APS-C crop) | sigma-x3f | experimental | same code path as 1/27; no corpus sample |
| X3F | 3/1e TRUE I/II (DP1, DP1s, SD15) | sigma-x3f | verified | corpus files decode; planes decode in parallel |
| X3F | 3/5 uncompressed 10-bit words (Polaroid x530) | sigma-x3f | verified | R/B swapped to RGB order |
| X3F | 3/6 10-bit Huffman (SD9, SD10, SD14) | sigma-x3f | verified | rows decode in parallel; negative running sums offset |
| X3F | CAMF calibration | - | unsupported | not parsed: not needed for the buffer, levels or white balance |
| X3F | Huffman thumbnail (2/b), Quattro thumbnail (2/19) | - | unsupported | JPEG and RGB thumbnails are exposed as previews |
| X3F | half/2-3 size raws (SD14/SD15/DP/Merrill/Quattro) | sigma-x3f | experimental | geometry from the camera database; no corpus sample |
<!-- END GENERATED SUPPORT TABLE -->
