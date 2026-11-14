//! `Demosaic::Ahd` on real files: the same bytes for any thread count and backend request, a
//! different image than bilinear, and the edge-aware demosaic on sensors it does not cover.
//!
//! Runs when the CC0 corpus is present (`$FLASHRAW_CORPUS` or `<repo>/corpus`, fetched with
//! `tools/corpus/fetch.py`); skipped otherwise.

use flashraw::{Backend, DecodeOptions, Demosaic, FlashRaw, OutputFormat, Rect};
use std::path::PathBuf;

const BAYER: &str = "Canon/EOS 40D/_MG_0153.CR2";
const XTRANS: &str = "Fujifilm/X-T5/DSCF0021.RAF";

fn corpus() -> Option<PathBuf> {
    let c = std::env::var("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    c.exists().then_some(c)
}

fn opts(output: OutputFormat, demosaic: Demosaic) -> DecodeOptions {
    DecodeOptions { output, demosaic, ..Default::default() }
}

#[test]
fn ahd_is_deterministic_and_differs_from_the_other_demosaics() {
    let Some(corpus) = corpus() else {
        eprintln!("corpus not found; skipping (see tools/corpus/fetch.py)");
        return;
    };
    let path = corpus.join(BAYER);
    if !path.exists() {
        eprintln!("missing {BAYER}");
        return;
    }
    let raw = FlashRaw::open_path(&path).unwrap();
    let region = Rect { x: 400, y: 300, width: 700, height: 500 };
    for output in [OutputFormat::Rgba8, OutputFormat::LinearRgbF32] {
        let base = DecodeOptions { region: Some(region), ..opts(output, Demosaic::Ahd) };
        let reference = raw.decode(DecodeOptions { threads: Some(1), backend: Backend::Cpu, ..base.clone() }).unwrap();
        for (threads, backend) in [(Some(3), Backend::Cpu), (None, Backend::CpuSimd), (None, Backend::Auto)] {
            let again = raw.decode(DecodeOptions { threads, backend, ..base.clone() }).unwrap();
            assert!(again.data == reference.data, "{output:?}: {threads:?} {backend:?} differs");
        }
        // A GPU request is served by the CPU (or refused when there is no adapter).
        match raw.decode(DecodeOptions { backend: Backend::Gpu, ..base.clone() }) {
            // The GPU result is within rounding of the CPU result, not bit-identical (float outputs).
            Ok(f) => match (f.as_f32(), reference.as_f32()) {
                (Some(g), Some(c)) => {
                    let worst = g.iter().zip(c).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                    eprintln!("gpu vs cpu max abs diff {worst}");
                    assert!(g.len() == c.len() && worst <= 1e-5, "{output:?}: gpu request differs by {worst}");
                }
                _ => assert!(f.data == reference.data, "{output:?}: gpu request differs"),
            },
            Err(flashraw::Error::GpuUnavailable(_)) => {}
            Err(e) => panic!("{e}"),
        }
        for other in [Demosaic::Bilinear, Demosaic::EdgeAware] {
            let o = raw.decode(DecodeOptions { demosaic: other, ..base.clone() }).unwrap();
            assert_eq!((o.width(), o.height()), (reference.width(), reference.height()));
            assert!(o.data != reference.data, "ahd equals {other:?}");
        }
    }
    // Whole frame: the geometry of bilinear, no warnings.
    let full = raw.decode(opts(OutputFormat::Rgba8, Demosaic::Ahd)).unwrap();
    let bil = raw.decode(opts(OutputFormat::Rgba8, Demosaic::Bilinear)).unwrap();
    assert_eq!((full.width(), full.height()), (bil.width(), bil.height()));
    assert!(full.warnings().is_empty());
}

#[test]
fn ahd_uses_the_edge_aware_demosaic_on_x_trans() {
    let Some(corpus) = corpus() else { return };
    let path = corpus.join(XTRANS);
    if !path.exists() {
        return;
    }
    let raw = FlashRaw::open_path(&path).unwrap();
    let region = Rect { x: 400, y: 300, width: 300, height: 200 };
    let with = |d| DecodeOptions { region: Some(region), ..opts(OutputFormat::Rgba8, d) };
    let a = raw.decode(with(Demosaic::Ahd)).unwrap();
    let e = raw.decode(with(Demosaic::EdgeAware)).unwrap();
    assert!(a.data == e.data);
}
