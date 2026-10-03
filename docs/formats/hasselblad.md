# Hasselblad 3FR / FFF

Module: `crates/flashraw-formats/src/hasselblad.rs`; codec:
`crates/flashraw-codecs/src/hasselblad.rs`.

Both are TIFFs with the CFA image in a SubIFD (photometric 32803):

* **Compression 7** – Hasselblad's lossless-JPEG variant: SOF3/DHT/SOS markers, but the
  scan is read MSB-first from 32-bit little-endian words without `FF 00` stuffing; pixels
  are coded in pairs (two length symbols, then two differences; a 16-bit all-ones code is
  -32768); prediction is from the same colour two columns left, rows start at 0x8000
  (+256 for H4D-60 3FR); predictor 11 adds half the gradient of the row two above. It is a
  single sequential stream (no restart markers or row index), decoded at ~155 MP/s on one
  core.
* **Compression 1** – little-endian 16-bit (X1D/X2D/CFV II/CFV 100C 3FR), unpacked in
  parallel and exposed as a `PackedPayload`.

Sony-made bodies sold as Hasselblad (Lunar, Stellar, HV — ARW compression 32767/32769/
32770) are handed to the Sony decoder.

## Metadata

* Maker note: sensor code (0x11), coating code (0x16), sensor unit connector (0x15) and
  colour matrix (0x2A) give the sensor family ("50-15-Coated5", "100-20-Coated6", ...).
  Container flavour: 3FR when the file has ColorMatrix1 only and no maker-note matrix,
  otherwise FFF. Family + flavour select the camera-database colour matrix ("50-15-Coated5-3FR").
* Uncropped sensor readouts get fixed borders per sensor code (H3D-39, H4D-40/
  H5D-40, H4D-50/H5D-50/CFV-50, H5D-50c/CFV-50c, X1D, H4D-60, H6D-100c, X2D/CFV 100C) and
  GRBG for the 39/40/50 MP Kodak sensors; odd margins are rounded to even.
* Black: BlackLevel tag (0xC61A) with the per-sensor overrides; X2D/CFV 100C carry
  DNG-style MaskedAreas, so black is measured from the masked pixels after decoding.
  White: WhiteLevel tag (0xC61D), 0xFFFF when the embedded Hasselblad TIFF (0xC51B) is
  present, 0x8101 for H4D-60 3FR. WB: AsShotNeutral.

## Unsupported

* Multi-shot FFF (H4D-200MS/H6D-400c MS 4/6-shot with several samples per pixel; the shots would have to be
  merged into a colour image): no sample — `UnsupportedCompression`. (The corpus
  H6D-400c file is a single-shot capture and decodes.)
* Imacon/Flexcolor FFF with Imacon tags 0xB4C3/0xB4C2 (Ixpress backs, 16-bit RGB or CFA
  data addressed through the Imacon tag block): no sample; files with Make "Imacon" are not
  dispatched to this module.
* DNG output of Phocus / L1D / L2D (handled by the DNG decoder).
