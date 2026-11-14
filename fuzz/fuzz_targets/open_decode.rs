//! Whole pipeline: format detection, metadata, previews, decode (all formats) under both
//! corruption policies. Truncation reports must be self-consistent, recovered frames must
//! account for what they filled, and a strict decode that succeeds must equal the
//! recovered decode, with no warnings.
#![no_main]
use flashraw::{CorruptionPolicy, DecodeOptions, FlashRaw, Limits, Rect};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = flashraw::supports(data);
    let limits = Limits::default().with_max_samples(16 << 20).with_max_dimension(16384);
    if let Ok(raw) = FlashRaw::open_with(data, limits) {
        let _ = raw.metadata().camera();
        for p in raw.previews() {
            let _ = raw.preview_bytes(p);
        }
        let opts = |corruption_policy| DecodeOptions { limits, threads: Some(1), corruption_policy, ..Default::default() };
        let strict = raw.decode(opts(CorruptionPolicy::Strict));
        if let Err(e) = &strict {
            if let Some(t) = e.truncation() {
                assert!(t.available < t.expected || t.range_overflows(), "{e}");
                assert!(t.available <= t.file_len && t.file_len == data.len() as u64, "{e}");
            }
        }
        let recovered = raw.decode(opts(CorruptionPolicy::Recover));
        if let Ok(f) = &recovered {
            let samples = f.data.len() as u64;
            for w in f.warnings() {
                assert!(w.samples_filled <= samples, "{w}");
            }
            let _ = f.hash_u16(Rect::full(f.width, f.height));
        }
        if let (Ok(s), Ok(r)) = (&strict, &recovered) {
            assert!(s.warnings().is_empty());
            assert!(r.warnings().is_empty());
            assert_eq!(s.data, r.data);
        }
        let _ = raw.decode_region(Rect::new(3, 5, 17, 9), opts(CorruptionPolicy::Strict));
        let _ = raw.decode_region(Rect::new(3, 5, 17, 9), opts(CorruptionPolicy::Recover));
    }
});
