//! Parallel decode: any thread count (including absurd ones, which are clamped) must give
//! the same result as the single-threaded decode, for sensor samples and for developed
//! RGB. Worker count and output come from the last two input bytes.
#![no_main]
use flashraw::{CorruptionPolicy, DecodeOptions, FlashRaw, Limits, OutputFormat};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((file, c)) = data.split_last_chunk::<2>().map(|(f, c)| (f, *c)) else { return };
    let limits = Limits::default().with_max_samples(8 << 20).with_max_dimension(8192);
    let Ok(raw) = FlashRaw::open_with(file, limits) else { return };
    let threads = [2, 3, 4, 8, 64, 300, usize::MAX, 0][(c[0] & 7) as usize];
    let output = if c[0] & 8 == 0 { OutputFormat::RawU16 } else { OutputFormat::Rgba8 };
    let opts = |threads| DecodeOptions {
        limits,
        output,
        threads: Some(threads),
        corruption_policy: if c[1] & 1 == 0 { CorruptionPolicy::Recover } else { CorruptionPolicy::Strict },
        ..Default::default()
    };
    let single = raw.decode(opts(1));
    let multi = raw.decode(opts(threads));
    match (&single, &multi) {
        (Ok(a), Ok(b)) => {
            assert_eq!((a.width(), a.height()), (b.width(), b.height()));
            assert_eq!(a.data, b.data, "threads {threads} differs from single-threaded");
            assert_eq!(a.warnings().len(), b.warnings().len());
        }
        (Err(_), Err(_)) => {}
        (a, b) => panic!("single-threaded {:?} vs {threads} threads {:?}", a.as_ref().err(), b.as_ref().err()),
    }
});
