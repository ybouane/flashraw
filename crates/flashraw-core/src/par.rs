//! Parallel execution helpers that degrade to sequential execution when the
//! `rayon` feature is disabled (e.g. single-threaded WASM) or parallelism is off.

/// Minimum number of samples before splitting work across threads pays off.
pub const PAR_MIN_SAMPLES: usize = 1 << 18;

/// Run `f(first_row, rows_chunk)` over `data` split into chunks of `rows_per_chunk`
/// rows of `row_len` elements each.
pub fn for_each_row_chunk<T, F>(data: &mut [T], row_len: usize, rows_per_chunk: usize, parallel: bool, f: F)
where
    T: Send,
    F: Fn(usize, &mut [T]) + Sync + Send,
{
    if row_len == 0 || data.is_empty() {
        return;
    }
    let chunk = row_len * rows_per_chunk.max(1);
    #[cfg(feature = "rayon")]
    if parallel && data.len() >= PAR_MIN_SAMPLES && rayon::current_num_threads() > 1 {
        use rayon::prelude::*;
        data.par_chunks_mut(chunk).enumerate().for_each(|(i, c)| f(i * rows_per_chunk.max(1), c));
        return;
    }
    let _ = parallel;
    for (i, c) in data.chunks_mut(chunk).enumerate() {
        f(i * rows_per_chunk.max(1), c);
    }
}

/// Workers worth using for purely memory-bound kernels (copies, byte swaps, 16-bit
/// unpack). Measured: such loops saturate memory bandwidth at ~4 threads and slow down
/// beyond that from contention, so callers cap their parallelism with this.
pub const MEMORY_BOUND_WORKERS: usize = 4;

/// Like [`for_each_row_chunk`] but split into at most `max_workers` tasks (used for
/// memory-bound kernels where more threads only add contention).
pub fn for_each_row_chunk_bounded<T, F>(data: &mut [T], row_len: usize, max_workers: usize, parallel: bool, f: F)
where
    T: Send,
    F: Fn(usize, &mut [T]) + Sync + Send,
{
    if row_len == 0 || data.is_empty() {
        return;
    }
    let rows = data.len().div_ceil(row_len);
    let workers = max_workers.clamp(1, threads().max(1));
    for_each_row_chunk(data, row_len, rows.div_ceil(workers), parallel && workers > 1, f)
}

/// Map `f` over `items` in parallel (when enabled), preserving order.
pub fn map<I, O, F>(items: Vec<I>, parallel: bool, f: F) -> Vec<O>
where
    I: Send,
    O: Send,
    F: Fn(I) -> O + Sync + Send,
{
    #[cfg(feature = "rayon")]
    if parallel && items.len() > 1 && rayon::current_num_threads() > 1 {
        use rayon::prelude::*;
        return items.into_par_iter().map(f).collect();
    }
    let _ = parallel;
    items.into_iter().map(f).collect()
}

/// Run `f(i)` for `i in 0..n` in parallel when enabled.
pub fn for_each_index<F>(n: usize, parallel: bool, f: F)
where
    F: Fn(usize) + Sync + Send,
{
    #[cfg(feature = "rayon")]
    if parallel && n > 1 && rayon::current_num_threads() > 1 {
        use rayon::prelude::*;
        (0..n).into_par_iter().for_each(f);
        return;
    }
    let _ = parallel;
    (0..n).for_each(f);
}

/// Number of worker threads available to parallel helpers.
pub fn threads() -> usize {
    #[cfg(feature = "rayon")]
    {
        rayon::current_num_threads()
    }
    #[cfg(not(feature = "rayon"))]
    {
        1
    }
}

/// Split a mutable slice into disjoint sub-slices at the given ascending offsets.
/// `bounds` are `(start, end)` pairs that must not overlap and be sorted. Returns
/// `None` if they are not valid for `data`.
pub fn split_disjoint<'a, T>(mut data: &'a mut [T], bounds: &[(usize, usize)]) -> Option<Vec<&'a mut [T]>> {
    let mut out = Vec::with_capacity(bounds.len());
    let mut consumed = 0usize;
    for &(s, e) in bounds {
        if s < consumed || e < s {
            return None;
        }
        let rest = std::mem::take(&mut data);
        if e - consumed > rest.len() {
            return None;
        }
        let (_, tail) = rest.split_at_mut(s - consumed);
        let (mid, tail) = tail.split_at_mut(e - s);
        out.push(mid);
        data = tail;
        consumed = e;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_chunks_cover_everything() {
        let mut v = vec![0u32; 1 << 19];
        for_each_row_chunk(&mut v, 1000, 7, true, |first, c| {
            for (i, x) in c.iter_mut().enumerate() {
                *x = (first * 1000 + i) as u32;
            }
        });
        assert!(v.iter().enumerate().all(|(i, &x)| x == i as u32));
    }

    #[test]
    fn disjoint_split() {
        let mut v: Vec<u8> = (0..10).collect();
        let parts = split_disjoint(&mut v, &[(1, 3), (5, 6), (6, 10)]).unwrap();
        assert_eq!(parts[0], &[1, 2]);
        assert_eq!(parts[1], &[5]);
        assert_eq!(parts[2], &[6, 7, 8, 9]);
        assert!(split_disjoint(&mut v, &[(3, 5), (4, 6)]).is_none());
        assert!(split_disjoint(&mut v, &[(3, 50)]).is_none());
    }
}

/// Concurrent writer for a buffer partitioned into a regular tile grid.
///
/// Tiles of a grid share buffer rows, so `chunks_mut` cannot hand them out to
/// threads. `GridWriter` lets each thread [`claim`](GridWriter::claim) a tile and
/// write its rows directly into the final image. Soundness rests on two checked
/// invariants: (1) every claim is recorded in an atomic flag, so a tile can be
/// claimed at most once; (2) grid tiles cover pairwise-disjoint rectangles, and
/// every row slice handed out is clipped to its tile, so no two claims alias.
///
/// The buffer may cover a sub-rectangle (`origin`, `width x height`) of the grid's
/// coordinate space, which is how region decoding writes only intersecting tiles.
pub struct GridWriter<'a, T> {
    ptr: *mut T,
    len: usize,
    width: u32,
    height: u32,
    cpp: usize,
    origin: (u32, u32),
    tile_w: u32,
    tile_h: u32,
    tiles_x: u32,
    tiles_y: u32,
    claimed: Vec<std::sync::atomic::AtomicBool>,
    _marker: std::marker::PhantomData<&'a mut [T]>,
}

// SAFETY: access to the underlying buffer is partitioned by claims (see type docs);
// `T: Send` values may be written from any thread.
unsafe impl<T: Send> Send for GridWriter<'_, T> {}
// SAFETY: shared access only hands out disjoint, exclusively-claimed tiles.
unsafe impl<T: Send> Sync for GridWriter<'_, T> {}

/// Exclusive access to one claimed tile's area within the buffer.
pub struct TileMut<'w, 'a, T> {
    writer: &'w GridWriter<'a, T>,
    /// Clipped area in buffer coordinates.
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    /// Offset of the clipped area relative to the tile's own origin.
    pub skip_x: u32,
    pub skip_y: u32,
}

impl<'a, T: Send> GridWriter<'a, T> {
    /// `buf` holds `width x height` pixels of `cpp` samples, covering grid coordinates
    /// starting at `origin`. The grid has `tile_w x tile_h` tiles anchored at (0, 0) and
    /// spans `grid_w x grid_h` pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        buf: &'a mut [T],
        width: u32,
        height: u32,
        cpp: u32,
        origin: (u32, u32),
        tile_w: u32,
        tile_h: u32,
        grid_w: u32,
        grid_h: u32,
    ) -> Option<Self> {
        if tile_w == 0 || tile_h == 0 || buf.len() < width as usize * height as usize * cpp as usize {
            return None;
        }
        let tiles_x = grid_w.div_ceil(tile_w);
        let tiles_y = grid_h.div_ceil(tile_h);
        let n = tiles_x as usize * tiles_y as usize;
        Some(GridWriter {
            ptr: buf.as_mut_ptr(),
            len: buf.len(),
            width,
            height,
            cpp: cpp as usize,
            origin,
            tile_w,
            tile_h,
            tiles_x,
            tiles_y,
            claimed: (0..n).map(|_| std::sync::atomic::AtomicBool::new(false)).collect(),
            _marker: std::marker::PhantomData,
        })
    }

    pub fn tiles_x(&self) -> u32 {
        self.tiles_x
    }
    pub fn tiles_y(&self) -> u32 {
        self.tiles_y
    }

    /// Claim tile `(tx, ty)`. Returns `None` if out of range, already claimed, or if
    /// the tile does not intersect the buffer area.
    pub fn claim(&self, tx: u32, ty: u32) -> Option<TileMut<'_, 'a, T>> {
        if tx >= self.tiles_x || ty >= self.tiles_y {
            return None;
        }
        let idx = ty as usize * self.tiles_x as usize + tx as usize;
        if self.claimed[idx].swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // Tile rect in grid coordinates, intersected with the buffer area.
        let (gx, gy) = (tx as u64 * self.tile_w as u64, ty as u64 * self.tile_h as u64);
        let (ox, oy) = (self.origin.0 as u64, self.origin.1 as u64);
        let l = gx.max(ox);
        let t = gy.max(oy);
        let r = (gx + self.tile_w as u64).min(ox + self.width as u64);
        let b = (gy + self.tile_h as u64).min(oy + self.height as u64);
        if r <= l || b <= t {
            return None;
        }
        Some(TileMut {
            writer: self,
            x0: (l - ox) as usize,
            y0: (t - oy) as usize,
            w: (r - l) as usize,
            h: (b - t) as usize,
            skip_x: (l - gx) as u32,
            skip_y: (t - gy) as u32,
        })
    }
}

impl<T> TileMut<'_, '_, T> {
    /// Width (pixels) and height of the clipped area.
    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    /// Mutable samples of row `y` of the clipped area (`w * cpp` samples).
    pub fn row(&mut self, y: usize) -> &mut [T] {
        assert!(y < self.h, "tile row out of range");
        let wr = self.writer;
        let start = ((self.y0 + y) * wr.width as usize + self.x0) * wr.cpp;
        let n = self.w * wr.cpp;
        assert!(start + n <= wr.len);
        // SAFETY: `start..start+n` lies within the buffer (asserted) and within this
        // tile's clipped rectangle, which is disjoint from every other tile's; the tile
        // was claimed exactly once (atomic flag) and `&mut self` prevents aliasing
        // between rows handed out by this same TileMut.
        unsafe { std::slice::from_raw_parts_mut(wr.ptr.add(start), n) }
    }
}

#[cfg(test)]
mod grid_tests {
    use super::*;

    #[test]
    fn grid_writes_and_single_claim() {
        let mut buf = vec![0u32; 10 * 7];
        {
            let g = GridWriter::new(&mut buf, 10, 7, 1, (0, 0), 4, 3, 10, 7).unwrap();
            assert_eq!((g.tiles_x(), g.tiles_y()), (3, 3));
            for_each_index(9, true, |i| {
                let (tx, ty) = (i as u32 % 3, i as u32 / 3);
                let mut t = g.claim(tx, ty).unwrap();
                let (_, h) = t.size();
                for y in 0..h {
                    for v in t.row(y).iter_mut() {
                        *v = i as u32 + 1;
                    }
                }
            });
            assert!(g.claim(0, 0).is_none());
        }
        for y in 0..7 {
            for x in 0..10 {
                assert_eq!(buf[y * 10 + x], (y / 3 * 3 + x / 4) as u32 + 1);
            }
        }
    }

    #[test]
    fn region_buffer() {
        // Buffer covers grid rect (3,2) 5x4; tile 4x4.
        let mut buf = vec![0u8; 5 * 4];
        let g = GridWriter::new(&mut buf, 5, 4, 1, (3, 2), 4, 4, 16, 16).unwrap();
        let t = g.claim(0, 0).unwrap();
        assert_eq!((t.x0, t.y0, t.size(), t.skip_x, t.skip_y), (0, 0, (1, 2), 3, 2));
        let t = g.claim(1, 1).unwrap();
        assert_eq!((t.x0, t.y0, t.size(), t.skip_x, t.skip_y), (1, 2, (4, 2), 0, 0));
        assert!(g.claim(3, 3).is_none());
    }
}
