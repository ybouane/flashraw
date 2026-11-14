//! DNG camera profiles: the profile embedded in a DNG, a `.dcp` round trip, determinism
//! across threads and backends, and sanity of the developed colours.

use flashraw::{Backend, CameraProfile, ColorSpace, DecodeOptions, Demosaic, FlashRaw, OutputFormat};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures").join(name)).unwrap()
}

fn opts(output: OutputFormat, profile: Option<CameraProfile>) -> DecodeOptions {
    DecodeOptions { output, demosaic: Demosaic::HalfSize, camera_profile: profile, ..Default::default() }
}

fn means(v: &[f32]) -> [f64; 3] {
    let n = (v.len() / 3) as f64;
    [0, 1, 2].map(|c| v.iter().skip(c).step_by(3).map(|&x| x as f64).sum::<f64>() / n)
}

#[test]
fn only_files_with_profile_data_have_an_embedded_profile() {
    let lossy = fixture("canon-5d3-lossy.dng");
    let p = FlashRaw::open(&lossy[..]).unwrap().embedded_camera_profile().expect("Adobe Standard profile");
    assert_eq!(p.name.as_deref(), Some("Adobe Standard"));
    assert_eq!(p.hue_sat_map.as_ref().map(|t| t.dims), Some([90, 30, 1]));
    assert!(p.hue_sat_map.as_ref().unwrap().data2.is_some());
    assert!(p.look_table.is_some());
    // Colour matrices alone are not a profile.
    let mlv = fixture("canon-5d3-mlv.dng");
    assert!(FlashRaw::open(&mlv[..]).unwrap().embedded_camera_profile().is_none());
}

#[test]
fn embedded_profile_develops_sane_finite_colours() {
    let bytes = fixture("canon-5d3-lossy.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let profile = raw.embedded_camera_profile().unwrap();
    for cs in [ColorSpace::Srgb, ColorSpace::DisplayP3, ColorSpace::ProPhoto] {
        let base = raw.decode(DecodeOptions { color_space: cs, ..opts(OutputFormat::LinearRgbF32, None) }).unwrap();
        let prof = raw
            .decode(DecodeOptions { color_space: cs, ..opts(OutputFormat::LinearRgbF32, Some(profile.clone())) })
            .unwrap();
        let (b, p) = (base.as_f32().unwrap(), prof.as_f32().unwrap());
        assert_eq!((base.width(), base.height()), (prof.width(), prof.height()));
        assert!(p.iter().all(|v| v.is_finite()), "{cs:?}: non-finite values");
        let (mb, mp) = (means(b), means(p));
        let (lb, lp) = (mb.iter().sum::<f64>() / 3.0, mp.iter().sum::<f64>() / 3.0);
        assert!((lp / lb - 1.0).abs() < 0.15, "{cs:?}: brightness {lb} -> {lp}");
        assert!(b != p, "{cs:?}: the profile changed nothing");
    }
}

#[test]
fn every_rgb_output_accepts_a_profile() {
    let bytes = fixture("canon-5d3-lossy.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let profile = raw.embedded_camera_profile().unwrap();
    for out in [
        OutputFormat::LinearRgbF32,
        OutputFormat::Rgba8,
        OutputFormat::Rgb16,
        OutputFormat::LinearRgb16,
        OutputFormat::LinearRgbF16,
    ] {
        let a = raw.decode(opts(out, None)).unwrap();
        let b = raw.decode(opts(out, Some(profile.clone()))).unwrap();
        assert_eq!((a.width(), a.height(), a.cpp), (b.width(), b.height(), b.cpp), "{out:?}");
        assert_ne!(a.data, b.data, "{out:?}");
    }
}

#[test]
fn dcp_round_trip_renders_identically() {
    let bytes = fixture("canon-5d3-lossy.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let embedded = raw.embedded_camera_profile().unwrap();
    let dcp = embedded.to_dcp();
    let from_file = CameraProfile::from_dcp(&dcp).unwrap();
    assert_eq!(from_file, embedded);
    let a = raw.decode(opts(OutputFormat::Rgba8, Some(embedded))).unwrap();
    let b = raw.decode(opts(OutputFormat::Rgba8, Some(from_file))).unwrap();
    assert_eq!(a.data, b.data);
}

#[test]
fn threads_and_backends_agree() {
    let bytes = fixture("canon-5d3-lossy.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let profile = raw.embedded_camera_profile().unwrap();
    let base = raw
        .decode(DecodeOptions {
            backend: Backend::Cpu,
            threads: Some(1),
            ..opts(OutputFormat::Rgba8, Some(profile.clone()))
        })
        .unwrap();
    // `Auto` decodes on the CPU when a profile is set.
    for (backend, threads) in [(Backend::Auto, None), (Backend::CpuSimd, None), (Backend::Cpu, None)] {
        let f =
            raw.decode(DecodeOptions { backend, threads, ..opts(OutputFormat::Rgba8, Some(profile.clone())) }).unwrap();
        assert_eq!(f.data, base.data, "{backend:?} {threads:?}");
    }
}

#[test]
fn raw_outputs_ignore_the_profile() {
    let bytes = fixture("canon-5d3-lossy.dng");
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let profile = raw.embedded_camera_profile().unwrap();
    let a = raw.decode(opts(OutputFormat::RawU16, None)).unwrap();
    let b = raw.decode(opts(OutputFormat::RawU16, Some(profile))).unwrap();
    assert_eq!(a.data, b.data);
}

#[test]
fn damaged_profiles_are_errors_not_panics() {
    assert!(CameraProfile::from_dcp(b"").is_err());
    assert!(CameraProfile::from_dcp(&fixture("canon-5d3-lossy.dng")).is_err());
}
