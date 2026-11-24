//! Luma and chroma noise reduction (`DecodeOptions::luma_noise_reduction`,
//! `DecodeOptions::chroma_noise_reduction`) on synthetic Bayer and X-Trans mosaics: each strength
//! reduces its own kind of noise and leaves the other alone, keeps the mean colour and the edges,
//! and zero changes nothing.

use flashraw::{Backend, DecodeOptions, Error, FlashRaw, OutputFormat};

mod common;
use common::synthetic::dng;

const BAYER: [u8; 4] = [0, 1, 1, 2];
#[rustfmt::skip]
const XTRANS: [u8; 36] = [
    1, 1, 0, 1, 1, 2,
    1, 1, 2, 1, 1, 0,
    2, 0, 1, 0, 2, 1,
    1, 1, 2, 1, 1, 0,
    1, 1, 0, 1, 1, 2,
    0, 2, 1, 2, 0, 1,
];

fn linear(raw: &FlashRaw<'_>, luma: f32, chroma: f32) -> (usize, usize, Vec<f32>) {
    let f = raw
        .decode(DecodeOptions {
            output: OutputFormat::LinearRgbF32,
            backend: Backend::Cpu,
            luma_noise_reduction: luma,
            chroma_noise_reduction: chroma,
            ..Default::default()
        })
        .unwrap();
    (f.width() as usize, f.height() as usize, f.as_f32().unwrap().to_vec())
}

/// Deviation of luminance and of the two colour differences from their 5x5 local means, over
/// the left half of the frame (away from the edge at the middle and from the borders).
fn noise(w: usize, h: usize, rgb: &[f32]) -> (f64, f64) {
    let px = |x: usize, y: usize| -> [f64; 3] {
        let p = &rgb[(y * w + x) * 3..][..3];
        let (r, g, b) = (p[0] as f64, p[1] as f64, p[2] as f64);
        [0.2126 * r + 0.7152 * g + 0.0722 * b, r - g, b - g]
    };
    let (mut lum, mut chroma, mut n) = (0.0, 0.0, 0.0);
    for y in (8..h - 8).step_by(3) {
        for x in (8..w / 2 - 8).step_by(3) {
            let mut mean = [0.0; 3];
            for dy in 0..5 {
                for dx in 0..5 {
                    let q = px(x + dx - 2, y + dy - 2);
                    (0..3).for_each(|c| mean[c] += q[c] / 25.0);
                }
            }
            let q = px(x, y);
            lum += (q[0] - mean[0]).powi(2);
            chroma += (q[1] - mean[1]).powi(2) + (q[2] - mean[2]).powi(2);
            n += 1.0;
        }
    }
    ((lum / n).sqrt(), (chroma / n / 2.0).sqrt())
}

fn means(rgb: &[f32]) -> [f64; 3] {
    let n = (rgb.len() / 3) as f64;
    let mut m = [0.0; 3];
    for p in rgb.chunks_exact(3) {
        (0..3).for_each(|c| m[c] += p[c] as f64 / n);
    }
    m
}

#[test]
fn each_strength_reduces_its_own_noise_and_keeps_the_mean_colour() {
    for (name, dim, pattern) in [("bayer", 2u16, &BAYER[..]), ("x-trans", 6, &XTRANS[..])] {
        let file = dng(384, 288, dim, pattern, 30.0);
        let raw = FlashRaw::open(file.as_slice()).unwrap();
        let (w, h, off) = linear(&raw, 0.0, 0.0);
        let (l0, c0) = noise(w, h, &off);

        let (_, _, luma) = linear(&raw, 1.0, 0.0);
        let (l1, c1) = noise(w, h, &luma);
        assert!(l1 < 0.5 * l0, "{name}: luma 1 left luminance noise {l1} of {l0}");
        assert!((c1 / c0 - 1.0).abs() < 0.1, "{name}: luma 1 changed colour noise {c0} -> {c1}");

        let (_, _, chroma) = linear(&raw, 0.0, 0.5);
        let (l2, c2) = noise(w, h, &chroma);
        assert!(c2 < 0.4 * c0, "{name}: chroma 0.5 left colour noise {c2} of {c0}");
        assert!((l2 / l0 - 1.0).abs() < 0.05, "{name}: chroma 0.5 changed luminance noise {l0} -> {l2}");

        // A middle strength lies in between.
        let (_, _, half) = linear(&raw, 0.5, 0.0);
        let (lh, _) = noise(w, h, &half);
        assert!(l1 < lh && lh < l0, "{name}: luma 0.5 gives {lh}, not between {l1} and {l0}");

        let (m0, m1) = (means(&off), means(&linear(&raw, 1.0, 1.0).2));
        for c in 0..3 {
            assert!((m1[c] / m0[c] - 1.0).abs() < 0.01, "{name}: mean of channel {c} {} -> {}", m0[c], m1[c]);
        }
    }
}

#[test]
fn the_edge_stays_sharp() {
    let file = dng(384, 288, 2, &BAYER, 30.0);
    let raw = FlashRaw::open(file.as_slice()).unwrap();
    let (w, h, off) = linear(&raw, 0.0, 0.0);
    let (_, _, nr) = linear(&raw, 1.0, 1.0);
    // Green jumps at the middle column: the step between the levels on either side stays.
    let col =
        |rgb: &[f32], x: usize| (8..h - 8).map(|y| rgb[(y * w + x) * 3 + 1] as f64).sum::<f64>() / (h - 16) as f64;
    let step = |rgb: &[f32]| col(rgb, w / 2 + 3) - col(rgb, w / 2 - 4);
    let (a, b) = (step(&off), step(&nr));
    assert!(a > 0.05, "{a}");
    assert!((b / a - 1.0).abs() < 0.08, "step {a} -> {b}");
}

#[test]
fn zero_changes_nothing_and_raw_outputs_ignore_it() {
    let file = dng(200, 160, 2, &BAYER, 8.0);
    let raw = FlashRaw::open(file.as_slice()).unwrap();
    for output in [OutputFormat::Rgba8, OutputFormat::Rgb16, OutputFormat::RawU16] {
        let o = DecodeOptions { output, ..Default::default() };
        let plain = raw.decode(o.clone()).unwrap();
        let zero =
            raw.decode(DecodeOptions { luma_noise_reduction: 0.0, chroma_noise_reduction: 0.0, ..o.clone() }).unwrap();
        assert_eq!(plain.as_u8().or(None), zero.as_u8().or(None), "{output:?}");
        assert_eq!(plain.as_u16().or(None), zero.as_u16().or(None), "{output:?}");
        if output == OutputFormat::RawU16 {
            let nr = raw.decode(DecodeOptions { luma_noise_reduction: 1.0, chroma_noise_reduction: 1.0, ..o }).unwrap();
            assert_eq!(plain.as_u16(), nr.as_u16());
        }
    }
}

#[test]
fn strengths_are_clamped_and_nan_is_rejected() {
    let file = dng(200, 160, 2, &BAYER, 8.0);
    let raw = FlashRaw::open(file.as_slice()).unwrap();
    let rgb = |l: f32, c: f32| {
        raw.decode(DecodeOptions {
            output: OutputFormat::Rgb16,
            luma_noise_reduction: l,
            chroma_noise_reduction: c,
            ..Default::default()
        })
    };
    assert_eq!(rgb(5.0, 7.0).unwrap().as_u16(), rgb(1.0, 1.0).unwrap().as_u16());
    assert_eq!(rgb(-1.0, -3.0).unwrap().as_u16(), rgb(0.0, 0.0).unwrap().as_u16());
    for (l, c) in [(f32::NAN, 0.0), (0.0, f32::NAN)] {
        let e = rgb(l, c).unwrap_err();
        assert!(matches!(e, Error::InvalidArgument(_)), "{e}");
    }
}
