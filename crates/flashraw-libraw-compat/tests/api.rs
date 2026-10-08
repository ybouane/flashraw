//! The `libraw_*` functions called the way C clients call them: call order, error codes,
//! NULL handles, malformed input, and the values a typical client reads.

use raw::*;
use std::ffi::{CStr, CString, c_int};
use std::path::PathBuf;
use std::ptr::{null, null_mut};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures").join(name)
}

fn cstr<'a>(p: *const std::ffi::c_char) -> &'a str {
    assert!(!p.is_null());
    // SAFETY: the library returns NUL-terminated strings.
    unsafe { CStr::from_ptr(p) }.to_str().unwrap()
}

fn chars(s: &[std::ffi::c_char]) -> String {
    get_cstr(s)
}

struct Handle(*mut libraw_data_t);
impl Handle {
    fn new() -> Self {
        let p = libraw_init(0);
        assert!(!p.is_null());
        Handle(p)
    }
    fn d(&self) -> &libraw_data_t {
        // SAFETY: live handle owned by this test.
        unsafe { &*self.0 }
    }
    fn d_mut(&mut self) -> &mut libraw_data_t {
        // SAFETY: live handle owned by this test.
        unsafe { &mut *self.0 }
    }
    fn open(&mut self, p: &std::path::Path) -> c_int {
        let c = CString::new(p.to_str().unwrap()).unwrap();
        // SAFETY: live handle, valid string.
        unsafe { libraw_open_file(self.0, c.as_ptr()) }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: closed exactly once.
        unsafe { libraw_close(self.0) };
    }
}

#[test]
fn version_and_strings() {
    assert_eq!(cstr(libraw_version()), "0.22.0-FlashRaw");
    assert_eq!(libraw_versionNumber(), 22 << 8);
    assert_eq!(cstr(libraw_strerror(err::LIBRAW_FILE_UNSUPPORTED)), "Unsupported file format or not a RAW file");
    assert_eq!(cstr(libraw_strerror(12345)), "Unknown error code");
    assert_eq!(cstr(libraw_strprogress(8)), "Reading RAW data");
    assert_eq!(libraw_cameraCount(), 0);
    // SAFETY: the list is NULL-terminated.
    assert!(unsafe { *libraw_cameraList() }.is_null());
}

#[test]
fn typical_client_sequence() {
    let mut h = Handle::new();
    assert_eq!(h.d().params.output_bps, 8);
    assert_eq!(h.d().params.user_flip, -1);
    assert!((h.d().params.gamm[0] - 0.45).abs() < 1e-9);
    assert_eq!(h.open(&fixture("canon-a410-chdk.dng")), 0);
    let d = h.d();
    assert_eq!(chars(&d.idata.make), "Canon");
    assert_eq!(chars(&d.idata.model), "PowerShot A410");
    assert_eq!(chars(&d.idata.cdesc), "RGBG");
    assert_eq!((d.idata.colors, d.idata.filters), (3, 0xB4B4_B4B4));
    assert_eq!((d.sizes.raw_width, d.sizes.raw_height), (2144, 1560));
    assert_eq!((d.sizes.width, d.sizes.height, d.sizes.left_margin, d.sizes.top_margin), (2088, 1552, 2, 6));
    assert_eq!((d.sizes.iwidth, d.sizes.iheight), (2088, 1552));
    assert_eq!(d.sizes.raw_pitch, 2144 * 2);
    assert_eq!((d.color.black, d.color.maximum), (31, 1023));
    assert_eq!(d.other.iso_speed, 50.0);
    assert!(d.color.pre_mul[0] > 0.0 && d.color.rgb_cam[0][0] > 1.0);
    assert_eq!(d.color.as_shot_wb_applied, 0);
    assert!(d.rawdata.raw_image.is_null());
    // SAFETY: live handle.
    unsafe {
        assert_eq!(libraw_get_raw_width(h.0), 2144);
        assert_eq!(libraw_get_color_maximum(h.0), 1023);
        assert!((libraw_get_cam_mul(h.0, 0) - 1.7455).abs() < 1e-3);
        assert_eq!(libraw_get_cam_mul(h.0, 9), -999.0);
        assert_eq!(libraw_COLOR(h.0, 0, 0), 0);
        assert_eq!(libraw_COLOR(h.0, 1, 0), 3);
        // Processing before unpacking is an out-of-order call.
        assert_eq!(libraw_dcraw_process(h.0), err::LIBRAW_OUT_OF_ORDER_CALL);
        let mut e = 0;
        assert!(libraw_dcraw_make_mem_image(h.0, &mut e).is_null());
        assert_eq!(e, err::LIBRAW_OUT_OF_ORDER_CALL);

        assert_eq!(libraw_unpack(h.0), 0);
        let d = h.d();
        assert!(!d.rawdata.raw_image.is_null());
        assert_eq!(d.rawdata.raw_alloc as *mut u16, d.rawdata.raw_image);
        let raw = std::slice::from_raw_parts(d.rawdata.raw_image, 2144 * 1560);
        // Sensor data as stored (DNG opcode lists are not applied by unpack).
        let mut fnv: u64 = 0xcbf2_9ce4_8422_2325;
        for b in raw.iter().flat_map(|v| v.to_le_bytes()) {
            fnv ^= b as u64;
            fnv = fnv.wrapping_mul(0x0000_0100_0000_01b3);
        }
        assert_eq!(format!("{fnv:016x}"), "437b0ba5c16a94b6");

        // half-size output geometry
        h.d_mut().params.half_size = 1;
        assert_eq!(libraw_adjust_sizes_info_only(h.0), 0);
        assert_eq!((h.d().sizes.iwidth, h.d().sizes.iheight), (1044, 776));
        h.d_mut().params.half_size = 0;

        // raw2image puts each sample in its colour channel.
        assert_eq!(libraw_raw2image(h.0), 0);
        let img = std::slice::from_raw_parts(h.d().image, 2088 * 1552);
        assert_eq!(img[0][0], raw[6 * 2144 + 2]);
        assert_eq!(img[0][1], 0);
        assert!(h.d().color.data_maximum > 0);
        libraw_free_image(h.0);
        assert!(h.d().image.is_null());

        h.d_mut().params.use_camera_wb = 1;
        assert_eq!(libraw_dcraw_process(h.0), 0);
        assert_eq!((libraw_get_iwidth(h.0), libraw_get_iheight(h.0)), (2088, 1552));
        let mut e = -1;
        let img = libraw_dcraw_make_mem_image(h.0, &mut e);
        assert_eq!(e, 0);
        let p = &*img;
        assert_eq!((p.type_, p.width, p.height, p.colors, p.bits), (2, 2088, 1552, 3, 8));
        assert_eq!(p.data_size, 2088 * 1552 * 3);
        let px = std::slice::from_raw_parts(p.data.as_ptr(), p.data_size as usize);
        let mean = px.iter().map(|&v| v as f64).sum::<f64>() / px.len() as f64;
        assert!(mean > 20.0 && mean < 200.0, "{mean}");
        libraw_dcraw_clear_mem(img);

        // Grey-world balance: the developed channel means move closer together.
        let spread = |img: *mut libraw_processed_image_t| {
            let p = &*img;
            let px = std::slice::from_raw_parts(p.data.as_ptr(), p.data_size as usize);
            let m: Vec<f64> = (0..3).map(|c| px.iter().skip(c).step_by(3).map(|&v| v as f64).sum::<f64>()).collect();
            libraw_dcraw_clear_mem(img);
            m.iter().copied().fold(f64::MIN, f64::max) / m.iter().copied().fold(f64::MAX, f64::min)
        };
        let camera = spread(libraw_dcraw_make_mem_image(h.0, null_mut()));
        h.d_mut().params.use_auto_wb = 1;
        assert_eq!(libraw_dcraw_process(h.0), 0);
        let auto = spread(libraw_dcraw_make_mem_image(h.0, null_mut()));
        assert!(auto < camera, "auto {auto} camera {camera}");
        h.d_mut().params.use_auto_wb = 0;

        // 16-bit output and a 90-degree flip swap the dimensions.
        h.d_mut().params.output_bps = 16;
        h.d_mut().params.user_flip = 6;
        let img = libraw_dcraw_make_mem_image(h.0, null_mut());
        assert_eq!(((*img).width, (*img).height, (*img).bits), (1552, 2088, 16));
        libraw_dcraw_clear_mem(img);

        assert_eq!(libraw_unpack_thumb(h.0), 0);
        assert_eq!(h.d().thumbnail.tformat, 2);
        let t = libraw_dcraw_make_mem_thumb(h.0, &mut e);
        assert_eq!(((*t).type_, (*t).width, (*t).height, (*t).data_size), (2, 128, 96, 128 * 96 * 3));
        libraw_dcraw_clear_mem(t);
        assert_eq!(libraw_unpack_thumb_ex(h.0, 5), err::LIBRAW_REQUEST_FOR_NONEXISTENT_THUMBNAIL);

        let mut info = std::mem::zeroed::<libraw_decoder_info_t>();
        assert_eq!(libraw_get_decoder_info(h.0, &mut info), 0);
        assert_eq!(cstr(info.decoder_name), "flashraw_dng");

        // recycle keeps params, drops the file
        libraw_recycle(h.0);
        assert_eq!(h.d().params.output_bps, 16);
        assert_eq!(h.d().sizes.raw_width, 0);
        assert!(h.d().rawdata.raw_image.is_null());
        assert_eq!(libraw_unpack(h.0), err::LIBRAW_OUT_OF_ORDER_CALL);
    }
}

/// Camera-balanced images (Sony lossless YCbCr M/S, Nikon sNEF): `as_shot_wb_applied` set,
/// and processing with the camera balance uses unit multipliers (no magenta cast); without
/// it the camera balance is divided out of the daylight multipliers. Runs on the CC0 corpus
/// (`$FLASHRAW_CORPUS`), skipped when absent.
#[test]
fn camera_applied_white_balance() {
    let Some(root) = std::env::var_os("FLASHRAW_CORPUS").map(PathBuf::from).filter(|p| p.is_dir()) else {
        eprintln!("FLASHRAW_CORPUS not set: skipping");
        return;
    };
    for (rel, code) in [
        ("Sony/ILCE-7RM5/7RM5-LosslessCompressedSmall.ARW", 0x21),
        ("Nikon/D810/DSC_1888.NEF", 0x9),
        ("Nikon/D810/DSC_1887.NEF", 0),
    ] {
        let p = root.join(rel);
        if !p.exists() {
            continue;
        }
        let mut h = Handle::new();
        assert_eq!(h.open(&p), 0, "{rel}");
        assert_eq!(h.d().color.as_shot_wb_applied, code, "{rel}");
        let means = |h: &mut Handle| {
            // SAFETY: live handle; the image is freed before returning.
            unsafe {
                assert_eq!(libraw_dcraw_process(h.0), 0);
                let img = libraw_dcraw_make_mem_image(h.0, null_mut());
                let p = &*img;
                let px = std::slice::from_raw_parts(p.data.as_ptr(), p.data_size as usize);
                let m: Vec<f64> =
                    (0..3).map(|c| px.iter().skip(c).step_by(3).map(|&v| v as f64).sum::<f64>()).collect();
                libraw_dcraw_clear_mem(img);
                m
            }
        };
        h.d_mut().params.half_size = 1;
        // SAFETY: live handle.
        assert_eq!(unsafe { libraw_unpack(h.0) }, 0);
        h.d_mut().params.use_camera_wb = 1;
        let camera = means(&mut h);
        assert!(camera[1] > 0.8 * camera[0].max(camera[2]), "{rel}: camera WB means {camera:?}");
        h.d_mut().params.use_camera_wb = 0;
        let daylight = means(&mut h);
        assert!(daylight[1] > 0.6 * daylight[0].max(daylight[2]), "{rel}: daylight means {daylight:?}");
    }
}

#[test]
fn jpeg_thumbnail_and_buffer_input() {
    let bytes = std::fs::read(fixture("canon-5d3-lossy.dng")).unwrap();
    let h = Handle::new();
    // SAFETY: `bytes` outlives the handle's input (closed in Drop before `bytes` drops).
    unsafe {
        assert_eq!(libraw_open_buffer(h.0, bytes.as_ptr().cast(), bytes.len()), 0);
        let d = h.d();
        assert_eq!(d.thumbs_list.thumbcount, 3);
        assert_eq!((d.thumbnail.twidth, d.thumbnail.theight), (3960, 2640));
        assert_eq!(libraw_unpack_thumb(h.0), 0);
        assert_eq!(h.d().thumbnail.tformat, 1);
        let mut e = 0;
        let t = libraw_dcraw_make_mem_thumb(h.0, &mut e);
        assert_eq!((*t).type_, 1);
        assert_eq!(std::slice::from_raw_parts((*t).data.as_ptr(), 2), &[0xFF, 0xD8]);
        libraw_dcraw_clear_mem(t);
        // Three samples per pixel: color4_image.
        assert_eq!(libraw_unpack(h.0), 0);
        assert!(h.d().rawdata.raw_image.is_null());
        assert!(!h.d().rawdata.color4_image.is_null());
        assert_eq!(h.d().idata.filters, 0);
        assert_eq!(libraw_COLOR(h.0, 0, 0), 6);
        assert_eq!(libraw_dcraw_process(h.0), 0);
        let img = libraw_dcraw_make_mem_image(h.0, &mut e);
        assert_eq!(((*img).width, (*img).height), (3960, 2640));
        libraw_dcraw_clear_mem(img);
        libraw_recycle_datastream(h.0);
        assert_eq!(libraw_unpack(h.0), err::LIBRAW_INPUT_CLOSED);
    }
}

#[test]
fn null_handles_and_bad_input() {
    // SAFETY: NULL handles and bad arguments must be rejected without UB.
    unsafe {
        assert_eq!(libraw_open_file(null_mut(), c"/x".as_ptr()), 22);
        assert_eq!(libraw_unpack(null_mut()), 22);
        assert_eq!(libraw_dcraw_process(null_mut()), 22);
        assert_eq!(libraw_get_raw_width(null_mut()), 22);
        assert_eq!(libraw_get_pre_mul(null_mut(), 0), -999.0);
        assert!(libraw_get_iparams(null_mut()).is_null());
        let mut e = 0;
        assert!(libraw_dcraw_make_mem_image(null_mut(), &mut e).is_null());
        assert_eq!(e, 22);
        libraw_close(null_mut());
        libraw_recycle(null_mut());
        libraw_dcraw_clear_mem(null_mut());

        let h = Handle::new();
        assert_eq!(libraw_open_file(h.0, null()), 22);
        assert_eq!(libraw_open_file(h.0, c"/definitely/missing.nef".as_ptr()), err::LIBRAW_IO_ERROR);
        assert!(!cstr(libraw_flashraw_last_error(h.0)).is_empty());
        let junk = b"not a raw file, just some text that is long enough to look at";
        assert_eq!(libraw_open_buffer(h.0, junk.as_ptr().cast(), junk.len()), err::LIBRAW_FILE_UNSUPPORTED);
        assert_eq!(libraw_open_buffer(h.0, null(), 10), 22);
        assert_eq!(libraw_unpack(h.0), err::LIBRAW_OUT_OF_ORDER_CALL);
        assert_eq!(libraw_unpack_thumb(h.0), err::LIBRAW_OUT_OF_ORDER_CALL);
        assert_eq!(libraw_open_bayer(h.0, null_mut(), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0), err::LIBRAW_NOT_IMPLEMENTED);
        h.0.as_mut().unwrap().params.output_tiff = 1;
        assert_eq!(libraw_dcraw_ppm_tiff_writer(h.0, c"/dev/null".as_ptr()), err::LIBRAW_NOT_IMPLEMENTED);
    }
}

/// Truncated and mutated files: every call returns a code, nothing crashes.
#[test]
fn malformed_inputs_never_crash() {
    let mut inputs = vec![];
    for name in ["canon-a410-chdk.dng", "bmpcc-tiles.dng", "canon-5d3-mlv.dng"] {
        let b = std::fs::read(fixture(name)).unwrap();
        for frac in [0.0005, 0.01, 0.2, 0.6, 0.97] {
            inputs.push(b[..(b.len() as f64 * frac) as usize].to_vec());
        }
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..4 {
            let mut m = b.clone();
            for _ in 0..24 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let i = (seed as usize) % m.len().min(8192);
                m[i] = (seed >> 40) as u8;
            }
            inputs.push(m);
        }
    }
    for input in &inputs {
        let mut h = Handle::new();
        // SAFETY: `input` outlives the handle (dropped at the end of the iteration).
        unsafe {
            if libraw_open_buffer(h.0, input.as_ptr().cast(), input.len()) != 0 {
                continue;
            }
            if libraw_unpack(h.0) == 0 {
                h.d_mut().params.half_size = 1;
                if libraw_dcraw_process(h.0) == 0 {
                    let img = libraw_dcraw_make_mem_image(h.0, null_mut());
                    libraw_dcraw_clear_mem(img);
                }
                let _ = libraw_raw2image(h.0);
                libraw_subtract_black(h.0);
            }
            let _ = libraw_unpack_thumb(h.0);
        }
    }
}
