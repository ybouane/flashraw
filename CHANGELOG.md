# Changelog

## 0.3.0 (unreleased)

- The decoders, the codecs and the camera database were rewritten from format descriptions and sample files. The
  camera database holds values measured from sample files or taken from Adobe DNG profiles and the files' own
  metadata. Decoded output is unchanged except where the entries below say otherwise.

- Hasselblad: without a camera-database row, the colour matrix is the maker note's CCD profile of the file (then
  ColorMatrix1). The H4D-40 FFF sample now uses its own profile instead of a "40-Coated-FFF" row of no known
  source (it had a hand-edited 1.0 on the diagonal); that row and the generic "40-Coated" row are removed. No other
  Hasselblad sample of the corpus changes.
- Panasonic: the Lumix DC-TX2 (the Japanese name of the TZ200/ZS200) and DC-ZS220D are aliases of the DC-ZS200 again;
  a missing comma had joined them into one name, so neither matched and the TX2 had no colour matrix.
- Panasonic: a body's own camera-database colour matrix wins over the colour table row, and a table row matches only
  at a model-name boundary (`DC-S1` is no longer the row of the `DC-S1M2`, nor `DC-S1R` of the `DC-S1RM2`). On the
  corpus the Lumix S1M2, S1M2ES and S1RM2 now use their own matrices instead of the first-generation bodies'; the
  FZ8, FZ18, FZ30, FZ50, L10, LX1, LX2 and their Leica twins move by at most 1/10000 per coefficient to their
  entries' values. Levels are unchanged.
- EXIF and XMP as bytes, for EXIF libraries that read tags `metadata` does not (issue #4): `FlashRaw::exif_bytes()`
  (`flashraw exif FILE OUT`, Python `raw.exif_bytes`, JavaScript `await raw.exifBytes()`, C `flashraw_file_exif_bytes`, C++
  `file::exif_bytes()`) returns a standalone TIFF stream (`II*\0` or `MM\0*`, a JPEG APP1 payload after `Exif\0\0`) with
  IFD0, the EXIF, GPS and interoperability directories and the maker note; `FlashRaw::xmp_bytes()` (`flashraw xmp`,
  `raw.xmp_bytes`, `await raw.xmpBytes()`, `flashraw_file_xmp_bytes`, `file::xmp_bytes()`) the XMP packet. TIFF-based files and CR3 are
  rebuilt from their directories without the pointers to image data; the maker note keeps its offset from the TIFF
  header, so the vendor offsets inside it stay valid. RAF and X3F return the EXIF segment of their embedded JPEG.
  On 116 corpus files of 31 extensions, 102 have EXIF and 33 XMP; `kamadak-exif` reads all 102 streams. For the 90 whose original an independent parser
  (`exifread`) reads, 5,402 of its 5,409 IFD0, EXIF, GPS, interoperability and maker note tags read the same from
  the stream; the other 7 are an empty tag that is left out and misreads of that parser (Olympus headers, one Apple
  offset).
- The camera-database mode names changed to FlashRaw's own descriptive names. They are visible as the `mode` entry of
  the metadata extras (`extra("mode")` in Rust, `metadata.extra["mode"]` in Python and JavaScript, `mode` in
  `flashraw info --json` and `dump-metadata`) and in `cameras.json`; which database entry a file uses is unchanged
  (checked on 608 corpus files and on synthetic files of every headerless size). Old → new: Nikon `12bit-compressed`
  → `12bit-huffman`, `14bit-compressed` → `14bit-huffman`, `12bit-uncompressed` → `12bit-plain`,
  `14bit-uncompressed` → `14bit-plain`, `sNEF` → `small-ycbcr`, `coolscan` → `film-scan`, `sNEF-uncompressed` →
  `small-ycbcr-plain`, `7424x4924-14bit-uncompressed` → `7424x4924-14bit-plain`; Canon CR2 `sRaw1` → `medium-raw`,
  `sRaw2` → `small-raw`; `compressed` → `golomb-rice` (Fujifilm), `huffman` (Pentax), `entropy-coded` (Samsung),
  `adaptive-rice` (Olympus); `dng` → `dng-container`; Panasonic `4:3`, `3:2`, `16:9`, `1:1` → `aspect-4x3`,
  `aspect-3x2`, `aspect-16x9`, `aspect-1x1` (the `aspect` extra keeps the ratio); Samsung `12bit` → `depth-12bit`;
  headerless `chdk`, `chdk-a` … `chdk-d` → `dump-bytes-<size>`, `dump-words-<le|be>-<size>` or
  `dump-packed-<bits>bit-<lsb|msb|msb16|msb32>-<size>`, named after the sample layout and file size of the entry
  (`camera-db validate` checks the name against the hints). `naked-<size>` and `x3f-<w>x<h>` are unchanged. The
  `flashraw supported` rows for NEF and CR2 use the new names.
- Canon IXY 220F headerless dumps: black level 128 from the optical-black columns (it was 0) and the image area
  measured from a sample file (4678×3498 at 98,18; it was 4676×3493 at 100,22).
- Black and white levels set in decoder code are kept only where a file states them or a sample shows them. Changed
  on samples: Fujifilm GFX, XF10 and S100FS white 15872 → 16382/16383 (where they clip); Fujifilm SuperCCD bodies
  their measured saturation (S5000 15866, E550 15873, S6500fd 15895, S9000/S9500 15896); Coolpix P7x00 NRW
  at ISO 400 and above black 255 → 0 (the data is already black-subtracted); Raspberry Pi IMX219 black 66 → 64
  (the sensor's 10-bit pedestal; OV5647 stays 16). Unmeasured Olympus E-300/E-500/E-330, Hasselblad, Samsung NX1/NX500
  and Kodak DCS660M rules are removed.

## 0.2.0 — 2026-11-11

RGB outputs change: the default demosaic is now `Demosaic::Auto` (AHD on Bayer, edge-aware on X-Trans); pass
`bilinear` for the 0.1 output and speed. Raw outputs are unchanged.

- Edge-aware on X-Trans (the default there): green at red and blue sites is re-estimated from the measured sample
  and the cleaned colour difference (fully on clean frames, less on noisy ones; CPU and GPU). The filtered green at
  those sites was softer than at green sites, a texture tied to the 6x6 pattern that made thin lines look
  stair-stepped. Real-file speckle index over 10 X-Trans files 1.84 -> 0.90, fine detail
  0.0254 -> 0.0292; issue #3's metric on DSCF0120.RAF 1.83 -> 0.84. The 2x2-binned ground-truth tiles score it lower on
  clean mosaics (42.00 / 39.37 -> 41.14 / 38.53 dB), unchanged under noise. Output changes.
- The default demosaic of RGB outputs is now `Demosaic::Auto` in Rust, the command line (`--demosaic auto`), Python,
  JavaScript and C/C++ (`FLASHRAW_DEMOSAIC_AUTO` from `flashraw_decode_options_init`): AHD on Bayer sensors, edge-aware
  on X-Trans, bilinear on other patterns; an explicit `noise_reduction` level selects edge-aware on Bayer too. RGB
  output changes and develops two to four times slower than bilinear; pass `bilinear` for the old output and speed.
  Raw outputs are unchanged. `decode_gpu_async` resolves `Auto` too (it took it for bilinear). The benchmark scripts
  pin `--demosaic bilinear` for the published RAW-to-RGB comparison and measure the defaults as a separate mode
  (`rgb-default`, with per-stage times for both libraries).
- AHD runs on the GPU backend (it was CPU only), so the default demosaic can use the GPU on Bayer files. GPU rounding
  can change AHD's threshold decisions: on Metal 0.35 % to 0.75 % of the 8-bit samples of the test frames differ from
  the CPU (`docs/design/ahd-demosaic.md`).
- Faster develop, same output bit for bit: AHD and edge-aware hand their bands to colour conversion and encoding in
  column chunks (no full-width band buffers, which ran out of memory bandwidth on 8 to 16 threads), edge-aware works
  on 240-column chunks, AHD computes each neighbour step once, the X-Trans median shares column sorts, half-size reads
  the CFA colours once per row, 8-bit encoding and the white balance and colour step of demosaiced chunks vectorise.
  45 MP Nikon Z8 to RGBA8 with AHD on a Ryzen 7 9700X: develop 649 -> 554 ms on one thread, 175 -> 97 ms on 16; 40 MP
  X-Trans with edge-aware 1,005 -> 896 ms and 205 -> 142 ms; half-size 218 -> 86 ms.
- Faster decoders, same output: Panasonic v5/v7 (Lumix S5 18.6 -> 5.8 ms on one thread), Kodak 65000 (85.9 -> 68.9 ms),
  16-bit little-endian uncompressed data copied as bytes and, on macOS, by one thread (Nikon D850 uncompressed on an
  Apple M1 Ultra, all cores: up to 13.7 -> 3.4 ms).
- `Demosaic::Auto` (`auto`, `FLASHRAW_DEMOSAIC_AUTO`): the best validated algorithm for the sensor, chosen when the file is
  decoded (now AHD on Bayer, edge-aware on X-Trans, bilinear on other patterns; see above). The algorithm behind it may change
  between minor versions. `Demosaic::Ppg` (`ppg`, `FLASHRAW_DEMOSAIC_PPG`) and `Demosaic::Rcd` (`rcd`, `FLASHRAW_DEMOSAIC_RCD`,
  experimental): two more Bayer demosaics, CPU only; on a sensor that is not Bayer they fail with `Error::InvalidArgument` for
  RGB outputs. Ground-truth numbers and the experiments that were not kept: `docs/design/bayer-demosaics.md`,
  `docs/design/demosaic-roadmap.md`. The edge-aware GPU build compiles again (the dense filter tables the GPU kernels read had
  been dropped by the register-tiled CPU rewrite).
- `Demosaic::Ahd` blends its two candidates by their homogeneity instead of keeping the one with the larger count:
  closer to ground truth at every noise level (clean 41.51 to 41.76 dB, noisy +0.7 to +0.8 dB) and fewer false-colour
  dots on real files (speckle index 1.62 to 1.44). Output changes.
- Edge-aware on X-Trans uses filters trained with a 5x5 support (level with the 7x7 ones on held-out tiles, fewer
  multiplications). Output changes on X-Trans files.
- Edge-aware on X-Trans follows the 3x3 median of the colour differences with the luminance-gated second pass of the
  Bayer stage (CPU and GPU): on held-out tiles 42.00 / 39.37 dB clean (was 41.92 / 39.20) and better under noise, and
  the real-file colour speckle index falls from 2.18 to 1.84, the lowest of the X-Trans demosaics measured
  (`docs/benchmarks.md`). Output changes on X-Trans files.
- Faster output encoding (`rgb16` 10 %, `linear-rgb16` 8 %, `linear-rgb-f16` 21 % on a bilinear develop, output
  unchanged) and edge-aware chroma passes (7 % on x86-64, 11 % on an Apple M1, output unchanged). On aarch64 the
  edge-aware filters use fused multiply-adds (another 9 %), so aarch64 results differ from x86-64 by rounding.
- Faster: `Ppg` and `Rcd` vectorise (interior output unchanged; the 4-pixel border ring is now developed by them
  instead of bilinear), and the black-level normalisation, the 16-bit encode loop, the AHD green loop and the bilinear
  path of X-Trans and other patterns do less work per pixel (output unchanged).
- `Demosaic::Ahd` (`ahd` in the CLI, Python, JavaScript and C, `FLASHRAW_DEMOSAIC_AHD`): homogeneity-directed
  interpolation (Hirakawa and Parks) for Bayer sensors, written from the paper; other sensors use the edge-aware
  demosaic where it exists. On ground-truth tiles it scores within 0.05 dB of a widely used implementation of the
  method (`docs/benchmarks.md`), and on real files its colour speckle index is 1.62 (edge-aware 2.09; the blend above followed). CPU only (the GPU backend decodes
  on the CPU for it); bit-identical for any thread count, band or crop. The compat layer maps `user_qual` 3 to it.
  Method and numbers: `docs/design/ahd-demosaic.md`, `docs/benchmarks.md`.
- `Highlights::Reconstruct` caps its prediction at 1.5 times the clipped value of the channel: the global model could
  overshoot where a clipped area is unlike the rest of the frame (a simulation on 26 real crops: worst case against
  plain clipping +0.067 to +0.010 in log error, mean 0.0517 to 0.0484, plain clipping 0.0561).

- Noise level and more white balance controls. `DecodeOptions::noise_reduction` (`--noise-reduction`, `noise_reduction=`,
  `noiseReduction`, `flashraw_decode_options.noise_reduction`): `auto` (the default, as before), `off`, `low`, `medium`,
  `high` set the smoothing of `Demosaic::EdgeAware` (levels are floors on the weight estimated from the frame's noise).
  `WhiteBalanceMode::Daylight` (D65 from the file's colour matrix) and `DecodeOptions::white_balance_multipliers`
  (`--wb-multipliers R,G,B`, `white_balance_multipliers=`, `whiteBalanceMultipliers`,
  `flashraw_decode_options.white_balance_multipliers`; all zero = unset). Defaults are unchanged. CPU only (the GPU
  backend decodes on the CPU for them). `flashraw_decode_options` grows by 12 bytes (`reserved2` became
  `noise_reduction`). The compat layer maps `fbdd_noiserd` 1 and 2 to `low` and `high`.

- DNG camera profiles, opt-in (`DecodeOptions::camera_profile`, `--camera-profile FILE.dcp|embedded`,
  `camera_profile=` in Python, `cameraProfile` in JavaScript, `camera_profile` / `camera_profile_embedded` in
  `flashraw_decode_options`): the RGB outputs of three-colour sensors are rendered with a `.dcp` file or with the
  profile embedded in the DNG (`FlashRaw::embedded_camera_profile`): forward matrices (or the colour matrices)
  interpolated by the colour temperature of the neutral, then the hue/sat map, look table and tone curve, as the DNG
  specification describes. New types `CameraProfile` (`from_dcp`, `to_dcp`), `ProfileTable`, `TableEncoding`.
  The default develop is unchanged. CPU only (the GPU backend decodes on the CPU for it).
  `flashraw_decode_options` grows by 24 bytes on 64-bit targets. Not compared against another implementation:
  see `docs/color-management.md`.
- `Demosaic::EdgeAware` (`edge-aware` in the CLI, Python, JavaScript and C): an opt-in demosaic for Bayer and
  X-Trans sensors with directionally weighted 7x7 filters and a luminance-gated colour-difference stage (3x3 median on X-Trans) and, on noisy frames, a gated smoothing. Far fewer zipper
  patterns and isolated coloured dots than bilinear (the X-Trans speckle of issue #3 goes from 4.29 to 2.25 on
  the chroma index of the report), 3.7x to 5.5x the develop time of bilinear. Bilinear stays the default. Method, training data and numbers:
  `docs/design/edge-aware-demosaic.md`; tables are generated by `tools/demosaic/edgeaware_train.py`.
- The GPU backend runs `Demosaic::EdgeAware` for Bayer and X-Trans (`Backend::Gpu`, `Backend::Auto` and the
  GPU-resident outputs): the new
  `edge.wgsl` passes (mosaic, `G0`, direction weights and 7x7 filters, chroma stage, smoothing) follow the CPU
  operation by operation and agree with it within f32 rounding (max 1.3e-6 on synthetic scenes, one code of 65535
  on real files; tests in `crates/flashraw-gpu/tests/edge_aware.rs`). On an Apple M1 Ultra the GPU develops an 18 MP
  Bayer frame in 34 ms against 253 ms on 20 CPU threads, and a 40 MP X-Trans frame in 55 ms against 357 ms. Bilinear
  keeps its fused kernel. `flashraw_gpu::DevelopSpec` has a new `edge` field (`EdgeSpec`), and `policy::Workload` a new
  `edge_aware` field. `Highlights::Neutral`/`Blend` and `WhiteBalanceMode::Auto` still decode on the CPU.
- `Highlights::Neutral` (`--highlights neutral`, `highlights="neutral"`, `highlights: 'neutral'`,
  `flashraw_decode_options.highlights`): pixels with a channel at or near sensor saturation fade smoothly to
  white instead of keeping the colour cast of a partly clipped pixel (magenta, yellow). `Clip`, the old
  behaviour, stays the default. CPU only (the GPU backend decodes on the CPU for it).
- `DecodeOptions::chromatic_aberration` (`--chromatic-aberration`, `chromatic_aberration=True`, `chromaticAberration: true`, `flashraw_decode_options.chromatic_aberration`):
  corrects the lateral chromatic aberration of 16-bit Bayer frames. The radial scale of the red and blue planes against green is
  estimated from the frame (tile-wise correlation of the Laplacians of half-resolution planes, robust fit) and the red and blue
  samples are resampled before demosaicing; frames without enough edges, X-Trans and other sensors are left as they are. CPU only.
- `Highlights::Reconstruct` (`--highlights reconstruct`, `"reconstruct"`, `FLASHRAW_HIGHLIGHTS_RECONSTRUCT`): as `Blend`, and clipped channels are
  rebuilt from the unclipped ones with a model fitted on the unclipped 6x6 blocks of the frame; only pixels clipped in every
  channel fade to white. Works when the highlights resemble the rest of the scene; frames it cannot fit behave as `Blend`.
- `WhiteBalanceMode::Auto` (`--white-balance auto`, `white_balance="auto"`, `whiteBalance: 'auto'`, `FLASHRAW_WHITE_BALANCE_AUTO`): a
  white balance that refines the camera's over the near-neutral areas of the frame, opt-in; the camera's balance stays the default. CPU only (the
  GPU backend decodes on the CPU for it). `flashraw_decode_options` grows by 8 bytes (`white_balance`, a reserved word).
- `Highlights::Blend` (`--highlights blend`, `highlights="blend"`, `highlights: 'blend'`, `FLASHRAW_HIGHLIGHTS_BLEND`): as
  `Neutral`, and pixels that exceed the white level after white balance are scaled down together instead of clamped
  channel by channel, so a bright red stays red where `Clip` turns it pink. Nothing changes below the white level.

## 0.1.1 — 2026-10-25

Package pages only; no code changes.

- READMEs of every crate, the npm package and the Python package: banner, a short description of FlashRaw,
  the official website (flashraw.ybouane.com), the example projects LightFlow and ImageFlow, and the author's X account.
- Registry metadata: homepage and project links point to the website (npm, PyPI).

## 0.1.0 — 2026-10-25

First complete release of FlashRaw.

### Colour management
- Output colour spaces for the RGB outputs: sRGB (default, unchanged output), Display P3,
  Adobe RGB (1998), ProPhoto RGB and Rec. 2020 (`DecodeOptions::color_space`, `--color-space`,
  `color_space=` in Python, `colorSpace` in JS, `flashraw_decode_options.color_space` in C).
  The output matrix is folded into the camera matrix once, so there is no per-pixel cost;
  scalar, SIMD, GPU, region/tile and DNG-opcode paths agree.
- Standard ICC v4 profiles for every space, generated by code (`ColorSpace::icc_profile`,
  `icc_profile_linear`), returned with each frame (`RawFrame::icc_profile`, `frame.icc_profile`,
  `frame.iccProfile`, `flashraw_frame_icc_profile`) and embedded by the CLI in TIFF (tag 34675)
  and JPEG (APP2) files.
- Arbitrary ICC output profiles with the optional `icc` cargo feature (on in the CLI, C library,
  Python wheels and WASM builds): `DecodeOptions::output_profile`, `--output-profile`,
  `--intent`, `--bpc`. Matrix/TRC profiles cost nothing extra; profiles with lookup tables run a
  second parallel pass. Invalid profiles return `InvalidArgument`; a new fuzz target covers the
  parser. See `docs/color-management.md`.

### Decoding
- Formats: DNG (uncompressed, lossless JPEG, deflate int/float, lossy JPEG, JPEG XL,
  GoPro VC-5, Blackmagic DCT), CR2 (incl. sRAW/mRAW), CR3 (CRX lossless and C-RAW),
  CRW, NEF/NRW (all Huffman trees, packed and unpacked variants, sNEF), ARW/SR2/SRF/ARQ
  (ARW1, ARW2, ARW6, lossless tiles, lossless S/M YCbCr, SR2/SRF decryption), RAF
  (uncompressed, packed, lossless and lossy compressed X-Trans/Bayer, SuperCCD),
  RW2/RWL (Panasonic v4–v8, early packed `.RAW`), ORF/ORI (Olympus compression, packed
  variants, high-res), PEF, SRW (V0/V1/V2), MRW, IIQ (formats 0/3/5/6/8), MOS,
  3FR/FFF, KDC/DCR/DCS/Kodak RAW, ERF, MEF, Sinar, X3F (TRUE I/II/III, Quattro, old
  Huffman), headerless/CHDK raws identified by size, ARRI ARI.
- 1,980 of the 2,016 files of the raw.pixls.us CC0 corpus decode (98.2%); full-buffer hashes of 1,965
  are pinned by the regression suite. The other 36 are valid files in formats not supported yet (28
  Nikon High Efficiency NEF, Light L16, Casio CAM, JPEG-in-DNG, a few more; docs/formats.md).
- Region and tile decoding for tiled/row-addressable formats; honest
  `RandomAccess` reporting.
- Zero-copy embedded preview/thumbnail access.
- DNG opcode lists applied by default, each in its specified stage (List 1 on stored
  values, List 2 on normalised active-area values, List 3 on demosaiced camera RGB):
  GainMap, MapTable, MapPolynomial, per-row/column delta and scale, bad-pixel repair
  and FixVignetteRadial; geometric opcodes reported. `apply_opcodes` / `--no-opcodes`
  / `applyOpcodes` turn them off; `DecodeStatistics::opcodes` reports what ran.

- Editor-facing output options: `normalized` as half floats (`sampleType: 'f16'`, converted
  in place in WASM memory) and raw outputs cropped to the active area (`crop: 'active'`).
- Region decodes of formats whose black level is measured from masked (optical black) areas
  now measure it from those areas even when the region excludes them (previously the region's
  own pixels were averaged, e.g. CR2 active-area crops normalised against ~980 instead of 512).
- Metadata: `metadata.dng` (BaselineExposure, BaselineNoise/Sharpness, LinearResponseLimit,
  AsShotNeutral, AsShotWhiteXY, DefaultUserCrop, DefaultScale, NoiseProfile, camera-profile
  tag presence: ProfileName, HueSatMap dims/count, LookTable dims, ToneCurve, EmbedPolicy,
  BaselineExposureOffset) and `metadata.opcodes` (every DNG opcode with id, flags, whether it
  is applied, and WarpRectilinear / FixVignetteRadial parameters).
- Orientation from vendor records: `RawMetadata::orientation_info` (`orientationInfo` in JS,
  `orientation_info` in `flashraw info/verify --json`) keeps the standard TIFF/EXIF tag, the
  vendor record and the camera's Auto Rotate setting next to the resolved value, its source
  and the per-camera policy. Canon CameraInfo orientation sensor and Auto Rotate setting
  (EXIF Normal stays authoritative unless the tag is missing/invalid or Auto Rotate was on),
  rotation saved by Nikon's software in the NEF edit record (D70, E5700 samples now open
  rotated), Kodak EasyShare orientation from the camera's embedded JPEG (Z980). Invalid
  Orientation values (0) are reported as missing instead of Normal. Preview orientation is
  resolved per preview for these formats: a preview's own EXIF orientation is used, and
  previews already rendered rotated are not rotated again.

### Truncated and corrupt sensor data
- A strip, tile or other sensor-data range running past the end of the file is never
  silently zero-filled (previously e.g. uncompressed DNG strips were). Ranges are checked
  before decoding (overflow, offset past EOF, range past EOF, data too short for the
  geometry) in every format: uncompressed and packed (8–16-bit) strips, tiles, planes and
  multi-sample data.
- `DecodeOptions::corruption_policy`: `CorruptionPolicy::Recover` (default) decodes every
  complete sample, zero-fills the rest and reports it in `RawFrame::warnings()`
  (`DecodeWarning`: unit, bytes expected/available, samples filled) with
  `RawFrame::is_incomplete()`; `CorruptionPolicy::Strict` fails with `Error::CorruptFile`
  carrying a structured `CorruptReason::Truncated(Truncation)` (unit and index, offset,
  expected and available bytes, file length; `Error::truncation()`), e.g. "truncated
  image data: strip 17 expected 131072 bytes at offset 24576000, but only 48291 bytes
  remain before EOF". `Error::kind()` is unchanged.
- Recovery also covers compressed DNG strips/tiles: a unit that is truncated or fails to
  decode is zero-filled with a warning. Other compressed streams fail under both
  policies; failures over a declared range past EOF report the truncation details.
- Tolerances for complete data: the last row needs no stride padding; declared lengths
  beyond the last needed sample may run past EOF; geometry-addressed payloads may read
  past an understated byte count when the file holds the bytes.
- Opening a file, its metadata and previews no longer fail because the payload is
  truncated or starts past EOF; only decodes that need the missing data report it.
- CLI: `--strict` on `decode`/`verify`; JSON output has `incomplete` and `warnings`, and
  errors carry `corrupt` (the structured cause). JS: `corruptionPolicy: 'recover' |
  'strict'`, `frame.incomplete`, `frame.warnings`, `CorruptFileError.corrupt`.
- Nikon and Pentax Huffman and Samsung SRW (V0 row streams, V1, V2) payloads cut by the end
  of the file are truncation: `Recover` keeps the rows decoded from real data, zero-fills the
  rest and warns, `Strict` fails with the structured `Truncated` cause. Truncated PEF and SRW
  files previously decoded the zero padding as image data without any warning, and a NEF
  missing only its last bytes decoded its last row from padding.
- A truncated file never decodes a thumbnail or preview as its sensor image. Raw IFD
  selection skips reduced-resolution (NewSubFileType 1) and 8-bit RGB/YCbCr images in NEF,
  PEF, SRW, ARW/SR2, ORF and MOS files (a NEF cut before its raw SubIFD decoded as a
  160x120 black frame: its RGB thumbnail taken for a film-scanner image). When the file
  ends inside its directory structure, before the raw image's IFD, every TIFF-based format
  fails with `CorruptFile` naming the missing directory instead of `InvalidMetadata` or a
  wrong image. `crates/flashraw/tests/truncated_files.rs` cuts corpus files of every format
  at fractions of the file and around the start and end of the sensor data.

### Fixes
- Sigma / Foveon X3F renders are no longer deep green: the CAMF calibration block (codings
  2/4/5, decoded on demand) provides the as-shot white balance (preset gains, temperature,
  aperture and sensor-adjustment factors) and an XYZ → camera matrix for every generation
  (SD9 … SD14, DP1 … Merrill, Quattro), reported in `FrameInfo` (`ColorSource::File`). Classic
  and Merrill bodies measure their per-layer black from the dark-shield rows (the database
  value was off by up to 54 levels, e.g. SD14, DP2/SD1 Merrill). Mean R/G of the corpus
  renders went from 0.004–0.9 (sd Quattro H 4.7) to within 0.2 of the embedded previews.
  Raw buffers are unchanged.
- Canon EOS R6 Mark III and PowerShot V1 CR3 renders are no longer magenta: their
  3778-word ColorData (sub-version 64 and later, shared with the R50 V) keeps the R5 Mark
  II / R1 word offsets, so the black level is read as 512 instead of from a tone-curve table
  (0/46/100/137 per channel). Mean R/G and B/G of the R6 Mark III renders went from
  1.55–1.58 / 1.14–1.15 to within 0.06 of the embedded previews (0.90–0.93 / 0.82–0.87). The
  R6 Mark III also gets its own colour matrix instead of the EOS R6 one. Raw buffers are
  unchanged.
- Olympus ORF no longer traps in the multithreaded WASM build ("failed to spawn thread"):
  the pipelined entropy/prediction stages use `rayon::join` on wasm32 instead of
  `std::thread::scope`.
- Truncated Nikon Huffman NEFs fail with `CorruptFile` instead of decoding with frozen
  rows (a stream may only run dry within its last row, for unflushed encoders).
- DNGs with AsShotWhiteXY but no AsShotNeutral (recent Pixel phones, several converters
  and scanners) get their as-shot white balance: the camera neutral of that chromaticity
  through the colour matrices interpolated for its temperature (including a third
  calibration described by IlluminantData). D75 maps to 7500 K instead of 6500 K.
- Floating-point DNGs written as big-endian TIFFs with little-endian 32-bit samples and a
  WhiteLevel LONG holding the bits of 1.0f (one HDR-merge writer) decode instead of
  producing noise: the samples are read little-endian (when they are plausible that way)
  and white is 1.0.
- `flashraw verify` reports the white level of the decoded frame, in the same units as the
  black level (floating-point DNGs reported the file's white next to a rescaled black).
- Samsung NX20/NX200/NX210/NX1000/NX1100 SRWs stored as unpacked 16-bit words (header
  BitsPerSample 14, no row table) are read as little-endian words with 12 significant
  bits and white 4095 (previously big-endian with white 16383).
- Sony ARWs from pre-production firmware whose IFD0 Model is a placeholder ("MODEL-NAME",
  or a model whose digits are all zeros) are identified by the maker-note model id (tag
  0xB001): camera-database entry, colour matrix and visible area of the production body
  (the IFD0 string is kept as the `ifd0_model` extra).
- DNGs with a staggered CFA (CFALayout 2-5, e.g. converted Fujifilm S2Pro files) decode:
  samples as stored with the stored-grid CFA (`OtherCfa`), like native SuperCCD buffers,
  and an extra `cfa_layout`. Layouts 6-9 fail with `CameraUnsupported` instead of
  `UnsupportedCompression`.
- Canonical model names for cameras without a database entry: the make prefix is removed
  only at a word boundary (whitespace, or a hyphen/underscore followed by a letter), so
  "LG-H815" gives "H815" instead of "-H815" and "OM-1" stays whole; a trailing Kodak
  "FILE VERSION n" signature and whitespace padding are dropped ("DCS460B").
- JPEG XL DNG samples are scaled to the DNG's BitsPerSample instead of the codestream's
  bit depth (a 12-bit codestream in a 16-bit DNG decoded 16x too dark), and JPEG XL tiles
  with RowInterleaveFactor/ColumnInterleaveFactor are de-interleaved (they decoded as four
  quadrant sub-images).
- RGB renders of four-colour sensors (CMYG: Canon PowerShot G1/Pro70/Pro90 IS, Nikon
  Coolpix E5700; RGBE: Sony DSC-F828) are neutral instead of saturated red or green: all
  four channels are demosaiced, white balanced per channel and converted with the
  pseudo-inverse of the 4-row colour matrix. Files without an as-shot balance use
  daylight derived from that matrix; the Coolpix red/blue levels are converted to channel
  multipliers. `CfaPattern::four_colour_order` documents the channel order of 4-row
  matrices and of their white balance. GPU backends leave these to the CPU.

- Sony lossless YCbCr S/M ARW and Nikon sNEF renders are no longer magenta: the camera
  white-balances these images before its YCbCr conversion, and the develop applied the
  as-shot balance a second time (ILCE-7RM5 Medium centre 146/86/86 vs 81/99/49 for Large,
  now 85/103/52; D810 sNEF 189/92/104, now 122/109/91 like the full-size frame).
  `FrameInfo::white_balance_applied` / `RawFrame::white_balance_applied` mark them (JS
  `whiteBalanceApplied`, Python `white_balance_applied`, C `flashraw_file_white_balance_applied`
  / `flashraw_frame_white_balance_applied`, CLI JSON), `white_balance` keeps the as-shot
  multipliers, and RGB outputs develop with unit multipliers. The compatibility layer sets
  `as_shot_wb_applied`. Raw buffers are unchanged.
- Nikon default crops come from the maker note's CropArea where the camera records it:
  unchanged for full-size files, and the camera's image area for the Bayer RAW M/S files of
  the D500, D6, D850 and Z bodies (D850 RAW S 6216×4136 → 6192×4128). These keep more
  photosites than the camera's M/S JPEG has pixels (about 8/7 for M, 3/2 for S).
- Fujifilm SuperCCD RAF RGB outputs have the camera's aspect ratio and orientation instead of
  a 2x squeeze: the stored 45-degree lattice (every other line offset by half a photosite) is
  developed, then resampled to square pixels (e.g. E900 3462x2608, S3 Pro 3009x2014, S2 Pro
  3032x2036). All 15 corpus and 19 real-world SuperCCD renders match their embedded JPEGs in
  aspect (within 0.8%, before 2x off) and orientation. Raw outputs keep the stored grid.
  Staggered DNGs (CFALayout 2-5) get the same resampling with their DefaultScale (the 10
  real-world FinePix S2Pro DNGs now match their thumbnails' 2:3), and rectangular grids of
  non-square photosites (`FrameInfo::pixel_aspect`, e.g. the Kodak DC120) are stretched to
  square pixels (DC120: 848x976 -> 1301x976, 4:3).
- X3F RGB outputs: clipped highlights render white instead of yellow-green (SD1 Merrill):
  Foveon white-balance multipliers are scaled so the smallest is 1 (the top layer's is about
  0.6), so saturated samples stay saturated in every layer. Renders gain about +0.7 EV.
- X3F lens shading: the CAMF spatial gain tables (classic SD9-SD15 and x530, DP1/DP2 per
  preset, DP Merrill per aperture, SD1 Merrill per aperture and focal length) and the Merrill
  colour shading factors are applied to RGB outputs as an OpcodeList2 gain map; raw buffers
  are unchanged. The green/cyan cast towards the frame edges of Merrill renders is mostly
  gone: corner-to-centre R/G and B/G error against the embedded previews (mean |log|) 0.34 /
  0.33 -> 0.14 / 0.12 on Merrill bodies, 0.21 / 0.19 -> 0.11 / 0.12 over all X3F files.
  Quattro bodies are unchanged.
- X3F exposure at high ISO: the camera amplifies above `SensorISO` digitally, so the white
  level is lowered by `CaptureISO / SensorISO`. Renders of ISO 800 / 6400 shots were 3 / 6 EV
  darker than the previews; all 46 corpus and real-world X3F files now render within
  -2.4..+0.1 EV of their previews (before: -7.1..-0.1).
- Phase One iXM-100, iXM-RS150F, iXU180 and iXU-RS1000 IIQs and the Leaf AFi-II 12 MOS
  render with a colour matrix (they had none, so renders were desaturated to 0.3-0.7 of the
  preview's saturation): where the camera database has no matrix, the file's own
  camera → ROMM matrix (IIQ tag 0x0106, MOS `CaptProf_color_matrix`) is converted to
  XYZ(D65) → camera (`ColorSource::File`). Mean R/G and B/G now follow the embedded
  previews (e.g. iXU-RS1000 0.91/0.90 → 0.72/0.82, preview 0.66/0.70). The conversion
  reproduces the database matrix of the IQ3 100MP from that back's own files.
- SJCam 5000X Elite `.RAW` files (18,000,000 bytes, the size of the Kodak "12MP" YCbCr
  dump) decode as 4000x3000 12-bit packed RGGB instead of striped YCbCr: the two layouts
  are told apart by the data (read as 12-bit pairs, YCbCr bytes make every first sample
  jump by multiples of 256). Raw buffers of these two corpus files change.

### Performance
- 64-bit bit pumps, LUT Huffman with fully-decoded short diffs, SIMD packed unpack
  (SSSE3/SSE4.1, NEON, SIMD128), tile/strip/stripe/packet/row parallelism, calloc-backed
  output buffers, fused single-pass RGB development with orientation.
- Speculative parallel decoding of single sequential entropy streams (Huffman
  self-synchronisation; anything speculation cannot prove exact falls back to the
  sequential decoder): CR2 lossless JPEG and sRAW, single-strip lossless DNG, Kodak
  DCS, Hasselblad 3FR/FFF, Nikon and Pentax predictive Huffman, Panasonic v8 strips,
  Sony ARW1.
- Lossless JPEG scans with restart markers (Samsung Galaxy DNGs) decode one restart
  interval per task: a 12.5 MP Galaxy S24 DNG drops from 282 to 26 ms (M1 Ultra).
- OpcodeList1 made of MapTable and bad-pixel repairs (Leica, LG, CHDK, Lumia DNGs) runs on
  the stored u16 samples instead of an f32 copy, with a block-wise search for the bad-pixel
  constant: Leica Q2 opcode stage 8.6 → 1.0 ms.
- Lossless JPEG: byte unstuffing in parallel (it was up to 40% of a parallel decode),
  predictors 2-7 rebuilt as a band/block wavefront instead of row by row (DJI 51 → 16 ms,
  Raspberry Pi 33 → 10 ms), and frames parallel from 1M samples instead of 4M (Canon
  EOS-1D TIF 27 → 3.6 ms).
- Samsung V0 (NX30/NX300/NX2000): residuals parsed straight into the output and
  reconstruction run as a wavefront, 58 → 11 ms for 20.6 MP.
- Fuji compressed: branch-free predictions, pass-local decoder state (bit pump in
  registers, bounds-check-free line windows and level tables, packed statistics with
  precomputed Rice parameters); Samsung V2: staged block decoding.
- Floating-point DNG: fused predictor and float assembly, zlib-rs inflate,
  parallel float → integer conversion.
- DNG OpcodeList2/OpcodeList3 develop streams through the fused pass (rows normalised with
  list 2 as the demosaic reads them, list 3 per band of camera RGB) instead of full-frame f32
  copies, with identical output: DJI Phantom 4 Pro RAW → RGBA8 peak memory 692 → 175 MB and
  34 → 20 ms (M1 Ultra), 48 MP DJI 1.7 GB → 450 MB; phone DNGs with list 2 gain maps 18 → 12 ms.
- `threads: 1` decodes on the calling thread (no one-worker pool), like `RAYON_NUM_THREADS=1`;
  explicit pools run their workers at the user-initiated QoS on Apple platforms.

### Camera database
- Single JSON database (`crates/flashraw-camera-db/data/cameras.json`): 1,624 camera entries, 814 fallback
  matrices, 71 model-alias groups, 25 size rules.
- Entries are checked against FlashRaw's decodes of the corpus; 24 cameras come from corpus files.
- Orientation policies per make (`orientation_rules`) and per camera (`orientation`: policy
  override, Canon CameraInfo orientation / Auto Rotate offsets for 25 EOS bodies).
- Default crops are the camera's own image area: formats without a crop field of their own
  now crop to the image size the camera produced (EXIF pixel dimensions, Sony FullImageSize,
  Minolta PRD image size, else the embedded full-size JPEG), per file, so aspect-ratio and
  DX/APS-C modes follow the file; Canon uses SensorInfo's valid-image rectangle (now
  inclusive of its last row/column) narrowed by AspectInfo. Examples: D850 8288×5520 →
  8256×5504, Z 9 8280×5520 → 8256×5504, Z 6 6064×4040 → 6048×4024, EOS R6 5496×3670 →
  5472×3648, EOS R5 8191×5463 → 8192×5464, EOS 250D 1:1 → 4000×4000. RGB outputs and
  `metadata.width/height` follow; DNG keeps its DefaultCrop. `RawMetadata::extra` reports
  `image_size` and `crop_source`.
- `image_crops` per camera (877 entries, 1,026 rules): the camera's image area per
  active-area size, measured from 6,337 corpus and real-world files, with positions located
  by matching the camera's JPEG against the raw data (`flashraw camera-db crops --register`,
  `tools/camera_db_crops.py`); the fallback for files that record no size.

### Platforms
- `flashraw` CLI: info, decode, preview, thumbnail, benchmark, verify,
  dump-metadata, supported, camera-db tooling.
- Optional wgpu/WebGPU backend with GPU-resident outputs and a measured Auto policy;
  frames beyond the adapter's buffer limits are processed in bands (pipelined
  uploads, banded or tiled outputs).
- WASM bindings and the `flashraw` npm package (browser, Node, workers, TypeScript):
  multithreaded build (shared-memory rayon pool, chosen automatically when available),
  opt-in WebGPU build with GPU-resident frames; worker pool with a shared request
  queue and `AbortSignal` cancellation.
- JPEG XL DNGs in the npm package: the codec ships as separate variants of the SIMD and
  threaded builds (`flashraw_simd_jxl.wasm`, `flashraw_simd_threads_jxl.wasm`, ~950 KiB
  brotli vs ~615 KiB), so the default download is unchanged. `init({ jpegxl: 'auto' })`
  (default) loads the matching variant once, on the first decode of a JPEG XL DNG, and moves
  that file to it (browsers, workers, threads, Node, bundlers); `'always'` loads it up front,
  `'never'` reports `UnsupportedCompressionError`. Frames report the binary that decoded
  them (`backend.binary`, `backend.jpegxl`); `getCapabilities().jpegxl`, `loadJpegXlSupport()`,
  package exports `flashraw/wasm/simd-jxl` and `flashraw/wasm/threads-jxl`.
- Python bindings (`crates/flashraw-python`, the `flashraw` package; PyO3 + maturin, abi3
  wheels for CPython 3.9+ on Linux, macOS and Windows): `flashraw.open` (paths are
  memory-mapped, `bytes` borrowed without a copy, file objects), typed frozen-dataclass
  metadata, previews, `RawFile.decode` returning NumPy arrays that own the decoder's buffer
  (no copy), GIL released while decoding (per-thread pools for explicit `threads`), a
  `FlashRawError` hierarchy with structured truncation details, `imread` and `to_pil`
  helpers, type stubs; pytest suite with corpus equivalence against the CLI and a binding
  overhead benchmark (see `docs/python.md`).
- C API (`crates/flashraw-capi`): stable C ABI with a cbindgen-generated `flashraw.h`
  (opaque file/frame handles, open from path/borrowed/copied memory, metadata and
  calibration getters, zero-copy previews, every output format, regions, threads,
  corruption policy, frame warnings and timings, a JSON escape hatch for full metadata,
  status codes matching the error kinds with a thread-local last error, caught panics,
  size-versioned option structs), a header-only C++17 wrapper `flashraw.hpp` (RAII
  handles, exception per error kind, span views), CMake install with
  `find_package(flashraw)` and pkg-config, C and C++ examples (built by the local CI on
  Linux and macOS). See `docs/c-api.md` for the ABI stability policy.
- `crates/flashraw-libraw-compat`: a source-compatible implementation of a documented
  subset of a widely used RAW library's C API (and the common members of its C++ class) on
  top of FlashRaw, so existing clients can switch by recompiling and relinking; see
  `docs/libraw-compat.md`.

### API
- New outputs: `Rgb16` (display-ready 16-bit sRGB, orientation applied), `LinearRgbF16`
  (linear RGB as IEEE half floats, `RawPixelData::F16`: half the memory of `LinearRgbF32`, e.g.
  1.24 GB → 620 MB for a 103 MP frame) and `LinearRgb16` (linear RGB as 16-bit integers), all
  from the fused develop pass and on the GPU path. Display outputs apply the orientation
  (`OutputFormat::applies_orientation`), linear outputs keep the sensor's. Names `rgb16`,
  `linear-rgb-f16`, `linear-rgb16` in the CLI, JS (`Uint16Array`; `sampleType: 'f16'` also
  applies to `linear-rgb`), Python (`uint16`, `float16`) and the C API
  (`FLASHRAW_OUTPUT_RGB16`, `_LINEAR_RGB16`, `_LINEAR_RGB_F16`, `FLASHRAW_SAMPLE_F16`).
- `DecodeOptions::exposure` (EV), `black_point` and `white_point`: convenience levels for RGB
  outputs, applied to linear values after the colour conversion (`--exposure`,
  `--black-point`, `--white-point`; `exposure`/`blackPoint`/`whitePoint` in JS; keyword
  arguments in Python; appended fields of `flashraw_decode_options`).
- `flashraw benchmark --json` reports `maxrss_kb` in KiB on every platform (it was bytes on
  macOS).
- Semver-ready surface: growable enums and result/metadata structs are
  `#[non_exhaustive]`; `RawMetadata::new`, `PreviewInfo::new`, `Limits::with_*`
  builders; `decode_preview_rgb` returns a `PreviewImage`; `FromStr`/`Display` for
  `Backend`, `OutputFormat` and `Demosaic` (`ParseOptionError`); internals (`low_level`, `process`, `decode_raw_image`) are
  hidden from the documented API; the `RawFile` alias is gone (use `FlashRaw`).
- Explicit preview fallback for sensor data FlashRaw cannot decode (Nikon High Efficiency
  NEF): `decode` fails with `UnsupportedCompression` by default, also for RGB outputs; with
  `DecodeOptions::preview_fallback` the `Rgba8` / `LinearRgbF32` outputs return the embedded
  full-size preview (at least 90% of the default crop; orientation applied for `Rgba8`),
  marked `RawFrame::source == FrameSource::Preview` with a `CorruptReason::PreviewFallback`
  warning. Raw outputs and region decodes never fall back. `CorruptReason::kind()` names
  every reason.
- CLI: `decode --preview-fallback`; the decode JSON has `source` (`sensor` / `preview`) and
  warnings of kind `preview-fallback`.
- JS: `decode({ previewFallback: true })`, `frame.source` (`FrameSource`), warning kind
  `preview-fallback`.
- Python: `decode(..., preview_fallback=True)`, `Frame.source`, warning kind
  `preview-fallback`.
- C API: `flashraw_decode_options.preview_fallback` (appended with a zero `reserved0`; the
  struct grows to 72 bytes, older 64-byte callers keep the default), `flashraw_frame_source()`
  (`FLASHRAW_SOURCE_SENSOR` / `_PREVIEW`), frame JSON `source`; C++
  `decode_options::preview_fallback` and `frame::from_preview()`.
- Error messages share one style in every crate and binding: lower case at the start (bar
  proper nouns, acronyms and format names), no trailing period, `truncated X` / `invalid X` /
  `missing X` wording, code in backticks and caller values quoted; `UnsupportedCompression`
  reads "unsupported compression in DNG: ..." and `CameraUnsupported` "unsupported camera:
  ...". A source scan and the malformed-input and truncation suites enforce it. Error kinds,
  codes and structured fields are unchanged.

### Quality
- Regression corpus, malformed-input suite, 10 fuzz targets, criterion benchmarks,
  benchmark runner with regression detection, a local CI (`tools/ci/local.sh`, Linux and macOS;
  the GitHub workflows run on manual dispatch only).
- Comparative benchmarks against other open-source RAW decoders on Linux x86-64 and
  Apple M1 Ultra (`tools/bench/compare.py`, `benches/popular-cameras.tsv`,
  `benches/results/*-compare.*`, `docs/benchmarks.md`); README rewritten around speed,
  security and support.
