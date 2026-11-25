//! Synthetic DNG files for tests that need a decodable mosaic of a chosen size and pattern.
#![allow(dead_code)]

fn shorts(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn longs(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A single-IFD little-endian DNG of `w x h` 16-bit samples with the given CFA (`dim x dim`
/// pattern of 0 R / 1 G / 2 B), holding a coloured scene with an edge and uniform noise of
/// amplitude `128 * noise` around it.
pub fn dng(w: u32, h: u32, dim: u16, pattern: &[u8], noise: f32) -> Vec<u8> {
    dng_with(w, h, dim, pattern, noise, &[])
}

/// [`dng`] with more IFD entries `(tag, type, count, value bytes)`.
pub fn dng_with(w: u32, h: u32, dim: u16, pattern: &[u8], noise: f32, more: &[(u16, u16, u32, Vec<u8>)]) -> Vec<u8> {
    let mut s = 12345u32;
    let data: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            let c = pattern[((y % dim as u32) * dim as u32 + x % dim as u32) as usize];
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((s >> 24) as f32 - 128.0) * noise;
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
    e.extend(more.iter().cloned());
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
