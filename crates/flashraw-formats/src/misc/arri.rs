//! ARRI ALEXA `.ari` frames: a 4 KiB little-endian header followed by 12-bit
//! samples packed MSB-first into 32-bit little-endian words, sample pairs stored
//! swapped, GRBG mosaic.
//!
//! Header fields used: payload offset (8), width (20), height (24), as-shot white
//! balance multipliers (100..112, f32 R/G/B) and the camera model string (668).

use crate::common::*;
use flashraw_codecs::misc_packed::{self, PackedParams};
use flashraw_core::*;

const MAGIC: &[u8; 8] = b"ARRI\x12\x34\x56\x78";

pub fn probe(data: &[u8]) -> bool {
    data.len() >= 4096 && data.starts_with(MAGIC)
}

fn le32(d: &[u8], o: usize) -> u32 {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0)
}

pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if !probe(data) {
        return Err(Error::UnsupportedFormat("not an ARRI frame".into()));
    }
    let offset = match le32(data, 8) as usize {
        0 => 4096,
        o => o,
    };
    let (w, h) = (le32(data, 20), le32(data, 24));
    opts.limits.check_dims(w, h, 1)?;
    if offset >= data.len() {
        return Err(Error::corrupt("ARRI payload offset beyond end of file"));
    }
    let model = flashraw_core::bytes::ascii_field(data.get(668..732).unwrap_or(&[]));
    let mut meta = RawMetadata::new("ARRI", model);
    let cam = db_lookup(&mut meta, "");
    let mut info = FrameInfo::new(w, h, 12);
    info.cfa = Some(CfaPattern::GRBG);
    info.layout = SensorLayout::Bayer;
    info.white_level = WhiteLevel::uniform(4095);
    info.compression = Compression::Uncompressed;
    let wb = [100, 104, 108].map(|o| f32::from_bits(le32(data, o)));
    info.white_balance = WhiteBalance::from_multipliers(wb[0], wb[1], wb[2], wb[1]);
    apply_db(&mut info, &meta, cam, Known { cfa: true, ..Default::default() });
    let p = PackedParams { width: w as usize, height: h as usize, bits: 12, flags: 88 };
    let mut dec = SimpleDecoder::new(
        RawFormat::Ari,
        meta,
        info,
        Box::new(move |ctx| {
            let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
            ctx.profiler
                .time(Stage::Unpack, || misc_packed::packed_rows(data, offset, p, img.u16_mut(), ctx.parallel))?;
            Ok(img)
        }),
    );
    dec.random_access = RandomAccess::Rows;
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_frame() {
        let (w, h) = (4u32, 2u32);
        let mut d = vec![0u8; 4096 + 24];
        d[..8].copy_from_slice(MAGIC);
        d[8..12].copy_from_slice(&4096u32.to_le_bytes());
        d[20..24].copy_from_slice(&w.to_le_bytes());
        d[24..28].copy_from_slice(&h.to_le_bytes());
        for (i, o) in [100, 104, 108].iter().enumerate() {
            d[*o..*o + 4].copy_from_slice(&[1.5f32, 1.0, 2.0][i].to_le_bytes());
        }
        // One 32-bit word holds samples a (top 12 bits), b, and 8 bits of c.
        let word: u32 = (0xABC << 20) | (0x123 << 8);
        d[4096..4100].copy_from_slice(&word.to_le_bytes());
        let dec = open(&d, &OpenOptions::default()).unwrap();
        assert_eq!(dec.frame_info().white_balance.unwrap().coeffs[2], 2.0);
        let p = Profiler::default();
        let img = dec.decode(&DecodeContext::new(&p)).unwrap();
        // Pairs are swapped: buffer column 1 holds the first stored sample.
        assert_eq!(&img.u16()[..2], &[0x123, 0xABC]);
        assert!(open(&d[..100], &OpenOptions::default()).is_err());
    }
}
