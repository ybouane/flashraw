//! DNG opcode lists (tags OpcodeList1, OpcodeList2 and OpcodeList3; DNG specification,
//! chapter "Opcode List Processing").
//!
//! A list is big-endian whatever the byte order of the file: a 32-bit entry count, then per
//! entry the opcode id, the DNG version it was defined in, flags (bit 0 optional, bit 1 may
//! be skipped for previews), the parameter byte count and the parameters.
//!
//! The pixel-value opcodes (tables, polynomials, gain maps, per-row/column deltas and scales,
//! radial vignetting, bad-pixel repair) are applied to an image window given in whole-image
//! coordinates, on f32 samples or, for lists made of tables and one repair, on stored u16
//! values. The geometric opcodes (warps, trim) are parsed and reported but never applied, and
//! unknown opcodes are reported, not rejected.
//!
//! Numeric conventions that make results reproducible bit for bit: f32 arithmetic for tables,
//! deltas, scales and gain interpolation, f64 for polynomials and the radial gain, every
//! operation in the documented order (no fused multiply-add), pixel coordinates without a
//! half-pixel offset.

use flashraw_core::bytes::Cursor;
use flashraw_core::{ByteOrder, Error, Result, par};

/// The rectangle, planes and pitches an opcode works on (DNG "area" fields).
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

/// One parsed opcode.
#[derive(Debug, Clone, PartialEq)]
pub enum Opcode {
    /// Lens distortion model: per plane radial `[kr0..kr3]` and tangential `[kt0, kt1]`
    /// coefficients, optical centre `(cx, cy)` in relative image coordinates. Not applied.
    WarpRectilinear {
        planes: Vec<([f64; 4], [f64; 2])>,
        cx: f64,
        cy: f64,
    },
    /// Fisheye warp. Not applied.
    WarpFisheye,
    /// Radial gain `1 + k0 r^2 + ... + k4 r^10` around the relative centre `(cx, cy)`.
    FixVignetteRadial {
        k: [f64; 5],
        cx: f64,
        cy: f64,
    },
    FixBadPixelsConstant {
        constant: u32,
        bayer_phase: u32,
    },
    /// `points` are `(row, column)`, `rects` are `(top, left, bottom, right)`.
    FixBadPixelsList {
        bayer_phase: u32,
        points: Vec<(u32, u32)>,
        rects: Vec<(u32, u32, u32, u32)>,
    },
    /// Crop. Not applied.
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
    /// An opcode id this module does not know, with its "optional" flag.
    Other {
        id: u32,
        optional: bool,
    },
}

impl Opcode {
    /// The DNG name of the opcode (`"Other"` for unknown ids).
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

    /// True when [`apply`] computes each sample from that sample and its coordinates only
    /// (everything but the bad-pixel repairs).
    pub fn is_row_local(&self) -> bool {
        !self.is_repair()
    }

    /// True when [`apply`] changes pixels for this opcode.
    pub fn is_pixel_opcode(&self) -> bool {
        !matches!(
            self,
            Opcode::WarpRectilinear { .. } | Opcode::WarpFisheye | Opcode::TrimBounds { .. } | Opcode::Other { .. }
        )
    }

    fn is_repair(&self) -> bool {
        matches!(self, Opcode::FixBadPixelsConstant { .. } | Opcode::FixBadPixelsList { .. })
    }
}

/// An opcode with the fields of its list entry header.
#[derive(Debug, Clone, PartialEq)]
pub struct OpcodeEntry {
    /// Opcode id.
    pub id: u32,
    /// Flags bit 0.
    pub optional: bool,
    /// Flags bit 1.
    pub preview_only: bool,
    pub op: Opcode,
}

// ============================================================================ parsing

/// Upper bound on the number of entries of one list.
const MAX_ENTRIES: u32 = 4096;
/// Upper bound on the element count of a table, a per-row/column vector or a gain map.
const MAX_ELEMENTS: u64 = 1 << 24;
/// MapPolynomial degree limit set by the DNG specification.
const MAX_DEGREE: u32 = 8;
/// WarpRectilinear plane limit set by the DNG specification.
const MAX_WARP_PLANES: u32 = 4;

/// Parse an opcode list.
pub fn parse(data: &[u8]) -> Result<Vec<Opcode>> {
    Ok(parse_entries(data)?.into_iter().map(|e| e.op).collect())
}

/// Parse an opcode list, keeping the header fields of every entry.
pub fn parse_entries(data: &[u8]) -> Result<Vec<OpcodeEntry>> {
    let mut list = Cursor::new(data, ByteOrder::Big);
    let count = list.u32()?;
    if count > MAX_ENTRIES {
        return Err(Error::corrupt(format!("invalid DNG opcode list with {count} opcodes")));
    }
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let id = list.u32()?;
        let _version = list.u32()?;
        let flags = list.u32()?;
        let length = list.u32()?;
        let body = list.bytes(length as usize)?;
        let optional = flags & 1 != 0;
        let op = decode_body(id, optional, body)?;
        entries.push(OpcodeEntry { id, optional, preview_only: flags & 2 != 0, op });
    }
    Ok(entries)
}

/// Big-endian reader over one entry's parameter bytes.
struct Body<'a>(Cursor<'a>);

impl Body<'_> {
    fn u32(&mut self) -> Result<u32> {
        self.0.u32()
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.0.u64()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.0.u32()?))
    }
    fn left(&self) -> u64 {
        self.0.remaining() as u64
    }
    fn region(&mut self) -> Result<Region> {
        Ok(Region {
            top: self.u32()?,
            left: self.u32()?,
            bottom: self.u32()?,
            right: self.u32()?,
            plane: self.u32()?,
            planes: self.u32()?,
            row_pitch: self.u32()?.max(1),
            col_pitch: self.u32()?.max(1),
        })
    }
    /// An element count that must be followed by that many elements of `size` bytes.
    fn element_count(&mut self, size: u64) -> Result<usize> {
        let n = self.u32()?;
        if u64::from(n) > MAX_ELEMENTS || u64::from(n) * size > self.left() {
            return Err(Error::corrupt(format!("invalid DNG opcode with {n} elements")));
        }
        Ok(n as usize)
    }
    fn f32s(&mut self, n: usize) -> Result<Vec<f32>> {
        (0..n).map(|_| self.f32()).collect()
    }
    fn per_line(&mut self) -> Result<(Region, Vec<f32>)> {
        let region = self.region()?;
        let n = self.element_count(4)?;
        Ok((region, self.f32s(n)?))
    }
    fn warp_rectilinear(&mut self) -> Result<Opcode> {
        let n = self.u32()?;
        if n == 0 || n > MAX_WARP_PLANES || u64::from(6 * n + 2) * 8 > self.left() {
            return Err(Error::corrupt("invalid WarpRectilinear"));
        }
        let mut planes = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let radial = [self.f64()?, self.f64()?, self.f64()?, self.f64()?];
            let tangential = [self.f64()?, self.f64()?];
            planes.push((radial, tangential));
        }
        Ok(Opcode::WarpRectilinear { planes, cx: self.f64()?, cy: self.f64()? })
    }
}

fn decode_body(id: u32, optional: bool, body: &[u8]) -> Result<Opcode> {
    let mut b = Body(Cursor::new(body, ByteOrder::Big));
    Ok(match id {
        // A broken warp is kept (it is reported, never applied) instead of failing the list.
        1 => b.warp_rectilinear().unwrap_or(Opcode::WarpRectilinear { planes: Vec::new(), cx: 0.5, cy: 0.5 }),
        2 => Opcode::WarpFisheye,
        3 => {
            let k = [b.f64()?, b.f64()?, b.f64()?, b.f64()?, b.f64()?];
            Opcode::FixVignetteRadial { k, cx: b.f64()?, cy: b.f64()? }
        }
        4 => Opcode::FixBadPixelsConstant { constant: b.u32()?, bayer_phase: b.u32()? },
        5 => {
            let bayer_phase = b.u32()?;
            let n_points = b.u32()?;
            let n_rects = b.u32()?;
            if 8 * u64::from(n_points) + 16 * u64::from(n_rects) > b.left() {
                return Err(Error::corrupt("FixBadPixelsList counts exceed opcode size"));
            }
            let mut points = Vec::with_capacity(n_points as usize);
            for _ in 0..n_points {
                points.push((b.u32()?, b.u32()?));
            }
            let mut rects = Vec::with_capacity(n_rects as usize);
            for _ in 0..n_rects {
                rects.push((b.u32()?, b.u32()?, b.u32()?, b.u32()?));
            }
            Opcode::FixBadPixelsList { bayer_phase, points, rects }
        }
        6 => Opcode::TrimBounds { top: b.u32()?, left: b.u32()?, bottom: b.u32()?, right: b.u32()? },
        7 => {
            let region = b.region()?;
            let n = b.element_count(2)?;
            let table = (0..n).map(|_| b.0.u16()).collect::<Result<Vec<u16>>>()?;
            Opcode::MapTable { region, table }
        }
        8 => {
            let region = b.region()?;
            let degree = b.u32()?;
            if degree > MAX_DEGREE || u64::from(degree + 1) * 8 > b.left() {
                return Err(Error::corrupt("invalid MapPolynomial degree"));
            }
            let coeffs = (0..=degree).map(|_| b.f64()).collect::<Result<Vec<f64>>>()?;
            Opcode::MapPolynomial { region, coeffs }
        }
        9 => {
            let region = b.region()?;
            let points_v = b.u32()?;
            let points_h = b.u32()?;
            let spacing_v = b.f64()?;
            let spacing_h = b.f64()?;
            let origin_v = b.f64()?;
            let origin_h = b.f64()?;
            let map_planes = b.u32()?.max(1);
            let total = u64::from(points_v).saturating_mul(u64::from(points_h)).saturating_mul(u64::from(map_planes));
            if total == 0 || total > MAX_ELEMENTS || total * 4 > b.left() {
                return Err(Error::corrupt("invalid GainMap dimensions"));
            }
            let gains = b.f32s(total as usize)?;
            Opcode::GainMap { region, points_v, points_h, spacing_v, spacing_h, origin_v, origin_h, map_planes, gains }
        }
        10 => {
            let (region, deltas) = b.per_line()?;
            Opcode::DeltaPerRow { region, deltas }
        }
        11 => {
            let (region, deltas) = b.per_line()?;
            Opcode::DeltaPerColumn { region, deltas }
        }
        12 => {
            let (region, scales) = b.per_line()?;
            Opcode::ScalePerRow { region, scales }
        }
        13 => {
            let (region, scales) = b.per_line()?;
            Opcode::ScalePerColumn { region, scales }
        }
        _ => Opcode::Other { id, optional },
    })
}

// ============================================================================ samples and windows

/// Sample types the opcodes run on.
pub trait Sample: Copy + Send + Sync + 'static {
    fn to_f32(self) -> f32;
    /// The stored form of a repaired value.
    fn from_mean(v: f32) -> Self;
}

impl Sample for f32 {
    #[inline]
    fn to_f32(self) -> f32 {
        self
    }
    #[inline]
    fn from_mean(v: f32) -> f32 {
        v
    }
}

impl Sample for u16 {
    #[inline]
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    #[inline]
    fn from_mean(v: f32) -> u16 {
        // `as` maps NaN to 0.
        v.round().clamp(0.0, 65535.0) as u16
    }
}

/// A window of `width x height` pixels (`planes` interleaved samples each, rows `stride`
/// pixels apart) at `(x0, y0)` of a `full_width x full_height` image.
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
}

/// Rows handed to one worker at a time.
const ROWS_PER_TASK: usize = 32;

/// Visit the rows of a window: `f(y, samples, scratch)` with `y` the whole-image row and
/// `samples` the `width * planes` samples of that row present in the buffer (fewer for a
/// final partial row). `scratch` is shared by the rows of one task. Nothing is visited for an
/// empty window or a stride narrower than the width.
fn for_each_window_row<T, F>(img: &mut PlaneImage<T>, parallel: bool, f: F)
where
    T: Send,
    F: Fn(usize, &mut [T], &mut Vec<f32>) + Sync + Send,
{
    if img.width == 0 || img.stride < img.width || img.planes == 0 {
        return;
    }
    let pitch = img.stride * img.planes;
    let used = img.width * img.planes;
    let end = img.height.saturating_mul(pitch).min(img.data.len());
    let y0 = img.y0;
    par::for_each_row_chunk(&mut img.data[..end], pitch, ROWS_PER_TASK, parallel, |first, chunk| {
        let mut scratch = Vec::new();
        for (k, row) in chunk.chunks_mut(pitch).enumerate() {
            let n = used.min(row.len());
            f(y0 + first + k, &mut row[..n], &mut scratch);
        }
    });
}

// ============================================================================ area selection

/// The window columns an area selects: `count` columns from `first`, `step` apart.
#[derive(Clone, Copy)]
struct Columns {
    first: usize,
    step: usize,
    count: usize,
    /// Index of the first selected column within the area (`(x - left) / pitch`).
    index0: usize,
}

/// The rows an area selects, in whole-image coordinates.
#[derive(Clone, Copy)]
struct Rows {
    top: usize,
    bottom: usize,
    step: usize,
}

impl Rows {
    /// Index of row `y` within the area (`(y - top) / pitch`), if the area selects it.
    #[inline]
    fn index(&self, y: usize) -> Option<usize> {
        if y < self.top || y >= self.bottom {
            return None;
        }
        let d = y - self.top;
        if d % self.step == 0 { Some(d / self.step) } else { None }
    }
}

/// Where an area meets the rows of a window: the rows, the columns, and the sample range
/// `lo..hi` within a pixel of `planes` samples.
#[derive(Clone, Copy)]
struct Selection {
    rows: Rows,
    cols: Columns,
    lo: usize,
    hi: usize,
    planes: usize,
}

impl Selection {
    fn new(r: &Region, x0: usize, width: usize, planes: usize) -> Self {
        let (left, right, step) = (r.left as usize, r.right as usize, r.col_pitch.max(1) as usize);
        let start = x0.max(left);
        let rem = (start - left) % step;
        let first_x = if rem == 0 { start } else { start.saturating_add(step - rem) };
        let end_x = x0.saturating_add(width).min(right);
        let cols = if first_x < end_x {
            Columns {
                first: first_x - x0,
                step,
                count: (end_x - first_x).div_ceil(step),
                index0: (first_x - left) / step,
            }
        } else {
            Columns { first: 0, step, count: 0, index0: 0 }
        };
        let lo = r.plane as usize;
        let hi = lo.saturating_add(r.planes as usize).min(planes).max(lo);
        let rows = Rows { top: r.top as usize, bottom: r.bottom as usize, step: r.row_pitch.max(1) as usize };
        Selection { rows, cols, lo, hi, planes }
    }

    fn is_empty(&self) -> bool {
        self.cols.count == 0 || self.lo >= self.hi
    }

    /// Visit the selected samples of a row present in `row`: `f(column_number, plane_in_area,
    /// sample)`, columns in order.
    #[inline]
    fn visit<T>(&self, row: &mut [T], mut f: impl FnMut(usize, usize, &mut T)) {
        let p = self.planes;
        let mut base = self.cols.first * p + self.lo;
        let jump = self.cols.step.saturating_mul(p);
        if self.hi - self.lo == 1 {
            if let Some(tail) = row.get_mut(base..) {
                for (c, v) in tail.iter_mut().step_by(jump).take(self.cols.count).enumerate() {
                    f(c, 0, v);
                }
            }
            return;
        }
        for c in 0..self.cols.count {
            for k in 0..self.hi - self.lo {
                match row.get_mut(base + k) {
                    Some(v) => f(c, k, v),
                    None => return,
                }
            }
            base += jump;
        }
    }
}

// ============================================================================ row-local kernels

/// Position on one gain-map axis: lower and upper map point, fraction between them.
#[derive(Clone, Copy)]
struct MapPos {
    lower: usize,
    upper: usize,
    t: f32,
}

/// Map position of pixel coordinate `p` on an axis of an image `extent` pixels long.
fn map_pos(p: usize, extent: usize, origin: f64, spacing: f64, points: u32) -> MapPos {
    let last = (points - 1) as usize;
    let rel = p as f64 / extent.max(1) as f64;
    let f = if spacing > 0.0 { ((rel - origin) / spacing).clamp(0.0, last as f64) } else { 0.0 };
    let lower = (f.floor() as usize).min(last);
    MapPos { lower, upper: (lower + 1).min(last), t: (f - lower as f64) as f32 }
}

/// Radial vignetting gain in whole-image pixel units.
struct Radial {
    k: [f64; 5],
    cy: f64,
    m2: f64,
    /// `(x - cx)^2` for each window column.
    dx2: Vec<f64>,
    full_height: usize,
    planes: usize,
}

impl Radial {
    #[inline(always)]
    fn gain(&self, dx2: f64, dy2: f64) -> f32 {
        let r2 = (dx2 + dy2) / self.m2;
        let k = &self.k;
        (1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * k[4]))))) as f32
    }

    fn run(&self, y: usize, row: &mut [f32]) {
        if y >= self.full_height {
            return;
        }
        let dy = y as f64 - self.cy;
        let dy2 = dy * dy;
        match self.planes {
            1 => {
                for (v, &dx2) in row.iter_mut().zip(&self.dx2) {
                    *v *= self.gain(dx2, dy2);
                }
            }
            3 => {
                for (px, &dx2) in row.chunks_exact_mut(3).zip(&self.dx2) {
                    let g = self.gain(dx2, dy2);
                    px[0] *= g;
                    px[1] *= g;
                    px[2] *= g;
                }
            }
            p => {
                for (px, &dx2) in row.chunks_exact_mut(p).zip(&self.dx2) {
                    let g = self.gain(dx2, dy2);
                    for v in px {
                        *v *= g;
                    }
                }
            }
        }
    }
}

/// Gain map prepared for the columns of a window.
struct Gains<'a> {
    sel: Selection,
    gains: &'a [f32],
    points_h: usize,
    map_planes: usize,
    origin_v: f64,
    spacing_v: f64,
    points_v: u32,
    full_height: usize,
    /// Horizontal map position of every selected column: lower point and fraction.
    cols: Vec<(u32, f32)>,
}

impl Gains<'_> {
    /// Scratch samples [`Gains::run`] needs.
    fn scratch_len(&self) -> usize {
        (self.sel.hi - self.sel.lo) * self.points_h * 2
    }

    fn run(&self, y: usize, row: &mut [f32], scratch: &mut [f32]) {
        if self.sel.rows.index(y).is_none() {
            return;
        }
        let v = map_pos(y, self.full_height, self.origin_v, self.spacing_v, self.points_v);
        let (ph, mp) = (self.points_h, self.map_planes);
        let nk = self.sel.hi - self.sel.lo;
        // Per selected plane, the map row interpolated vertically (`level`) and the difference
        // to the next map column (`rise`, 0 at the last), each `ph` long.
        let scratch = &mut scratch[..nk * ph * 2];
        for (k, buf) in scratch.chunks_exact_mut(ph * 2).enumerate() {
            let m = k.min(mp - 1);
            let (level, rise) = buf.split_at_mut(ph);
            let upper = &self.gains[v.upper * ph * mp..];
            let lower = &self.gains[v.lower * ph * mp..];
            for (j, l) in level.iter_mut().enumerate() {
                let a = lower[j * mp + m];
                let b = upper[j * mp + m];
                *l = a + (b - a) * v.t;
            }
            for j in 0..ph {
                rise[j] = level[(j + 1).min(ph - 1)] - level[j];
            }
        }
        let table: &[f32] = scratch;
        if nk == 1 {
            let (level, rise) = table.split_at(ph);
            self.sel.visit(row, |c, _, s| {
                let (h, t) = self.cols[c];
                *s *= level[h as usize] + rise[h as usize] * t;
            });
        } else {
            self.sel.visit(row, |c, k, s| {
                let (h, t) = self.cols[c];
                let at = k * ph * 2 + h as usize;
                *s *= table[at] + table[at + ph] * t;
            });
        }
    }
}

/// A row-local opcode prepared for one window geometry. Opcodes that change nothing for the
/// geometry are not prepared at all.
enum Kernel<'a> {
    Table {
        sel: Selection,
        /// `table[i] / scale` for every entry.
        values: Vec<f32>,
        last: f32,
        scale: f32,
    },
    Polynomial {
        sel: Selection,
        coeffs: &'a [f64],
        scale: f32,
    },
    Gain(Gains<'a>),
    DeltaRow {
        sel: Selection,
        deltas: &'a [f32],
        scale: f32,
    },
    ScaleRow {
        sel: Selection,
        scales: &'a [f32],
    },
    /// Offsets in sample units, one per selected column.
    ColumnAdd {
        sel: Selection,
        add: Vec<f32>,
    },
    /// Factors, one per selected column.
    ColumnMul {
        sel: Selection,
        mul: Vec<f32>,
    },
    Radial(Radial),
}

/// Window geometry and value scale the kernels are prepared for.
struct Geometry {
    width: usize,
    planes: usize,
    x0: usize,
    full_width: usize,
    full_height: usize,
    scale: f32,
}

impl<'a> Kernel<'a> {
    /// Prepare a row-local pixel opcode; `None` when it changes nothing in this geometry
    /// (or is not such an opcode).
    fn prepare(op: &'a Opcode, g: &Geometry) -> Option<Kernel<'a>> {
        let select = |r: &Region| Some(Selection::new(r, g.x0, g.width, g.planes)).filter(|s| !s.is_empty());
        let column_values = |sel: &Selection, v: &[f32], default: f32, f: &dyn Fn(f32) -> f32| -> Vec<f32> {
            (0..sel.cols.count).map(|c| f(v.get(sel.cols.index0 + c).copied().unwrap_or(default))).collect()
        };
        Some(match op {
            Opcode::MapTable { region, table } => {
                let sel = select(region)?;
                let last = table.len().checked_sub(1)? as f32;
                let values = table.iter().map(|&t| f32::from(t) / g.scale).collect();
                Kernel::Table { sel, values, last, scale: g.scale }
            }
            Opcode::MapPolynomial { region, coeffs } => {
                Kernel::Polynomial { sel: select(region)?, coeffs, scale: g.scale }
            }
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
                let needed = (*points_v as usize)
                    .checked_mul(*points_h as usize)
                    .and_then(|n| n.checked_mul(*map_planes as usize))
                    .filter(|&n| n > 0)?;
                if gains.len() < needed {
                    return None;
                }
                let sel = select(region)?;
                let cols = (0..sel.cols.count)
                    .map(|c| {
                        let x = g.x0 + sel.cols.first + c * sel.cols.step;
                        let h = map_pos(x, g.full_width, *origin_h, *spacing_h, *points_h);
                        (h.lower as u32, h.t)
                    })
                    .collect();
                Kernel::Gain(Gains {
                    sel,
                    gains,
                    points_h: *points_h as usize,
                    map_planes: *map_planes as usize,
                    origin_v: *origin_v,
                    spacing_v: *spacing_v,
                    points_v: *points_v,
                    full_height: g.full_height,
                    cols,
                })
            }
            Opcode::DeltaPerRow { region, deltas } => Kernel::DeltaRow { sel: select(region)?, deltas, scale: g.scale },
            Opcode::DeltaPerColumn { region, deltas } => {
                let sel = select(region)?;
                let add = column_values(&sel, deltas, 0.0, &|d| (d * 65535.0) / g.scale);
                Kernel::ColumnAdd { sel, add }
            }
            Opcode::ScalePerRow { region, scales } => Kernel::ScaleRow { sel: select(region)?, scales },
            Opcode::ScalePerColumn { region, scales } => {
                let sel = select(region)?;
                let mul = column_values(&sel, scales, 1.0, &|s| s);
                Kernel::ColumnMul { sel, mul }
            }
            Opcode::FixVignetteRadial { k, cx, cy } => {
                let (w, h) = (g.full_width as f64, g.full_height as f64);
                let (cx, cy) = (cx * w, cy * h);
                // Squared distance to the farthest image corner (NaN distances are ignored).
                let mut m2 = 0.0f64;
                for (px, py) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
                    let (dx, dy) = (px - cx, py - cy);
                    m2 = m2.max(dx * dx + dy * dy);
                }
                if m2 <= 0.0 || !m2.is_finite() || g.planes == 0 {
                    return None;
                }
                let dx2 = (0..g.width)
                    .map(|i| {
                        let dx = (g.x0 + i) as f64 - cx;
                        dx * dx
                    })
                    .collect();
                Kernel::Radial(Radial { k: *k, cy, m2, dx2, full_height: g.full_height, planes: g.planes })
            }
            _ => return None,
        })
    }

    /// Scratch samples [`Kernel::run`] needs.
    fn scratch_len(&self) -> usize {
        match self {
            Kernel::Gain(gm) => gm.scratch_len(),
            _ => 0,
        }
    }

    fn run(&self, y: usize, row: &mut [f32], scratch: &mut [f32]) {
        match self {
            Kernel::Table { sel, values, last, scale } => {
                if sel.rows.index(y).is_some() {
                    let (last, scale) = (*last, *scale);
                    sel.visit(row, |_, _, v| {
                        // `as` maps NaN to 0.
                        let i = (*v * scale).round().clamp(0.0, last) as usize;
                        *v = values[i];
                    });
                }
            }
            Kernel::Polynomial { sel, coeffs, scale } => {
                if sel.rows.index(y).is_some() {
                    let scale = *scale;
                    let back = f64::from(scale);
                    sel.visit(row, |_, _, v| {
                        let u = f64::from((*v * scale) / 65535.0);
                        let mut acc = 0.0f64;
                        for &c in coeffs.iter().rev() {
                            acc = acc * u + c;
                        }
                        *v = ((acc * 65535.0) / back) as f32;
                    });
                }
            }
            Kernel::Gain(gm) => gm.run(y, row, scratch),
            Kernel::DeltaRow { sel, deltas, scale } => {
                if let Some(i) = sel.rows.index(y) {
                    let d = deltas.get(i).copied().unwrap_or(0.0);
                    let add = (d * 65535.0) / *scale;
                    sel.visit(row, |_, _, v| *v += add);
                }
            }
            Kernel::ScaleRow { sel, scales } => {
                if let Some(i) = sel.rows.index(y) {
                    let s = scales.get(i).copied().unwrap_or(1.0);
                    sel.visit(row, |_, _, v| *v *= s);
                }
            }
            Kernel::ColumnAdd { sel, add } => {
                if sel.rows.index(y).is_some() {
                    sel.visit(row, |c, _, v| *v += add[c]);
                }
            }
            Kernel::ColumnMul { sel, mul } => {
                if sel.rows.index(y).is_some() {
                    sel.visit(row, |c, _, v| *v *= mul[c]);
                }
            }
            Kernel::Radial(r) => r.run(y, row),
        }
    }
}

/// Scratch samples kept on the stack by [`RowOps::apply`] (gain maps up to 128 columns wide
/// per plane in use).
const STACK_SCRATCH: usize = 256;

/// Row-local opcodes prepared for one window geometry, applied a row at a time.
pub struct RowOps<'a> {
    kernels: Vec<Kernel<'a>>,
    scratch: usize,
}

impl<'a> RowOps<'a> {
    /// `None` when a pixel opcode of `ops` is not row-local.
    pub fn new(
        ops: &'a [Opcode],
        width: usize,
        planes: usize,
        x0: usize,
        full_width: usize,
        full_height: usize,
        value_scale: f32,
    ) -> Option<Self> {
        if ops.iter().any(|o| o.is_pixel_opcode() && !o.is_row_local()) {
            return None;
        }
        let g = Geometry { width, planes, x0, full_width, full_height, scale: value_scale };
        let kernels: Vec<Kernel> = ops.iter().filter_map(|o| Kernel::prepare(o, &g)).collect();
        let scratch = kernels.iter().map(Kernel::scratch_len).max().unwrap_or(0);
        Some(RowOps { kernels, scratch })
    }

    /// True when no prepared opcode changes anything.
    pub fn is_empty(&self) -> bool {
        self.kernels.is_empty()
    }

    /// Apply the opcodes in order to row `y` (whole-image coordinates) of the window.
    pub fn apply(&self, y: usize, row: &mut [f32]) {
        let mut stack = [0.0f32; STACK_SCRATCH];
        let mut heap = Vec::new();
        let scratch = if self.scratch <= STACK_SCRATCH {
            &mut stack[..]
        } else {
            heap.resize(self.scratch, 0.0);
            &mut heap[..]
        };
        for k in &self.kernels {
            k.run(y, row, scratch);
        }
    }
}

// ============================================================================ bad-pixel repair

/// Neighbour directions (in CFA periods), in the order their values are summed.
const NEIGHBOURS: [(isize, isize); 8] = [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)];

/// Rows per task when scanning for bad values.
const SCAN_ROWS: usize = 64;

/// Window pixels (`j * width + i`, ascending) whose first sample is bad.
fn scan_bad<T: Sample>(img: &PlaneImage<T>, parallel: bool, is_bad: impl Fn(T) -> bool + Sync) -> Vec<usize> {
    let (w, h, p, stride) = (img.width, img.height, img.planes, img.stride);
    let data: &[T] = img.data;
    let bands: Vec<usize> = (0..h.div_ceil(SCAN_ROWS)).collect();
    let found = par::map(bands, parallel, |band| {
        let mut out = Vec::new();
        for j in band * SCAN_ROWS..((band + 1) * SCAN_ROWS).min(h) {
            let row = &data[j * stride * p..(j * stride + w) * p];
            if p == 1 {
                // Most blocks hold no bad value: test a whole block first.
                for (b, block) in row.chunks(64).enumerate() {
                    if block.iter().fold(false, |any, &v| any | is_bad(v)) {
                        for (i, &v) in block.iter().enumerate() {
                            if is_bad(v) {
                                out.push(j * w + b * 64 + i);
                            }
                        }
                    }
                }
            } else {
                for (i, px) in row.chunks_exact(p).enumerate() {
                    if is_bad(px[0]) {
                        out.push(j * w + i);
                    }
                }
            }
        }
        out
    });
    found.concat()
}

/// Window pixels (`j * width + i`, ascending, no duplicates) covered by listed points and
/// rectangles (bottom and right exclusive).
fn listed_bad<T>(img: &PlaneImage<T>, points: &[(u32, u32)], rects: &[(u32, u32, u32, u32)]) -> Vec<usize> {
    let (x0, y0, w) = (img.x0, img.y0, img.width);
    let (x1, y1) = (x0 + w, y0 + img.height);
    let mut out = Vec::new();
    for &(row, col) in points {
        let (y, x) = (row as usize, col as usize);
        if (y0..y1).contains(&y) && (x0..x1).contains(&x) {
            out.push((y - y0) * w + (x - x0));
        }
    }
    for &(top, left, bottom, right) in rects {
        let (xa, xb) = ((left as usize).max(x0), (right as usize).min(x1));
        for y in (top as usize).max(y0)..(bottom as usize).min(y1) {
            out.extend((xa..xb).map(|x| (y - y0) * w + (x - x0)));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Replace every sample of every bad pixel by the mean of the same sample of its good
/// neighbours one CFA period away inside the window. All means use the values before the
/// repair.
fn repair<T: Sample>(img: &mut PlaneImage<T>, bad: &[usize], period: usize, parallel: bool) {
    if bad.is_empty() {
        return;
    }
    let (w, h, p, stride) = (img.width, img.height, img.planes, img.stride);
    let period = period.max(1);
    let step = |c: usize, d: isize, n: usize| match d {
        -1 => c.checked_sub(period),
        1 => c.checked_add(period).filter(|&v| v < n),
        _ => Some(c),
    };
    let data: &[T] = img.data;
    let chunks: Vec<&[usize]> = bad.chunks(4096).collect();
    let fixes = par::map(chunks, parallel, |chunk| {
        let mut out: Vec<(usize, T)> = Vec::with_capacity(chunk.len() * p);
        let mut good = [0usize; 8];
        for &pos in chunk {
            let (i, j) = (pos % w, pos / w);
            let mut n = 0;
            for (dx, dy) in NEIGHBOURS {
                if let (Some(ni), Some(nj)) = (step(i, dx, w), step(j, dy, h)) {
                    if bad.binary_search(&(nj * w + ni)).is_err() {
                        good[n] = (nj * stride + ni) * p;
                        n += 1;
                    }
                }
            }
            if n == 0 {
                continue;
            }
            let at = (j * stride + i) * p;
            for q in 0..p {
                let mut sum = 0.0f32;
                let mut count = 0.0f32;
                for &g in &good[..n] {
                    sum += data[g + q].to_f32();
                    count += 1.0;
                }
                out.push((at + q, T::from_mean(sum / count)));
            }
        }
        out
    });
    for (idx, v) in fixes.into_iter().flatten() {
        img.data[idx] = v;
    }
}

/// Run a bad-pixel opcode; `is_constant(sample, constant)` tells whether a first sample holds
/// the FixBadPixelsConstant value. Nothing happens unless every window sample is present.
fn fix_bad_pixels<T: Sample>(
    op: &Opcode,
    img: &mut PlaneImage<T>,
    period: usize,
    parallel: bool,
    is_constant: impl Fn(T, u32) -> bool + Sync,
) {
    let complete = img.width > 0
        && img.height > 0
        && img.planes > 0
        && img.stride >= img.width
        && ((img.height - 1) as u128 * img.stride as u128 + img.width as u128) * img.planes as u128
            <= img.data.len() as u128;
    if !complete {
        return;
    }
    let bad = match op {
        Opcode::FixBadPixelsConstant { constant, .. } => scan_bad(img, parallel, |v| is_constant(v, *constant)),
        Opcode::FixBadPixelsList { points, rects, .. } => listed_bad(img, points, rects),
        _ => return,
    };
    repair(img, &bad, period, parallel);
}

// ============================================================================ list application

fn skipped_names(ops: &[Opcode]) -> Vec<&'static str> {
    ops.iter().filter(|o| !o.is_pixel_opcode()).map(Opcode::name).collect()
}

/// Run prepared row-local kernels, in order, on every row of the window.
fn run_kernels(kernels: &[Kernel], img: &mut PlaneImage, parallel: bool) {
    if kernels.is_empty() {
        return;
    }
    let need = kernels.iter().map(Kernel::scratch_len).max().unwrap_or(0);
    for_each_window_row(img, parallel, |y, row, scratch| {
        scratch.resize(need, 0.0);
        for k in kernels {
            k.run(y, row, scratch);
        }
    });
}

/// Apply the pixel opcodes of `ops` to `img`; returns the names of the opcodes not applied.
pub fn apply(
    ops: &[Opcode],
    img: &mut PlaneImage,
    cfa_period: usize,
    value_scale: f32,
    parallel: bool,
) -> Vec<&'static str> {
    let g = Geometry {
        width: img.width,
        planes: img.planes,
        x0: img.x0,
        full_width: img.full_width,
        full_height: img.full_height,
        scale: value_scale,
    };
    // Row-local opcodes between two repairs see only their own rows: run them together, row
    // by row. A repair needs the whole window as left by everything before it.
    let mut pending: Vec<Kernel> = Vec::new();
    for op in ops {
        if op.is_pixel_opcode() && op.is_repair() {
            run_kernels(&pending, img, parallel);
            pending.clear();
            fix_bad_pixels(op, img, cfa_period, parallel, |v: f32, c| v == c as f32 / value_scale);
        } else if let Some(k) = Kernel::prepare(op, &g) {
            pending.push(k);
        }
    }
    run_kernels(&pending, img, parallel);
    skipped_names(ops)
}

/// True when [`apply_u16`] handles `ops` with the result of [`apply`] rounded to u16.
pub fn u16_exact(ops: &[Opcode]) -> bool {
    let mut repairs = 0;
    for op in ops.iter().filter(|o| o.is_pixel_opcode()) {
        if op.is_repair() {
            repairs += 1;
            if repairs > 1 {
                return false;
            }
        } else if !matches!(op, Opcode::MapTable { .. }) {
            return false;
        }
    }
    true
}

/// [`apply`] on stored u16 values (value scale 1); `None` when [`u16_exact`] is false.
pub fn apply_u16(
    ops: &[Opcode],
    img: &mut PlaneImage<u16>,
    cfa_period: usize,
    parallel: bool,
) -> Option<Vec<&'static str>> {
    if !u16_exact(ops) {
        return None;
    }
    for op in ops {
        match op {
            Opcode::MapTable { region, table } => {
                let sel = Selection::new(region, img.x0, img.width, img.planes);
                let Some(last) = table.len().checked_sub(1) else { continue };
                if sel.is_empty() {
                    continue;
                }
                for_each_window_row(img, parallel, |y, row, _| {
                    if sel.rows.index(y).is_some() {
                        sel.visit(row, |_, _, v| *v = table[(*v as usize).min(last)]);
                    }
                });
            }
            Opcode::FixBadPixelsConstant { .. } | Opcode::FixBadPixelsList { .. } => {
                fix_bad_pixels(op, img, cfa_period, parallel, |v: u16, c| u32::from(v) == c)
            }
            _ => {}
        }
    }
    Some(skipped_names(ops))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn region(top: u32, left: u32, bottom: u32, right: u32, plane: u32, planes: u32, rp: u32, cp: u32) -> Region {
        Region { top, left, bottom, right, plane, planes, row_pitch: rp, col_pitch: cp }
    }

    fn be(out: &mut Vec<u8>, words: &[u32]) {
        for w in words {
            out.extend(w.to_be_bytes());
        }
    }

    fn doubles(out: &mut Vec<u8>, v: &[f64]) {
        for d in v {
            out.extend(d.to_bits().to_be_bytes());
        }
    }

    fn floats(out: &mut Vec<u8>, v: &[f32]) {
        for d in v {
            out.extend(d.to_bits().to_be_bytes());
        }
    }

    /// A list of `(id, flags, body)` entries.
    fn list(entries: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        be(&mut out, &[entries.len() as u32]);
        for (id, flags, body) in entries {
            be(&mut out, &[*id, 0x0103_0000, *flags, body.len() as u32]);
            out.extend(body);
        }
        out
    }

    #[test]
    fn parses_and_applies_gain_map_and_scale_per_row() {
        let mut gm = Vec::new();
        be(&mut gm, &[0, 0, 2, 2, 0, 1, 1, 1, 2, 2]);
        doubles(&mut gm, &[1.0, 1.0, 0.0, 0.0]);
        be(&mut gm, &[1]);
        floats(&mut gm, &[1.0, 2.0, 3.0, 4.0]);
        let mut sr = Vec::new();
        be(&mut sr, &[0, 0, 2, 2, 0, 1, 0, 0, 2]);
        floats(&mut sr, &[0.5, 2.0]);
        let entries = parse_entries(&list(&[(9, 1, gm), (12, 2, sr)])).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].optional && !entries[0].preview_only);
        assert!(!entries[1].optional && entries[1].preview_only);
        match &entries[1].op {
            Opcode::ScalePerRow { region: r, scales } => {
                assert_eq!((r.row_pitch, r.col_pitch), (1, 1), "pitch 0 is read as 1");
                assert_eq!(scales, &[0.5, 2.0]);
            }
            o => panic!("{o:?}"),
        }
        let ops: Vec<Opcode> = entries.into_iter().map(|e| e.op).collect();
        // 2x2 image; map points at relative 0 and 1, so pixel 1 (relative 0.5) is midway.
        let mut px = vec![1.0f32; 4];
        let skipped = apply(&ops, &mut PlaneImage::new(&mut px, 2, 2, 1), 1, 65535.0, false);
        assert!(skipped.is_empty());
        assert_eq!(px, vec![0.5, 0.75, 4.0, 5.0]);
    }

    #[test]
    fn malformed_lists_fail_with_corrupt_file() {
        let mut too_many = Vec::new();
        be(&mut too_many, &[4097]);
        let mut cut = list(&[(3, 0, vec![0; 56])]);
        cut.truncate(cut.len() - 1);
        for c in [vec![0, 0], too_many, list(&[(9, 0, vec![0; 40])]), cut] {
            let e = parse(&c).unwrap_err();
            assert_eq!(e.kind(), "CorruptFile", "{e}");
        }
        let mut b = Vec::new();
        be(&mut b, &[0, 0, 0, 0, 0, 0, 0, 0, 9]);
        assert_eq!(parse(&list(&[(8, 0, b)])).unwrap_err().to_string(), "corrupt file: invalid MapPolynomial degree");
        // Unknown opcodes and broken warps are not errors.
        let ops = parse(&list(&[(14, 1, vec![1, 2, 3]), (1, 0, vec![0, 0, 0, 9])])).unwrap();
        assert_eq!(ops[0], Opcode::Other { id: 14, optional: true });
        assert_eq!(ops[1], Opcode::WarpRectilinear { planes: Vec::new(), cx: 0.5, cy: 0.5 });
        assert_eq!(parse(&[0, 0, 0, 0]).unwrap(), Vec::new());
    }

    #[test]
    fn bad_pixels_take_the_mean_of_same_colour_neighbours() {
        let (w, h) = (6, 6);
        let mut px: Vec<f32> = (0..w * h).map(|i| i as f32).collect();
        let ops = vec![Opcode::FixBadPixelsList { bayer_phase: 0, points: vec![(2, 2)], rects: vec![] }];
        apply(&ops, &mut PlaneImage::new(&mut px, w, h, 1), 2, 1.0, false);
        // All eight neighbours two pixels away are inside the image.
        let expect = [12, 16, 2, 26, 0, 28, 24, 4].iter().map(|&i| i as f32).sum::<f32>() / 8.0;
        assert_eq!(px[2 * w + 2], expect);
        let mut u: Vec<u16> = (0..(w * h) as u16).collect();
        u[0] = 7;
        let ops = vec![Opcode::FixBadPixelsConstant { constant: 7, bayer_phase: 0 }];
        apply_u16(&ops, &mut PlaneImage::new(&mut u, w, h, 1), 2, false).unwrap();
        // Pixels 0 and 7 hold 7; pixel 0's good neighbours hold 2, 12 and 14.
        assert_eq!(u[0], 9);
    }

    #[test]
    fn a_window_matches_the_whole_image() {
        let (fw, fh, p) = (20usize, 16usize, 2usize);
        let ops = vec![
            Opcode::GainMap {
                region: region(1, 1, 16, 20, 0, 2, 2, 2),
                points_v: 3,
                points_h: 4,
                spacing_v: 0.5,
                spacing_h: 0.3,
                origin_v: 0.0,
                origin_h: 0.05,
                map_planes: 1,
                gains: (0..12).map(|i| 1.0 + i as f32 / 10.0).collect(),
            },
            Opcode::DeltaPerRow { region: region(0, 0, 16, 20, 1, 1, 3, 1), deltas: vec![0.001, 0.002, 0.003] },
            Opcode::FixVignetteRadial { k: [0.3, 0.1, 0.0, 0.0, 0.0], cx: 0.4, cy: 0.6 },
        ];
        let whole: Vec<f32> = (0..fw * fh * p).map(|i| (i % 97) as f32 * 300.0).collect();
        let mut a = whole.clone();
        apply(&ops, &mut PlaneImage::new(&mut a, fw, fh, p), 2, 1.0, false);
        let (x0, y0, w, h) = (3, 5, 11, 7);
        let span = |v: &[f32], j: usize| v[((y0 + j) * fw + x0) * p..((y0 + j) * fw + x0 + w) * p].to_vec();
        let mut win: Vec<f32> = (0..h).flat_map(|j| span(&whole, j)).collect();
        let mut img = PlaneImage {
            data: &mut win,
            width: w,
            height: h,
            planes: p,
            stride: w,
            x0,
            y0,
            full_width: fw,
            full_height: fh,
        };
        apply(&ops, &mut img, 2, 1.0, true);
        let rows = RowOps::new(&ops, w, p, x0, fw, fh, 1.0).unwrap();
        assert!(!rows.is_empty());
        for j in 0..h {
            let expect = span(&a, j);
            assert_eq!(&win[j * w * p..(j + 1) * w * p], &expect[..]);
            let mut row = span(&whole, j);
            rows.apply(y0 + j, &mut row);
            assert_eq!(row, expect);
        }
    }

    #[test]
    fn stored_integer_path_matches_the_float_path() {
        let (w, h) = (9usize, 7usize);
        let ops = vec![
            Opcode::MapTable { region: region(0, 1, 7, 9, 0, 1, 2, 2), table: (0..300u16).map(|i| i * 3).collect() },
            Opcode::FixBadPixelsList { bayer_phase: 0, points: vec![(3, 4), (0, 0)], rects: vec![(5, 5, 7, 9)] },
            Opcode::TrimBounds { top: 0, left: 0, bottom: 1, right: 1 },
        ];
        assert!(u16_exact(&ops));
        let base: Vec<u16> = (0..(w * h) as u16).map(|i| i * 5 % 400).collect();
        let mut u = base.clone();
        let names = apply_u16(&ops, &mut PlaneImage::new(&mut u, w, h, 1), 2, false).unwrap();
        let mut f: Vec<f32> = base.iter().map(|&v| f32::from(v)).collect();
        assert_eq!(apply(&ops, &mut PlaneImage::new(&mut f, w, h, 1), 2, 1.0, false), names);
        assert_eq!(names, vec!["TrimBounds"]);
        let rounded: Vec<u16> = f.iter().map(|&v| u16::from_mean(v)).collect();
        assert_eq!(u, rounded);
        let two = [ops[1].clone(), ops[1].clone()];
        assert!(!u16_exact(&two));
        assert!(RowOps::new(&two, w, 1, 0, w, h, 1.0).is_none());
    }

    #[test]
    fn radial_vignette_reaches_one_plus_k0_at_the_corner() {
        let mut body = Vec::new();
        doubles(&mut body, &[0.5, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5]);
        let ops = parse(&list(&[(3, 0, body)])).unwrap();
        assert_eq!(ops[0], Opcode::FixVignetteRadial { k: [0.5, 0.0, 0.0, 0.0, 0.0], cx: 0.5, cy: 0.5 });
        let mut px = vec![1.0f32; 16];
        apply(&ops, &mut PlaneImage::new(&mut px, 4, 4, 1), 1, 1.0, false);
        // Pixel (0, 0) is the image corner, at distance 1 relative to the farthest corner;
        // pixel (2, 2) is the centre.
        assert_eq!(px[0], 1.5);
        assert_eq!(px[2 * 4 + 2], 1.0);
    }
}
