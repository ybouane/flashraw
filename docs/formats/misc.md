# Long-tail formats (`misc`)

Module: `crates/flashraw-formats/src/misc/` — `naked.rs` (file-size identified dumps), `rpi.rs`
(Raspberry Pi), `arri.rs` (ARRI), `tiff_ep.rs` (TIFF/EP fallback for unknown makes).
Codecs: `crates/flashraw-codecs/src/misc_packed.rs`, `crates/flashraw-codecs/src/vc5.rs` (GoPro).

Dispatch (`flashraw-formats/src/lib.rs`): `misc::probe`/`open` for magic-identified files (ARRI,
Light LRI), `misc::open_tiff` for TIFFs whose make has no module, `misc::open_headerless` for files
without a TIFF header (Raspberry Pi JPEG+BRCM first, then the file-size table).

## Headerless ("naked") dumps — `naked.rs`


   layouts are omitted). Hints: `full_width`/`full_height` (stored raster), `offset` (or `reversed`
   `flip`, `zero_is_bad`, `monochrome`, `right_mask_trim`. The bit depth is computed from the size and

   | bits | loader |
   |---|---|
   | 6 | Minolta RD-175 three-CCD interleave (1534x986 GRBG) |
   | 16 | 16-bit words, big-endian if `flags & 1`, right shift `(flags >> 1) & 7` |

   entry is anchored at the (unadjusted) visible origin and re-anchored at the buffer origin; CHDK
   dumps (`flags & 32`) and 8-bit dumps measure black from the left/right optical-black columns
   test says so; Samsung WB550 words are little-endian; Kodak C330/C603/"12MP" YCbCr dumps decode to a
   `offset`, `bits`, `order` (`plain` = LSB, `jpeg` = MSB, `jpeg16`/default = MSB in 16-bit words,

PowerShot G7 X CHDK (experimental).

the SIMD kernels in `packed.rs`) and a bit-exact sequential reference used for the other flag
combinations; tests check both agree.

## Raspberry Pi — `rpi.rs`

`raspistill --raw` / picamera captures: JPEG + `BRCM` block at a mode-specific distance from the end
of the file (32 KiB header, then rows padded to 32 bytes). Header at `BRCM+0xB0`: width, height,
`RPi IMX219` (66), `RPi OV5647 v.1/v.2` (16), `RP_imx477` (256, 64 in 10-bit mode). White balance
wide, reading past the stride into the start of the row) is reproduced.

## ARRI — `arri.rs`

`ARRI\x12\x34\x56\x78`, payload offset (8), width (20), height (24), WB multipliers (100..112, f32),
GRBG, white 4095.

## TIFF/EP fallback — `tiff_ep.rs`

For TIFF files from makes without a module: the largest full-resolution IFD with
PhotometricInterpretation 32803, uncompressed (MSB-first packed rows or 16-bit words in file byte
order, strips contiguous or not) or lossless JPEG (one stream per strip), CFA from
`CFARepeatPatternDim`/`CFAPattern` (or EXIF `CFAPattern`), white from `BitsPerSample`; the camera
database refines it. Files without a CFA IFD or pattern are rejected, never guessed (the Leaf `.mos`
and Sinar `.sti` files of the corpus that reach this path because their makes are not dispatched are
declined cleanly and belong to the medium-format module).

## GoPro GPR (VC-5) — `flashraw-codecs/src/vc5.rs`

GPR files are DNGs with TIFF compression 9: one VC-5 (SMPTE ST 2073) bitstream per image. The codec
parses the big-endian tag stream, decodes the 40 coded bands in parallel (16-bit lowpass; run-length
VLC highpass with GoPro's table 17, decompanding and dequantisation), runs the 3-level inverse wavelet
per channel (prescale, 14-bit clamp at the finest level) and recombines G-sum / R−G / B−G / G-diff
into the Bayer mosaic through the inverse-log curve scaled to the DNG white level.

API: `vc5::decode(data, width, height, white_level, phase, out, parallel)` with
`vc5::CfaPhase::from_pattern(&CfaPattern)` for the phase. All 17 corpus GPR files are

**Integration hook for `dng.rs`** (owned by the DNG module; verified locally, not committed here):

```rust
enum Payload { /* … */ Vc5 }
struct DngRaw<'a> { /* … */ vc5_white: u32, vc5_phase: Option<flashraw_codecs::vc5::CfaPhase> }
// parse_raw(): compression match
9 => (Payload::Vc5, Compression::Other),
// DngRaw construction
vc5_white: info.white_level.max(),
vc5_phase: info.cfa.as_ref().and_then(flashraw_codecs::vc5::CfaPhase::from_pattern),
// decode_region(), before the tile loop: one bitstream covers the whole image
if self.payload == Payload::Vc5 {
    let mut img = RawImage::new_u16(self.width, self.height, 1, &ctx.limits)?;
    let phase = self.vc5_phase.ok_or_else(|| Error::unsupported_compression("DNG", "VC-5 without a Bayer CFA"))?;
    ctx.profiler.time(Stage::EntropyDecode, || flashraw_codecs::vc5::decode(self.unit_bytes(0)?,
        self.width as usize, self.height as usize, self.vc5_white, phase, img.u16_mut(), ctx.parallel))?;
    return if region == full { Ok(img) } else { img.crop(region) };
}
// decode_unit_u16(): Payload::Vc5 => Err(Error::corrupt("VC-5 is decoded as a whole image")),
```


|---|---|

## Unsupported (corpus)

- **Light L16 `.lri`**: multi-module container (16 camera modules, protobuf metadata, per-module
- **Casio QV-11 / QV-5000SX `.cam`**: Casio's processed CAM image format (not sensor data); no
- **Canon A620 `Portulaca_1216.CRW`**: the file is a bzip2 archive (`BZh9`), not a raw.
