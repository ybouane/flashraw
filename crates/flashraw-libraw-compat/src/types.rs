//! `#[repr(C)]` mirrors of `include/libraw/libraw_types.h`. The `layout` test compiles the
//! header and compares every size and offset with these definitions.

use std::ffi::{c_char, c_int, c_uint, c_void};

#[cfg(unix)]
pub type time_t = libc::time_t;
#[cfg(not(unix))]
pub type time_t = i64;

pub const LIBRAW_CBLACK_SIZE: usize = 4104;
pub const LIBRAW_THUMBNAIL_MAXCOUNT: usize = 8;
pub const LIBRAW_XTRANS: u32 = 9;

/// `enum LibRaw_errors`.
pub mod err {
    use std::ffi::c_int;
    pub const LIBRAW_SUCCESS: c_int = 0;
    pub const LIBRAW_UNSPECIFIED_ERROR: c_int = -1;
    pub const LIBRAW_FILE_UNSUPPORTED: c_int = -2;
    pub const LIBRAW_REQUEST_FOR_NONEXISTENT_IMAGE: c_int = -3;
    pub const LIBRAW_OUT_OF_ORDER_CALL: c_int = -4;
    pub const LIBRAW_NO_THUMBNAIL: c_int = -5;
    pub const LIBRAW_UNSUPPORTED_THUMBNAIL: c_int = -6;
    pub const LIBRAW_INPUT_CLOSED: c_int = -7;
    pub const LIBRAW_NOT_IMPLEMENTED: c_int = -8;
    pub const LIBRAW_REQUEST_FOR_NONEXISTENT_THUMBNAIL: c_int = -9;
    pub const LIBRAW_UNSUFFICIENT_MEMORY: c_int = -100007;
    pub const LIBRAW_DATA_ERROR: c_int = -100008;
    pub const LIBRAW_IO_ERROR: c_int = -100009;
    pub const LIBRAW_CANCELLED_BY_CALLBACK: c_int = -100010;
    pub const LIBRAW_BAD_CROP: c_int = -100011;
    pub const LIBRAW_TOO_BIG: c_int = -100012;
    pub const LIBRAW_MEMPOOL_OVERFLOW: c_int = -100013;
}

/// `enum LibRaw_As_Shot_WB_Applied_codes` (`color.as_shot_wb_applied`).
pub mod aswb {
    use std::ffi::c_int;
    pub const LIBRAW_ASWB_APPLIED: c_int = 1;
    pub const LIBRAW_ASWB_CANON: c_int = 2;
    pub const LIBRAW_ASWB_NIKON: c_int = 4;
    pub const LIBRAW_ASWB_NIKON_SRAW: c_int = 8;
    pub const LIBRAW_ASWB_PENTAX: c_int = 16;
    pub const LIBRAW_ASWB_SONY: c_int = 32;
}

/// `enum LibRaw_progress` bits used here.
pub mod progress {
    pub const OPEN: u32 = 1;
    pub const IDENTIFY: u32 = 1 << 1;
    pub const SIZE_ADJUST: u32 = 1 << 2;
    pub const LOAD_RAW: u32 = 1 << 3;
    pub const RAW2_IMAGE: u32 = 1 << 4;
    pub const SCALE_COLORS: u32 = 1 << 9;
    pub const PRE_INTERPOLATE: u32 = 1 << 10;
    pub const INTERPOLATE: u32 = 1 << 11;
    pub const CONVERT_RGB: u32 = 1 << 18;
    pub const THUMB_LOAD: u32 = 1 << 28;
}

/// `enum LibRaw_thumbnail_formats` / `enum LibRaw_image_formats` values used here.
pub mod fmt {
    pub const LIBRAW_THUMBNAIL_UNKNOWN: i32 = 0;
    pub const LIBRAW_THUMBNAIL_JPEG: i32 = 1;
    pub const LIBRAW_THUMBNAIL_BITMAP: i32 = 2;
    pub const LIBRAW_THUMBNAIL_H265: i32 = 6;
    pub const LIBRAW_THUMBNAIL_JPEGXL: i32 = 7;
    pub const LIBRAW_IMAGE_JPEG: i32 = 1;
    pub const LIBRAW_IMAGE_BITMAP: i32 = 2;
    pub const LIBRAW_IMAGE_JPEGXL: i32 = 3;
    pub const LIBRAW_IMAGE_H265: i32 = 4;
    pub const LIBRAW_INTERNAL_THUMBNAIL_UNKNOWN: i32 = 0;
    pub const LIBRAW_INTERNAL_THUMBNAIL_JPEG: i32 = 4;
    pub const LIBRAW_INTERNAL_THUMBNAIL_PPM: i32 = 7;
    pub const LIBRAW_INTERNAL_THUMBNAIL_JPEGXL: i32 = 11;
}

pub type exif_parser_callback =
    Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_uint, *mut c_void, i64)>;
pub type data_callback = Option<unsafe extern "C" fn(*mut c_void, *const c_char, i64)>;
pub type progress_callback = Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int) -> c_int>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_decoder_info_t {
    pub decoder_name: *const c_char,
    pub decoder_flags: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_internal_output_params_t {
    pub mix_green: c_uint,
    pub raw_color: c_uint,
    pub zero_is_bad: c_uint,
    pub shrink: u16,
    pub fuji_width: u16,
}

#[repr(C)]
pub struct libraw_processed_image_t {
    pub type_: c_int,
    pub height: u16,
    pub width: u16,
    pub colors: u16,
    pub bits: u16,
    pub data_size: c_uint,
    pub data: [u8; 1],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_iparams_t {
    pub guard: [c_char; 4],
    pub make: [c_char; 64],
    pub model: [c_char; 64],
    pub software: [c_char; 64],
    pub normalized_make: [c_char; 64],
    pub normalized_model: [c_char; 64],
    pub maker_index: c_uint,
    pub raw_count: c_uint,
    pub dng_version: c_uint,
    pub is_foveon: c_uint,
    pub colors: c_int,
    pub filters: c_uint,
    pub xtrans: [[c_char; 6]; 6],
    pub xtrans_abs: [[c_char; 6]; 6],
    pub cdesc: [c_char; 5],
    pub xmplen: c_uint,
    pub xmpdata: *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_raw_inset_crop_t {
    pub cleft: u16,
    pub ctop: u16,
    pub cwidth: u16,
    pub cheight: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_image_sizes_t {
    pub raw_height: u16,
    pub raw_width: u16,
    pub height: u16,
    pub width: u16,
    pub top_margin: u16,
    pub left_margin: u16,
    pub iheight: u16,
    pub iwidth: u16,
    pub raw_pitch: c_uint,
    pub pixel_aspect: f64,
    pub flip: c_int,
    pub mask: [[c_int; 4]; 8],
    pub raw_aspect: u16,
    pub raw_inset_crops: [libraw_raw_inset_crop_t; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ph1_t {
    pub format: c_int,
    pub key_off: c_int,
    pub tag_21a: c_int,
    pub t_black: c_int,
    pub split_col: c_int,
    pub black_col: c_int,
    pub split_row: c_int,
    pub black_row: c_int,
    pub tag_210: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_dng_color_t {
    pub parsedfields: c_uint,
    pub illuminant: u16,
    pub calibration: [[f32; 4]; 4],
    pub colormatrix: [[f32; 3]; 4],
    pub forwardmatrix: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_dng_rawopcode_t {
    pub len: c_uint,
    pub data: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_dng_levels_t {
    pub parsedfields: c_uint,
    pub dng_cblack: [c_uint; LIBRAW_CBLACK_SIZE],
    pub dng_black: c_uint,
    pub dng_fcblack: [f32; LIBRAW_CBLACK_SIZE],
    pub dng_fblack: f32,
    pub dng_whitelevel: [c_uint; 4],
    pub default_crop: [u16; 4],
    pub user_crop: [f32; 4],
    pub preview_colorspace: c_uint,
    pub analogbalance: [f32; 4],
    pub asshotneutral: [f32; 4],
    pub baseline_exposure: f32,
    pub LinearResponseLimit: f32,
    pub rawopcodes: [libraw_dng_rawopcode_t; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_P1_color_t {
    pub romm_cam: [f32; 9],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_colordata_t {
    pub curve: [u16; 0x10000],
    pub cblack: [c_uint; LIBRAW_CBLACK_SIZE],
    pub black: c_uint,
    pub data_maximum: c_uint,
    pub maximum: c_uint,
    pub linear_max: [c_uint; 4],
    pub fmaximum: f32,
    pub fnorm: f32,
    pub white: [[u16; 8]; 8],
    pub cam_mul: [f32; 4],
    pub pre_mul: [f32; 4],
    pub cmatrix: [[f32; 4]; 3],
    pub ccm: [[f32; 4]; 3],
    pub rgb_cam: [[f32; 4]; 3],
    pub cam_xyz: [[f32; 3]; 4],
    pub phase_one_data: ph1_t,
    pub flash_used: f32,
    pub canon_ev: f32,
    pub model2: [c_char; 64],
    pub UniqueCameraModel: [c_char; 64],
    pub LocalizedCameraModel: [c_char; 64],
    pub ImageUniqueID: [c_char; 64],
    pub RawDataUniqueID: [c_char; 17],
    pub OriginalRawFileName: [c_char; 64],
    pub profile: *mut c_void,
    pub profile_length: c_uint,
    pub black_stat: [c_uint; 8],
    pub dng_color: [libraw_dng_color_t; 2],
    pub dng_levels: libraw_dng_levels_t,
    pub WB_Coeffs: [[c_int; 4]; 256],
    pub WBCT_Coeffs: [[f32; 5]; 64],
    pub as_shot_wb_applied: c_int,
    pub P1_color: [libraw_P1_color_t; 2],
    pub raw_bps: c_uint,
    pub ExifColorSpace: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_thumbnail_t {
    pub tformat: c_int,
    pub twidth: u16,
    pub theight: u16,
    pub tlength: c_uint,
    pub tcolors: c_int,
    pub thumb: *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_thumbnail_item_t {
    pub tformat: c_int,
    pub twidth: u16,
    pub theight: u16,
    pub tflip: u16,
    pub tlength: c_uint,
    pub tmisc: c_uint,
    pub toffset: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_thumbnail_list_t {
    pub thumbcount: c_int,
    pub thumblist: [libraw_thumbnail_item_t; LIBRAW_THUMBNAIL_MAXCOUNT],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_gps_info_t {
    pub latitude: [f32; 3],
    pub longitude: [f32; 3],
    pub gpstimestamp: [f32; 3],
    pub altitude: f32,
    pub altref: c_char,
    pub latref: c_char,
    pub longref: c_char,
    pub gpsstatus: c_char,
    pub gpsparsed: c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_imgother_t {
    pub iso_speed: f32,
    pub shutter: f32,
    pub aperture: f32,
    pub focal_len: f32,
    pub timestamp: time_t,
    pub shot_order: c_uint,
    pub gpsdata: [c_uint; 32],
    pub parsed_gps: libraw_gps_info_t,
    pub desc: [c_char; 512],
    pub artist: [c_char; 64],
    pub analogbalance: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_output_params_t {
    pub greybox: [c_uint; 4],
    pub cropbox: [c_uint; 4],
    pub aber: [f64; 4],
    pub gamm: [f64; 6],
    pub user_mul: [f32; 4],
    pub bright: f32,
    pub threshold: f32,
    pub half_size: c_int,
    pub four_color_rgb: c_int,
    pub highlight: c_int,
    pub use_auto_wb: c_int,
    pub use_camera_wb: c_int,
    pub use_camera_matrix: c_int,
    pub output_color: c_int,
    pub output_profile: *mut c_char,
    pub camera_profile: *mut c_char,
    pub bad_pixels: *mut c_char,
    pub dark_frame: *mut c_char,
    pub output_bps: c_int,
    pub output_tiff: c_int,
    pub output_flags: c_int,
    pub user_flip: c_int,
    pub user_qual: c_int,
    pub user_black: c_int,
    pub user_cblack: [c_int; 4],
    pub user_sat: c_int,
    pub med_passes: c_int,
    pub auto_bright_thr: f32,
    pub adjust_maximum_thr: f32,
    pub no_auto_bright: c_int,
    pub use_fuji_rotate: c_int,
    pub use_p1_correction: c_int,
    pub green_matching: c_int,
    pub dcb_iterations: c_int,
    pub dcb_enhance_fl: c_int,
    pub fbdd_noiserd: c_int,
    pub exp_correc: c_int,
    pub exp_shift: f32,
    pub exp_preser: f32,
    pub no_auto_scale: c_int,
    pub no_interpolation: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_raw_unpack_params_t {
    pub use_rawspeed: c_int,
    pub use_dngsdk: c_int,
    pub options: c_uint,
    pub shot_select: c_uint,
    pub specials: c_uint,
    pub max_raw_memory_mb: c_uint,
    pub sony_arw2_posterization_thr: c_int,
    pub coolscan_nef_gamma: f32,
    pub p4shot_order: [c_char; 5],
    pub custom_camera_strings: *mut *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_rawdata_t {
    pub raw_alloc: *mut c_void,
    pub raw_image: *mut u16,
    pub color4_image: *mut [u16; 4],
    pub color3_image: *mut [u16; 3],
    pub float_image: *mut f32,
    pub float3_image: *mut [f32; 3],
    pub float4_image: *mut [f32; 4],
    pub ph1_cblack: *mut [i16; 2],
    pub ph1_rblack: *mut [i16; 2],
    pub iparams: libraw_iparams_t,
    pub sizes: libraw_image_sizes_t,
    pub ioparams: libraw_internal_output_params_t,
    pub color: libraw_colordata_t,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_makernotes_lens_t {
    pub LensID: u64,
    pub Lens: [c_char; 128],
    pub LensFormat: u16,
    pub LensMount: u16,
    pub CamID: u64,
    pub CameraFormat: u16,
    pub CameraMount: u16,
    pub body: [c_char; 64],
    pub FocalType: i16,
    pub LensFeatures_pre: [c_char; 16],
    pub LensFeatures_suf: [c_char; 16],
    pub MinFocal: f32,
    pub MaxFocal: f32,
    pub MaxAp4MinFocal: f32,
    pub MaxAp4MaxFocal: f32,
    pub MinAp4MinFocal: f32,
    pub MinAp4MaxFocal: f32,
    pub MaxAp: f32,
    pub MinAp: f32,
    pub CurFocal: f32,
    pub CurAp: f32,
    pub MaxAp4CurFocal: f32,
    pub MinAp4CurFocal: f32,
    pub MinFocusDistance: f32,
    pub FocusRangeIndex: f32,
    pub LensFStops: f32,
    pub TeleconverterID: u64,
    pub Teleconverter: [c_char; 128],
    pub AdapterID: u64,
    pub Adapter: [c_char; 128],
    pub AttachmentID: u64,
    pub Attachment: [c_char; 128],
    pub FocalUnits: u16,
    pub FocalLengthIn35mmFormat: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_nikonlens_t {
    pub EffectiveMaxAp: f32,
    pub LensIDNumber: u8,
    pub LensFStops: u8,
    pub MCUVersion: u8,
    pub LensType: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_dnglens_t {
    pub MinFocal: f32,
    pub MaxFocal: f32,
    pub MaxAp4MinFocal: f32,
    pub MaxAp4MaxFocal: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_lensinfo_t {
    pub MinFocal: f32,
    pub MaxFocal: f32,
    pub MaxAp4MinFocal: f32,
    pub MaxAp4MaxFocal: f32,
    pub EXIF_MaxAp: f32,
    pub LensMake: [c_char; 128],
    pub Lens: [c_char; 128],
    pub LensSerial: [c_char; 128],
    pub InternalLensSerial: [c_char; 128],
    pub FocalLengthIn35mmFormat: u16,
    pub nikon: libraw_nikonlens_t,
    pub dng: libraw_dnglens_t,
    pub makernotes: libraw_makernotes_lens_t,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_shootinginfo_t {
    pub DriveMode: i16,
    pub FocusMode: i16,
    pub MeteringMode: i16,
    pub AFPoint: i16,
    pub ExposureMode: i16,
    pub ExposureProgram: i16,
    pub ImageStabilization: i16,
    pub BodySerial: [c_char; 64],
    pub InternalBodySerial: [c_char; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct libraw_data_t {
    pub image: *mut [u16; 4],
    pub sizes: libraw_image_sizes_t,
    pub idata: libraw_iparams_t,
    pub lens: libraw_lensinfo_t,
    pub shootinginfo: libraw_shootinginfo_t,
    pub params: libraw_output_params_t,
    pub rawparams: libraw_raw_unpack_params_t,
    pub progress_flags: c_uint,
    pub process_warnings: c_uint,
    pub color: libraw_colordata_t,
    pub other: libraw_imgother_t,
    pub thumbnail: libraw_thumbnail_t,
    pub thumbs_list: libraw_thumbnail_list_t,
    pub rawdata: libraw_rawdata_t,
    pub parent_class: *mut c_void,
}

/// Copy `s` into a fixed C string field (truncated, always NUL-terminated).
pub fn set_cstr(dst: &mut [c_char], s: &str) {
    dst.fill(0);
    let n = s.len().min(dst.len().saturating_sub(1));
    for (d, &b) in dst.iter_mut().zip(&s.as_bytes()[..n]) {
        *d = b as c_char;
    }
}

/// A fixed C string field as a Rust string (up to the first NUL).
pub fn get_cstr(src: &[c_char]) -> String {
    let bytes: Vec<u8> = src.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
