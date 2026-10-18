//! `flashraw camera-db probe`: what a file states and what its sample data shows.
//!
//! One JSON object per file with the decoder's view of the file (geometry, CFA, levels,
//! colour matrices, masked areas), the sample statistics needed to check levels and
//! geometry against real data, and the camera-database entry the file resolves to.
//! `tools/camera_db/rederive.py` runs it with a normal build and with a build against an
//! empty database (`FLASHRAW_CAMERA_DB`, see docs/camera-db.md): whatever the empty build
//! reports is stated by the file itself.

use flashraw::{CfaPattern, DecodeOptions, FlashRaw, Rect};
use serde_json::{Value, json};
use std::path::Path;

/// Distinct-value count kept per channel, from the top of the range down.
const TOP_VALUES: usize = 12;
/// Columns / rows kept at each frame edge for the edge profiles.
const EDGE: usize = 192;

fn rect(r: Rect) -> Value {
    json!([r.x, r.y, r.width, r.height])
}

/// Channel index of the sample at `(x, y)`: the CFA colour, or the component for
/// multi-sample pixels.
fn channel(cfa: Option<&CfaPattern>, cpp: usize, x: usize, y: usize, comp: usize) -> usize {
    if cpp > 1 { comp } else { cfa.map(|c| c.color_at(x, y) as usize).unwrap_or(0) }
}

/// Probe one file; errors are reported in the returned object.
pub fn probe(path: &Path) -> Value {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let raw = match FlashRaw::open_path(path) {
        Ok(r) => r,
        Err(e) => return json!({"file": path, "size": size, "error": e.to_string()}),
    };
    let m = raw.metadata();
    let info = raw.frame_info();
    let mode = m.extra("mode").unwrap_or("");
    let entry = flashraw_camera_db::find(&m.make, &m.model, mode)
        .or_else(|| flashraw_camera_db::find_any(&m.make, &m.model))
        .map(|c| json!([c.make, c.model, c.mode]));
    let colour = &info.color_matrices;
    let matrices: Vec<Value> = colour
        .color
        .iter()
        .map(|c| json!({"illuminant": c.illuminant.0, "rows": c.rows, "values": c.values}))
        .collect();
    let mut out = json!({
        "file": path,
        "size": size,
        "format": raw.format().name(),
        "make": m.make,
        "model": m.model,
        "mode": mode,
        "entry": entry,
        "canonical_make": m.canonical_make,
        "canonical_model": m.canonical_model,
        "software": m.software,
        "unique_camera_model": m.unique_camera_model,
        "dng_version": m.dng_version,
        "compression": info.compression.name(),
        "raw": [info.width, info.height],
        "cpp": info.cpp,
        "bits": info.bits_per_sample,
        "cfa": info.cfa.map(|c| c.to_string()),
        "active_area": rect(info.active_area),
        "crop": rect(info.crop),
        "masked": info.masked_areas.iter().map(|r| rect(*r)).collect::<Vec<_>>(),
        "black": {"rows": info.black_level.rows, "cols": info.black_level.cols,
                   "samples": info.black_level.samples, "values": info.black_level.values,
                   "from_masked": info.black_from_masked},
        "white": info.white_level.values,
        "iso": m.exposure.iso,
        "orientation": m.orientation.exif(),
        "orientation_info": {"exif": m.orientation_info.exif.map(|o| o.exif())},
        "image_size": m.image_size().map(|(w, h)| json!([w, h])),
        "color": {"source": format!("{:?}", colour.source), "matrices": matrices},
    });
    match raw.decode(DecodeOptions::default()) {
        Ok(frame) => {
            if let Some(d) = frame.as_u16() {
                out["stats"] = stats(d, frame.width as usize, frame.height as usize, frame.cpp as usize, info);
            }
        }
        Err(e) => out["decode_error"] = json!(e.to_string()),
    }
    out
}

fn stats(d: &[u16], w: usize, h: usize, cpp: usize, info: &flashraw::FrameInfo) -> Value {
    let cfa = info.cfa.as_ref();
    let a = info.active_area.clamp_to(w as u32, h as u32);
    let (ax, ay, ar, ab) = (a.x as usize, a.y as usize, a.right() as usize, a.bottom() as usize);
    // Per-channel histograms of the active area.
    let mut hist: Vec<Vec<u32>> = Vec::new();
    for y in ay..ab {
        for x in ax..ar {
            for comp in 0..cpp {
                let c = channel(cfa, cpp, x, y, comp);
                if hist.len() <= c {
                    hist.resize_with(c + 1, || vec![0u32; 65536]);
                }
                hist[c][d[(y * w + x) * cpp + comp] as usize] += 1;
            }
        }
    }
    let channels: Vec<Value> = hist
        .iter()
        .enumerate()
        .filter_map(|(c, hs)| {
            let n: u64 = hs.iter().map(|&v| v as u64).sum();
            if n == 0 {
                return None;
            }
            let top: Vec<Value> = hs
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, k)| **k > 0)
                .take(TOP_VALUES)
                .map(|(v, k)| json!([v, *k]))
                .collect();
            let min = hs.iter().position(|&k| k > 0).unwrap_or(0);
            let (mut acc, mut p50, mut p9999) = (0u64, 0usize, 0usize);
            for (v, &k) in hs.iter().enumerate() {
                acc += k as u64;
                if p50 == 0 && acc * 2 >= n {
                    p50 = v;
                }
                if p9999 == 0 && acc * 10000 >= n * 9999 {
                    p9999 = v;
                }
            }
            Some(json!({"c": c, "n": n, "min": min, "p50": p50, "p9999": p9999, "top": top}))
        })
        .collect();
    // Declared masked areas: mean / standard deviation per channel.
    let masked: Vec<Value> = info
        .masked_areas
        .iter()
        .map(|r| {
            let r = r.clamp_to(w as u32, h as u32);
            let mut acc: Vec<(f64, f64, u64)> = Vec::new();
            for y in r.y as usize..r.bottom() as usize {
                for x in r.x as usize..r.right() as usize {
                    for comp in 0..cpp {
                        let c = channel(cfa, cpp, x, y, comp);
                        if acc.len() <= c {
                            acc.resize(c + 1, (0.0, 0.0, 0));
                        }
                        let v = d[(y * w + x) * cpp + comp] as f64;
                        acc[c].0 += v;
                        acc[c].1 += v * v;
                        acc[c].2 += 1;
                    }
                }
            }
            let per: Vec<Value> = acc
                .iter()
                .enumerate()
                .filter(|(_, (_, _, n))| *n > 0)
                .map(|(c, (s, s2, n))| {
                    let mean = s / *n as f64;
                    json!({"c": c, "n": n, "mean": mean, "std": (s2 / *n as f64 - mean * mean).max(0.0).sqrt()})
                })
                .collect();
            json!({"rect": rect(r), "channels": per})
        })
        .collect();
    // Column / row means at the frame edges, over the central half of the other axis
    // (all channels together): masked and unused borders show as steps against the image.
    let edge = EDGE.min(w / 2).min(h / 2);
    let (y0, y1) = (h / 4, h - h / 4);
    let (x0, x1) = (w / 4, w - w / 4);
    let col_mean = |x: usize| -> f64 {
        let mut s = 0f64;
        for y in y0..y1 {
            for comp in 0..cpp {
                s += d[(y * w + x) * cpp + comp] as f64;
            }
        }
        s / ((y1 - y0) * cpp).max(1) as f64
    };
    let row_mean = |y: usize| -> f64 {
        let mut s = 0f64;
        for x in x0..x1 {
            for comp in 0..cpp {
                s += d[(y * w + x) * cpp + comp] as f64;
            }
        }
        s / ((x1 - x0) * cpp).max(1) as f64
    };
    let r1 = |v: f64| (v * 10.0).round() / 10.0;
    json!({
        "channels": channels,
        "masked": masked,
        "edges": {
            "left": (0..edge).map(|x| r1(col_mean(x))).collect::<Vec<_>>(),
            "right": (0..edge).map(|i| r1(col_mean(w - 1 - i))).collect::<Vec<_>>(),
            "top": (0..edge).map(|y| r1(row_mean(y))).collect::<Vec<_>>(),
            "bottom": (0..edge).map(|i| r1(row_mean(h - 1 - i))).collect::<Vec<_>>(),
        },
    })
}
