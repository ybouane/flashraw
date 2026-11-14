//! CLI handling of truncated sensor data: recovered by default (warnings in the JSON),
//! a structured error with `--strict`.

use std::path::PathBuf;
use std::process::Command;

/// A 16x8 16-bit uncompressed DNG whose single strip lacks its last `missing` bytes.
fn truncated_dng(missing: usize) -> Vec<u8> {
    let (w, h) = (16u32, 8u32);
    let entries: Vec<(u16, u16, u32, u32)> = vec![
        (0x00FE, 4, 1, 0),
        (0x0100, 4, 1, w),
        (0x0101, 4, 1, h),
        (0x0102, 3, 1, 16),
        (0x0103, 3, 1, 1),
        (0x0106, 3, 1, 32803),
        (0x0111, 4, 1, 0), // patched below
        (0x0115, 3, 1, 1),
        (0x0116, 4, 1, h),
        (0x0117, 4, 1, w * h * 2),
        (0x828D, 3, 2, 2 | (2 << 16)),
        (0x828E, 1, 4, u32::from_le_bytes([0, 1, 1, 2])),
        (0xC612, 1, 4, u32::from_le_bytes([1, 4, 0, 0])),
    ];
    let payload_off = 8 + 2 + entries.len() as u32 * 12 + 4;
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend((entries.len() as u16).to_le_bytes());
    for &(tag, typ, count, v) in &entries {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        let v = if tag == 0x0111 { payload_off } else { v };
        out.extend(v.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes());
    for i in 0..w * h {
        out.extend(((i * 97 % 4000) as u16 + 1).to_le_bytes());
    }
    out.truncate(out.len() - missing);
    out
}

fn write_temp(name: &str, data: &[u8]) -> PathBuf {
    let p = std::env::temp_dir().join(format!("flashraw-cli-{}-{name}.dng", std::process::id()));
    std::fs::write(&p, data).unwrap();
    p
}

fn verify(path: &PathBuf, extra: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_flashraw"))
        .arg("verify")
        .arg("--json")
        .args(extra)
        .arg(path)
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), serde_json::from_slice(&out.stdout).unwrap())
}

#[test]
fn verify_recovers_by_default_and_fails_with_strict() {
    let p = write_temp("verify", &truncated_dng(10));
    let (code, v) = verify(&p, &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["incomplete"], true);
    let w = &v["warnings"][0];
    assert_eq!(w["kind"], "truncated");
    assert_eq!((w["unit"].as_str(), w["index"].as_u64()), (Some("strip"), Some(0)));
    assert_eq!((w["expected"].as_u64(), w["available"].as_u64()), (Some(256), Some(246)));
    assert_eq!(w["samples_filled"], 5);
    assert!(w["message"].as_str().unwrap().starts_with("recovered truncated strip 0"));

    let (code, v) = verify(&p, &["--strict"]);
    assert_eq!(code, 2);
    assert_eq!(v["error_kind"], "CorruptFile");
    assert_eq!(v["corrupt"]["kind"], "truncated");
    assert_eq!(v["corrupt"]["available"], 246);
    assert!(v["error"].as_str().unwrap().contains("but only 246 bytes remain before EOF"));
    let _ = std::fs::remove_file(p);
}

#[test]
fn complete_files_have_no_warnings() {
    let p = write_temp("complete", &truncated_dng(0));
    let (code, v) = verify(&p, &["--strict"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["incomplete"], false);
    assert_eq!(v["warnings"].as_array().map(|a| a.len()), Some(0));
    let _ = std::fs::remove_file(p);
}

#[test]
fn decode_reports_warnings() {
    let p = write_temp("decode", &truncated_dng(4));
    let out_path = p.with_extension("raw");
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_flashraw")).arg("decode").arg(&p).arg(&out_path).args(extra).output().unwrap()
    };
    let o = run(&["--json"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("warning:"));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["incomplete"], true);
    assert_eq!(v["warnings"][0]["samples_filled"], 2);
    let o = run(&["--strict"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("truncated image data"));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(&out_path);
}
