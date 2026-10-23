//! Corpus regression test: every row of `tests/regression/*.tsv` names a corpus file
//! and the FNV-1a hash of its full decoded sensor buffer (the decode was validated
//! when the row was added).
//!
//! Needs the corpus (`$FLASHRAW_CORPUS` or `<repo>/corpus`; fetch it with
//! `tools/corpus/fetch.py`). Without it, or with rows whose file is missing, the test
//! fails: a green run must mean the hashes were verified. Set
//! `FLASHRAW_ALLOW_NO_CORPUS=1` to accept a missing or partial corpus (the skipped rows
//! are still reported). Set `FLASHRAW_REGRESSION_FILTER=substring` to narrow.

use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn corpus_regression() {
    let corpus = std::env::var("FLASHRAW_CORPUS").map(PathBuf::from).unwrap_or_else(|_| repo().join("corpus"));
    let allow_partial = std::env::var("FLASHRAW_ALLOW_NO_CORPUS").is_ok_and(|v| !v.is_empty() && v != "0");
    if !corpus.exists() {
        if allow_partial {
            eprintln!("corpus not found at {}; skipping (FLASHRAW_ALLOW_NO_CORPUS is set)", corpus.display());
            return;
        }
        panic!(
            "regression corpus not found at {}: nothing was verified. Fetch it with tools/corpus/fetch.py, \
             point FLASHRAW_CORPUS at it, or set FLASHRAW_ALLOW_NO_CORPUS=1 to skip this test deliberately",
            corpus.display()
        );
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
                            // Complete files decode without recovery warnings.
                            let warn = f.warnings().iter().map(|w| format!(" [warning: {w}]")).collect::<String>();
                            f.hash_u16(flashraw::Rect::full(f.width, f.height))
                                .map(|h| format!("{h:016x}{warn}"))
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
    let missing = missing.into_inner();
    eprintln!(
        "{} regression rows, {} verified, {} missing from corpus, {} failures",
        rows.len(),
        rows.len() - missing,
        missing,
        failures.len()
    );
    assert!(failures.is_empty(), "regressions:\n{}", failures.join("\n"));
    if !allow_partial {
        assert!(
            rows.len() > missing,
            "no regression row was verified: no listed file exists in the corpus (filter {filter:?})"
        );
        assert!(
            missing == 0,
            "{missing} of {} regression files are missing from the corpus at {}; fetch the full corpus \
             (tools/corpus/fetch.py) or set FLASHRAW_ALLOW_NO_CORPUS=1 to accept a partial one",
            rows.len(),
            corpus.display()
        );
    }
}
