//! Canon CR2 lossless-JPEG payload with vertical slicing.
//!
//! CR2 bodies encode the sensor as a single lossless-JPEG stream but feed the
//! encoder in vertical strips ("slices"): the decoded sample sequence fills slice 0
//! top to bottom (each slice row is `slice_width` samples), then slice 1, and so on;
//! the last slice may have a different width. The slice layout is stored in TIFF
//! tag `0xC640` as `[count, width, last_width]` where `count` counts the full-width
//! slices preceding the last one. Without the tag (or with `count == 0`) the samples
//! simply fill the image row by row.
//!
//! Large frames are entropy-decoded on all threads ([`ljpeg::decode_frame_parallel`])
//! into a frame buffer whose rows are then copied to their slice positions in
//! parallel. Otherwise the decoder streams JPEG rows straight into the output image,
//! splitting every row into runs copied to their final position.

use crate::ljpeg::{self, LjpegHeader};
use flashraw_core::{Error, Result};

/// Slice layout from tag 0xC640.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Slices {
    pub count: u32,
    pub width: u32,
    pub last_width: u32,
}

impl Slices {
    pub fn from_tag(v: &[u32]) -> Slices {
        Slices {
            count: v.first().copied().unwrap_or(0),
            width: v.get(1).copied().unwrap_or(0),
            last_width: v.get(2).copied().unwrap_or(0),
        }
    }
    pub fn is_sliced(&self) -> bool {
        self.count != 0
    }
}

/// Where the decoded sample stream goes.
struct Placement {
    raw_width: usize,
    raw_height: usize,
    /// Slice widths in order (a single slice of `raw_width` when unsliced).
    widths: Vec<usize>,
    /// First column of each slice.
    starts: Vec<usize>,
    slice: usize,
    row: usize,
    col: usize,
}

impl Placement {
    fn new(raw_width: usize, raw_height: usize, s: Slices) -> Result<Placement> {
        let (widths, starts) = if s.is_sliced() {
            if s.width == 0 || s.last_width == 0 {
                return Err(Error::corrupt("CR2: zero slice width"));
            }
            if s.count > 64 * 1024 {
                return Err(Error::corrupt("CR2: implausible slice count"));
            }
            let mut w = vec![s.width as usize; s.count as usize];
            w.push(s.last_width as usize);
            let starts = (0..w.len()).map(|i| i * s.width as usize).collect();
            (w, starts)
        } else {
            (vec![raw_width], vec![0])
        };
        Ok(Placement { raw_width, raw_height, widths, starts, slice: 0, row: 0, col: 0 })
    }

    /// Copy `src` (consecutive stream samples) into `out`.
    #[inline]
    fn put(&mut self, mut src: &[u16], out: &mut [u16]) -> Result<()> {
        while !src.is_empty() {
            // Samples past the end keep following the last slice's geometry below the
            // the bottom.
            if self.row > self.raw_height {
                return Err(Error::corrupt("CR2: more samples than the slice layout holds"));
            }
            let sw = self.widths[self.slice];
            let take = (sw - self.col).min(src.len());
            let x = self.starts[self.slice] + self.col;
            if self.row < self.raw_height && x < self.raw_width {
                let n = take.min(self.raw_width - x);
                let o = self.row * self.raw_width + x;
                out[o..o + n].copy_from_slice(&src[..n]);
            }
            src = &src[take..];
            self.col += take;
            if self.col == sw {
                self.col = 0;
                self.row += 1;
                if self.row == self.raw_height && self.slice + 1 < self.widths.len() {
                    self.row = 0;
                    self.slice += 1;
                }
            }
        }
        Ok(())
    }
}

/// Decode the frame with [`ljpeg::decode_frame_parallel`] and place it. Returns
/// `Ok(false)` when the stream has to be decoded sequentially.
fn decode_parallel(
    data: &[u8],
    hdr: &LjpegHeader,
    place: &Placement,
    out: &mut [u16],
    chunk: Option<usize>,
) -> Result<bool> {
    let frame_len = hdr.row_len() * hdr.height as usize;
    if place.widths.len() == 1 && frame_len <= out.len() {
        // Unsliced: the sample stream is the image in raster order.
        return crate::ljpeg_par::decode_frame_chunked(data, hdr, hdr.height as usize, &mut out[..frame_len], chunk);
    }
    let mut frame = vec![0u16; frame_len];
    if !crate::ljpeg_par::decode_frame_chunked(data, hdr, hdr.height as usize, &mut frame, chunk)? {
        return Ok(false);
    }
    let (w, h) = (place.raw_width, place.raw_height);
    let workers = flashraw_core::par::memory_bound_workers();
    flashraw_core::par::for_each_row_chunk_bounded(out, w, workers, true, |y0, rows| {
        for (dy, row) in rows.chunks_exact_mut(w).enumerate() {
            let y = y0 + dy;
            let mut base = 0usize;
            for (&sw, &x0) in place.widths.iter().zip(&place.starts) {
                let src = base + y * sw;
                base += sw * h;
                if x0 >= w || src >= frame.len() {
                    continue;
                }
                let n = sw.min(w - x0).min(frame.len() - src);
                row[x0..x0 + n].copy_from_slice(&frame[src..src + n]);
            }
        }
    });
    Ok(true)
}

/// Decode a CR2 lossless-JPEG stream into `out` (`raw_width * raw_height` samples).
///
/// `shift_left_2` reproduces the EOS-1D Mark III quirk where the stream is offset by
/// two samples (every sample lands two positions earlier in raster order).
#[allow(clippy::too_many_arguments)]
pub fn decode(
    data: &[u8],
    hdr: &LjpegHeader,
    raw_width: usize,
    raw_height: usize,
    slices: Slices,
    shift_left_2: bool,
    parallel: bool,
    out: &mut [u16],
) -> Result<()> {
    let total = raw_width.checked_mul(raw_height).ok_or_else(|| Error::corrupt("CR2: size overflow"))?;
    if out.len() < total || total == 0 {
        return Err(Error::corrupt("CR2: output buffer too small"));
    }
    let out = &mut out[..total];
    let mut place = Placement::new(raw_width, raw_height, slices)?;
    if parallel && total >= crate::ljpeg_par::PARALLEL_MIN_SAMPLES && decode_parallel(data, hdr, &place, out, None)? {
        if shift_left_2 && total > 2 {
            out.copy_within(2.., 0);
            out[total - 2..].fill(0);
        }
        return Ok(());
    }
    let mut overflow = false;
    ljpeg::decode_rows(data, hdr, |_, row| {
        if overflow {
            return Ok(());
        }
        if place.put(row, out).is_err() {
            // Extra rows beyond the layout are ignored as long as they would land
            overflow = true;
        }
        Ok(())
    })?;
    if shift_left_2 && total > 2 {
        out.copy_within(2.., 0);
        out[total - 2..].fill(0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ljpeg::encoder::encode;

    fn reference(stream: &[u16], w: usize, h: usize, s: Slices) -> Vec<u16> {
        let mut out = vec![0u16; w * h];
        let (mut row, mut col) = (0usize, 0usize);
        for (jidx, &v) in stream.iter().enumerate() {
            if s.is_sliced() {
                let per = s.width as usize * h;
                let mut i = jidx / per;
                let last = i >= s.count as usize;
                if last {
                    i = s.count as usize;
                }
                let sw = if last { s.last_width } else { s.width } as usize;
                let k = jidx - i * per;
                row = k / sw;
                col = k % sw + i * s.width as usize;
            }
            if row < h && col < w {
                out[row * w + col] = v;
            }
            col += 1;
            if col >= w {
                col = 0;
                row += 1;
            }
        }
        out
    }

    fn pseudo(n: usize) -> Vec<u16> {
        let mut x = 12345u32;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                ((x >> 8) & 0x3fff) as u16
            })
            .collect()
    }

    #[test]
    fn slices_match_reference_formula() {
        // 14 x 6 image, JPEG frame of 2 components x 7 wide x 6 high; slices 2 x 5 + 4.
        let (w, h) = (14usize, 6usize);
        let samples = pseudo(w * h);
        let jpg = encode(7, 6, 2, 14, 1, &samples);
        let hdr = LjpegHeader::parse(&jpg, Default::default()).unwrap();
        for s in [
            Slices { count: 2, width: 5, last_width: 4 },
            Slices { count: 1, width: 7, last_width: 7 },
            Slices { count: 0, width: 0, last_width: 14 },
        ] {
            let mut out = vec![0u16; w * h];
            decode(&jpg, &hdr, w, h, s, false, false, &mut out).unwrap();
            let place = Placement::new(w, h, s).unwrap();
            let mut par = vec![0u16; w * h];
            assert!(decode_parallel(&jpg, &hdr, &place, &mut par, Some(9)).unwrap());
            assert_eq!(par, out, "parallel {s:?}");
            assert_eq!(out, reference(&samples, w, h, s), "{s:?}");
        }
    }

    #[test]
    fn shift_quirk() {
        let (w, h) = (8usize, 2usize);
        let samples = pseudo(w * h);
        let jpg = encode(8, 2, 1, 12, 1, &samples);
        let hdr = LjpegHeader::parse(&jpg, Default::default()).unwrap();
        let mut out = vec![0u16; w * h];
        decode(&jpg, &hdr, w, h, Slices::default(), true, false, &mut out).unwrap();
        assert_eq!(&out[..w * h - 2], &samples[2..]);
        assert_eq!(&out[w * h - 2..], &[0, 0]);
    }

    #[test]
    fn bad_slices_rejected() {
        assert!(Placement::new(10, 10, Slices { count: 1, width: 0, last_width: 3 }).is_err());
        let mut p = Placement::new(4, 1, Slices { count: 1, width: 2, last_width: 2 }).unwrap();
        let mut out = vec![0u16; 4];
        p.put(&[1, 2, 3, 4], &mut out).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4]);
        p.put(&[5, 6], &mut out).unwrap(); // one row below the image: dropped
        assert!(p.put(&[7, 8, 9], &mut out).is_err());
        assert_eq!(out, vec![1, 2, 3, 4]);
    }
}
