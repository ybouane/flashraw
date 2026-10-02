#![no_main]
use flashraw_codecs::ljpeg::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for bug16 in [true, false] {
        if let Ok(h) = LjpegHeader::parse(data, LjpegOptions { dng_bug16: bug16 }) {
            if (h.width as u64) * (h.height as u64) * (h.clrs as u64) > 1 << 22 {
                continue;
            }
            let mut n = 0usize;
            let _ = decode_rows(data, &h, |_, r| {
                n += r.len();
                Ok(())
            });
            let mut dst = vec![0u16; 64 * 64];
            let _ = decode_tile(data, LjpegOptions::default(), &mut dst, 64, 48, 40, 64);
        }
    }
});
