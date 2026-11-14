//! Lossless JPEG streams with restart intervals, as stored by Leaf backs in MOS files.
//!
//! The stream is one lossless JPEG whose restart interval is a whole number of rows.
//! Every interval is entropy-coded independently (the predictors start afresh and the
//! data is byte aligned after a restart marker), so the intervals, called bands here,
//! can be decoded on their own and concurrently. The decoded values are laid out as one
//! linear sample stream that the caller maps onto its own rows.

use crate::ljpeg::{self, LjpegHeader};
use flashraw_core::{Error, Result, par};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Index of the next 0xFF byte at or after `from`, if any.
fn find_ff(data: &[u8], from: usize) -> Option<usize> {
    const BLOCK: usize = 32;
    let mut pos = from;
    while pos + BLOCK <= data.len() {
        let block = &data[pos..pos + BLOCK];
        // A plain fold over a fixed-size block compiles to a vector compare.
        if block.iter().fold(false, |any, &b| any | (b == 0xFF)) {
            break;
        }
        pos += BLOCK;
    }
    data.get(pos..)?.iter().position(|&b| b == 0xFF).map(|i| pos + i)
}

/// Byte offsets (into `data`) at which each restart interval of the scan begins: the
/// scan start, then the byte after every restart marker found before the end of image.
pub fn restart_segments(data: &[u8], hdr: &LjpegHeader) -> Vec<usize> {
    let mut starts = vec![hdr.scan_offset];
    let mut pos = hdr.scan_offset;
    while let Some(ff) = find_ff(data, pos) {
        let Some(&code) = data.get(ff + 1) else { break };
        match code {
            0xD0..=0xD7 => {
                starts.push(ff + 2);
                pos = ff + 2;
            }
            0xD9 => break,
            // Stuffing, fill or another marker: the byte after the 0xFF may itself start a marker.
            _ => pos = ff + 1,
        }
    }
    starts
}

/// Decode the lossless JPEG `data` (header `hdr`, parsed from the same bytes) into `out`
/// as a row-major run of samples. Surplus samples are dropped, samples the stream does
/// not provide are left as they were. `parallel` allows decoding intervals on several
/// threads; the result is the same either way.
pub fn decode_linear(data: &[u8], hdr: &LjpegHeader, out: &mut [u16], parallel: bool) -> Result<()> {
    let row_len = hdr.row_len();
    if row_len == 0 {
        return Err(Error::corrupt("lossless JPEG: empty rows"));
    }
    let rows = hdr.height as usize;
    let interval = hdr.restart_interval as usize;
    let width = hdr.width as usize;
    if interval > 0 && interval % width == 0 && interval / width < rows {
        decode_bands(data, hdr, interval / width, out, parallel)
    } else {
        decode_sequential(data, hdr, out)
    }
}

/// Copy as much of `row` as fits at `dst[offset..]`.
#[inline]
fn store_row(dst: &mut [u16], offset: usize, row: &[u16]) {
    if let Some(room) = dst.get_mut(offset..) {
        let n = room.len().min(row.len());
        room[..n].copy_from_slice(&row[..n]);
    }
}

/// One pass over the whole frame; every row is decoded even when `out` is full.
fn decode_sequential(data: &[u8], hdr: &LjpegHeader, out: &mut [u16]) -> Result<()> {
    let row_len = hdr.row_len();
    ljpeg::decode_rows(data, hdr, |y, row| {
        store_row(out, y * row_len, row);
        Ok(())
    })
}

/// The failure of the lowest-numbered band.
struct FirstError {
    band: AtomicUsize,
    err: Mutex<Option<(usize, Error)>>,
}

impl FirstError {
    fn new() -> Self {
        FirstError { band: AtomicUsize::new(usize::MAX), err: Mutex::new(None) }
    }
    /// True when a band below `band` has failed already (so `band`'s result cannot matter).
    fn precedes(&self, band: usize) -> bool {
        self.band.load(Ordering::Relaxed) < band
    }
    fn record(&self, band: usize, e: Error) {
        let mut slot = self.err.lock().unwrap_or_else(|p| p.into_inner());
        if slot.as_ref().is_none_or(|(b, _)| band < *b) {
            self.band.store(band, Ordering::Relaxed);
            *slot = Some((band, e));
        }
    }
    fn into_result(self) -> Result<()> {
        match self.err.into_inner().unwrap_or_else(|p| p.into_inner()) {
            Some((_, e)) => Err(e),
            None => Ok(()),
        }
    }
}

/// Frame made of bands of `band_rows` rows, each starting at a restart interval.
fn decode_bands(data: &[u8], hdr: &LjpegHeader, band_rows: usize, out: &mut [u16], parallel: bool) -> Result<()> {
    let row_len = hdr.row_len();
    let rows = hdr.height as usize;
    let band_count = rows.div_ceil(band_rows);
    let starts = restart_segments(data, hdr);
    if starts.len() < band_count {
        return Err(Error::corrupt(format!(
            "lossless JPEG: {} restart intervals, expected {band_count}",
            starts.len()
        )));
    }
    // The part of `out` the stream can fill. Its last row may be a partial one.
    let fill = out.len().min(rows.saturating_mul(row_len));
    let dst = &mut out[..fill];
    let band_samples = band_rows * row_len;
    let bands_used = dst.len().div_ceil(band_samples);

    // Hand workers runs of whole bands, so that each clones the header only once.
    let threaded = parallel && dst.len() >= par::PAR_MIN_SAMPLES && par::threads() > 1;
    let groups = if threaded { bands_used.min(par::threads() * 4) } else { 1 };
    let bands_per_group = bands_used.div_ceil(groups.max(1)).max(1);

    let failure = FirstError::new();
    par::for_each_row_chunk(dst, row_len, band_rows * bands_per_group, threaded, |first_row, run| {
        let mut band_hdr = hdr.clone();
        band_hdr.restart_interval = 0;
        band_hdr.scan_offset = 0;
        for (k, area) in run.chunks_mut(band_samples).enumerate() {
            let band = first_row / band_rows + k;
            if failure.precedes(band) {
                return;
            }
            // A band that holds only the first samples of a final row needs just that row.
            let wanted = if area.len() < row_len { 1 } else { band_rows.min(rows - band * band_rows) };
            band_hdr.height = wanted as u32;
            let end = starts.get(band + 1).map(|&next| next - 2);
            if let Err(e) = decode_band(data, starts[band], end, &band_hdr, area) {
                failure.record(band, e);
                return;
            }
        }
    });
    failure.into_result()
}

/// Decode one band, whose entropy data lies in `data[start..end]` (`end`: up to the next
/// marker, or the end of the data for the last band), into `area`.
fn decode_band(data: &[u8], start: usize, end: Option<usize>, hdr: &LjpegHeader, area: &mut [u16]) -> Result<()> {
    let Some(bytes) = data.get(start..end.unwrap_or(data.len()).max(start)) else {
        return Err(Error::corrupt("lossless JPEG: scan offset past the end of the data"));
    };
    let row_len = hdr.row_len();
    ljpeg::decode_rows(bytes, hdr, |y, row| {
        store_row(area, y * row_len, row);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ljpeg::LjpegOptions;
    use crate::ljpeg::encoder::encode_with_restarts;

    fn samples(n: usize) -> Vec<u16> {
        let mut s = 0x1234_5678u32;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 16) as u16 & 0x0FFF
            })
            .collect()
    }

    fn stream(w: usize, h: usize, comps: usize, band: usize) -> (Vec<u8>, Vec<u16>, LjpegHeader) {
        let src = samples(w * comps * h);
        let data = encode_with_restarts(w, h, comps, 12, 1, &src, w * band);
        let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
        (data, src, hdr)
    }

    #[test]
    fn bands_round_trip_with_and_without_threads() {
        for (w, h, comps, band) in [(37, 20, 2, 8), (16, 9, 1, 4), (5, 7, 3, 1), (2000, 200, 2, 8)] {
            let (data, src, hdr) = stream(w, h, comps, band);
            assert_eq!(restart_segments(&data, &hdr).len(), h.div_ceil(band));
            for parallel in [false, true] {
                let mut out = vec![0u16; src.len()];
                decode_linear(&data, &hdr, &mut out, parallel).unwrap();
                assert_eq!(out, src, "{w}x{h}x{comps} band {band} parallel {parallel}");
            }
        }
    }

    #[test]
    fn short_and_long_destinations() {
        let (data, src, hdr) = stream(13, 11, 2, 3);
        for len in [0, 5, 26, 27, 80, 3 * 26, 3 * 26 + 1, src.len() - 1, src.len()] {
            let mut out = vec![0xABCDu16; len];
            decode_linear(&data, &hdr, &mut out, false).unwrap();
            assert_eq!(out[..], src[..len], "length {len}");
        }
        let mut out = vec![0xABCDu16; src.len() + 40];
        decode_linear(&data, &hdr, &mut out, true).unwrap();
        assert_eq!(out[..src.len()], src[..]);
        assert!(out[src.len()..].iter().all(|&v| v == 0xABCD));
    }

    #[test]
    fn unaligned_interval_decodes_sequentially() {
        let src = samples(8 * 6);
        let data = encode_with_restarts(8, 6, 1, 12, 1, &src, 0);
        let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
        let mut out = vec![0u16; src.len()];
        decode_linear(&data, &hdr, &mut out, true).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn damaged_streams_fail() {
        let (data, src, hdr) = stream(20, 12, 2, 4);
        let mut out = vec![0u16; src.len()];
        for cut in [data.len() / 2, data.len() - 40] {
            let short = &data[..cut];
            assert!(decode_linear(short, &hdr, &mut out, false).is_err());
            assert!(decode_linear(short, &hdr, &mut out, true).is_err());
        }
        // A missing marker leaves too few intervals.
        let mut d = data.clone();
        let at = d.windows(2).position(|p| p[0] == 0xFF && (0xD0..=0xD7).contains(&p[1])).unwrap();
        d.drain(at..at + 2);
        assert!(decode_linear(&d, &hdr, &mut out, false).is_err());
    }

    #[test]
    fn empty_rows_are_rejected() {
        let (data, _, hdr) = stream(4, 4, 1, 2);
        let mut zero = hdr.clone();
        zero.width = 0;
        let err = decode_linear(&data, &zero, &mut [0u16; 4], false).unwrap_err();
        assert!(err.to_string().contains("empty rows"));
    }

    fn header_at(offset: usize) -> LjpegHeader {
        let (_, _, mut hdr) = stream(4, 4, 1, 2);
        hdr.scan_offset = offset;
        hdr
    }

    #[test]
    fn marker_scan_rules() {
        let hdr = header_at(0);
        let segs = |d: &[u8]| restart_segments(d, &hdr);
        assert_eq!(segs(&[1, 2, 3]), [0]);
        assert_eq!(segs(&[1, 0xFF, 0xD0, 5, 0xFF, 0xD7, 9, 0xFF, 0xD9, 0xFF, 0xD1]), [0, 3, 6]);
        // Fill and stuffing before a marker: the marker still counts.
        assert_eq!(segs(&[0xFF, 0xFF, 0xD3, 7]), [0, 3]);
        assert_eq!(segs(&[0xFF, 0x00, 0xFF, 0xD1, 7]), [0, 4]);
        // A lone trailing 0xFF and a non-restart marker are stepped over.
        assert_eq!(segs(&[3, 0xFF]), [0]);
        assert_eq!(segs(&[0xFF, 0xC4, 0xFF, 0xD2]), [0, 4]);
        // Long runs of plain bytes before a marker.
        let mut long = vec![1u8; 100];
        long.extend([0xFF, 0xD4, 0]);
        assert_eq!(segs(&long), [0, 102]);
        assert_eq!(restart_segments(&long, &header_at(1000)), [1000]);
    }
}
