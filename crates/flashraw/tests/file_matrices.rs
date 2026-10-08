//! Colour matrices read from the file for bodies the camera database has no matrix for:
//! Phase One iXM/iXU backs (IIQ tag 0x0106) and the Leaf AFi-II 12 (MOS
//! `CaptProf_color_matrix`), both camera → ROMM matrices. Renders follow the colour balance
//! of the embedded preview.
//!
//! Runs on corpus files (`$FLASHRAW_CORPUS` or `<repo>/corpus`); skipped when absent.

use flashraw::{ColorSource, DecodeOptions, Demosaic, FlashRaw, OutputFormat};
use std::path::PathBuf;

fn corpus() -> PathBuf {
    std::env::var_os("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

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
fn medium_format_file_matrices() {
    const FILES: &[&str] = &[
        "Phase One/iXM-100/_848.IIQ",
        "Phase One/iXM-RS150F/RGB_86_0_4.IIQ",
        "Phase One/iXU180/cap_22908.IIQ",
        "Phase One AS/iXU-RS1000 NIR/RGB_791813.IIQ",
        "Leaf/AFi-II 12/L_000859.mos",
    ];
    let root = corpus();
    let mut seen = 0;
    for rel in FILES {
        let path = root.join(rel);
        if !path.exists() {
            continue;
        }
        seen += 1;
        let raw = FlashRaw::open_path(&path).unwrap();
        let cm = &raw.frame_info().color_matrices;
        assert_eq!(cm.source, ColorSource::File, "{rel}");
        assert_eq!(cm.color.len(), 1, "{rel}");
        let opts = DecodeOptions { output: OutputFormat::Rgba8, demosaic: Demosaic::HalfSize, ..Default::default() };
        let frame = raw.decode(opts).unwrap();
        let (rg, bg) = ratios(frame.as_u8().unwrap(), 4);
        let p = raw.decode_preview_rgb(raw.preview().unwrap()).unwrap();
        let (prg, pbg) = ratios(&p.rgb, 3);
        eprintln!("{rel}: R/G {rg:.3} B/G {bg:.3}, preview R/G {prg:.3} B/G {pbg:.3}");
        assert!((rg - prg).abs() < 0.2 && (bg - pbg).abs() < 0.2, "{rel}: colour balance off");
    }
    if seen == 0 {
        eprintln!("file matrices: corpus not found, skipped");
    }
}
