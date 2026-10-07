//! Builds the C and C++ examples (`examples/c`, `examples/cpp`) against the libraries cargo
//! just built and runs them on a repository fixture. Unix only (CI covers Windows with
//! CMake); skipped when no C/C++ compiler is available (`CC` / `CXX` override `cc` / `c++`).

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Where cargo put the cdylib and staticlib built for this test run: `target/<profile>/deps`
/// (`cargo test` does not copy them up to `target/<profile>` the way `cargo build` does).
fn lib_dir() -> PathBuf {
    let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    if deps.join("libflashraw_capi.a").exists() { deps } else { deps.parent().unwrap().to_path_buf() }
}

fn compiler(var: &str, default: &str) -> Option<String> {
    let c = std::env::var(var).unwrap_or_else(|_| default.to_string());
    Command::new(&c).arg("--version").output().ok().filter(|o| o.status.success()).map(|_| c)
}

fn out_dir(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("flashraw-capi-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(cmd: &mut Command) -> String {
    let o = cmd.output().unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success(), "{cmd:?} failed:\n{stdout}\n{}", String::from_utf8_lossy(&o.stderr));
    stdout
}

/// Libraries a static Rust library needs on this platform, as reported by rustc.
fn native_libs(dir: &std::path::Path) -> Vec<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let probe = dir.join("probe.rs");
    std::fs::write(&probe, "").unwrap();
    let reported = Command::new(rustc)
        .args(["--crate-type", "staticlib", "--crate-name", "probe", "--print", "native-static-libs", "-o"])
        .arg(dir.join("libprobe.a"))
        .arg(&probe)
        .output()
        .ok()
        .and_then(|o| {
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            err.lines().find_map(|l| l.split("native-static-libs:").nth(1).map(|s| s.trim().to_string()))
        });
    let mut libs: Vec<String> = match reported {
        Some(libs) => libs.split_whitespace().map(String::from).collect(),
        None => ["-lpthread", "-ldl", "-lm"].map(String::from).to_vec(),
    };
    // In a workspace build the library may include the GPU backend (features are unified
    // across members), which on Apple platforms needs the Metal and Objective-C runtimes;
    // the probe crate above does not report those.
    if cfg!(target_os = "macos") {
        for f in ["Metal", "QuartzCore", "Foundation", "CoreGraphics"] {
            libs.extend(["-framework".to_string(), f.to_string()]);
        }
        libs.push("-lobjc".into());
    }
    libs
}

#[test]
fn c_example_static() {
    let Some(cc) = compiler("CC", "cc") else {
        eprintln!("no C compiler: skipping");
        return;
    };
    let lib = lib_dir().join("libflashraw_capi.a");
    assert!(lib.exists(), "{} missing", lib.display());
    let dir = out_dir("c");
    let exe = dir.join("flashraw_info");
    run(Command::new(&cc)
        .args(["-std=c99", "-Wall", "-Wextra", "-Werror", "-pedantic", "-O1"])
        .arg("-I")
        .arg(root().join("crates/flashraw-capi/include"))
        .arg(root().join("examples/c/flashraw_info.c"))
        .arg(&lib)
        .args(native_libs(&dir))
        .arg("-o")
        .arg(&exe));
    let ppm = dir.join("c.ppm");
    let out = run(Command::new(&exe).arg(root().join("npm/tests/fixtures/canon-a410-chdk.dng")).arg(&ppm));
    assert!(out.contains("camera:      Canon PowerShot A410"), "{out}");
    assert!(out.contains("raw:         2144x1560x1 u16"), "{out}");
    assert!(out.contains("cfa:         2x2 RGGB"), "{out}");
    let ppm = std::fs::read(&ppm).unwrap();
    assert!(ppm.starts_with(b"P6\n2048 1536\n255\n"));
    // Errors come back as a status and a message, not a crash.
    let o = Command::new(&exe).arg(root().join("README.md")).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("[UnsupportedFormat]"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cpp_example_shared() {
    let Some(cxx) = compiler("CXX", "c++") else {
        eprintln!("no C++ compiler: skipping");
        return;
    };
    let dir = out_dir("cpp");
    let exe = dir.join("flashraw_decode");
    let libs = lib_dir();
    run(Command::new(&cxx)
        .args(["-std=c++17", "-Wall", "-Wextra", "-Wshadow", "-Werror", "-pedantic", "-O1"])
        .arg("-I")
        .arg(root().join("crates/flashraw-capi/include"))
        .arg(root().join("examples/cpp/flashraw_decode.cpp"))
        .arg(format!("-L{}", libs.display()))
        .arg("-lflashraw_capi")
        .arg(format!("-Wl,-rpath,{}", libs.display()))
        .arg("-lpthread")
        .arg("-o")
        .arg(&exe));
    let out = run(Command::new(&exe)
        .arg(root().join("npm/tests/fixtures/canon-5d3-lossy.dng"))
        .arg(dir.join("cpp.ppm"))
        .arg(dir.join("preview.jpg")));
    assert!(out.contains("DNG Canon EOS 5D Mark III, sensor 3960x2640"), "{out}");
    assert!(out.contains("junk input rejected: UnsupportedFormat"), "{out}");
    let jpeg = std::fs::read(dir.join("preview.jpg")).unwrap();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    let _ = std::fs::remove_dir_all(dir);
}

/// The header compiles as strict C99/C11 and as C++ with every warning enabled, and
/// the C++ wrapper compiles without the library (header-only).
#[test]
fn headers_compile_standalone() {
    let dir = out_dir("headers");
    let inc = root().join("crates/flashraw-capi/include");
    if let Some(cc) = compiler("CC", "cc") {
        let src = dir.join("h.c");
        std::fs::write(&src, "#include <flashraw.h>\nint main(void) { return FLASHRAW_OK; }\n").unwrap();
        for std in ["-std=c99", "-std=c11"] {
            run(Command::new(&cc)
                .args([std, "-Wall", "-Wextra", "-Werror", "-pedantic", "-fsyntax-only"])
                .arg("-I")
                .arg(&inc)
                .arg(&src));
        }
    }
    if let Some(cxx) = compiler("CXX", "c++") {
        let src = dir.join("h.cpp");
        std::fs::write(&src, "#include <flashraw.hpp>\nint main() { return flashraw::version().empty(); }\n").unwrap();
        for std in ["-std=c++17", "-std=c++20"] {
            run(Command::new(&cxx)
                .args([std, "-Wall", "-Wextra", "-Wshadow", "-Werror", "-pedantic", "-fsyntax-only"])
                .arg("-I")
                .arg(&inc)
                .arg(&src));
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}
