//! Builds the C client (`tests/c/libraw_client.c`, plain LibRaw C API usage) and the C++
//! client (`tests/c/libraw_cpp_client.cpp`, the `LibRaw` class) against this crate's
//! headers and libraries and runs them on repository fixtures, plus a sample of the corpus
//! when `FLASHRAW_CORPUS` points at it. Unix only; skipped without a compiler.

#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_dir() -> PathBuf {
    let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    if deps.join("libraw.a").exists() { deps } else { deps.parent().unwrap().to_path_buf() }
}

fn compiler(var: &str, default: &str) -> Option<String> {
    let c = std::env::var(var).unwrap_or_else(|_| default.to_string());
    Command::new(&c).arg("--version").output().ok().filter(|o| o.status.success()).map(|_| c)
}

fn native_libs(dir: &Path) -> Vec<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let probe = dir.join("probe.rs");
    std::fs::write(&probe, "").unwrap();
    Command::new(rustc)
        .args(["--crate-type", "staticlib", "--crate-name", "probe", "--print", "native-static-libs", "-o"])
        .arg(dir.join("libprobe.a"))
        .arg(&probe)
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stderr).lines().find_map(|l| {
                l.split("native-static-libs:").nth(1).map(|s| s.split_whitespace().map(String::from).collect())
            })
        })
        .map(|mut libs: Vec<String>| {
            // In a workspace build the library may include the GPU backend (features are
            // unified across members), which on Apple platforms needs the Metal and
            // Objective-C runtimes; the probe crate above does not report those.
            if cfg!(target_os = "macos") {
                for f in ["Metal", "QuartzCore", "Foundation", "CoreGraphics"] {
                    libs.extend(["-framework".to_string(), f.to_string()]);
                }
                libs.push("-lobjc".into());
            }
            libs
        })
        .unwrap_or_else(|| ["-lpthread", "-ldl", "-lm"].map(String::from).to_vec())
}

fn out_dir(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("flashraw-libraw-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn build_c_client(dir: &Path) -> Option<PathBuf> {
    let cc = compiler("CC", "cc")?;
    let exe = dir.join("libraw_client");
    let o = Command::new(cc)
        .args(["-std=c99", "-O1", "-Wall", "-Wextra", "-Werror", "-I"])
        .arg(root().join("include"))
        .arg(root().join("tests/c/libraw_client.c"))
        .arg(lib_dir().join("libraw.a"))
        .args(native_libs(dir))
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    Some(exe)
}

/// Run the client on `files` and parse its output into one key/value map per file.
fn run_client(exe: &Path, args: &[&str], files: &[PathBuf]) -> (i32, Vec<HashMap<String, String>>) {
    let o = Command::new(exe).args(args).args(files).output().unwrap();
    let mut out: Vec<HashMap<String, String>> = vec![];
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        if k == "file" {
            out.push(HashMap::new());
        }
        if let Some(m) = out.last_mut() {
            m.entry(k.to_string()).or_insert_with(|| v.to_string());
        }
    }
    (o.status.code().unwrap_or(-1), out)
}

#[test]
fn c_client_on_fixtures() {
    let dir = out_dir("c");
    let Some(exe) = build_c_client(&dir) else {
        eprintln!("no C compiler: skipping");
        return;
    };
    let fx = |n: &str| root().join("../../npm/tests/fixtures").join(n);
    let files = [fx("canon-a410-chdk.dng"), fx("canon-5d3-mlv.dng"), fx("bmpcc-tiles.dng"), fx("canon-5d3-lossy.dng")];
    let (code, out) = run_client(&exe, &[], &files);
    assert_eq!(code, 0, "{out:?}");
    assert_eq!(out.len(), 4);
    let a410 = &out[0];
    for (k, v) in [
        ("make", "Canon"),
        ("model", "PowerShot A410"),
        ("filters", "b4b4b4b4"),
        ("raw_size", "2144x1560"),
        ("visible", "2088x1552+2+6"),
        ("black", "31"),
        ("maximum", "1023"),
        ("layout", "raw_image"),
        ("raw_hash", "437b0ba5c16a94b6"),
        ("color00", "0132"),
        ("data_maximum", "896"),
        ("process", "0"),
        ("mem_image", "0"),
        ("mem_image_format", "2 2088x1552 colors=3 bits=8 size=9721728"),
        ("unpack_thumb", "0"),
    ] {
        assert_eq!(a410.get(k).map(String::as_str), Some(v), "{k}: {a410:?}");
    }
    let mean: Vec<f64> = a410["mem_image_mean"].split(' ').map(|v| v.parse().unwrap()).collect();
    // Developed image of a daylight scene: plausible, not black, not clipped.
    assert!(mean.iter().all(|&m| m > 10.0 && m < 200.0), "{mean:?}");
    assert_eq!(out[1]["raw_hash"], "ec88ca511154e121");
    assert_eq!(out[2]["raw_hash"], "62d2eb2145670cc5");
    assert_eq!(out[3]["layout"], "color4_image");
    assert!(out[3]["mem_thumb"].contains("soi=ffd8"), "{:?}", out[3]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cpp_client_class_interface() {
    let Some(cxx) = compiler("CXX", "c++") else {
        eprintln!("no C++ compiler: skipping");
        return;
    };
    let dir = out_dir("cpp");
    let exe = dir.join("libraw_cpp_client");
    let libs = lib_dir();
    let o = Command::new(cxx)
        .args(["-std=c++11", "-O1", "-Wall", "-Wextra", "-Werror", "-I"])
        .arg(root().join("include"))
        .arg(root().join("tests/c/libraw_cpp_client.cpp"))
        .arg(format!("-L{}", libs.display()))
        .arg("-lraw")
        .arg(format!("-Wl,-rpath,{}", libs.display()))
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = Command::new(&exe).arg(root().join("../../npm/tests/fixtures/canon-a410-chdk.dng")).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}{}", String::from_utf8_lossy(&o.stderr));
    assert!(out.contains("camera=Canon PowerShot A410"), "{out}");
    assert!(out.contains("output=1044x776 colors=3 bps=8"), "{out}");
    let _ = std::fs::remove_dir_all(dir);
}

/// One file per camera model of the corpus (`FLASHRAW_CORPUS`): open + unpack must succeed
/// wherever FlashRaw decodes the file, and the raw data must be FlashRaw's.
#[test]
fn c_client_on_corpus_sample() {
    let Some(corpus) = std::env::var_os("FLASHRAW_CORPUS").map(PathBuf::from).filter(|p| p.is_dir()) else {
        eprintln!("FLASHRAW_CORPUS not set: skipping");
        return;
    };
    let dir = out_dir("corpus");
    let Some(exe) = build_c_client(&dir) else { return };
    let mut files = vec![];
    for make in std::fs::read_dir(&corpus).unwrap().flatten() {
        for model in std::fs::read_dir(make.path()).into_iter().flatten().flatten() {
            let mut fs: Vec<PathBuf> = std::fs::read_dir(model.path())
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            fs.sort();
            files.extend(fs.into_iter().take(1));
        }
    }
    files.sort();
    let files: Vec<PathBuf> = files.into_iter().step_by(8).collect();
    let mut checked = 0;
    for chunk in files.chunks(16) {
        let (_, out) = run_client(&exe, &["--no-process"], chunk);
        for (f, m) in chunk.iter().zip(&out) {
            let decodes = flashraw::FlashRaw::open_path(f).ok().and_then(|r| {
                (r.frame_info().width <= 65535 && r.frame_info().height <= 65535)
                    .then(|| r.decode(flashraw::DecodeOptions { apply_opcodes: false, ..Default::default() }).ok())?
            });
            let Some(frame) = decodes else { continue };
            assert_eq!(m.get("unpack").map(String::as_str), Some("0"), "{}: {m:?}", f.display());
            if frame.cpp == 1 {
                let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                for b in frame.as_u16().unwrap().iter().flat_map(|v| v.to_le_bytes()) {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
                assert_eq!(m["raw_hash"], format!("{h:016x}"), "{}", f.display());
            }
            checked += 1;
        }
    }
    eprintln!("{checked} corpus files checked");
    let _ = std::fs::remove_dir_all(dir);
}
