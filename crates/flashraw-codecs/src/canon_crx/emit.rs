//! Assembly of the decoded units into the interleaved CFA buffer.

use super::header::CrxHeader;
use super::layout::Layout;
use flashraw_core::par;

/// Combine the four decorrelated planes of one position (encoding type 3) into R, G1, G2, B.
#[inline]
pub(super) fn recorrelate(p: [u16; 4], median_bits: u8) -> [u16; 4] {
    let [p0, p1, p2, p3] = p.map(|v| v as i16 as i32);
    let mb = median_bits as u32;
    let max = ((1i64 << mb) - 1) as i32;
    let base = ((1i32 << (mb - 1)) << 10) + (p0 << 10);
    let g = base - 168 * p1 - 585 * p3;
    let g = if g < 0 { -(((-g + 512) >> 9) & !1) } else { ((g + 512) >> 9) & !1 };
    let r = (base + 1510 * p3 + 512) >> 10;
    let g1 = (p2 + g + 1) >> 1;
    let g2 = (g - p2 + 1) >> 1;
    let b = (base + 1927 * p1 + 512) >> 10;
    [r, g1, g2, b].map(|v| v.clamp(0, max) as u16)
}

/// Write the units (indexed `tile * planes + plane`) into `out`.
pub(super) fn interleave(h: &CrxHeader, layout: &Layout, units: &[Vec<u16>], out: &mut [u16], parallel: bool) {
    let width = h.width as usize;
    let height = h.height as usize;
    let out = &mut out[..width * height];
    if layout.plane_count == 1 {
        let tiles = &layout.tiles;
        let tile_h = h.tile_height as usize;
        let cols = tiles.iter().take_while(|t| t.y == 0).count();
        par::for_each_row_chunk(out, width, 64, parallel, |first, chunk| {
            for (i, row) in chunk.chunks_exact_mut(width).enumerate() {
                let y = first + i;
                let tr = y / tile_h;
                for (tc, t) in tiles[tr * cols..(tr + 1) * cols].iter().enumerate() {
                    let src = &units[tr * cols + tc];
                    let ry = y - t.y;
                    row[t.x..t.x + t.width].copy_from_slice(&src[ry * t.width..(ry + 1) * t.width]);
                }
            }
        });
        return;
    }

    // cell[dy][dx] = plane stored at that position of the 2x2 cell
    let mut cell = [[0usize; 2]; 2];
    for p in 0..4 {
        let (dx, dy) = h.plane_position(p);
        cell[dy][dx] = p;
    }
    let decorrelated = h.enc_type == 3;
    let median_bits = h.median_bits;
    let tile_h = h.tile_height as usize / 2;
    let tiles = &layout.tiles;
    let cols = tiles.iter().take_while(|t| t.y == 0).count();
    par::for_each_row_chunk(out, width, 64, parallel, |first, chunk| {
        for (i, pair) in chunk.chunks_exact_mut(2 * width).enumerate() {
            let py = first / 2 + i;
            let tr = py / tile_h;
            let (row_a, row_b) = pair.split_at_mut(width);
            for tc in 0..cols {
                let ti = tr * cols + tc;
                let t = &tiles[ti];
                let ry = py - tr * tile_h;
                let planes: [&[u16]; 4] = std::array::from_fn(|p| &units[ti * 4 + p][ry * t.width..(ry + 1) * t.width]);
                let a = &mut row_a[2 * t.x..2 * (t.x + t.width)];
                let b = &mut row_b[2 * t.x..2 * (t.x + t.width)];
                if decorrelated {
                    for (x, (da, db)) in a.chunks_exact_mut(2).zip(b.chunks_exact_mut(2)).enumerate() {
                        let v = recorrelate([planes[0][x], planes[1][x], planes[2][x], planes[3][x]], median_bits);
                        da[0] = v[cell[0][0]];
                        da[1] = v[cell[0][1]];
                        db[0] = v[cell[1][0]];
                        db[1] = v[cell[1][1]];
                    }
                } else {
                    let (p00, p01, p10, p11) =
                        (planes[cell[0][0]], planes[cell[0][1]], planes[cell[1][0]], planes[cell[1][1]]);
                    for (x, (da, db)) in a.chunks_exact_mut(2).zip(b.chunks_exact_mut(2)).enumerate() {
                        da[0] = p00[x];
                        da[1] = p01[x];
                        db[0] = p10[x];
                        db[1] = p11[x];
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::recorrelate;

    #[test]
    fn neutral_planes_give_mid_grey() {
        assert_eq!(recorrelate([0, 0, 0, 0], 14), [8192; 4]);
        assert_eq!(recorrelate([0, 0, 0, 0], 10), [512; 4]);
    }

    #[test]
    fn results_are_clamped_to_the_precision() {
        let v = recorrelate([0x7fff, 0, 0, 0], 14);
        assert_eq!(v[0], 16383);
        let v = recorrelate([(-0x7fff_i16) as u16, 0, 0, 0], 14);
        assert_eq!(v[0], 0);
    }

    #[test]
    fn luma_offsets_all_channels_equally() {
        // a positive luma value brightens R and B by the same amount
        let v = recorrelate([100, 0, 0, 0], 14);
        assert_eq!(v[0], 8292);
        assert_eq!(v[3], 8292);
    }
}
