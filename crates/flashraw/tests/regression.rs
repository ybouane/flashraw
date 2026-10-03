//! Corpus regression test: every row of `tests/regression/*.tsv` names a corpus file
//! and the FNV-1a hash of its full decoded sensor buffer (the decode was validated
//! when the row was added).
//!
//! Runs when the corpus is present (`$FLASHRAW_CORPUS` or `<repo>/corpus`); fetch it with
//! `tools/corpus/fetch.py`. Set `FLASHRAW_REGRESSION_FILTER=substring` to narrow.

use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn corpus_regression() {
    let corpus = std::env::var("FLASHRAW_CORPUS").map(PathBuf::from).unwrap_or_else(|_| repo().join("corpus"));
    if !corpus.exists() {
        eprintln!("corpus not found at {}; skipping (see tools/corpus/fetch.py)", corpus.display());
        return;
    }
    let filter = std::env::var("FLASHRAW_REGRESSION_FILTER").unwrap_or_default().to_lowercase();
    let mut rows = Vec::new();
    let mut tsvs: Vec<_> =
        std::fs::read_dir(repo().join("tests/regression")).unwrap().flatten().map(|e| e.path()).collect();
    tsvs.sort();
    for tsv in tsvs.iter().filter(|p| p.extension().is_some_and(|e| e == "tsv")) {
        for line in std::fs::read_to_string(tsv).unwrap().lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let c: Vec<&str> = line.split('\t').collect();
            if c.len() >= 2 && c[0].to_lowercase().contains(&filter) {
                rows.push((c[0].to_string(), c[1].to_string()));
            }
        }
    }
    let failures = std::sync::Mutex::new(Vec::new());
    let missing = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for chunk in rows.chunks(rows.len().div_ceil(8).max(1)) {
            let (failures, missing, corpus) = (&failures, &missing, &corpus);
            s.spawn(move || {
                for (rel, expected) in chunk {
                    let p = corpus.join(rel);
                    if !p.exists() {
                        missing.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        continue;
                    }
                    let got = flashraw::FlashRaw::open_path(&p)
                        .and_then(|r| {
                            r.decode(flashraw::DecodeOptions {
                                threads: Some(2),
                                apply_opcodes: false,
                                ..Default::default()
                            })
                        })
                        .map(|f| {
                            f.hash_u16(flashraw::Rect::full(f.width, f.height))
                                .map(|h| format!("{h:016x}"))
                                .unwrap_or_default()
                        });
                    match got {
                        Ok(h) if &h == expected => {}
                        Ok(h) => failures.lock().unwrap().push(format!("{rel}: hash {h} != expected {expected}")),
                        Err(e) => failures.lock().unwrap().push(format!("{rel}: {e}")),
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    eprintln!(
        "{} regression files checked, {} missing from corpus, {} failures",
        rows.len(),
        missing.into_inner(),
        failures.len()
    );
    assert!(failures.is_empty(), "regressions:\n{}", failures.join("\n"));
}
