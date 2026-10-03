# References

FlashRaw is an independent Rust implementation. Behaviour was established from the
format specifications below and from study of the files themselves: the CC0 corpus of
every make and model on raw.pixls.us, whose decodes are pinned by the regression suite
(see [testing.md](testing.md)).

| Document | Used for |
|---|---|
| Adobe DNG Specification 1.7.1 | ActiveArea/DefaultCrop anchoring, BlackLevelRepeatDim/Delta, LinearizationTable, floating-point predictor, JPEG XL (52546), opcode lists. |
| ITU T.81 | Lossless JPEG (process 14), Huffman tables, restart markers. |
| ISO/IEC 14496-12 | ISO BMFF container (CR3). |
| TIFF 6.0, TIFF/EP, EXIF 2.32 | Container structure, CFA tags, maker-note pointers. |

## Findings that shaped the design

- **Representation is a choice.** Several things a decoder must decide are not dictated
  by the data: which frame of a multi-frame DNG is primary (Pentax Pixel Shift: frame
  0), whether 3-channel linear data is stored with a 4th component, how float DNGs map
  to integers, whether per-model margins override a DNG's ActiveArea. FlashRaw follows
  the specification where it speaks, and documents its choice where it does not (see the
  vendor pages).
- **Malformed-but-common files exist.** DJI writes StripByteCounts smaller than the
  data; Blackmagic labels lossless streams SOF1 and also writes genuine 12-bit DCT
  tiles under compression 7; smartphones emit restart markers on every row; some
  files named `.dng` are plain JPEGs. Decoders accept these real-world variants, and
  nothing more.
- **The camera table is the long tail.** About 1,600 camera entries and 800 colour
  matrices encode two decades of quirks; keeping them as data (with verification
  records) makes them reviewable and replaceable.
- **Performance headroom is architectural.** Generic bit-reading loops and per-pixel
  copies cost ~10 ns/pixel even for uncompressed data; 64-bit pumps, SIMD unpack,
  tile-parallel and speculative parallel entropy decoding and fused post-processing
  remove most of that (see [benchmarks.md](benchmarks.md)).

Per-format notes (compression variants, maker-note layouts, black/white logic, CFA
anchoring, firmware differences) are in [docs/formats/](formats/).
