# Tone of the display outputs (`tone`)

The display outputs (`rgba8`, `rgb16`) are rendered with a standard tone by default (since 0.5.0): a brightness
offset per camera (the DNG `BaselineExposure` of the file, or for other formats the offset the camera database
measured for the model), then one S-curve shared by every camera. The module is `crates/flashraw/src/tone.rs`;
the GPU develop kernel does the same (`shaders/develop.wgsl`).

| Rust | CLI | Python | JavaScript | C |
|---|---|---|---|---|
| `DecodeOptions::tone = Tone::Standard` | `--tone standard` | `tone="standard"` | `tone: 'standard'` | `flashraw_decode_options.tone = FLASHRAW_TONE_STANDARD` |

`Tone::Linear` (`--tone linear`, `tone="linear"`, `tone: 'linear'`, `FLASHRAW_TONE_LINEAR`), the output before
0.5.0, applies no curve: the linear values, as exposed, go through the transfer curve of the output colour space
only. That rendering is colourimetric. Next to the renderings of cameras and raw developers it
looks dark and flat, because those add a brightness offset per camera model and a tone curve that lifts the mid-tones.
On the 26 files below, mid-tones are about 1 to 1.5 stops darker without the tone, and the brightness differs by a
median of 10.6 L* from a raw developer's default rendering. With `Tone::Standard` the difference is 0.9 L*.

The linear outputs (`linear-rgb*`) and the raw outputs ignore the option. A camera profile
(`DecodeOptions::camera_profile`) with its own tone curve takes precedence: `Tone::Standard` then changes nothing. A
profile without one gets the standard curve after its matrices and tables. The GPU develop kernel applies the same
table with the same arithmetic: RGBA8 within 3 codes of the CPU (more than 1 on at most 0.1 % of the samples) and
16-bit within 64 codes, where the curve is steep in the shadows.

## Method

1. **Exposure.** Linear values are multiplied by `2^offset`. For a DNG file the offset is its `BaselineExposure`
   (0 without the tag). Other formats do not carry one: they take the offset of their camera model from the
   camera database (`exposures`, [camera database](../camera-db.md#exposure-offsets)), else their make's default,
   else 0 (see "Offsets for other formats" below). `FlashRaw::tone_exposure()` and `flashraw info` report the
   offset a file gets. `DecodeOptions::exposure` and the black and white points come before the tone, so they move
   values along the curve.
2. **Curve.** One curve for every camera, defined by 15 control points in gamma 2.2 encoded values with monotone
   cubic interpolation (Fritsch and Carlson) between them. In linear values:

   | input | 0.001 | 0.0025 | 0.005 | 0.01 | 0.02 | 0.035 | 0.06 | 0.1 | 0.16 | 0.25 | 0.38 | 0.55 | 0.75 | 1 |
   |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
   | output | 0.000088 | 0.00049 | 0.0018 | 0.0043 | 0.014 | 0.038 | 0.090 | 0.19 | 0.34 | 0.53 | 0.72 | 0.86 | 0.93 | 1 |
   | stops | -3.5 | -2.4 | -1.5 | -1.2 | -0.5 | +0.1 | +0.6 | +0.9 | +1.1 | +1.1 | +0.9 | +0.6 | +0.3 | 0 |

   Mid grey (0.18) becomes 0.39. Inputs above 1 (after the exposure) become 1. The darkest shadows get darker,
   which hides the noise floor and gives blacks some depth. The point at 0.001 (below 1 L*, black on any display) is
   not measured: few files have enough pixels that dark, so it continues the slope of the points above it.
3. **Hue-preserving application, in linear ProPhoto RGB.** For a pixel with channels `max >= mid >= min`, the
   largest and the smallest go through the curve and the middle one keeps its relative place between them:
   `mid' = min' + (mid - min) (max' - min') / (max - min)`. This is the RGB tone curve of the DNG specification's
   reference rendering. Neutral pixels get the curve on every channel. Colours keep their hue, and their saturation
   follows the contrast of the curve (as with a per-channel curve). A per-channel curve applied to sRGB values
   shifts hues: oranges towards yellow, saturated blues towards cyan. ProPhoto is the space the measurements below
   were taken in. Its gamut holds every colour a camera matrix produces in practice, so channels rarely need
   clamping at 0.

## Implementation and cost

The curve is tabulated once per process: 1,024 steps evenly spaced in `sqrt(x)`, which gives finer steps in the
shadows than a table over linear values, read with linear interpolation. The exposure only changes the scale of the
index, so the table does not depend on the file. Per pixel:

- one 3x3 product to ProPhoto to find the largest and the smallest channel;
- two table reads;
- one multiply-add per channel, back in the output space: `(min' - k min) w + k p`, where `k = (max' - min') / (max
  - min)`, `p` is the input pixel and `w` is ProPhoto white in the output space. This is the same result as blending
  in ProPhoto and converting back, without the second matrix product.

Pixels are toned in blocks of 64: the pixels are copied into three planes, then the arithmetic before and after the
table reads runs as straight loops over plain floats, which the compiler vectorises (checked in the x86-64 AVX2
build), and the table reads run in between. AVX2 gathers for the table reads were measured too: no faster than scalar loads on this
machine, so they are not used. Toning happens just before the output encoding:

- in the fused develop, rows are toned in blocks before the encoder;
- mirrored and rotated outputs, which store pixels one at a time, tone their rows in blocks first;
- float passes (noise reduction, camera profiles, SuperCCD resampling) tone the image in place before encoding;
- output ICC profiles with lookup tables get the tone before their transform.

Every path gives the same bytes, and unit tests check this for all eight orientations.

One thread of an AMD Ryzen 7 9700X tones 0.73 billion pixels per second (1.37 ns per pixel; the first version, one
pixel at a time, took 3.3 ns). Whole decodes on 16 threads, median of 15:

| File | output | demosaic | linear | standard | difference |
|---|---|---|---|---|---|
| Nikon D610 NEF, 24 MP | rgba8 | bilinear | 37.1 ms | 42.6 ms | +15 % |
| | rgba8 | auto | 68.5 ms | 74.7 ms | +9 % |
| | rgb16 | auto | 76.5 ms | 82.9 ms | +8 % |
| Canon CR3, 45 MP | rgba8 | bilinear | 92.0 ms | 102.6 ms | +12 % |
| | rgba8 | auto | 154.8 ms | 166.0 ms | +7 % |
| | rgb16 | auto | 168.9 ms | 181.1 ms | +7 % |

Memory: a few kilobytes per thread, and one row of floats per thread for mirrored and rotated outputs.

## Measurements

The reference is the default rendering of a widely used raw developer (settings reset, sharpening and noise
reduction off, exported as 16-bit sRGB). The files are the Adobe DNG conversions of camera files from the corpus,
whose `BaselineExposure` is the brightness offset that developer applies to that camera, and four camera-native DNGs
(two Ricoh, two Pentax). The 26 files come from 12 makers. FlashRaw rendered the same files with `Tone::Linear`, to
16-bit sRGB.

- **The curve.** On 14 files whose rendering lines up pixel for pixel with FlashRaw's, pixel values were averaged
  over 4x4 blocks and converted to linear ProPhoto. For the low-saturation blocks (largest channel below 1.6 times the
  smallest), the reference's largest and smallest channel were taken as a function of FlashRaw's, times
  `2^BaselineExposure`, in 80 logarithmic bins. The control points are the medians over files. Below 0.004 and above
  0.4, where these blocks are few, the points come from matching quantiles of the luminance instead, which also
  covers the files that do not line up. The two estimates agree within 0.05 stops where both exist. The spread
  between files is small: at mid grey the quartiles of the files lie within 0.02 stops of the median.
- **The files that do not line up** differ in geometry. Most are from mirrorless and compact cameras, whose built-in
  lens corrections the reference applies even after a reset; the others have an in-camera aspect crop or a reduced
  size (small raw) that it honours.

Brightness difference between FlashRaw with `Tone::Standard` and the reference, in L*: the median over the 1st to
99th percentiles of the luminance (all files), and the median over 8x8 blocks (files that line up):

| File (camera) | BaselineExposure | percentiles | blocks |
|---|---|---|---|
| Sony NEX-3 | +0.35 | 1.79 | 1.82 |
| Sony DSC-RX10M4 | -0.25 | 0.21 | |
| Sony ILCE-7SM2 | +0.35 | 1.08 | |
| Sony ILCE-7SM3 | +0.15 | 1.39 | |
| Sony DSLR-A390 | 0 | 1.20 | 1.35 |
| Canon EOS R6 | +0.37 | 1.40 | 2.27 |
| Canon PowerShot SX1 IS | -0.60 | 0.24 | 0.29 |
| Canon EOS R8 | +1.27 | 2.71 | |
| Nikon Z6III | +0.25 | 1.43 | |
| Nikon Z7 (small raw) | +0.15 | 0.63 | |
| Nikon D500 (small raw) | +0.35 | 1.43 | |
| Fujifilm X-T20 | +0.15 | 0.57 | |
| Fujifilm FinePix S6000fd | -0.50 | 0.94 | |
| Olympus E-400 | -0.50 | 0.92 | 0.95 |
| Olympus E-P5 | +0.50 | 0.21 | 0.29 |
| Panasonic DC-G100D | +0.25 | 1.01 | |
| Panasonic DC-GX7MK3 | +0.25 | 1.50 | |
| Pentax K-3 Mark III | +0.70 | 0.50 | 0.65 |
| Leica V-Lux 4 | -0.50 | 1.75 | |
| Hasselblad CFV-50 | -0.25 | 0.30 | 0.46 |
| Samsung NX300M | 0 | 0.44 | 0.34 |
| Phase One P25+ | -1.00 | 0.78 | 0.60 |
| Ricoh GR IIIx (native DNG) | -0.51 | 0.20 | 0.16 |
| Ricoh GR Digital 2 (native DNG) | -0.50 | 10.23 | 9.99 |
| Pentax K10D (native DNG) | -0.50 | 0.50 | 0.60 |
| Pentax K-r (native DNG) | -0.52 | 0.15 | 0.17 |

The median is 0.93 L* over the percentiles (24 of 26 files under 2) and 0.60 L* over the blocks. Without the tone it
is 10.6 L*.

The Ricoh GR Digital 2 file is the exception. The reference renders it about 0.6 stops brighter in the mid-tones,
and much brighter in the shadows, than its `BaselineExposure` of -0.5 and the curve predict. Another Ricoh DNG with
the same offset fits to 0.2 L*. The cause is not known. The file is one of 26 in the medians the curve is made of,
so it does not move them.

## Offsets for other formats

DNG files carry their offset; for the other formats the camera database stores one per model, measured from the
cameras' own files (`flashraw camera-db exposure`, `tools/camera_db/exposure.py`,
[camera database](../camera-db.md#exposure-offsets)):

1. **Per file:** the offset that makes the luminance of the standard rendering match the camera's embedded JPEG over
   the mid-tones (the 30th to 70th percentiles, compared as percentiles so that lens corrections and crops do not
   matter). The camera's JPEG is the maker's rendering of the same exposure.
2. **Per model:** the median over the model's files.
3. **Per make:** camera JPEG engines are brighter or darker than the standard rendering by an amount that depends on
   the make, so a manual correction per make accounts for each maker's particularities: -0.14 stops for Canon, -0.12
   Pentax, -0.07 Nikon, 0 Sony, between -0.08 and +0.35 for the others, -0.06 for the makes not listed. A make's
   default (for models without files) is the median over its models.
4. **Few files:** a model's value is pulled towards its make's default with the weight of half a file, since one
   scene can be off by half a stop.

The database holds 712 entries (698 models and 14 make defaults) from 1,532 files of the CC0 corpus with an
embedded JPEG at least 320 pixels wide; film scanners are not measured. Formats whose files carry no JPEG preview
(Phase One and Leaf IIQ/MOS, most early digital backs and some compacts) have no entry.

## Limits

- **The offsets of other formats are estimates.** They follow the camera's own JPEG, corrected per make, so a
  model whose JPEG engine departs from its make's usual rendering keeps that difference. Makes with fewer than three
  measured models have no default. In-camera modes that lower the raw exposure to keep highlights (highlight tone
  priority, extended dynamic range) are not detected: the database holds one value per model.
- **Colour is not part of the tone.** The colour matrices are the file's. The reference renders colour with its own
  camera profiles, which have hue and saturation tables, so saturated colours still differ. The tone matches
  brightness, and colours keep their hue.
- **No adaptive controls.** There are no shadows/highlights sliders and no local contrast; the curve is the same for
  every image.
