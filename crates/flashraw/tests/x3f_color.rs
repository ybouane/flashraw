//! Colour and exposure of developed Sigma / Foveon X3F files: the as-shot white balance,
//! colour matrix, lens shading and ISO gain from the CAMF block give renders whose colour
//! balance and exposure follow the camera's embedded preview.
//!
//! Runs on the X3F files of the CC0 corpus (`$FLASHRAW_CORPUS` or `<repo>/corpus`);
//! skipped when it is absent.

use flashraw::{ColorSource, DecodeOptions, Demosaic, FlashRaw, OutputFormat};
use std::path::{Path, PathBuf};

fn corpus() -> PathBuf {
    std::env::var_os("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

fn x3f_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for make in ["Sigma", "Polaroid"] {
        let Ok(models) = std::fs::read_dir(root.join(make)) else { continue };
        for m in models.flatten() {
            for f in std::fs::read_dir(m.path()).into_iter().flatten().flatten() {
                if f.path().extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f")) {
                    out.push(f.path());
                }
            }
        }
    }
    out.sort();
    out
}

/// Mean R/G and B/G of sRGB-encoded 8-bit pixels (`step` bytes per pixel).
fn ratios(px: &[u8], step: usize) -> (f64, f64) {
    let mut s = [0u64; 3];
    for p in px.chunks_exact(step) {
        for c in 0..3 {
            s[c] += p[c] as u64;
        }
    }
    let g = s[1].max(1) as f64;
    (s[0] as f64 / g, s[2] as f64 / g)
}

/// Mean linear luminance of sRGB-encoded 8-bit pixels.
fn luminance(px: &[u8], step: usize) -> f64 {
    let lin = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (mut s, mut n) = (0.0, 0usize);
    for p in px.chunks_exact(step) {
        s += 0.2126 * lin(p[0]) + 0.7152 * lin(p[1]) + 0.0722 * lin(p[2]);
        n += 1;
    }
    s / n.max(1) as f64
}

#[test]
fn renders_follow_the_preview_colour_balance() {
    let files = x3f_files(&corpus());
    if files.is_empty() {
        eprintln!("x3f colour: corpus not found, skipped");
        return;
    }
    let mut failures = Vec::new();
    for path in &files {
        let raw = FlashRaw::open_path(path).unwrap();
        let info = raw.frame_info();
        assert!(info.white_balance.is_some(), "{}: no white balance", path.display());
        assert_eq!(info.color_matrices.source, ColorSource::File, "{}", path.display());
        let opts = DecodeOptions { output: OutputFormat::Rgba8, demosaic: Demosaic::HalfSize, ..Default::default() };
        // Lens shading from the CAMF block for every generation but Quattro (the SD1
        // Merrill sample was taken with a lens its tables do not cover).
        let model = &raw.metadata().model;
        if model.contains("Quattro") || model.contains("SD1 Merrill") {
            assert!(info.opcodes.is_none(), "{}: unexpected lens shading", path.display());
        } else {
            assert!(info.opcodes.is_some(), "{}: no lens shading", path.display());
        }
        let frame = raw.decode(opts).unwrap();
        let (rg, bg) = ratios(frame.as_u8().unwrap(), 4);
        let preview = raw.preview().and_then(|p| raw.decode_preview_rgb(p).ok());
        let line = match &preview {
            // Renders are colorimetric (no camera colour mode or tone curve) and the
            // previews of some bodies are less saturated, so the means differ a little.
            Some(p) => {
                let (prg, pbg) = ratios(&p.rgb, 3);
                // Exposure: renders have no tone curve, so they sit about 1 EV under the
                // previews; ISO gain applied digitally by the camera is included.
                let ev = (luminance(frame.as_u8().unwrap(), 4) / luminance(&p.rgb, 3)).log2();
                let ok = (rg - prg).abs() < 0.25 && (bg - pbg).abs() < 0.25 && (-2.6..0.6).contains(&ev);
                (ok, format!("R/G {rg:.3} B/G {bg:.3}, preview R/G {prg:.3} B/G {pbg:.3}, {ev:+.2} EV"))
            }
            None => ((0.7..1.4).contains(&rg) && (0.7..1.4).contains(&bg), format!("R/G {rg:.3} B/G {bg:.3}")),
        };
        eprintln!("{}: {}", path.display(), line.1);
        if !line.0 {
            failures.push(format!("{}: {}", path.display(), line.1));
        }
    }
    assert!(failures.is_empty(), "colour balance off:\n{}", failures.join("\n"));
}

/// Saturated highlights render white, not tinted: the SD1 Merrill sample has a clipped
/// light source (its blue multiplier is below 1).
#[test]
fn clipped_highlights_are_neutral() {
    let path = corpus().join("Sigma/SIGMA SD1 Merrill/SDIM0042.X3F");
    if !path.exists() {
        eprintln!("x3f highlights: corpus file not found, skipped");
        return;
    }
    let raw = FlashRaw::open_path(&path).unwrap();
    let opts = DecodeOptions { output: OutputFormat::Rgba8, demosaic: Demosaic::HalfSize, ..Default::default() };
    let frame = raw.decode(opts).unwrap();
    let (mut s, mut n) = ([0f64; 3], 0usize);
    for p in frame.as_u8().unwrap().chunks_exact(4).filter(|p| p[..3].iter().any(|&v| v >= 250)) {
        for c in 0..3 {
            s[c] += p[c] as f64;
        }
        n += 1;
    }
    assert!(n > 1000, "no clipped area found");
    let (r, g, b) = (s[0] / n as f64, s[1] / n as f64, s[2] / n as f64);
    assert!(r > 0.9 * g && b > 0.9 * g && g > 0.9 * b, "highlights tinted: {r:.0} {g:.0} {b:.0}");
}
