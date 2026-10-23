# Sigma / Foveon X3F

Module: `crates/flashraw-formats/src/sigma/` (`camf.rs`: calibration block); codec: `crates/flashraw-codecs/src/x3f.rs`.
Sigma DNG files (fp, fp L, and DNGs written by Sigma Photo Pro) go through the DNG decoder.

## Container

| Part | Content |
|---|---|
| `FOVb` header | version (2.x/3.x classic, 4.x Quattro), rotation (0/90/180/270 → EXIF orientation) |
| directory | `SECd` block at the offset stored in the last 4 bytes; ≤ 50 entries of (offset, size, type) |
| `SECp` | UTF-16 property list: `CAMMANUF`, `CAMMODEL`, `ISO`, `SHUTTER`, `APERTURE`, `FLENGTH`, `FLEQ35MM`, `LENSMODEL`, `LENSFRANGE`, `LENSARANGE`, `TIME`, `CAMSERIAL`, `FIRMVERS`, `WB_DESC` |
| `SECi` | images: raw (see below), JPEG thumbnail (`2/12`), plain RGB thumbnail (`2/3`); Huffman thumbnails (`2/b`) and Quattro `2/19` thumbnails are not exposed |
| `SECc` | CAMF calibration block: white balance, colour matrix and dark-shield areas (see [Colour](#colour)) |

Quattro files have no property list: the make is `SIGMA` and the model is derived from
`SIGMA dp<n>` / `sd Quattro (H)` strings in the first 2 KiB, falling back to the raster size.

## Raw image types

| type/format | Cameras (corpus) | Coding |
|---|---|---|
| 3/5 | Polaroid x530 | uncompressed: one LE u32 per pixel holding three 10-bit indices into a 1024-entry difference table; running sums per colour |
| 3/6 | SD9, SD10, SD14 | 10-bit Huffman: explicit (non-canonical) codes mapped through the difference table; per-row offsets → rows decode in parallel; 16-bit signed running sums, globally offset when negative |
| 3/1e | DP1, DP1s, SD15 | TRUE I/II |
| 1/1e | DP1/2/3 Merrill, SD1 Merrill | TRUE II |
| 1/23, 1/25, 1/27, 1/29 | dp0/dp2 Quattro, sd Quattro, sd Quattro H | TRUE III with per-plane geometry |

TRUE: three independent planes (bottom, middle, top) at 16-byte aligned offsets, one shared prefix
code whose symbol is the bit length of a JPEG-style difference; prediction per 2x2 phase (first two
columns from two rows above, others from two columns left), seeds from the header. The planes decode
concurrently (3 threads); each plane is a strictly sequential stream.

## Output representation

`width x height x 3` u16 (`SensorLayout::Foveon`, cpp 3):

- classic/Merrill: the three full-resolution layers interleaved (channel 0 = bottom … 2 = top);
- Quattro (1:1:4): channel 2 = full-resolution top layer; channels 0/1 = half-resolution bottom/middle
  layers placed on even 2x2 sites, then two post-steps:
  the phase-detect AF pixel repair (dp Quattro and sd Quattro patterns, float arithmetic) and
  the replication of the lower layers over each 2x2 block (block rows `2..h/2-2`, block columns
  `0..w/2-4`, borders stay zero);
- Polaroid x530: channels swapped to R,G,B order.

## Metadata

Margins (active area) and white level come from the camera database (entries
`mode = "x3f-<width>x<height>"` in `crates/flashraw-camera-db/data/cameras.json`), the fallback black level from the
camera database's colour table (prefix-matched). Unknown bodies/sizes still decode with full-frame
geometry, the colour-table black and white 0x3fff.

## Colour

The `SECc` block (CAMF) holds the camera calibration as a sequence of `CMb` entries: text (`CMbT`),
property lists (`CMbP`, name → value strings) and matrices (`CMbM`: element type, dimensions, data).
It is coded in one of three ways (`flashraw_codecs::x3f::CamfStream`): type 2 (SD9–SD14) XORs the
bytes with a pseudo-random stream; type 4 (DP1–Merrill) is a TRUE-style prefix code over 12-bit
samples (2x2-phase prediction, two samples per three bytes); type 5 (Quattro) the same code over a
running byte sum. Blocks reach several MiB (spatial gain and defect tables), so they are decoded on
demand: lookups stop once the entry is found or the `IncludeBlocks` list shows it is absent.

- **White-balance preset**: the `WB_DESC` property (falling back to the header's 32-byte name at
  offset 40, versions 2.1–3.x), or the CAMF `WhiteBalance` index (Quattro: 1 Auto, 2 Sunlight,
  3 Shade, 4 Overcast, 5 Incandescent, 6 Fluorescent, 7 Flash, 8 Custom, 12 AutoLSP). `Daylight`
  names the `Sunlight` preset; unknown presets use `Auto`.
- **DP1 … Quattro**: gains from `WhiteBalanceGains` (`DP1_WhiteBalanceGains`) for the preset, times
  `TempGainFact`, `FNumberGainFact`, `SensorAdjustmentGainFact` and `DigitalISOGain` (sd Quattro H:
  the lower layers are stored at half the top layer's gain) when present; the preset's matrix in
  `WhiteBalanceColorCorrections` (`DP1_…`) maps balanced raw to linear sRGB.
- **SD9 … SD14, Polaroid x530**: `WhiteBalanceIlluminants` gives camera → XYZ and
  `WhiteBalanceCorrections` an XYZ correction; the gains are the reciprocal of `<preset>RGBNeutral`
  or, without it, of the camera response to D65.

`FrameInfo::white_balance` holds the gains (normalised to the middle channel) and
`color_matrices.color` the XYZ → camera matrix (source `File`, illuminant from the preset). Files
without a usable block keep the colour-table matrix and report no white balance.

**Black level**: classic and Merrill bodies record their optically shielded rows
(`DarkShieldTop`/`DarkShieldBottom`, outside `ActiveImageArea`) as `FrameInfo::masked_areas` with
`black_from_masked`, so the per-layer black is measured after decoding (the database value is the
fallback: actual levels vary per body and file, e.g. 16 vs 31 on DP2 Merrill, 69 vs 15 on SD14).
Quattro bodies keep the database level (their lower layers fill the shielded rows only partly).

**Lens shading** (RGB outputs; the raw buffer stays as stored): the CAMF gain tables become an
OpcodeList2 of three GainMaps (one per layer) in `FrameInfo::opcodes`, applied like a DNG's
(`apply_opcodes`). The grids span the whole raw buffer, corner point on corner pixel, and are
stored turned by 180 degrees relative to the decoded image.

- SD9 … SD15, Polaroid x530: `SpatialGain` (`[rows, cols, 3]` float gains).
- DP1/DP1s/DP2: the preset's `SpatialGainTables` entry (else `SpatialGain`).
- DP Merrill: `SpatialGain{R,G,B}_<i>_INF` (12-bit tables, gain `MinGains + Delta x value` from
  `SpatialGainsProps_<i>_INF`) for the apertures of `SpatialGain_Fstop`, interpolated over the
  `CaptureAperture` in stops; the preset's `WhiteBalanceColorShadingFactor` (2x2) scales the bottom
  and top layers by `1 + a r + b r^2` (`r`: distance from the centre over the half diagonal).
- SD1 Merrill: `SpatialGainsProps_<f-number>_<focal length>`, interpolated over the aperture and
  the focal length; none outside the calibrated focal lengths (other lenses).
- Quattro: not applied (their shading is described by polynomial colouring coefficients).

Measured against the embedded previews (corpus and real-world files), the corner-to-centre R/G and
B/G error of the renders (mean |log|) went from 0.21 / 0.19 to 0.11 / 0.12; on Merrill bodies from
0.34 / 0.33 to 0.14 / 0.12. Which tables apply and their 180-degree turn were chosen by that
measurement (the closest-focus `_MOD` tables made it worse and are not used).

**ISO**: above `SensorISO` the camera amplifies digitally and the raw samples stay at base gain.
The white level is lowered to `black + (white - black) x SensorISO / CaptureISO` (the point where
the camera's output saturates), so RGB outputs carry the shot's exposure (extra
`digital_iso_gain`); renders of ISO 800 / 6400 shots were 3 / 6 EV darker than at base ISO.

**Highlights**: Foveon balances scale a layer down (top layer ~0.6). RGB outputs scale the
multipliers so the smallest is 1, so a saturated pixel stays saturated in every layer and renders
white (the clipped light of the SD1 Merrill sample rendered yellow-green).

Not applied: colour-mode compensations and tone curves (renders are colorimetric).

## Verification

All 18 corpus X3F files decode and are covered by regression hashes (`tests/regression/sigma.tsv`).

Performance (16 threads, decode only, `flashraw benchmark`): Merrill 4928x3264x3 ≈ 52 ms,
sd Quattro H ≈ 97 ms, SD9/SD10 ≈ 5–15 ms.
TRUE planes use a fused 14-bit table (prefix code + difference bits in one probe); the Quattro top
plane (a single sequential stream of up to 30 M samples) bounds the wall time.
