//! Tests of the Nikon reader: properties that follow from the format description, and golden
//! hashes of real files (ignored unless the corpus is present).

use super::*;
use conventions::robustness::{corpus_file, open_decode};

fn facts<'a>(w: u64, h: u64, bytes: u64, bps: u32, compression: u32, strip: &'a [u8]) -> RawFacts<'a> {
    RawFacts {
        width: w,
        height: h,
        bytes,
        bps,
        compression,
        photometric: Some(PHOTOMETRIC_CFA),
        samples: 1,
        scanner: false,
        file_order: ByteOrder::Little,
        raw_model: "NIKON D40",
        model: "D40",
        strip,
    }
}

// --- payload selection -------------------------------------------------------------------------

#[test]
fn strip_size_selects_the_payload() {
    let (w, h) = (100u64, 20u64);
    let pick = |bytes, bps, c| choose_payload(&facts(w, h, bytes, bps, c, &[])).unwrap();
    assert!(matches!(pick(10 * 16 * h, 12, 34713), Payload::Packed(s) if s.flags == 1 && s.bits == 12));
    assert!(matches!(pick(w * h * 3 / 2, 12, 34713), Payload::Packed(s) if s.flags == FLAGS_TIGHT_N));
    assert!(matches!(pick(w * h * 3, 12, 34713), Payload::SNef));
    assert!(matches!(pick(w * h * 2, 12, 34713), Payload::Words { order: ByteOrder::Big, shift: 4 }));
    // w = 101: floor(1.5 w) = 151 bytes pad to 160 (a width of 100 ties with the filler layout).
    let p = choose_payload(&facts(101, h, 160 * h, 12, 34713, &[])).unwrap();
    assert!(matches!(p, Payload::PaddedLsb { bits: 12, row_stride: 160 }));
    assert!(matches!(pick(12345, 12, 34713), Payload::Huffman));
    assert!(matches!(pick(w * h * 2, 14, 1), Payload::Words { shift: 0, .. }));
    assert!(matches!(pick(w * h * 3 / 2, 12, 1), Payload::Packed(s) if s.flags == 0));
}

#[test]
fn high_efficiency_is_recognised_by_its_first_bytes() {
    let strip = [0xFF, 0x10, 0xFF, 0x50, 0, 0];
    let p = choose_payload(&facts(100, 20, 12345, 12, 34713, &strip)).unwrap();
    assert!(matches!(p, Payload::HighEfficiency));
}

#[test]
fn unsupported_codings_are_refused() {
    let err = |bps, c| choose_payload(&facts(100, 20, 1000, bps, c, &[])).unwrap_err();
    assert_eq!(err(12, 7).kind(), "UnsupportedCompression");
    assert_eq!(err(10, 1).kind(), "UnsupportedCompression");
    assert_eq!(err(8, 1).kind(), "UnsupportedCompression");
    assert_eq!(err(8, 34713).kind(), "UnsupportedCompression");
}

#[test]
fn scanner_files_are_coolscan_payloads() {
    let mut f = facts(100, 20, 6000, 8, 1, &[]);
    f.photometric = Some(2);
    assert!(matches!(choose_payload(&f).unwrap(), Payload::Coolscan { .. }));
    f.photometric = Some(PHOTOMETRIC_CFA);
    f.scanner = true;
    f.bps = 16;
    assert!(matches!(choose_payload(&f).unwrap(), Payload::Coolscan { .. }));
}

#[test]
fn z_cameras_and_d6_use_fourteen_bit_rows() {
    let (w, h) = (5600u64, 8u64);
    let bytes = (w * 7 / 4).div_ceil(16) * 16 * h;
    let mut f = facts(w, h, bytes, 14, 34713, &[]);
    f.model = "Z FC";
    assert!(matches!(choose_payload(&f).unwrap(), Payload::PaddedLsb { bits: 14, row_stride } if row_stride == 9808));
    f.model = "D40";
    assert!(matches!(choose_payload(&f).unwrap(), Payload::Huffman));
    // 12-bit tight rows win over the 14-bit padded ones where both relations hold.
    f.model = "Z FC";
    f.raw_model = "NIKON Z fc";
    let tight = w * h * 3 / 2;
    f.bytes = tight;
    f.bps = 12;
    assert!(matches!(choose_payload(&f).unwrap(), Payload::Packed(s) if s.flags == FLAGS_TIGHT_N));
}

// --- the model rule table ------------------------------------------------------------------------

struct Outcome {
    vis: Visible,
    quirks: Quirks,
    payload: Payload,
}

fn rules(model: &str, raw_w: u32, raw_h: u32, mut payload: Payload) -> Outcome {
    let mut vis = Visible { left: 0, width: raw_w, height: raw_h, buffer_width: raw_w };
    let mut quirks = Quirks::default();
    let input = RuleInput { model, raw_width: raw_w };
    apply_model_rules(&input, &mut payload, &mut vis, &mut quirks);
    Outcome { vis, quirks, payload }
}

fn area(model: &str, raw_w: u32, raw_h: u32) -> (u32, u32, u32) {
    let o = rules(model, raw_w, raw_h, Payload::Huffman);
    (o.vis.left, o.vis.width, o.vis.height)
}

#[test]
fn visible_areas_of_the_sample_cameras() {
    assert_eq!(area("D1X", 4028, 1324), (0, 4024, 1324));
    assert_eq!(area("D80", 3904, 2616), (0, 3900, 2613));
    assert_eq!(area("D3", 4288, 2844), (2, 4284, 2844));
    assert_eq!(area("D3100", 4672, 3084), (6, 4644, 3084));
    assert_eq!(area("D5000", 4352, 2868), (0, 4310, 2868));
    assert_eq!(area("D7000", 4992, 3280), (0, 4948, 3280));
    assert_eq!(area("D800E", 7424, 4924), (0, 7378, 4924));
    assert_eq!(area("D4", 4992, 3292), (2, 4940, 3292));
    assert_eq!(area("D500", 5600, 3728), (0, 5600, 3728));
    assert_eq!(area("D70S", 3040, 2014), (0, 3039, 2014));
    assert_eq!(area("D200", 3904, 2616), (2, 3899, 2616));
    assert_eq!(area("D2HS", 2496, 1648), (6, 2482, 1648));
    assert_eq!(area("D2XS", 4320, 2868), (0, 4312, 2868));
    assert_eq!(area("D2X", 3264, 2000), (0, 3232, 2000));
    assert_eq!(area("D300S", 4352, 2868), (0, 4320, 2868));
    assert_eq!(area("1 J1", 3904, 2606), (0, 3904, 2604));
    assert_eq!(area("COOLPIX P1000", 4624, 3470), (0, 4624, 3470));
}

#[test]
fn exact_names_beat_the_prefixes_that_would_swallow_them() {
    // D3000 / D3100 are not D300*, D5000 / D500 are not D50*, D700 / D7000 are not D70*.
    assert_eq!(area("D3000", 3904, 2616).1, 3900);
    assert_eq!(area("D5000", 4352, 2868).1, 4310);
    assert_eq!(area("D500", 5600, 3728).1, 5600);
    assert_eq!(area("D700", 4288, 2844).1, 4284);
    assert_eq!(area("D7000", 4992, 3280).1, 4948);
    assert_eq!(area("D40X", 3904, 2616), (0, 3900, 2613));
    assert_eq!(area("D40", 3040, 2014).1, 3039);
    // Unknown models keep the whole buffer, and the left origin is always even.
    assert_eq!(area("Z 6", 6048, 4024), (0, 6048, 4024));
    for (name, _) in [("D2H", 0), ("D200", 0), ("D3", 0), ("D3100", 0), ("D4", 0)] {
        assert_eq!(area(name, 6000, 4000).0 % 2, 0);
    }
}

#[test]
fn unit_white_balance_only_for_the_d1() {
    assert!(rules("D1", 2012, 1324, Payload::Huffman).quirks.unit_white_balance);
    assert!(!rules("D1X", 4028, 1324, Payload::Huffman).quirks.unit_white_balance);
}

#[test]
fn d100_filler_layout_widens_the_buffer() {
    let spec = |flags| PackedSpec { width: 3034, height: 2024, bits: 12, flags, field2_offset: None };
    let o = rules("D100", 3034, 2024, Payload::Packed(spec(1)));
    assert_eq!((o.vis.width, o.vis.buffer_width), (3037, 3040));
    let o = rules("D100", 3034, 2024, Payload::Packed(spec(0)));
    assert_eq!((o.vis.width, o.vis.buffer_width), (3034, 3034));
}

#[test]
fn d200_starts_on_green() {
    assert_eq!(rules("D200", 3904, 2616, Payload::Huffman).quirks.cfa, Some(CfaPattern::GRBG));
}

#[test]
fn compact_4032_rules_stop_the_coolpix_rules() {
    let words = Payload::Words { order: ByteOrder::Big, shift: 4 };
    let o = rules("COOLPIX P7700", 4032, 3024, words.clone());
    assert_eq!(o.quirks.white_level, Some(65504));
    assert!(matches!(o.payload, Payload::Words { shift: 0, .. }));
    assert_eq!(o.quirks.cfa, None);
    // P330 is the "any other" case: shift stays 4, nothing else changes.
    let o = rules("COOLPIX P330", 4032, 3024, words);
    assert!(matches!(o.payload, Payload::Words { shift: 4, .. }));
    assert_eq!(o.quirks.white_level, None);
    let packed = Payload::Packed(PackedSpec { width: 4032, height: 3024, bits: 12, flags: 80, field2_offset: None });
    assert!(matches!(rules("COOLPIX P340", 4032, 3024, packed).payload, Payload::Packed(s) if s.flags == 0));
}

#[test]
fn coolpix_nrw_rules() {
    let packed = Payload::Packed(PackedSpec { width: 4624, height: 3470, bits: 12, flags: 0, field2_offset: None });
    let o = rules("COOLPIX P1000", 4624, 3470, packed.clone());
    assert!(matches!(o.payload, Payload::Packed(s) if s.flags == FLAGS_NRW));
    assert_eq!(o.quirks.cfa, Some(CfaPattern::RGGB));
    // B700 gets the flags but not the CFA.
    let o = rules("COOLPIX B700", 5200, 3902, packed.clone());
    assert!(matches!(o.payload, Payload::Packed(s) if s.flags == FLAGS_NRW));
    assert_eq!(o.quirks.cfa, None);
}

#[test]
fn e_series_are_interlaced_and_nikon_1_loses_rows() {
    let packed = Payload::Packed(PackedSpec { width: 2608, height: 1950, bits: 12, flags: 0, field2_offset: None });
    assert!(matches!(rules("E5400", 2608, 1950, packed).payload, Payload::Packed(s) if s.flags == 2));
    assert_eq!(rules("1 AW1", 4620, 3084, Payload::Huffman).vis.height, 3082);
}

#[test]
fn every_rule_is_reachable() {
    // No rule is shadowed by an earlier one: each can be selected by some model.
    let samples = [
        "D1",
        "D1X",
        "D80",
        "D3",
        "D3100",
        "D90",
        "D7000",
        "D3200",
        "D4",
        "D500",
        "D40",
        "D100",
        "D200",
        "D2H",
        "D2X",
        "D300",
        "COOLPIX P1000",
        "1 J1",
        "E5400",
    ];
    let mut hit = [false; 20];
    for name in samples.iter().chain(&["X"]) {
        let w = if *name == "X" { 4032 } else { 5000 };
        let input = RuleInput { model: name, raw_width: w };
        if let Some(i) = MODEL_RULES.iter().position(|r| r.select.matches(&input)) {
            hit[i] = true;
        }
    }
    assert!(hit.iter().all(|&h| h), "{hit:?}");
}

// --- names and white balance ---------------------------------------------------------------------

#[test]
fn table_names() {
    assert_eq!(table_name("COOLPIX P7000"), "Coolpix P7000");
    assert_eq!(table_name("Coolpix A"), "Coolpix A");
    assert_eq!(table_name("COOLPIX "), "COOLPIX");
    assert_eq!(table_name("D850"), "D850");
    assert_eq!(table_name("E5400"), "E5400");
}

#[test]
fn rotation_records_map_to_orientations() {
    assert_eq!(rotation_from_degrees(0), Some(Orientation::Normal));
    assert_eq!(rotation_from_degrees(90), Some(Orientation::Rotate90));
    assert_eq!(rotation_from_degrees(450), Some(Orientation::Rotate90));
    assert_eq!(rotation_from_degrees(270), Some(Orientation::Rotate270));
    assert_eq!(rotation_from_degrees(45), None);
}

#[test]
fn four_colour_white_balance_from_red_blue_levels() {
    let mut meta = RawMetadata::default();
    meta.make = "NIKON".into();
    meta.model = "E5700".into();
    let cam = flashraw_camera_db::find("Nikon", "E5700", "");
    let table = flashraw_camera_db::find_matrix("Nikon", "E5700");
    let colour = conventions::color_from_table(table, &meta, cam);
    if colour.preferred().is_none_or(|m| m.rows != 4) {
        return;
    }
    let wb = four_colour_wb(&colour, Some([0.586, 1.375])).expect("white balance");
    assert!(wb.coeffs.iter().all(|c| c.is_finite() && *c > 0.0));
    assert!(four_colour_wb(&colour, Some([0.0, 1.0])).is_none());
    assert!(four_colour_wb(&colour, Some([f32::NAN, 1.0])).is_none());
    assert!(four_colour_wb(&colour, None).is_none());
    assert!(four_colour_wb(&ColorMatrices::default(), Some([1.0, 1.0])).is_none());
}

// --- payload arithmetic --------------------------------------------------------------------------

#[test]
fn scan_samples_are_stretched_to_sixteen_bits() {
    assert_eq!(stretch_8_to_16(0), 0);
    assert_eq!(stretch_8_to_16(1), 257);
    assert_eq!(stretch_8_to_16(254), 65278);
    assert_eq!(stretch_8_to_16(255), 65535);
    let mut last = 0;
    for i in 0..=255u8 {
        assert!(stretch_8_to_16(i) >= last);
        last = stretch_8_to_16(i);
    }
}

#[test]
fn snef_tone_table_is_monotone_and_bounded() {
    let t = snef_tone_table();
    assert_eq!(t.len(), 3073);
    assert_eq!(t[0], 0);
    assert!(t.windows(2).all(|w| w[0] <= w[1]));
    assert!(t[3072] <= 16383);
}

#[test]
fn snef_pixels() {
    let tone = snef_tone_table();
    // Neutral chroma (1280) with zero luma has no red or blue; bright luma is neutral grey-white.
    let black = snef_pixel(0, 1280, 1280, tone);
    assert_eq!((black[0], black[2]), (0, 0));
    let white = snef_pixel(2549, 0, 4095, tone);
    assert_eq!(white[0], white[1]);
    assert_eq!(white[1], white[2]);
    let grey = snef_pixel(1200, 1280, 1280, tone);
    assert_eq!(grey[0], grey[2]);
}

#[test]
fn snef_rows_share_chroma_between_neighbours() {
    // Pair 0: Y 16 / 16, Cb 1280, Cr 1280. Pair 1: Y 32 / 32, Cb 1536, Cr 1536.
    let tone = snef_tone_table();
    let line = [0x10, 0x00, 0x01, 0x00, 0x05, 0x50, 0x20, 0x00, 0x02, 0x00, 0x06, 0x60];
    let mut out = [0u16; 12];
    snef_row(&line, &mut out, tone);
    assert_eq!(out[0..3], snef_pixel(16, 1280, 1280, tone));
    // The second pixel of a pair takes the mean chroma of its pair and the next one.
    assert_eq!(out[3..6], snef_pixel(16, 1408, 1408, tone));
    assert_eq!(out[6..9], snef_pixel(32, 1536, 1536, tone));
    // The last pair repeats its own chroma.
    assert_eq!(out[9..12], snef_pixel(32, 1536, 1536, tone));
}

#[test]
fn odd_sized_snef_rows_do_not_panic() {
    let tone = snef_tone_table();
    for w in 2..9usize {
        let line = vec![0x55u8; 3 * w];
        let mut out = vec![0u16; 3 * w];
        snef_row(&line, &mut out, tone);
    }
}

// --- whole files ---------------------------------------------------------------------------------

fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// A digest of everything the reader reports about a file, and of the sensor buffer.
fn digest(data: &[u8]) -> String {
    let dec = crate::open(data, &OpenOptions::default()).expect("open");
    let described = format!("{:?}{:?}{:?}{:?}", dec.format(), dec.metadata(), dec.frame_info(), dec.units());
    let sensor = match open_decode(data) {
        Ok(img) => format!("{:016x}", fnv(img.u16().iter().flat_map(|v| v.to_le_bytes()))),
        Err(e) => e.kind().to_string(),
    };
    format!("{:016x} {sensor}", fnv(described.bytes()))
}

/// Representative files of every payload variant, with the digest the reader produced when
/// it was checked against the previous implementation on the whole corpus.
const GOLDEN: &[(&str, &str)] = &[
    ("Nikon/D1/RAW_NIKON_D1.NEF", "bac9a7dff666fd2a a39adc8da4831f7f"),
    ("Nikon/D100/DSC_2376.NEF", "239be7c9aa350d11 b8bea8165563c3b6"),
    ("Nikon/D2H/D2H_3957.NEF", "619b05a460c9c5b5 0ca67d6803aa4ae1"),
    ("Nikon/D2Xs/DSC_0035.NEF", "8dda0e09040f8f51 4f84d44d0d9fc09f"),
    ("Nikon/D40/DSC_1842.NEF", "30f65dd2e337b1e1 85366f21fbb05eb9"),
    ("Nikon/D50/DSC_5155.NEF", "51d7e72ff444160f e23b8f5e4b96b285"),
    ("Nikon/D70s/RAW_NIKON_D70S.NEF", "2a666916020938ab cd2e6f871fbe8c5e"),
    ("Nikon/D200/pbs_7117.nef", "973283c936ec6a0d d33ff00aa90f0f78"),
    ("Nikon/D3/JD1_8203.NEF", "2ca25f7638ca873c 722b6aaa43471793"),
    ("Nikon/D80/DSC_1114.NEF", "6965b5578772c502 1fb40222c74a7856"),
    ("Nikon/D5100/00.nikon.12b-lossy-raw.nef", "2df139f408920010 b98f36c5078fef7a"),
    ("Nikon/D4S/FU9_3802.NEF", "d9d1db49a66d80b1 6ec79b4f2648a843"),
    ("Nikon/D6/D06_0317.NEF", "9009fc6d5307a496 dd9f097b558d345d"),
    ("Nikon/Coolpix P330/DSCN0138.NRW", "c236ff103e98e710 080d303cc0a628a6"),
    ("Nikon/Coolpix P7000/RAW_NIKON_P7000.NRW", "6d8da3cc42e71519 83ab7153c5a0831f"),
    ("Nikon/Coolpix P340/RAW_NIKON_COOLPIX_P340.NRW", "d12f1f48f91cfd29 e105e2e4880286e6"),
    ("Nikon/Coolpix P6000/RAW_NIKON_P6000_GPS.NRW", "4c9d42a6c07cad73 ea115ba71c4f7ac8"),
    ("Nikon/Coolpix A1000/DSCN0417.NRW", "54be1240ca683227 2ab87617000c0a7d"),
    ("Nikon/E5700/20080115-DSCN1758.NEF", "68ecb8ca442dead7 b48748045e91fbb6"),
    ("Nikon/E5400/RAW_NIKON_E5400.NEF", "333f748c294cc777 6e312a42eef74d79"),
    ("Nikon/1 J1/_DSC8140.NEF", "04c40cbab1c63754 7e734aadc3f69e5b"),
    ("Nikon/Nikon COOLSCAN IV ED/NEF_from_Nikon_Scan_4.0.3_at_600dpi.nef", "42fe73902e0c702b c486b2f5bd35d07b"),
    ("Nikon/Nikon COOLSCAN V ED/Image21-600pixels.nef", "bdb9b337d4c222ea 3dbeb4ddcc66429a"),
    ("NIKON CORPORATION/NIKON Z fc/Nikon_Z_fc_12bit_uncompressed.NEF", "ea1e2141927d9387 b0cfd8d747f722a2"),
    ("NIKON CORPORATION/NIKON Z fc/Nikon_Z_fc_14bit_uncompressed.NEF", "49403757792372c5 67a2e88df1ca2bd0"),
    ("Nikon/Z f/DSC_0043.NEF", "f0275ec5942de9f6 UnsupportedCompression"),
];

#[test]
#[ignore = "needs the corpus"]
fn real_files_keep_their_digests() {
    let generate = std::env::var_os("FLASHRAW_PRINT_GOLDEN").is_some();
    let mut checked = 0;
    for (path, expected) in GOLDEN {
        let Some(data) = corpus_file(path) else { continue };
        let got = digest(&data);
        if generate {
            println!("    (\"{path}\", \"{got}\"),");
            continue;
        }
        assert_eq!(&got, expected, "{path}");
        checked += 1;
    }
    eprintln!("{checked} files checked");
}

#[test]
#[ignore = "needs the corpus"]
fn truncated_and_damaged_real_files_do_not_panic() {
    for (i, path) in
        ["Nikon/D40/DSC_1842.NEF", "Nikon/D4S/FU9_3802.NEF", "Nikon/Coolpix P330/DSCN0138.NRW"].iter().enumerate()
    {
        let Some(data) = corpus_file(path) else { continue };
        conventions::robustness::mutate(&data, 17 + i as u64);
    }
}

#[test]
#[ignore = "needs the corpus"]
fn region_decodes_match_full_decodes() {
    for path in ["Nikon/D1/RAW_NIKON_D1.NEF", "Nikon/Coolpix P330/DSCN0138.NRW", "Nikon/D6/D06_0317.NEF"] {
        let Some(data) = corpus_file(path) else { continue };
        assert!(conventions::robustness::region_matches_full(&data), "{path}");
    }
}
