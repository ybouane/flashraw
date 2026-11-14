//! Tests of the threaded frame decoder against the sequential one.

use super::*;
use crate::ljpeg::encoder::{encode, encode_with_restarts};
use crate::ljpeg::{LjpegOptions, decode_rows};
use crate::par_entropy::step_bulk;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn smooth(r: &mut Rng, w: usize, h: usize, comps: usize, bits: u32) -> Vec<u16> {
    let max = (1u32 << bits) - 1;
    (0..w * h * comps)
        .map(|i| {
            let base = ((i / comps) % w * 3 + (i / (w * comps)) * 5) as u32;
            ((base + r.below(60) as u32 * (1 + (i as u32 % 3))) & max) as u16
        })
        .collect()
}

/// The rows the sequential decoder delivers (`rows` of them), and whether it succeeded.
fn sequential(data: &[u8], hdr: &LjpegHeader, rows: usize) -> (Vec<u16>, bool) {
    let mut out = Vec::new();
    let res = decode_rows(data, hdr, |y, row| {
        if y < rows {
            out.extend_from_slice(row);
        }
        Ok(())
    });
    (out, res.is_ok())
}

fn parse(data: &[u8]) -> LjpegHeader {
    LjpegHeader::parse(data, LjpegOptions::default()).expect("parse")
}

#[test]
fn encoder_frames_decode_like_the_sequential_decoder() {
    let mut r = Rng(0xABCD);
    for comps in 1..=4usize {
        for psv in 1..=7u32 {
            for bits in [14u32, 16] {
                for (w, h) in [(97usize, 61usize), (300, 2), (129, 5), (700, 61)] {
                    let samples = smooth(&mut r, w, h, comps, bits);
                    let data = encode(w, h, comps, bits, psv, &samples);
                    let hdr = parse(&data);
                    let (want, ok) = sequential(&data, &hdr, h);
                    assert!(ok && want == samples);
                    let chunks: &[usize] = if w == 700 { &[5000, 1 << 20] } else { &[7, 64, 333, 1000, 5000, 1 << 20] };
                    for &chunk in chunks {
                        let mut frame = vec![0u16; w * h * comps];
                        let done = decode_frame_chunked(&data, &hdr, h, &mut frame, Some(chunk)).unwrap();
                        assert!(done, "comps {comps} psv {psv} bits {bits} {w}x{h} chunk {chunk}");
                        assert_eq!(frame, samples, "comps {comps} psv {psv} bits {bits} {w}x{h} chunk {chunk}");
                    }
                }
            }
        }
    }
}

#[test]
fn a_stream_cut_in_half_is_left_to_the_sequential_decoder() {
    let mut r = Rng(9);
    let (w, h) = (97, 61);
    let samples = smooth(&mut r, w, h, 2, 14);
    let data = encode(w, h, 2, 14, 1, &samples);
    let hdr = parse(&data);
    let cut = &data[..hdr.scan_offset + (data.len() - hdr.scan_offset) / 2];
    let mut frame = vec![0u16; w * h * 2];
    for chunk in [7usize, 333, 1 << 20] {
        assert!(!decode_frame_chunked(cut, &hdr, h, &mut frame, Some(chunk)).unwrap());
    }
}

#[test]
fn a_row_limit_decodes_the_leading_rows() {
    let mut r = Rng(11);
    let (w, h) = (129, 40);
    let samples = smooth(&mut r, w, h, 2, 12);
    let data = encode(w, h, 2, 12, 1, &samples);
    let hdr = parse(&data);
    let rows = 17;
    let mut frame = vec![0u16; w * 2 * rows];
    // Stopping inside the data is only accepted when the slack of the sequential decoder allows it.
    if decode_frame_chunked(&data, &hdr, rows, &mut frame, Some(100)).unwrap() {
        assert_eq!(frame, samples[..w * 2 * rows]);
    }
    let mut over = vec![0u16; w * 2 * (h + 1)];
    assert!(!decode_frame_chunked(&data, &hdr, h + 1, &mut over, Some(100)).unwrap());
}

#[test]
fn frame_size_overflow_is_an_error_and_a_short_buffer_is_declined() {
    let mut r = Rng(3);
    let samples = smooth(&mut r, 50, 50, 1, 12);
    let data = encode(50, 50, 1, 12, 1, &samples);
    let hdr = parse(&data);
    let mut frame = vec![0u16; 10];
    assert!(!decode_frame_chunked(&data, &hdr, 50, &mut frame, Some(64)).unwrap());
    let mut wide = hdr.clone();
    wide.width = u32::MAX;
    wide.clrs = 4;
    let err = decode_frame_chunked(&data, &wide, usize::MAX / 2, &mut frame, Some(64));
    assert!(err.is_err());
}

#[test]
fn restart_intervals() {
    let mut r = Rng(77);
    for comps in 1..=4usize {
        for psv in 1..=7u32 {
            for interval in [1usize, 3, 8] {
                let (w, h) = (40, 30);
                let samples = smooth(&mut r, w, h, comps, 14);
                let data = encode_with_restarts(w, h, comps, 14, psv, &samples, interval * w);
                let hdr = parse(&data);
                let mut frame = vec![0u16; w * h * comps];
                let done = decode_frame_restarts(&data, &hdr, h, &mut frame).unwrap();
                if psv == 1 {
                    assert!(done);
                    assert_eq!(frame, samples);
                    // Only the intervals that hold the leading rows are needed.
                    let rows = interval + 1;
                    let mut part = vec![0u16; w * comps * rows];
                    assert!(decode_frame_restarts(&data, &hdr, rows, &mut part).unwrap());
                    assert_eq!(part, samples[..w * comps * rows]);
                } else {
                    assert!(!done);
                }
            }
        }
    }
}

#[test]
fn damaged_restart_streams_are_declined_or_identical() {
    let mut r = Rng(5);
    let (w, h, comps) = (40usize, 30usize, 2usize);
    let samples = smooth(&mut r, w, h, comps, 14);
    let data = encode_with_restarts(w, h, comps, 14, 1, &samples, 3 * w);
    let hdr = parse(&data);
    let markers: Vec<usize> = (hdr.scan_offset..data.len() - 1)
        .filter(|&i| data[i] == 0xFF && (0xD0..=0xD7).contains(&data[i + 1]))
        .collect();
    assert!(markers.len() > 3);
    // A missing marker leaves too few intervals.
    let mut cut = data.clone();
    cut.drain(markers[2]..markers[2] + 2);
    let mut frame = vec![0u16; w * h * comps];
    assert!(!decode_frame_restarts(&cut, &hdr, h, &mut frame).unwrap());
    // Damage in the middle: either declined or exactly what the sequential decoder makes of it.
    for k in 0..40 {
        let mut bad = data.clone();
        let at = hdr.scan_offset + 10 + k * 17 % (bad.len() - hdr.scan_offset - 20);
        bad[at] ^= 0x5A;
        let mut frame = vec![0u16; w * h * comps];
        if decode_frame_restarts(&bad, &hdr, h, &mut frame).unwrap() {
            let (want, ok) = sequential(&bad, &hdr, h);
            assert!(ok);
            assert_eq!(frame, want);
        }
    }
}

fn header_with_tables(ids: &[u8]) -> LjpegHeader {
    let data = encode(8, 8, 1, 12, 1, &[0u16; 64]);
    let mut hdr = parse(&data);
    hdr.clrs = ids.len() as u32;
    hdr.tables = vec![hdr.tables[0].clone(); ids.len()];
    hdr.table_ids = ids.to_vec();
    hdr
}

#[test]
fn table_period() {
    let period = |ids: &[u8]| {
        let hdr = header_with_tables(ids);
        Scan::new(&hdr, &[]).period()
    };
    assert_eq!(period(&[0]), 1);
    assert_eq!(period(&[0, 0]), 1);
    assert_eq!(period(&[0, 1]), 2);
    assert_eq!(period(&[0, 1, 1]), 3);
    assert_eq!(period(&[0, 1, 0, 1]), 2);
    assert_eq!(period(&[0, 0, 1, 1]), 4);
    assert_eq!(period(&[2, 2, 2, 2, 2, 2]), 1);
}

#[test]
fn the_batch_decoder_agrees_with_symbol_by_symbol_decoding() {
    let mut r = Rng(0xFEED);
    let data = encode(64, 8, 2, 14, 1, &smooth(&mut r, 64, 8, 2, 14));
    let hdr = parse(&data);
    for clrs in 1..=4usize {
        let mut h = hdr.clone();
        h.clrs = clrs as u32;
        h.tables = vec![hdr.tables[0].clone(); clrs];
        h.table_ids = (0..clrs as u8).map(|i| i / 2).collect();
        // Random bytes include invalid codes and long codes.
        let bytes: Vec<u8> = (0..4000).map(|_| (r.next() >> 3) as u8).collect();
        let scan = Scan::new(&h, &bytes);
        for _ in 0..200 {
            let start = r.below(bytes.len() as u64 * 8 - 400);
            let phase = r.below(clrs as u64) as usize;
            let limit = start + r.below(600);
            let max = 1 + r.below(150) as usize;
            let (mut p1, mut p2) = (scan.pump_at(start), scan.pump_at(start));
            let mut a = vec![0u16; max];
            let mut b = vec![0u16; max];
            let ra = scan.bulk(&mut p1, phase, limit, max, &mut a);
            let rb = step_bulk(&scan, &mut p2, phase, limit, max, &mut b);
            assert_eq!(ra, rb);
            assert_eq!(Scan::position(&p1), Scan::position(&p2));
            assert_eq!(a[..ra.0], b[..rb.0]);
        }
    }
}

/// The samples of `diffs` by the definition (T.81 H.1.2.1) for plain frames of up to six values per MCU.
fn plain_reference(hdr: &LjpegHeader, diffs: &[u16], rows: usize) -> Vec<u16> {
    let clrs = hdr.clrs as usize;
    let rl = hdr.row_len();
    let q = initial_prediction(hdr.bits);
    let mut s = vec![0u16; rl * rows];
    for y in 0..rows {
        for x in 0..hdr.width as usize {
            for c in 0..clrs {
                let i = y * rl + x * clrs + c;
                let d = diffs[i];
                s[i] = if x == 0 {
                    (if y == 0 { q } else { s[i - rl] }).wrapping_add(d)
                } else if y == 0 {
                    s[i - clrs].wrapping_add(d)
                } else {
                    let (ra, rb, rc) = (s[i - clrs] as i32, s[i - rl] as i32, s[i - rl - clrs] as i32);
                    let p = match hdr.predictor {
                        1 => ra,
                        2 => rb,
                        3 => rc,
                        4 => ra + rb - rc,
                        5 => ra + ((rb - rc) >> 1),
                        6 => rb + ((ra - rc) >> 1),
                        7 => (ra + rb) >> 1,
                        _ => 0,
                    };
                    (p as u16).wrapping_add(d)
                };
            }
        }
    }
    s
}

/// A header for synthetic differences: `width` MCUs of `clrs` values.
fn synthetic_header(width: u32, height: u32, clrs: u32, sraw: u32, predictor: u32) -> LjpegHeader {
    let data = encode(8, 8, 1, 14, 1, &[0u16; 64]);
    let mut hdr = parse(&data);
    hdr.width = width;
    hdr.height = height;
    hdr.clrs = clrs;
    hdr.sraw = sraw;
    hdr.predictor = predictor;
    hdr.tables = vec![hdr.tables[0].clone(); clrs as usize];
    hdr.table_ids = vec![0; clrs as usize];
    hdr
}

#[test]
fn every_rebuild_path_follows_the_definition() {
    let mut r = Rng(0xD1FF);
    // (clrs, sraw)
    for (clrs, sraw) in [(1u32, 0u32), (2, 0), (3, 0), (4, 0), (5, 0), (6, 0)] {
        for predictor in 0..=8u32 {
            for (width, height) in [(1u32, 9u32), (37, 1), (37, 12), (200, 9)] {
                let hdr = synthetic_header(width, height, clrs, sraw, predictor);
                let rows = height as usize;
                let diffs: Vec<u16> = (0..hdr.row_len() * rows).map(|_| r.next() as u16).collect();
                let want = plain_reference(&hdr, &diffs, rows);
                // One piece, and split in two at an awkward place.
                let cut = diffs.len() / 3 + 1;
                for values in [
                    Values::from_vec(diffs.clone()),
                    Values::from_pieces(vec![diffs[..cut].to_vec(), diffs[cut..].to_vec()]),
                ] {
                    let mut frame = vec![0u16; diffs.len()];
                    assert!(rebuild(&hdr, &values, rows, &mut frame).unwrap());
                    assert_eq!(frame, want, "clrs {clrs} predictor {predictor} {width}x{height}");
                }
            }
        }
    }
}

#[test]
fn sampled_frames_rebuild_like_the_row_decoder() {
    let mut r = Rng(0xA11);
    for (clrs, sraw) in [(4u32, 1u32), (4, 3), (6, 3)] {
        for predictor in [1u32, 2, 6, 7] {
            for (width, height) in [(30u32, 11u32), (5, 1)] {
                let hdr = synthetic_header(width, height, clrs, sraw, predictor);
                let rows = height as usize;
                let rl = hdr.row_len();
                let diffs: Vec<u16> = (0..rl * rows).map(|_| r.next() as u16).collect();
                // The row-by-row definition is the sequential decoder's reconstruction.
                let mut want = vec![0u16; rl * rows];
                let mut vpred = [initial_prediction(hdr.bits); 6];
                for y in 0..rows {
                    let (done, rest) = want.split_at_mut(y * rl);
                    let prev: &[u16] = if y > 0 { &done[(y - 1) * rl..] } else { &[] };
                    let mut it = diffs[y * rl..].iter();
                    let next = |_: usize| Ok(*it.next().unwrap() as i16 as i32);
                    ljpeg::reconstruct_row(&hdr, next, &mut rest[..rl], prev, y, clrs as usize, &mut vpred).unwrap();
                }
                let mut frame = vec![0u16; rl * rows];
                assert!(rebuild(&hdr, &Values::from_vec(diffs), rows, &mut frame).unwrap());
                assert_eq!(frame, want, "clrs {clrs} sraw {sraw} predictor {predictor} {width}x{height}");
            }
        }
    }
}
