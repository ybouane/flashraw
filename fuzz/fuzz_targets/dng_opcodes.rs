#![no_main]
use flashraw_codecs::dng_opcodes::{apply, parse, PlaneImage};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(ops) = parse(data) {
        let mut px = vec![0.5f32; 32 * 24];
        let mut img = PlaneImage::new(&mut px, 32, 24, 1);
        let _ = apply(&ops, &mut img, 2, 65535.0, false);
    }
});
