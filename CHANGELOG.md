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
  opt-in WebGPU build with GPU-resident frames.

### API
- Semver-ready surface: growable enums and result/metadata structs are
  `#[non_exhaustive]`; `RawMetadata::new`, `PreviewInfo::new`, `Limits::with_*`
  builders; `FromStr`/`Display` for `Backend`, `OutputFormat` and `Demosaic`
  (`ParseOptionError`); internals (`low_level`, `process`, `decode_raw_image`) are
  hidden from the documented API; the `RawFile` alias is gone (use `FlashRaw`).

### Quality
- Regression corpus, malformed-input suite, 10 fuzz targets, criterion benchmarks,
  benchmark runner with regression detection, CI for Linux/macOS/Windows/ARM64.
