//! Output colour spaces: every backend and thread count agree, the linear outputs are the
//! sRGB outputs times the space's matrix, the display outputs apply its curve, regions match
//! full decodes, and frames report their space and profile.

use flashraw::{
    Backend, ColorSpace, DecodeOptions, Demosaic, Error, FlashRaw, OutputFormat, OutputProfile, RawPixelData, Rect,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures").join(name)).unwrap()
}

fn opts(output: OutputFormat, cs: ColorSpace) -> DecodeOptions {
    DecodeOptions { output, color_space: cs, demosaic: Demosaic::HalfSize, ..Default::default() }
}

const RGB_OUTPUTS: [OutputFormat; 5] = [
    OutputFormat::LinearRgbF32,
    OutputFormat::Rgba8,
    OutputFormat::Rgb16,
    OutputFormat::LinearRgb16,
    OutputFormat::LinearRgbF16,
];

#[test]
fn backends_and_thread_counts_agree() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    for cs in ColorSpace::ALL {
        for out in RGB_OUTPUTS {
            let base = raw.decode(DecodeOptions { backend: Backend::Cpu, threads: Some(1), ..opts(out, cs) }).unwrap();
            for (backend, threads) in [(Backend::CpuSimd, Some(1)), (Backend::CpuSimd, None), (Backend::Cpu, None)] {
                let f = raw.decode(DecodeOptions { backend, threads, ..opts(out, cs) }).unwrap();
                assert_eq!(f.data, base.data, "{cs:?} {out:?} {backend:?} {threads:?}");
                assert_eq!(f.color_space, cs);
            }
        }
    }
}

#[test]
fn linear_outputs_are_the_srgb_outputs_times_the_matrix() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let srgb = raw.decode(opts(OutputFormat::LinearRgbF32, ColorSpace::Srgb)).unwrap();
    let s = srgb.as_f32().unwrap();
    for cs in ColorSpace::ALL {
        let f = raw.decode(opts(OutputFormat::LinearRgbF32, cs)).unwrap();
        let d = f.as_f32().unwrap();
        let m = cs.matrix_from_srgb();
        let mut worst = 0f64;
        for (a, b) in s.chunks_exact(3).zip(d.chunks_exact(3)) {
            for i in 0..3 {
                let want: f64 = (0..3).map(|k| m[i][k] * a[k] as f64).sum();
                worst = worst.max((want - b[i] as f64).abs());
            }
        }
        assert!(worst < 2e-5, "{cs:?}: {worst}");
    }
}

#[test]
fn display_outputs_apply_the_space_curve() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    for cs in ColorSpace::ALL {
        let lin = raw.decode(opts(OutputFormat::LinearRgbF32, cs)).unwrap();
        let rgb16 = raw.decode(opts(OutputFormat::Rgb16, cs)).unwrap();
        let rgba8 = raw.decode(opts(OutputFormat::Rgba8, cs)).unwrap();
        // The fixture is stored upright, so linear and display outputs share a pixel grid.
        assert_eq!(rgb16.orientation, flashraw::Orientation::Normal);
        assert_eq!(lin.orientation, flashraw::Orientation::Normal);
        let l = lin.as_f32().unwrap();
        let (w16, w8) = (rgb16.as_u16().unwrap(), rgba8.as_u8().unwrap());
        assert_eq!((l.len() / 3, w8.len() / 4), (w16.len() / 3, l.len() / 3));
        for (i, px) in l.chunks_exact(3).enumerate().step_by(7) {
            for c in 0..3 {
                let v = (px[c] as f64).clamp(0.0, 1.0);
                let want16 = cs.encode(v) * 65535.0;
                // Linear interpolation of the 16384-segment table: coarse only in the first
                // segments of the pure power curve (Adobe RGB), below 0.2 % of full scale, which
                // is under one 8-bit code.
                let tol16 = if v < 0.002 { 250.0 } else { 1.01 };
                assert!(
                    (w16[i * 3 + c] as f64 - want16).abs() <= tol16,
                    "{cs:?} 16-bit {v}: {} vs {want16}",
                    w16[i * 3 + c]
                );
                // 8-bit table resolution: the curve changes by less than one code per table step,
                // except in the first steps of the steep power curves.
                let want8 = cs.encode(v) * 255.0;
                let tol = if v < 0.01 { 3.5 } else { 1.01 };
                assert!((w8[i * 4 + c] as f64 - want8).abs() <= tol, "{cs:?} 8-bit {v}: {} vs {want8}", w8[i * 4 + c]);
            }
        }
    }
}

#[test]
fn regions_match_the_full_decode() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let rect = Rect::new(64, 48, 128, 64);
    for cs in [ColorSpace::DisplayP3, ColorSpace::ProPhoto, ColorSpace::AdobeRgb] {
        let o = DecodeOptions { crop: false, demosaic: Demosaic::Bilinear, ..opts(OutputFormat::LinearRgbF32, cs) };
        let full = raw.decode(o.clone()).unwrap();
        let part = raw.decode_region(rect, o).unwrap();
        let (fw, f) = (full.width as usize, full.as_f32().unwrap());
        let p = part.as_f32().unwrap();
        // Interior pixels (the region edge has less demosaic context).
        let mut compared = 0;
        for y in 2..rect.height as usize - 2 {
            for x in 2..rect.width as usize - 2 {
                let fi = ((y + rect.y as usize) * fw + x + rect.x as usize) * 3;
                let pi = (y * part.width as usize + x) * 3;
                assert_eq!(f[fi..fi + 3], p[pi..pi + 3], "{cs:?} at {x},{y}");
                compared += 1;
            }
        }
        assert!(compared > 1000);
    }
}

#[test]
fn frames_report_their_space_and_profile() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let def = raw.decode(DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() }).unwrap();
    assert_eq!(def.color_space, ColorSpace::Srgb);
    assert_eq!(def.icc_profile().unwrap(), ColorSpace::Srgb.icc_profile());
    let lin = raw.decode(opts(OutputFormat::LinearRgbF32, ColorSpace::DisplayP3)).unwrap();
    assert_eq!(lin.icc_profile().unwrap(), ColorSpace::DisplayP3.icc_profile_linear());
    let disp = raw.decode(opts(OutputFormat::Rgb16, ColorSpace::DisplayP3)).unwrap();
    assert_eq!(disp.icc_profile().unwrap(), ColorSpace::DisplayP3.icc_profile());
    let rawf = raw.decode(opts(OutputFormat::RawU16, ColorSpace::ProPhoto)).unwrap();
    assert!(rawf.icc_profile().is_none());
    assert!(matches!(&rawf.data, RawPixelData::U16(_)));
    // Custom needs a profile.
    let e = raw.decode(opts(OutputFormat::Rgba8, ColorSpace::Custom)).unwrap_err();
    assert!(matches!(e, Error::InvalidArgument(_)), "{e}");
    // Sensor-domain outputs ignore the colour space entirely (even Custom).
    assert!(raw.decode(opts(OutputFormat::RawU16, ColorSpace::Custom)).is_ok());
}

#[test]
fn profiles_without_the_feature_or_malformed_are_invalid_arguments() {
    let bytes = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let mut o = opts(OutputFormat::Rgba8, ColorSpace::Srgb);
    o.output_profile = Some(OutputProfile::new(vec![0u8; 500]));
    let e = raw.decode(o.clone()).unwrap_err();
    assert!(matches!(e, Error::InvalidArgument(_)), "{e}");
    o.output_profile = Some(OutputProfile::new(ColorSpace::DisplayP3.icc_profile()));
    let r = raw.decode(o);
    if flashraw::icc_supported() {
        let f = r.unwrap();
        assert_eq!(f.color_space, ColorSpace::Custom);
        let want = raw.decode(opts(OutputFormat::Rgba8, ColorSpace::DisplayP3)).unwrap();
        let (a, b) = (f.as_u8().unwrap(), want.as_u8().unwrap());
        assert!(a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= 1));
        assert_eq!(f.icc_profile().unwrap(), ColorSpace::DisplayP3.icc_profile());
    } else {
        assert!(r.unwrap_err().to_string().contains("built without ICC support"));
    }
}
