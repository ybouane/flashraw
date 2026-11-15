//! Conformance of the Nikon High Efficiency decoder on bare codestreams.
//!
//! `nikon_he/cases.tsv` lists small codestreams (`group`, `file`, `width`, `height`, the expected
//! `OK` or `ERR <kind>`, and the FNV-1a 64 of the decoded little endian `u16` buffer). They cover
//! what the 28 corpus files never exercise: raw packets, every band mode, significance flags
//! with and without prediction, padding coefficients, slack inside sub-packets, many level
//! layouts, and damaged streams. Set `FLASHRAW_HE_SYNTHETIC` to a directory laid out the same
//! way (`synthetic.tsv` plus `valid/`, `extreme/`, `mutated/`, `deviation/` folders) to run a
//! larger set.

use flashraw_codecs::nikon_he;
use flashraw_core::{Error, Limits};
use std::path::{Path, PathBuf};

fn fnv1a64(samples: &[u16]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for s in samples {
        for b in s.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

fn decode(cs: &[u8], w: usize, h: usize, pitch: usize, parallel: bool) -> Result<Vec<u16>, Error> {
    let mut out = vec![0xA5A5u16; pitch * h];
    nikon_he::decode(cs, &mut out, w, h, pitch, &Limits::default(), parallel).map(|()| out)
}

struct Case {
    group: String,
    path: PathBuf,
    w: usize,
    h: usize,
    expected: String,
    hash: String,
}

fn load(root: &Path, flat: bool) -> Vec<Case> {
    let list = if flat { root.join("cases.tsv") } else { root.join("synthetic.tsv") };
    let text = std::fs::read_to_string(&list).unwrap_or_else(|e| panic!("{}: {e}", list.display()));
    text.lines()
        .skip(1)
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            let path = if flat { root.join(c[1]) } else { root.join(c[0]).join(c[1]) };
            Case {
                group: c[0].to_string(),
                path,
                w: c[2].parse().unwrap(),
                h: c[3].parse().unwrap(),
                expected: c[4].to_string(),
                hash: c.get(5).copied().unwrap_or("").to_string(),
            }
        })
        .collect()
}

fn check(cases: &[Case]) {
    let (mut ok, mut err) = (0, 0);
    for c in cases {
        let cs = std::fs::read(&c.path).unwrap();
        let name = c.path.display();
        let seq = nikon_he::decode_sequential_for_test(&cs, c.w, c.h);
        let par = decode(&cs, c.w, c.h, c.w, true);
        // One thread and many produce the same result.
        match (&seq, &par) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{name}: threads differ"),
            (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind(), "{name}: threads differ"),
            _ => panic!("{name}: threads differ"),
        }
        if c.group == "deviation" {
            // More than eight horizontal levels: unsupported by design (the hash is not defined).
            assert_eq!(seq.unwrap_err().kind(), "UnsupportedCompression", "{name}");
            err += 1;
            continue;
        }
        match c.expected.strip_prefix("ERR ") {
            Some(kind) => {
                let e = seq.expect_err(&format!("{name}: expected {kind}"));
                assert_eq!(e.kind(), kind, "{name}: {e}");
                err += 1;
            }
            None => {
                let out = seq.unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(format!("{:016x}", fnv1a64(&out)), c.hash, "{name}");
                // A wider destination keeps its extra columns and gives the same samples.
                let wide = decode(&cs, c.w, c.h, c.w + 3, true).unwrap();
                for y in 0..c.h {
                    assert_eq!(&wide[y * (c.w + 3)..y * (c.w + 3) + c.w], &out[y * c.w..(y + 1) * c.w], "{name}");
                    assert!(wide[y * (c.w + 3) + c.w..(y + 1) * (c.w + 3)].iter().all(|&v| v == 0xA5A5), "{name}");
                }
                ok += 1;
            }
        }
    }
    eprintln!("nikon he synthetic: {ok} decoded, {err} rejected");
    assert!(ok > 0 && err > 0);
}

#[test]
fn fixture_streams() {
    check(&load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/nikon_he"), true));
}

/// The full set of 1,966 streams, when available.
#[test]
fn full_synthetic_set() {
    let Some(dir) = std::env::var_os("FLASHRAW_HE_SYNTHETIC") else {
        eprintln!("FLASHRAW_HE_SYNTHETIC not set; skipping");
        return;
    };
    check(&load(Path::new(&dir), false));
}
