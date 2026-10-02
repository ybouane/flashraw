# Pentax / Ricoh PEF (and Samsung GX-1/GX-10/GX-20)

Module: `crates/flashraw-formats/src/pentax.rs`, codec `flashraw_codecs::pentax_huffman`
(on the predictive Huffman engine in `nikon_huffman`).

PEF files are TIFF files with make `PENTAX`, `PENTAX Corporation` or `RICOH IMAGING
COMPANY, LTD.` (models `PENTAX …`). Samsung-branded Pentax bodies (`SAMSUNG TECHWIN`
GX-1L/GX-1S/GX-10/GX-20) write PEF with an `AOC` makernote and are forwarded here from
the Samsung module. Pentax/Ricoh/Samsung DNGs are handled by the DNG decoder.

## Payload variants

| variant | detection | codec |
|---|---|---|
| Pentax Huffman | TIFF compression 65535 | single predictive Huffman stream; JPEG-style difference categories; table from makernote `0x0220` (`(n + 12) & 15` codes, 12-bit left-aligned code values and lengths) or the default table |
| 12-bit packed, MSB first | compression 32773 (mislabelled PackBits) or 1 | `packed` (SIMD) |
| 16-bit words | `w*h*2` byte strips (*ist D) or 14/16 bps | `packed` |

Pixel-shift files (K-1, K-1 II, K-3 II, K-3 III, KP, K-70) store four frames; the first
K-3 Mark III Monochrome files (LinearRaw photometric) decode as monochrome.

## Metadata

| item | source |
|---|---|
| makernote | `AOC\0` + order + IFD (offsets relative to the file) or `PENTAX \0` + order + IFD (relative to the makernote) |
| black level | `0x0200` (RGGB per channel); reference colour-table black when present |
| white level | `(1 << bps) - 1`, reference colour-table maximum (645D: 15872) |
| white balance | `0x0201` (RGGB) |
| colour matrix | camera-database colour table (K2000 uses its K-m twin's entry), camera database otherwise |

## Verification

