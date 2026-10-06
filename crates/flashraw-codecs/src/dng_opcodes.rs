//! DNG opcode lists (OpcodeList1/2/3, DNG spec 1.3+ chapter 7).
//!
//! Opcode lists carry processing steps the camera maker specifies: bad-pixel repair,
//! lens-shading gain maps (ubiquitous in smartphone DNGs), linearisation tables and
//! polynomials, per-row/column offsets and scales, and geometric corrections.
//!
//! FlashRaw parses all lists and applies the pixel-value opcodes to f32 planes (the
//! high-level API does so by default, each list in its own domain; lists of tables and
//! bad-pixel repairs on stored integers run on the u16 samples directly, with the same
//! result); geometric opcodes
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
    /// Lens distortion / lateral CA model (DNG 1.3 §6.2): per plane radial `kr[0..4]`
    /// and tangential `kt[0..2]` coefficients around the optical centre `(cx, cy)`
    /// (relative image coordinates). Reported, never applied.
    WarpRectilinear {
        planes: Vec<([f64; 4], [f64; 2])>,
        cx: f64,
        cy: f64,
    },
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
            Opcode::WarpRectilinear { .. } => "WarpRectilinear",
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
    /// Whether [`apply`] computes each sample of this opcode from that sample and its
    /// coordinates alone (no neighbours): such opcodes give the same result applied to
    /// any window of rows (e.g. band by band) as applied to the whole image. True for
    /// everything except the bad-pixel repairs.
    pub fn is_row_local(&self) -> bool {
        !matches!(self, Opcode::FixBadPixelsConstant { .. } | Opcode::FixBadPixelsList { .. })
    }
    /// Whether [`apply`] implements this opcode.
    pub fn is_pixel_opcode(&self) -> bool {
        !matches!(
            self,
            Opcode::WarpRectilinear { .. } | Opcode::WarpFisheye | Opcode::TrimBounds { .. } | Opcode::Other { .. }
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

fn warp_rectilinear(b: &mut Cursor) -> Result<Opcode> {
    let n = b.u32()? as usize;
    if n == 0 || n > 4 || (n * 6 + 2) * 8 > b.remaining() {
        return Err(Error::corrupt("WarpRectilinear plane count"));
    }
    let mut f = || b.u64().map(f64::from_bits);
    let mut planes = Vec::with_capacity(n);
    for _ in 0..n {
        planes.push(([f()?, f()?, f()?, f()?], [f()?, f()?]));
    }
    let (cx, cy) = (f()?, f()?);
    Ok(Opcode::WarpRectilinear { planes, cx, cy })
}

/// One opcode of a list with its header fields.
#[derive(Debug, Clone, PartialEq)]
pub struct OpcodeEntry {
    /// DNG opcode id (1 = WarpRectilinear ... 13 = ScalePerColumn).
    pub id: u32,
    /// Flag bit 0: the opcode may be skipped by readers that do not understand it.
    pub optional: bool,
    /// Flag bit 1: may be skipped when rendering previews.
    pub preview_only: bool,
    pub op: Opcode,
}

/// Parse an opcode list (always big-endian, per the DNG spec).
pub fn parse(data: &[u8]) -> Result<Vec<Opcode>> {
    Ok(parse_entries(data)?.into_iter().map(|e| e.op).collect())
}

/// [`parse`] keeping each opcode's id and flags.
pub fn parse_entries(data: &[u8]) -> Result<Vec<OpcodeEntry>> {
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
            // Never applied, so a malformed body must not cost the rest of the list: report
            // it without parameters instead.
            1 => warp_rectilinear(&mut b).unwrap_or(Opcode::WarpRectilinear { planes: Vec::new(), cx: 0.5, cy: 0.5 }),
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
        out.push(OpcodeEntry { id, optional: flags & 1 != 0, preview_only: flags & 2 != 0, op });
    }
    Ok(out)
}

/// Sample types the opcodes run on: f32 for every opcode, u16 (stored integer values)
/// for the lists [`apply_u16`] accepts.
pub trait Sample: Copy + Send + Sync + 'static {
    fn to_f32(self) -> f32;
    /// A repaired value (mean of neighbours) as stored.
    fn from_mean(v: f32) -> Self;
}

impl Sample for f32 {
    fn to_f32(self) -> f32 {
        self
    }
    fn from_mean(v: f32) -> f32 {
        v
    }
}

impl Sample for u16 {
    fn to_f32(self) -> f32 {
        self as f32
    }
    /// Rounded as stored values are after an f32 pass.
    fn from_mean(v: f32) -> u16 {
        v.round().clamp(0.0, 65535.0) as u16
    }
}

/// A mutable image the opcodes operate on: a `width x height` window (rows
/// `stride` pixels apart, `planes` interleaved samples per pixel) placed at
/// `(x0, y0)` in the coordinate system of the whole image the list describes, which is
/// `full_width x full_height` (GainMap positions are relative to it).
pub struct PlaneImage<'a, T = f32> {
    pub data: &'a mut [T],
    pub width: usize,
    pub height: usize,
    pub planes: usize,
    pub stride: usize,
    pub x0: usize,
    pub y0: usize,
    pub full_width: usize,
    pub full_height: usize,
}

impl<'a, T: Sample> PlaneImage<'a, T> {
    /// A whole, tightly packed image.
    pub fn new(data: &'a mut [T], width: usize, height: usize, planes: usize) -> Self {
        PlaneImage { data, width, height, planes, stride: width, x0: 0, y0: 0, full_width: width, full_height: height }
    }

    /// Run `f(y, row)` for every row `y` (whole-image coordinates) of the window that
    /// `r` selects; `row` is the window row (first pixel at `x0`).
    fn rows(&mut self, r: &Region, parallel: bool, f: impl Fn(usize, &mut [T]) + Sync + Send) {
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
    fn for_region(&mut self, r: &Region, parallel: bool, f: impl Fn(usize, usize, usize, &mut T) + Sync + Send) {
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

impl PlaneImage<'_, f32> {
    /// Run `f(y, row)` for every row of the window (`y` in whole-image coordinates).
    fn each_row(&mut self, parallel: bool, f: &RowFn<'_>) {
        let (w, h, p, stride, y0) = (self.width, self.height, self.planes, self.stride, self.y0);
        if w == 0 || stride < w {
            return;
        }
        let rl = stride * p;
        let n = self.data.len().min(h * rl);
        par::for_each_row_chunk(&mut self.data[..n], rl, 16, parallel, |first, chunk| {
            for (i, row) in chunk.chunks_mut(rl).enumerate() {
                let len = (w * p).min(row.len());
                f(y0 + first + i, &mut row[..len]);
            }
        });
    }
}

/// A row-local opcode prepared for a window: `f(y, row)` processes window row `y`
/// (whole-image coordinates; `row` holds the window's `width * planes` samples).
type RowFn<'a> = Box<dyn Fn(usize, &mut [f32]) + Send + Sync + 'a>;

/// Columns of a window's row: placement of the window in the whole image.
#[derive(Debug, Clone, Copy)]
struct RowGeometry {
    width: usize,
    planes: usize,
    x0: usize,
    full_width: usize,
    full_height: usize,
}

impl RowGeometry {
    /// Columns `x` (whole-image coordinates) that `r` selects, with the sample offset of
    /// plane `r.plane` inside a window row.
    fn columns(&self, r: &Region) -> Vec<(usize, usize)> {
        let left = r.left as usize;
        let start = left.max(self.x0);
        // First selected column at or after the window start.
        let start = left + (start - left).div_ceil(r.col_pitch as usize) * r.col_pitch as usize;
        let right = (r.right as usize).min(self.x0 + self.width);
        let (x0, p, plane) = (self.x0, self.planes, r.plane as usize);
        (start..right.max(start)).step_by(r.col_pitch as usize).map(move |x| (x, (x - x0) * p + plane)).collect()
    }

    /// Number of selected planes that exist.
    fn plane_count(&self, r: &Region) -> usize {
        (r.plane as usize).saturating_add(r.planes as usize).min(self.planes).saturating_sub(r.plane as usize)
    }
}

/// Whether row `y` is selected by `r` (rows `top..bottom`, every `row_pitch`).
#[inline]
fn row_selected(r: &Region, y: usize) -> bool {
    let top = r.top as usize;
    y >= top && y < r.bottom as usize && (y - top) % r.row_pitch as usize == 0
}

/// `f(x, y, plane_index_within_region, sample)` for every sample of a row that `r` selects.
fn region_fn<'a>(
    g: &RowGeometry,
    r: &'a Region,
    f: impl Fn(usize, usize, usize, &mut f32) + Send + Sync + 'a,
) -> Option<RowFn<'a>> {
    let cols = g.columns(r);
    let np = g.plane_count(r);
    if cols.is_empty() || np == 0 {
        return None;
    }
    Some(Box::new(move |y, row| {
        if !row_selected(r, y) {
            return;
        }
        for &(x, off) in &cols {
            for k in 0..np {
                if let Some(v) = row.get_mut(off + k) {
                    f(x, y, k, v);
                }
            }
        }
    }))
}

/// Prepare a row-local pixel opcode for windows of geometry `g`: `Some(Some(f))`, or
/// `Some(None)` when it changes nothing there; `None` for the opcodes that are not
/// row-local or not applied (bad-pixel repairs, geometric and unknown ones).
fn row_op<'a>(op: &'a Opcode, g: &RowGeometry, value_scale: f32) -> Option<Option<RowFn<'a>>> {
    Some(match op {
        Opcode::MapTable { region, table } if !table.is_empty() => {
            let last = (table.len() - 1) as f32;
            region_fn(g, region, move |_, _, _, v| {
                let i = (*v * value_scale).round().clamp(0.0, last) as usize;
                *v = table[i] as f32 / value_scale;
            })
        }
        Opcode::MapTable { .. } => None,
        Opcode::MapPolynomial { region, coeffs } => region_fn(g, region, move |_, _, _, v| {
            // Polynomials map values normalised to [0, 1].
            let x = (*v * value_scale / 65535.0) as f64;
            let mut acc = 0f64;
            for c in coeffs.iter().rev() {
                acc = acc * x + c;
            }
            *v = (acc * 65535.0 / value_scale as f64) as f32;
        }),
        Opcode::DeltaPerRow { region, deltas } => region_fn(g, region, move |_, y, _, v| {
            let i = (y - region.top as usize) / region.row_pitch as usize;
            *v += deltas.get(i).copied().unwrap_or(0.0) * 65535.0 / value_scale;
        }),
        Opcode::DeltaPerColumn { region, deltas } => region_fn(g, region, move |x, _, _, v| {
            let i = (x - region.left as usize) / region.col_pitch as usize;
            *v += deltas.get(i).copied().unwrap_or(0.0) * 65535.0 / value_scale;
        }),
        Opcode::ScalePerRow { region, scales } => region_fn(g, region, move |_, y, _, v| {
            let i = (y - region.top as usize) / region.row_pitch as usize;
            *v *= scales.get(i).copied().unwrap_or(1.0);
        }),
        Opcode::ScalePerColumn { region, scales } => region_fn(g, region, move |x, _, _, v| {
            let i = (x - region.left as usize) / region.col_pitch as usize;
            *v *= scales.get(i).copied().unwrap_or(1.0);
        }),
        Opcode::GainMap { region, points_v, points_h, spacing_v, spacing_h, origin_v, origin_h, map_planes, gains } => {
            let (pv, ph, mp) = (*points_v as usize, *points_h as usize, *map_planes as usize);
            if pv == 0 || ph == 0 || mp == 0 || gains.len() < pv * ph * mp {
                return Some(None);
            }
            let (fw, fh) = (g.full_width.max(1) as f64, g.full_height.max(1) as f64);
            // Map positions are relative to the whole image (0..1). Horizontal
            // interpolation weights are per column; vertical ones per row.
            let pos = |rel: f64, origin: f64, spacing: f64, n: usize| -> (usize, usize, f32) {
                let f = if spacing > 0.0 { ((rel - origin) / spacing).clamp(0.0, (n - 1) as f64) } else { 0.0 };
                let i0 = (f.floor() as usize).min(n - 1);
                (i0, (i0 + 1).min(n - 1), (f - i0 as f64) as f32)
            };
            let cols: Vec<(usize, usize, usize, f32)> = g
                .columns(region)
                .into_iter()
                .map(|(x, off)| {
                    let (h0, h1, t) = pos(x as f64 / fw, *origin_h, *spacing_h, ph);
                    (off, h0, h1, t)
                })
                .collect();
            let np = g.plane_count(region);
            if cols.is_empty() || np == 0 {
                return Some(None);
            }
            let (origin_v, spacing_v) = (*origin_v, *spacing_v);
            Some(Box::new(move |y, row| {
                if !row_selected(region, y) {
                    return;
                }
                let (v0, v1, tv) = pos(y as f64 / fh, origin_v, spacing_v, pv);
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
            }))
        }
        Opcode::FixVignetteRadial { k, cx, cy } => {
            let (fw, fh) = (g.full_width as f64, g.full_height as f64);
            let (cx, cy) = (cx * fw, cy * fh);
            // Squared distance to the farthest corner normalises r to 1 there.
            let m2 = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)]
                .iter()
                .map(|&(x, y): &(f64, f64)| (x - cx).powi(2) + (y - cy).powi(2))
                .fold(0.0, f64::max);
            if m2 <= 0.0 || !m2.is_finite() || g.planes == 0 {
                return Some(None);
            }
            let (np, x0, full_h) = (g.planes, g.x0, g.full_height);
            Some(Box::new(move |y, row| {
                if y >= full_h {
                    return;
                }
                let dy2 = (y as f64 - cy).powi(2);
                for (i, px) in row.chunks_exact_mut(np).enumerate() {
                    let r2 = (((x0 + i) as f64 - cx).powi(2) + dy2) / m2;
                    let g = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * k[4]))));
                    for v in px {
                        *v *= g as f32;
                    }
                }
            }))
        }
        _ => return None,
    })
}

/// A list of row-local opcodes prepared for one window geometry, applied a row at a time
/// (e.g. by a streaming pipeline that produces the rows band by band). For every row this
/// gives exactly what [`apply`] gives on the whole window.
pub struct RowOps<'a> {
    fns: Vec<RowFn<'a>>,
}

impl<'a> RowOps<'a> {
    /// Prepare `ops` for windows `width` pixels wide of `planes` samples per pixel, whose
    /// first column is column `x0` of a `full_width x full_height` image (`value_scale` as
    /// for [`apply`]). `None` when a pixel opcode is not row-local
    /// ([`Opcode::is_row_local`]); geometric and unknown opcodes are left out, as by
    /// [`apply`].
    pub fn new(
        ops: &'a [Opcode],
        width: usize,
        planes: usize,
        x0: usize,
        full_width: usize,
        full_height: usize,
        value_scale: f32,
    ) -> Option<Self> {
        let g = RowGeometry { width, planes, x0, full_width, full_height };
        let mut fns = Vec::new();
        for op in ops.iter().filter(|o| o.is_pixel_opcode()) {
            if let Some(f) = row_op(op, &g, value_scale)? {
                fns.push(f);
            }
        }
        Some(RowOps { fns })
    }

    /// Whether no opcode changes anything.
    pub fn is_empty(&self) -> bool {
        self.fns.is_empty()
    }

    /// Apply the opcodes, in order, to window row `y` (whole-image coordinates; `row`:
    /// `width * planes` samples).
    pub fn apply(&self, y: usize, row: &mut [f32]) {
        for f in &self.fns {
            f(y, row);
        }
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
    let g = RowGeometry {
        width: img.width,
        planes: img.planes,
        x0: img.x0,
        full_width: img.full_width,
        full_height: img.full_height,
    };
    for op in ops {
        if let Some(f) = row_op(op, &g, value_scale) {
            if let Some(f) = f {
                img.each_row(parallel, &f);
            }
            continue;
        }
        match op {
            Opcode::FixBadPixelsConstant { constant, bayer_phase } => {
                let c = *constant as f32 / value_scale;
                fix_bad_pixels(img, cfa_period, *bayer_phase, |v: f32| v == c, &[], &[], parallel);
            }
            Opcode::FixBadPixelsList { bayer_phase, points, rects } => {
                fix_bad_pixels(img, cfa_period, *bayer_phase, |_| false, points, rects, parallel);
            }
            other => skipped.push(other.name()),
        }
    }
    skipped
}

/// Whether [`apply_u16`] gives exactly what [`apply`] gives on the same values converted
/// to f32 (value scale 1) and rounded back: tables look up rounded values and repairs
/// produce means that only a final rounding (or a table) reads afterwards.
pub fn u16_exact(ops: &[Opcode]) -> bool {
    let mut repaired = false;
    for op in ops {
        match op {
            Opcode::MapTable { .. } => {}
            _ if !op.is_pixel_opcode() => {}
            Opcode::FixBadPixelsConstant { .. } | Opcode::FixBadPixelsList { .. } if !repaired => repaired = true,
            _ => return false,
        }
    }
    true
}

/// [`apply`] on stored integer samples (value scale 1), for lists made of tables and
/// bad-pixel repairs: no f32 copy of the image is needed and the result is identical
/// to the f32 pass rounded back to u16. Returns `None` (leaving `img` untouched) for
/// lists it does not cover.
pub fn apply_u16(
    ops: &[Opcode],
    img: &mut PlaneImage<u16>,
    cfa_period: usize,
    parallel: bool,
) -> Option<Vec<&'static str>> {
    if !u16_exact(ops) {
        return None;
    }
    let mut skipped = Vec::new();
    for op in ops {
        match op {
            Opcode::MapTable { region, table } if !table.is_empty() => {
                let last = table.len() - 1;
                img.for_region(region, parallel, |_, _, _, v| *v = table[(*v as usize).min(last)]);
            }
            Opcode::FixBadPixelsConstant { constant, bayer_phase } => {
                let c = *constant;
                fix_bad_pixels(img, cfa_period, *bayer_phase, |v: u16| v as u32 == c, &[], &[], parallel);
            }
            Opcode::FixBadPixelsList { bayer_phase, points, rects } => {
                fix_bad_pixels(img, cfa_period, *bayer_phase, |_| false, points, rects, parallel);
            }
            op if !op.is_pixel_opcode() => skipped.push(op.name()),
            _ => {}
        }
    }
    Some(skipped)
}

/// Indices `x < w` of `row` (`p` samples per pixel) whose first sample is bad. Rows are
/// tested in blocks first: bad values are rare.
fn bad_in_row<T: Sample>(row: &[T], w: usize, p: usize, is_bad: &impl Fn(T) -> bool) -> impl Iterator<Item = usize> {
    const BLOCK: usize = 64;
    let mut out = Vec::new();
    for b in (0..w).step_by(BLOCK) {
        let e = (b + BLOCK).min(w);
        let hit = if p == 1 {
            row[b..e].iter().fold(false, |a, &v| a | is_bad(v))
        } else {
            (b..e).fold(false, |a, x| a | is_bad(row[x * p]))
        };
        if hit {
            out.extend((b..e).filter(|&x| is_bad(row[x * p])));
        }
    }
    out.into_iter()
}

/// Replace bad pixels by the mean of the nearest same-colour neighbours (distance =
/// CFA period) that are not themselves bad. Coordinates are whole-image ones.
fn fix_bad_pixels<T: Sample>(
    img: &mut PlaneImage<T>,
    period: usize,
    _phase: u32,
    is_bad_value: impl Fn(T) -> bool + Sync,
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
        let row = &data[at(0, y)..at(w - 1, y) + 1];
        bad_in_row(row, w, p, &is_bad_value).map(|x| y * w + x).collect()
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
                    s += img.data[at(xx as usize, yy as usize) + pl].to_f32();
                    n += 1.0;
                }
            }
            if n > 0.0 {
                fixes.push((at(x, y) + pl, s / n));
            }
        }
    }
    for (i, v) in fixes {
        img.data[i] = T::from_mean(v);
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
    fn u16_path_matches_f32_path() {
        let (w, h) = (150usize, 23usize);
        let table: Vec<u16> = (0..3000u32).map(|i| (i * 7 / 3 + 5) as u16).collect();
        let pitched =
            Region { top: 1, left: 3, bottom: 20, right: 140, plane: 0, planes: 1, row_pitch: 2, col_pitch: 2 };
        let map = |region: Region| Opcode::MapTable { region, table: table.clone() };
        let all =
            |planes| Region { top: 0, left: 0, bottom: 100, right: 1000, plane: 0, planes, row_pitch: 1, col_pitch: 1 };
        let constant = Opcode::FixBadPixelsConstant { constant: 0, bayer_phase: 1 };
        let list =
            Opcode::FixBadPixelsList { bayer_phase: 0, points: vec![(4, 7), (0, 0)], rects: vec![(10, 60, 13, 66)] };
        let warp = Opcode::WarpRectilinear { planes: vec![], cx: 0.5, cy: 0.5 };
        let lists: Vec<Vec<Opcode>> = vec![
            vec![constant.clone()],
            vec![list.clone()],
            vec![map(pitched.clone()), map(all(1)), constant.clone(), map(pitched.clone())],
            vec![warp.clone(), list.clone(), map(all(3))],
        ];
        for planes in [1usize, 3] {
            for ops in &lists {
                assert!(u16_exact(ops));
                let mut x = 99u32;
                let base: Vec<u16> = (0..w * h * planes)
                    .map(|_| {
                        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                        // Some zeros (bad for the constant), values past the table's end.
                        if x >> 27 == 0 { 0 } else { (x >> 20) as u16 % 3500 }
                    })
                    .collect();
                let mut f: Vec<f32> = base.iter().map(|&v| v as f32).collect();
                let skipped_f = apply(ops, &mut PlaneImage::new(&mut f, w, h, planes), 2, 1.0, true);
                let want: Vec<u16> = f.iter().map(|v| v.round().clamp(0.0, 65535.0) as u16).collect();
                let mut got = base.clone();
                let skipped = apply_u16(ops, &mut PlaneImage::new(&mut got, w, h, planes), 2, true).unwrap();
                assert_eq!(skipped, skipped_f);
                assert_eq!(got, want, "planes {planes} ops {ops:?}");
                assert_ne!(got, base);
            }
        }
        // Repairs read by a later repair (or by a non-table opcode) need the f32 pass.
        assert!(!u16_exact(&[constant.clone(), list]));
        assert!(!u16_exact(&[constant, Opcode::ScalePerRow { region: all(1), scales: vec![1.0] }]));
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
