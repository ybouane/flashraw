//! Tests built from the format description: unit tests of the parsing rules, synthetic
//! containers, mutation robustness, and golden summaries of real files (corpus needed).

use super::*;
use flashraw_core::Profiler;

// ---- synthetic container builder ---------------------------------------------------------

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}

fn record(tag: u16, body: &[u8]) -> Vec<u8> {
    let mut r = be16(tag).to_vec();
    r.extend(be16(body.len() as u16));
    r.extend_from_slice(body);
    r
}

fn size_record(tag: u16, a: u16, b: u16) -> Vec<u8> {
    record(tag, &[be16(a), be16(b)].concat())
}

/// A little-endian TIFF section whose IFD0 points at a raw IFD with LONG entries.
fn tiff_section(entries: &[(u16, u32)], payload: &[u8], payload_offset_field: Option<u32>) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort();
    let raw_ifd_at = 8 + 2 + 12 + 4;
    let data_at = raw_ifd_at + 2 + (sorted.len() + 1) * 12 + 4;
    let mut t = b"II*\0".to_vec();
    t.extend(8u32.to_le_bytes());
    t.extend(1u16.to_le_bytes());
    for v in [
        0xF000u16.to_le_bytes().to_vec(),
        4u16.to_le_bytes().to_vec(),
        1u32.to_le_bytes().to_vec(),
        (raw_ifd_at as u32).to_le_bytes().to_vec(),
    ] {
        t.extend(v);
    }
    t.extend(0u32.to_le_bytes());
    t.extend((sorted.len() as u16 + 1).to_le_bytes());
    let mut all = sorted;
    all.push((0xF007, payload_offset_field.unwrap_or(data_at as u32)));
    all.sort();
    for (tag, v) in &all {
        t.extend(tag.to_le_bytes());
        t.extend(4u16.to_le_bytes());
        t.extend(1u32.to_le_bytes());
        t.extend(v.to_le_bytes());
    }
    t.extend(0u32.to_le_bytes());
    assert_eq!(t.len(), data_at);
    t.extend_from_slice(payload);
    t
}

fn raf(model: &str, records: &[Vec<u8>], section: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 0x94];
    f[..15].copy_from_slice(b"FUJIFILMCCD-RAW");
    f[0x1C..0x1C + model.len()].copy_from_slice(model.as_bytes());
    f[0x3C..0x40].copy_from_slice(b"0201");
    let mut dir = (records.len() as u32).to_be_bytes().to_vec();
    for r in records {
        dir.extend_from_slice(r);
    }
    let dir_at = f.len();
    f.extend(dir);
    while f.len() % 16 != 0 {
        f.push(0);
    }
    let section_at = f.len();
    f[0x54..0x58].copy_from_slice(&0x94u32.to_be_bytes());
    f[0x5C..0x60].copy_from_slice(&(dir_at as u32).to_be_bytes());
    f[0x64..0x68].copy_from_slice(&(section_at as u32).to_be_bytes());
    f.extend_from_slice(section);
    f
}

fn words_le(n: usize) -> Vec<u8> {
    (0..n).flat_map(|i| (i as u16).wrapping_mul(7).to_le_bytes()).collect()
}

fn decode(dec: &dyn RawDecoder, recover: bool) -> Result<RawImage> {
    let p = Profiler::new();
    let mut ctx = DecodeContext::new(&p);
    ctx.recover = recover;
    dec.decode(&ctx)
}

fn open_default(data: &[u8]) -> Result<Box<dyn RawDecoder + '_>> {
    open(data, &OpenOptions::default())
}

// ---- header and signature -----------------------------------------------------------------

#[test]
fn signature_and_header_errors() {
    assert!(probe(b"FUJIFILMCCD-RAW 0201"));
    assert!(!probe(b"FUJIFILMCCD-RA"));
    assert!(!probe(b""));
    let short = &b"FUJIFILMCCD-RAW 0201"[..];
    assert_eq!(open_default(short).err().map(|e| e.kind()), Some("CorruptFile"));
    assert!(open_tiff_error());
}

fn open_tiff_error() -> bool {
    let tiff_bytes = tiff_section(&[], &[], None);
    let tiff = Tiff::parse(&tiff_bytes).unwrap();
    matches!(open_tiff(&tiff_bytes, tiff, &OpenOptions::default()), Err(Error::UnsupportedFormat(_)))
}

#[test]
fn bare_samples_need_a_raw_size_record() {
    let f = raf("FinePix S5600", &[], &words_le(100));
    assert_eq!(open_default(&f).err().map(|e| e.kind()), Some("InvalidMetadata"));
}

#[test]
fn raw_size_below_22_is_rejected() {
    let f = raf("FinePix S5600", &[size_record(0x100, 30, 21)], &words_le(1000));
    assert_eq!(open_default(&f).err().map(|e| e.kind()), Some("InvalidMetadata"));
}

#[test]
fn limits_apply_to_the_raw_size() {
    let f = raf("FinePix S5600", &[size_record(0x100, 100, 100)], &words_le(10000));
    let opts = OpenOptions { limits: Limits::default().with_max_dimension(50) };
    assert_eq!(open(&f, &opts).err().map(|e| e.kind()), Some("LimitExceeded"));
}

// ---- bare samples (form B) ----------------------------------------------------------------

#[test]
fn bare_words_decode_in_little_endian_without_exif() {
    let (w, h) = (40u16, 30u16);
    let f = raf("FinePix S5600", &[size_record(0x100, h, w)], &words_le(40 * 30));
    let dec = open_default(&f).unwrap();
    assert_eq!(dec.format(), RawFormat::Raf);
    let info = dec.frame_info();
    assert_eq!((info.width, info.height), (40, 30));
    assert_eq!(info.compression, Compression::Uncompressed);
    assert_eq!(dec.random_access(), RandomAccess::Rows);
    assert_eq!(dec.metadata().make, "FUJIFILM");
    assert_eq!(dec.metadata().model, "FinePix S5600");
    let img = decode(&*dec, false).unwrap();
    assert_eq!(img.u16()[..4], [0, 7, 14, 21]);
    let p = dec.packed_payload().unwrap();
    assert_eq!((p.bits, p.row_stride, p.order), (16, 80, PackOrder::LsbBytes));
    // Without a TIFF the depth is not stated: a white level over 4095 means 14 bits.
    assert_eq!(dec.frame_info().bits_per_sample, 14);
}

#[test]
fn truncated_bare_words_recover_or_fail() {
    let mut f = raf("FinePix S5600", &[size_record(0x100, 30, 40)], &words_le(40 * 30));
    f.truncate(f.len() - 101); // 50 samples and one odd byte
    let dec = open_default(&f).unwrap();
    assert_eq!(decode(&*dec, false).err().map(|e| e.kind()), Some("CorruptFile"));
    let img = decode(&*dec, true).unwrap();
    let px = img.u16();
    assert_eq!(px[40 * 30 - 52], ((40 * 30 - 52) as u16).wrapping_mul(7));
    assert!(px[40 * 30 - 51..].iter().all(|&v| v == 0));
    // Region decode reports the zeroed samples inside the window only.
    let p = Profiler::new();
    let ctx = DecodeContext::new(&p);
    let r = dec.decode_region(&ctx, Rect::new(10, 28, 10, 2)).unwrap();
    assert_eq!((r.width, r.height), (10, 2));
    let warnings = ctx.take_warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].samples_filled, 10);
}

#[test]
fn dual_frame_models_keep_the_first_half_of_each_row() {
    // F700-style: second-frame pointer set, layout flag set, rows hold both exposures.
    let mut f = raf(
        "FinePix F700",
        &[size_record(0x100, 40, 30), record(0x130, &[0x80, 0x81, 0x82, 0x83])],
        &words_le(60 * 40),
    );
    f[0x78..0x7C].copy_from_slice(&1u32.to_be_bytes());
    let dec = open_default(&f).unwrap();
    let info = dec.frame_info();
    assert_eq!((info.width, info.height), (30, 40));
    assert!(dec.packed_payload().is_none());
    let img = decode(&*dec, false).unwrap();
    // Row 1 starts at sample 60 of the payload.
    assert_eq!(img.u16()[30], 60u16.wrapping_mul(7));
}

#[test]
fn second_frame_pointer_is_ignored_for_small_jpeg_offsets() {
    let mut f = raf(
        "FinePix S5600",
        &[size_record(0x100, 30, 40), record(0x130, &[0x80, 0x81, 0x82, 0x83])],
        &words_le(80 * 30),
    );
    f[0x78..0x7C].copy_from_slice(&1u32.to_be_bytes());
    f[0x54..0x58].copy_from_slice(&100u32.to_be_bytes());
    assert_eq!(open_default(&f).unwrap().frame_info().width, 40);
}

#[test]
fn the_5504_back_is_tiled() {
    let (w, h) = (5504usize, 24usize);
    let mut payload = Vec::new();
    for t in 0..8usize {
        for y in 0..h {
            for i in 0..w / 8 {
                payload.extend(((t * 1000 + y * 10 + i) as u16).to_le_bytes());
            }
        }
    }
    let f = raf("DBP for GX680", &[size_record(0x100, h as u16, w as u16), record(0x130, &[0, 1, 2, 1])], &payload);
    let dec = open_default(&f).unwrap();
    assert_eq!(dec.random_access(), RandomAccess::Sequential);
    assert_eq!(dec.frame_info().orientation, Orientation::Rotate90);
    let img = decode(&*dec, false).unwrap();
    assert_eq!(img.u16()[688 + 3], 1000 + 3);
    assert_eq!(img.u16()[w + 2], 10 + 2);
    let p = Profiler::new();
    let ctx = DecodeContext::new(&p);
    let r = dec.decode_region(&ctx, Rect::new(686, 1, 4, 2)).unwrap();
    assert_eq!(
        r.u16(),
        &[10 + 686, 10 + 687, 1000 + 10, 1000 + 11, 20 + 686, 20 + 687, 1000 + 20, 1000 + 21].map(|v| v as u16)
    );
    assert!(dec.decode_region(&ctx, Rect::new(6000, 0, 4, 2)).is_err());
}

// ---- TIFF section (form A) ----------------------------------------------------------------

fn form_a(model: &str, records: &[Vec<u8>], entries: &[(u16, u32)], payload: &[u8]) -> Vec<u8> {
    raf(model, records, &tiff_section(entries, payload, None))
}

#[test]
fn tiff_section_with_16_bit_words() {
    let payload = words_le(64 * 48);
    let entries = [(0xF001, 64), (0xF002, 48), (0xF003, 16), (0xF008, payload.len() as u32)];
    let f = form_a("X-T3", &[], &entries, &payload);
    let dec = open_default(&f).unwrap();
    let info = dec.frame_info();
    assert_eq!((info.width, info.height, info.bits_per_sample), (64, 48, 16));
    assert_eq!(info.white_level.values, vec![65535]);
    assert_eq!(decode(&*dec, false).unwrap().u16()[3], 21);
    assert_eq!(dec.packed_payload().unwrap().row_stride, 128);
}

#[test]
fn packed_flag_selects_bit_packed_rows() {
    // 12 bits, LSB-first bytes (flag 0): two samples in three bytes.
    let (w, h) = (24u32, 22u32);
    let payload: Vec<u8> = (0..(w * h * 12 / 8)).map(|i| (i * 37) as u8).collect();
    let entries = [(0xF001, w), (0xF002, h), (0xF003, 12), (0xF006, 0), (0xF008, payload.len() as u32)];
    let f = form_a("X10", &[], &entries, &payload);
    let dec = open_default(&f).unwrap();
    let p = dec.packed_payload().unwrap();
    assert_eq!((p.bits, p.order, p.row_stride), (12, PackOrder::LsbBytes, 36));
    let img = decode(&*dec, false).unwrap();
    let b = &payload;
    assert_eq!(img.u16()[0], u16::from(b[0]) | (u16::from(b[1] & 15) << 8));
    assert_eq!(img.u16()[1], u16::from(b[1] >> 4) | (u16::from(b[2]) << 4));
    // An MSB-32 flag changes only the order.
    let entries = [(0xF001, w), (0xF002, h), (0xF003, 12), (0xF006, 1), (0xF008, payload.len() as u32)];
    let f = form_a("FinePix S1", &[], &entries, &payload);
    assert_eq!(open_default(&f).unwrap().packed_payload().unwrap().order, PackOrder::Msb32);
}

#[test]
fn fourteen_bit_packing_is_recognised_from_the_size() {
    let (w, h) = (32u32, 24u32);
    let payload = vec![0x55u8; (w * h * 7 / 4) as usize];
    let entries = [(0xF001, w), (0xF002, h), (0xF003, 14), (0xF008, payload.len() as u32)];
    let f = form_a("X-A3", &[], &entries, &payload);
    let dec = open_default(&f).unwrap();
    let p = dec.packed_payload().unwrap();
    assert_eq!((p.bits, p.order), (14, PackOrder::Msb32));
    assert_eq!(dec.frame_info().white_level.values, vec![16383]);
}

#[test]
fn unknown_payload_sizes_are_read_as_words() {
    let payload = words_le(30 * 30);
    let entries = [(0xF001, 40), (0xF002, 40), (0xF003, 14), (0xF008, 123)];
    let f = form_a("X-T3", &[], &entries, &payload);
    let dec = open_default(&f).unwrap();
    let img = decode(&*dec, true).unwrap();
    assert_eq!(img.u16()[900 - 1], (899u16).wrapping_mul(7));
    assert_eq!(img.u16()[900], 0);
    assert_eq!(dec.units().len(), 0);
}

#[test]
fn huge_payload_offsets_leave_an_empty_payload() {
    let entries = [(0xF001, 40), (0xF002, 40), (0xF003, 14), (0xF008, 3200)];
    let f = raf("X-T3", &[], &tiff_section(&entries, &words_le(1600), Some(u32::MAX)));
    // On 64-bit targets the sum fits and nothing is there to read; on 32-bit it overflows.
    match open_default(&f) {
        Ok(dec) => {
            assert!(decode(&*dec, false).is_err());
            assert_eq!(decode(&*dec, true).unwrap().u16().iter().filter(|&&v| v != 0).count(), 0);
        }
        Err(e) => assert_eq!(e.kind(), "CorruptFile"),
    }
}

#[test]
fn black_levels_from_the_record_follow_the_pattern() {
    // The record form: R G B G2 follow the Bayer pattern of the file.
    let words = [be16(10), be16(20), be16(30), be16(40)].concat(); // G R G2 B
    let f = raf("FinePix S5600", &[size_record(0x100, 30, 40), record(0x4000, &words)], &words_le(40 * 30));
    let dec = open_default(&f).unwrap();
    // RGGB at the visible origin: R=20, G=10, G2=30, B=40.
    assert_eq!(dec.frame_info().black_level.values, vec![20.0, 10.0, 30.0, 40.0]);
}

#[test]
fn white_balance_corrections_and_averaging() {
    let words = [be16(80), be16(163), be16(80), be16(125)].concat();
    let f = raf("FinePix S5000", &[size_record(0x100, 30, 40), record(0x2FF0, &words)], &words_le(40 * 30));
    let wb = open_default(&f).unwrap().frame_info().white_balance.unwrap();
    assert!((wb.coeffs[0] - 146.0 / 80.0).abs() < 1e-6);
    assert!((wb.coeffs[2] - 125.0 * ((2.0 / 3.0) / (3.0 / 4.0 + 1.0 / 300.0)) as f32 / 80.0).abs() < 0.02);
    // Two sets are averaged with integer division, then reordered.
    let two = [80u16, 129, 80, 100, 82, 131, 80, 101].iter().flat_map(|v| be16(*v)).collect::<Vec<u8>>();
    assert_eq!(
        container::Directory::read(&raf("X", &[record(0x2FF0, &two)], &[]), 0x94).white_balance,
        Some([130, 81, 100])
    );
}

// ---- record directory ---------------------------------------------------------------------

fn read_dir(records: &[Vec<u8>]) -> Directory {
    Directory::read(&raf("X", records, &[]), 0x94)
}

#[test]
fn directory_rules() {
    // Crop records count only after the raw size; later records replace earlier ones.
    let d = read_dir(&[
        size_record(0x110, 1, 2),
        size_record(0x100, 10, 20),
        size_record(0x110, 3, 4),
        size_record(0x100, 11, 21),
    ]);
    assert_eq!((d.raw_size, d.crop_origin), (Some((11, 21)), Some((3, 4))));
    // Width 4284 is reported as 4287.
    assert_eq!(read_dir(&[size_record(0x121, 100, 4284)]).image_size, Some((100, 4287)));
    // Short records are ignored.
    assert_eq!(read_dir(&[record(0x100, &[0, 1, 0])]).raw_size, None);
    assert!(read_dir(&[record(0x4000, &[0; 12])]).black.is_none());
    // SuperCCD flags.
    let d = read_dir(&[record(0x130, &[0x80, 0x00])]);
    assert!(d.layout && d.wide);
    let d = read_dir(&[record(0x130, &[0x00, 0x08])]);
    assert!(!d.layout && !d.wide);
    // X-Trans: stored back to front, large values count as blue.
    let mut body: Vec<u8> = (0..36).map(|i| (i % 3) as u8).collect();
    body[35] = 9;
    let d = read_dir(&[record(0x131, &body)]);
    let p = d.xtrans.unwrap();
    assert_eq!((p[0], p[1], p[35]), (2, 1, 0));
    // A directory that is too long, or cut off, yields fewer records.
    assert!(Directory::read(&[0, 0, 1, 0], 0).raw_size.is_none());
    let mut f = raf("X", &[size_record(0x100, 10, 20), size_record(0x121, 5, 6)], &[]);
    f.truncate(0x94 + 4 + 8 + 6);
    let d = Directory::read(&f, 0x94);
    assert_eq!((d.raw_size, d.image_size), (Some((10, 20)), None));
}

#[test]
fn size_blob_generations() {
    let blob = |prefix: &[u8], at: usize, w: u32, h: u32| {
        let mut b = vec![0u8; 21000];
        b[..prefix.len()].copy_from_slice(prefix);
        b[at..at + 4].copy_from_slice(&w.to_le_bytes());
        b[at + 4..at + 8].copy_from_slice(&h.to_le_bytes());
        b
    };
    let parse = |b: Vec<u8>| {
        let d = read_dir(&[record(0xC000, &b)]);
        (d.size_blob.generation, d.size_blob.size)
    };
    assert_eq!(parse(blob(&[0x40, 0x06], 0, 1600, 1200)), (1, Some((0x640, 1200))));
    assert_eq!(parse(blob(&[0x20, 0x4E, 0, 0, 0, 0, 0, 0], 4, 4000, 3000)), (2, Some((4000, 3000))));
    assert_eq!(parse(blob(&[0, 0, 0x6C, 0x02, b'W', b'R', b'T', b'S'], 8, 6000, 4000)), (4, Some((6000, 4000))));
    assert_eq!(parse(blob(&[0, 0, 5, 0, 1, 2, 3, 4], 4, 7, 8)), (3, Some((7, 8))));
    assert_eq!(parse(blob(&[1, 0, 1, 0], 100, 0, 0)), (0, None));
    let d = read_dir(&[record(0xC000, &vec![0u8; 4096])]);
    assert_eq!((d.size_blob.generation, d.size_blob.size), (4096, None));
    assert_eq!(read_dir(&[record(0xC000, &vec![0u8; 5000])]).size_blob.generation, 0);
}

// ---- geometry -----------------------------------------------------------------------------

fn area(
    model: &str,
    nominal: (i64, i64),
    generation: u32,
    crop_mode: u32,
    table: Option<(i64, i64)>,
) -> geometry::Area {
    area_with(model, nominal, generation, crop_mode, table, true)
}

fn area_with(
    model: &str,
    nominal: (i64, i64),
    generation: u32,
    crop_mode: u32,
    table: Option<(i64, i64)>,
    xtrans: bool,
) -> geometry::Area {
    geometry::active_area(&geometry::Inputs {
        model,
        nominal,
        stored: nominal,
        image_size: None,
        generation,
        size_table: table,
        crop_mode,
        vendor_crop: None,
        layout: false,
        wide: false,
        xtrans,
        packed: false,
    })
}

#[test]
fn modern_sensor_margins() {
    let a = area("X-T3", (6384, 4182), 4, 0, Some((6252, 4168)));
    assert_eq!((a.left, a.top, a.width, a.height), (0, 6, 6246, 4170));
    let a = area("X-T3", (6384, 3348), 4, 2, Some((6252, 4168)));
    assert_eq!((a.left, a.width, a.height), (624, 5004, 3348));
    let a = area("X-S20", (6336, 4182), 4, 0, Some((0, 0)));
    assert_eq!((a.top, a.width, a.height), (6, 6252, 4176));
    let a = area("X-T5", (7872, 5196), 4, 0, Some((7728, 5152)));
    assert_eq!((a.left, a.top, a.width, a.height), (0, 6, 7752, 5178));
    let a = area_with("GFX100S", (11808, 8754), 4, 0, Some((11648, 8736)), false);
    assert_eq!((a.left, a.top, a.width, a.height), (0, 2, 11662, 8746));
    assert!(a.fourteen_bit);
    let a = area_with("GFX50S II", (8472, 6368), 4, 0, Some((8432, 6336)), false);
    assert_eq!((a.left, a.top, a.width), (0, 0, 8280));
}

#[test]
fn legacy_sensor_margins() {
    let dbp = geometry::active_area(&geometry::Inputs {
        model: "DBP for GX680",
        nominal: (5504, 3856),
        stored: (5504, 3856),
        image_size: Some((5504, 3856)),
        generation: 0,
        size_table: None,
        crop_mode: 0,
        vendor_crop: Some((32, 24, 7648, 2704)),
        layout: false,
        wide: true,
        xtrans: false,
        packed: false,
    });
    assert_eq!((dbp.left, dbp.top, dbp.width, dbp.height, dbp.bggr, dbp.superccd), (32, 8, 5440, 3840, true, false));
    let s2 = geometry::active_area(&geometry::Inputs {
        model: "S2Pro",
        nominal: (2944, 2192),
        stored: (2944, 2192),
        image_size: Some((2880, 2144)),
        generation: 0,
        size_table: None,
        crop_mode: 0,
        vendor_crop: None,
        layout: false,
        wide: true,
        xtrans: false,
        packed: false,
    });
    assert_eq!((s2.left, s2.top, s2.width, s2.height, s2.superccd), (32, 24, 2880, 2144, true));
    assert!(!s2.fourteen_bit);
}

#[test]
fn superccd_lattice_period() {
    let a = geometry::Area {
        left: 0,
        top: 0,
        width: 100,
        height: 50,
        bggr: false,
        fourteen_bit: true,
        superccd: true,
        columns: 51,
    };
    let p = geometry::pattern(None, &a, true, Rect::new(0, 0, 100, 50)).unwrap().0;
    assert_eq!((p.width(), p.height()), (2, 4));
    let p = geometry::pattern(None, &a, false, Rect::new(0, 0, 100, 50)).unwrap().0;
    assert_eq!((p.width(), p.height()), (4, 2));
}

// ---- robustness ---------------------------------------------------------------------------

#[test]
fn mutated_containers_never_panic() {
    let payload = words_le(64 * 48);
    let entries = [(0xF001, 64), (0xF002, 48), (0xF003, 16), (0xF008, payload.len() as u32)];
    let words = [be16(10), be16(20), be16(30), be16(40)].concat();
    let bases = [
        form_a(
            "X-T3",
            &[size_record(0x100, 48, 64), record(0x131, &[0u8; 36]), record(0x4000, &words)],
            &entries,
            &payload,
        ),
        raf("FinePix S5600", &[size_record(0x100, 30, 40), record(0x130, &[0x80, 0, 0, 0])], &words_le(40 * 30)),
    ];
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for base in &bases {
        for round in 0..400 {
            let mut f = base.clone();
            for _ in 0..1 + round % 4 {
                let at = (next() % 0x200) as usize % f.len();
                f[at] = next() as u8;
            }
            if round % 5 == 0 {
                f.truncate((next() as usize) % f.len());
            }
            if let Ok(dec) = open_default(&f) {
                let _ = decode(&*dec, true);
                let _ = decode(&*dec, false);
                let p = Profiler::new();
                let ctx = DecodeContext::new(&p);
                let _ = dec.decode_region(&ctx, Rect::new(3, 2, 20, 10));
                let _ = dec.units();
            }
        }
    }
}

// ---- real files (need the corpus) ---------------------------------------------------------

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// A stable one-line description of everything the reader derives.
fn summary(dec: &dyn RawDecoder) -> String {
    let (m, i) = (dec.metadata(), dec.frame_info());
    format!(
        "{} {} {}x{} bps{} cfa{:?} {:?} aa{} crop{} black{:?}/{}x{} white{:?} wb{:?} ori{:?} {:?} {:?} units{} extra{:?} previews{} cm{}",
        m.make,
        m.model,
        i.width,
        i.height,
        i.bits_per_sample,
        i.cfa.map(|c| c.to_string()),
        i.layout,
        i.active_area,
        i.crop,
        i.black_level.values,
        i.black_level.rows,
        i.black_level.cols,
        i.white_level.values,
        i.white_balance.map(|w| w.coeffs),
        i.orientation,
        i.compression,
        dec.random_access(),
        dec.units().len(),
        m.extra,
        m.previews.len(),
        i.color_matrices.color.len(),
    )
}

/// `(corpus path, FNV-1a of summary)`; regenerate with `FLASHRAW_PRINT_GOLDEN=1`.
const GOLDEN: &[(&str, u64)] = &[
    ("Fujifilm/X-T3/AFXT2721.RAF", 0xc0a6d5b46c4a9dcf),
    ("Fujifilm/X-T5/DSCF4964_70-300.RAF", 0x89792d8135fdee1f),
    ("Fujifilm/X-S20/DSCF2529.RAF", 0x7e89b7dfcb806a26),
    ("Fujifilm/X-H2S/IMG-20221005-0667.raf", 0xe0848fb40bcd67d1),
    ("FUJIFILM/X-M5/DSCF0623_-_crop_1,25.RAF", 0xbd27b049f97f6096),
    ("FUJIFILM/GFX100RF/DSCF0092.RAF", 0x4ad459e8179d68c0),
    ("Fujifilm/GFX 50S/20170525_0037TEST.RAF", 0xc767d7298e64d549),
    ("Fujifilm/GFX50S II/Fuji_GFX50S_II_-_Compressed.RAF", 0x4c0760733ce34ec7),
    ("Fujifilm/X-E4/fuji_xe4_32_uncompressed.RAF", 0x2efefa350413e2ec),
    ("Fujifilm/X-A3/DSCF7241.RAF", 0x14d4ca85fe422037),
    ("Fujifilm/X-A1/DSCF2482.RAF", 0xe94dc77d833aee05),
    ("Fujifilm/X-E1/DSCF6490.RAF", 0x463059bb7308f02a),
    ("Fujifilm/FinePix HS10 HS11/RAW_FUJI_FINEPIX_HS10.RAF", 0x2a9ac06568ba76ae),
    ("Fujifilm/FinePix HS20EXR/RAW_FUJIFILM_HS20EXR.RAF", 0x2ddbef6ca8fc2bbc),
    ("Fujifilm/FinePix F900EXR/RAW_FUJI_FINPIX_F900EXR.RAF", 0x6fb81b22d16016c5),
    ("Fujifilm/FinePix F700/RAW_FUJI_F700.RAF", 0x56adbd23b28fa9f8),
    ("Fujifilm/FinePix S2Pro/DSCF0002.RAF", 0x7c23d7285549fec8),
    ("Fujifilm/FinePix S5000/2007_0912Image0022.RAF", 0xedb767899453533a),
    ("Fujifilm/FinePix S3Pro/DSCF3694.RAF", 0xef0a5e1585983d4f),
    ("Fujifilm/FinePix S6000fd/DSCF1973.RAF", 0x513bf5de45f7cb51),
    ("Fujifilm/DBP for GX680/DSCF0010.RAF", 0xd28eac69a8eafc50),
    ("Fujifilm/FinePix X100/DSCF1676.RAF", 0x96e1d4500f8eee91),
];

#[test]
#[ignore = "needs the corpus (FLASHRAW_CORPUS)"]
fn golden_summaries_of_real_files() {
    let Some(root) = std::env::var_os("FLASHRAW_CORPUS") else { return };
    let print = std::env::var_os("FLASHRAW_PRINT_GOLDEN").is_some();
    let mut checked = 0;
    for (path, golden) in GOLDEN {
        let Ok(data) = std::fs::read(std::path::Path::new(&root).join(path)) else { continue };
        let dec = open_default(&data).unwrap_or_else(|e| panic!("{path}: {e}"));
        let s = summary(&*dec);
        if print {
            println!("    (\"{path}\", {:#x}),", fnv(&s));
            continue;
        }
        assert_eq!(fnv(&s), *golden, "{path}: {s}");
        checked += 1;
    }
    eprintln!("{checked} golden files checked");
}
