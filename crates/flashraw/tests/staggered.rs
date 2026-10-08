//! Staggered photosite lattices (Fujifilm SuperCCD RAF, DNG CFALayout 2-5): raw outputs
//! keep the stored grid, RGB outputs are resampled to square pixels with the camera's
//! aspect ratio and orientation.
//!
//! The SuperCCD part runs on corpus files (`$FLASHRAW_CORPUS` or `<repo>/corpus`) and is
//! skipped when they are absent; the DNG part is synthetic.

use flashraw::{DecodeOptions, Demosaic, FlashRaw, OutputFormat, SensorLayout};
use std::path::PathBuf;

fn corpus() -> PathBuf {
    std::env::var_os("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

/// Grey levels of `px` (`step` bytes per pixel) on an `n x m` grid of box averages,
/// normalised to zero mean and unit norm.
fn signature(px: &[u8], step: usize, w: usize, h: usize, n: usize, m: usize) -> Vec<f64> {
    let mut s = vec![0f64; n * m];
    let mut c = vec![0usize; n * m];
    for y in 0..h {
        for x in 0..w {
            let p = &px[(y * w + x) * step..];
            let i = (y * m / h) * n + x * n / w;
            s[i] += p[0] as f64 + p[1] as f64 + p[2] as f64;
            c[i] += 1;
        }
    }
    let v: Vec<f64> = s.iter().zip(&c).map(|(a, &b)| a / b.max(1) as f64).collect();
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let norm = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>().sqrt().max(1e-9);
    v.iter().map(|x| (x - mean) / norm).collect()
}

#[test]
fn superccd_renders_match_the_previews() {
    const FILES: &[&str] = &[
        "FinePix E550/RAW_FUJI_E550.RAF",
        "FinePix E900/RAW_FUJI_E900.RAF",
        "FinePix F700/RAW_FUJI_F700.RAF",
        "FinePix S100FS/DSCF8410.RAF",
        "FinePix S2Pro/DSCF0002.RAF",
        "FinePix S3Pro/DSCF3694.RAF",
        "FinePix S5000/2007_0912Image0022.RAF",
        "FinePix S5200/DSCF1523.RAF",
        "FinePix S5600/RAW_FUJI_S5600S5200.RAF",
        "FinePix S5Pro/2018_06150003.RAF",
        "FinePix S6000fd/DSCF1973.RAF",
        "FinePix S6500fd/DSCF1333.RAF",
        "FinePix S7000/dscf3648.raf",
        "FinePix S9500/20170430_0041.RAF",
        "FinePix S9600/2007-11-10--16-32-45-original.raf",
    ];
    let root = corpus().join("Fujifilm");
    let mut seen = 0;
    for rel in FILES {
        let path = root.join(rel);
        if !path.exists() {
            continue;
        }
        seen += 1;
        let raw = FlashRaw::open_path(&path).unwrap();
        let info = raw.frame_info().clone();
        assert_eq!(info.layout, SensorLayout::OtherCfa, "{rel}");
        // Raw outputs: the stored grid.
        let r = raw.decode(DecodeOptions::default()).unwrap();
        assert_eq!((r.width, r.height, r.sensor_layout), (info.width, info.height, SensorLayout::OtherCfa), "{rel}");
        // RGB: square pixels, oriented, with the aspect ratio and content of the preview.
        let opts = DecodeOptions { output: OutputFormat::Rgba8, demosaic: Demosaic::HalfSize, ..Default::default() };
        let f = raw.decode(opts).unwrap();
        let samples = info.active_area.width as f64 * info.active_area.height as f64 / 4.0;
        let pixels = f.width as f64 * f.height as f64;
        assert!((pixels / samples - 1.0).abs() < 0.02, "{rel}: {}x{} for {samples} samples", f.width, f.height);
        let p = raw.decode_preview_rgb(raw.preview().unwrap()).unwrap();
        let (ar, ap) = (f.width as f64 / f.height as f64, p.width as f64 / p.height as f64);
        assert!((ar / ap - 1.0).abs() < 0.015, "{rel}: aspect {ar:.3}, preview {ap:.3}");
        let a = signature(f.as_u8().unwrap(), 4, f.width as usize, f.height as usize, 24, 16);
        let b = signature(&p.rgb, 3, p.width as usize, p.height as usize, 24, 16);
        let ncc: f64 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        eprintln!("{rel}: {}x{} aspect {ar:.3} (preview {ap:.3}), correlation {ncc:.3}", f.width, f.height);
        assert!(ncc > 0.85, "{rel}: render does not match the preview ({ncc:.3})");
    }
    if seen == 0 {
        eprintln!("SuperCCD: corpus not found, skipped");
    }
}

// ---- synthetic staggered DNG -------------------------------------------------------------

const W: u32 = 64;
const H: u32 = 48;

fn shorts(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn longs(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn rationals(v: &[(u32, u32)]) -> Vec<u8> {
    v.iter().flat_map(|&(a, b)| a.to_le_bytes().into_iter().chain(b.to_le_bytes())).collect()
}

/// A single-IFD little-endian DNG: `W x H` 16-bit samples, a 4x2 CFA, `CFALayout` and
/// `DefaultScale` as given.
fn staggered_dng(layout: u16, scale: (u32, u32)) -> Vec<u8> {
    let data: Vec<u8> = (0..W * H).flat_map(|i| (((i % W) * 500 + (i / W) * 300) as u16).to_le_bytes()).collect();
    let mut e: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        (0x00FE, 4, 1, longs(&[0])),
        (0x0100, 4, 1, longs(&[W])),
        (0x0101, 4, 1, longs(&[H])),
        (0x0102, 3, 1, shorts(&[16])),
        (0x0103, 3, 1, shorts(&[1])),
        (0x0106, 3, 1, shorts(&[32803])),
        (0x010F, 2, 5, b"Test\0".to_vec()),
        (0x0110, 2, 6, b"Model\0".to_vec()),
        (0x0111, 4, 1, longs(&[0])),
        (0x0115, 3, 1, shorts(&[1])),
        (0x0116, 4, 1, longs(&[H])),
        (0x0117, 4, 1, longs(&[W * H * 2])),
        (0x828D, 3, 2, shorts(&[2, 4])),
        (0x828E, 1, 8, vec![0, 1, 2, 1, 2, 1, 0, 1]),
        (0xC612, 1, 4, vec![1, 4, 0, 0]),
        (0xC617, 3, 1, shorts(&[layout])),
        (0xC61A, 4, 1, longs(&[0])),
        (0xC61D, 4, 1, longs(&[65535])),
        (0xC61E, 5, 2, rationals(&[(scale.0, 1), (scale.1, 1)])),
    ];
    e.sort_by_key(|x| x.0);
    let ifd_end = 8 + 2 + e.len() * 12 + 4;
    let extra: usize = e.iter().filter(|x| x.3.len() > 4).map(|x| x.3.len() + (x.3.len() & 1)).sum();
    let strip = (ifd_end + extra) as u32;
    for x in e.iter_mut().filter(|x| x.0 == 0x0111) {
        x.3 = longs(&[strip]);
    }
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend((e.len() as u16).to_le_bytes());
    let mut ext = Vec::new();
    for (tag, typ, count, d) in &e {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        if d.len() <= 4 {
            let mut d = d.clone();
            d.resize(4, 0);
            out.extend(d);
        } else {
            out.extend(((ifd_end + ext.len()) as u32).to_le_bytes());
            ext.extend(d);
            if d.len() & 1 == 1 {
                ext.push(0);
            }
        }
    }
    out.extend(0u32.to_le_bytes());
    out.extend(ext);
    out.extend(data);
    out
}

#[test]
fn staggered_dng_rgb_has_square_pixels() {
    for (layout, scale, want) in [(2u16, (1, 2), (W, 2 * H)), (3, (1, 2), (W, 2 * H)), (4, (2, 1), (2 * W, H))] {
        let f = staggered_dng(layout, scale);
        let raw = FlashRaw::open(&f[..]).unwrap();
        assert_eq!(raw.frame_info().layout, SensorLayout::OtherCfa);
        let r = raw.decode(DecodeOptions::default()).unwrap();
        assert_eq!((r.width, r.height), (W, H), "raw output keeps the stored grid");
        let opts = DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() };
        let rgb = raw.decode(opts).unwrap();
        assert_eq!((rgb.width, rgb.height), want, "CFALayout {layout}");
    }
    // A rectangular layout is not resampled.
    let f = staggered_dng(1, (1, 1));
    let rgb =
        FlashRaw::open(&f[..]).unwrap().decode(DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() });
    assert_eq!(rgb.map(|r| (r.width, r.height)).unwrap(), (W, H));
}
