# Changelog

## 0.1.0 — unreleased

First complete release of FlashRaw.

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
- All 1,980 decodable files of the 2,016-file CC0 corpus (every make and model on
  raw.pixls.us) decode; full-buffer hashes of 1,965 are pinned by the regression suite.
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

### Fixes
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
- JPEG XL DNG samples are scaled to the DNG's BitsPerSample instead of the codestream's
  bit depth (a 12-bit codestream in a 16-bit DNG decoded 16x too dark).

### Performance
- 64-bit bit pumps, LUT Huffman with fully-decoded short diffs, SIMD packed unpack
  (SSE4.1/NEON/SIMD128), tile/strip/stripe/packet/row parallelism, calloc-backed
  output buffers, fused single-pass RGB development with orientation.
- Speculative parallel decoding of single sequential entropy streams (Huffman
  self-synchronisation; anything speculation cannot prove exact falls back to the
  sequential decoder): CR2 lossless JPEG and sRAW, single-strip lossless DNG, Kodak
  DCS, Hasselblad 3FR/FFF, Nikon and Pentax predictive Huffman, Panasonic v8 strips,
  Sony ARW1.
- Fuji compressed: branch-free predictions, pass-local decoder state (bit pump in
  registers, bounds-check-free line windows and level tables, packed statistics with
  precomputed Rice parameters); Samsung V2: staged block decoding.
- Floating-point DNG: fused predictor and float assembly, zlib-rs inflate,
  parallel float → integer conversion.

### Camera database
- Single JSON database (`camera-db/cameras.json`): 1,624 camera entries, 814 fallback
  matrices, 71 model-alias groups, 25 size rules.
- Entries checked against FlashRaw's decodes of the corpus; 44 colour-matrix and 13
  white-level corrections, 24 cameras added from the corpus.

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

### API
- Semver-ready surface: growable enums and result/metadata structs are
  `#[non_exhaustive]`; `RawMetadata::new`, `PreviewInfo::new`, `Limits::with_*`
  builders; `decode_preview_rgb` returns a `PreviewImage`; `FromStr`/`Display` for
  `Backend`, `OutputFormat` and `Demosaic` (`ParseOptionError`); internals (`low_level`, `process`, `decode_raw_image`) are
  hidden from the documented API; the `RawFile` alias is gone (use `FlashRaw`).

### Quality
- Regression corpus, malformed-input suite, 10 fuzz targets, criterion benchmarks,
  benchmark runner with regression detection, CI for Linux/macOS/Windows/ARM64.
