//! Restart-interval parallel lossless JPEG, as written by Leaf (and Phase One-era
//! Leaf/Mamiya backs) for MOS files.
//!
//! Leaf streams carry a DRI marker whose interval is a whole number of JPEG rows, so
//! each restart interval is an independently decodable band (predictors reset at a
//! restart). The scan is split at `RSTn` markers and the bands decode in parallel
//! straight into the output, which is filled as one linear sample stream (the

use crate::ljpeg::{LjpegHeader, decode_rows};
use flashraw_core::{Error, Result, par};

/// Start offsets (into `data`) of every restart interval of the scan.
pub fn restart_segments(data: &[u8], hdr: &LjpegHeader) -> Vec<usize> {
    let mut segs = vec![hdr.scan_offset];
    let mut p = hdr.scan_offset;
    while p + 1 < data.len() {
        // Entropy data never contains FF followed by anything but 00 or a marker.
        match data[p..].iter().position(|&b| b == 0xFF) {
            Some(i) => p += i,
            None => break,
        }
        let Some(&m) = data.get(p + 1) else { break };
        if (0xD0..=0xD7).contains(&m) {
            segs.push(p + 2);
            p += 2;
        } else if m == 0xD9 {
            break;
        } else {
            p += 1;
        }
    }
    segs
}

/// Decode a lossless JPEG stream as a linear sample stream into `out` (row-major
/// `out.len()` samples), decoding restart intervals in parallel when they cover whole
/// rows. Samples beyond `out` are dropped; missing samples are left untouched.
pub fn decode_linear(data: &[u8], hdr: &LjpegHeader, out: &mut [u16], parallel: bool) -> Result<()> {
    let rl = hdr.row_len();
    let width = hdr.width as usize;
    let restart = hdr.restart_interval as usize;
    if rl == 0 {
        return Err(Error::corrupt("lossless JPEG: empty rows"));
    }
    let band_rows = if restart > 0 && restart % width == 0 { restart / width } else { 0 };
    let total_rows = hdr.height as usize;
    if band_rows == 0 || band_rows >= total_rows {
        return decode_sequential(data, hdr, out);
    }
    let segs = restart_segments(data, hdr);
    let nbands = total_rows.div_ceil(band_rows);
    if segs.len() < nbands {
        return Err(Error::corrupt(format!("lossless JPEG: {} restart intervals, expected {nbands}", segs.len())));
    }
    let errs = std::sync::Mutex::new(None::<Error>);
    let usable = (out.len() / rl) * rl;
    let (body, tail) = out.split_at_mut(usable);
    par::for_each_row_chunk(body, rl, band_rows, parallel, |first_row, chunk| {
        let band = first_row / band_rows;
        let mut h = hdr.clone();
        h.height = band_rows.min(total_rows - first_row) as u32;
        h.restart_interval = 0;
        h.scan_offset = segs[band];
        let res = decode_rows(data, &h, |r, s| {
            if let Some(dst) = chunk.get_mut(r * rl..(r + 1) * rl) {
                dst.copy_from_slice(s);
            }
            Ok(())
        });
        if let Err(e) = res {
            if let Ok(mut g) = errs.lock() {
                g.get_or_insert(e);
            }
        }
    });
    if let Some(e) = errs.into_inner().ok().flatten() {
        return Err(e);
    }
    // A partial final row (when `out` is not a multiple of the JPEG row length).
    if !tail.is_empty() && usable / rl < total_rows {
        let row = usable / rl;
        let band = row / band_rows;
        let mut h = hdr.clone();
        h.height = (row - band * band_rows + 1) as u32;
        h.restart_interval = 0;
        h.scan_offset = segs[band];
        let target = row - band * band_rows;
        decode_rows(data, &h, |r, s| {
            if r == target {
                let n = tail.len().min(s.len());
                tail[..n].copy_from_slice(&s[..n]);
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn decode_sequential(data: &[u8], hdr: &LjpegHeader, out: &mut [u16]) -> Result<()> {
    let mut pos = 0usize;
    decode_rows(data, hdr, |_, s| {
        if pos < out.len() {
            let n = (out.len() - pos).min(s.len());
            out[pos..pos + n].copy_from_slice(&s[..n]);
            pos += n;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ljpeg::LjpegOptions;

    /// Insert a DRI segment and RST markers into a test-encoded stream by re-encoding
    /// bands separately and splicing them (each band restarts its predictors).
    fn encode_with_restarts(w: usize, h: usize, band: usize, samples: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, rows) in samples.chunks(w * band).enumerate() {
            let bh = rows.len() / w;
            let enc = crate::ljpeg::encoder::encode(w, bh, 1, 14, 1, rows);
            let hdr = LjpegHeader::parse(&enc, LjpegOptions::default()).unwrap();
            if k == 0 {
                // Header with the full height and a DRI marker.
                let mut head = enc[..hdr.scan_offset].to_vec();
                // Patch SOF3 height.
                let sof = head.windows(2).position(|x| x == [0xFF, 0xC3]).unwrap();
                head[sof + 5..sof + 7].copy_from_slice(&(h as u16).to_be_bytes());
                // Insert DRI before SOS.
                let sos = head.windows(2).rposition(|x| x == [0xFF, 0xDA]).unwrap();
                let dri = [0xFF, 0xDD, 0x00, 0x04, ((w * band) >> 8) as u8, (w * band) as u8];
                head.splice(sos..sos, dri);
                out.extend(head);
            } else {
                out.extend([0xFF, 0xD0 + ((k - 1) % 8) as u8]);
            }
            // Entropy data of this band (strip trailing EOI).
            let body = &enc[hdr.scan_offset..enc.len() - 2];
            out.extend(body);
        }
        out.extend([0xFF, 0xD9]);
        out
    }

    #[test]
    fn parallel_bands_match_sequential() {
        let (w, h, band) = (24usize, 37usize, 4usize);
        let samples: Vec<u16> = (0..w * h).map(|i| ((i * 7919) % 16384) as u16).collect();
        let data = encode_with_restarts(w, h, band, &samples);
        let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
        assert_eq!(hdr.restart_interval as usize, w * band);
        for parallel in [false, true] {
            let mut out = vec![0u16; w * h];
            decode_linear(&data, &hdr, &mut out, parallel).unwrap();
            assert_eq!(out, samples);
        }
        // Truncated stream: error, no panic.
        let mut out = vec![0u16; w * h];
        assert!(decode_linear(&data[..data.len() / 2], &hdr, &mut out, true).is_err());
    }
}
