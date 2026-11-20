//! Cancelling a decode: a `CancelToken` in `DecodeOptions::cancel` stops it from another thread
//! with `Error::Cancelled`, whatever the thread count, output or demosaic, and a token that is
//! never cancelled changes nothing.

use flashraw::{Backend, CancelToken, CorruptionPolicy, DecodeOptions, Demosaic, Error, FlashRaw, OutputFormat};
use std::time::{Duration, Instant};

fn shorts(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn longs(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A single-IFD little-endian DNG of `w x h` 16-bit samples with the given CFA (`dim x dim`
/// pattern of 0 R / 1 G / 2 B), holding a coloured scene with an edge and a little noise.
fn dng(w: u32, h: u32, dim: u16, pattern: &[u8]) -> Vec<u8> {
    let mut s = 12345u32;
    let data: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            let c = pattern[((y % dim as u32) * dim as u32 + x % dim as u32) as usize];
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((s >> 24) as f32 - 128.0) * 8.0;
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

/// A Bayer file big enough that a develop takes tens of milliseconds.
fn big() -> Vec<u8> {
    dng(2400, 1800, 2, &[0, 1, 1, 2])
}

fn small() -> Vec<u8> {
    dng(96, 80, 2, &[0, 1, 1, 2])
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
        assert!(matches!(r, Err(Error::Cancelled)), "threads {threads:?}: {:?}", r.map(|_| ()));
        assert!(
            took < full.mul_f64(0.75),
            "threads {threads:?}: cancelled after {took:?}, a full decode takes {full:?}"
        );

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
