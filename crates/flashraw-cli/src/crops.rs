//! `flashraw camera-db crops`: measure the camera's own image area from files.
//!
//! For every file this reports the active area, the default crop the decoder resolves
//! (and where it came from), the image size the file records, the embedded full-size
//! JPEG's size and, with `--register`, where that JPEG sits inside the active area: the
//! camera's own rendering is matched against a half-size development of the raw data by
//! normalised cross-correlation. `tools/camera_db_crops.py` aggregates the output into
//! `image_crops` entries of `camera-db/cameras.json`.

use flashraw::{DecodeOptions, Demosaic, FlashRaw, OutputFormat, PreviewEncoding, PreviewInfo, PreviewKind, Rect};
use serde_json::{Value, json};
use std::path::Path;

/// Measure one file; errors are reported in the returned object.
pub fn measure(path: &Path, register: bool, rgba: bool) -> Value {
    let raw = match FlashRaw::open_path(path) {
        Ok(r) => r,
        Err(e) => return json!({"file": path, "error": e.to_string()}),
    };
    let m = raw.metadata();
    let info = raw.frame_info();
    let rect = |r: Rect| json!([r.x, r.y, r.width, r.height]);
    let preview = full_size_jpeg(raw.previews());
    let mut out = json!({
        "file": path,
        "format": format!("{:?}", raw.format()),
        "make": m.make,
        "model": m.model,
        "mode": m.extra("mode").unwrap_or(""),
        "canonical_make": m.canonical_make,
        "canonical_model": m.canonical_model,
        "raw": [info.width, info.height],
        "cpp": info.cpp,
        "active_area": rect(info.active_area),
        "crop": rect(info.crop),
        "crop_source": m.extra(flashraw::EXTRA_CROP_SOURCE),
        "image_size": m.image_size().map(|(w, h)| json!([w, h])),
        "jpeg": preview.map(|p| json!([p.width, p.height])),
        "orientation": m.orientation.exif(),
        "width": m.width,
        "height": m.height,
    });
    // DNG keeps its own DefaultCrop; nothing to measure.
    if register && raw.format() != flashraw::RawFormat::Dng {
        if let Some(p) = preview {
            out["registration"] = match register_preview(&raw, p) {
                Ok(r) => r,
                Err(e) => json!({"error": e}),
            };
        }
    }
    if rgba {
        let opts = DecodeOptions { output: OutputFormat::Rgba8, demosaic: Demosaic::HalfSize, ..Default::default() };
        // Half-size output: report the full-size equivalent of the oriented crop.
        out["rgba"] = match raw.decode(opts) {
            Ok(f) => {
                let swap = m.orientation.exif() >= 5;
                let (w, h) =
                    if swap { (info.crop.height, info.crop.width) } else { (info.crop.width, info.crop.height) };
                json!({"width": f.width, "height": f.height, "expected_half": [w / 2, h / 2]})
            }
            Err(e) => json!({"error": e.to_string()}),
        };
    }
    out
}

/// The largest embedded full-size JPEG preview.
fn full_size_jpeg(previews: &[PreviewInfo]) -> Option<&PreviewInfo> {
    previews
        .iter()
        .filter(|p| p.encoding == PreviewEncoding::Jpeg && p.kind == PreviewKind::FullSize)
        .max_by_key(|p| p.width as u64 * p.height as u64)
}

/// A single-channel image.
struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Plane {
    fn at(&self, x: usize, y: usize) -> f32 {
        self.v[y * self.w + x]
    }
    /// Rotate by `quarter` clockwise quarter turns.
    fn rotated(&self, quarter: u32) -> Plane {
        let (w, h) = (self.w, self.h);
        match quarter % 4 {
            0 => Plane { w, h, v: self.v.clone() },
            2 => Plane { w, h, v: self.v.iter().rev().copied().collect() },
            q => {
                let mut v = vec![0f32; w * h];
                for y in 0..h {
                    for x in 0..w {
                        // Clockwise: (x, y) -> (h - 1 - y, x); counter-clockwise: (y, w - 1 - x).
                        let (nx, ny) = if q == 1 { (h - 1 - y, x) } else { (y, w - 1 - x) };
                        v[ny * h + nx] = self.at(x, y);
                    }
                }
                Plane { w: h, h: w, v }
            }
        }
    }
}

/// Where the camera's own JPEG lies inside the active area, in full-resolution pixels
/// relative to the active area's top-left corner.
fn register_preview(raw: &FlashRaw, p: &PreviewInfo) -> Result<Value, String> {
    let info = raw.frame_info();
    let aa = info.active_area;
    let img = raw.decode_preview_rgb(p).map_err(|e| e.to_string())?;
    // JPEG luma at half resolution (the raw is developed at half size).
    let (jw, jh) = (img.width as usize / 2, img.height as usize / 2);
    let mut jv = vec![0f32; jw * jh];
    for y in 0..jh {
        for x in 0..jw {
            let mut s = 0f32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let i = ((2 * y + dy) * img.width as usize + 2 * x + dx) * 3;
                s += 0.25 * img.rgb[i] as f32 + 0.5 * img.rgb[i + 1] as f32 + 0.25 * img.rgb[i + 2] as f32;
            }
            jv[y * jw + x] = s / 4.0;
        }
    }
    let jpeg = Plane { w: jw, h: jh, v: jv };
    let opts = DecodeOptions {
        output: OutputFormat::LinearRgbF32,
        demosaic: Demosaic::HalfSize,
        crop: false,
        ..Default::default()
    };
    let f = raw.decode(opts).map_err(|e| e.to_string())?;
    let rgb = f.as_f32().ok_or("no f32 output")?;
    let (rw, rh) = (f.width as usize, f.height as usize);
    let mut rv: Vec<f32> =
        rgb.chunks_exact(3).map(|c| (0.25 * c[0] + 0.5 * c[1] + 0.25 * c[2]).max(0.0).powf(1.0 / 2.2)).collect();
    rv.truncate(rw * rh);
    let dev = Plane { w: rw, h: rh, v: rv };
    // The active area in half-size coordinates.
    let (ax0, ay0) = (aa.x.div_ceil(2) as usize, aa.y.div_ceil(2) as usize);
    let (ax1, ay1) = (((aa.right() / 2) as usize).min(rw), ((aa.bottom() / 2) as usize).min(rh));
    let mut best: Option<(f64, u32, f64, f64, f64)> = None;
    for quarter in 0..4u32 {
        let j = jpeg.rotated(quarter);
        if j.w > ax1.saturating_sub(ax0) || j.h > ay1.saturating_sub(ay0) || j.w < 16 || j.h < 16 {
            continue;
        }
        let pts = sample_points(&j);
        let (mx, my) = (ax1 - ax0 - j.w, ay1 - ay0 - j.h);
        let score = |dx: usize, dy: usize| ncc(&j, &dev, &pts, ax0 + dx, ay0 + dy);
        // Coarse search on a grid, then refine around the best position.
        let step = ((mx + 1) * (my + 1) / 4000).max(1).isqrt().max(1);
        let mut top = (f64::MIN, 0usize, 0usize);
        for dy in (0..=my).step_by(step) {
            for dx in (0..=mx).step_by(step) {
                let s = score(dx, dy);
                if s > top.0 {
                    top = (s, dx, dy);
                }
            }
        }
        if step > 1 {
            let (_, cx, cy) = top;
            for dy in cy.saturating_sub(step)..=(cy + step).min(my) {
                for dx in cx.saturating_sub(step)..=(cx + step).min(mx) {
                    let s = score(dx, dy);
                    if s > top.0 {
                        top = (s, dx, dy);
                    }
                }
            }
        }
        let (s, dx, dy) = top;
        // Sub-pixel refinement by a parabola through the neighbours.
        let para = |a: f64, b: f64, c: f64| {
            let d = a - 2.0 * b + c;
            if d.abs() > 1e-12 { (0.5 * (a - c) / d).clamp(-0.5, 0.5) } else { 0.0 }
        };
        let fx = if dx > 0 && dx < mx { para(score(dx - 1, dy), s, score(dx + 1, dy)) } else { 0.0 };
        let fy = if dy > 0 && dy < my { para(score(dx, dy - 1), s, score(dx, dy + 1)) } else { 0.0 };
        // Full-resolution offset relative to the active area.
        let x = 2.0 * ((ax0 + dx) as f64 + fx) - aa.x as f64;
        let y = 2.0 * ((ay0 + dy) as f64 + fy) - aa.y as f64;
        if best.is_none_or(|b| s > b.0) {
            best = Some((s, quarter, x, y, 0.0));
        }
    }
    let (score, quarter, x, y, _) = best.ok_or("preview does not fit the active area")?;
    Ok(
        json!({"x": (x * 10.0).round() / 10.0, "y": (y * 10.0).round() / 10.0, "score": (score * 1000.0).round() / 1000.0,
        "rotation": quarter * 90}),
    )
}

/// High-gradient sample points of the central 80% of `j` (lens corrections that cameras
/// apply to their JPEGs move the corners most).
fn sample_points(j: &Plane) -> Vec<(usize, usize)> {
    let (x0, x1, y0, y1) = (j.w / 10 + 1, j.w - j.w / 10 - 1, j.h / 10 + 1, j.h - j.h / 10 - 1);
    let step = (((x1 - x0) * (y1 - y0)) / 60_000).max(1).isqrt().max(1);
    let mut g: Vec<(f32, usize, usize)> = Vec::new();
    for y in (y0..y1).step_by(step) {
        for x in (x0..x1).step_by(step) {
            let gx = j.at(x + 1, y) - j.at(x - 1, y);
            let gy = j.at(x, y + 1) - j.at(x, y - 1);
            g.push((gx * gx + gy * gy, x, y));
        }
    }
    g.sort_by(|a, b| b.0.total_cmp(&a.0));
    g.truncate((g.len() / 4).max(1000).min(g.len()));
    g.into_iter().map(|(_, x, y)| (x, y)).collect()
}

/// Normalised cross-correlation of `j` at `pts` with `dev` shifted by `(ox, oy)`.
fn ncc(j: &Plane, dev: &Plane, pts: &[(usize, usize)], ox: usize, oy: usize) -> f64 {
    let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for &(x, y) in pts {
        let a = j.at(x, y) as f64;
        let b = dev.at(x + ox, y + oy) as f64;
        sa += a;
        sb += b;
        saa += a * a;
        sbb += b * b;
        sab += a * b;
    }
    let n = pts.len() as f64;
    let cov = sab - sa * sb / n;
    let va = saa - sa * sa / n;
    let vb = sbb - sb * sb / n;
    if va <= 0.0 || vb <= 0.0 { 0.0 } else { cov / (va * vb).sqrt() }
}
