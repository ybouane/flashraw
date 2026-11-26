//! `flashraw camera-db exposure`: measure the brightness offset of a camera's standard
//! rendering from its files.
//!
//! For every file this finds the offset (stops) that, applied before the standard tone, makes
//! the luminance of FlashRaw's rendering match that of the camera's own embedded JPEG over
//! the mid-tones (the 30th to 70th percentiles). Percentiles rather than pixels are compared,
//! so the JPEG need not line up with the raw data (lens corrections, crops). The camera's JPEG
//! is the maker's rendering of the same exposure; `tools/camera_db/exposure.py` aggregates the
//! output per model into the `exposures` table of `crates/flashraw-camera-db/data/cameras.json`.

use flashraw::process::ToneMap;
use flashraw::{DecodeOptions, Demosaic, FlashRaw, OutputFormat, PreviewEncoding, PreviewInfo, PreviewKind, Tone};
use serde_json::{Value, json};
use std::path::Path;

/// Luminance samples taken from each image.
const SAMPLES: usize = 120_000;
/// Percentiles compared.
const QUANTILES: [f64; 9] = [0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70];
/// Narrowest JPEG measured: previews from 320 pixels wide match about as well as full-size ones;
/// smaller thumbnails were not tested.
const MIN_JPEG_WIDTH: u32 = 320;

/// Measure one file; errors are reported in the returned object.
pub fn measure(path: &Path) -> Value {
    let raw = match FlashRaw::open_path(path) {
        Ok(r) => r,
        Err(e) => return json!({"file": path, "error": e.to_string()}),
    };
    let m = raw.metadata();
    let mut out = json!({
        "file": path,
        "format": format!("{:?}", raw.format()),
        "make": m.make,
        "model": m.model,
        "mode": m.extra("mode").unwrap_or(""),
        "canonical_make": m.canonical_make,
        "canonical_model": m.canonical_model,
        "iso": m.exposure.iso,
        "dng_baseline_exposure": m.dng.as_ref().and_then(|d| d.baseline_exposure),
    });
    let Some(p) = camera_jpeg(raw.previews()) else {
        out["error"] = json!("no embedded JPEG at least 320 pixels wide");
        return out;
    };
    out["jpeg"] = json!([p.width, p.height]);
    match offset(&raw, p) {
        Ok((ev, per_quantile)) => {
            out["ev"] = json!((ev * 1000.0).round() / 1000.0);
            out["quantile_ev"] =
                json!(per_quantile.iter().map(|e| e.map(|v| (v * 1000.0).round() / 1000.0)).collect::<Vec<_>>());
        }
        Err(e) => out["error"] = json!(e),
    }
    out
}

/// The largest embedded JPEG, full-size previews first.
fn camera_jpeg(previews: &[PreviewInfo]) -> Option<&PreviewInfo> {
    let jpegs = || previews.iter().filter(|p| p.encoding == PreviewEncoding::Jpeg && p.width >= MIN_JPEG_WIDTH);
    let area = |p: &&PreviewInfo| p.width as u64 * p.height as u64;
    jpegs().filter(|p| p.kind == PreviewKind::FullSize).max_by_key(area).or_else(|| jpegs().max_by_key(area))
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn luminance(p: [f32; 3]) -> f32 {
    (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).clamp(0.0, 1.0)
}

/// Every `n`-th item so that about `SAMPLES` remain.
fn stride(len: usize) -> usize {
    (len / SAMPLES).max(1)
}

fn quantile(v: &mut [f32], q: f64) -> f32 {
    let k = ((v.len() - 1) as f64 * q).round() as usize;
    *v.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
}

/// The offset over all compared percentiles (their median) and per percentile (`None` where
/// the JPEG is near black or white there).
fn offset(raw: &FlashRaw, p: &PreviewInfo) -> Result<(f64, Vec<Option<f64>>), String> {
    let img = raw.decode_preview_rgb(p).map_err(|e| e.to_string())?;
    let n = img.rgb.len() / 3;
    let mut jpeg: Vec<f32> = (0..n)
        .step_by(stride(n))
        .map(|i| {
            let c = |k: usize| srgb_to_linear(img.rgb[3 * i + k] as f32 / 255.0);
            luminance([c(0), c(1), c(2)])
        })
        .collect();
    let opts = DecodeOptions {
        output: OutputFormat::LinearRgbF32,
        demosaic: Demosaic::HalfSize,
        tone: Tone::Linear,
        ..Default::default()
    };
    let f = raw.decode(opts).map_err(|e| e.to_string())?;
    let rgb = f.as_f32().ok_or("no f32 output")?;
    let n = rgb.len() / 3;
    let lin: Vec<[f32; 3]> = (0..n).step_by(stride(n)).map(|i| [rgb[3 * i], rgb[3 * i + 1], rgb[3 * i + 2]]).collect();
    let mut buf = vec![0f32; lin.len()];
    // Toned luminance percentile at an offset; it grows with the offset.
    let mut toned = |ev: f64, q: f64| {
        let t = ToneMap::srgb(ev);
        for (o, &px) in buf.iter_mut().zip(&lin) {
            *o = luminance(t.apply(px));
        }
        quantile(&mut buf, q)
    };
    let mut per = Vec::with_capacity(QUANTILES.len());
    for &q in &QUANTILES {
        let target = quantile(&mut jpeg, q);
        if !(0.002..0.95).contains(&target) {
            per.push(None);
            continue;
        }
        let (mut lo, mut hi) = (-4.0f64, 4.0f64);
        if toned(lo, q) > target || toned(hi, q) < target {
            per.push(None);
            continue;
        }
        for _ in 0..22 {
            let mid = 0.5 * (lo + hi);
            if toned(mid, q) < target { lo = mid } else { hi = mid }
        }
        per.push(Some(0.5 * (lo + hi)));
    }
    let mut ok: Vec<f64> = per.iter().flatten().copied().collect();
    if ok.len() < 3 {
        return Err("the JPEG's mid-tones are too dark or too bright to compare".into());
    }
    ok.sort_by(f64::total_cmp);
    Ok((ok[ok.len() / 2], per))
}
