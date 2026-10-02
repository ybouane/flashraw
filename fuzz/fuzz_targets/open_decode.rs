//! Whole pipeline: format detection, metadata, previews, decode (all formats).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = flashraw::supports(data);
    let limits = flashraw::Limits { max_samples: 16 << 20, max_dimension: 16384 };
    if let Ok(raw) = flashraw::FlashRaw::open_with(data, limits) {
        let _ = raw.metadata().camera();
        for p in raw.previews() {
            let _ = raw.preview_bytes(p);
        }
        let opts = flashraw::DecodeOptions { limits, threads: Some(1), ..Default::default() };
        if let Ok(f) = raw.decode(opts.clone()) {
            let _ = f.hash_u16(flashraw::Rect::full(f.width, f.height));
        }
        let _ = raw.decode_region(flashraw::Rect::new(3, 5, 17, 9), opts);
    }
});
