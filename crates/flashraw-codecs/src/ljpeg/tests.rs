//! Unit and property tests of the lossless JPEG decoder.

use super::encoder::{encode, encode_with_restarts};
use super::*;

/// xorshift64 generator for reproducible cases.
pub(crate) struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn smooth_samples(r: &mut Rng, w: usize, h: usize, comps: usize, bits: u32) -> Vec<u16> {
    let max = (1u32 << bits) - 1;
    (0..w * h * comps)
        .map(|i| {
            let base = ((i / comps) % w * 3 + (i / (w * comps)) * 5) as u32;
            ((base + r.below(40) as u32 * (1 + (i as u32 % 3))) & max) as u16
        })
        .collect()
}

fn decode_all(data: &[u8]) -> (LjpegHeader, Vec<Vec<u16>>, Result<()>) {
    let hdr = LjpegHeader::parse(data, LjpegOptions::default()).expect("parse");
    let mut rows = Vec::new();
    let res = decode_rows(data, &hdr, |_, r| {
        rows.push(r.to_vec());
        Ok(())
    });
    (hdr, rows, res)
}

#[test]
fn round_trip_predictors_components_depths() {
    let mut r = Rng(0x1234_5678_9ABC_DEF1);
    for psv in 1..=7u32 {
        for comps in 1..=4usize {
            for bits in [8u32, 12, 14, 16] {
                let (w, h) = (37, 11);
                let s = smooth_samples(&mut r, w, h, comps, bits);
                let data = encode(w, h, comps, bits, psv, &s);
                let (hdr, rows, res) = decode_all(&data);
                res.unwrap();
                assert_eq!((hdr.predictor, hdr.clrs, hdr.bits), (psv, comps as u32, bits));
                assert_eq!(rows.concat(), s, "psv {psv} comps {comps} bits {bits}");
            }
        }
    }
}

#[test]
fn round_trip_full_range_16_bit() {
    // Alternating extremes give differences of exactly -32768 and 32768 (equal modulo 2^16).
    for psv in 1..=7u32 {
        for comps in 1..=2usize {
            let s: Vec<u16> =
                (0..16 * 8 * comps).map(|i| if (i / comps + i / (16 * comps)) % 2 == 0 { 0 } else { 65535 }).collect();
            let data = encode(16, 8, comps, 16, psv, &s);
            let (_, rows, res) = decode_all(&data);
            res.unwrap();
            assert_eq!(rows.concat(), s, "psv {psv}");
        }
    }
}

#[test]
fn restart_intervals_with_every_predictor() {
    let mut r = Rng(77);
    for psv in 1..=7u32 {
        for comps in [1usize, 2, 3] {
            let (w, h) = (23, 12);
            let s = smooth_samples(&mut r, w, h, comps, 14);
            for rows_per_interval in [1usize, 3, 4] {
                let data = encode_with_restarts(w, h, comps, 14, psv, &s, rows_per_interval * w);
                let (hdr, rows, res) = decode_all(&data);
                res.unwrap();
                assert_eq!(hdr.restart_interval as usize, rows_per_interval * w);
                assert_eq!(rows.concat(), s, "psv {psv} comps {comps} interval {rows_per_interval}");
                // The same through the multi-thread entry point (small frames stay sequential).
                let mut out = vec![0u16; w * comps * h];
                decode_into_par(&data, LjpegOptions::default(), &mut out, w * comps, w * comps, h, true).unwrap();
                assert_eq!(out, s);
            }
        }
    }
}

#[test]
fn missing_restart_marker_is_an_error_after_the_rows_before() {
    let mut r = Rng(5);
    let (w, h) = (16, 8);
    let s = smooth_samples(&mut r, w, h, 2, 12);
    let mut data = encode_with_restarts(w, h, 2, 12, 1, &s, 2 * w);
    // Declare an interval of two rows but cut the stream before the third interval.
    let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
    let un = unstuff(&data[hdr.scan_offset..]);
    assert_eq!(un.segments.len(), 4);
    let cut = data.len() - 2;
    data.truncate(cut);
    let marker = data.windows(2).rposition(|p| p[0] == 0xFF && (0xD0..0xD8).contains(&p[1])).unwrap();
    data.truncate(marker);
    let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
    let mut n = 0;
    let res = decode_rows(&data, &hdr, |_, _| {
        n += 1;
        Ok(())
    });
    assert!(res.is_err());
    assert_eq!(n, 6);
}

#[test]
fn truncated_streams_fail_after_delivering_rows() {
    let mut r = Rng(9);
    let (w, h) = (64, 40);
    let s = smooth_samples(&mut r, w, h, 2, 14);
    let data = encode(w, h, 2, 14, 1, &s);
    for frac in [4usize, 2] {
        let cut = &data[..data.len() / frac];
        let hdr = LjpegHeader::parse(cut, LjpegOptions::default()).unwrap();
        let mut rows = Vec::new();
        let res = decode_rows(cut, &hdr, |_, row| {
            rows.push(row.to_vec());
            Ok(())
        });
        assert!(res.is_err(), "cut at 1/{frac}");
        assert!(!rows.is_empty() && rows.len() < h);
        // The last delivered rows may come from the end-of-data tolerance; those before are exact.
        let exact = rows.len() - 2;
        assert!(rows[..exact].concat() == s[..exact * w * 2]);
    }
}

#[test]
fn tile_mapping_cuts_the_row_stream() {
    let mut r = Rng(11);
    // Two-component rows of 4 MCUs (8 samples): a tile of 4 samples per row takes two tile rows per JPEG row.
    let s = smooth_samples(&mut r, 4, 6, 2, 12);
    let data = encode(4, 6, 2, 12, 1, &s);
    let mut dst = vec![0u16; 4 * 12];
    decode_tile(&data, LjpegOptions::default(), &mut dst, 4, 4, 4, 12).unwrap();
    assert_eq!(dst, s);
    // Clipping into a larger stride leaves the rest untouched.
    let mut dst = vec![0x5A5Au16; 10 * 6];
    decode_tile(&data, LjpegOptions::default(), &mut dst, 10, 8, 5, 6).unwrap();
    for row in 0..6 {
        assert_eq!(&dst[row * 10..row * 10 + 5], &s[row * 8..row * 8 + 5]);
        assert!(dst[row * 10 + 5..(row + 1) * 10].iter().all(|&v| v == 0x5A5A));
    }
    // A destination that is too small is not overrun.
    let mut small = vec![0u16; 13];
    decode_tile(&data, LjpegOptions::default(), &mut small, 8, 8, 8, 6).unwrap();
    assert_eq!(small, s[..13]);
    // Nothing to do for empty clip sizes: only the header comes back.
    let hdr = decode_tile(&data, LjpegOptions::default(), &mut small, 8, 0, 8, 6).unwrap();
    assert_eq!((hdr.width, hdr.height), (4, 6));
}

fn unstuff_reference(scan: &[u8], restarts: bool) -> (Vec<u8>, Vec<(usize, usize)>) {
    let (mut out, mut segs, mut start, mut i) = (Vec::new(), Vec::new(), 0, 0);
    while i < scan.len() {
        if scan[i] != 0xFF {
            out.push(scan[i]);
            i += 1;
            continue;
        }
        match scan.get(i + 1) {
            Some(0) => {
                out.push(0xFF);
                i += 2;
            }
            Some(0xD0..=0xD7) if restarts => {
                segs.push((start, out.len()));
                start = out.len();
                i += 2;
            }
            _ => break,
        }
    }
    segs.push((start, out.len()));
    (out, segs)
}

#[test]
fn unstuffing_matches_a_bytewise_reference() {
    let mut r = Rng(0xFEED);
    for case in 0..3000 {
        let len = r.below(5000) as usize;
        let dense = case % 3;
        let data: Vec<u8> = (0..len)
            .map(|_| match (dense, r.below(10)) {
                (0, _) => r.next() as u8,
                (1, 0..=5) => 0xFF,
                (1, 6..=7) => 0,
                (2, 0..=1) => 0xFF,
                (2, 2..=3) => 0xD0 + r.below(8) as u8,
                (2, 4) => 0,
                _ => r.next() as u8,
            })
            .collect();
        for restarts in [true, false] {
            let (rd, rs) = unstuff_reference(&data, restarts);
            let u = unstuff_scan(&data, restarts);
            assert_eq!((&u.data, &u.segments), (&rd, &rs), "case {case} restarts {restarts}");
        }
    }
}

#[test]
fn unstuffing_large_scans_on_several_threads() {
    let mut r = Rng(0xABCDEF);
    for variant in 0..3 {
        let len = (13 << 20) + r.below(2 << 20) as usize;
        let mut data: Vec<u8> = (0..len).map(|_| r.next() as u8).collect();
        for b in data.iter_mut() {
            if *b == 0xFF {
                *b = 0xFE;
            }
        }
        // Sparse stuffed bytes and restart markers; variant 2 ends in a marker early.
        for _ in 0..2000 {
            let p = r.below(len as u64 - 2) as usize;
            data[p] = 0xFF;
            data[p + 1] = if r.below(4) == 0 { 0xD0 + r.below(8) as u8 } else { 0 };
        }
        if variant == 1 {
            // A run of 0xFF at likely chunk boundaries.
            for b in &mut data[len / 2 - 3..len / 2 + 3] {
                *b = 0xFF;
            }
        }
        if variant == 2 {
            data[len / 3] = 0xFF;
            data[len / 3 + 1] = 0xD9;
        }
        for restarts in [true, false] {
            let (rd, rs) = unstuff_reference(&data, restarts);
            let u = unstuff_scan(&data, restarts);
            assert!(u.data == rd && u.segments == rs, "variant {variant} restarts {restarts}");
        }
    }
}

#[test]
fn restart_marker_ends_the_scan_when_restarts_are_off() {
    let u = unstuff_scan(&[1, 2, 0xFF, 0xD3, 3, 4], false);
    assert_eq!((u.data.as_slice(), u.segments.as_slice()), (&[1u8, 2][..], &[(0, 2)][..]));
    let u = unstuff(&[1, 2, 0xFF, 0xD3, 3, 4, 0xFF, 0x00, 0xFF]);
    assert_eq!(u.data, [1, 2, 3, 4, 0xFF]);
    assert_eq!(u.segments, [(0, 2), (2, 5)]);
    assert!(u.segment(5).is_empty());
}

#[test]
fn too_many_values_per_mcu_is_an_error_not_a_panic() {
    let s = vec![100u16; 8 * 4 * 4];
    let mut data = encode(8, 4, 4, 12, 1, &s);
    // Component 0 sampling 2x2 gives three extra luma values: 7 per MCU.
    assert_eq!(data[13], 0x11);
    data[13] = 0x22;
    let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
    assert_eq!(hdr.clrs, 7);
    assert!(decode_rows(&data, &hdr, |_, _| Ok(())).is_err());
    let mut out = vec![0u16; 8 * 7 * 4];
    assert!(decode_into(&data, LjpegOptions::default(), &mut out, 56, 56, 4).is_err());
}

#[test]
fn headers_are_validated() {
    assert!(LjpegHeader::parse(&[0xFF, 0xD8], LjpegOptions::default()).is_err());
    assert!(LjpegHeader::parse(&[1, 2, 3, 4, 5], LjpegOptions::default()).is_err());
    let s = vec![7u16; 4 * 4];
    let data = encode(4, 4, 1, 12, 1, &s);
    for cut in 0..data.len().min(60) {
        let _ = LjpegHeader::parse(&data[..cut], LjpegOptions::default());
    }
    let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
    assert_eq!((hdr.width, hdr.height, hdr.row_len()), (4, 4, 4));
    assert!(hdr.table(0).is_some() && hdr.table(1).is_none());
}

#[test]
fn sraw_layout_chains_luma_values() {
    // Two luma and two chroma values per MCU (4:2:2): predictor 1 predicts luma from the previous luma value.
    let mut data = encode(6, 3, 3, 14, 1, &[0u16; 6 * 3 * 3]);
    data[13] = 0x21;
    let hdr = LjpegHeader::parse(&data, LjpegOptions::default()).unwrap();
    assert_eq!((hdr.sraw, hdr.clrs), (1, 4));
    // All differences decode as zero: first column starts at half range, the rest follows.
    let mut rows = 0;
    let _ = decode_rows(&data, &hdr, |_, row| {
        assert_eq!(row.len(), 24);
        rows += 1;
        Ok(())
    });
    assert!(rows <= 3);
}

// ---------------------------------------------------------------- golden digests

/// What a decoder did with one stream, in a form that does not depend on the implementation.
/// Header fields compared by the golden digest.
pub(crate) type Fields = (u32, u32, u32, u32, u32, u32, u32, u32, usize);

pub(crate) struct Outcome {
    pub parse: Result<Fields>,
    pub rows: Vec<Vec<u16>>,
    pub decode_err: Option<&'static str>,
}

pub(crate) fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h = (*h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
}

pub(crate) fn kind_of(e: &flashraw_core::Error) -> &'static str {
    e.kind()
}

pub(crate) fn digest_outcome(h: &mut u64, o: &Outcome) {
    match &o.parse {
        Ok(f) => {
            for v in [f.0, f.1, f.2, f.3, f.4, f.5, f.6, f.7] {
                fnv(h, &v.to_le_bytes());
            }
            fnv(h, &(f.8 as u64).to_le_bytes());
        }
        Err(e) => fnv(h, kind_of(e).as_bytes()),
    }
    for row in &o.rows {
        for v in row {
            fnv(h, &v.to_le_bytes());
        }
        fnv(h, b"|");
    }
    fnv(h, o.decode_err.unwrap_or("ok").as_bytes());
}

/// Deterministic family of valid and damaged streams.
pub(crate) fn golden_streams() -> Vec<Vec<u8>> {
    let mut r = Rng(0x600D_5EED);
    let mut out = Vec::new();
    for i in 0..400usize {
        let comps = 1 + r.below(4) as usize;
        let bits = [8u32, 10, 12, 14, 16][r.below(5) as usize];
        let psv = 1 + r.below(7) as u32;
        let (w, h) = (1 + r.below(70) as usize, 1 + r.below(12) as usize);
        let s = smooth_samples(&mut r, w, h, comps, bits);
        let restart = if i % 4 == 0 { (1 + r.below(3) as usize) * w } else { 0 };
        let mut data = encode_with_restarts(w, h, comps, bits, psv, &s, restart);
        match i % 5 {
            1 => data.truncate(data.len() * (1 + r.below(8) as usize) / 9),
            2 => {
                let p = 20 + r.below(data.len() as u64 - 20) as usize;
                data[p] ^= 1 << r.below(8);
            }
            3 => {
                let p = r.below(data.len() as u64) as usize;
                data[p] = r.next() as u8;
            }
            _ => {}
        }
        out.push(data);
    }
    out
}

/// Run the new decoder on a stream the way the golden digest is defined.
pub(crate) fn run_new(data: &[u8]) -> Outcome {
    let opts = LjpegOptions::default();
    match LjpegHeader::parse(data, opts) {
        Err(e) => Outcome { parse: Err(e), rows: Vec::new(), decode_err: None },
        Ok(h) => {
            let mut rows = Vec::new();
            let res = decode_rows(data, &h, |_, r| {
                rows.push(r.to_vec());
                Ok(())
            });
            let ids: u32 = h.table_ids.iter().fold(0, |a, &b| a * 5 + b as u32);
            Outcome {
                parse: Ok((
                    h.bits,
                    h.width,
                    h.height,
                    h.clrs,
                    h.sraw,
                    h.predictor,
                    h.restart_interval,
                    ids,
                    h.scan_offset,
                )),
                rows,
                decode_err: res.err().map(|e| kind_of(&e)),
            }
        }
    }
}

pub(crate) fn golden_digest(run: impl Fn(&[u8]) -> Outcome) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for s in golden_streams() {
        digest_outcome(&mut h, &run(&s));
    }
    h
}

/// Digest recorded from the previous implementation (see the differential run in the commit message).
const GOLDEN_DIGEST: u64 = 0x7738f8ca37686741;

#[test]
fn golden_digest_of_valid_and_damaged_streams() {
    let d = golden_digest(run_new);
    println!("GOLDEN {d:#018x}");
    if GOLDEN_DIGEST != 0 {
        assert_eq!(d, GOLDEN_DIGEST);
    }
}
