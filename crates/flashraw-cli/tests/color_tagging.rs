//! `--color-space` / `--output-profile`: TIFF and JPEG files carry the matching ICC profile.

use std::path::PathBuf;
use std::process::Command;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures/canon-5d3-mlv.dng")
}

fn decode(out: &str, args: &[&str]) -> (std::process::Output, PathBuf) {
    let path = std::env::temp_dir().join(format!("flashraw-cli-{}-{out}", std::process::id()));
    let o = Command::new(env!("CARGO_BIN_EXE_flashraw"))
        .arg("decode")
        .arg(fixture())
        .arg(&path)
        .args(["--demosaic", "half", "--json"])
        .args(args)
        .output()
        .unwrap();
    (o, path)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn tiff_and_jpeg_embed_the_space_profile() {
    for (space, desc) in
        [("srgb", "FlashRaw sRGB"), ("display-p3", "FlashRaw Display P3"), ("prophoto", "FlashRaw ProPhoto RGB")]
    {
        let profile = flashraw::ColorSpace::parse(space).unwrap().icc_profile();
        let (o, tif) = decode(&format!("{space}.tif"), &["--output", "rgb16", "--color-space", space]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["color_space"], space);
        let bytes = std::fs::read(&tif).unwrap();
        assert!(contains(&bytes, &profile), "{space}: profile bytes in the TIFF");
        // ICCProfile tag 34675 (0x8773) is present in the directory (little endian).
        assert!(contains(&bytes[..200], &[0x73, 0x87, 7, 0]), "{space}: tag 34675 in the IFD");
        assert!(
            contains(&bytes, desc.as_bytes())
                || contains(&bytes, &desc.encode_utf16().flat_map(|c| c.to_be_bytes()).collect::<Vec<_>>())
        );
        let (o, jpg) = decode(&format!("{space}.jpg"), &["--output", "rgba8", "--color-space", space]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let bytes = std::fs::read(&jpg).unwrap();
        // APP2 "ICC_PROFILE\0" chunk followed by the profile.
        assert!(contains(&bytes, b"ICC_PROFILE\0"));
        assert!(contains(&bytes, &profile), "{space}: profile bytes in the JPEG");
        let _ = (std::fs::remove_file(tif), std::fs::remove_file(jpg));
    }
}

#[test]
fn output_profile_is_embedded_and_garbage_is_rejected() {
    let p3 = flashraw::ColorSpace::DisplayP3.icc_profile();
    let icc = std::env::temp_dir().join(format!("flashraw-cli-{}-p3.icc", std::process::id()));
    std::fs::write(&icc, &p3).unwrap();
    let (o, tif) = decode(
        "custom.tif",
        &["--output", "rgb16", "--output-profile", icc.to_str().unwrap(), "--intent", "relative", "--bpc"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["color_space"], "custom");
    assert!(contains(&std::fs::read(&tif).unwrap(), &p3));
    std::fs::write(&icc, [7u8; 400]).unwrap();
    let (o, _) = decode("bad.tif", &["--output", "rgb16", "--output-profile", icc.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("invalid output profile"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let (o, _) = decode("bad2.tif", &["--output", "rgb16", "--color-space", "nope"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown color space"));
    let (o, _) = decode("bad3.tif", &["--output", "rgb16", "--output-profile", "/nonexistent.icc"]);
    assert!(!o.status.success());
    let _ = (std::fs::remove_file(icc), std::fs::remove_file(tif));
}
