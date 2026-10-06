//! Tests of the C ABI through the exported `extern "C"` functions, used exactly as C code
//! would: raw pointers, out-parameters, status codes and the thread-local last error.

use flashraw_capi::*;
use std::ffi::{CStr, CString, c_char};
use std::path::PathBuf;
use std::ptr::{null, null_mut};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures").join(name)
}

fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    // SAFETY: the library returns NULL or valid NUL-terminated strings.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_str().unwrap())
}

/// The thread's last error message, checked against the message style: lower case at the
/// start unless the first word is an acronym or identifier, no trailing period or space.
fn last_error() -> String {
    let m = cstr(flashraw_last_error_message()).unwrap_or("").to_string();
    if !m.is_empty() {
        assert_eq!(m.trim(), m, "stray whitespace in {m:?}");
        assert!(!m.ends_with('.') || m.ends_with("..."), "trailing period in {m:?}");
        let word = m.split([' ', ':']).next().unwrap_or("");
        assert!(
            !word.starts_with(|c: char| c.is_uppercase())
                || word.chars().skip(1).any(|c| c.is_uppercase() || c.is_ascii_digit()),
            "capitalised message {m:?}"
        );
    }
    m
}

fn open_path(p: &std::path::Path) -> *mut flashraw_file {
    let c = CString::new(p.to_str().unwrap()).unwrap();
    let mut f = null_mut();
    // SAFETY: valid string and out pointer.
    let s = unsafe { flashraw_open_path(c.as_ptr(), null(), &mut f) };
    assert_eq!(s, flashraw_status::FLASHRAW_OK, "{}", last_error());
    assert!(!f.is_null());
    f
}

fn options() -> flashraw_decode_options {
    let mut o = std::mem::MaybeUninit::<flashraw_decode_options>::zeroed();
    // SAFETY: valid out pointer.
    assert_eq!(unsafe { flashraw_decode_options_init(o.as_mut_ptr()) }, flashraw_status::FLASHRAW_OK);
    // SAFETY: initialised by the call above.
    unsafe { o.assume_init() }
}

fn decode(f: *const flashraw_file, o: &flashraw_decode_options) -> Result<*mut flashraw_frame, flashraw_status> {
    let mut fr = null_mut();
    // SAFETY: live file, initialised options, valid out pointer.
    let s = unsafe { flashraw_decode(f, o, &mut fr) };
    if s == flashraw_status::FLASHRAW_OK { Ok(fr) } else { Err(s) }
}

fn fnv_u16(d: &[u16]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &v in d {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// A 16x8 16-bit uncompressed DNG missing the last `missing` bytes of its strip.
fn truncated_dng(missing: usize) -> Vec<u8> {
    let e: [(u16, u16, u32, u32); 12] = [
        (0x0100, 4, 1, 16),
        (0x0101, 4, 1, 8),
        (0x0102, 3, 1, 16),
        (0x0103, 3, 1, 1),
        (0x0106, 3, 1, 32803),
        (0x0111, 4, 1, 8 + 2 + 12 * 12 + 4),
        (0x0115, 3, 1, 1),
        (0x0116, 4, 1, 8),
        (0x0117, 4, 1, 256),
        (0x828D, 3, 2, 2 | (2 << 16)),
        (0x828E, 1, 4, u32::from_le_bytes([0, 1, 1, 2])),
        (0xC612, 1, 4, u32::from_le_bytes([1, 4, 0, 0])),
    ];
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend((e.len() as u16).to_le_bytes());
    for (tag, typ, count, v) in e {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(v.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes());
    out.extend((0..128u16).flat_map(|i| (i * 31 + 1).to_le_bytes()));
    out.truncate(out.len() - missing);
    out
}

#[test]
fn version_and_probe() {
    assert_eq!(cstr(flashraw_version()), Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(flashraw_abi_version(), FLASHRAW_ABI_VERSION);
    assert_eq!(flashraw_version_number() >> 16, env!("CARGO_PKG_VERSION_MAJOR").parse::<u32>().unwrap());
    let dng = std::fs::read(fixture("canon-a410-chdk.dng")).unwrap();
    // SAFETY: valid slices.
    unsafe {
        assert!(flashraw_supports(dng.as_ptr(), dng.len()));
        assert_eq!(cstr(flashraw_detect_format(dng.as_ptr(), dng.len())), Some("DNG"));
        assert!(!flashraw_supports(b"hello world".as_ptr(), 11));
        assert!(!flashraw_supports(null(), 0));
        assert!(flashraw_detect_format(null(), 0).is_null());
    }
    assert_eq!(cstr(flashraw_error_kind_name(3)), Some("CorruptFile"));
    assert_eq!(cstr(flashraw_error_kind_name(999)), Some("Unknown"));
    assert_eq!(cstr(flashraw_stage_name(2)), Some("Entropy decode"));
    assert_eq!(cstr(flashraw_output_format_name(4)), Some("rgba8"));
    assert!(flashraw_output_format_name(42).is_null());
    let mut o = -1;
    // SAFETY: valid string and out pointer.
    assert_eq!(unsafe { flashraw_output_format_parse(c"Linear_RGB".as_ptr(), &mut o) }, flashraw_status::FLASHRAW_OK);
    assert_eq!(o, flashraw_output_format::FLASHRAW_OUTPUT_LINEAR_RGB_F32 as i32);
}

#[test]
fn metadata_and_raw_decode_match_expected() {
    let f = open_path(&fixture("canon-a410-chdk.dng"));
    // SAFETY: `f` is a live handle for the whole block, freed at the end.
    unsafe {
        assert_eq!(cstr(flashraw_file_make(f)), Some("Canon"));
        assert_eq!(cstr(flashraw_file_canonical_model(f)), Some("PowerShot A410"));
        assert_eq!(cstr(flashraw_file_format_name(f)), Some("DNG"));
        assert_eq!(cstr(flashraw_file_string(f, 4)), Some("Canon PowerShot A410"));
        assert!(flashraw_file_string(f, 99).is_null());
        assert_eq!(flashraw_last_error_code(), flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT);
        assert_eq!((flashraw_file_raw_width(f), flashraw_file_raw_height(f)), (2144, 1560));
        assert_eq!(flashraw_file_cpp(f), 1);
        assert!((1..=8).contains(&flashraw_file_orientation(f)));
        assert_eq!(flashraw_file_sensor_layout(f), flashraw_sensor_layout::FLASHRAW_LAYOUT_BAYER as i32);
        let mut cfa = std::mem::zeroed::<flashraw_cfa>();
        assert!(flashraw_file_cfa(f, &mut cfa));
        assert_eq!((cfa.width, cfa.height, &cfa.colors[..4]), (2, 2, &[0u8, 1, 1, 2][..]));
        let mut white = [0u32; 8];
        let n = flashraw_file_white_level(f, white.as_mut_ptr(), white.len());
        assert!(n >= 1);
        assert_eq!(flashraw_file_white_level_max(f), 1023);
        let mut bi = flashraw_black_level::default();
        let mut bv = [0f32; 64];
        let nb = flashraw_file_black_level(f, &mut bi, bv.as_mut_ptr(), bv.len());
        assert_eq!(nb as u32, bi.count);
        assert!(bi.rows >= 1 && bi.cols >= 1);
        // Size query without a buffer.
        assert_eq!(flashraw_file_black_level(f, null_mut(), null_mut(), 0), nb);
        let mut active = flashraw_rect::default();
        assert_eq!(flashraw_file_active_area(f, &mut active), flashraw_status::FLASHRAW_OK);
        assert!(active.width > 0 && active.width <= 2144);
        let n_color = flashraw_file_color_matrix_count(f, 0);
        if n_color > 0 {
            let mut m = std::mem::zeroed::<flashraw_color_matrix>();
            assert_eq!(flashraw_file_color_matrix(f, 0, 0, &mut m), flashraw_status::FLASHRAW_OK);
            assert_eq!((m.rows, m.cols), (3, 3));
        }
        assert_eq!(
            flashraw_file_color_matrix(f, 0, 1000, &mut std::mem::zeroed()),
            flashraw_status::FLASHRAW_ERROR_NOT_AVAILABLE
        );
        assert_eq!(flashraw_file_color_matrix_count(f, 7), 0);
        assert_eq!(flashraw_last_error_code(), flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT);

        // JSON escape hatch.
        let j = flashraw_file_metadata_json(f);
        let v: serde_json::Value = serde_json::from_str(cstr(j).unwrap()).unwrap();
        flashraw_string_free(j);
        assert_eq!(v["format"], "DNG");
        assert_eq!(v["metadata"]["make"], "Canon");
        assert_eq!(v["frame_info"]["width"], 2144);
        assert_eq!(v["abi_version"], 1);

        // Default decode: raw u16, full buffer, pinned hash.
        let fr = decode(f, &options()).unwrap();
        assert_eq!((flashraw_frame_width(fr), flashraw_frame_height(fr), flashraw_frame_cpp(fr)), (2144, 1560, 1));
        assert_eq!(flashraw_frame_sample_type(fr), flashraw_sample_type::FLASHRAW_SAMPLE_U16 as i32);
        assert_eq!(flashraw_frame_output_format(fr), 0);
        assert_eq!(flashraw_frame_sample_count(fr), 2144 * 1560);
        assert_eq!(flashraw_frame_data_len(fr), 2144 * 1560 * 2);
        let data = std::slice::from_raw_parts(flashraw_frame_data(fr) as *const u16, flashraw_frame_sample_count(fr));
        assert_eq!(format!("{:016x}", fnv_u16(data)), "b2dd16c3b6c17497");
        assert!(!flashraw_frame_is_incomplete(fr));
        assert_eq!(flashraw_frame_warning_count(fr), 0);
        assert!(flashraw_frame_warning(fr, 0).is_null());
        assert!(cstr(flashraw_frame_backend(fr)).unwrap().starts_with("CPU"));
        assert!(flashraw_frame_postprocess_backend(fr).is_null());
        assert_eq!(flashraw_frame_color_at(fr, 0, 0), 0);
        assert_eq!(flashraw_frame_color_at(fr, 1, 1), 2);
        let mut st =
            flashraw_decode_stats { size: std::mem::size_of::<flashraw_decode_stats>() as u32, ..Default::default() };
        assert_eq!(flashraw_frame_stats(fr, &mut st), flashraw_status::FLASHRAW_OK);
        assert!(st.total_ms > 0.0 && st.threads >= 1);
        assert_eq!(st.output_bytes, 2144 * 1560 * 2);
        assert_eq!(st.size, std::mem::size_of::<flashraw_decode_stats>() as u32);
        let j = flashraw_frame_json(fr);
        let v: serde_json::Value = serde_json::from_str(cstr(j).unwrap()).unwrap();
        flashraw_string_free(j);
        assert_eq!(v["output"], "raw-u16");
        assert_eq!(v["incomplete"], false);
        flashraw_frame_free(fr);

        // Region decode.
        let mut o = options();
        o.use_region = 1;
        o.region = flashraw_rect { x: 100, y: 50, width: 64, height: 32 };
        let fr = decode(f, &o).unwrap();
        assert_eq!((flashraw_frame_width(fr), flashraw_frame_height(fr)), (64, 32));
        let (mut x, mut y) = (0, 0);
        assert_eq!(flashraw_frame_origin(fr, &mut x, &mut y), flashraw_status::FLASHRAW_OK);
        assert_eq!((x, y), (100, 50));
        flashraw_frame_free(fr);

        // RGB outputs.
        for (out, cpp, ty) in [
            (flashraw_output_format::FLASHRAW_OUTPUT_RGBA8, 4, flashraw_sample_type::FLASHRAW_SAMPLE_U8),
            (flashraw_output_format::FLASHRAW_OUTPUT_LINEAR_RGB_F32, 3, flashraw_sample_type::FLASHRAW_SAMPLE_F32),
            (flashraw_output_format::FLASHRAW_OUTPUT_NORMALIZED_F32, 1, flashraw_sample_type::FLASHRAW_SAMPLE_F32),
            (flashraw_output_format::FLASHRAW_OUTPUT_RAW_F32, 1, flashraw_sample_type::FLASHRAW_SAMPLE_F32),
            (flashraw_output_format::FLASHRAW_OUTPUT_RGB16, 3, flashraw_sample_type::FLASHRAW_SAMPLE_U16),
            (flashraw_output_format::FLASHRAW_OUTPUT_LINEAR_RGB16, 3, flashraw_sample_type::FLASHRAW_SAMPLE_U16),
            (flashraw_output_format::FLASHRAW_OUTPUT_LINEAR_RGB_F16, 3, flashraw_sample_type::FLASHRAW_SAMPLE_F16),
        ] {
            let mut o = options();
            o.output = out as i32;
            o.threads = 2;
            let fr = decode(f, &o).unwrap();
            assert_eq!(flashraw_frame_cpp(fr), cpp);
            assert_eq!(flashraw_frame_sample_type(fr), ty as i32);
            assert_eq!(flashraw_frame_output_format(fr), out as i32);
            let elem = match ty {
                flashraw_sample_type::FLASHRAW_SAMPLE_U8 => 1,
                flashraw_sample_type::FLASHRAW_SAMPLE_U16 | flashraw_sample_type::FLASHRAW_SAMPLE_F16 => 2,
                _ => 4,
            };
            assert_eq!(
                flashraw_frame_data_len(fr),
                (flashraw_frame_width(fr) * flashraw_frame_height(fr) * cpp) as usize * elem
            );
            flashraw_frame_free(fr);
        }
        // Exposure and black/white points: +1 EV doubles linear values.
        let mut o = options();
        o.output = flashraw_output_format::FLASHRAW_OUTPUT_LINEAR_RGB_F32 as i32;
        o.demosaic = flashraw_demosaic::FLASHRAW_DEMOSAIC_HALF_SIZE as i32;
        let base = decode(f, &o).unwrap();
        o.exposure = 1.0;
        let bright = decode(f, &o).unwrap();
        let n = flashraw_frame_sample_count(base);
        let a = std::slice::from_raw_parts(flashraw_frame_data(base) as *const f32, n);
        let b = std::slice::from_raw_parts(flashraw_frame_data(bright) as *const f32, n);
        assert!(a.iter().zip(b).all(|(x, y)| *y == *x * 2.0));
        flashraw_frame_free(base);
        flashraw_frame_free(bright);
        o.exposure = 0.0;
        o.black_point = 0.5;
        o.white_point = 0.5;
        let mut fr = null_mut();
        assert_eq!(flashraw_decode(f, &o, &mut fr), flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT);
        // Half-size demosaic.
        let mut o = options();
        o.output = flashraw_output_format::FLASHRAW_OUTPUT_RGBA8 as i32;
        o.demosaic = flashraw_demosaic::FLASHRAW_DEMOSAIC_HALF_SIZE as i32;
        o.crop = 0;
        let fr = decode(f, &o).unwrap();
        assert!(flashraw_frame_width(fr) <= 2144 / 2 + 1);
        flashraw_frame_free(fr);
        flashraw_file_free(f);
    }
}

#[test]
fn previews_are_zero_copy() {
    let f = open_path(&fixture("canon-5d3-lossy.dng"));
    // SAFETY: live handle until freed at the end.
    unsafe {
        let n = flashraw_file_preview_count(f);
        assert_eq!(n, 3);
        let largest = flashraw_file_largest_preview(f);
        assert!(largest >= 0 && (largest as usize) < n);
        let mut info =
            flashraw_preview_info { size: std::mem::size_of::<flashraw_preview_info>() as u32, ..Default::default() };
        assert_eq!(flashraw_file_preview_info(f, largest as usize, &mut info), flashraw_status::FLASHRAW_OK);
        let mut len = 0usize;
        let p = flashraw_file_preview_bytes(f, largest as usize, &mut len);
        assert!(!p.is_null());
        assert_eq!(len as u64, info.length);
        if info.encoding == flashraw_preview_encoding::FLASHRAW_PREVIEW_JPEG as i32 {
            assert_eq!(std::slice::from_raw_parts(p, 2), &[0xFF, 0xD8]);
        }
        assert!(flashraw_file_preview_bytes(f, 99, &mut len).is_null());
        assert_eq!(len, 0);
        assert_eq!(flashraw_last_error_code(), flashraw_status::FLASHRAW_ERROR_NOT_AVAILABLE);
        // An output struct with a bad size is rejected, not overrun.
        let mut bad = flashraw_preview_info::default();
        assert_eq!(flashraw_file_preview_info(f, 0, &mut bad), flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT);
        flashraw_file_free(f);
    }
}

#[test]
fn memory_sources() {
    let bytes = std::fs::read(fixture("canon-5d3-mlv.dng")).unwrap();
    // SAFETY: `bytes` outlives the borrowed handle, which is freed before it is dropped.
    unsafe {
        let mut f = null_mut();
        assert_eq!(flashraw_open_memory(bytes.as_ptr(), bytes.len(), null(), &mut f), flashraw_status::FLASHRAW_OK);
        let fr = decode(f, &options()).unwrap();
        let data = std::slice::from_raw_parts(flashraw_frame_data(fr) as *const u16, flashraw_frame_sample_count(fr));
        assert_eq!(format!("{:016x}", fnv_u16(data)), "ec88ca511154e121");
        flashraw_file_free(f);
        // Frames outlive their file.
        assert_eq!(flashraw_frame_width(fr), 1920);
        flashraw_frame_free(fr);

        let mut f = null_mut();
        let copy = bytes.clone();
        assert_eq!(flashraw_open_memory_copy(copy.as_ptr(), copy.len(), null(), &mut f), flashraw_status::FLASHRAW_OK);
        drop(copy);
        assert_eq!(cstr(flashraw_file_canonical_model(f)), Some("EOS 5D Mark III"));
        flashraw_file_free(f);
    }
}

#[test]
fn errors_and_null_pointers() {
    // SAFETY: deliberately invalid arguments the API must reject without UB (NULLs and bad
    // values only; no dangling pointers).
    unsafe {
        flashraw_clear_last_error();
        assert_eq!(flashraw_last_error_code(), flashraw_status::FLASHRAW_OK);
        assert!(flashraw_last_error_message().is_null());

        let mut f = null_mut();
        assert_eq!(flashraw_open_path(null(), null(), &mut f), flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT);
        assert!(f.is_null());
        assert!(last_error().contains("NULL"));
        assert_eq!(cstr(flashraw_last_error_kind()), Some("InvalidArgument"));
        assert_eq!(
            flashraw_open_path(c"/x".as_ptr(), null(), null_mut()),
            flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(
            flashraw_open_path(c"/definitely/not/here.dng".as_ptr(), null(), &mut f),
            flashraw_status::FLASHRAW_ERROR_IO
        );
        assert_eq!(cstr(flashraw_last_error_kind()), Some("Io"));

        let junk = b"this is not a raw file at all, just text";
        assert_eq!(
            flashraw_open_memory(junk.as_ptr(), junk.len(), null(), &mut f),
            flashraw_status::FLASHRAW_ERROR_UNSUPPORTED_FORMAT
        );
        assert!(f.is_null());
        assert_eq!(flashraw_open_memory(null(), 10, null(), &mut f), flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT);
        assert_ne!(flashraw_open_memory(null(), 0, null(), &mut f), flashraw_status::FLASHRAW_OK);

        // Getters on NULL handles.
        assert_eq!(flashraw_file_raw_width(null()), 0);
        assert!(flashraw_file_make(null()).is_null());
        assert!(flashraw_file_metadata_json(null()).is_null());
        assert_eq!(flashraw_frame_width(null()), 0);
        assert!(flashraw_frame_data(null()).is_null());
        assert_eq!(flashraw_last_error_code(), flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT);
        flashraw_file_free(null_mut());
        flashraw_frame_free(null_mut());
        flashraw_string_free(null_mut());

        // Bad options are rejected before decoding.
        let f = open_path(&fixture("canon-a410-chdk.dng"));
        let mut fr = null_mut();
        assert_eq!(flashraw_decode(f, null(), null_mut()), flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT);
        let mut o = options();
        o.output = 77;
        assert_eq!(decode(f, &o), Err(flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT));
        assert!(last_error().contains("output format"));
        let mut o = options();
        o.size = 0;
        assert_eq!(decode(f, &o), Err(flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT));
        let mut o = options();
        o.use_region = 1;
        o.region = flashraw_rect { x: 99999, y: 0, width: 4, height: 4 };
        assert_eq!(decode(f, &o), Err(flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT));
        let mut o = options();
        o.backend = flashraw_backend::FLASHRAW_BACKEND_GPU as i32;
        if !flashraw_gpu_available() {
            assert_eq!(decode(f, &o), Err(flashraw_status::FLASHRAW_ERROR_GPU_UNAVAILABLE));
        }
        let mut o = options();
        o.max_samples = 100;
        assert_eq!(decode(f, &o), Err(flashraw_status::FLASHRAW_ERROR_LIMIT_EXCEEDED));
        // NULL options = defaults.
        assert_eq!(flashraw_decode(f, null(), &mut fr), flashraw_status::FLASHRAW_OK);
        flashraw_frame_free(fr);
        flashraw_file_free(f);
    }
}

#[test]
fn versioned_structs() {
    let f = open_path(&fixture("canon-a410-chdk.dng"));
    // SAFETY: live handle; option buffers are larger than the sizes they declare.
    unsafe {
        // A caller built against a newer header: extra zero bytes are accepted...
        #[repr(C)]
        struct Newer {
            base: flashraw_decode_options,
            extra: [u64; 4],
        }
        let mut n = Newer { base: options(), extra: [0; 4] };
        n.base.size = std::mem::size_of::<Newer>() as u32;
        n.base.output = flashraw_output_format::FLASHRAW_OUTPUT_RAW_F32 as i32;
        let mut fr = null_mut();
        assert_eq!(flashraw_decode(f, &n.base, &mut fr), flashraw_status::FLASHRAW_OK);
        assert_eq!(flashraw_frame_sample_type(fr), flashraw_sample_type::FLASHRAW_SAMPLE_F32 as i32);
        flashraw_frame_free(fr);
        // ...but options this version does not know are refused.
        n.extra[1] = 5;
        assert_eq!(flashraw_decode(f, &n.base, &mut fr), flashraw_status::FLASHRAW_ERROR_INVALID_ARGUMENT);
        assert!(last_error().contains("unknown to this library version"));

        // Open options.
        let mut oo = std::mem::zeroed::<flashraw_open_options>();
        assert_eq!(flashraw_open_options_init(&mut oo), flashraw_status::FLASHRAW_OK);
        assert_eq!(oo.size as usize, std::mem::size_of::<flashraw_open_options>());
        oo.max_dimension = 100;
        let path = CString::new(fixture("canon-a410-chdk.dng").to_str().unwrap()).unwrap();
        let mut g = null_mut();
        let s = flashraw_open_path(path.as_ptr(), &oo, &mut g);
        // Either refused at open or at decode: the limit must apply.
        if s == flashraw_status::FLASHRAW_OK {
            assert_eq!(flashraw_decode(g, null(), &mut fr), flashraw_status::FLASHRAW_ERROR_LIMIT_EXCEEDED);
            flashraw_file_free(g);
        } else {
            assert_eq!(s, flashraw_status::FLASHRAW_ERROR_LIMIT_EXCEEDED);
        }
        flashraw_file_free(f);
    }
}

#[test]
fn preview_fallback() {
    let path = CString::new(fixture("nikon-he-synthetic.nef").to_str().unwrap()).unwrap();
    // SAFETY: live handles, freed below.
    unsafe {
        let mut f = null_mut();
        assert_eq!(flashraw_open_path(path.as_ptr(), null(), &mut f), flashraw_status::FLASHRAW_OK);
        let mut o = options();
        assert_eq!(o.preview_fallback, 0);
        o.output = flashraw_output_format::FLASHRAW_OUTPUT_RGBA8 as i32;
        assert_eq!(decode(f, &o), Err(flashraw_status::FLASHRAW_ERROR_UNSUPPORTED_COMPRESSION));
        o.preview_fallback = 1;
        let fr = decode(f, &o).unwrap();
        assert_eq!(flashraw_frame_source(fr), flashraw_pixel_source::FLASHRAW_SOURCE_PREVIEW as i32);
        assert!(flashraw_frame_is_incomplete(fr));
        assert!(cstr(flashraw_frame_warning(fr, 0)).unwrap().contains("embedded full-size preview"));
        assert_eq!((flashraw_frame_width(fr), flashraw_frame_height(fr), flashraw_frame_cpp(fr)), (64, 48, 4));
        let j = flashraw_frame_json(fr);
        let v: serde_json::Value = serde_json::from_str(cstr(j).unwrap()).unwrap();
        flashraw_string_free(j);
        assert_eq!(v["source"], "preview");
        assert_eq!(v["warnings"][0]["kind"], "preview-fallback");
        flashraw_frame_free(fr);
        // Raw outputs never fall back.
        o.output = flashraw_output_format::FLASHRAW_OUTPUT_RAW_U16 as i32;
        assert_eq!(decode(f, &o), Err(flashraw_status::FLASHRAW_ERROR_UNSUPPORTED_COMPRESSION));
        flashraw_file_free(f);
        assert_eq!(flashraw_frame_source(null()), flashraw_pixel_source::FLASHRAW_SOURCE_SENSOR as i32);
    }
}

#[test]
fn corruption_policies() {
    let bytes = truncated_dng(6);
    // SAFETY: `bytes` outlives the borrowed handle.
    unsafe {
        let mut f = null_mut();
        assert_eq!(flashraw_open_memory(bytes.as_ptr(), bytes.len(), null(), &mut f), flashraw_status::FLASHRAW_OK);
        let fr = decode(f, &options()).unwrap();
        assert!(flashraw_frame_is_incomplete(fr));
        assert_eq!(flashraw_frame_warning_count(fr), 1);
        assert!(cstr(flashraw_frame_warning(fr, 0)).unwrap().contains("strip"));
        assert_eq!(flashraw_frame_samples_filled(fr), 3);
        let j = flashraw_frame_json(fr);
        let v: serde_json::Value = serde_json::from_str(cstr(j).unwrap()).unwrap();
        flashraw_string_free(j);
        assert_eq!(v["warnings"][0]["kind"], "truncated");
        flashraw_frame_free(fr);

        let mut o = options();
        o.corruption_policy = flashraw_corruption_policy::FLASHRAW_CORRUPTION_STRICT as i32;
        assert_eq!(decode(f, &o), Err(flashraw_status::FLASHRAW_ERROR_CORRUPT_FILE));
        let detail: serde_json::Value = serde_json::from_str(cstr(flashraw_last_error_detail_json()).unwrap()).unwrap();
        assert_eq!(detail["kind"], "truncated");
        assert_eq!(detail["unit"], "strip");
        flashraw_file_free(f);
    }
}

#[test]
fn concurrent_decodes_of_one_file() {
    struct Shared(*mut flashraw_file);
    // SAFETY: the C API documents `const flashraw_file*` functions as thread-safe.
    unsafe impl Sync for Shared {}
    let f = Shared(open_path(&fixture("canon-5d3-mlv.dng")));
    let hashes: Vec<u64> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..4)
            .map(|i| {
                let f = &f;
                s.spawn(move || {
                    let mut o = options();
                    o.threads = 1 + i % 2;
                    let fr = decode(f.0, &o).unwrap();
                    // SAFETY: live frame.
                    let h = unsafe {
                        fnv_u16(std::slice::from_raw_parts(
                            flashraw_frame_data(fr) as *const u16,
                            flashraw_frame_sample_count(fr),
                        ))
                    };
                    // Errors are per thread: a failure here is not visible to the others.
                    // SAFETY: NULL argument, rejected.
                    assert_eq!(unsafe { flashraw_file_raw_width(null()) }, 0);
                    // SAFETY: live frame, freed once.
                    unsafe { flashraw_frame_free(fr) };
                    h
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(hashes.iter().all(|&h| format!("{h:016x}") == "ec88ca511154e121"));
    flashraw_clear_last_error();
    assert_eq!(flashraw_last_error_code(), flashraw_status::FLASHRAW_OK);
    // SAFETY: freed once, no other users left.
    unsafe { flashraw_file_free(f.0) };
}

/// Truncations and byte mutations of real files: every call returns a status, never
/// crashes or unwinds.
#[test]
fn malformed_inputs_never_crash() {
    let mut inputs = vec![truncated_dng(0), truncated_dng(100), truncated_dng(200)];
    for name in ["canon-a410-chdk.dng", "bmpcc-tiles.dng"] {
        let b = std::fs::read(fixture(name)).unwrap();
        for frac in [0.001, 0.01, 0.1, 0.5, 0.9, 0.999] {
            inputs.push(b[..(b.len() as f64 * frac) as usize].to_vec());
        }
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..6 {
            let mut m = b.clone();
            for _ in 0..32 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let i = (seed as usize) % m.len().min(4096);
                m[i] = (seed >> 32) as u8;
            }
            inputs.push(m);
        }
    }
    for input in &inputs {
        // SAFETY: `input` outlives the borrowed handle.
        unsafe {
            let mut f = null_mut();
            let s = flashraw_open_memory(input.as_ptr(), input.len(), null(), &mut f);
            if s != flashraw_status::FLASHRAW_OK {
                assert!(f.is_null());
                assert!(!flashraw_last_error_message().is_null());
                assert_ne!(s, flashraw_status::FLASHRAW_ERROR_INTERNAL, "{}", last_error());
                continue;
            }
            let j = flashraw_file_metadata_json(f);
            flashraw_string_free(j);
            for out in 0..5 {
                let mut o = options();
                o.output = out;
                let mut fr = null_mut();
                let s = flashraw_decode(f, &o, &mut fr);
                assert_ne!(s, flashraw_status::FLASHRAW_ERROR_INTERNAL, "{}", last_error());
                if s == flashraw_status::FLASHRAW_OK {
                    assert!(flashraw_frame_data_len(fr) > 0);
                    flashraw_frame_free(fr);
                } else {
                    assert!(fr.is_null());
                }
            }
            flashraw_file_free(f);
        }
    }
}

/// Every corpus file of a few formats opens and decodes through the C API like through
/// the Rust API (skipped without `FLASHRAW_CORPUS`).
#[test]
fn corpus_sample_matches_rust_api() {
    let Some(root) = std::env::var_os("FLASHRAW_CORPUS").map(PathBuf::from).filter(|p| p.is_dir()) else {
        eprintln!("FLASHRAW_CORPUS not set: skipping");
        return;
    };
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.metadata().map(|m| m.len() < 40 << 20).unwrap_or(false) {
                files.push(p);
            }
        }
    }
    files.sort();
    let step = (files.len() / 40).max(1);
    for p in files.iter().step_by(step) {
        let Ok(rust) = flashraw::FlashRaw::open_path(p) else { continue };
        let Ok(expected) = rust.decode(Default::default()) else { continue };
        let f = open_path(p);
        let fr = decode(f, &options()).unwrap_or_else(|s| panic!("{}: {s:?} {}", p.display(), last_error()));
        // SAFETY: live handles, freed below.
        unsafe {
            assert_eq!(flashraw_frame_width(fr), expected.width, "{}", p.display());
            assert_eq!(flashraw_frame_data_len(fr), expected.data.byte_len());
            if let Some(e) = expected.as_u16() {
                let d = std::slice::from_raw_parts(flashraw_frame_data(fr) as *const u16, e.len());
                assert!(d == e, "{}", p.display());
            }
            flashraw_frame_free(fr);
            flashraw_file_free(f);
        }
    }
}
