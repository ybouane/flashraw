//! `decode --preview-fallback`: undecodable sensor data (a synthetic Nikon High Efficiency
//! NEF) fails by default and writes the embedded full-size preview with the flag, reported
//! as `"source": "preview"` and a `preview-fallback` warning.

use std::path::PathBuf;
use std::process::Command;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures/nikon-he-synthetic.nef")
}

#[test]
fn decode_with_and_without_preview_fallback() {
    let out_path = std::env::temp_dir().join(format!("flashraw-cli-{}-he.ppm", std::process::id()));
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_flashraw"))
            .arg("decode")
            .arg(fixture())
            .arg(&out_path)
            .args(["--output", "rgba8", "--json"])
            .args(extra)
            .output()
            .unwrap()
    };
    let o = run(&[]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("unsupported compression"));

    let o = run(&["--preview-fallback"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("embedded full-size preview"));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        (v["source"].as_str(), v["width"].as_u64(), v["height"].as_u64()),
        (Some("preview"), Some(64), Some(48))
    );
    assert_eq!(v["incomplete"], true);
    assert_eq!(v["warnings"][0]["kind"], "preview-fallback");
    assert!(std::fs::metadata(&out_path).is_ok_and(|m| m.len() > 64 * 48 * 3));
    let _ = std::fs::remove_file(&out_path);
}
