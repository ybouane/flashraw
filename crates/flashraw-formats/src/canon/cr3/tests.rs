//! CR3 tests: synthetic containers and corruption robustness on corpus samples.

use super::*;
use std::path::{Path, PathBuf};

fn corpus_cr3() -> Vec<PathBuf> {
    // Tests run with the crate directory as cwd; the corpus lives at the workspace root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/Canon");
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(&root) else { return out };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("cr3")) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn ctx(p: &Profiler) -> DecodeContext<'_> {
    DecodeContext::new(p)
}

#[test]
fn rejects_non_cr3() {
    assert!(!probe(b"II*\0\x08\0\0\0"));
    assert!(open(b"\0\0\0\x0cftypcrx ", &OpenOptions::default()).is_err());
}

/// Smallest lossless and C-RAW corpus samples: truncation and byte flips must
/// produce errors (or images), never panics.
#[test]
fn corrupted_corpus_samples_do_not_panic() {
    let files = corpus_cr3();
    if files.is_empty() {
        eprintln!("corpus/Canon not present; skipping");
        return;
    }
    let mut picks: Vec<&PathBuf> = Vec::new();
    for want_wavelet in [false, true] {
        let best = files
            .iter()
            .filter_map(|p| {
                let d = std::fs::read(p).ok()?;
                let c = Cr3::parse(&d).ok()?;
                let (_, _, h) = c.main_raw()?;
                ((h.levels > 0) == want_wavelet).then(|| (d.len(), p))
            })
            .min_by_key(|x| x.0);
        if let Some((_, p)) = best {
            picks.push(p);
        }
    }
    let prof = Profiler::new();
    for p in picks {
        let data = std::fs::read(p).unwrap();
        let dec = open(&data, &OpenOptions::default()).unwrap();
        let full = dec.decode(&ctx(&prof)).unwrap();
        let (offset, _) = {
            let c = Cr3::parse(&data).unwrap();
            c.main_raw().unwrap().1.sample(0).unwrap()
        };
        let offset = offset as usize;
        // Truncations: inside the container, inside the CRX header, inside the payload.
        for cut in [100, 4000, offset + 50, offset + 2000, offset + (data.len() - offset) / 2, data.len() - 1000] {
            let d = &data[..cut.min(data.len())];
            if let Ok(dec) = open(d, &OpenOptions::default()) {
                let _ = dec.decode(&ctx(&prof));
            }
        }
        // Byte flips in the CRX header and payload.
        let mut seed = 0x1234_5678u32;
        for i in 0..24 {
            let mut d = data.clone();
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let pos =
                if i < 12 { offset + (seed as usize % 1100) } else { offset + seed as usize % (data.len() - offset) };
            d[pos] ^= 1 << (seed >> 29);
            if let Ok(dec) = open(&d, &OpenOptions::default()) {
                if let Ok(img) = dec.decode(&ctx(&prof)) {
                    assert_eq!(img.u16().len(), full.u16().len());
                }
            }
        }
    }
}

/// Sequential and parallel decoding produce identical output.
#[test]
fn parallel_matches_sequential() {
    let files = corpus_cr3();
    let Some(p) = files.iter().min_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX)) else {
        return;
    };
    let data = std::fs::read(p).unwrap();
    let dec = open(&data, &OpenOptions::default()).unwrap();
    let prof = Profiler::new();
    let mut c = ctx(&prof);
    let a = dec.decode(&c).unwrap();
    c.parallel = false;
    let b = dec.decode(&c).unwrap();
    assert_eq!(a.u16(), b.u16());
}
