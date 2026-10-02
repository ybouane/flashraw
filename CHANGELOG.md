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
  metadata check (with documented differences).
- Region and tile decoding for tiled/row-addressable formats; honest
  `RandomAccess` reporting.
- Zero-copy embedded preview/thumbnail access.

### Performance
- 64-bit bit pumps, LUT Huffman with fully-decoded short diffs, SIMD packed unpack
  (SSE4.1/NEON/SIMD128), tile/strip/stripe/packet/row parallelism, calloc-backed
  output buffers, fused single-pass RGB development with orientation.

### Platforms
- `flashraw` CLI: info, decode, preview, thumbnail, benchmark, verify,
  dump-metadata, supported, camera-db tooling.
- Optional wgpu/WebGPU backend with GPU-resident outputs and a measured Auto policy.
- WASM bindings and the `flashraw` npm package (browser, Node, workers, TypeScript).

### Quality
  malformed-input suite, 10 fuzz targets, criterion benchmarks, comparative
  benchmark runner with regression detection, CI for Linux/macOS/Windows/ARM64.
