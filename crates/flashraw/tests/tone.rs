//! The tone of the display outputs (`DecodeOptions::tone`): `Standard`, the default, lifts the
//! mid-tones, keeps the order of the values and the hues, applies the DNG baseline exposure, and
//! leaves the linear and raw outputs alone; `Linear` applies no curve.

use flashraw::{Backend, DecodeOptions, Demosaic, FlashRaw, OutputFormat, Tone};

mod common;
use common::synthetic::{dng, dng_with};

const BAYER: [u8; 4] = [0, 1, 1, 2];

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures").join(name)).unwrap()
}

fn opts(output: OutputFormat, tone: Tone) -> DecodeOptions {
    DecodeOptions { output, tone, demosaic: Demosaic::HalfSize, backend: Backend::Cpu, ..Default::default() }
}

fn rgb16(raw: &FlashRaw<'_>, o: DecodeOptions) -> Vec<u16> {
    raw.decode(DecodeOptions { output: OutputFormat::Rgb16, ..o }).unwrap().as_u16().unwrap().to_vec()
}

/// sRGB-encoded 16-bit sample to linear.
fn lin(v: u16) -> f64 {
    let x = v as f64 / 65535.0;
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

#[test]
fn standard_is_the_default_and_lifts_the_mid_tones() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let plain = rgb16(&raw, opts(OutputFormat::Rgb16, Tone::Linear));
    let toned = rgb16(&raw, opts(OutputFormat::Rgb16, Tone::Standard));
    assert_eq!(toned, rgb16(&raw, DecodeOptions { demosaic: Demosaic::HalfSize, ..Default::default() }));
    assert_eq!(plain.len(), toned.len());
    // Mid-tones (linear 0.1 to 0.3) come out about one stop brighter; the darkest values darker.
    let (mut mid, mut n_mid, mut dark, mut n_dark) = (0.0, 0, 0.0, 0);
    for (&a, &b) in plain.iter().zip(&toned) {
        let (x, y) = (lin(a), lin(b));
        if (0.1..0.3).contains(&x) {
            mid += (y / x).log2();
            n_mid += 1;
        } else if (0.0005..0.003).contains(&x) {
            dark += (y / x).log2();
            n_dark += 1;
        }
    }
    let (mid, dark) = (mid / n_mid as f64, dark / n_dark.max(1) as f64);
    assert!(n_mid > 300 && (0.6..1.4).contains(&mid), "mid-tones {mid:+.2} EV over {n_mid} samples");
    assert!(n_dark == 0 || dark < 0.0, "deep shadows {dark:+.2} EV over {n_dark} samples");
}

#[test]
fn the_tone_keeps_the_order_of_values_and_of_channels() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let plain = rgb16(&raw, opts(OutputFormat::Rgb16, Tone::Linear));
    let toned = rgb16(&raw, opts(OutputFormat::Rgb16, Tone::Standard));
    let (mut swapped, mut pixels) = (0, 0);
    for (p, q) in plain.chunks_exact(3).zip(toned.chunks_exact(3)) {
        // Unclipped pixels with distinct channels keep the order of their channels (the hue).
        if p.iter().all(|&v| v > 2000 && v < 60000) {
            pixels += 1;
            for (i, j) in [(0, 1), (1, 2), (0, 2)] {
                if p[i].abs_diff(p[j]) > 800 && (p[i] > p[j]) != (q[i] > q[j]) {
                    swapped += 1;
                }
            }
        }
    }
    assert!(pixels > 1000);
    assert!(swapped * 1000 < pixels, "{swapped} channel pairs swapped in {pixels} pixels");
    // Neutral input stays neutral, and brighter input stays brighter.
    let mut pairs: Vec<(u16, u16)> = plain.iter().copied().zip(toned.iter().copied()).collect();
    pairs.sort_unstable();
    let inversions = pairs.windows(2).filter(|w| w[1].0 > w[0].0 + 2000 && w[1].1 + 2000 < w[0].1).count();
    assert!(inversions * 1000 < pairs.len(), "{inversions} inversions");
}

#[test]
fn rgba8_rgb16_threads_and_tiles_agree() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let wide = rgb16(&raw, opts(OutputFormat::Rgb16, Tone::Standard));
    let one = rgb16(&raw, DecodeOptions { threads: Some(1), ..opts(OutputFormat::Rgb16, Tone::Standard) });
    assert_eq!(wide, one);
    let simd = rgb16(&raw, DecodeOptions { backend: Backend::CpuSimd, ..opts(OutputFormat::Rgb16, Tone::Standard) });
    assert_eq!(wide, simd);
    let f = raw.decode(opts(OutputFormat::Rgba8, Tone::Standard)).unwrap();
    let narrow = f.as_u8().unwrap();
    for (p, q) in wide.chunks_exact(3).zip(narrow.chunks_exact(4)) {
        for c in 0..3 {
            let a = (p[c] as f64 / 257.0).round();
            assert!((a - q[c] as f64).abs() <= 2.0, "{p:?} vs {q:?}");
        }
    }
}

#[test]
fn linear_and_raw_outputs_ignore_the_tone() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    for output in [OutputFormat::LinearRgbF32, OutputFormat::LinearRgb16, OutputFormat::RawU16] {
        let a = raw.decode(opts(output, Tone::Linear)).unwrap();
        let b = raw.decode(opts(output, Tone::Standard)).unwrap();
        assert_eq!(a.data, b.data, "{output:?}");
    }
}

#[test]
fn the_baseline_exposure_of_a_dng_is_applied_before_the_curve() {
    // SRATIONAL BaselineExposure (tag 0xC62A) of +1 stop.
    let be = |num: i32| vec![(0xC62Au16, 10u16, 1u32, [num.to_le_bytes(), 1i32.to_le_bytes()].concat())];
    let plain = dng(192, 128, 2, &BAYER, 1.0);
    let bright = dng_with(192, 128, 2, &BAYER, 1.0, &be(1));
    let (a, b) = (FlashRaw::open(&plain[..]).unwrap(), FlashRaw::open(&bright[..]).unwrap());
    assert_eq!(b.metadata().dng.as_ref().and_then(|d| d.baseline_exposure), Some(1.0));
    // Linear ignores it.
    assert_eq!(rgb16(&a, opts(OutputFormat::Rgb16, Tone::Linear)), rgb16(&b, opts(OutputFormat::Rgb16, Tone::Linear)));
    // Standard: the same as one more stop of exposure on the file without it.
    let toned = rgb16(&b, opts(OutputFormat::Rgb16, Tone::Standard));
    let exposed = rgb16(&a, DecodeOptions { exposure: 1.0, ..opts(OutputFormat::Rgb16, Tone::Standard) });
    let worst = toned.iter().zip(&exposed).map(|(p, q)| p.abs_diff(*q)).max().unwrap();
    assert!(worst <= 2, "{worst}");
    assert_ne!(toned, rgb16(&a, opts(OutputFormat::Rgb16, Tone::Standard)));
}

#[test]
fn a_camera_profile_with_its_own_tone_curve_wins() {
    let bytes = fixture("canon-5d3-lossy.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let mut profile = raw.embedded_camera_profile().expect("embedded profile");
    let with = |p: &flashraw::CameraProfile, tone| {
        rgb16(&raw, DecodeOptions { camera_profile: Some(p.clone()), ..opts(OutputFormat::Rgb16, tone) })
    };
    // The embedded profile has no tone curve: the standard one applies.
    assert!(profile.tone_curve.is_none());
    assert_ne!(with(&profile, Tone::Linear), with(&profile, Tone::Standard));
    profile.tone_curve = Some(vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]);
    assert_eq!(with(&profile, Tone::Linear), with(&profile, Tone::Standard));
}

#[test]
fn names_parse() {
    assert_eq!(Tone::parse("Standard"), Some(Tone::Standard));
    assert_eq!(Tone::parse("linear"), Some(Tone::Linear));
    assert_eq!(Tone::parse("filmic"), None);
    assert_eq!(Tone::Standard.name(), "standard");
    assert_eq!(Tone::default(), Tone::Standard);
    assert_eq!(DecodeOptions::default().tone, Tone::Standard);
}
