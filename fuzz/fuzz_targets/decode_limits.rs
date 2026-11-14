//! Resource limits: with limits drawn from the input (down to tiny), nothing the library
//! returns may exceed them, and nothing may allocate beyond them. Covers the container
//! parsers, raw decode of every format, and embedded-preview decode. The limits come
//! from the last six input bytes.
#![no_main]
use flashraw::{DecodeOptions, FlashRaw, Limits, OutputFormat};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((file, c)) = data.split_last_chunk::<6>().map(|(f, c)| (f, *c)) else { return };
    let max_samples = 1u64 << (4 + c[0] % 24);
    let max_dimension = [1u32, 2, 16, 64, 256, 1024, 4096, 65535][(c[1] & 7) as usize];
    let limits = Limits::default().with_max_samples(max_samples).with_max_dimension(max_dimension);
    let Ok(raw) = FlashRaw::open_with(file, limits) else { return };
    for p in raw.previews() {
        if let Ok(img) = raw.decode_preview_rgb(p) {
            assert!(img.width <= 65535 && img.height <= 65535);
            assert_eq!(img.rgb.len(), img.width as usize * img.height as usize * 3);
        }
    }
    let output = if c[2] & 1 == 0 { OutputFormat::RawU16 } else { OutputFormat::RawF32 };
    // The decode may be given tighter limits than the open.
    let tighter = Limits::default().with_max_samples(max_samples >> (c[3] & 3)).with_max_dimension(max_dimension);
    for l in [limits, tighter] {
        let opts = DecodeOptions { limits: l, output, threads: Some(1 + (c[4] & 1) as usize), ..Default::default() };
        if let Ok(f) = raw.decode(opts) {
            assert!(
                f.width() <= l.max_dimension.max(max_dimension) && f.height() <= l.max_dimension.max(max_dimension)
            );
            assert!(
                f.data.len() as u64 <= l.max_samples.max(max_samples),
                "{} samples over limit {}",
                f.data.len(),
                l.max_samples
            );
        }
    }
});
