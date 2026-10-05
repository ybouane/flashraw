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

### Performance
- 64-bit bit pumps, LUT Huffman with fully-decoded short diffs, SIMD packed unpack
  (SSE4.1/NEON/SIMD128), tile/strip/stripe/packet/row parallelism, calloc-backed
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

### Camera database
- Single JSON database (`camera-db/cameras.json`): 1,624 camera entries, 814 fallback
  matrices, 71 model-alias groups, 25 size rules.
- Entries are checked against FlashRaw's decodes of the corpus; 24 cameras come from corpus files.
- Orientation policies per make (`orientation_rules`) and per camera (`orientation`: policy
  override, Canon CameraInfo orientation / Auto Rotate offsets for 25 EOS bodies).

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
- Python bindings (`crates/flashraw-python`, the `flashraw` package; PyO3 + maturin, abi3
  wheels for CPython 3.9+ on Linux, macOS and Windows): `flashraw.open` (paths are
  memory-mapped, `bytes` borrowed without a copy, file objects), typed frozen-dataclass
  metadata, previews, `RawFile.decode` returning NumPy arrays that own the decoder's buffer
  (no copy), GIL released while decoding (per-thread pools for explicit `threads`), a
  `FlashRawError` hierarchy with structured truncation details, `imread` and `to_pil`
  helpers, type stubs; pytest suite with corpus equivalence against the CLI and a binding
  overhead benchmark (see `docs/python.md`).

### API
- Semver-ready surface: growable enums and result/metadata structs are
  `#[non_exhaustive]`; `RawMetadata::new`, `PreviewInfo::new`, `Limits::with_*`
  builders; `decode_preview_rgb` returns a `PreviewImage`; `FromStr`/`Display` for
  `Backend`, `OutputFormat` and `Demosaic` (`ParseOptionError`); internals (`low_level`, `process`, `decode_raw_image`) are
  hidden from the documented API; the `RawFile` alias is gone (use `FlashRaw`).

### Quality
- Regression corpus, malformed-input suite, 10 fuzz targets, criterion benchmarks,
  benchmark runner with regression detection, CI for Linux/macOS/Windows/ARM64.
