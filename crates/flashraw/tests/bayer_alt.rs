//! `Demosaic::Ppg` and `Demosaic::Rcd` on synthetic DNG files: Bayer sensors develop (the same
//! bytes for any thread count, different from bilinear), other sensors are rejected with
//! `Error::InvalidArgument`, and `Demosaic::Auto` stands for AHD on Bayer and edge-aware on X-Trans.

use flashraw::{Backend, DecodeOptions, Demosaic, Error, FlashRaw, OutputFormat};

fn shorts(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn longs(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A single-IFD little-endian DNG of `w x h` 16-bit samples with the given CFA (`dim x dim`
/// pattern of 0 R / 1 G / 2 B), holding a coloured scene with an edge and a little noise.
fn dng(w: u32, h: u32, dim: u16, pattern: &[u8]) -> Vec<u8> {
    let mut s = 12345u32;
    let data: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            let c = pattern[((y % dim as u32) * dim as u32 + x % dim as u32) as usize];
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((s >> 24) as f32 - 128.0) * 8.0;
            let base = [
                12000.0 + x as f32 * 60.0,
                20000.0 + y as f32 * 40.0 + if x > w / 2 { 9000.0 } else { 0.0 },
                30000.0 - x as f32 * 50.0,
            ][c as usize];
            ((base + noise).clamp(0.0, 65535.0) as u16).to_le_bytes()
        })
        .collect();
    let mut e: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        (0x00FE, 4, 1, longs(&[0])),
        (0x0100, 4, 1, longs(&[w])),
        (0x0101, 4, 1, longs(&[h])),
        (0x0102, 3, 1, shorts(&[16])),
        (0x0103, 3, 1, shorts(&[1])),
        (0x0106, 3, 1, shorts(&[32803])),
        (0x010F, 2, 5, b"Test\0".to_vec()),
        (0x0110, 2, 6, b"Model\0".to_vec()),
        (0x0111, 4, 1, longs(&[0])),
        (0x0115, 3, 1, shorts(&[1])),
        (0x0116, 4, 1, longs(&[h])),
        (0x0117, 4, 1, longs(&[w * h * 2])),
        (0x828D, 3, 2, shorts(&[dim, dim])),
        (0x828E, 1, pattern.len() as u32, pattern.to_vec()),
        (0xC612, 1, 4, vec![1, 4, 0, 0]),
        (0xC61A, 4, 1, longs(&[0])),
        (0xC61D, 4, 1, longs(&[65535])),
    ];
    e.sort_by_key(|x| x.0);
    let ifd_end = 8 + 2 + e.len() * 12 + 4;
    let extra: usize = e.iter().filter(|x| x.3.len() > 4).map(|x| x.3.len() + (x.3.len() & 1)).sum();
    let strip = (ifd_end + extra) as u32;
    for x in e.iter_mut().filter(|x| x.0 == 0x0111) {
        x.3 = longs(&[strip]);
    }
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend((e.len() as u16).to_le_bytes());
    let mut ext = Vec::new();
    for (tag, typ, count, d) in &e {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        if d.len() <= 4 {
            let mut d = d.clone();
            d.resize(4, 0);
            out.extend(d);
        } else {
            out.extend(((ifd_end + ext.len()) as u32).to_le_bytes());
            ext.extend(d);
            if d.len() & 1 == 1 {
                ext.push(0);
            }
        }
    }
    out.extend(0u32.to_le_bytes());
    out.extend(ext);
    out.extend(data);
    out
}

fn bayer() -> Vec<u8> {
    dng(96, 80, 2, &[0, 1, 1, 2])
}

fn xtrans() -> Vec<u8> {
    let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
    let p: Vec<u8> =
        rows.iter().flat_map(|r| r.bytes().map(|c| b"RGB".iter().position(|&x| x == c).unwrap() as u8)).collect();
    dng(96, 96, 6, &p)
}

fn rgb(raw: &FlashRaw<'_>, demosaic: Demosaic, threads: Option<usize>) -> flashraw::Result<Vec<u8>> {
    let f = raw.decode(DecodeOptions {
        output: OutputFormat::Rgba8,
        demosaic,
        threads,
        backend: Backend::Auto,
        ..Default::default()
    })?;
    Ok(f.as_u8().unwrap().to_vec())
}

#[test]
fn bayer_demosaics_are_deterministic_and_differ_from_bilinear() {
    let raw = FlashRaw::open(bayer()).unwrap();
    let bilinear = rgb(&raw, Demosaic::Bilinear, Some(1)).unwrap();
    for d in [Demosaic::Ppg, Demosaic::Rcd] {
        let one = rgb(&raw, d, Some(1)).unwrap();
        assert_eq!(one, rgb(&raw, d, Some(3)).unwrap(), "{d:?}");
        assert_eq!(one, rgb(&raw, d, None).unwrap(), "{d:?}");
        assert_ne!(one, bilinear, "{d:?}");
    }
    assert_ne!(rgb(&raw, Demosaic::Ppg, Some(1)).unwrap(), rgb(&raw, Demosaic::Rcd, Some(1)).unwrap());
}

#[test]
fn bayer_only_demosaics_reject_other_sensors_but_not_raw_outputs() {
    let raw = FlashRaw::open(xtrans()).unwrap();
    for d in [Demosaic::Ppg, Demosaic::Rcd] {
        match rgb(&raw, d, Some(1)) {
            Err(Error::InvalidArgument(m)) => assert!(m.contains("Bayer"), "{m}"),
            r => panic!("{d:?}: {:?}", r.map(|v| v.len())),
        }
        // The demosaic is irrelevant to raw outputs.
        raw.decode(DecodeOptions { demosaic: d, ..Default::default() }).unwrap();
    }
}

#[test]
fn auto_is_ahd_on_bayer_and_edge_aware_on_x_trans() {
    for (file, same) in [(bayer(), Demosaic::Ahd), (xtrans(), Demosaic::EdgeAware)] {
        let raw = FlashRaw::open(file).unwrap();
        assert_eq!(rgb(&raw, Demosaic::Auto, Some(1)).unwrap(), rgb(&raw, same, Some(1)).unwrap());
    }
}
