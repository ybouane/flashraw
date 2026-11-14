#![no_main]
use flashraw_core::tiff::Tiff;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(t) = Tiff::parse(data) {
        for ifd in t.all_ifds() {
            for e in &ifd.entries {
                let _ = (e.u32s(), e.f64s(), e.string(), e.i32(0));
            }
            let mut m = flashraw_core::RawMetadata::default();
            flashraw_core::exif::read_ifd0(ifd, &mut m);
        }
    }
});
