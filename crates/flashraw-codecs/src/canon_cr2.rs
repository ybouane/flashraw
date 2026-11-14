//! Canon CR2 sensor data: a lossless JPEG stream whose samples are stored in vertical strips.
//!
//! See the module specification for the exact placement rules.

use crate::ljpeg::LjpegHeader;
use crate::ljpeg::decode_rows;
use crate::ljpeg_par::decode_frame_chunked;
use flashraw_core::{Error, Result};

/// Strip layout stored in TIFF tag 0xC640: `count` strips of `width` samples, then one of `last_width`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Slices {
    /// Number of full-width strips before the last one (0: not stripped).
    pub count: u32,
    /// Width of each full strip, in samples.
    pub width: u32,
    /// Width of the final strip, in samples.
    pub last_width: u32,
}

impl Slices {
    /// Build the layout from the tag's values (missing values read as 0).
    pub fn from_tag(v: &[u32]) -> Slices {
        let at = |i: usize| v.get(i).copied().unwrap_or(0);
        Slices { count: at(0), width: at(1), last_width: at(2) }
    }

    /// True when the data is stored in strips (`count != 0`).
    pub fn is_sliced(&self) -> bool {
        self.count != 0
    }
}

/// Most strips a layout may declare.
const MAX_STRIPS: u32 = 65536;

/// Where the decoded stream lands in the sensor buffer.
///
/// Strip `i` covers sensor columns starting at `i * pitch` and `strip_width(i)` wide; the stream
/// holds strip 0 first, each strip row by row. An unsliced image is a single strip as wide as the
/// sensor.
struct Layout {
    width: usize,
    height: usize,
    /// Index of the last strip.
    last: usize,
    /// Column distance between strip starts (0 when unsliced).
    pitch: usize,
    /// Width of every strip but the last.
    strip_w: usize,
    /// Width of the last strip.
    last_w: usize,
}

impl Layout {
    fn new(width: usize, height: usize, s: Slices) -> Layout {
        if s.is_sliced() {
            Layout {
                width,
                height,
                last: s.count as usize,
                pitch: s.width as usize,
                strip_w: s.width as usize,
                last_w: s.last_width as usize,
            }
        } else {
            Layout { width, height, last: 0, pitch: 0, strip_w: width, last_w: width }
        }
    }

    fn strip_width(&self, i: usize) -> usize {
        if i == self.last { self.last_w } else { self.strip_w }
    }

    /// Stream index of the first sample of strip `i`.
    fn strip_start(&self, i: usize) -> usize {
        i.saturating_mul(self.strip_w).saturating_mul(self.height)
    }

    /// Place stream samples `src`, which start at stream index `first`, into `out`.
    fn place(&self, first: usize, src: &[u16], out: &mut [u16]) {
        let mut p = first;
        let mut rest = src;
        while !rest.is_empty() {
            let i = if self.last == 0 {
                0
            } else {
                match self.strip_start(1) {
                    0 | usize::MAX => 0,
                    size => (p / size).min(self.last),
                }
            };
            let w = self.strip_width(i);
            let base = self.strip_start(i);
            let in_strip = if i == self.last { rest.len() } else { rest.len().min(self.strip_start(i + 1) - p) };
            let (seg, tail) = rest.split_at(in_strip);
            let x0 = i * self.pitch;
            let mut k = p - base;
            let mut seg = seg;
            while !seg.is_empty() {
                let (row, col) = (k / w, k % w);
                let n = seg.len().min(w - col);
                if row < self.height && x0 + col < self.width {
                    let take = n.min(self.width - (x0 + col));
                    let at = row * self.width + x0 + col;
                    out[at..at + take].copy_from_slice(&seg[..take]);
                }
                seg = &seg[n..];
                k += n;
            }
            p += in_strip;
            rest = tail;
        }
    }

    /// Fill output rows `y0..` (the rows held by `rows`) from a whole decoded stream.
    fn gather(&self, y0: usize, rows: &mut [u16], stream: &[u16]) {
        for (dy, line) in rows.chunks_exact_mut(self.width).enumerate() {
            let y = y0 + dy;
            for i in 0..=self.last {
                let x0 = i * self.pitch;
                if x0 >= self.width {
                    break;
                }
                let w = self.strip_width(i);
                let start = self.strip_start(i).saturating_add(y.saturating_mul(w));
                if start >= stream.len() {
                    continue;
                }
                let n = w.min(self.width - x0).min(stream.len() - start);
                line[x0..x0 + n].copy_from_slice(&stream[start..start + n]);
            }
        }
    }
}

/// Decode a CR2 lossless JPEG stream into `out` (`raw_width * raw_height` samples).
///
/// `shift_left_2` applies the two-sample stream offset of one camera family;
/// `parallel` allows multi-threaded decoding.
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
    let total = raw_width.checked_mul(raw_height).ok_or_else(|| Error::corrupt("CR2: image size overflows"))?;
    if total == 0 || out.len() < total {
        return Err(Error::corrupt("CR2: output buffer too small"));
    }
    if slices.is_sliced() && (slices.width == 0 || slices.last_width == 0 || slices.count > MAX_STRIPS) {
        return Err(Error::corrupt("CR2: invalid slice layout"));
    }
    let out = &mut out[..total];
    let layout = Layout::new(raw_width, raw_height, slices);
    let row_len = hdr.row_len();
    let frame_len = row_len.saturating_mul(hdr.height as usize);

    let done = parallel
        && frame_len >= crate::ljpeg_par::LJPEG_PARALLEL_MIN_SAMPLES
        && flashraw_core::par::threads() > 1
        && decode_parallel(data, hdr, &layout, frame_len, out)?;
    if !done {
        decode_sequential(data, hdr, &layout, row_len, out)?;
    }

    if shift_left_2 && total > 2 {
        out.copy_within(2.., 0);
        out[total - 2..].fill(0);
    }
    Ok(())
}

fn decode_sequential(data: &[u8], hdr: &LjpegHeader, layout: &Layout, row_len: usize, out: &mut [u16]) -> Result<()> {
    let direct = layout.last == 0;
    decode_rows(data, hdr, |r, row| {
        let first = r * row_len;
        if direct {
            // One strip as wide as the image: stream index == buffer index.
            if first < out.len() {
                let n = row.len().min(out.len() - first);
                out[first..first + n].copy_from_slice(&row[..n]);
            }
        } else {
            layout.place(first, row, out);
        }
        Ok(())
    })
}

/// Multi-threaded path; `Ok(false)` when the frame decoder declines.
fn decode_parallel(data: &[u8], hdr: &LjpegHeader, layout: &Layout, frame_len: usize, out: &mut [u16]) -> Result<bool> {
    let height = hdr.height as usize;
    if layout.last == 0 && frame_len <= out.len() {
        return decode_frame_chunked(data, hdr, height, &mut out[..frame_len], None);
    }
    let mut frame = vec![0u16; frame_len];
    if !decode_frame_chunked(data, hdr, height, &mut frame, None)? {
        return Ok(false);
    }
    let workers = flashraw_core::par::memory_bound_workers();
    flashraw_core::par::for_each_row_chunk_bounded(out, layout.width, workers, true, |y0, rows| {
        layout.gather(y0, rows, &frame)
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ljpeg::encoder::encode;

    /// Per-sample model of the stream-to-sensor mapping.
    fn model(stream: &[u16], w: usize, h: usize, s: Slices, shift: bool, outlen: usize) -> Vec<u16> {
        let mut out = vec![0u16; outlen];
        let (mut strip, mut start) = (0usize, 0usize);
        let width_of = |i: usize| {
            if !s.is_sliced() {
                w
            } else if i == s.count as usize {
                s.last_width as usize
            } else {
                s.width as usize
            }
        };
        for (p, &v) in stream.iter().enumerate() {
            while strip < s.count as usize && s.is_sliced() && p >= start + width_of(strip) * h {
                start += width_of(strip) * h;
                strip += 1;
            }
            let sw = width_of(strip);
            let k = p - start;
            let (row, x) = (k / sw, strip * s.width as usize * s.is_sliced() as usize + k % sw);
            if row < h && x < w {
                out[row * w + x] = v;
            }
        }
        let total = w * h;
        if shift && total > 2 {
            out.copy_within(2..total, 0);
            out[total - 2..total].fill(0);
        }
        out
    }

    fn run(w: usize, h: usize, s: Slices, shift: bool, jw: usize, jh: usize) {
        let samples: Vec<u16> = (0..jw * 2 * jh).map(|i| ((i * 2654435761usize) >> 7 & 0x3fff) as u16).collect();
        let jpg = encode(jw, jh, 2, 14, 1, &samples);
        let hdr = LjpegHeader::parse(&jpg, Default::default()).unwrap();
        let mut out = vec![0u16; w * h];
        decode(&jpg, &hdr, w, h, s, shift, false, &mut out).unwrap();
        assert_eq!(out, model(&samples, w, h, s, shift, w * h), "{s:?} {w}x{h}");
    }

    #[test]
    fn placement_matches_model() {
        let sl = |count, width, last_width| Slices { count, width, last_width };
        for s in [sl(0, 0, 0), sl(1, 6, 4), sl(2, 3, 4), sl(2, 5, 5), sl(1, 4, 9), sl(3, 2, 2)] {
            for (w, h) in [(10, 6), (9, 7), (12, 3)] {
                run(w, h, s, false, 6, 7);
                run(w, h, s, true, 5, 4);
            }
        }
    }

    #[test]
    fn invalid_layouts() {
        let samples = vec![1u16; 8];
        let jpg = encode(2, 2, 2, 14, 1, &samples);
        let hdr = LjpegHeader::parse(&jpg, Default::default()).unwrap();
        let mut out = vec![0u16; 16];
        let sl = |count, width, last_width| Slices { count, width, last_width };
        assert!(decode(&jpg, &hdr, 4, 4, sl(1, 0, 4), false, false, &mut out).is_err());
        assert!(decode(&jpg, &hdr, 4, 4, sl(1, 2, 0), false, false, &mut out).is_err());
        assert!(decode(&jpg, &hdr, 4, 4, sl(70000, 1, 1), false, false, &mut out).is_err());
        assert!(decode(&jpg, &hdr, 4, 5, sl(0, 0, 0), false, false, &mut out).is_err());
        assert!(decode(&jpg, &hdr, 0, 4, sl(0, 0, 0), false, false, &mut out).is_err());
        assert_eq!(Slices::from_tag(&[2, 5]), sl(2, 5, 0));
    }
}
