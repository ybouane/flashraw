//! The Rust mirrors in `src/types.rs` must match `include/libraw/libraw_types.h` exactly:
//! compile a C program printing every struct size and field offset, and compare. Unix only;
//! skipped without a C compiler (`CC` overrides `cc`).

#![cfg(unix)]

use raw::types::*;
use std::mem::{offset_of, size_of};
use std::path::PathBuf;
use std::process::Command;

macro_rules! layout {
    ($( $t:ident { $($f:ident),* $(,)? } )*) => {
        vec![ $(
            (stringify!($t).to_string(), String::new(), size_of::<$t>()),
            $( (stringify!($t).to_string(), stringify!($f).to_string(), offset_of!($t, $f)), )*
        )* ]
    };
}

fn rust_layout() -> Vec<(String, String, usize)> {
    let mut v = layout! {
        libraw_decoder_info_t { decoder_name, decoder_flags }
        libraw_internal_output_params_t { mix_green, raw_color, zero_is_bad, shrink, fuji_width }
        libraw_iparams_t { guard, make, model, software, normalized_make, normalized_model, maker_index, raw_count,
            dng_version, is_foveon, colors, filters, xtrans, xtrans_abs, cdesc, xmplen, xmpdata }
        libraw_raw_inset_crop_t { cleft, ctop, cwidth, cheight }
        libraw_image_sizes_t { raw_height, raw_width, height, width, top_margin, left_margin, iheight, iwidth,
            raw_pitch, pixel_aspect, flip, mask, raw_aspect, raw_inset_crops }
        libraw_dng_color_t { parsedfields, illuminant, calibration, colormatrix, forwardmatrix }
        libraw_dng_levels_t { parsedfields, dng_cblack, dng_black, dng_fcblack, dng_fblack, dng_whitelevel,
            default_crop, user_crop, preview_colorspace, analogbalance, asshotneutral, baseline_exposure,
            LinearResponseLimit, rawopcodes }
        libraw_colordata_t { curve, cblack, black, data_maximum, maximum, linear_max, fmaximum, fnorm, white,
            cam_mul, pre_mul, cmatrix, ccm, rgb_cam, cam_xyz, phase_one_data, flash_used, canon_ev, model2,
            UniqueCameraModel, LocalizedCameraModel, ImageUniqueID, RawDataUniqueID, OriginalRawFileName, profile,
            profile_length, black_stat, dng_color, dng_levels, WB_Coeffs, WBCT_Coeffs, as_shot_wb_applied, P1_color,
            raw_bps, ExifColorSpace }
        libraw_thumbnail_t { tformat, twidth, theight, tlength, tcolors, thumb }
        libraw_thumbnail_item_t { tformat, twidth, theight, tflip, tlength, tmisc, toffset }
        libraw_thumbnail_list_t { thumbcount, thumblist }
        libraw_gps_info_t { latitude, longitude, gpstimestamp, altitude, altref, latref, longref, gpsstatus, gpsparsed }
        libraw_imgother_t { iso_speed, shutter, aperture, focal_len, timestamp, shot_order, gpsdata, parsed_gps, desc,
            artist, analogbalance }
        libraw_output_params_t { greybox, cropbox, aber, gamm, user_mul, bright, threshold, half_size, four_color_rgb,
            highlight, use_auto_wb, use_camera_wb, use_camera_matrix, output_color, output_profile, camera_profile,
            bad_pixels, dark_frame, output_bps, output_tiff, output_flags, user_flip, user_qual, user_black,
            user_cblack, user_sat, med_passes, auto_bright_thr, adjust_maximum_thr, no_auto_bright, use_fuji_rotate,
            use_p1_correction, green_matching, dcb_iterations, dcb_enhance_fl, fbdd_noiserd, exp_correc, exp_shift,
            exp_preser, no_auto_scale, no_interpolation }
        libraw_raw_unpack_params_t { use_rawspeed, use_dngsdk, options, shot_select, specials, max_raw_memory_mb,
            sony_arw2_posterization_thr, coolscan_nef_gamma, p4shot_order, custom_camera_strings }
        libraw_rawdata_t { raw_alloc, raw_image, color4_image, color3_image, float_image, float3_image, float4_image,
            ph1_cblack, ph1_rblack, iparams, sizes, ioparams, color }
        libraw_makernotes_lens_t { LensID, Lens, LensFormat, LensMount, CamID, CameraFormat, CameraMount, body,
            FocalType, LensFeatures_pre, LensFeatures_suf, MinFocal, MaxFocal, MaxAp4MinFocal, MaxAp4MaxFocal,
            MinAp4MinFocal, MinAp4MaxFocal, MaxAp, MinAp, CurFocal, CurAp, MaxAp4CurFocal, MinAp4CurFocal,
            MinFocusDistance, FocusRangeIndex, LensFStops, TeleconverterID, Teleconverter, AdapterID, Adapter,
            AttachmentID, Attachment, FocalUnits, FocalLengthIn35mmFormat }
        libraw_nikonlens_t { EffectiveMaxAp, LensIDNumber, LensFStops, MCUVersion, LensType }
        libraw_dnglens_t { MinFocal, MaxFocal, MaxAp4MinFocal, MaxAp4MaxFocal }
        libraw_lensinfo_t { MinFocal, MaxFocal, MaxAp4MinFocal, MaxAp4MaxFocal, EXIF_MaxAp, LensMake, Lens, LensSerial,
            InternalLensSerial, FocalLengthIn35mmFormat, nikon, dng, makernotes }
        libraw_shootinginfo_t { DriveMode, FocusMode, MeteringMode, AFPoint, ExposureMode, ExposureProgram,
            ImageStabilization, BodySerial, InternalBodySerial }
        libraw_data_t { image, sizes, idata, lens, shootinginfo, params, rawparams, progress_flags, process_warnings,
            color, other, thumbnail, thumbs_list, rawdata, parent_class }
    };
    // `type` is a Rust keyword: the field is `type_` on this side.
    v.push(("libraw_processed_image_t".into(), String::new(), size_of::<libraw_processed_image_t>()));
    for (f, o) in [
        ("type", offset_of!(libraw_processed_image_t, type_)),
        ("height", offset_of!(libraw_processed_image_t, height)),
        ("width", offset_of!(libraw_processed_image_t, width)),
        ("colors", offset_of!(libraw_processed_image_t, colors)),
        ("bits", offset_of!(libraw_processed_image_t, bits)),
        ("data_size", offset_of!(libraw_processed_image_t, data_size)),
        ("data", offset_of!(libraw_processed_image_t, data)),
    ] {
        v.push(("libraw_processed_image_t".into(), f.into(), o));
    }
    v.push(("struct ph1_t".into(), String::new(), size_of::<ph1_t>()));
    v
}

#[test]
fn rust_structs_match_the_c_header() {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    if Command::new(&cc).arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("no C compiler: skipping");
        return;
    }
    let expected = rust_layout();
    let mut src =
        String::from("#include <libraw/libraw.h>\n#include <stddef.h>\n#include <stdio.h>\nint main(void) {\n");
    for (t, f, _) in &expected {
        if f.is_empty() {
            src += &format!("  printf(\"{t} %zu\\n\", sizeof({t}));\n");
        } else {
            src += &format!("  printf(\"{t}.{f} %zu\\n\", offsetof({t}, {f}));\n");
        }
    }
    src += "  return 0;\n}\n";
    let dir = std::env::temp_dir().join(format!("flashraw-libraw-layout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let c = dir.join("layout.c");
    let exe = dir.join("layout");
    std::fs::write(&c, src).unwrap();
    let inc = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("include");
    let o = Command::new(&cc)
        .args(["-std=c99", "-Wall", "-Werror", "-I"])
        .arg(&inc)
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(Command::new(&exe).output().unwrap().stdout).unwrap();
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(got.len(), expected.len());
    let mut mismatches = vec![];
    for ((t, f, v), line) in expected.iter().zip(got) {
        let want = if f.is_empty() { format!("{t} {v}") } else { format!("{t}.{f} {v}") };
        if want != line {
            mismatches.push(format!("C: {line}  Rust: {want}"));
        }
    }
    let _ = std::fs::remove_dir_all(dir);
    assert!(mismatches.is_empty(), "layout mismatches:\n{}", mismatches.join("\n"));
}
