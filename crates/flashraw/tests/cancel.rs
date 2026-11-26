//! Cancelling a decode: a `CancelToken` in `DecodeOptions::cancel` stops it from another thread
//! with `Error::Cancelled`, whatever the thread count, output or demosaic, and a token that is
//! never cancelled changes nothing.

use flashraw::{Backend, CancelToken, CorruptionPolicy, DecodeOptions, Demosaic, Error, FlashRaw, OutputFormat};
use std::time::{Duration, Instant};

mod common;
use common::synthetic::dng;

/// A Bayer file big enough that a develop takes tens of milliseconds.
fn big() -> Vec<u8> {
    dng(2400, 1800, 2, &[0, 1, 1, 2], 8.0)
}

fn small() -> Vec<u8> {
    dng(96, 80, 2, &[0, 1, 1, 2], 8.0)
}

fn opts(output: OutputFormat, demosaic: Demosaic, threads: Option<usize>) -> DecodeOptions {
    DecodeOptions { output, demosaic, threads, backend: Backend::Cpu, ..Default::default() }
}

fn bytes_of(f: &flashraw::RawFrame) -> Vec<u8> {
    if let Some(v) = f.as_u8() {
        v.to_vec()
    } else if let Some(v) = f.as_u16() {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    } else {
        panic!("unexpected sample type")
    }
}

#[test]
fn a_token_that_is_never_cancelled_changes_nothing() {
    let file = big();
    let raw = FlashRaw::open(file.as_slice()).unwrap();
    for threads in [None, Some(1), Some(3)] {
        for (output, demosaic) in [
            (OutputFormat::RawU16, Demosaic::Bilinear),
            (OutputFormat::Rgba8, Demosaic::Bilinear),
            (OutputFormat::Rgba8, Demosaic::Ahd),
            (OutputFormat::Rgb16, Demosaic::Rcd),
            (OutputFormat::Rgba8, Demosaic::HalfSize),
        ] {
            let plain = raw.decode(opts(output, demosaic, threads)).unwrap();
            let token = CancelToken::new();
            let with = raw.decode(opts(output, demosaic, threads).with_cancel(token.clone())).unwrap();
            assert_eq!(bytes_of(&plain), bytes_of(&with), "{output:?} {demosaic:?} threads {threads:?}");
            assert!(!token.is_cancelled());
        }
    }
}

#[test]
fn a_token_cancelled_before_the_decode_stops_it_at_once() {
    let file = big();
    let raw = FlashRaw::open(file.as_slice()).unwrap();
    for threads in [None, Some(1), Some(2)] {
        for (output, demosaic) in [
            (OutputFormat::RawU16, Demosaic::Bilinear),
            (OutputFormat::Rgba8, Demosaic::Ahd),
            (OutputFormat::Rgb16, Demosaic::Rcd),
        ] {
            let token = CancelToken::new();
            token.cancel();
            let t = Instant::now();
            let e = raw.decode(opts(output, demosaic, threads).with_cancel(token)).unwrap_err();
            assert!(matches!(e, Error::Cancelled), "{e}");
            assert_eq!(e.kind(), "Cancelled");
            assert!(e.is_cancelled());
            assert!(t.elapsed() < Duration::from_millis(100), "took {:?}", t.elapsed());
        }
    }
    // Region and tile decodes honour it too.
    let token = CancelToken::new();
    token.cancel();
    let region = flashraw::Rect { x: 0, y: 0, width: 64, height: 64 };
    let e = raw.decode_region(region, DecodeOptions::default().with_cancel(token)).unwrap_err();
    assert!(e.is_cancelled());
}

#[test]
fn cancelling_while_it_runs_ends_the_decode_early() {
    let file = big();
    let raw = FlashRaw::open(file.as_slice()).unwrap();
    for threads in [None, Some(1), Some(4)] {
        let o = opts(OutputFormat::Rgba8, Demosaic::Ahd, threads);
        raw.decode(o.clone()).unwrap(); // warm up
        // Timings on a busy machine are noisy: one of a few attempts has to stop early.
        let mut seen = Vec::new();
        let stopped_early = (0..5).any(|_| {
            let t = Instant::now();
            raw.decode(o.clone()).unwrap();
            let full = t.elapsed();

            let token = CancelToken::new();
            let canceller = {
                let token = token.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(full / 8);
                    token.cancel();
                })
            };
            let t = Instant::now();
            let r = raw.decode(o.clone().with_cancel(token));
            let took = t.elapsed();
            canceller.join().unwrap();
            let cancelled = match r {
                // The decode finished before the cancel arrived: a slow scheduler, try again.
                Ok(_) => false,
                Err(Error::Cancelled) => true,
                Err(e) => panic!("threads {threads:?}: {e}"),
            };
            seen.push((cancelled, took, full));
            cancelled && took < full.mul_f64(0.75)
        });
        assert!(stopped_early, "threads {threads:?}: (cancelled, took, full decode) {seen:?}");

        // The next decode is not affected by the cancelled one.
        let again = raw.decode(o).unwrap();
        assert_eq!(again.width(), 2400);
    }
}

#[test]
fn one_token_stops_several_decodes_and_leaves_no_state_behind() {
    let file = big();
    let raw = FlashRaw::open(file.as_slice()).unwrap();
    let token = CancelToken::new();
    let results: Vec<Error> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let (raw, token) = (&raw, token.clone());
                s.spawn(move || {
                    let o = opts(OutputFormat::Rgba8, Demosaic::Ahd, Some(2)).with_cancel(token);
                    // Keep decoding until stopped.
                    loop {
                        match raw.decode(o.clone()) {
                            Ok(_) => continue,
                            Err(e) => break e,
                        }
                    }
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(60));
        token.cancel();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(results.iter().all(Error::is_cancelled), "{results:?}");
    // A cancelled decode on this thread leaves nothing behind for the next one.
    let t = CancelToken::new();
    t.cancel();
    assert!(raw.decode(DecodeOptions::default().with_cancel(t)).is_err());
    assert!(raw.decode(DecodeOptions::default()).is_ok());
}

#[test]
fn cancelling_is_not_recovered_as_a_corrupt_file() {
    // A truncated file decodes (with warnings) under Recover; a cancelled one must not turn into
    // that result, and `preview_fallback` must not stand in for it either.
    let mut file = small();
    file.truncate(file.len() - 4000);
    let raw = FlashRaw::open(file.as_slice()).unwrap();
    let token = CancelToken::new();
    token.cancel();
    let o = DecodeOptions {
        corruption_policy: CorruptionPolicy::Recover,
        preview_fallback: true,
        output: OutputFormat::Rgba8,
        ..Default::default()
    }
    .with_cancel(token);
    assert!(raw.decode(o).unwrap_err().is_cancelled());
}

#[test]
fn cancelling_stops_the_noise_reduction_too() {
    let file = big();
    let raw = FlashRaw::open(file.as_slice()).unwrap();
    let o = DecodeOptions {
        luma_noise_reduction: 1.0,
        chroma_noise_reduction: 1.0,
        ..opts(OutputFormat::Rgba8, Demosaic::Bilinear, None)
    };
    raw.decode(o.clone()).unwrap(); // warm up
    let t = Instant::now();
    raw.decode(o.clone()).unwrap();
    let full = t.elapsed();
    let token = CancelToken::new();
    let canceller = {
        let token = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(full / 3);
            token.cancel();
        })
    };
    let t = Instant::now();
    let r = raw.decode(o.with_cancel(token));
    let took = t.elapsed();
    canceller.join().unwrap();
    assert!(matches!(r, Err(Error::Cancelled)), "{:?}", r.map(|_| ()));
    assert!(took < full.mul_f64(0.75), "cancelled after {took:?}, a full decode takes {full:?}");
}
