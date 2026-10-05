//! Malformed-input suite: hostile files must produce errors (or, for truncated sensor
//! data under the default recover policy, frames flagged incomplete with warnings),
//! never panics, hangs or unbounded allocations.
//!
//! Part 1 builds synthetic DNGs and corrupts them in targeted ways (truncated payload,
//! invalid offsets, recursive IFDs, huge dimensions, invalid bit depth, bad Huffman
//! tables, corrupted strip offsets, overlapping data, BMFF size errors, integer
//! overflow, dangling maker-note pointers). Part 2 mutates real corpus files of every
//! format when the corpus is available. The DNG opcode-list test also checks that
//! lists apply in their stages and that unparsable lists are reported and ignored.

use flashraw::{CorruptionPolicy, DecodeOptions, FlashRaw, Limits, OutputFormat, RawPixelData};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

const W: u32 = 64;
const H: u32 = 48;

/// Little-endian TIFF/DNG writer with explicit control over every field.
struct Dng {
    entries: Vec<(u16, u16, u32, Vec<u8>)>,
    payload: Vec<u8>,
}

fn shorts(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn longs(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

impl Dng {
    fn new(compression: u16, bps: u16, payload: Vec<u8>) -> Self {
        let mut d = Dng { entries: vec![], payload };
        d.set(0x00FE, 4, 1, longs(&[0]));
        d.set(0x0100, 4, 1, longs(&[W]));
        d.set(0x0101, 4, 1, longs(&[H]));
        d.set(0x0102, 3, 1, shorts(&[bps]));
        d.set(0x0103, 3, 1, shorts(&[compression]));
        d.set(0x0106, 3, 1, shorts(&[32803]));
        d.set(0x010F, 2, 5, b"Test\0".to_vec());
        d.set(0x0110, 2, 6, b"Model\0".to_vec());
        d.set(0x0115, 3, 1, shorts(&[1]));
        d.set(0x0116, 4, 1, longs(&[H]));
        d.set(0x828D, 3, 2, shorts(&[2, 2]));
        d.set(0x828E, 1, 4, vec![0, 1, 1, 2]);
        d.set(0xC612, 1, 4, vec![1, 4, 0, 0]);
        d.set(0xC61A, 4, 1, longs(&[64]));
        d.set(0xC61D, 4, 1, longs(&[16383]));
        d
    }
    fn set(&mut self, tag: u16, typ: u16, count: u32, data: Vec<u8>) {
        self.entries.retain(|e| e.0 != tag);
        self.entries.push((tag, typ, count, data));
    }
    /// Serialise; strip offset/bytecount are filled unless overridden.
    fn build(&self) -> Vec<u8> {
        let mut entries = self.entries.clone();
        let n = entries.len() + 2;
        let ifd_size = 2 + n * 12 + 4;
        let mut extra: Vec<u8> = vec![];
        let extra_base = 8 + ifd_size;
        let total_extra: usize = entries.iter().filter(|e| e.3.len() > 4).map(|e| e.3.len() + (e.3.len() & 1)).sum();
        let payload_off = (extra_base + total_extra) as u32;
        if !entries.iter().any(|e| e.0 == 0x0111) {
            entries.push((0x0111, 4, 1, longs(&[payload_off])));
        }
        if !entries.iter().any(|e| e.0 == 0x0117) {
            entries.push((0x0117, 4, 1, longs(&[self.payload.len() as u32])));
        }
        entries.sort_by_key(|e| e.0);
        let mut out = b"II*\0\x08\0\0\0".to_vec();
        out.extend((entries.len() as u16).to_le_bytes());
        for (tag, typ, count, data) in &entries {
            out.extend(tag.to_le_bytes());
            out.extend(typ.to_le_bytes());
            out.extend(count.to_le_bytes());
            if data.len() <= 4 {
                let mut d = data.clone();
                d.resize(4, 0);
                out.extend(d);
            } else {
                out.extend(((extra_base + extra.len()) as u32).to_le_bytes());
                extra.extend(data);
                if data.len() & 1 == 1 {
                    extra.push(0);
                }
            }
        }
        out.extend(0u32.to_le_bytes());
        // Pad if fewer entries than reserved.
        while out.len() < extra_base {
            out.push(0);
        }
        out.extend(extra);
        while out.len() < payload_off as usize {
            out.push(0);
        }
        out.extend(&self.payload);
        out
    }
}

fn samples() -> Vec<u16> {
    (0..W * H).map(|i| ((i * 37) % 4000 + 64) as u16).collect()
}

fn uncompressed_dng() -> Dng {
    Dng::new(1, 16, samples().iter().flat_map(|v| v.to_le_bytes()).collect())
}

fn ljpeg_dng() -> Dng {
    let jpg = flashraw_codecs_encode(&samples());
    Dng::new(7, 14, jpg)
}

fn flashraw_codecs_encode(s: &[u16]) -> Vec<u8> {
    flashraw::low_level::codecs::ljpeg::encoder::encode(W as usize / 2, H as usize, 2, 14, 1, s)
}

/// Open + metadata + previews + decode + region decode; must not panic or hang.
fn exercise(data: &[u8]) -> Result<(), String> {
    let t = Instant::now();
    let r = catch_unwind(AssertUnwindSafe(|| {
        let limits = Limits::default().with_max_samples(64 << 20).with_max_dimension(65535);
        if let Ok(raw) = FlashRaw::open_with(data, limits) {
            let _ = raw.metadata().camera();
            for p in raw.previews() {
                let _ = raw.preview_bytes(p);
            }
            let opts = DecodeOptions { limits, threads: Some(2), ..Default::default() };
            let _ = raw.decode(opts.clone());
            let _ = raw.decode_region(flashraw::Rect::new(1, 1, 7, 5), opts);
        }
    }));
    if r.is_err() {
        return Err("panicked".into());
    }
    if t.elapsed() > Duration::from_secs(20) {
        return Err(format!("took {:?}", t.elapsed()));
    }
    Ok(())
}

#[test]
fn baseline_fixtures_decode() {
    for (name, d) in [("uncompressed", uncompressed_dng()), ("ljpeg", ljpeg_dng())] {
        let bytes = d.build();
        let raw = FlashRaw::open(&bytes[..]).unwrap_or_else(|e| panic!("{name}: {e}"));
        let f = raw.decode(DecodeOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(f.as_u16().unwrap(), &samples()[..], "{name}");
    }
}

#[test]
fn truncated_payload() {
    for d in [uncompressed_dng(), ljpeg_dng()] {
        let full = d.build();
        for cut in [8, 20, 100, full.len() / 3, full.len() / 2, full.len() - 50, full.len() - 1] {
            exercise(&full[..cut.min(full.len())]).unwrap();
        }
        // A payload truncated mid-stream is never silent garbage: an error under the
        // strict policy, a frame flagged incomplete (with warnings) under the default.
        let raw = FlashRaw::open(&full[..full.len() - 200]).expect("metadata parses without the payload");
        let strict = DecodeOptions { corruption_policy: CorruptionPolicy::Strict, ..Default::default() };
        let e = raw.decode(strict).expect_err("truncated payload must fail under Strict");
        assert!(e.truncation().is_some(), "{e}");
        let f = raw.decode(DecodeOptions::default()).expect("recovered by default");
        assert!(f.is_incomplete() && !f.warnings().is_empty());
    }
}

#[test]
fn invalid_offsets_and_counts() {
    let mut d = uncompressed_dng();
    d.set(0x0111, 4, 1, longs(&[0xFFFF_FFF0]));
    let strict = DecodeOptions { corruption_policy: CorruptionPolicy::Strict, ..Default::default() };
    let r = FlashRaw::open(&d.build()[..]).and_then(|r| r.decode(strict));
    assert!(r.is_err_and(|e| e.truncation().is_some_and(|t| t.starts_beyond_eof())));
    let f = FlashRaw::open(&d.build()[..]).and_then(|r| r.decode(DecodeOptions::default())).unwrap();
    assert!(f.is_incomplete());
    let mut d = uncompressed_dng();
    d.set(0x0117, 4, 1, longs(&[u32::MAX]));
    exercise(&d.build()).unwrap();
    // Strip offsets array pointing past EOF, count claiming 2^30 entries.
    let mut d = uncompressed_dng();
    d.set(0x0111, 4, 0x4000_0000, longs(&[0xFFFF_0000]));
    exercise(&d.build()).unwrap();
}

#[test]
fn recursive_ifds() {
    let mut d = uncompressed_dng();
    d.set(0x014A, 4, 2, longs(&[8, 8]));
    d.set(0x8769, 4, 1, longs(&[8]));
    exercise(&d.build()).unwrap();
    let mut bytes = uncompressed_dng().build();
    // Next-IFD pointer of IFD0 → IFD0 (cycle).
    let n = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    let p = 10 + n * 12;
    bytes[p..p + 4].copy_from_slice(&8u32.to_le_bytes());
    exercise(&bytes).unwrap();
}

#[test]
fn huge_dimensions_and_overflow() {
    for (w, h) in [(u32::MAX, u32::MAX), (65535, 65535), (1 << 20, 1), (0, 10), (100_000, 100_000)] {
        let mut d = uncompressed_dng();
        d.set(0x0100, 4, 1, longs(&[w]));
        d.set(0x0101, 4, 1, longs(&[h]));
        let bytes = d.build();
        exercise(&bytes).unwrap();
        let r = FlashRaw::open(&bytes[..]).and_then(|r| r.decode(DecodeOptions::default()));
        assert!(r.is_err(), "{w}x{h} must be rejected");
    }
    // Tile sizes that overflow tile-count arithmetic.
    let mut d = ljpeg_dng();
    d.set(0x0142, 4, 1, longs(&[1]));
    d.set(0x0143, 4, 1, longs(&[1]));
    d.set(0x0144, 4, 1, longs(&[100]));
    d.set(0x0145, 4, 1, longs(&[10]));
    exercise(&d.build()).unwrap();
}

#[test]
fn invalid_bit_depths() {
    for bps in [0u16, 1, 7, 17, 31, 33, 64, 255, 65535] {
        let mut d = uncompressed_dng();
        d.set(0x0102, 3, 1, shorts(&[bps]));
        exercise(&d.build()).unwrap();
    }
    let mut d = uncompressed_dng();
    d.set(0x0153, 3, 1, shorts(&[3])); // float with 16 bit is valid; 8-bit float isn't
    d.set(0x0102, 3, 1, shorts(&[8]));
    assert!(FlashRaw::open(&d.build()[..]).is_err());
}

#[test]
fn bad_huffman_tables() {
    let mut d = ljpeg_dng();
    let p = d.payload.windows(2).position(|w| w == [0xFF, 0xC4]).unwrap();
    // Over-subscribe the table: set all 16 counts to 255.
    for b in &mut d.payload[p + 5..p + 21] {
        *b = 255;
    }
    exercise(&d.build()).unwrap();
    let mut d = ljpeg_dng();
    let p = d.payload.windows(2).position(|w| w == [0xFF, 0xC4]).unwrap();
    d.payload[p + 2] = 0;
    d.payload[p + 3] = 1; // segment length 1
    exercise(&d.build()).unwrap();
    // Remove the DHT marker entirely.
    let mut d = ljpeg_dng();
    let p = d.payload.windows(2).position(|w| w == [0xFF, 0xC4]).unwrap();
    d.payload[p + 1] = 0xEE;
    exercise(&d.build()).unwrap();
}

#[test]
fn corrupted_entropy_data() {
    let base = ljpeg_dng();
    for seed in 0..40u32 {
        let mut d = ljpeg_dng();
        let start = d.payload.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap() + 14;
        let mut x = seed.wrapping_mul(2654435761) | 1;
        for _ in 0..8 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let i = start + (x as usize % (d.payload.len() - start));
            d.payload[i] ^= (x >> 8) as u8 | 1;
        }
        exercise(&d.build()).unwrap();
    }
    let _ = base;
}

#[test]
fn overlapping_and_shared_data() {
    // Strip pointing into the IFD itself.
    let mut d = uncompressed_dng();
    d.set(0x0111, 4, 1, longs(&[0]));
    exercise(&d.build()).unwrap();
    // Tiles all pointing at the same bytes.
    let mut d = ljpeg_dng();
    let off = 1000u32;
    d.set(0x0142, 4, 1, longs(&[32]));
    d.set(0x0143, 4, 1, longs(&[16]));
    d.set(0x0144, 4, 6, longs(&[off; 6]));
    d.set(0x0145, 4, 6, longs(&[500; 6]));
    exercise(&d.build()).unwrap();
}

#[test]
fn invalid_maker_note_pointer() {
    // EXIF IFD with a maker note pointing outside the file.
    let mut d = uncompressed_dng();
    let mut bytes = d.build();
    let exif_off = bytes.len() as u32;
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(0x927Cu16.to_le_bytes());
    bytes.extend(7u16.to_le_bytes());
    bytes.extend(1_000_000u32.to_le_bytes());
    bytes.extend(0xFFFF_FF00u32.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    d.set(0x8769, 4, 1, longs(&[exif_off]));
    let _ = d;
    exercise(&bytes).unwrap();
}

#[test]
fn invalid_bmff_sizes() {
    // CR3-like ftyp + moov with sizes beyond EOF, zero, 64-bit overflow.
    let mut f = vec![0, 0, 0, 24];
    f.extend(b"ftypcrx \0\0\0\x01crx isom");
    for size in [0u32, 1, 7, 0xFFFF_FFFF, 100] {
        let mut b = f.clone();
        b.extend(size.to_be_bytes());
        b.extend(b"moov");
        if size == 1 {
            b.extend(u64::MAX.to_be_bytes());
        }
        b.extend([0u8; 64]);
        exercise(&b).unwrap();
    }
}

#[test]
fn random_garbage_and_magic_prefixes() {
    let magics: &[&[u8]] = &[
        b"II*\0",
        b"MM\0*",
        b"IIRO",
        b"IIRS",
        b"MMOR",
        b"IIU\0",
        b"FUJIFILMCCD-RAW ",
        b"FOVb",
        b"\0MRM",
        b"IIII",
        b"II\x1a\0\0\0HEAPCCDR",
        b"\0\0\0\x18ftypcrx ",
        b"\xFF\xD8\xFF\xE1",
    ];
    let mut x: u64 = 0x9E3779B97F4A7C15;
    for m in magics {
        for len in [16usize, 64, 512, 4096, 70_000] {
            let mut v = m.to_vec();
            while v.len() < len {
                x ^= x << 7;
                x ^= x >> 9;
                v.push(x as u8);
            }
            exercise(&v).unwrap();
        }
    }
}

/// Mutations of real files (all formats) when the corpus is present.
#[test]
fn corpus_mutations() {
    let corpus = std::env::var("FLASHRAW_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    if !corpus.exists() {
        eprintln!("corpus not present; skipping");
        return;
    }
    // One small-ish file per extension.
    let mut by_ext: std::collections::BTreeMap<String, std::path::PathBuf> = Default::default();
    let mut stack = vec![corpus];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Some(ext) = p.extension().map(|x| x.to_string_lossy().to_lowercase()) {
                let size = e.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
                if size < 30_000_000 {
                    let better = by_ext
                        .get(&ext)
                        .map(|q| std::fs::metadata(q).map(|m| m.len()).unwrap_or(0) > size)
                        .unwrap_or(true);
                    if better {
                        by_ext.insert(ext, p);
                    }
                }
            }
        }
    }
    let mut failures = vec![];
    for (ext, p) in &by_ext {
        let data = std::fs::read(p).unwrap();
        let len = data.len();
        let mut variants: Vec<(String, Vec<u8>)> = vec![];
        for frac in [0.01, 0.1, 0.5, 0.9, 0.999] {
            variants.push((format!("truncate {frac}"), data[..(len as f64 * frac) as usize].to_vec()));
        }
        let mut x: u64 = 12345;
        for k in 0..6 {
            let mut v = data.clone();
            // Corrupt the header region (first 64 KB) where structure lives.
            for _ in 0..16 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let i = (x as usize) % len.min(65536);
                v[i] = (x >> 32) as u8;
            }
            variants.push((format!("header-corrupt {k}"), v));
        }
        let mut v = data.clone();
        for b in v[len / 2..].iter_mut() {
            *b = 0xFF;
        }
        variants.push(("payload 0xFF".into(), v));
        for (what, v) in variants {
            if let Err(e) = exercise(&v) {
                failures.push(format!("{ext} {} [{what}]: {e}", p.display()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Big-endian opcode list with one opcode.
fn opcode_list(id: u32, body: &[u8]) -> Vec<u8> {
    let mut v = 1u32.to_be_bytes().to_vec();
    for x in [id, 0x0103_0000, 0, body.len() as u32] {
        v.extend(x.to_be_bytes());
    }
    v.extend(body);
    v
}

fn whole_region() -> Vec<u8> {
    [0, 0, H, W, 0, 1, 1, 1].iter().flat_map(|v: &u32| v.to_be_bytes()).collect()
}

#[test]
fn dng_opcode_lists() {
    // List 1: MapTable doubling every stored value.
    let mut body = whole_region();
    body.extend(65536u32.to_be_bytes());
    for i in 0..65536u32 {
        body.extend(((i * 2).min(65535) as u16).to_be_bytes());
    }
    let mut d = uncompressed_dng();
    d.set(0xC740, 7, 0, opcode_list(7, &body));
    let n = d.entries.len() - 1;
    d.entries[n].2 = d.entries[n].3.len() as u32;
    let bytes = d.build();
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let on = raw.decode(DecodeOptions::default()).unwrap();
    let off = raw.decode(DecodeOptions { apply_opcodes: false, ..Default::default() }).unwrap();
    let (a, b) = (on.as_u16().unwrap(), off.as_u16().unwrap());
    assert!(a.iter().zip(b).all(|(x, y)| *x == *y * 2)); // samples stay below 32768
    assert!(on.stats.opcodes.iter().any(|n| n.contains("applied MapTable")), "{:?}", on.stats.opcodes);
    // Region decodes use whole-buffer coordinates.
    let r = raw.decode_region(flashraw::Rect::new(3, 2, 5, 4), DecodeOptions::default()).unwrap();
    assert_eq!(r.as_u16().unwrap()[0], b[2 * W as usize + 3] * 2);

    // List 2: rows scaled by 0.5 on normalised output; raw output untouched.
    let mut body = whole_region();
    body.extend(H.to_be_bytes());
    for _ in 0..H {
        body.extend(0.5f32.to_bits().to_be_bytes());
    }
    let mut d = uncompressed_dng();
    d.set(0xC741, 7, 0, opcode_list(12, &body));
    let n = d.entries.len() - 1;
    d.entries[n].2 = d.entries[n].3.len() as u32;
    let bytes = d.build();
    let raw = FlashRaw::open(&bytes[..]).unwrap();
    let norm = |apply| {
        let f = raw
            .decode(DecodeOptions { output: OutputFormat::NormalizedF32, apply_opcodes: apply, ..Default::default() })
            .unwrap();
        match f.data {
            RawPixelData::F32(v) => v,
            _ => unreachable!(),
        }
    };
    let (a, b) = (norm(true), norm(false));
    assert!(a.iter().zip(&b).all(|(x, y)| (x - y * 0.5).abs() < 1e-6));
    let raw_out = raw.decode(DecodeOptions::default()).unwrap();
    assert_eq!(raw_out.as_u16().unwrap(), &samples()[..]);
    // RGB output takes the staged path and still succeeds.
    raw.decode(DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() }).unwrap();

    // Garbage lists are reported and ignored.
    let mut d = uncompressed_dng();
    d.set(0xC740, 7, 12, vec![0, 0, 0, 9, 1, 2, 3, 4, 5, 6, 7, 8]);
    let bytes = d.build();
    let f = FlashRaw::open(&bytes[..]).unwrap().decode(DecodeOptions::default()).unwrap();
    assert_eq!(f.as_u16().unwrap(), &samples()[..]);
    assert!(f.stats.opcodes.iter().any(|n| n.contains("OpcodeList1: not applied")), "{:?}", f.stats.opcodes);
    exercise(&bytes).unwrap();
}
