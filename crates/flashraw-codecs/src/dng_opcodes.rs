//! DNG opcode lists (OpcodeList1/2/3, DNG spec 1.3+ chapter 7).
//!
//! Opcode lists carry processing steps the camera maker specifies: bad-pixel repair,
//! lens-shading gain maps (ubiquitous in smartphone DNGs), linearisation tables and
//! polynomials, per-row/column offsets and scales, and geometric corrections.
//!
//! FlashRaw parses all lists and applies the pixel-value opcodes to f32 planes (the
//! high-level API does so by default, each list in its own domain); geometric opcodes
//! (WarpRectilinear, WarpFisheye, TrimBounds) are parsed and reported but not applied.

use flashraw_core::bytes::Cursor;
use flashraw_core::{ByteOrder, Error, Result, par};

/// Area/plane selector shared by most pixel opcodes.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub top: u32,
    pub left: u32,
    pub bottom: u32,
    pub right: u32,
    pub plane: u32,
    pub planes: u32,
    pub row_pitch: u32,
    pub col_pitch: u32,
}

/// A parsed opcode.
#[derive(Debug, Clone, PartialEq)]
pub enum Opcode {
    WarpRectilinear,
    WarpFisheye,
    /// Radial gain `1 + k0 r^2 + k1 r^4 + ... + k4 r^10` around `(cx, cy)` (relative
    /// image coordinates), `r` normalised to 1 at the farthest image corner.
    FixVignetteRadial {
        k: [f64; 5],
        cx: f64,
        cy: f64,
    },
    FixBadPixelsConstant {
        constant: u32,
        bayer_phase: u32,
    },
    FixBadPixelsList {
        bayer_phase: u32,
        points: Vec<(u32, u32)>,
        rects: Vec<(u32, u32, u32, u32)>,
    },
    TrimBounds {
        top: u32,
        left: u32,
        bottom: u32,
        right: u32,
    },
    MapTable {
        region: Region,
        table: Vec<u16>,
    },
    MapPolynomial {
        region: Region,
        coeffs: Vec<f64>,
    },
    GainMap {
        region: Region,
        points_v: u32,
        points_h: u32,
        spacing_v: f64,
        spacing_h: f64,
        origin_v: f64,
        origin_h: f64,
        map_planes: u32,
        gains: Vec<f32>,
    },
    DeltaPerRow {
        region: Region,
        deltas: Vec<f32>,
    },
    DeltaPerColumn {
        region: Region,
        deltas: Vec<f32>,
    },
    ScalePerRow {
        region: Region,
        scales: Vec<f32>,
    },
    ScalePerColumn {
        region: Region,
        scales: Vec<f32>,
    },
    /// Unknown or unsupported opcode (id, flags); `optional` ones may be skipped.
    Other {
        id: u32,
        optional: bool,
    },
}

impl Opcode {
    pub fn name(&self) -> &'static str {
        match self {
            Opcode::WarpRectilinear => "WarpRectilinear",
            Opcode::WarpFisheye => "WarpFisheye",
            Opcode::FixVignetteRadial { .. } => "FixVignetteRadial",
            Opcode::FixBadPixelsConstant { .. } => "FixBadPixelsConstant",
            Opcode::FixBadPixelsList { .. } => "FixBadPixelsList",
            Opcode::TrimBounds { .. } => "TrimBounds",
            Opcode::MapTable { .. } => "MapTable",
            Opcode::MapPolynomial { .. } => "MapPolynomial",
            Opcode::GainMap { .. } => "GainMap",
            Opcode::DeltaPerRow { .. } => "DeltaPerRow",
            Opcode::DeltaPerColumn { .. } => "DeltaPerColumn",
            Opcode::ScalePerRow { .. } => "ScalePerRow",
            Opcode::ScalePerColumn { .. } => "ScalePerColumn",
            Opcode::Other { .. } => "Other",
        }
    }
    /// Whether [`apply`] implements this opcode.
    pub fn is_pixel_opcode(&self) -> bool {
        !matches!(
            self,
            Opcode::WarpRectilinear | Opcode::WarpFisheye | Opcode::TrimBounds { .. } | Opcode::Other { .. }
        )
    }
}

const MAX_ELEMENTS: usize = 1 << 24;

fn region(c: &mut Cursor) -> Result<Region> {
    Ok(Region {
        top: c.u32()?,
        left: c.u32()?,
        bottom: c.u32()?,
        right: c.u32()?,
        plane: c.u32()?,
        planes: c.u32()?,
        row_pitch: c.u32()?.max(1),
        col_pitch: c.u32()?.max(1),
    })
}

fn count(c: &mut Cursor, elem: usize) -> Result<usize> {
    let n = c.u32()? as usize;
    if n > MAX_ELEMENTS || n.saturating_mul(elem) > c.remaining() {
        return Err(Error::corrupt(format!("DNG opcode with {n} elements")));
    }
    Ok(n)
}

/// Parse an opcode list (always big-endian, per the DNG spec).
pub fn parse(data: &[u8]) -> Result<Vec<Opcode>> {
    let mut c = Cursor::new(data, ByteOrder::Big);
    let n = c.u32()? as usize;
    if n > 4096 {
        return Err(Error::corrupt(format!("DNG opcode list with {n} opcodes")));
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let id = c.u32()?;
        let _version = c.u32()?;
        let flags = c.u32()?;
        let len = c.u32()? as usize;
        let body = c.bytes(len)?;
        let mut b = Cursor::new(body, ByteOrder::Big);
        let op = match id {
            1 => Opcode::WarpRectilinear,
            2 => Opcode::WarpFisheye,
            3 => {
                let mut v = [0f64; 7];
                for x in &mut v {
                    *x = f64::from_bits(b.u64()?);
                }
                Opcode::FixVignetteRadial { k: [v[0], v[1], v[2], v[3], v[4]], cx: v[5], cy: v[6] }
            }
            4 => Opcode::FixBadPixelsConstant { constant: b.u32()?, bayer_phase: b.u32()? },
            5 => {
                let bayer_phase = b.u32()?;
                let np = b.u32()? as usize;
                let nr = b.u32()? as usize;
                if np.saturating_mul(8).saturating_add(nr.saturating_mul(16)) > b.remaining() {
                    return Err(Error::corrupt("FixBadPixelsList counts exceed opcode size"));
                }
                let points = (0..np).map(|_| Ok((b.u32()?, b.u32()?))).collect::<Result<Vec<_>>>()?;
                let rects =
                    (0..nr).map(|_| Ok((b.u32()?, b.u32()?, b.u32()?, b.u32()?))).collect::<Result<Vec<_>>>()?;
                Opcode::FixBadPixelsList { bayer_phase, points, rects }
            }
            6 => Opcode::TrimBounds { top: b.u32()?, left: b.u32()?, bottom: b.u32()?, right: b.u32()? },
            7 => {
                let region = region(&mut b)?;
                let n = count(&mut b, 2)?;
                let table = (0..n).map(|_| b.u16()).collect::<Result<Vec<_>>>()?;
                Opcode::MapTable { region, table }
            }
            8 => {
                let region = region(&mut b)?;
                let n = b.u32()? as usize;
                if n > 8 || (n + 1) * 8 > b.remaining() {
                    return Err(Error::corrupt("MapPolynomial degree"));
                }
                let coeffs = (0..=n).map(|_| Ok(f64::from_bits(b.u64()?))).collect::<Result<Vec<_>>>()?;
                Opcode::MapPolynomial { region, coeffs }
            }
            9 => {
                let region = region(&mut b)?;
                let points_v = b.u32()?;
                let points_h = b.u32()?;
                let spacing_v = f64::from_bits(b.u64()?);
                let spacing_h = f64::from_bits(b.u64()?);
                let origin_v = f64::from_bits(b.u64()?);
                let origin_h = f64::from_bits(b.u64()?);
                let map_planes = b.u32()?;
                let total =
                    (points_v as usize).saturating_mul(points_h as usize).saturating_mul(map_planes.max(1) as usize);
                if total == 0 || total > MAX_ELEMENTS || total * 4 > b.remaining() {
                    return Err(Error::corrupt("GainMap dimensions"));
                }
                let gains = (0..total).map(|_| Ok(f32::from_bits(b.u32()?))).collect::<Result<Vec<_>>>()?;
                Opcode::GainMap {
                    region,
                    points_v,
                    points_h,
                    spacing_v,
                    spacing_h,
                    origin_v,
                    origin_h,
                    map_planes: map_planes.max(1),
                    gains,
                }
            }
            10..=13 => {
                let region = region(&mut b)?;
                let n = count(&mut b, 4)?;
                let v = (0..n).map(|_| Ok(f32::from_bits(b.u32()?))).collect::<Result<Vec<_>>>()?;
                match id {
                    10 => Opcode::DeltaPerRow { region, deltas: v },
                    11 => Opcode::DeltaPerColumn { region, deltas: v },
                    12 => Opcode::ScalePerRow { region, scales: v },
                    _ => Opcode::ScalePerColumn { region, scales: v },
                }
            }
            _ => Opcode::Other { id, optional: flags & 1 != 0 },
        };
        out.push(op);
    }
    Ok(out)
}

/// A mutable f32 image the opcodes operate on: a `width x height` window (rows
/// `stride` pixels apart, `planes` interleaved samples per pixel) placed at
/// `(x0, y0)` in the coordinate system of the whole image the list describes, which is
/// `full_width x full_height` (GainMap positions are relative to it).
pub struct PlaneImage<'a> {
    pub data: &'a mut [f32],
    pub width: usize,
    pub height: usize,
    pub planes: usize,
    pub stride: usize,
    pub x0: usize,
    pub y0: usize,
    pub full_width: usize,
    pub full_height: usize,
}

impl<'a> PlaneImage<'a> {
    /// A whole, tightly packed image.
    pub fn new(data: &'a mut [f32], width: usize, height: usize, planes: usize) -> Self {
        PlaneImage { data, width, height, planes, stride: width, x0: 0, y0: 0, full_width: width, full_height: height }
    }

    /// Run `f(y, row)` for every row `y` (whole-image coordinates) of the window that
    /// `r` selects; `row` is the window row (first pixel at `x0`).
    fn rows(&mut self, r: &Region, parallel: bool, f: impl Fn(usize, &mut [f32]) + Sync + Send) {
        let (w, h, p, stride) = (self.width, self.height, self.planes, self.stride);
        let y0 = self.y0;
        let top = r.top as usize;
        let bottom = (r.bottom as usize).min(y0 + h);
        if bottom <= top.max(y0) || w == 0 || stride < w {
            return;
        }
        let rl = stride * p;
        let n = self.data.len().min(h * rl);
        par::for_each_row_chunk(&mut self.data[..n], rl, 16, parallel, |first, chunk| {
            for (i, row) in chunk.chunks_mut(rl).enumerate() {
                let y = y0 + first + i;
                if y >= top && y < bottom && (y - top) % r.row_pitch as usize == 0 {
                    let len = (w * p).min(row.len());
                    f(y, &mut row[..len]);
                }
            }
        });
    }

    /// Columns `x` (whole-image coordinates) of the window that `r` selects, with the
    /// sample offset of plane `r.plane` inside a window row.
    fn columns(&self, r: &Region) -> impl Iterator<Item = (usize, usize)> + Clone + '_ {
        let left = r.left as usize;
        let start = left.max(self.x0);
        // First selected column at or after the window start.
        let start = left + (start - left).div_ceil(r.col_pitch as usize) * r.col_pitch as usize;
        let right = (r.right as usize).min(self.x0 + self.width);
        let (x0, p, plane) = (self.x0, self.planes, r.plane as usize);
        (start..right.max(start)).step_by(r.col_pitch as usize).map(move |x| (x, (x - x0) * p + plane))
    }

    /// Number of selected planes that exist.
    fn plane_count(&self, r: &Region) -> usize {
        (r.plane as usize).saturating_add(r.planes as usize).min(self.planes).saturating_sub(r.plane as usize)
    }

    /// Apply `f(x, y, plane_index_within_region, sample)` to every selected sample.
    fn for_region(&mut self, r: &Region, parallel: bool, f: impl Fn(usize, usize, usize, &mut f32) + Sync + Send) {
        let cols: Vec<(usize, usize)> = self.columns(r).collect();
        let np = self.plane_count(r);
        if cols.is_empty() || np == 0 {
            return;
        }
        self.rows(r, parallel, |y, row| {
            for &(x, off) in &cols {
                for k in 0..np {
                    if let Some(v) = row.get_mut(off + k) {
                        f(x, y, k, v);
                    }
                }
            }
        });
    }
}

/// Apply pixel opcodes to `img`, whose samples are in the units the list expects
/// (OpcodeList1: stored values; OpcodeList2: linear reference values in [0,1];
/// OpcodeList3: demosaiced values). `cfa_period` is the CFA repeat (2 for Bayer) used by
/// the bad-pixel opcodes; `value_scale` maps sample values to the 16-bit table/constant
/// domain (1 for stored values, 65535 for normalised ones). Returns the names of
/// opcodes that were not applied (geometric and unknown ones).
pub fn apply(
    ops: &[Opcode],
    img: &mut PlaneImage,
    cfa_period: usize,
    value_scale: f32,
    parallel: bool,
) -> Vec<&'static str> {
    let mut skipped = Vec::new();
    for op in ops {
        match op {
            Opcode::MapTable { region, table } if !table.is_empty() => {
                let last = (table.len() - 1) as f32;
                img.for_region(region, parallel, |_, _, _, v| {
                    let i = (*v * value_scale).round().clamp(0.0, last) as usize;
                    *v = table[i] as f32 / value_scale;
                });
            }
            Opcode::MapTable { .. } => {}
            Opcode::MapPolynomial { region, coeffs } => img.for_region(region, parallel, |_, _, _, v| {
                // Polynomials map values normalised to [0, 1].
                let x = (*v * value_scale / 65535.0) as f64;
                let mut acc = 0f64;
                for c in coeffs.iter().rev() {
                    acc = acc * x + c;
                }
                *v = (acc * 65535.0 / value_scale as f64) as f32;
            }),
            Opcode::DeltaPerRow { region, deltas } => img.for_region(region, parallel, |_, y, _, v| {
                let i = (y - region.top as usize) / region.row_pitch as usize;
                *v += deltas.get(i).copied().unwrap_or(0.0) * 65535.0 / value_scale;
            }),
            Opcode::DeltaPerColumn { region, deltas } => img.for_region(region, parallel, |x, _, _, v| {
                let i = (x - region.left as usize) / region.col_pitch as usize;
                *v += deltas.get(i).copied().unwrap_or(0.0) * 65535.0 / value_scale;
            }),
            Opcode::ScalePerRow { region, scales } => img.for_region(region, parallel, |_, y, _, v| {
                let i = (y - region.top as usize) / region.row_pitch as usize;
                *v *= scales.get(i).copied().unwrap_or(1.0);
            }),
            Opcode::ScalePerColumn { region, scales } => img.for_region(region, parallel, |x, _, _, v| {
                let i = (x - region.left as usize) / region.col_pitch as usize;
                *v *= scales.get(i).copied().unwrap_or(1.0);
            }),
            Opcode::GainMap {
                region,
                points_v,
                points_h,
                spacing_v,
                spacing_h,
                origin_v,
                origin_h,
                map_planes,
                gains,
            } => {
                let (pv, ph, mp) = (*points_v as usize, *points_h as usize, *map_planes as usize);
                if pv == 0 || ph == 0 || mp == 0 || gains.len() < pv * ph * mp {
                    continue;
                }
                let (fw, fh) = (img.full_width.max(1) as f64, img.full_height.max(1) as f64);
                // Map positions are relative to the whole image (0..1). Horizontal
                // interpolation weights are per column; vertical ones per row.
                let pos = |rel: f64, origin: f64, spacing: f64, n: usize| -> (usize, usize, f32) {
                    let f = if spacing > 0.0 { ((rel - origin) / spacing).clamp(0.0, (n - 1) as f64) } else { 0.0 };
                    let i0 = (f.floor() as usize).min(n - 1);
                    (i0, (i0 + 1).min(n - 1), (f - i0 as f64) as f32)
                };
                let cols: Vec<(usize, usize, usize, f32)> = img
                    .columns(region)
                    .map(|(x, off)| {
                        let (h0, h1, t) = pos(x as f64 / fw, *origin_h, *spacing_h, ph);
                        (off, h0, h1, t)
                    })
                    .collect();
                let np = img.plane_count(region);
                if cols.is_empty() || np == 0 {
                    continue;
                }
                img.rows(region, parallel, |y, row| {
                    let (v0, v1, tv) = pos(y as f64 / fh, *origin_v, *spacing_v, pv);
                    let mut line = vec![0f32; ph];
                    for k in 0..np {
                        let m = k.min(mp - 1);
                        for (hi, l) in line.iter_mut().enumerate() {
                            let a = gains[(v0 * ph + hi) * mp + m];
                            let b = gains[(v1 * ph + hi) * mp + m];
                            *l = a + (b - a) * tv;
                        }
                        for &(off, h0, h1, th) in &cols {
                            if let Some(v) = row.get_mut(off + k) {
                                *v *= line[h0] + (line[h1] - line[h0]) * th;
                            }
                        }
                    }
                });
            }
            Opcode::FixVignetteRadial { k, cx, cy } => {
                let (fw, fh) = (img.full_width as f64, img.full_height as f64);
                let (cx, cy) = (cx * fw, cy * fh);
                // Squared distance to the farthest corner normalises r to 1 there.
                let m2 = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)]
                    .iter()
                    .map(|&(x, y): &(f64, f64)| (x - cx).powi(2) + (y - cy).powi(2))
                    .fold(0.0, f64::max);
                if m2 <= 0.0 || !m2.is_finite() {
                    continue;
                }
                let all = Region {
                    top: 0,
                    left: 0,
                    bottom: img.full_height as u32,
                    right: img.full_width as u32,
                    plane: 0,
                    planes: img.planes as u32,
                    row_pitch: 1,
                    col_pitch: 1,
                };
                let np = img.planes;
                let x0 = img.x0;
                img.rows(&all, parallel, |y, row| {
                    let dy2 = (y as f64 - cy).powi(2);
                    for (i, px) in row.chunks_exact_mut(np).enumerate() {
                        let r2 = (((x0 + i) as f64 - cx).powi(2) + dy2) / m2;
                        let g = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * k[4]))));
                        for v in px {
                            *v *= g as f32;
                        }
                    }
                });
            }
            Opcode::FixBadPixelsConstant { constant, bayer_phase } => {
                let c = *constant as f32 / value_scale;
                fix_bad_pixels(img, cfa_period, *bayer_phase, |v| v == c, &[], &[], parallel);
            }
            Opcode::FixBadPixelsList { bayer_phase, points, rects } => {
                fix_bad_pixels(img, cfa_period, *bayer_phase, |_| false, points, rects, parallel);
            }
            other => skipped.push(other.name()),
        }
    }
    skipped
}

/// Replace bad pixels by the mean of the nearest same-colour neighbours (distance =
/// CFA period) that are not themselves bad. Coordinates are whole-image ones.
fn fix_bad_pixels(
    img: &mut PlaneImage,
    period: usize,
    _phase: u32,
    is_bad_value: impl Fn(f32) -> bool + Sync,
    points: &[(u32, u32)],
    rects: &[(u32, u32, u32, u32)],
    parallel: bool,
) {
    let (w, h, p, stride) = (img.width, img.height, img.planes, img.stride);
    let (x0, y0) = (img.x0, img.y0);
    if w == 0 || h == 0 || stride < w || img.data.len() < ((h - 1) * stride + w) * p {
        return;
    }
    let period = period.max(1);
    let at = |x: usize, y: usize| (y * stride + x) * p;
    // Bad pixels are sparse: collect their window indices (y * w + x), sorted.
    let data = &*img.data;
    let per_row: Vec<Vec<usize>> = par::map((0..h).collect(), parallel, |y| {
        (0..w).filter(|&x| is_bad_value(data[at(x, y)])).map(|x| y * w + x).collect()
    });
    let mut bad: Vec<usize> = per_row.into_iter().flatten().collect();
    let mut mark = |row: usize, col: usize| {
        if row >= y0 && col >= x0 && row - y0 < h && col - x0 < w {
            bad.push((row - y0) * w + col - x0);
        }
    };
    for &(row, col) in points {
        mark(row as usize, col as usize);
    }
    for &(t, l, bm, r) in rects {
        for y in (t as usize).max(y0)..(bm as usize).min(y0 + h) {
            for x in (l as usize).max(x0)..(r as usize).min(x0 + w) {
                mark(y, x);
            }
        }
    }
    if bad.is_empty() {
        return;
    }
    bad.sort_unstable();
    bad.dedup();
    let is_bad = |i: usize| bad.binary_search(&i).is_ok();
    let mut fixes = Vec::new();
    for &i in &bad {
        let (x, y) = (i % w, i / w);
        for pl in 0..p {
            let (mut s, mut n) = (0f32, 0f32);
            for (dx, dy) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
                let xx = x as isize + dx * period as isize;
                let yy = y as isize + dy * period as isize;
                if xx >= 0
                    && yy >= 0
                    && (xx as usize) < w
                    && (yy as usize) < h
                    && !is_bad(yy as usize * w + xx as usize)
                {
                    s += img.data[at(xx as usize, yy as usize) + pl];
                    n += 1.0;
                }
            }
            if n > 0.0 {
                fixes.push((at(x, y) + pl, s / n));
            }
        }
    }
    for (i, v) in fixes {
        img.data[i] = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(id: u32, body: &[u8]) -> Vec<u8> {
        let mut v = id.to_be_bytes().to_vec();
        v.extend(0x01030000u32.to_be_bytes());
        v.extend(0u32.to_be_bytes());
        v.extend((body.len() as u32).to_be_bytes());
        v.extend(body);
        v
    }
    fn region_bytes(t: u32, l: u32, b: u32, r: u32) -> Vec<u8> {
        [t, l, b, r, 0, 1, 1, 1].iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    #[test]
    fn gain_map_and_scale() {
        let mut body = region_bytes(0, 0, 2, 2);
        body.extend(2u32.to_be_bytes());
        body.extend(2u32.to_be_bytes());
        for f in [1.0f64, 1.0, 0.0, 0.0] {
            body.extend(f.to_bits().to_be_bytes());
        }
        body.extend(1u32.to_be_bytes());
        for g in [1.0f32, 2.0, 3.0, 4.0] {
            body.extend(g.to_bits().to_be_bytes());
        }
        let mut list = 2u32.to_be_bytes().to_vec();
        list.extend(op(9, &body));
        let mut sb = region_bytes(0, 0, 2, 2);
        sb.extend(2u32.to_be_bytes());
        sb.extend(1.0f32.to_bits().to_be_bytes());
        sb.extend(0.5f32.to_bits().to_be_bytes());
        list.extend(op(12, &sb));
        let ops = parse(&list).unwrap();
        assert_eq!(ops.len(), 2);
        let mut data = vec![1.0f32; 4];
        let mut img = PlaneImage::new(&mut data, 2, 2, 1);
        assert!(apply(&ops, &mut img, 2, 65535.0, false).is_empty());
        // gain at (0,0)=1, (0,1)=1.5 (half way to 2), row 1 scaled by 0.5 after gain.
        assert_eq!(data[0], 1.0);
        assert!((data[1] - 1.5).abs() < 1e-6);
    }

    #[test]
    fn malformed_lists_error() {
        assert!(parse(&[0, 0, 0, 1]).is_err());
        let mut list = 1u32.to_be_bytes().to_vec();
        list.extend(op(9, &[0; 20]));
        assert!(parse(&list).is_err());
        let mut list = 1u32.to_be_bytes().to_vec();
        let mut body = region_bytes(0, 0, 1, 1);
        body.extend(u32::MAX.to_be_bytes());
        list.extend(op(10, &body));
        assert!(parse(&list).is_err());
    }

    #[test]
    fn bad_pixel_list() {
        let mut data: Vec<f32> = (0..36).map(|i| i as f32).collect();
        data[2 * 6 + 2] = 1000.0;
        let ops = vec![Opcode::FixBadPixelsList { bayer_phase: 0, points: vec![(2, 2)], rects: vec![] }];
        let mut img = PlaneImage::new(&mut data, 6, 6, 1);
        apply(&ops, &mut img, 2, 1.0, false);
        assert!(data[14] < 100.0);
    }

    fn gain_map_op(region: Region) -> Opcode {
        Opcode::GainMap {
            region,
            points_v: 3,
            points_h: 4,
            spacing_v: 0.5,
            spacing_h: 1.0 / 3.0,
            origin_v: 0.0,
            origin_h: 0.0,
            map_planes: 1,
            gains: (0..12).map(|i| 1.0 + i as f32 * 0.1).collect(),
        }
    }

    #[test]
    fn windows_match_the_whole_image() {
        // Pitched (one Bayer plane) gain map, row deltas, bad pixels and a vignette: applied to a
        // window of a larger buffer they must give exactly the whole-image result there.
        let r = Region { top: 1, left: 0, bottom: 20, right: 24, plane: 0, planes: 1, row_pitch: 2, col_pitch: 2 };
        let ops = vec![
            gain_map_op(r.clone()),
            Opcode::DeltaPerRow { region: r.clone(), deltas: (0..10).map(|i| i as f32 * 1e-5).collect() },
            Opcode::FixVignetteRadial { k: [0.3, -0.1, 0.0, 0.0, 0.0], cx: 0.4, cy: 0.6 },
            Opcode::FixBadPixelsList { bayer_phase: 0, points: vec![(9, 11)], rects: vec![] },
        ];
        let (w, h) = (24usize, 20usize);
        let base: Vec<f32> = (0..w * h).map(|i| 100.0 + (i * 7 % 13) as f32).collect();
        let mut whole = base.clone();
        apply(&ops, &mut PlaneImage::new(&mut whole, w, h, 1), 2, 1.0, false);
        // Window (5, 4) .. (21, 18) inside a buffer with a 3-pixel border.
        let (wx, wy, ww, wh) = (5usize, 4usize, 16usize, 14usize);
        let stride = ww + 3;
        let mut buf = vec![0f32; stride * (wh + 1)];
        for y in 0..wh {
            for x in 0..ww {
                buf[(y + 1) * stride + x + 2] = base[(y + wy) * w + x + wx];
            }
        }
        let mut pi = PlaneImage {
            data: &mut buf[stride + 2..],
            width: ww,
            height: wh,
            planes: 1,
            stride,
            x0: wx,
            y0: wy,
            full_width: w,
            full_height: h,
        };
        apply(&ops, &mut pi, 2, 1.0, true);
        for y in 0..wh {
            for x in 0..ww {
                let (a, b) = (buf[(y + 1) * stride + x + 2], whole[(y + wy) * w + x + wx]);
                assert!((a - b).abs() < 1e-3, "({x},{y}): {a} vs {b}");
            }
        }
        // Pixels outside the gain map's plane pitch are untouched by it.
        assert_eq!(whole[0], base[0] * vignette_gain(0.0, 0.0, w, h));
    }

    fn vignette_gain(x: f64, y: f64, w: usize, h: usize) -> f32 {
        let (cx, cy) = (0.4 * w as f64, 0.6 * h as f64);
        let m2 = [(0.0, 0.0), (w as f64, 0.0), (0.0, h as f64), (w as f64, h as f64)]
            .iter()
            .map(|&(a, b): &(f64, f64)| (a - cx).powi(2) + (b - cy).powi(2))
            .fold(0.0, f64::max);
        let r2 = ((x - cx).powi(2) + (y - cy).powi(2)) / m2;
        (1.0 + 0.3 * r2 - 0.1 * r2 * r2) as f32
    }

    #[test]
    fn vignette_parse_and_gain() {
        let mut body = Vec::new();
        for f in [0.5f64, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5] {
            body.extend(f.to_bits().to_be_bytes());
        }
        let mut list = 1u32.to_be_bytes().to_vec();
        list.extend(op(3, &body));
        let ops = parse(&list).unwrap();
        assert!(ops[0].is_pixel_opcode());
        let mut data = vec![1.0f32; 3 * 4 * 4];
        apply(&ops, &mut PlaneImage::new(&mut data, 4, 4, 3), 1, 65535.0, false);
        // Corner (0,0): r = 1 -> gain 1.5 on every plane; centre (2,2): gain 1.
        assert!(data[..3].iter().all(|&v| (v - 1.5).abs() < 1e-6));
        assert!(data[(2 * 4 + 2) * 3..][..3].iter().all(|&v| (v - 1.0).abs() < 1e-6));
    }
}
