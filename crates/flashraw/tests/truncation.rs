//! Truncated sensor data: structured errors under the strict policy, zero-filled data
//! with warnings under the (default) recover policy, never silent.
//!
//! Synthetic little-endian DNGs with explicit strip/tile layouts are cut at precise
//! points: exactly at the end of the data, one byte short, mid-sample, mid-strip, with
//! a strip or tile starting past the end of the file, and with ranges that overflow.

use flashraw::{CorruptReason, CorruptionPolicy, DataUnit, DecodeOptions, Error, FlashRaw, RawFrame, Rect};

const W: u32 = 64;
const H: u32 = 48;

fn shorts(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn longs(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

type Entry = (u16, u16, u32, Vec<u8>);

/// Serialise a single-IFD TIFF whose entries may refer to the offsets of `blobs`
/// (written after the IFD and its out-of-line values, in order).
fn tiff(blobs: &[Vec<u8>], entries: impl Fn(&[u32]) -> Vec<Entry>) -> Vec<u8> {
    let layout = |offs: &[u32]| {
        let mut e = entries(offs);
        e.sort_by_key(|x| x.0);
        let extra: usize = e.iter().filter(|x| x.3.len() > 4).map(|x| x.3.len() + (x.3.len() & 1)).sum();
        let end = 8 + 2 + e.len() * 12 + 4;
        (e, end, extra)
    };
    let (_, ifd_end, extra) = layout(&vec![0; blobs.len()]);
    let mut offs = Vec::new();
    let mut pos = ifd_end + extra;
    for b in blobs {
        offs.push(pos as u32);
        pos += b.len();
    }
    let (e, ifd_end, _) = layout(&offs);
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend((e.len() as u16).to_le_bytes());
    let mut extra = Vec::new();
    for (tag, typ, count, data) in &e {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        if data.len() <= 4 {
            let mut d = data.clone();
            d.resize(4, 0);
            out.extend(d);
        } else {
            out.extend(((ifd_end + extra.len()) as u32).to_le_bytes());
            extra.extend(data);
            if data.len() & 1 == 1 {
                extra.push(0);
            }
        }
    }
    out.extend(0u32.to_le_bytes());
    out.extend(extra);
    for b in blobs {
        out.extend(b);
    }
    out
}

/// Base DNG tags for a `W x H` RGGB image of `bps` bits with `compression`.
fn dng_tags(compression: u16, bps: u16) -> Vec<Entry> {
    vec![
        (0x00FE, 4, 1, longs(&[0])),
        (0x0100, 4, 1, longs(&[W])),
        (0x0101, 4, 1, longs(&[H])),
        (0x0102, 3, 1, shorts(&[bps])),
        (0x0103, 3, 1, shorts(&[compression])),
        (0x0106, 3, 1, shorts(&[32803])),
        (0x010F, 2, 5, b"Test\0".to_vec()),
        (0x0110, 2, 6, b"Model\0".to_vec()),
        (0x0115, 3, 1, shorts(&[1])),
        (0x828D, 3, 2, shorts(&[2, 2])),
        (0x828E, 1, 4, vec![0, 1, 1, 2]),
        (0xC612, 1, 4, vec![1, 4, 0, 0]),
        (0xC61A, 4, 1, longs(&[0])),
        (0xC61D, 4, 1, longs(&[(1u32 << bps.min(16)) - 1])),
    ]
}

fn samples(bits: u32) -> Vec<u16> {
    let m = (1u32 << bits) - 1;
    (0..W * H).map(|i| (((i * 2_654_435_761u32) >> 7) & m).max(1) as u16).collect()
}

/// MSB-first bit packing of `v` at `bits` per sample.
fn pack_msb(v: &[u16], bits: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut n) = (0u64, 0u32);
    for &s in v {
        acc = (acc << bits) | s as u64;
        n += bits;
        while n >= 8 {
            out.push((acc >> (n - 8)) as u8);
            n -= 8;
        }
    }
    if n > 0 {
        out.push((acc << (8 - n)) as u8);
    }
    out
}

/// Payload of `rows` rows starting at `row0` (rows packed back to back; W*bits is a
/// multiple of 8 for the depths used here).
fn rows_payload(v: &[u16], bits: u32, row0: u32, rows: u32) -> Vec<u8> {
    let s = &v[(row0 * W) as usize..((row0 + rows) * W) as usize];
    if bits == 16 { shorts(s) } else { pack_msb(s, bits) }
}

/// Uncompressed DNG with strips of `rps` rows, stored in `order` (indices into the strip list).
fn stripped(bits: u32, rps: u32, order: &[usize]) -> Vec<u8> {
    let v = samples(bits);
    let n = H.div_ceil(rps) as usize;
    let strips: Vec<Vec<u8>> = (0..n as u32).map(|i| rows_payload(&v, bits, i * rps, rps.min(H - i * rps))).collect();
    let blobs: Vec<Vec<u8>> = order.iter().map(|&i| strips[i].clone()).collect();
    let lens: Vec<u32> = strips.iter().map(|s| s.len() as u32).collect();
    tiff(&blobs, |offs| {
        let mut e = dng_tags(1, bits as u16);
        let mut so = vec![0u32; n];
        for (k, &i) in order.iter().enumerate() {
            so[i] = offs[k];
        }
        e.push((0x0111, 4, n as u32, longs(&so)));
        e.push((0x0116, 4, 1, longs(&[rps])));
        e.push((0x0117, 4, n as u32, longs(&lens)));
        e
    })
}

/// Uncompressed 16-bit DNG with 32x16 tiles in row-major order.
fn tiled() -> Vec<u8> {
    let v = samples(16);
    let (tw, th) = (32u32, 16u32);
    let mut blobs = Vec::new();
    for ty in 0..H / th {
        for tx in 0..W / tw {
            let mut t = Vec::new();
            for y in 0..th {
                let r = ((ty * th + y) * W + tx * tw) as usize;
                t.extend(shorts(&v[r..r + tw as usize]));
            }
            blobs.push(t);
        }
    }
    let n = blobs.len() as u32;
    let lens: Vec<u32> = blobs.iter().map(|b| b.len() as u32).collect();
    tiff(&blobs, |offs| {
        let mut e = dng_tags(1, 16);
        e.push((0x0142, 4, 1, longs(&[tw])));
        e.push((0x0143, 4, 1, longs(&[th])));
        e.push((0x0144, 4, n, longs(offs)));
        e.push((0x0145, 4, n, longs(&lens)));
        e
    })
}

/// Lossless-JPEG DNG (one strip).
fn ljpeg() -> Vec<u8> {
    let v = samples(14);
    let jpg = flashraw::low_level::codecs::ljpeg::encoder::encode(W as usize / 2, H as usize, 2, 14, 1, &v);
    let len = jpg.len() as u32;
    tiff(&[jpg], |offs| {
        let mut e = dng_tags(7, 14);
        e.push((0x0111, 4, 1, longs(&[offs[0]])));
        e.push((0x0116, 4, 1, longs(&[H])));
        e.push((0x0117, 4, 1, longs(&[len])));
        e
    })
}

fn opts(policy: CorruptionPolicy) -> DecodeOptions {
    DecodeOptions { corruption_policy: policy, threads: Some(2), ..Default::default() }
}

fn decode(data: &[u8], policy: CorruptionPolicy) -> flashraw::Result<RawFrame> {
    FlashRaw::open(data)?.decode(opts(policy))
}

fn strict_err(data: &[u8]) -> Error {
    decode(data, CorruptionPolicy::Strict).expect_err("strict decode of truncated data must fail")
}

#[test]
fn recover_is_the_default() {
    assert_eq!(DecodeOptions::default().corruption_policy, CorruptionPolicy::Recover);
}

#[test]
fn exact_length_files_decode_without_warnings() {
    for (bits, rps) in [(16, H), (16, 5), (12, 7), (14, 16)] {
        let f = stripped(bits, rps, &(0..H.div_ceil(rps) as usize).collect::<Vec<_>>());
        for p in [CorruptionPolicy::Strict, CorruptionPolicy::Recover] {
            let fr = decode(&f, p).unwrap_or_else(|e| panic!("{bits}-bit rps {rps}: {e}"));
            assert_eq!(fr.as_u16().unwrap(), &samples(bits)[..], "{bits}-bit rps {rps}");
            assert!(fr.warnings().is_empty() && !fr.is_incomplete());
        }
    }
    let t = tiled();
    assert_eq!(decode(&t, CorruptionPolicy::Strict).unwrap().as_u16().unwrap(), &samples(16)[..]);
}

#[test]
fn eof_at_end_of_final_strip_is_complete() {
    // The last strip ends exactly at EOF; a declared length beyond it (padding) is not needed.
    let mut f = stripped(16, 10, &[0, 1, 2, 3, 4]);
    let fr = decode(&f, CorruptionPolicy::Strict).unwrap();
    assert!(fr.warnings().is_empty());
    // Trailing bytes after the last strip change nothing.
    f.extend([0xAB; 7]);
    assert!(decode(&f, CorruptionPolicy::Strict).unwrap().warnings().is_empty());
}

#[test]
fn final_strip_one_byte_short() {
    let f = stripped(16, 10, &[0, 1, 2, 3, 4]);
    let cut = &f[..f.len() - 1];
    let e = strict_err(cut);
    assert_eq!(e.kind(), "CorruptFile");
    let t = e.truncation().expect("structured truncation");
    assert_eq!(t.unit, DataUnit::Strip(4));
    assert_eq!(t.expected, 8 * W as u64 * 2);
    assert_eq!(t.available, t.expected - 1);
    assert_eq!(t.file_len, cut.len() as u64);
    assert!(e.to_string().contains("strip 4 expected 1024 bytes"), "{e}");
    assert!(e.to_string().contains("but only 1023 bytes remain before EOF"), "{e}");

    let fr = decode(cut, CorruptionPolicy::Recover).unwrap();
    assert!(fr.is_incomplete());
    assert_eq!(fr.warnings().len(), 1);
    let w = &fr.warnings()[0];
    assert_eq!(w.samples_filled, 1);
    assert_eq!(w.reason.truncation().unwrap().unit, DataUnit::Strip(4));
    assert!(w.to_string().starts_with("recovered truncated strip 4: 1023 / 1024 bytes available"), "{w}");
    let (got, want) = (fr.as_u16().unwrap(), samples(16));
    assert_eq!(&got[..got.len() - 1], &want[..want.len() - 1]);
    assert_eq!(got[got.len() - 1], 0);
}

#[test]
fn strip_truncated_substantially() {
    let f = stripped(16, H, &[0]);
    let keep = f.len() - (W * H) as usize; // half the 16-bit payload
    let cut = &f[..keep];
    let t = strict_err(cut).truncation().cloned().unwrap();
    assert_eq!((t.unit, t.expected, t.available), (DataUnit::Strip(0), (W * H * 2) as u64, (W * H) as u64));
    let fr = decode(cut, CorruptionPolicy::Recover).unwrap();
    let half = (W * H / 2) as usize;
    assert_eq!(&fr.as_u16().unwrap()[..half], &samples(16)[..half]);
    assert!(fr.as_u16().unwrap()[half..].iter().all(|&v| v == 0));
    assert_eq!(fr.warnings()[0].samples_filled, half as u64);
    // A region above the missing rows needs none of them: no error, no warning.
    let raw = FlashRaw::open(cut).unwrap();
    let r = raw.decode_region(Rect::new(0, 0, W, H / 2), opts(CorruptionPolicy::Strict)).unwrap();
    assert!(r.warnings().is_empty());
    assert!(raw.decode_region(Rect::new(0, H / 2, W, 4), opts(CorruptionPolicy::Strict)).is_err());
}

#[test]
fn tile_beyond_eof() {
    let f = tiled();
    // Last tile (index 5) loses its final 100 bytes.
    let cut = &f[..f.len() - 100];
    let t = strict_err(cut).truncation().cloned().unwrap();
    assert_eq!(t.unit, DataUnit::Tile(5));
    assert_eq!(t.available, t.expected - 100);
    let fr = decode(cut, CorruptionPolicy::Recover).unwrap();
    assert_eq!(fr.warnings().len(), 1);
    assert_eq!(fr.warnings()[0].samples_filled, 50);
    // Tile 4 (left of tile 5) is untouched.
    let want = samples(16);
    let got = fr.as_u16().unwrap();
    for y in 32..48 {
        let r = (y * W) as usize;
        assert_eq!(&got[r..r + 32], &want[r..r + 32]);
    }
    // A region inside complete tiles decodes strictly.
    let raw = FlashRaw::open(cut).unwrap();
    assert!(raw.decode_region(Rect::new(0, 0, 32, 48), opts(CorruptionPolicy::Strict)).is_ok());
}

#[test]
fn offset_beyond_eof() {
    let mut f = stripped(16, H, &[0]);
    let payload_len = (W * H * 2) as usize;
    f.truncate(f.len() - payload_len); // the strip offset now equals the file length
    let raw = FlashRaw::open(&f[..]).expect("metadata parses without the payload");
    assert_eq!(raw.metadata().make, "Test");
    assert_eq!(raw.frame_info().width, W);
    let e = raw.decode(opts(CorruptionPolicy::Strict)).unwrap_err();
    let t = e.truncation().unwrap();
    assert!(t.starts_beyond_eof());
    assert_eq!(t.available, 0);
    assert!(e.to_string().contains("beyond the end of the file"), "{e}");
    let fr = raw.decode(opts(CorruptionPolicy::Recover)).unwrap();
    assert!(fr.as_u16().unwrap().iter().all(|&v| v == 0));
    assert_eq!(fr.warnings()[0].samples_filled, (W * H) as u64);
}

#[test]
fn offset_plus_length_overflow() {
    // Strip offset near u32::MAX: offset + length overflows 32 bits and is far past EOF.
    let v = samples(16);
    let f = tiff(&[shorts(&v)], |_| {
        let mut e = dng_tags(1, 16);
        e.push((0x0111, 4, 1, longs(&[0xFFFF_FFF0])));
        e.push((0x0116, 4, 1, longs(&[H])));
        e.push((0x0117, 4, 1, longs(&[u32::MAX])));
        e
    });
    let raw = FlashRaw::open(&f[..]).unwrap();
    let t = raw.decode(opts(CorruptionPolicy::Strict)).unwrap_err().truncation().cloned().unwrap();
    assert_eq!(t.offset, 0xFFFF_FFF0);
    assert!(t.starts_beyond_eof());
    let fr = raw.decode(opts(CorruptionPolicy::Recover)).unwrap();
    assert!(fr.is_incomplete());
    // An overflowing range in the address space itself is reported, not a panic.
    let t = flashraw::Truncation::new(DataUnit::Strip(0), u64::MAX - 4, 100, 10);
    assert!(t.range_overflows());
    assert!(t.to_string().contains("overflows"));
}

#[test]
fn packed_incomplete_final_sample_group() {
    for bits in [12u32, 14] {
        let f = stripped(bits, H, &[0]);
        let want = samples(bits);
        // Cut one byte: the last sample loses some of its bits.
        let cut = &f[..f.len() - 1];
        let t = strict_err(cut).truncation().cloned().unwrap();
        assert_eq!(t.expected, (W * H * bits / 8) as u64);
        let fr = decode(cut, CorruptionPolicy::Recover).unwrap();
        let got = fr.as_u16().unwrap();
        let n = got.len();
        assert_eq!(&got[..n - 1], &want[..n - 1], "{bits}-bit");
        assert_eq!(got[n - 1], 0, "{bits}-bit: a partial sample is zero, not padding bits");
        assert_eq!(fr.warnings()[0].samples_filled, 1);
        // Cut mid-group: 3 bytes short of a 12-bit pair / 14-bit quad.
        let cut = &f[..f.len() - 3];
        let fr = decode(cut, CorruptionPolicy::Recover).unwrap();
        let got = fr.as_u16().unwrap();
        let lost = 24u32.div_ceil(bits) as usize;
        assert_eq!(&got[..n - lost], &want[..n - lost], "{bits}-bit");
        assert!(got[n - lost..].iter().all(|&v| v == 0));
        assert_eq!(fr.warnings()[0].samples_filled, lost as u64);
    }
}

#[test]
fn only_one_of_several_strips_truncated() {
    // Strip 1 is stored last and cut; strips 0, 2, 3, 4 are complete.
    let f = stripped(16, 10, &[0, 2, 3, 4, 1]);
    let cut = &f[..f.len() - 300];
    let e = strict_err(cut);
    assert_eq!(e.truncation().unwrap().unit, DataUnit::Strip(1));
    let fr = decode(cut, CorruptionPolicy::Recover).unwrap();
    assert_eq!(fr.warnings().len(), 1);
    assert_eq!(fr.warnings()[0].reason.unit(), DataUnit::Strip(1));
    assert_eq!(fr.warnings()[0].samples_filled, 150);
    let (got, want) = (fr.as_u16().unwrap(), samples(16));
    let row = |y: u32| (y * W) as usize;
    assert_eq!(&got[..row(20) - 150], &want[..row(20) - 150]);
    assert!(got[row(20) - 150..row(20)].iter().all(|&v| v == 0));
    assert_eq!(&got[row(20)..], &want[row(20)..]);
}

#[test]
fn compressed_and_uncompressed_truncation_are_consistent() {
    let unc = stripped(16, H, &[0]);
    let ljp = ljpeg();
    for f in [&unc, &ljp] {
        let cut = &f[..f.len() - 200];
        // Strict: the same error kind and a structured truncation of strip 0.
        let e = strict_err(cut);
        assert_eq!(e.kind(), "CorruptFile");
        let t = e.truncation().unwrap();
        assert_eq!((t.unit, t.available + 200), (DataUnit::Strip(0), t.expected));
        // Recover: a frame flagged incomplete with a truncation warning.
        let fr = decode(cut, CorruptionPolicy::Recover).unwrap();
        assert!(fr.is_incomplete());
        assert!(matches!(fr.warnings()[0].reason, CorruptReason::Truncated(_)));
        // Metadata never depended on the payload.
        assert!(FlashRaw::open(cut).unwrap().metadata().width > 0);
    }
    // Corrupt (not truncated) entropy data: strict error, recovered as an invalid unit.
    let mut bad = ljp.clone();
    let n = bad.len();
    for b in &mut bad[n - 400..n - 2] {
        *b = 0xFF;
    }
    let e = strict_err(&bad);
    assert_eq!(e.kind(), "CorruptFile");
    if let Ok(fr) = decode(&bad, CorruptionPolicy::Recover) {
        assert!(fr.is_incomplete());
        assert!(matches!(fr.warnings()[0].reason, CorruptReason::InvalidUnit { .. }));
    }
}

#[test]
fn every_cut_point_is_reported() {
    // Cutting anywhere inside the payload: strict always fails with a truncation, recover
    // always warns, and both agree on the bytes available.
    let f = stripped(12, 7, &(0..7).collect::<Vec<_>>());
    let payload_start = f.len() - (W * H * 12 / 8) as usize;
    for cut in (payload_start..f.len()).step_by(37) {
        let e = strict_err(&f[..cut]);
        let t = e.truncation().cloned().unwrap_or_else(|| panic!("cut {cut}: {e}"));
        let fr = decode(&f[..cut], CorruptionPolicy::Recover).unwrap();
        assert!(fr.is_incomplete(), "cut {cut}");
        let first = fr.warnings()[0].reason.truncation().unwrap().clone();
        assert_eq!(first, t, "cut {cut}");
    }
}

#[test]
fn previews_survive_a_truncated_payload() {
    let jpeg = [&[0xFF, 0xD8, 0xFF, 0xD9][..], &[0u8; 60][..]].concat();
    let v = samples(16);
    let f = tiff(&[jpeg.clone(), shorts(&v)], |offs| {
        let mut e = dng_tags(1, 16);
        e.push((0x0111, 4, 1, longs(&[offs[1]])));
        e.push((0x0116, 4, 1, longs(&[H])));
        e.push((0x0117, 4, 1, longs(&[W * H * 2])));
        e.push((0x0201, 4, 1, longs(&[offs[0]])));
        e.push((0x0202, 4, 1, longs(&[jpeg.len() as u32])));
        e
    });
    // Keep the preview and a few bytes of the payload.
    let cut = &f[..f.len() - (W * H * 2) as usize + 10];
    let raw = FlashRaw::open(cut).unwrap();
    let p = raw.preview().expect("preview listed").clone();
    assert_eq!(raw.preview_bytes(&p).unwrap(), &jpeg[..]);
    assert!(raw.decode(opts(CorruptionPolicy::Strict)).unwrap_err().truncation().is_some());
    assert_eq!(raw.decode(opts(CorruptionPolicy::Recover)).unwrap().warnings()[0].samples_filled, (W * H - 5) as u64);
}
