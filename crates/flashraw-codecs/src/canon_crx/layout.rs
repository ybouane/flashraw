//! CRX stream layout: the per-sample header (tiles → planes → subbands with their
//! sizes and quantisation parameters), subband geometry including the inter-tile
//! overlap coefficients, and the C-RAW v2 quantisation maps.

use super::CrxHeader;
use super::entropy::decode_qp_map;
use super::plane::q_step;
use flashraw_core::{Error, Result};

/// Neighbour flags of a tile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TileFlags {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// One subband of one plane of one tile.
#[derive(Debug, Clone, Default)]
pub(crate) struct Subband {
    pub width: usize,
    pub height: usize,
    /// Absolute offset of the coded data within the sample slice.
    pub data_offset: usize,
    /// Coded size (0 = band is all zero).
    pub data_size: usize,
    /// C-RAW v1: the quantisation parameter is updated at every line.
    pub supports_partial: bool,
    pub q_param: i32,
    pub q_step_base: i32,
    pub q_step_mult: u32,
    pub row_start: i32,
    pub row_end: i32,
    pub col_start: i32,
    pub col_end: i32,
    pub level_shift: i32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Plane {
    /// Predictive (vs detail) coding of the first subband.
    pub supports_partial: bool,
    pub rounded_mask: i32,
    pub bands: Vec<Subband>,
}

/// Quantisation steps of one wavelet level (C-RAW v2).
#[derive(Debug, Clone, Default)]
pub(crate) struct QStep {
    pub table: Vec<u32>,
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Tile {
    /// Position and size in plane coordinates.
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub flags: TileFlags,
    pub planes: Vec<Plane>,
    /// Per-level quantisation steps (coarsest first) when the tile carries a QP map.
    pub qstep: Option<Vec<QStep>>,
}

/// Plane-level geometry of a CRX image.
#[derive(Debug, Clone)]
pub(crate) struct Image {
    pub plane_width: usize,
    pub plane_height: usize,
    pub tile_cols: usize,
    pub tile_rows: usize,
    pub tiles: Vec<Tile>,
}

/// Number of overlap coefficients added to subbands at tile edges, indexed by
/// `[levels - 1][tile size & 7][2 * level + {0: high-pass, 1: low-pass}]`.
const EX_COEF: [[[i32; 6]; 8]; 3] = [
    [
        [1, 1, 0, 0, 0, 0],
        [1, 0, 0, 0, 0, 0],
        [1, 1, 0, 0, 0, 0],
        [1, 0, 0, 0, 0, 0],
        [1, 1, 0, 0, 0, 0],
        [1, 0, 0, 0, 0, 0],
        [1, 1, 0, 0, 0, 0],
        [1, 0, 0, 0, 0, 0],
    ],
    [
        [1, 1, 1, 1, 0, 0],
        [1, 0, 1, 0, 0, 0],
        [1, 2, 2, 1, 0, 0],
        [1, 1, 1, 1, 0, 0],
        [1, 1, 1, 1, 0, 0],
        [1, 0, 1, 0, 0, 0],
        [1, 2, 2, 1, 0, 0],
        [1, 1, 1, 1, 0, 0],
    ],
    [
        [1, 1, 1, 1, 1, 1],
        [1, 0, 1, 0, 1, 0],
        [1, 2, 2, 2, 2, 1],
        [1, 1, 1, 2, 2, 1],
        [1, 1, 1, 2, 2, 1],
        [1, 0, 1, 1, 1, 1],
        [1, 2, 2, 1, 1, 1],
        [1, 1, 1, 1, 1, 1],
    ],
];

fn be16(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as u32).ok_or_else(truncated)
}
fn be32(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(truncated)
}
fn truncated() -> Error {
    Error::corrupt("CRX: truncated image header")
}

impl Image {
    /// Plane geometry and tile grid from the codec header (no sample data needed).
    pub(crate) fn grid(h: &CrxHeader) -> Result<Image> {
        let div = if h.planes == 4 { 2 } else { 1 };
        let (pw, ph) = ((h.width / div) as usize, (h.height / div) as usize);
        let (tw, th) = ((h.tile_width / div) as usize, (h.tile_height / div) as usize);
        if tw < 0x16 || th < 0x16 || pw > 0x7FFF || ph > 0x7FFF {
            return Err(Error::corrupt("CRX: invalid tile/plane size"));
        }
        let cols = pw.div_ceil(tw);
        let rows = ph.div_ceil(th);
        if cols == 0
            || rows == 0
            || cols > 0xFF
            || rows > 0xFF
            || pw < tw * (cols - 1) + 0x16
            || ph < th * (rows - 1) + 0x16
        {
            return Err(Error::corrupt("CRX: invalid tile grid"));
        }
        let mut tiles = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            for c in 0..cols {
                let last_c = c + 1 == cols;
                let last_r = r + 1 == rows;
                tiles.push(Tile {
                    x: c * tw,
                    y: r * th,
                    width: if last_c { pw - tw * (cols - 1) } else { tw },
                    height: if last_r { ph - th * (rows - 1) } else { th },
                    flags: TileFlags { left: c > 0, right: !last_c, top: r > 0, bottom: !last_r },
                    ..Default::default()
                });
            }
        }
        Ok(Image { plane_width: pw, plane_height: ph, tile_cols: cols, tile_rows: rows, tiles })
    }

    /// Parse the per-sample header (`data[..h.mdat_header_size]`) and resolve the
    /// subband data ranges within `data`.
    pub(crate) fn parse(h: &CrxHeader, data: &[u8]) -> Result<Image> {
        let mut img = Image::grid(h)?;
        let hdr_size = h.mdat_header_size as usize;
        let hdr = data.get(..hdr_size).ok_or_else(|| Error::corrupt("CRX: sample shorter than its header"))?;
        let levels = h.levels as usize;
        let nbands = 3 * levels + 1;
        let mut pos = 0usize;
        let mut tile_offset = 0u64;
        let mut qp_info = Vec::with_capacity(img.tiles.len());
        for (ti, tile) in img.tiles.iter_mut().enumerate() {
            let sign = be16(hdr, pos)?;
            let size = be16(hdr, pos + 2)? as usize;
            if !((sign == 0xFF01 && size == 8) || (sign == 0xFF11 && (size == 8 || size == 16))) {
                return Err(Error::corrupt("CRX: bad tile header"));
            }
            if hdr.len() < pos + size + 4 {
                return Err(truncated());
            }
            let tail = be16(hdr, pos + 10)?;
            if (size == 8 && tail != 0) || (size == 16 && tail != 0x4000) {
                return Err(Error::corrupt("CRX: bad tile header tail"));
            }
            if be16(hdr, pos + 8)? as usize != ti {
                return Err(Error::corrupt("CRX: tile number mismatch"));
            }
            let tile_size = be32(hdr, pos + 4)? as u64;
            let (qp_size, extra) = if size == 16 {
                if be16(hdr, pos + 18)? != 0 {
                    return Err(Error::corrupt("CRX: bad extended tile header"));
                }
                (be32(hdr, pos + 12)? as u64, be16(hdr, pos + 16)? as u64)
            } else {
                (0, 0)
            };
            pos += size + 4;
            qp_info.push((size == 16, hdr_size as u64 + tile_offset, qp_size));
            let mut comp_offset = 0u64;
            for pi in 0..h.planes as usize {
                if hdr.len() < pos + 12 {
                    return Err(truncated());
                }
                let sign = be16(hdr, pos)?;
                let size = be16(hdr, pos + 2)?;
                if (sign != 0xFF02 && sign != 0xFF12) || size != 8 {
                    return Err(Error::corrupt("CRX: bad plane header"));
                }
                let flags = hdr[pos + 8];
                if (flags >> 4) as usize != pi || hdr[pos + 9] != 0 || hdr[pos + 10] != 0 || hdr[pos + 11] != 0 {
                    return Err(Error::corrupt("CRX: bad plane header fields"));
                }
                let comp_size = be32(hdr, pos + 4)? as u64;
                let rounded_bits = (flags >> 1) & 3;
                let mut plane = Plane { supports_partial: flags & 8 != 0, rounded_mask: 0, bands: Vec::new() };
                if rounded_bits != 0 {
                    if levels != 0 || !plane.supports_partial {
                        return Err(Error::corrupt("CRX: rounded coding with wavelet levels"));
                    }
                    plane.rounded_mask = 1 << (rounded_bits - 1);
                }
                pos += 12;
                let base = hdr_size as u64 + tile_offset + qp_size + extra + comp_offset;
                plane.bands = parse_subbands(hdr, &mut pos, nbands, base, data.len())?;
                set_subband_geometry(h, tile, levels, &mut plane.bands);
                comp_offset += comp_size;
                tile.planes.push(plane);
            }
            tile_offset += tile_size;
        }
        if h.version == 0x200 {
            for (tile, (has_qp, off, size)) in img.tiles.iter_mut().zip(qp_info) {
                if !has_qp {
                    continue;
                }
                let qp_w = tile.width.div_ceil(8);
                let qp_h = tile.height.div_ceil(2);
                let start = usize::try_from(off).ok().filter(|&o| o <= data.len()).ok_or_else(truncated)?;
                let end = usize::try_from(off + size).unwrap_or(usize::MAX).min(data.len());
                let map = decode_qp_map(&data[start..end], qp_w, qp_h)?;
                tile.qstep = Some(make_qsteps(&map, qp_w, qp_h, tile.height, levels)?);
            }
        }
        Ok(img)
    }
}

fn parse_subbands(hdr: &[u8], pos: &mut usize, n: usize, base: u64, data_len: usize) -> Result<Vec<Subband>> {
    let mut bands = Vec::with_capacity(n);
    let mut offset = 0u64;
    for i in 0..n {
        let p = *pos;
        let sign = be16(hdr, p)?;
        let size = be16(hdr, p + 2)? as usize;
        if hdr.len() < p + size + 4 {
            return Err(truncated());
        }
        if !((sign == 0xFF03 && size == 8) || (sign == 0xFF13 && size == 16)) {
            return Err(Error::corrupt("CRX: bad subband header"));
        }
        let band_size = be32(hdr, p + 4)? as i32 as i64;
        if (hdr[p + 8] >> 4) as usize != i {
            return Err(Error::corrupt("CRX: subband number mismatch"));
        }
        let mut b = Subband::default();
        let data_size = if sign == 0xFF03 {
            let bits = be32(hdr, p + 8)?;
            b.supports_partial = bits & 0x0800_0000 != 0;
            b.q_param = ((bits >> 19) & 0xFF) as i32;
            band_size - (bits & 0x7FFFF) as i64
        } else {
            if be16(hdr, p + 8)? & 0xFFF != 0 || be16(hdr, p + 18)? != 0 {
                return Err(Error::corrupt("CRX: unsupported subband header"));
            }
            b.q_step_base = be32(hdr, p + 12)? as i32;
            b.q_step_mult = be16(hdr, p + 10)?;
            band_size - be16(hdr, p + 16)? as i64
        };
        if data_size < 0 {
            return Err(Error::corrupt("CRX: negative subband size"));
        }
        let abs = base + offset;
        b.data_size = data_size as usize;
        b.data_offset = if b.data_size > 0 {
            usize::try_from(abs).ok().filter(|&o| o <= data_len).ok_or_else(truncated)?
        } else {
            0
        };
        offset = offset.wrapping_add(band_size as u64);
        *pos += size + 4;
        bands.push(b);
    }
    Ok(bands)
}

/// Subband sizes of a tile plane: each level halves the tile (rounding the low-pass
/// up), plus overlap coefficients towards neighbouring tiles.
fn set_subband_geometry(h: &CrxHeader, tile: &Tile, levels: usize, bands: &mut [Subband]) {
    let v2 = h.version == 0x200;
    let set_idx = |b: &mut Subband, level: i32, cs: i32, ce: i32, rs: i32, re: i32| {
        if v2 {
            (b.row_start, b.row_end, b.col_start, b.col_end, b.level_shift) = (rs, re, cs, ce, 3 - level);
        }
    };
    let (mut bw, mut bh) = (tile.width as i32, tile.height as i32);
    if levels == 0 {
        if let Some(b) = bands.first_mut() {
            b.width = bw as usize;
            b.height = bh as usize;
        }
        return;
    }
    let row_ex = &EX_COEF[levels - 1][tile.width & 7];
    let col_ex = &EX_COEF[levels - 1][tile.height & 7];
    let f = tile.flags;
    let mut idx = bands.len() - 1;
    for level in 0..levels {
        let (odd_w, odd_h) = (bw & 1, bh & 1);
        bw = (bw + odd_w) >> 1;
        bh = (bh + odd_h) >> 1;
        let (mut w0, mut w1, mut h0, mut h1, mut cs, mut rs) = (0, 0, 0, 0, 0, 0);
        if f.right {
            w0 = row_ex[2 * level];
            w1 = row_ex[2 * level + 1];
        }
        if f.left {
            w0 += 1;
            cs = 1;
        }
        if f.bottom {
            h0 = col_ex[2 * level];
            h1 = col_ex[2 * level + 1];
        }
        if f.top {
            h0 += 1;
            rs = 1;
        }
        let lvl = level as i32 + 1;
        let b = &mut bands[idx];
        b.width = (bw + w0 - odd_w).max(0) as usize;
        b.height = (bh + h0 - odd_h).max(0) as usize;
        set_idx(b, lvl, cs, w0 - cs, rs, h0 - rs);
        let b = &mut bands[idx - 1];
        b.width = (bw + w1).max(0) as usize;
        b.height = (bh + h0 - odd_h).max(0) as usize;
        set_idx(b, lvl, 0, w1, rs, h0 - rs);
        let b = &mut bands[idx - 2];
        b.width = (bw + w0 - odd_w).max(0) as usize;
        b.height = (bh + h1).max(0) as usize;
        set_idx(b, lvl, cs, w0 - cs, 0, h1);
        idx -= 3;
    }
    let ew = if f.right { row_ex[2 * levels - 1] } else { 0 };
    let eh = if f.bottom { col_ex[2 * levels - 1] } else { 0 };
    let b = &mut bands[0];
    b.width = (bw + ew) as usize;
    b.height = (bh + eh) as usize;
    set_idx(b, levels as i32, 0, ew, 0, eh);
}

/// Quantisation steps per level from the QP map (finest level at full map
/// resolution, coarser levels averaging 2 / 4 map rows).
fn make_qsteps(map: &[i32], qp_w: usize, qp_h: usize, tile_h: usize, levels: usize) -> Result<Vec<QStep>> {
    if !(1..=3).contains(&levels) {
        return Err(Error::corrupt("CRX: quantisation map needs 1-3 wavelet levels"));
    }
    let qp_h4 = tile_h.div_ceil(4);
    let qp_h8 = tile_h.div_ceil(8);
    let at = |row: usize, col: usize| map[qp_w * row.min(qp_h - 1) + col];
    let mut out = Vec::with_capacity(levels);
    if levels == 3 {
        let mut t = Vec::with_capacity(qp_w * qp_h8);
        for r in 0..qp_h8 {
            for c in 0..qp_w {
                let s = at(4 * r, c)
                    .wrapping_add(at(4 * r + 1, c))
                    .wrapping_add(at(4 * r + 2, c))
                    .wrapping_add(at(4 * r + 3, c));
                // Rounds towards zero like a C division by 4.
                let q = (s + if s < 0 { 3 } else { 0 }) >> 2;
                t.push(q_step(q));
            }
        }
        out.push(QStep { table: t, width: qp_w, height: qp_h8 });
    }
    if levels >= 2 {
        let mut t = Vec::with_capacity(qp_w * qp_h4);
        for r in 0..qp_h4 {
            for c in 0..qp_w {
                t.push(q_step(at(2 * r, c).wrapping_add(at(2 * r + 1, c)) / 2));
            }
        }
        out.push(QStep { table: t, width: qp_w, height: qp_h4 });
    }
    out.push(QStep { table: map.iter().map(|&q| q_step(q)).collect(), width: qp_w, height: qp_h });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(w: u32, h: u32, tw: u32, th: u32, levels: u8) -> CrxHeader {
        CrxHeader {
            version: 0x100,
            width: w,
            height: h,
            tile_width: tw,
            tile_height: th,
            bits: 14,
            planes: 4,
            cfa_layout: 0,
            enc_type: 0,
            levels,
            mdat_header_size: 0,
            median_bits: 14,
        }
    }

    #[test]
    fn tile_grid() {
        let img = Image::grid(&header(6888, 4546, 3444, 4546, 3)).unwrap();
        assert_eq!((img.plane_width, img.plane_height, img.tile_cols, img.tile_rows), (3444, 2273, 2, 1));
        assert_eq!(img.tiles[0].flags, TileFlags { left: false, right: true, top: false, bottom: false });
        assert_eq!(img.tiles[1].flags, TileFlags { left: true, right: false, top: false, bottom: false });
        assert_eq!((img.tiles[1].x, img.tiles[1].width), (1722, 1722));
        // Last tile too small.
        assert!(Image::grid(&header(200, 200, 90, 200, 0)).is_err());
        assert!(Image::grid(&header(100, 100, 40, 100, 0)).is_err());
    }

    #[test]
    fn subband_sizes_single_tile() {
        let h = header(200, 100, 200, 100, 3);
        let img = Image::grid(&h).unwrap();
        let mut bands = vec![Subband::default(); 10];
        set_subband_geometry(&h, &img.tiles[0], 3, &mut bands);
        // Plane 100x50 → level widths 50/25/13 (low) and 50/25/12 (high).
        let dims: Vec<(usize, usize)> = bands.iter().map(|b| (b.width, b.height)).collect();
        assert_eq!(dims[0], (13, 7));
        assert_eq!(dims[1], (12, 7)); // HL coarsest
        assert_eq!(dims[2], (13, 6)); // LH
        assert_eq!(dims[3], (12, 6)); // HH
        assert_eq!(dims[7], (50, 25));
        assert_eq!(dims[9], (50, 25));
    }

    #[test]
    fn overlap_coefficients() {
        let h = header(400, 100, 200, 100, 3);
        let img = Image::grid(&h).unwrap();
        let mut b0 = vec![Subband::default(); 10];
        set_subband_geometry(&h, &img.tiles[0], 3, &mut b0);
        let mut b1 = vec![Subband::default(); 10];
        set_subband_geometry(&h, &img.tiles[1], 3, &mut b1);
        // Left tile (right neighbour): bands extend to the right; right tile: high
        // bands carry one leading coefficient.
        assert!(b0[9].width > 50);
        assert_eq!(b1[9].width, 51);
        assert_eq!(b1[8].width, 50);
    }

    #[test]
    fn qsteps_levels() {
        let map: Vec<i32> = (0..4 * 6).map(|i| 30 + (i % 7)).collect();
        let q = make_qsteps(&map, 4, 6, 12, 3).unwrap();
        assert_eq!(q.len(), 3);
        assert_eq!((q[0].width, q[0].height), (4, 2));
        assert_eq!((q[1].width, q[1].height), (4, 3));
        assert_eq!((q[2].width, q[2].height), (4, 6));
        assert_eq!(q[2].table[0], q_step(30));
        assert!(make_qsteps(&map, 4, 6, 12, 0).is_err());
    }
}
