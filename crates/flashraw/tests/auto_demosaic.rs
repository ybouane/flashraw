//! `Demosaic::Auto` stands for AHD on Bayer sensors (edge-aware when a smoothing level is asked
//! for), the edge-aware demosaic on X-Trans sensors and bilinear elsewhere (a sensor without a colour filter array), whatever the output.
//!
//! Runs when the CC0 corpus is present (`$FLASHRAW_CORPUS` or `<repo>/corpus`); skipped otherwise.

use flashraw::{CfaColor, CfaPattern, DecodeOptions, Demosaic, FlashRaw, NoiseReduction, OutputFormat, Rect};
use std::path::PathBuf;

fn corpus() -> Option<PathBuf> {
    let c = std::env::var("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    c.exists().then_some(c)
}

#[test]
fn auto_resolves_by_pattern() {
    let bayer = CfaPattern::new(2, 2, &[CfaColor::Red, CfaColor::Green, CfaColor::Green, CfaColor::Blue]).unwrap();
    let g = CfaColor::Green;
    let (r, b) = (CfaColor::Red, CfaColor::Blue);
    let xtrans = CfaPattern::new(
        6,
        6,
        &[g, b, g, g, r, g, r, g, r, b, g, b, g, b, g, g, r, g, g, r, g, g, b, g, b, g, b, r, g, r, g, r, g, g, b, g],
    )
    .unwrap();
    assert_eq!(Demosaic::Auto.resolve(Some(&bayer)), Demosaic::Ahd);
    assert_eq!(Demosaic::Auto.resolve_for(Some(&bayer), NoiseReduction::Auto), Demosaic::Ahd);
    assert_eq!(Demosaic::Auto.resolve_for(Some(&bayer), NoiseReduction::Off), Demosaic::Ahd);
    assert_eq!(Demosaic::Auto.resolve_for(Some(&bayer), NoiseReduction::Medium), Demosaic::EdgeAware);
    assert_eq!(Demosaic::Auto.resolve_for(Some(&xtrans), NoiseReduction::Off), Demosaic::EdgeAware);
    assert_eq!(Demosaic::Ahd.resolve_for(Some(&bayer), NoiseReduction::High), Demosaic::Ahd);
    assert_eq!(Demosaic::Auto.resolve(Some(&xtrans)), Demosaic::EdgeAware);
    assert_eq!(Demosaic::Auto.resolve(None), Demosaic::Bilinear);
    for d in [Demosaic::Bilinear, Demosaic::HalfSize, Demosaic::EdgeAware, Demosaic::Ahd] {
        assert_eq!(d.resolve(Some(&bayer)), d);
    }
    assert_eq!("auto".parse::<Demosaic>(), Ok(Demosaic::Auto));
}

#[test]
fn auto_decodes_like_ahd_on_bayer_and_edge_aware_on_x_trans() {
    let Some(corpus) = corpus() else {
        eprintln!("corpus not found; skipping (see tools/corpus/fetch.py)");
        return;
    };
    for (rel, same) in
        [("Canon/EOS 40D/_MG_0153.CR2", Demosaic::Ahd), ("Fujifilm/X-T5/DSCF0021.RAF", Demosaic::EdgeAware)]
    {
        let path = corpus.join(rel);
        if !path.exists() {
            eprintln!("missing {rel}");
            continue;
        }
        let raw = FlashRaw::open_path(&path).unwrap();
        let region = Rect { x: 400, y: 300, width: 600, height: 400 };
        let run = |demosaic| {
            let f = raw
                .decode(DecodeOptions {
                    output: OutputFormat::Rgba8,
                    demosaic,
                    region: Some(region),
                    ..Default::default()
                })
                .unwrap();
            f.as_u8().unwrap().to_vec()
        };
        assert!(run(Demosaic::Auto) == run(same), "{rel}: auto differs from {same:?}");
        assert!(run(Demosaic::Auto) != run(Demosaic::Bilinear), "{rel}: auto equals bilinear");
    }
}
