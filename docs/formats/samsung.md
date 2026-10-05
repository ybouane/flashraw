# Samsung SRW

Module: `crates/flashraw-formats/src/samsung.rs`, codecs `flashraw_codecs::samsung`
(V0, V1, V2) plus `nikon_packed`/`packed` for uncompressed data.

SRW files are TIFF files (big- or little-endian) with make `SAMSUNG`. The raw IFD
declares 3 samples per pixel although it holds CFA data. Samsung-branded Pentax PEFs
(GX series) are forwarded to the Pentax decoder; Samsung DNGs (Galaxy phones, GX DNG)
are handled by the DNG decoder.

## Compressions

| TIFF compression | name | cameras | layout |
|---|---|---|---|
| 32770 + tag 40976 | V0 | NX300, NX300M, NX30, NX2000, EK-GN120 (NX U family) | per-row offset table (tag 40976, little-endian); 16-sample blocks with a direction bit (predict from 1–2 rows above or from the block's left neighbours) and four adaptive bit lengths; MSB-first bits in LE 32-bit words; rows stored as transposed 2×2 pairs |
| 32772 | V1 | NX mini, NX3000, NX3300 | single predictive Huffman stream with a fixed 10-bit table |
| 32773 | V2 | NX1, NX500 | 128-bit header (bit depth, option flags, initial value); rows aligned to 16 bytes; 16-sample blocks with motion-compensated prediction from previous rows, per-colour adaptive bit lengths and a quantisation scale |
| 32770 without tag 40976 | packed | NX5/NX10/NX11/NX100 (MSB first), NX20/NX200/NX210/NX1000/NX1100 (LSB first) | 12-bit packed; NX20-family frames of 16-bit words (BitsPerSample 14 in the header) hold little-endian words with 12 significant bits |
| 32769 | uncompressed | EX1, WB2000 (14-bit LE words), EX2F (12-bit in LE words, shifted right by one) | 16-bit words |

## Metadata

| item | source |
|---|---|
| makernote | plain IFD, offsets relative to the makernote |
| white balance | `0xA021` (RGGB) minus the per-file keys of `0xA020` |
| black level | `0xA028` (RGGB) minus keys; NX5/NX10/NX11/NX100 measure the optical-black columns left and right of the visible area (integer per-colour means); NX1/NX500 default to `1 << (bps - 7)` |
| white level | `(1 << bps) - 1` or the camera-database colour-table maximum (EX1 15872, EX2F 2047) |
| visible area / CFA | per-model/size rules; CFA overrides re-anchored at the buffer origin |
| colour matrix | camera-database colour table, other camera-database entries otherwise |

## Verification

All 27 SRW files in the corpus (every compression above) decode and are covered by
regression hashes (`tests/regression/samsung.tsv`).
