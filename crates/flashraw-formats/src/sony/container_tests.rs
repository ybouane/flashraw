//! Tests of the Sony container reader on synthetic containers (small TIFFs built here) and, ignored,
//! on real corpus files against recorded hashes of everything the reader reports.

use super::*;
use crate::OpenOptions;
use flashraw_core::tiff::tags;

// ---------------------------------------------------------------------------------------------------------
// A little-endian TIFF builder: one IFD0, optional extra IFDs appended by the caller.

struct Ent {
    tag: u16,
    typ: u16,
    count: u32,
    value: Vec<u8>,
}

fn short(tag: u16, v: &[u16]) -> Ent {
    Ent { tag, typ: 3, count: v.len() as u32, value: v.iter().flat_map(|x| x.to_le_bytes()).collect() }
}
fn long(tag: u16, v: &[u32]) -> Ent {
    Ent { tag, typ: 4, count: v.len() as u32, value: v.iter().flat_map(|x| x.to_le_bytes()).collect() }
}
fn ascii(tag: u16, s: &str) -> Ent {
    let mut value = s.as_bytes().to_vec();
    value.push(0);
    Ent { tag, typ: 2, count: value.len() as u32, value }
}

/// A TIFF whose IFD0 holds `entries` (sorted by tag here); the payload follows the directory and is what
/// StripOffsets (0x111) points at. Returns the bytes and the payload offset.
fn tiff(mut entries: Vec<Ent>, payload: &[u8]) -> (Vec<u8>, usize) {
    entries.sort_by_key(|e| e.tag);
    let dir_end = 8 + 2 + 12 * entries.len() + 4;
    let big: usize = entries.iter().map(|e| e.value.len().next_multiple_of(2)).filter(|&n| n > 4).sum();
    let payload_at = dir_end + big;
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend((entries.len() as u16).to_le_bytes());
    let mut tail: Vec<u8> = Vec::new();
    for e in &entries {
        out.extend(e.tag.to_le_bytes());
        out.extend(e.typ.to_le_bytes());
        out.extend(e.count.to_le_bytes());
        if e.tag == tags::STRIP_OFFSETS {
            out.extend((payload_at as u32).to_le_bytes());
        } else if e.value.len() <= 4 {
            let mut v = e.value.clone();
            v.resize(4, 0);
            out.extend(v);
        } else {
            out.extend(((dir_end + tail.len()) as u32).to_le_bytes());
            tail.extend(&e.value);
            if tail.len() % 2 == 1 {
                tail.push(0);
            }
        }
    }
    out.extend(0u32.to_le_bytes());
    assert_eq!(out.len(), dir_end);
    out.extend(tail);
    assert_eq!(out.len(), payload_at);
    out.extend(payload);
    (out, payload_at)
}

/// The raw-IFD description of a `w x h` image: compression, bits, samples, byte count (payload is zeros).
struct Raw {
    model: &'static str,
    w: u32,
    h: u32,
    comp: u16,
    bps: u16,
    samples: u16,
    photometric: u16,
    bytes: usize,
    extra: Vec<Ent>,
}

impl Raw {
    fn new(model: &'static str, w: u32, h: u32, comp: u16, bps: u16, bytes: usize) -> Raw {
        Raw { model, w, h, comp, bps, samples: 1, photometric: 32803, bytes, extra: Vec::new() }
    }
    fn file(self) -> Vec<u8> {
        let mut e = vec![
            ascii(tags::MAKE, "SONY"),
            ascii(tags::MODEL, self.model),
            long(tags::IMAGE_WIDTH, &[self.w]),
            long(tags::IMAGE_LENGTH, &[self.h]),
            short(tags::BITS_PER_SAMPLE, &[self.bps]),
            short(tags::COMPRESSION, &[self.comp]),
            short(tags::PHOTOMETRIC, &[self.photometric]),
            short(tags::SAMPLES_PER_PIXEL, &[self.samples]),
            long(tags::STRIP_OFFSETS, &[0]),
            long(tags::STRIP_BYTE_COUNTS, &[self.bytes as u32]),
        ];
        e.extend(self.extra);
        tiff(e, &vec![0u8; self.bytes]).0
    }
}

fn open_file(bytes: &[u8]) -> Result<Box<dyn RawDecoder + '_>> {
    crate::open(bytes, &OpenOptions::default())
}

fn kind_of(file: &[u8]) -> (String, Compression, (u32, u32)) {
    let dec = open_file(file).expect("opens");
    let info = dec.frame_info();
    (dec.metadata().extra("mode").unwrap_or("").to_string(), info.compression, (info.width, info.height))
}

// ---------------------------------------------------------------------------------------------------------
// Payload classification

#[test]
fn classification_by_compression_and_byte_count() {
    let (w, h) = (4000u32, 4u32);
    let area = (w * h) as usize;
    let mk = |comp, bps, bytes| Raw::new("ZZ-TEST", w, h, comp, bps, bytes).file();
    // (compression, bps, bytes) -> mode, compression, buffer height
    let cases = [
        (32767, 12, area, "arw2", Compression::SonyArw2, h),
        (32767, 14, 2 * area, "uncompressed", Compression::Uncompressed, h),
        (1, 14, 2 * area, "uncompressed", Compression::Uncompressed, h),
        (1, 16, 12345, "uncompressed", Compression::Uncompressed, h),
        (32767, 12, 1234, "arw1", Compression::SonyArw1, h + 8),
        (32767, 12, area * 3 / 2, "12bit-packed", Compression::Uncompressed, h),
        (1, 12, area * 3 / 2, "12bit-packed", Compression::Uncompressed, h),
    ];
    for (comp, bps, bytes, mode, compression, height) in cases {
        let got = kind_of(&mk(comp, bps, bytes));
        assert_eq!(got, (mode.to_string(), compression, (w, height)), "comp {comp} bps {bps} bytes {bytes}");
    }
    let mut arq = Raw::new("ZZ-TEST", w, h, 1, 16, 8 * area);
    arq.samples = 4;
    assert_eq!(kind_of(&arq.file()).0, "arq");
    let wavelet = Raw::new("ZZ-TEST", w, h, 32766, 14, 100).file();
    assert_eq!(kind_of(&wavelet), ("arw6".to_string(), Compression::Other, (w, h)));
}

#[test]
fn unsupported_and_invalid_combinations() {
    let unsupported = Raw::new("ZZ-TEST", 64, 64, 99, 14, 100).file();
    assert_eq!(open_file(&unsupported).err().map(|e| e.kind()), Some("UnsupportedCompression"));
    // Lossless JPEG without a tile description.
    let untiled = Raw::new("ZZ-TEST", 64, 64, 7, 14, 100).file();
    assert_eq!(open_file(&untiled).err().map(|e| e.kind()), Some("InvalidMetadata"));
    let mut ycc = Raw::new("ZZ-TEST", 64, 64, 7, 8, 100);
    ycc.samples = 3;
    ycc.photometric = 6;
    ycc.bps = 14;
    assert_eq!(open_file(&ycc.file()).err().map(|e| e.kind()), Some("InvalidMetadata"));
    // No image directory at all.
    let (nothing, _) = tiff(vec![ascii(tags::MAKE, "SONY"), ascii(tags::MODEL, "ZZ-TEST")], &[]);
    assert_eq!(open_file(&nothing).err().map(|e| e.kind()), Some("InvalidMetadata"));
}

#[test]
fn limits_apply_to_the_buffer() {
    let file = Raw::new("ZZ-TEST", 4000, 4, 32767, 12, 16000).file();
    let tight = OpenOptions { limits: Limits::default().with_max_dimension(3000) };
    assert_eq!(crate::open(&file, &tight).err().map(|e| e.kind()), Some("LimitExceeded"));
    // Four samples per pixel count for the composite.
    let mut arq = Raw::new("ZZ-TEST", 4000, 4, 1, 16, 8 * 16000);
    arq.samples = 4;
    let few = OpenOptions { limits: Limits::default().with_max_samples(40_000) };
    assert_eq!(crate::open(&arq.file(), &few).err().map(|e| e.kind()), Some("LimitExceeded"));
}

// ---------------------------------------------------------------------------------------------------------
// Levels, white balance, geometry

fn arw2(w: u32, extra: Vec<Ent>) -> Vec<u8> {
    let mut r = Raw::new("ZZ-TEST", w, 2, 32767, 12, 2 * w as usize);
    r.extra = extra;
    r.file()
}

#[test]
fn levels_from_the_raw_ifd() {
    let file = arw2(
        4000,
        vec![
            short(0x7310, &[600, 601, 603, 602]),
            short(0x7313, &[2000, 1000, 1000, 3000]),
            long(tags::WHITE_LEVEL, &[16000]),
        ],
    );
    let dec = open_file(&file).unwrap();
    let info = dec.frame_info();
    // 0x7310 is R, G1, G2, B; the pattern follows the RGGB cell.
    assert_eq!(info.black_level.values, vec![600.0, 601.0, 603.0, 602.0]);
    assert_eq!(info.white_level.values, vec![16000]);
    let wb = info.white_balance.as_ref().expect("white balance");
    assert_eq!(wb.coeffs, [2.0, 1.0, 3.0, 1.0]);
}

#[test]
fn default_levels() {
    // ARW2 without a black tag: 512 on wide sensors; a stated black is kept at any width.
    let black = |w: u32, extra: Vec<Ent>| open_file(&arw2(w, extra)).unwrap().frame_info().black_level.values.clone();
    assert_eq!(black(4000, vec![]), vec![512.0]);
    assert_eq!(black(3888, vec![]), vec![0.0]);
    assert_eq!(black(4600, vec![short(0x7310, &[700, 700, 700, 700])]), vec![700.0]);
    let white = |w: u32| open_file(&arw2(w, vec![])).unwrap().frame_info().white_level.values.clone();
    assert_eq!(white(4000), vec![16383]);
    // Other payloads get no default.
    let packed = Raw::new("ZZ-TEST", 4000, 2, 32767, 12, 12000).file();
    assert_eq!(open_file(&packed).unwrap().frame_info().black_level.values, vec![0.0]);
    let sixteen = Raw::new("ZZ-TEST", 4000, 2, 1, 16, 5).file();
    assert_eq!(open_file(&sixteen).unwrap().frame_info().black_level.values, vec![0.0]);
}

#[test]
fn level_conventions_of_special_payloads() {
    // 12-bit packed data with levels in 14-bit units is brought down to 12 bits.
    let mut packed = Raw::new("ZZ-TEST", 4000, 2, 32767, 12, 12000);
    packed.extra = vec![short(0x7310, &[512, 512, 512, 512]), long(tags::WHITE_LEVEL, &[16383])];
    let info = open_file(&packed.file()).unwrap().frame_info().clone();
    assert_eq!((info.black_level.values.clone(), info.white_level.values.clone()), (vec![128.0], vec![4095]));
    // The wavelet format has fixed levels.
    let wavelet = Raw::new("ZZ-TEST", 4000, 2, 32766, 14, 100).file();
    let info = open_file(&wavelet).unwrap().frame_info().clone();
    assert_eq!((info.black_level.values.clone(), info.white_level.values.clone()), (vec![1024.0], vec![39002]));
}

#[test]
fn tags_with_too_few_values_are_ignored() {
    let file = arw2(4000, vec![short(0x7310, &[1, 2, 3]), short(0x7313, &[1, 1, 1])]);
    let dec = open_file(&file).unwrap();
    assert!(dec.frame_info().white_balance.is_none());
    assert_eq!(dec.frame_info().black_level.values, vec![512.0]);
}

#[test]
fn implausible_white_balance_is_dropped() {
    for v in [[0u16, 1000, 1000, 1000], [1000, 1000, 1000, 1], [65535, 1000, 1000, 500]] {
        let file = arw2(4000, vec![short(0x7313, &v)]);
        assert!(open_file(&file).unwrap().frame_info().white_balance.is_none(), "{v:?}");
    }
}

#[test]
fn visible_area_and_crop() {
    let info_of = |model: &'static str, w: u32, extra: Vec<Ent>| {
        let mut r = Raw::new(model, w, 8, 32767, 12, 8 * w as usize);
        r.extra = extra;
        open_file(&r.file()).unwrap().frame_info().clone()
    };
    // The 24 MP bodies' 6048-wide buffers lose 24 columns.
    let i = info_of("ILCE-6000", 6048, vec![]);
    assert_eq!((i.active_area, i.crop), (Rect::new(0, 0, 6024, 8), Rect::new(0, 0, 6024, 8)));
    // The file's own crop rectangle is intersected with the area.
    let crop = vec![long(0x74C7, &[4, 2]), long(0x74C8, &[100, 4])];
    let i = info_of("ILCE-6000", 6048, crop);
    assert_eq!(i.crop, Rect::new(4, 2, 100, 4));
    // A crop outside the area is ignored.
    let far = vec![long(0x74C7, &[7000, 0]), long(0x74C8, &[10, 4])];
    assert_eq!(info_of("ILCE-6000", 6048, far).crop, Rect::new(0, 0, 6024, 8));
    // A zero-sized crop is not a crop.
    let zero = vec![long(0x74C7, &[1, 1]), long(0x74C8, &[0, 4])];
    assert_eq!(info_of("ILCE-6000", 6048, zero).crop, Rect::new(0, 0, 6024, 8));
}

#[test]
fn arq_buffers_lose_a_fixed_margin() {
    let mut arq = Raw::new("ZZ-TEST", 4000, 2, 1, 16, 8 * 8000);
    arq.samples = 4;
    let file = arq.file();
    let dec = open_file(&file).unwrap();
    let info = dec.frame_info();
    assert_eq!((info.cpp, info.cfa.is_none(), info.layout), (4, true, SensorLayout::Linear));
    assert_eq!(info.active_area, Rect::new(0, 0, 3968, 2));
}

#[test]
fn arw1_stores_fewer_rows_than_it_codes() {
    // The A350 family: 4600 wide, the last rows of the buffer are not image.
    let file = Raw::new("DSLR-A350", 4600, 3064, 32767, 12, 1000).file();
    let dec = open_file(&file).unwrap();
    let info = dec.frame_info();
    assert_eq!((info.width, info.height), (4600, 3072));
    assert_eq!(info.active_area, Rect::new(0, 0, 4600, 3068));
    assert_eq!(info.black_level.values, vec![0.0]);
}

// ---------------------------------------------------------------------------------------------------------
// The encrypted SR2 private directory

/// A file whose levels live in an encrypted SR2 block; returns it with the block's file offset.
fn with_sr2_block(plain: &[u8], key: u32) -> Vec<u8> {
    let make = |private_at: u32| {
        let mut r = Raw::new("ZZ-TEST", 4000, 2, 32767, 12, 8000);
        r.extra = vec![Ent { tag: tags::DNG_PRIVATE_DATA, typ: 1, count: 4, value: private_at.to_le_bytes().to_vec() }];
        r.file()
    };
    let mut file = make(0);
    let private_at = file.len() as u32;
    file = make(private_at);
    let block_at = private_at + 2 + 3 * 12 + 4;
    file.extend(3u16.to_le_bytes());
    for (tag, v) in [(0x7200u16, block_at), (0x7201, plain.len() as u32), (0x7221, key)] {
        file.extend(tag.to_le_bytes());
        file.extend(4u16.to_le_bytes());
        file.extend(1u32.to_le_bytes());
        file.extend(v.to_le_bytes());
    }
    file.extend(0u32.to_le_bytes());
    assert_eq!(file.len() as u32, block_at);
    let mut block = plain.to_vec();
    crypt::decrypt(&mut block, key);
    file.extend(block);
    file
}

/// Plaintext SR2 directory (little endian) whose entries are `(tag, four SHORT values)`; the values sit
/// behind the entries and are addressed by absolute file offset.
fn sr2_plain(entries: &[(u16, [u16; 4])], block_at: u32) -> Vec<u8> {
    let mut out = (entries.len() as u16).to_le_bytes().to_vec();
    let values_at = 2 + 12 * entries.len() as u32;
    for (i, (tag, _)) in entries.iter().enumerate() {
        out.extend(tag.to_le_bytes());
        out.extend(3u16.to_le_bytes());
        out.extend(4u32.to_le_bytes());
        out.extend((block_at + values_at + 8 * i as u32).to_le_bytes());
    }
    for (_, v) in entries {
        out.extend(v.iter().flat_map(|x| x.to_le_bytes()));
    }
    out
}

#[test]
fn levels_from_the_sr2_block() {
    // The block offset is only known after the file is laid out: build once to learn it.
    let probe = with_sr2_block(&[0; 2], 1);
    let block_at = probe.len() as u32 - 2;
    let entries = [(0x7300u16, [10u16, 20, 30, 40]), (0x7303, [1000, 2000, 1000, 3000])];
    let file = with_sr2_block(&sr2_plain(&entries, block_at), 0x1234_5678);
    let dec = open_file(&file).unwrap();
    let info = dec.frame_info();
    // 0x7300 states the offsets R, G, B, G2 (no base level, and the offsets are non-zero: no default applies).
    assert_eq!(info.black_level.values, vec![10.0, 20.0, 40.0, 30.0]);
    // 0x7303 is G, R, B, G2.
    assert_eq!(info.white_balance.as_ref().unwrap().coeffs, [2.0, 1.0, 1.0, 3.0]);
}

#[test]
fn damaged_sr2_block_is_ignored() {
    let probe = with_sr2_block(&[0; 2], 1);
    let block_at = probe.len() as u32 - 2;
    let entries = [(0x7300u16, [10u16, 20, 30, 40])];
    let mut file = with_sr2_block(&sr2_plain(&entries, block_at), 7);
    // Cut the file inside the block: the block lies outside the file, there are no SR2 levels.
    file.truncate(file.len() - 3);
    let dec = open_file(&file).unwrap();
    assert_eq!(dec.frame_info().black_level.values, vec![512.0]);
}

// ---------------------------------------------------------------------------------------------------------
// Pure rules

#[test]
fn camera_image_size_rule() {
    // A 16:9 shot on a 3:2 sensor: the EXIF size is the image.
    assert_eq!(camera_image_size((4912, 3264), Some((4912, 2760))), (4912, 2760));
    // A padded EXIF size of the same ratio: the maker note's size wins.
    assert_eq!(camera_image_size((4592, 3056), Some((4600, 3064))), (4592, 3056));
    assert_eq!(camera_image_size((6000, 4000), None), (6000, 4000));
    // Never larger than the native size.
    assert_eq!(camera_image_size((3000, 2000), Some((8000, 2000))), (3000, 2000));
}

#[test]
fn black_pattern_follows_the_cfa_cell() {
    let cb = [1, 2, 3, 4]; // R, G, B, G2 offsets
    let value = |cfa: &CfaPattern| cfa_black(cfa, 100, &cb).values;
    assert_eq!(value(&CfaPattern::RGGB), vec![101.0, 102.0, 104.0, 103.0]);
    assert_eq!(value(&CfaPattern::GRBG), vec![102.0, 101.0, 103.0, 104.0]);
    let uniform = cfa_black(&CfaPattern::RGGB, 512, &[0; 4]);
    assert_eq!((uniform.rows, uniform.values), (1, vec![512.0]));
    // Equal green offsets only: still a pattern.
    assert_eq!(cfa_black(&CfaPattern::RGGB, 0, &[1, 5, 9, 5]).rows, 2);
}

// ---------------------------------------------------------------------------------------------------------
// Real files (ignored: need the corpus)

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// FNV-1a of everything the reader reports about a file: metadata, frame description, previews.
fn report_hash(bytes: &[u8]) -> u64 {
    let dec = open_file(bytes).expect("opens");
    fnv(&format!("{:?}\n{:?}", dec.metadata(), dec.frame_info()))
}

/// Hashes recorded from the reader's output on public corpus files (relative to the corpus root); they pin
/// body identification, payload class, geometry, levels, white balance, crop, colour data and previews.
/// Run: `cargo test --release -p flashraw-formats --lib sony::container_tests -- --ignored`
/// (`SONY_GOLDEN_PRINT=1` prints fresh values).
const GOLDEN: &[(&str, u64)] = &[
    ("SONY/DSC-RX100M7A/RX700064.ARW", 0x7159cd59420578f4),
    ("SONY/DSC-RX1RM3/DSC00254.ARW", 0x0995b4fd2cda0037),
    ("SONY/ILCE-1M2/A1_07627_lossless_compressed_large_full_frame.ARW", 0xeb4271306f8d6d4c),
    ("SONY/ILCE-1M2/A1_07630_lossy_compressed_full_frame.ARW", 0x4b6406c0d6d0e567),
    ("SONY/ILCE-1M2/A1_07633_lossless_compressed_small_apsc.ARW", 0x87e9d048aef2205d),
    ("SONY/ILCE-6400A/DSC04880.ARW", 0x53f08837c4304fab),
    ("SONY/ILCE-7M5/apcs_compressed_hq.ARW", 0x7e8ed9c0a4aa64a1),
    ("SONY/ILCE-7M5/full_compressed.ARW", 0xf245b0b0bc05883d),
    ("SONY/ILME-FX2/Image_1_-_Uncompressed_-_Landscape.ARW", 0x91a2e0762aa95a2a),
    ("SONY/ILME-FX2/Image_5_-_Lossless_Compression_(S)_-_Shade.ARW", 0x9b5f446de3361e02),
    ("SONY/ZV-E10M2/DSC00696.ARW", 0x5e21360acee8e10f),
    ("Sony/DSC-F828/DSC06227.SRF", 0x3f71c4425a80fde9),
    ("Sony/DSC-R1/_DSC1477.SR2", 0x2b817304408f2517),
    ("Sony/DSC-RX0/DSC00009.ARW", 0x5422e2ef7b7e676a),
    ("Sony/DSC-RX1/_DSC0812.ARW", 0x9947b2aa199047c2),
    ("Sony/DSLR-A100/_DSC0258.ARW", 0x63a01954c0538ecc),
    ("Sony/DSLR-A330/RAW_SONY_A330.ARW", 0x3bcda7163372abb8),
    ("Sony/DSLR-A350/DSC05472.ARW", 0x43a6d88e27963ba4),
    ("Sony/DSLR-A500/DSC02421.ARW", 0xf5d445939befb391),
    ("Sony/DSLR-A580/RAW_SONY_A580.ARW", 0x7609cfb467b759fb),
    ("Sony/DSLR-A850/RAW_SONY_A850.ARW", 0xd7347b22d70f8f63),
    ("Sony/ILCE-3000/_DSC3941.ARW", 0x3f1bf4e4c295fc69),
    ("Sony/ILCE-7CR/DSC00795.ARW", 0xa442118ef1a71ee2),
    ("Sony/ILCE-7R/DSC06741.ARW", 0x1706b8830e7cfe70),
    ("Sony/ILCE-7S/DSC04126.ARW", 0xcd839b6eb3e63694),
    ("Sony/ILCE-9M3/DSC00111.ARW", 0x3b3c2eccd2b271cc),
    ("Sony/NEX-3/RAW_SONY_NEX3.ARW", 0x9b5f2ebbedea15e7),
    ("Sony/SLT-A33/DSC01867.ARW", 0x3578ed6204d879b2),
    ("Sony/UMC-R10C/20240501120008.arw", 0xdc0e24c206b64aa3),
    ("Sony/ZV-E1/DSC00489.ARW", 0xc36c00fef909dd20),
];

#[test]
#[ignore = "needs the corpus (FLASHRAW_CORPUS or ./corpus)"]
fn golden_reports_of_real_files() {
    let root =
        std::env::var("FLASHRAW_CORPUS").unwrap_or_else(|_| format!("{}/../../corpus", env!("CARGO_MANIFEST_DIR")));
    let print = std::env::var_os("SONY_GOLDEN_PRINT").is_some();
    let mut missing = Vec::new();
    for &(rel, want) in GOLDEN {
        let Ok(bytes) = std::fs::read(format!("{root}/{rel}")) else {
            missing.push(rel);
            continue;
        };
        let got = report_hash(&bytes);
        if print {
            println!("    (\"{rel}\", {got:#018x}),");
        } else {
            assert_eq!(got, want, "{rel}");
        }
    }
    assert!(missing.is_empty(), "files missing from the corpus: {missing:?}");
}
