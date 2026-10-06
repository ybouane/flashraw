//! Colour of developed Sigma / Foveon X3F files: the as-shot white balance and colour
//! matrix from the CAMF block give renders whose colour balance follows the camera's
//! embedded preview.
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
        let frame = raw.decode(opts).unwrap();
        let (rg, bg) = ratios(frame.as_u8().unwrap(), 4);
        let preview = raw.preview().and_then(|p| raw.decode_preview_rgb(p).ok());
        let line = match &preview {
            // Renders are colorimetric (no camera colour mode or tone curve) and the
            // previews of some bodies are less saturated, so the means differ a little.
            Some(p) => {
                let (prg, pbg) = ratios(&p.rgb, 3);
                let ok = (rg - prg).abs() < 0.25 && (bg - pbg).abs() < 0.25;
                (ok, format!("R/G {rg:.3} B/G {bg:.3}, preview R/G {prg:.3} B/G {pbg:.3}"))
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
