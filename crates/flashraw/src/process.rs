//! Optional CPU post-processing for convenience outputs (normalised samples, linear
//! RGB, display RGBA8). This is deliberately small: FlashRaw is a decoder, not a
//! raw developer. All RGB outputs come from one fused pass over the sensor data
//! (black subtraction, scaling, demosaic, white balance, colour matrix, transfer
//! curve, orientation), with no full-size intermediates.
//!
//! The same operations exist as WGSL compute shaders in `flashraw-gpu`; tests check
//! both paths agree.

use crate::options::Demosaic;
use flashraw_core::*;

/// sRGB (D65) → XYZ.
const XYZ_FROM_SRGB: [[f32; 3]; 3] =
    [[0.412_453, 0.357_580, 0.180_423], [0.212_671, 0.715_160, 0.072_169], [0.019_334, 0.119_193, 0.950_227]];

fn mat_mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut r = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn mat_inv(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [(e * i - f * h) * inv, (c * h - b * i) * inv, (b * f - c * e) * inv],
        [(f * g - d * i) * inv, (a * i - c * g) * inv, (c * d - a * f) * inv],
        [(d * h - e * g) * inv, (b * g - a * h) * inv, (a * e - b * d) * inv],
    ])
}

/// Camera RGB → linear sRGB matrix from an XYZ→camera matrix (rows of camera←sRGB are normalised to 1 so white balance maps white to white).
pub fn camera_to_srgb(cm: &ColorMatrices) -> [[f32; 3]; 3] {
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let Some(m) = cm.preferred() else { return identity };
    if m.values.len() < 9 {
        return identity;
    }
    let cam_xyz = [
        [m.values[0], m.values[1], m.values[2]],
        [m.values[3], m.values[4], m.values[5]],
        [m.values[6], m.values[7], m.values[8]],
    ];
    let mut cam_rgb = mat_mul(&cam_xyz, &XYZ_FROM_SRGB);
    for row in cam_rgb.iter_mut() {
        let s: f32 = row.iter().sum();
        if s.abs() > 1e-6 {
            row.iter_mut().for_each(|v| *v /= s);
        }
    }
    mat_inv(&cam_rgb).unwrap_or(identity)
}

/// Measure per-position black levels from masked (optical black) areas.
/// Returns a pattern with the CFA's dimensions (or 1x1 without CFA), anchored at buffer (0, 0);
/// images with several samples per pixel get one value per sample.
pub fn black_from_masked(img: &RawImage, info: &FrameInfo) -> Option<BlackLevel> {
    black_from_masked_at(img, info, (0, 0))
}

/// [`black_from_masked`] for an image whose top-left is at `origin` in the sensor buffer
/// (region decodes): `info.masked_areas` are buffer coordinates, the pattern is still
/// anchored at buffer (0, 0).
pub fn black_from_masked_at(img: &RawImage, info: &FrameInfo, origin: (u32, u32)) -> Option<BlackLevel> {
    if info.masked_areas.is_empty() {
        return None;
    }
    let data = img.data.as_u16()?;
    if img.cpp != 1 {
        return black_per_sample(data, img, info, origin);
    }
    let (pw, ph) = info.cfa.map(|p| (p.width(), p.height())).unwrap_or((1, 1));
    let w = img.width as usize;
    let (ox, oy) = (origin.0 as usize, origin.1 as usize);
    let frame = Rect::new(origin.0, origin.1, img.width, img.height);
    // (row, x0, x1) spans of every masked area (buffer coordinates); rows are summed in
    // parallel with integer accumulators and no per-pixel modulo.
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    for r in &info.masked_areas {
        let Some(r) = r.intersect(&frame) else { continue };
        for y in r.y as usize..r.bottom() as usize {
            spans.push((y, r.x as usize, r.right() as usize));
        }
    }
    let partial = par::map(spans, true, |(y, x0, x1)| {
        let mut sum = vec![0u64; pw * ph];
        let mut cnt = vec![0u64; pw * ph];
        let row = &data[(y - oy) * w..(y - oy) * w + w];
        let base = (y % ph) * pw;
        for k in 0..pw {
            // First x >= x0 with x % pw == k.
            let mut x = x0 + (k + pw - x0 % pw) % pw;
            while x < x1 {
                sum[base + k] += row[x - ox] as u64;
                cnt[base + k] += 1;
                x += pw;
            }
        }
        (sum, cnt)
    });
    let mut sum = vec![0f64; pw * ph];
    let mut cnt = vec![0u64; pw * ph];
    for (s, c) in partial {
        for i in 0..pw * ph {
            sum[i] += s[i] as f64;
            cnt[i] += c[i];
        }
    }
    if cnt.contains(&0) {
        let total: u64 = cnt.iter().sum();
        if total == 0 {
            return None;
        }
        let mean = (sum.iter().sum::<f64>() / total as f64) as f32;
        return Some(BlackLevel::uniform(mean));
    }
    let values: Vec<f32> = sum.iter().zip(&cnt).map(|(s, &c)| (s / c as f64) as f32).collect();
    Some(BlackLevel::pattern(ph, pw, values))
}

/// Masked-area black of a multi-sample image (Foveon, linear RGB): one mean per sample.
fn black_per_sample(data: &[u16], img: &RawImage, info: &FrameInfo, origin: (u32, u32)) -> Option<BlackLevel> {
    let (w, cpp) = (img.width as usize, img.cpp as usize);
    let frame = Rect::new(origin.0, origin.1, img.width, img.height);
    let mut sum = vec![0u64; cpp];
    let mut cnt = 0u64;
    for r in &info.masked_areas {
        let Some(r) = r.intersect(&frame) else { continue };
        for y in r.y as usize..r.bottom() as usize {
            let row = (y - origin.1 as usize) * w;
            let px = &data
                [(row + r.x as usize - origin.0 as usize) * cpp..(row + r.right() as usize - origin.0 as usize) * cpp];
            for p in px.chunks_exact(cpp) {
                for (s, &v) in sum.iter_mut().zip(p) {
                    *s += v as u64;
                }
            }
            cnt += r.width as u64;
        }
    }
    if cnt == 0 {
        return None;
    }
    let values = sum.iter().map(|&s| (s as f64 / cnt as f64) as f32).collect();
    Some(BlackLevel { rows: 1, cols: 1, samples: cpp as u8, values, row_deltas: vec![], col_deltas: vec![] })
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}
fn lcm(a: usize, b: usize) -> usize {
    a / gcd(a, b) * b
}

/// Precomputed per-position calibration: `(v - black[pos]) * scale[pos]` gives
/// normalised [0,1] sensor values.
struct Calib {
    pw: usize,
    ph: usize,
    black: Vec<f32>,
    scale: Vec<f32>,
    row_deltas: Vec<f32>,
    col_deltas: Vec<f32>,
}

impl Calib {
    fn new(info: &FrameInfo, c: usize) -> Self {
        let (cw, chh) = info.cfa.map(|p| (p.width(), p.height())).unwrap_or((1, 1));
        let b = &info.black_level;
        let pw = lcm(cw, b.cols.max(1) as usize).min(64);
        let ph = lcm(chh, b.rows.max(1) as usize).min(64);
        let mut black = Vec::with_capacity(pw * ph);
        let mut scale = Vec::with_capacity(pw * ph);
        for y in 0..ph {
            for x in 0..pw {
                // Base pattern only; deltas are added separately.
                let bl = b
                    .values
                    .get(
                        ((y % b.rows.max(1) as usize) * b.cols.max(1) as usize + x % b.cols.max(1) as usize)
                            * b.samples.max(1) as usize
                            + c % b.samples.max(1) as usize,
                    )
                    .copied()
                    .unwrap_or(0.0);
                let color = info.cfa.map(|p| p.color_at(x, y) as usize).unwrap_or(c);
                let white = info.white_level.at(if info.white_level.values.len() > 1 {
                    color.min(info.white_level.values.len() - 1)
                } else {
                    0
                }) as f32;
                black.push(bl);
                scale.push(if white > bl { 1.0 / (white - bl) } else { 1.0 });
            }
        }
        Calib { pw, ph, black, scale, row_deltas: b.row_deltas.clone(), col_deltas: b.col_deltas.clone() }
    }
    #[inline(always)]
    fn norm(&self, v: f32, x: usize, y: usize) -> f32 {
        let i = (y % self.ph) * self.pw + x % self.pw;
        let mut b = self.black[i];
        if !self.row_deltas.is_empty() {
            b += self.row_deltas.get(y).copied().unwrap_or(0.0);
        }
        if !self.col_deltas.is_empty() {
            b += self.col_deltas.get(x).copied().unwrap_or(0.0);
        }
        (v - b) * self.scale[i]
    }
}

/// Normalise sensor samples to `[0, 1]` (black subtracted, white scaled), keeping layout.
pub fn normalize(img: &RawImage, info: &FrameInfo, origin: (u32, u32), parallel: bool) -> Vec<f32> {
    let cpp = img.cpp as usize;
    let calibs: Vec<Calib> = (0..cpp).map(|c| Calib::new(info, c)).collect();
    let w = img.width as usize;
    let mut out = vec![0f32; img.data.len()];
    let rl = w * cpp;
    par::for_each_row_chunk(&mut out, rl, 32, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(rl).enumerate() {
            let y = first + i;
            for x in 0..w {
                for c in 0..cpp {
                    let v = img.sample(x as u32, y as u32, c as u32);
                    row[x * cpp + c] = calibs[c].norm(v, x + origin.0 as usize, y + origin.1 as usize).max(0.0);
                }
            }
        }
    });
    out
}

/// Parameters of the fused develop pass.
pub struct DevelopParams {
    pub demosaic: Demosaic,
    pub crop: Rect,
    /// White balance multipliers per CFA colour index (R, G, B, other).
    pub wb: [f32; 4],
    pub cam_to_rgb: [[f32; 3]; 3],
    pub orientation: Orientation,
}

impl DevelopParams {
    pub fn new(info: &FrameInfo, demosaic: Demosaic, crop: Rect) -> Self {
        let wb = info.white_balance.map(|w| w.coeffs).unwrap_or([1.0; 4]);
        DevelopParams {
            demosaic,
            crop,
            wb,
            cam_to_rgb: camera_to_srgb(&info.color_matrices),
            orientation: info.orientation,
        }
    }
}

/// Output of [`develop`].
pub struct Developed {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<f32>,
}

/// Rows per destination strip processed by one task.
const STRIP: u32 = 16;

/// Same-colour neighbour offsets `(dx, dy)` and the reciprocal of their count.
type NeighbourList = (Vec<(isize, isize)>, f32);

/// Per-image state of the fused develop kernel.
struct Kernel<'a> {
    img: &'a RawImage,
    calibs: Vec<Calib>,
    origin: (usize, usize),
    cfa: Option<CfaPattern>,
    /// Same-colour 3x3 neighbour offsets per CFA position and channel, with 1/count.
    neigh: Vec<[NeighbourList; 3]>,
    pw: usize,
    ph: usize,
    wb: [f32; 4],
    m: [[f32; 3]; 3],
    crop: Rect,
    half: bool,
}

/// Small cache of normalised sensor rows (three are live at a time).
struct RowCache {
    rows: [(usize, Vec<f32>); 4],
    tick: [u64; 4],
    clock: u64,
}

impl RowCache {
    fn new(len: usize) -> Self {
        RowCache { rows: std::array::from_fn(|_| (usize::MAX, vec![0f32; len])), tick: [0; 4], clock: 0 }
    }
    fn get(&mut self, k: &Kernel, y: usize) -> usize {
        self.clock += 1;
        if let Some(i) = self.rows.iter().position(|r| r.0 == y) {
            self.tick[i] = self.clock;
            return i;
        }
        let i = (0..4).min_by_key(|&i| self.tick[i]).unwrap();
        self.tick[i] = self.clock;
        self.rows[i].0 = y;
        k.normalize_row(y, &mut self.rows[i].1);
        i
    }
}

impl Kernel<'_> {
    fn normalize_row(&self, y: usize, out: &mut [f32]) {
        let img = self.img;
        let (w, cpp) = (img.width as usize, img.cpp as usize);
        let (ox, oy) = self.origin;
        let has_deltas = self.calibs.iter().any(|c| !c.row_deltas.is_empty() || !c.col_deltas.is_empty());
        if !has_deltas && cpp > 1 && self.calibs.iter().all(|c| c.pw == 1 && c.ph == 1) {
            // Interleaved channels with constant per-channel calibration.
            let blk: Vec<f32> = self.calibs.iter().map(|c| c.black[0]).collect();
            let scl: Vec<f32> = self.calibs.iter().map(|c| c.scale[0]).collect();
            match &img.data {
                PixelData::U16(d) => {
                    for (o, px) in out.chunks_exact_mut(cpp).zip(d[y * w * cpp..(y + 1) * w * cpp].chunks_exact(cpp)) {
                        for c in 0..cpp {
                            o[c] = (px[c] as f32 - blk[c]) * scl[c];
                        }
                    }
                }
                PixelData::F32(d) => {
                    for (o, px) in out.chunks_exact_mut(cpp).zip(d[y * w * cpp..(y + 1) * w * cpp].chunks_exact(cpp)) {
                        for c in 0..cpp {
                            o[c] = (px[c] - blk[c]) * scl[c];
                        }
                    }
                }
            }
            return;
        }
        if has_deltas || cpp != 1 {
            for (i, o) in out.iter_mut().enumerate().take(w * cpp) {
                let v = match &img.data {
                    PixelData::U16(d) => d[y * w * cpp + i] as f32,
                    PixelData::F32(d) => d[y * w * cpp + i],
                };
                *o = self.calibs[i % cpp].norm(v, i / cpp + ox, y + oy);
            }
            return;
        }
        // Per-row black/scale table over one pattern period, then a modulo-free loop.
        let c = &self.calibs[0];
        let base = ((y + oy) % c.ph) * c.pw;
        let period = c.pw;
        let blk: Vec<f32> = (0..period).map(|i| c.black[base + (i + ox) % period]).collect();
        let scl: Vec<f32> = (0..period).map(|i| c.scale[base + (i + ox) % period]).collect();
        match &img.data {
            PixelData::U16(d) => {
                let row = &d[y * w..(y + 1) * w];
                for ((oc, rc), _) in out[..w].chunks_mut(period).zip(row.chunks(period)).zip(0..) {
                    for (k, (o, &v)) in oc.iter_mut().zip(rc).enumerate() {
                        *o = (v as f32 - blk[k]) * scl[k];
                    }
                }
            }
            PixelData::F32(d) => {
                let row = &d[y * w..(y + 1) * w];
                for (oc, rc) in out[..w].chunks_mut(period).zip(row.chunks(period)) {
                    for (k, (o, &v)) in oc.iter_mut().zip(rc).enumerate() {
                        *o = (v - blk[k]) * scl[k];
                    }
                }
            }
        }
    }

    #[inline(always)]
    fn finish(&self, rgb: [f32; 3]) -> [f32; 3] {
        let m = &self.m;
        let r = (rgb[0] * self.wb[0]).clamp(0.0, 1.0);
        let g = (rgb[1] * self.wb[1]).clamp(0.0, 1.0);
        let b = (rgb[2] * self.wb[2]).clamp(0.0, 1.0);
        [
            m[0][0] * r + m[0][1] * g + m[0][2] * b,
            m[1][0] * r + m[1][1] * g + m[1][2] * b,
            m[2][0] * r + m[2][1] * g + m[2][2] * b,
        ]
    }

    /// Develop output row `oy` (crop/half coordinates) into `out` (one RGB per output pixel).
    fn row(&self, cache: &mut RowCache, oy: usize, out: &mut [[f32; 3]]) {
        let img = self.img;
        let (w, h, cpp) = (img.width as usize, img.height as usize, img.cpp as usize);
        let (ox0, oy0) = (self.origin.0, self.origin.1);
        let color = |x: usize, y: usize| self.cfa.map(|p| (p.color_at(x + ox0, y + oy0) as usize).min(3)).unwrap_or(1);
        if self.half {
            let y0 = self.crop.y as usize + oy * 2;
            let r0 = cache.get(self, y0);
            let r1 = if y0 + 1 < h { Some(cache.get(self, y0 + 1)) } else { None };
            for (i, o) in out.iter_mut().enumerate() {
                let x0 = self.crop.x as usize + i * 2;
                let mut acc = [0f32; 4];
                let mut n = [0f32; 4];
                for (dy, ri) in [(0usize, Some(r0)), (1, r1)] {
                    let Some(ri) = ri else { continue };
                    let row = &cache.rows[ri].1;
                    for dx in 0..2 {
                        let x = x0 + dx;
                        if x >= w {
                            continue;
                        }
                        if cpp == 1 && self.cfa.is_some() {
                            let c = color(x, y0 + dy);
                            acc[c] += row[x];
                            n[c] += 1.0;
                        } else if cpp == 1 {
                            // Monochrome: average into the green slot, replicated below.
                            acc[1] += row[x];
                            n[1] += 1.0;
                        } else {
                            for c in 0..cpp.min(3) {
                                acc[c] += row[x * cpp + c];
                                n[c] += 1.0;
                            }
                        }
                    }
                }
                let mut rgb = [0f32; 3];
                for c in 0..3 {
                    rgb[c] = if n[c] > 0.0 { acc[c] / n[c] } else { 0.0 };
                }
                if cpp == 1 && self.cfa.is_none() {
                    rgb = [acc[1] / n[1].max(1.0); 3];
                }
                *o = self.finish(rgb);
            }
            return;
        }
        let y = self.crop.y as usize + oy;
        let cur = cache.get(self, y);
        if !(cpp == 1 && self.cfa.is_some()) {
            let row = &cache.rows[cur].1;
            for (i, o) in out.iter_mut().enumerate() {
                let x = self.crop.x as usize + i;
                let rgb = if cpp >= 3 { [row[x * cpp], row[x * cpp + 1], row[x * cpp + 2]] } else { [row[x * cpp]; 3] };
                *o = self.finish(rgb);
            }
            return;
        }
        let up = if y > 0 { Some(cache.get(self, y - 1)) } else { None };
        let down = if y + 1 < h { Some(cache.get(self, y + 1)) } else { None };
        let rows: [Option<&[f32]>; 3] =
            [up.map(|i| &cache.rows[i].1[..]), Some(&cache.rows[cur].1[..]), down.map(|i| &cache.rows[i].1[..])];
        let interior_y = up.is_some() && down.is_some();
        let (r_up, r_mid, r_dn) = (rows[0].unwrap_or(&[]), rows[1].unwrap(), rows[2].unwrap_or(&[]));
        let py = (y + oy0) % self.ph;
        let bayer = self.cfa.is_some_and(|p| p.is_bayer());
        // Bayer fast path: colour of this row's sites indexed by absolute column parity.
        let row_colors = [color((2 - ox0 % 2) % 2, y), color((3 - ox0 % 2) % 2, y)];
        for (i, o) in out.iter_mut().enumerate() {
            let x = self.crop.x as usize + i;
            if bayer && interior_y && x > 0 && x + 1 < w {
                let c0 = row_colors[(x + ox0) & 1];
                let own = r_mid[x];
                let (l, r) = (r_mid[x - 1], r_mid[x + 1]);
                let (u, d) = (r_up[x], r_dn[x]);
                let rgb = if c0 == 1 {
                    // Green site: horizontal neighbours carry this row's other colour.
                    let ch = row_colors[(x + ox0 + 1) & 1];
                    let mut v = [0f32; 3];
                    v[1] = own;
                    v[ch] = (l + r) * 0.5;
                    v[2 - ch] = (u + d) * 0.5;
                    v
                } else {
                    let orth = (l + r + u + d) * 0.25;
                    let diag = (r_up[x - 1] + r_up[x + 1] + r_dn[x - 1] + r_dn[x + 1]) * 0.25;
                    if c0 == 0 { [own, orth, diag] } else { [diag, orth, own] }
                };
                *o = self.finish(rgb);
                continue;
            }
            let lists = &self.neigh[py * self.pw + (x + ox0) % self.pw];
            let mut rgb = [0f32; 3];
            if interior_y && x > 0 && x + 1 < w {
                // Fast path: all 3x3 neighbours exist.
                for c in 0..3 {
                    let (list, inv) = &lists[c];
                    let mut s = 0f32;
                    for &(dx, dy) in list {
                        let r = match dy {
                            -1 => r_up,
                            0 => r_mid,
                            _ => r_dn,
                        };
                        s += r[(x as isize + dx) as usize];
                    }
                    rgb[c] = s * inv;
                }
            } else {
                // Border: average only neighbours inside the image.
                for c in 0..3 {
                    let mut s = 0f32;
                    let mut n = 0f32;
                    for &(dx, dy) in &lists[c].0 {
                        let xx = x as isize + dx;
                        if xx < 0 || xx as usize >= w {
                            continue;
                        }
                        if let Some(r) = rows[(dy + 1) as usize] {
                            s += r[xx as usize];
                            n += 1.0;
                        }
                    }
                    rgb[c] = if n > 0.0 { s / n } else { 0.0 };
                }
            }
            *o = self.finish(rgb);
        }
    }
}

fn kernel<'a>(
    img: &'a RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    params: &DevelopParams,
    crop: Rect,
) -> Kernel<'a> {
    let cpp = img.cpp as usize;
    let (pw, ph) = info.cfa.map(|p| (p.width(), p.height())).unwrap_or((1, 1));
    let mut neigh = Vec::with_capacity(pw * ph);
    for py in 0..ph {
        for px in 0..pw {
            let mut lists: [NeighbourList; 3] = Default::default();
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let c = info
                        .cfa
                        .map(|p| {
                            (p.color_at(
                                (px as isize + dx + pw as isize * 4) as usize,
                                (py as isize + dy + ph as isize * 4) as usize,
                            ) as usize)
                                .min(3)
                        })
                        .unwrap_or(1);
                    if c < 3 {
                        lists[c].0.push((dx, dy));
                    }
                }
            }
            // The site's own colour is measured, not interpolated.
            let own = info.cfa.map(|p| (p.color_at(px, py) as usize).min(3)).unwrap_or(1);
            if own < 3 {
                lists[own].0 = vec![(0, 0)];
            }
            for l in lists.iter_mut() {
                l.1 = if l.0.is_empty() { 0.0 } else { 1.0 / l.0.len() as f32 };
            }
            neigh.push(lists);
        }
    }
    Kernel {
        img,
        calibs: (0..cpp).map(|c| Calib::new(info, c)).collect(),
        origin: (origin.0 as usize, origin.1 as usize),
        cfa: info.cfa,
        neigh,
        pw,
        ph,
        wb: params.wb,
        m: params.cam_to_rgb,
        crop,
        half: params.demosaic == Demosaic::HalfSize,
    }
}

fn output_dims(img: &RawImage, params: &DevelopParams) -> Result<(Rect, u32, u32)> {
    let crop = params.crop.clamp_to(img.width, img.height);
    if crop.is_empty() {
        return Err(Error::InvalidArgument("empty develop region".into()));
    }
    let (ow, oh) = if params.demosaic == Demosaic::HalfSize {
        (crop.width / 2, crop.height / 2)
    } else {
        (crop.width, crop.height)
    };
    if ow == 0 || oh == 0 {
        return Err(Error::InvalidArgument("develop region too small".into()));
    }
    Ok((crop, ow, oh))
}

/// Demosaic + white balance + colour conversion to linear sRGB (f32 RGB), over `params.crop`
/// (frame coordinates). `origin` is the frame's position in sensor coordinates.
pub fn develop(
    img: &RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    params: &DevelopParams,
    parallel: bool,
) -> Result<Developed> {
    let (crop, ow, oh) = output_dims(img, params)?;
    let k = kernel(img, info, origin, params, crop);
    let mut out = vec![0f32; ow as usize * oh as usize * 3];
    let rl = ow as usize * 3;
    par::for_each_row_chunk(&mut out, rl, STRIP as usize, parallel, |first, chunk| {
        let mut cache = RowCache::new(img.width as usize * img.cpp as usize);
        let mut rgb = vec![[0f32; 3]; ow as usize];
        for (i, orow) in chunk.chunks_mut(rl).enumerate() {
            k.row(&mut cache, first + i, &mut rgb);
            for (o, p) in orow.chunks_exact_mut(3).zip(&rgb) {
                o.copy_from_slice(p);
            }
        }
    });
    Ok(Developed { width: ow, height: oh, rgb: out })
}

/// Fused develop straight to oriented sRGB RGBA8 (no intermediate RGB image).
/// Returns `(width, height, pixels)` after orientation.
pub fn develop_rgba8(
    img: &RawImage,
    info: &FrameInfo,
    origin: (u32, u32),
    params: &DevelopParams,
    parallel: bool,
) -> Result<(u32, u32, Vec<u8>)> {
    let (crop, w, h) = output_dims(img, params)?;
    let k = kernel(img, info, origin, params, crop);
    let lut = srgb_lut();
    let o = params.orientation;
    let swap = o.swaps_dimensions();
    let (dw, dh) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    // Destination strips: horizontal (rows) normally, vertical (columns) when transposing,
    // so each strip corresponds to a contiguous range of source rows.
    let (tw, th) = if swap { (STRIP, dh) } else { (dw, STRIP) };
    let grid = par::GridWriter::new(&mut out, dw, dh, 4, (0, 0), tw, th, dw, dh)
        .ok_or_else(|| Error::InvalidArgument("invalid output size".into()))?;
    let n = if swap { grid.tiles_x() } else { grid.tiles_y() } as usize;
    let (w, h) = (w as usize, h as usize);
    let enc = |p: [f32; 3]| -> [u8; 4] {
        [
            lut[(p[0].clamp(0.0, 1.0) * 4096.0) as usize],
            lut[(p[1].clamp(0.0, 1.0) * 4096.0) as usize],
            lut[(p[2].clamp(0.0, 1.0) * 4096.0) as usize],
            255,
        ]
    };
    par::for_each_index(n, parallel, |t| {
        let Some(mut tile) = (if swap { grid.claim(t as u32, 0) } else { grid.claim(0, t as u32) }) else { return };
        let mut cache = RowCache::new(img.width as usize * img.cpp as usize);
        let mut rgb = vec![[0f32; 3]; w];
        let (tw_, th_) = tile.size();
        if !swap {
            for ty in 0..th_ {
                let oy = t * STRIP as usize + ty;
                let sy = match o {
                    Orientation::Rotate180 | Orientation::FlipV => h - 1 - oy,
                    _ => oy,
                };
                k.row(&mut cache, sy, &mut rgb);
                let flip = matches!(o, Orientation::FlipH | Orientation::Rotate180);
                let row = tile.row(ty);
                for ox in 0..w {
                    let sx = if flip { w - 1 - ox } else { ox };
                    row[ox * 4..ox * 4 + 4].copy_from_slice(&enc(rgb[sx]));
                }
            }
        } else {
            for tx in 0..tw_ {
                let ox = t * STRIP as usize + tx;
                let sy = match o {
                    Orientation::Rotate90 | Orientation::Transverse => h - 1 - ox,
                    _ => ox,
                };
                k.row(&mut cache, sy, &mut rgb);
                let rev = matches!(o, Orientation::Rotate270 | Orientation::Transverse);
                for oy in 0..th_ {
                    let sx = if rev { w - 1 - oy } else { oy };
                    tile.row(oy)[tx * 4..tx * 4 + 4].copy_from_slice(&enc(rgb[sx]));
                }
            }
        }
    });
    drop(grid);
    Ok((dw, dh, out))
}

/// Linear → sRGB-encoded 8-bit lookup (4096 entries over [0, 1]).
fn srgb_lut() -> &'static [u8; 4097] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[u8; 4097]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut l = [0u8; 4097];
        for (i, v) in l.iter_mut().enumerate() {
            let x = i as f32 / 4096.0;
            let s = if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
            *v = (s * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
        l
    })
}

/// Encode linear RGB to sRGB RGBA8 and apply EXIF orientation.
pub fn to_rgba8(d: &Developed, orientation: Orientation, parallel: bool) -> (u32, u32, Vec<u8>) {
    let lut = srgb_lut();
    let (w, h) = (d.width as usize, d.height as usize);
    let (ow, oh) = if orientation.swaps_dimensions() { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh * 4];
    par::for_each_row_chunk(&mut out, ow * 4, 16, parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(ow * 4).enumerate() {
            let oy = first + i;
            for ox in 0..ow {
                // Map output pixel back to source pixel.
                let (sx, sy) = match orientation {
                    Orientation::Normal => (ox, oy),
                    Orientation::FlipH => (w - 1 - ox, oy),
                    Orientation::Rotate180 => (w - 1 - ox, h - 1 - oy),
                    Orientation::FlipV => (ox, h - 1 - oy),
                    Orientation::Transpose => (oy, ox),
                    Orientation::Rotate90 => (oy, h - 1 - ox),
                    Orientation::Transverse => (w - 1 - oy, h - 1 - ox),
                    Orientation::Rotate270 => (w - 1 - oy, ox),
                };
                let s = &d.rgb[(sy * w + sx) * 3..(sy * w + sx) * 3 + 3];
                let p = &mut row[ox * 4..ox * 4 + 4];
                for c in 0..3 {
                    p[c] = lut[(s[c].clamp(0.0, 1.0) * 4096.0) as usize];
                }
                p[3] = 255;
            }
        }
    });
    (ow as u32, oh as u32, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_matrix_without_color() {
        let m = camera_to_srgb(&ColorMatrices::default());
        assert_eq!(m[0], [1.0, 0.0, 0.0]);
    }

    #[test]
    fn matrix_maps_white_to_white() {
        // Any XYZ->cam matrix: after normalisation, camera white (1,1,1) maps to sRGB white.
        let cm = ColorMatrices {
            color: vec![
                ColorMatrix::new(Illuminant::D65, 3, vec![0.6, -0.1, -0.05, -0.4, 1.2, 0.2, -0.1, 0.25, 0.6]).unwrap(),
            ],
            ..Default::default()
        };
        let m = camera_to_srgb(&cm);
        for row in m {
            let s: f32 = row.iter().sum();
            assert!((s - 1.0).abs() < 1e-4, "{s}");
        }
    }

    #[test]
    fn bilinear_flat_field() {
        // A flat grey Bayer image develops to flat grey.
        let mut img = RawImage::new_u16(8, 8, 1, &Limits::default()).unwrap();
        img.u16_mut().iter_mut().for_each(|v| *v = 1000);
        let mut info = FrameInfo::new(8, 8, 12);
        info.cfa = Some(CfaPattern::RGGB);
        info.black_level = BlackLevel::uniform(0.0);
        info.white_level = WhiteLevel::uniform(4000);
        let p = DevelopParams::new(&info, Demosaic::Bilinear, Rect::full(8, 8));
        let d = develop(&img, &info, (0, 0), &p, false).unwrap();
        assert!(d.rgb.iter().all(|&v| (v - 0.25).abs() < 1e-5));
        let p = DevelopParams::new(&info, Demosaic::HalfSize, Rect::full(8, 8));
        let d = develop(&img, &info, (0, 0), &p, false).unwrap();
        assert_eq!((d.width, d.height), (4, 4));
    }

    #[test]
    fn fused_rgba8_matches_two_pass_for_all_orientations() {
        let (w, h) = (37u32, 23u32);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        for (i, v) in img.u16_mut().iter_mut().enumerate() {
            *v = ((i * 7919) % 3900 + 100) as u16;
        }
        for cfa in [CfaPattern::RGGB, CfaPattern::parse("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap()] {
            let mut info = FrameInfo::new(w, h, 12);
            info.cfa = Some(cfa);
            info.black_level = BlackLevel::uniform(64.0);
            info.white_level = WhiteLevel::uniform(4095);
            info.white_balance = WhiteBalance::from_multipliers(2.0, 1.0, 1.5, 1.0);
            for demosaic in [Demosaic::Bilinear, Demosaic::HalfSize] {
                for o in 1..=8 {
                    info.orientation = Orientation::from_exif(o);
                    let p = DevelopParams::new(&info, demosaic, Rect::new(1, 2, 33, 19));
                    let d = develop(&img, &info, (3, 1), &p, true).unwrap();
                    let a = to_rgba8(&d, info.orientation, false);
                    let b = develop_rgba8(&img, &info, (3, 1), &p, true).unwrap();
                    assert_eq!(a, b, "orientation {o} {demosaic:?}");
                }
            }
        }
    }

    /// Naive bilinear reference: own colour = centre sample; other colours = mean of the
    /// same-colour samples in the 3x3 neighbourhood that lie inside the image.
    fn naive(img: &RawImage, info: &FrameInfo, origin: (u32, u32)) -> Vec<[f32; 3]> {
        let (w, h) = (img.width as i64, img.height as i64);
        let cfa = info.cfa.unwrap();
        let col =
            |x: i64, y: i64| cfa.color_at((x + origin.0 as i64) as usize, (y + origin.1 as i64) as usize) as usize;
        let calib = Calib::new(info, 0);
        let v = |x: i64, y: i64| {
            calib.norm(
                img.u16()[(y * w + x) as usize] as f32,
                (x + origin.0 as i64) as usize,
                (y + origin.1 as i64) as usize,
            )
        };
        let mut out = vec![];
        for y in 0..h {
            for x in 0..w {
                let own = col(x, y);
                let mut rgb = [0f32; 3];
                for (c, slot) in rgb.iter_mut().enumerate() {
                    if c == own {
                        *slot = v(x, y);
                        continue;
                    }
                    let (mut s, mut n) = (0f32, 0f32);
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let (xx, yy) = (x + dx, y + dy);
                            if xx >= 0 && yy >= 0 && xx < w && yy < h && col(xx, yy) == c {
                                s += v(xx, yy);
                                n += 1.0;
                            }
                        }
                    }
                    *slot = if n > 0.0 { s / n } else { 0.0 };
                }
                out.push(rgb);
            }
        }
        out
    }

    #[test]
    fn bilinear_matches_naive_reference() {
        let (w, h) = (29u32, 17u32);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        for (i, v) in img.u16_mut().iter_mut().enumerate() {
            *v = ((i * 7919) % 3900 + 100) as u16;
        }
        let pats = [
            CfaPattern::RGGB,
            CfaPattern::GBRG,
            CfaPattern::GRBG,
            CfaPattern::BGGR,
            CfaPattern::parse("GGRGGB/GGBGGR/BRGRBG/GGBGGR/GGRGGB/RBGBRG").unwrap(),
        ];
        for cfa in pats {
            for origin in [(0, 0), (1, 0), (0, 1), (3, 5)] {
                let mut info = FrameInfo::new(w, h, 12);
                info.cfa = Some(cfa);
                info.black_level = BlackLevel::pattern2x2([60.0, 64.0, 62.0, 66.0]);
                info.white_level = WhiteLevel::uniform(4095);
                // Identity WB/matrix so the output is the demosaiced camera RGB.
                let mut p = DevelopParams::new(&info, Demosaic::Bilinear, Rect::full(w, h));
                p.cam_to_rgb = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
                let d = develop(&img, &info, origin, &p, false).unwrap();
                let r = naive(&img, &info, origin);
                for (i, px) in d.rgb.chunks(3).enumerate() {
                    for c in 0..3 {
                        let want = r[i][c].clamp(0.0, 1.0);
                        assert!(
                            (px[c] - want).abs() < 1e-5,
                            "{cfa} origin {origin:?} pixel {i} ch {c}: {} vs {want}",
                            px[c]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn half_size_monochrome_is_grey() {
        let mut img = RawImage::new_u16(8, 8, 1, &Limits::default()).unwrap();
        img.u16_mut().iter_mut().for_each(|v| *v = 1000);
        let mut info = FrameInfo::new(8, 8, 12);
        info.cfa = None;
        info.layout = SensorLayout::Monochrome;
        info.white_level = WhiteLevel::uniform(4000);
        let p = DevelopParams::new(&info, Demosaic::HalfSize, Rect::full(8, 8));
        let d = develop(&img, &info, (0, 0), &p, false).unwrap();
        assert!(d.rgb.iter().all(|&v| (v - 0.25).abs() < 1e-5), "{:?}", &d.rgb[..3]);
    }

    #[test]
    fn masked_black_per_position() {
        let mut img = RawImage::new_u16(10, 6, 1, &Limits::default()).unwrap();
        for y in 0..6 {
            for x in 0..10 {
                img.u16_mut()[y * 10 + x] = (100 + (y % 2) * 20 + (x % 2) * 10) as u16;
            }
        }
        let mut info = FrameInfo::new(10, 6, 12);
        info.cfa = Some(CfaPattern::RGGB);
        info.masked_areas = vec![Rect::new(1, 0, 3, 6), Rect::new(0, 4, 10, 2)];
        let b = black_from_masked(&img, &info).unwrap();
        assert_eq!(b.values, vec![100.0, 110.0, 120.0, 130.0]);
    }

    #[test]
    fn masked_black_per_sample() {
        // Three samples per pixel (Foveon): one black per sample, rows 0 and 5 masked.
        let mut img = RawImage::new_u16(4, 6, 3, &Limits::default()).unwrap();
        for (i, px) in img.u16_mut().chunks_exact_mut(3).enumerate() {
            let masked = i / 4 == 0 || i / 4 == 5;
            px.copy_from_slice(&if masked { [10 + (i % 2) as u16 * 2, 20, 30] } else { [500, 600, 700] });
        }
        let mut info = FrameInfo::new(4, 6, 12);
        info.layout = SensorLayout::Foveon;
        info.masked_areas = vec![Rect::new(0, 0, 4, 1), Rect::new(0, 5, 4, 1)];
        let b = black_from_masked(&img, &info).unwrap();
        assert_eq!((b.samples, b.values.clone()), (3, vec![11.0, 20.0, 30.0]));
        // Region decodes: the masked rows outside the region are not counted.
        let region = img.crop(Rect::new(0, 4, 4, 2)).unwrap();
        let b = black_from_masked_at(&region, &info, (0, 4)).unwrap();
        assert_eq!(b.values, vec![11.0, 20.0, 30.0]);
        info.masked_areas.clear();
        assert!(black_from_masked(&img, &info).is_none());
    }

    #[test]
    fn multichannel_fast_normalisation_matches_reference() {
        let (w, h) = (7u32, 3u32);
        let mut img = RawImage::new_u16(w, h, 3, &Limits::default()).unwrap();
        for (i, v) in img.u16_mut().iter_mut().enumerate() {
            *v = (i * 997 % 4000) as u16;
        }
        let mut info = FrameInfo::new(w, h, 12);
        info.cfa = None;
        info.cpp = 3;
        info.layout = SensorLayout::Linear;
        info.black_level =
            BlackLevel { rows: 1, cols: 1, samples: 3, values: vec![10.0, 20.0, 30.0], ..Default::default() };
        info.white_level = WhiteLevel { values: vec![4000, 3900, 3800] };
        let p = DevelopParams::new(&info, Demosaic::Bilinear, Rect::full(w, h));
        let k = kernel(&img, &info, (0, 0), &p, Rect::full(w, h));
        let mut row = vec![0f32; (w * 3) as usize];
        for y in 0..h as usize {
            k.normalize_row(y, &mut row);
            for x in 0..w as usize {
                for c in 0..3 {
                    let want = k.calibs[c].norm(img.sample(x as u32, y as u32, c as u32), x, y);
                    assert!((row[x * 3 + c] - want).abs() < 1e-6);
                }
            }
        }
    }

    #[test]
    fn orientation_rotation() {
        let d = Developed { width: 2, height: 1, rgb: vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0] };
        let (w, h, px) = to_rgba8(&d, Orientation::Rotate90, false);
        assert_eq!((w, h), (1, 2));
        // Rotate 90 CW: left pixel (red) ends on top.
        assert_eq!(&px[0..3], &[255, 0, 0]);
        assert_eq!(&px[4..7], &[0, 0, 255]);
    }
}
