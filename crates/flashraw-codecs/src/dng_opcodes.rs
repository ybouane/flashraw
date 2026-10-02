//! DNG opcode lists (OpcodeList1/2/3, DNG spec 1.3+ chapter 7).
//!
//! Opcode lists carry processing steps the camera maker specifies: bad-pixel repair,
//! lens-shading gain maps (ubiquitous in smartphone DNGs), linearisation tables and
//! polynomials, per-row/column offsets and scales, and geometric corrections.
//!
//! FlashRaw parses all lists and can apply the pixel-value opcodes to f32 planes;
//! geometric opcodes (WarpRectilinear, WarpFisheye, FixVignetteRadial, TrimBounds)
//! are parsed and reported but not applied by the sensor-domain decoder.

use flashraw_core::bytes::Cursor;
use flashraw_core::{ByteOrder, Error, Result};

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
    FixVignetteRadial,
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
            Opcode::FixVignetteRadial => "FixVignetteRadial",
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
            Opcode::WarpRectilinear
                | Opcode::WarpFisheye
                | Opcode::FixVignetteRadial
                | Opcode::TrimBounds { .. }
                | Opcode::Other { .. }
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
            3 => Opcode::FixVignetteRadial,
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

/// A mutable f32 image: `width x height` pixels with `planes` interleaved samples.
pub struct PlaneImage<'a> {
    pub data: &'a mut [f32],
    pub width: usize,
    pub height: usize,
    pub planes: usize,
}

impl PlaneImage<'_> {
    fn for_region(&mut self, r: &Region, mut f: impl FnMut(usize, usize, usize, &mut f32)) {
        let (w, h, p) = (self.width, self.height, self.planes);
        let bottom = (r.bottom as usize).min(h);
        let right = (r.right as usize).min(w);
        let plane_end = (r.plane as usize).saturating_add(r.planes as usize).min(p);
        let mut y = r.top as usize;
        while y < bottom {
            let mut x = r.left as usize;
            while x < right {
                for pl in r.plane as usize..plane_end {
                    f(x, y, pl, &mut self.data[(y * w + x) * p + pl]);
                }
                x += r.col_pitch as usize;
            }
            y += r.row_pitch as usize;
        }
    }
}

/// Apply pixel opcodes to `img`, whose samples are in the units the list expects
/// (OpcodeList1: stored values; OpcodeList2: linear reference values in [0,1];
/// OpcodeList3: demosaiced values). `cfa_period` is the CFA repeat (2 for Bayer) used by
/// the bad-pixel opcodes. Returns the names of opcodes that were skipped.
pub fn apply(ops: &[Opcode], img: &mut PlaneImage, cfa_period: usize, value_scale: f32) -> Vec<&'static str> {
    let mut skipped = Vec::new();
    for op in ops {
        match op {
            Opcode::MapTable { region, table } if !table.is_empty() => {
                let last = (table.len() - 1) as f32;
                img.for_region(region, |_, _, _, v| {
                    let i = (*v * value_scale).round().clamp(0.0, last) as usize;
                    *v = table[i] as f32 / value_scale;
                });
            }
            Opcode::MapPolynomial { region, coeffs } => img.for_region(region, |_, _, _, v| {
                let x = *v as f64;
                let mut acc = 0f64;
                for c in coeffs.iter().rev() {
                    acc = acc * x + c;
                }
                *v = acc as f32;
            }),
            Opcode::DeltaPerRow { region, deltas } => img.for_region(region, |_, y, _, v| {
                let i = (y - region.top as usize) / region.row_pitch as usize;
                *v += deltas.get(i).copied().unwrap_or(0.0);
            }),
            Opcode::DeltaPerColumn { region, deltas } => img.for_region(region, |x, _, _, v| {
                let i = (x - region.left as usize) / region.col_pitch as usize;
                *v += deltas.get(i).copied().unwrap_or(0.0);
            }),
            Opcode::ScalePerRow { region, scales } => img.for_region(region, |_, y, _, v| {
                let i = (y - region.top as usize) / region.row_pitch as usize;
                *v *= scales.get(i).copied().unwrap_or(1.0);
            }),
            Opcode::ScalePerColumn { region, scales } => img.for_region(region, |x, _, _, v| {
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
                let (w, h) = (img.width as f64, img.height as f64);
                let gain = |row: f64, col: f64, plane: usize| -> f32 {
                    // Map coordinates are relative to the image (0..1).
                    let fv = ((row / h - origin_v) / spacing_v).clamp(0.0, (pv - 1) as f64);
                    let fh = ((col / w - origin_h) / spacing_h).clamp(0.0, (ph - 1) as f64);
                    let (v0, h0) = (fv.floor() as usize, fh.floor() as usize);
                    let (v1, h1) = ((v0 + 1).min(pv - 1), (h0 + 1).min(ph - 1));
                    let (tv, th) = ((fv - v0 as f64) as f32, (fh - h0 as f64) as f32);
                    let p = plane.min(mp - 1);
                    let g = |vi: usize, hi: usize| gains[(vi * ph + hi) * mp + p];
                    let top = g(v0, h0) * (1.0 - th) + g(v0, h1) * th;
                    let bot = g(v1, h0) * (1.0 - th) + g(v1, h1) * th;
                    top * (1.0 - tv) + bot * tv
                };
                let first_plane = region.plane as usize;
                img.for_region(region, |x, y, pl, v| *v *= gain(y as f64, x as f64, pl - first_plane));
            }
            Opcode::FixBadPixelsConstant { constant, bayer_phase } => {
                let c = *constant as f32 / value_scale;
                fix_bad_pixels(img, cfa_period, *bayer_phase, |v| v == c, &[], &[]);
            }
            Opcode::FixBadPixelsList { bayer_phase, points, rects } => {
                fix_bad_pixels(img, cfa_period, *bayer_phase, |_| false, points, rects);
            }
            other => skipped.push(other.name()),
        }
    }
    skipped
}

/// Replace bad pixels by the mean of the nearest same-colour neighbours (distance =
/// CFA period) that are not themselves bad.
fn fix_bad_pixels(
    img: &mut PlaneImage,
    period: usize,
    _phase: u32,
    is_bad_value: impl Fn(f32) -> bool,
    points: &[(u32, u32)],
    rects: &[(u32, u32, u32, u32)],
) {
    let (w, h, p) = (img.width, img.height, img.planes);
    let period = period.max(1);
    let mut bad = vec![false; w * h];
    for (i, b) in bad.iter_mut().enumerate() {
        *b = is_bad_value(img.data[i * p]);
    }
    for &(row, col) in points {
        if (row as usize) < h && (col as usize) < w {
            bad[row as usize * w + col as usize] = true;
        }
    }
    for &(t, l, bm, r) in rects {
        for y in (t as usize)..(bm as usize).min(h) {
            for x in (l as usize)..(r as usize).min(w) {
                bad[y * w + x] = true;
            }
        }
    }
    let src: Vec<f32> = img.data.to_vec();
    for y in 0..h {
        for x in 0..w {
            if !bad[y * w + x] {
                continue;
            }
            for pl in 0..p {
                let (mut s, mut n) = (0f32, 0f32);
                for (dx, dy) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
                    let xx = x as isize + dx * period as isize;
                    let yy = y as isize + dy * period as isize;
                    if xx >= 0
                        && yy >= 0
                        && (xx as usize) < w
                        && (yy as usize) < h
                        && !bad[yy as usize * w + xx as usize]
                    {
                        s += src[(yy as usize * w + xx as usize) * p + pl];
                        n += 1.0;
                    }
                }
                if n > 0.0 {
                    img.data[(y * w + x) * p + pl] = s / n;
                }
            }
        }
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
        let mut img = PlaneImage { data: &mut data, width: 2, height: 2, planes: 1 };
        assert!(apply(&ops, &mut img, 2, 1.0).is_empty());
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
        let mut img = PlaneImage { data: &mut data, width: 6, height: 6, planes: 1 };
        apply(&ops, &mut img, 2, 1.0);
        assert!(data[14] < 100.0);
    }
}
