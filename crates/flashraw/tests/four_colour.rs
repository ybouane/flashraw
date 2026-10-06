//! Colour of RGB renders from four-colour sensors (CMYG complementary filters, RGBE):
//! the developed image must be neutral, with the colour balance of the camera's own
//! embedded preview.
//!
//! Two sets, each skipped cleanly when absent:
//! - the CC0 corpus (`$FLASHRAW_CORPUS` or `<repo>/corpus`);
//! - extra local files (`$FLASHRAW_FOUR_COLOUR_SAMPLES`, a directory laid out as listed
//!   in `SAMPLES`; these files are not redistributable).

use flashraw::{DecodeOptions, Demosaic, FlashRaw, OutputFormat, SensorLayout};
use std::path::{Path, PathBuf};

/// Corpus files with a four-colour CFA.
const CORPUS: &[&str] = &[
    "Canon/PowerShot G1/crw_1693.crw",
    "Canon/PowerShot Pro70/RAW_CANON_PRO70_SRGB.CRW",
    "Nikon/E5700/20080115-DSCN1758.NEF",
    "Sony/DSC-F828/DSC06227.SRF",
];

/// Extra files (relative to `$FLASHRAW_FOUR_COLOUR_SAMPLES`).
const SAMPLES: &[&str] = &[
    "web/rawsamples.ch/canon/g1/RAW_CANON_G1.CRW",
    "web/rawsamples.ch/canon/pro70/RAW_CANON_PRO70_SRGB.CRW",
    "web/rawsamples.ch/nikon/e5700/RAW_NIKON_E5700_SRGB.NEF",
    "ir/p90/P90DBAU.CRW",
    "ir/p90/P90HMU.CRW",
    "ir/p90/P90MUSALU.CRW",
    "ir/p90/P90RESWU.CRW",
];

/// Largest allowed difference between the render's and the preview's mean R/G and B/G
/// (the camera's tone and saturation rendering differ from a plain sRGB render; before
/// four-colour development, renders were off by several units).
const TOLERANCE: f64 = 0.2;

/// Plausible mean R/G and B/G of a render of a file without a preview.
const NEUTRAL: std::ops::RangeInclusive<f64> = 0.6..=1.5;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Mean R/G and B/G of interleaved 8-bit pixels (`stride` bytes per pixel).
fn balance(px: &[u8], stride: usize) -> (f64, f64) {
    let mut s = [0f64; 3];
    for p in px.chunks_exact(stride) {
        for c in 0..3 {
            s[c] += p[c] as f64;
        }
    }
    (s[0] / s[1].max(1.0), s[2] / s[1].max(1.0))
}

/// Render `(R/G, B/G)` and, when the file embeds one, the preview's.
fn measure(path: &Path, demosaic: Demosaic) -> ((f64, f64), Option<(f64, f64)>) {
    let raw = FlashRaw::open_path(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let info = raw.frame_info();
    assert_eq!(info.layout, SensorLayout::OtherCfa, "{}", path.display());
    let distinct = info.cfa.map(|c| {
        let mut v = c.colors();
        v.sort_by_key(|c| *c as u8);
        v.dedup();
        v.len()
    });
    assert_eq!(distinct, Some(4), "{}: expected a four-colour CFA", path.display());
    let frame = raw
        .decode(DecodeOptions { output: OutputFormat::Rgba8, demosaic, ..Default::default() })
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let render = balance(frame.data.as_u8().expect("rgba8"), 4);
    let preview = raw.preview().map(|p| {
        let img = raw.decode_preview_rgb(p).unwrap_or_else(|e| panic!("{}: preview: {e}", path.display()));
        balance(&img.rgb, 3)
    });
    (render, preview)
}

fn check(root: &Path, files: &[&str]) -> usize {
    let mut tested = 0;
    let mut failures = Vec::new();
    for f in files {
        let path = root.join(f);
        if !path.exists() {
            continue;
        }
        for demosaic in [Demosaic::Bilinear, Demosaic::HalfSize] {
            let ((rr, rb), preview) = measure(&path, demosaic);
            let f = format!("{f} ({demosaic:?})");
            match preview {
                Some((pr, pb)) => {
                    eprintln!("{f}: render R/G {rr:.3} B/G {rb:.3}, preview R/G {pr:.3} B/G {pb:.3}");
                    if (rr - pr).abs() > TOLERANCE || (rb - pb).abs() > TOLERANCE {
                        failures
                            .push(format!("{f}: render R/G {rr:.3} B/G {rb:.3} vs preview R/G {pr:.3} B/G {pb:.3}"));
                    }
                }
                None => {
                    // No preview to compare with: the render must at least be roughly neutral.
                    eprintln!("{f}: render R/G {rr:.3} B/G {rb:.3} (no preview)");
                    if !(NEUTRAL.contains(&rr) && NEUTRAL.contains(&rb)) {
                        failures.push(format!("{f}: render R/G {rr:.3} B/G {rb:.3} is far from neutral"));
                    }
                }
            }
        }
        tested += 1;
    }
    assert!(failures.is_empty(), "colour balance differs from the preview:\n{}", failures.join("\n"));
    tested
}

#[test]
fn corpus_four_colour_renders_match_previews() {
    let root = std::env::var("FLASHRAW_CORPUS").map(PathBuf::from).unwrap_or_else(|_| repo().join("corpus"));
    let n = check(&root, CORPUS);
    eprintln!("four-colour corpus files checked: {n}");
}

#[test]
fn sample_four_colour_renders_match_previews() {
    let Ok(root) = std::env::var("FLASHRAW_FOUR_COLOUR_SAMPLES") else {
        eprintln!("FLASHRAW_FOUR_COLOUR_SAMPLES not set; skipped");
        return;
    };
    let n = check(Path::new(&root), SAMPLES);
    eprintln!("four-colour sample files checked: {n}");
}

// ---- Synthetic frames ----

use flashraw::low_level::core::RawImage;
use flashraw::process::{DevelopParams, develop};
use flashraw::{
    BlackLevel, CfaColor, CfaPattern, ColorMatrices, ColorMatrix, ColorSource, FrameInfo, Illuminant, Limits, Rect,
    WhiteBalance, WhiteLevel,
};

/// A complementary-colour (G1-like) and an RGBE (F828-like) sensor: pattern and XYZ→camera
/// matrix with rows in the pattern's four-colour order.
fn sensors() -> Vec<(CfaPattern, Vec<f32>)> {
    use CfaColor::*;
    vec![
        (
            CfaPattern::new(2, 2, &[Green, Magenta, Yellow, Cyan]).unwrap(),
            vec![-0.5686, 1.03, 0.2223, 0.4725, -0.1157, 0.4383, -0.6128, 1.0783, 0.6163, -0.2688, 1.2087, 0.0598],
        ),
        (
            CfaPattern::new(2, 2, &[Emerald, Red, Blue, Green]).unwrap(),
            vec![0.7924, -0.1910, -0.0780, -0.8233, 1.5458, 0.3003, -0.1523, 0.2200, 0.6817, -0.7235, 1.1396, 0.3479],
        ),
    ]
}

/// A flat field of the camera response to linear sRGB `rgb`, scaled by `gain`.
fn flat_field(cfa: CfaPattern, matrix: &[f32], rgb: [f32; 3], gain: f32) -> (RawImage, FrameInfo) {
    let (w, h) = (16u32, 16u32);
    let m = ColorMatrix::new(Illuminant::D65, 4, matrix.to_vec()).unwrap();
    let order = cfa.four_colour_order().expect("four-colour pattern");
    let response = m.camera_response(rgb);
    let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
    for (i, v) in img.u16_mut().iter_mut().enumerate() {
        let c = cfa.color_at(i % w as usize, i / w as usize);
        let ch = order.iter().position(|o| *o == c).unwrap();
        *v = (response[ch] * gain * 4095.0).round().clamp(0.0, 4095.0) as u16;
    }
    let mut info = FrameInfo::new(w, h, 12);
    info.cfa = Some(cfa);
    info.layout = SensorLayout::OtherCfa;
    info.black_level = BlackLevel::uniform(0.0);
    info.white_level = WhiteLevel::uniform(4095);
    info.color_matrices = ColorMatrices { color: vec![m], source: ColorSource::CameraDb, ..Default::default() };
    (img, info)
}

/// Mean developed RGB (linear) of the interior of a flat field.
fn developed_mean(img: &RawImage, info: &FrameInfo, demosaic: Demosaic) -> [f32; 3] {
    let p = DevelopParams::new(info, demosaic, Rect::full(info.width, info.height));
    let d = develop(img, info, (0, 0), &p, false).unwrap();
    let (w, h) = (d.width as usize, d.height as usize);
    let mut s = [0f32; 3];
    let mut n = 0f32;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            for c in 0..3 {
                s[c] += d.rgb[(y * w + x) * 3 + c];
            }
            n += 1.0;
        }
    }
    s.map(|v| v / n)
}

fn assert_proportional(got: [f32; 3], want: [f32; 3], what: &str) {
    for c in [0, 2] {
        let (g, w) = (got[c] / got[1], want[c] / want[1]);
        assert!((g - w).abs() < 0.02, "{what}: channel {c} ratio {g:.4}, expected {w:.4} (got {got:?})");
    }
}

/// Without a recorded white balance, a colour lit by daylight develops to that colour.
#[test]
fn synthetic_four_colour_daylight_flat_field_keeps_its_colour() {
    for (cfa, matrix) in sensors() {
        for rgb in [[0.25f32, 0.25, 0.25], [0.3, 0.2, 0.1], [0.1, 0.2, 0.3]] {
            // Daylight white balance reciprocal: a camera response divided by it.
            let m = ColorMatrix::new(Illuminant::D65, 4, matrix.clone()).unwrap();
            let gain = 1.0 / m.camera_response([1.0; 3]).iter().copied().fold(0.0, f32::max);
            let (img, info) = flat_field(cfa, &matrix, rgb, gain);
            assert!(info.white_balance.is_none());
            for d in [Demosaic::Bilinear, Demosaic::HalfSize] {
                let got = developed_mean(&img, &info, d);
                assert!(got.iter().all(|v| v.is_finite() && *v > 0.0), "{cfa} {d:?}: {got:?}");
                assert_proportional(got, rgb, &format!("{cfa} {d:?} {rgb:?}"));
            }
        }
    }
}

/// With an as-shot white balance, the colour it neutralises develops to grey.
#[test]
fn synthetic_four_colour_as_shot_balance_neutralises_the_illuminant() {
    for (cfa, matrix) in sensors() {
        let m = ColorMatrix::new(Illuminant::D65, 4, matrix.clone()).unwrap();
        // A warm (tungsten-like) illuminant in linear sRGB.
        let warm = [1.0f32, 0.75, 0.45];
        let mul = m.multipliers_for(warm).unwrap();
        let gain = 0.5 / m.camera_response(warm).iter().copied().fold(0.0, f32::max);
        let (img, mut info) = flat_field(cfa, &matrix, warm, gain);
        info.white_balance = WhiteBalance::from_multipliers(mul[0], mul[1], mul[2], mul[3]);
        for d in [Demosaic::Bilinear, Demosaic::HalfSize] {
            let got = developed_mean(&img, &info, d);
            assert_proportional(got, [1.0; 3], &format!("{cfa} {d:?}"));
        }
    }
}

/// Every four-colour channel is developed: a flat field of one channel alone is not black.
#[test]
fn synthetic_four_colour_uses_every_channel() {
    for (cfa, matrix) in sensors() {
        let order = cfa.four_colour_order().unwrap();
        for only in order {
            let (mut img, info) = flat_field(cfa, &matrix, [0.0; 3], 0.0);
            let w = info.width as usize;
            for (i, v) in img.u16_mut().iter_mut().enumerate() {
                if cfa.color_at(i % w, i / w) == only {
                    *v = 2000;
                }
            }
            let base = developed_mean(&img, &info, Demosaic::Bilinear);
            assert!(base.iter().any(|v| v.abs() > 1e-3), "{cfa}: channel {only:?} ignored ({base:?})");
        }
    }
}
