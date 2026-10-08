//! The processor behind a `libraw_data_t`: opening, metadata ("identify"), unpacking and
//! the raw-image helpers. All functions here work on safe references; `api.rs` owns the
//! pointer handling.

use crate::color::{self, CfaModel};
use crate::types::*;
use flashraw::{CorruptionPolicy, DecodeOptions, FlashRaw, FrameInfo, PreviewEncoding, PreviewInfo, RawMetadata, Rect};
use std::ffi::{CString, c_int};

/// Types for which the all-zero byte pattern is a valid value (integers, floats, raw
/// pointers, arrays and `repr(C)` structs of those).
///
/// # Safety
/// Implement only for such types.
pub unsafe trait Pod: Copy {}
macro_rules! pod {
    ($($t:ty),*) => { $(
        // SAFETY: plain `repr(C)` aggregate of integers, floats, raw pointers and nullable
        // function pointers (`Option<extern fn>`), all valid when zeroed.
        unsafe impl Pod for $t {}
    )* };
}
pod!(
    libraw_data_t,
    libraw_image_sizes_t,
    libraw_iparams_t,
    libraw_lensinfo_t,
    libraw_shootinginfo_t,
    libraw_output_params_t,
    libraw_raw_unpack_params_t,
    libraw_colordata_t,
    libraw_imgother_t,
    libraw_thumbnail_t,
    libraw_thumbnail_list_t,
    libraw_rawdata_t
);

/// Reset `x` to all zeroes.
pub fn zero<T: Pod>(x: &mut T) {
    // SAFETY: `x` is a valid, exclusive reference and all-zero is a valid `T` (Pod).
    unsafe { std::ptr::write_bytes(x as *mut T, 0, 1) };
}

/// Private state of a handle, reachable from `libraw_data_t::parent_class`.
#[derive(Default)]
pub struct State {
    /// The opened file (the "datastream").
    pub raw: Option<FlashRaw<'static>>,
    /// Geometry and calibration captured at open.
    pub info: Option<FrameInfo>,
    /// Container format name.
    pub format: &'static str,
    /// Samples per pixel of the sensor data.
    pub cpp: u32,
    /// Backing store of `rawdata.raw_alloc` (1 or 4 samples per pixel).
    pub raw_buf: Vec<u16>,
    /// Samples per pixel stored in `raw_buf`.
    pub raw_buf_cpp: u32,
    /// Backing store of `imgdata.image`.
    pub image_buf: Vec<[u16; 4]>,
    /// Backing store of `thumbnail.thumb`.
    pub thumb_buf: Vec<u8>,
    /// Message of the last FlashRaw error on this handle.
    pub last_error: CString,
}

impl State {
    pub fn cfa(&self, s: &libraw_image_sizes_t) -> CfaModel {
        CfaModel::new(self.info.as_ref().and_then(|i| i.cfa), self.cpp, s.left_margin as u32, s.top_margin as u32)
    }

    pub fn set_error(&mut self, e: &dyn std::fmt::Display) {
        self.last_error = CString::new(e.to_string().replace('\0', "")).unwrap_or_default();
    }
}

/// LibRaw defaults for `params` and `rawparams`.
pub fn default_params(d: &mut libraw_data_t) {
    let p = &mut d.params;
    zero(p);
    p.greybox = [0, 0, u32::MAX, u32::MAX];
    p.cropbox = [0, 0, u32::MAX, u32::MAX];
    p.aber = [1.0; 4];
    p.gamm[0] = 0.45;
    p.gamm[1] = 4.5;
    let g = color::gamma_params(0.45, 4.5);
    p.gamm[2..6].copy_from_slice(&g);
    p.bright = 1.0;
    p.use_camera_matrix = 1;
    p.output_color = 1;
    p.output_bps = 8;
    p.output_flags = 1;
    p.user_flip = -1;
    p.user_qual = -1;
    p.user_black = -1;
    p.user_cblack = [-1_000_001; 4];
    p.user_sat = -1;
    p.auto_bright_thr = 0.01;
    p.adjust_maximum_thr = 0.75;
    p.use_fuji_rotate = 1;
    p.use_p1_correction = 1;
    p.dcb_iterations = -1;
    p.exp_shift = 1.0;
    let r = &mut d.rawparams;
    zero(r);
    r.max_raw_memory_mb = 2048;
    r.coolscan_nef_gamma = 1.0;
    set_cstr(&mut r.p4shot_order, "0123");
}

/// Forget everything about the current file, keeping the processing parameters.
pub fn recycle(d: &mut libraw_data_t, st: &mut State) {
    let (params, rawparams, parent) = (d.params, d.rawparams, d.parent_class);
    zero(d);
    d.params = params;
    d.rawparams = rawparams;
    d.parent_class = parent;
    let last_error = std::mem::take(&mut st.last_error);
    *st = State { last_error, ..Default::default() };
}

/// LibRaw status for an error while opening.
pub fn open_error(e: &flashraw::Error) -> c_int {
    match e.kind() {
        "Io" => err::LIBRAW_IO_ERROR,
        "OutOfMemory" => err::LIBRAW_UNSUFFICIENT_MEMORY,
        "LimitExceeded" => err::LIBRAW_TOO_BIG,
        _ => err::LIBRAW_FILE_UNSUPPORTED,
    }
}

/// LibRaw status for an error while decoding.
pub fn decode_error(e: &flashraw::Error) -> c_int {
    match e.kind() {
        "Io" => err::LIBRAW_IO_ERROR,
        "OutOfMemory" => err::LIBRAW_UNSUFFICIENT_MEMORY,
        "LimitExceeded" => err::LIBRAW_TOO_BIG,
        "CorruptFile" | "InvalidMetadata" => err::LIBRAW_DATA_ERROR,
        "UnsupportedFormat" | "UnsupportedCompression" | "CameraUnsupported" => err::LIBRAW_FILE_UNSUPPORTED,
        _ => err::LIBRAW_UNSPECIFIED_ERROR,
    }
}

fn u16_dim(v: u32) -> Option<u16> {
    u16::try_from(v).ok()
}

/// LibRaw's internal thumbnail format code for a preview.
fn internal_thumb_format(e: PreviewEncoding) -> i32 {
    match e {
        PreviewEncoding::Jpeg => fmt::LIBRAW_INTERNAL_THUMBNAIL_JPEG,
        PreviewEncoding::Rgb8 => fmt::LIBRAW_INTERNAL_THUMBNAIL_PPM,
        PreviewEncoding::JpegXl => fmt::LIBRAW_INTERNAL_THUMBNAIL_JPEGXL,
        _ => fmt::LIBRAW_INTERNAL_THUMBNAIL_UNKNOWN,
    }
}

/// LibRaw's public thumbnail format code for a preview.
pub fn thumb_format(e: PreviewEncoding) -> i32 {
    match e {
        PreviewEncoding::Jpeg => fmt::LIBRAW_THUMBNAIL_JPEG,
        PreviewEncoding::Rgb8 => fmt::LIBRAW_THUMBNAIL_BITMAP,
        PreviewEncoding::JpegXl => fmt::LIBRAW_THUMBNAIL_JPEGXL,
        PreviewEncoding::Heif => fmt::LIBRAW_THUMBNAIL_H265,
        _ => fmt::LIBRAW_THUMBNAIL_UNKNOWN,
    }
}

/// Parse `YYYY-MM-DDTHH:MM:SS[...]` into a time stamp. Like LibRaw, the recorded time is
/// taken as local time (a recorded zone offset is ignored) and converted with the C library
/// where available.
pub fn timestamp(s: &str) -> Option<time_t> {
    let num = |a: usize, n: usize| -> Option<i64> { s.get(a..a + n)?.parse::<i64>().ok() };
    let (y, mo, d, h, mi, se) = (num(0, 4)?, num(5, 2)?, num(8, 2)?, num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || y < 1900 || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    local_time(y, mo, d, h, mi, se)
}

#[cfg(not(unix))]
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(unix)]
fn local_time(y: i64, mo: i64, d: i64, h: i64, mi: i64, se: i64) -> Option<time_t> {
    // SAFETY: `tm` is a plain C struct (all-zero is valid); `mktime` only reads and
    // normalises it.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = (y - 1900) as c_int;
        tm.tm_mon = (mo - 1) as c_int;
        tm.tm_mday = d as c_int;
        tm.tm_hour = h as c_int;
        tm.tm_min = mi as c_int;
        tm.tm_sec = se as c_int;
        tm.tm_isdst = -1;
        let t = libc::mktime(&mut tm);
        (t != -1).then_some(t)
    }
}

#[cfg(not(unix))]
fn local_time(y: i64, mo: i64, d: i64, h: i64, mi: i64, se: i64) -> Option<time_t> {
    // No portable local-time conversion: interpreted as UTC (documented deviation).
    Some((days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se) as time_t)
}

fn dms(v: f64) -> [f32; 3] {
    let a = v.abs();
    let deg = a.trunc();
    let min = ((a - deg) * 60.0).trunc();
    let sec = ((a - deg) * 60.0 - min) * 60.0;
    [deg as f32, min as f32, sec as f32]
}

/// Fill sizes / idata / color / other / lens / thumbnail from the opened file.
pub fn identify(d: &mut libraw_data_t, st: &mut State) -> c_int {
    let Some(raw) = st.raw.as_ref() else { return err::LIBRAW_OUT_OF_ORDER_CALL };
    let info = raw.frame_info().clone();
    let m: RawMetadata = raw.metadata().clone();
    let (Some(rw), Some(rh)) = (u16_dim(info.width), u16_dim(info.height)) else {
        st.set_error(&format!("{}x{} exceeds the 65535-pixel limit of this API", info.width, info.height));
        return err::LIBRAW_TOO_BIG;
    };
    st.format = raw.format().name();
    st.cpp = info.cpp.max(1);
    st.info = Some(info.clone());
    let full = Rect::full(info.width, info.height);
    let active = info.active_area.intersect(&full).filter(|r| !r.is_empty()).unwrap_or(full);

    // ---- sizes ----
    let s = &mut d.sizes;
    zero(s);
    s.raw_width = rw;
    s.raw_height = rh;
    s.left_margin = active.x as u16;
    s.top_margin = active.y as u16;
    s.width = active.width as u16;
    s.height = active.height as u16;
    s.raw_pitch = rw as u32
        * match (st.cpp, info.layout) {
            (1, _) => 2,
            (3, flashraw::SensorLayout::Foveon) => 6,
            _ => 8,
        };
    s.pixel_aspect =
        if info.pixel_aspect.is_finite() && info.pixel_aspect > 0.0 { info.pixel_aspect as f64 } else { 1.0 };
    s.flip = color::flip_from_orientation(m.orientation);
    for (i, a) in info.masked_areas.iter().take(8).enumerate() {
        s.mask[i] = [a.y as c_int, a.x as c_int, a.bottom() as c_int, a.right() as c_int];
    }
    let crop = info.crop.intersect(&full).unwrap_or(active);
    // Unknown inset crops are marked with 0xffff origins and zero sizes.
    let unknown = libraw_raw_inset_crop_t { cleft: 0xffff, ctop: 0xffff, cwidth: 0, cheight: 0 };
    s.raw_inset_crops = [unknown; 2];
    if crop != active {
        s.raw_inset_crops[0] = libraw_raw_inset_crop_t {
            cleft: crop.x as u16,
            ctop: crop.y as u16,
            cwidth: crop.width as u16,
            cheight: crop.height as u16,
        };
    }
    if let Some(uc) = m.dng.as_ref().and_then(|x| x.default_user_crop) {
        // DefaultUserCrop: top, left, bottom, right as fractions of the default crop.
        let (t, l, b, r) = (uc[0], uc[1], uc[2], uc[3]);
        if (0.0..1.0).contains(&t)
            && (0.0..1.0).contains(&l)
            && b > t
            && r > l
            && b <= 1.0
            && r <= 1.0
            && (t, l, b, r) != (0.0, 0.0, 1.0, 1.0)
        {
            let (cw, chh) = (crop.width as f64, crop.height as f64);
            s.raw_inset_crops[1] = libraw_raw_inset_crop_t {
                cleft: (crop.x as f64 + l * cw).round() as u16,
                ctop: (crop.y as f64 + t * chh).round() as u16,
                cwidth: ((r - l) * cw).round() as u16,
                cheight: ((b - t) * chh).round() as u16,
            };
        }
    }
    let cfa = st.cfa(&d.sizes);

    // ---- idata ----
    let id = &mut d.idata;
    zero(id);
    let make = if m.canonical_make.is_empty() { &m.make } else { &m.canonical_make };
    let model = if m.canonical_model.is_empty() { &m.model } else { &m.canonical_model };
    set_cstr(&mut id.make, make);
    set_cstr(&mut id.model, model);
    set_cstr(&mut id.normalized_make, make);
    set_cstr(&mut id.normalized_model, model);
    set_cstr(&mut id.software, m.software.as_deref().unwrap_or(""));
    id.raw_count = 1;
    id.dng_version = m.dng_version.map_or(0, u32::from_be_bytes);
    id.is_foveon = (info.layout == flashraw::SensorLayout::Foveon) as u32;
    id.colors = cfa.colors;
    id.filters = cfa.filters;
    let (xt, xa) = cfa.xtrans(d.sizes.left_margin as u32, d.sizes.top_margin as u32);
    id.xtrans = xt.map(|r| r.map(|v| v as std::ffi::c_char));
    id.xtrans_abs = xa.map(|r| r.map(|v| v as std::ffi::c_char));
    set_cstr(&mut id.cdesc, &cfa.cdesc);
    set_output_size(d, cfa.filters != 0);

    // ---- color ----
    let c = &mut d.color;
    zero(c);
    for (i, v) in c.curve.iter_mut().enumerate() {
        *v = i as u16;
    }
    let (left, top) = (d.sizes.left_margin as u32, d.sizes.top_margin as u32);
    let (black, cblack) = color::black_to_libraw(&info.black_level, &cfa, st.cpp, left, top);
    c.black = black;
    c.cblack.copy_from_slice(&cblack);
    c.maximum = info.white_level.max();
    if info.white_level.values.len() > 1 {
        for (i, v) in info.white_level.values.iter().take(4).enumerate() {
            c.linear_max[i] = *v;
        }
    }
    if let Some(wb) = info.white_balance {
        c.cam_mul = wb.coeffs;
    }
    if info.white_balance_applied {
        // Sony lossless YCbCr (sony_ycbcr_load_raw) and Nikon sNEF (nikon_load_sraw).
        use crate::types::aswb::*;
        c.as_shot_wb_applied = LIBRAW_ASWB_APPLIED
            | match make.as_str() {
                "Sony" => LIBRAW_ASWB_SONY,
                "Nikon" => LIBRAW_ASWB_NIKON_SRAW,
                _ => 0,
            };
    }
    c.pre_mul = [1.0, 1.0, 1.0, 0.0];
    for i in 0..3 {
        c.rgb_cam[i][i] = 1.0;
        c.ccm[i][i] = 1.0;
    }
    if let Some(cx) = color::cam_xyz(&info.color_matrices) {
        for (i, row) in cx.iter().enumerate().take(4) {
            c.cam_xyz[i] = row.map(|v| v as f32);
        }
        let n = (cfa.colors.clamp(3, 4) as usize).min(cx.len());
        if let Some((rgb_cam, pre_mul)) = color::rgb_cam_and_pre_mul(&cx[..n]) {
            c.rgb_cam = rgb_cam;
            c.pre_mul = pre_mul;
        }
    }
    set_cstr(&mut c.model2, m.firmware.as_deref().unwrap_or(""));
    set_cstr(&mut c.UniqueCameraModel, m.unique_camera_model.as_deref().unwrap_or(""));
    c.raw_bps = info.bits_per_sample as u32;
    c.ExifColorSpace = 255;
    if let Some(x) = &m.dng {
        let l = &mut c.dng_levels;
        l.dng_black = black;
        l.dng_whitelevel[0] = info.white_level.max();
        for (i, v) in info.white_level.values.iter().take(4).enumerate() {
            l.dng_whitelevel[i] = *v;
        }
        l.default_crop = [crop.x as u16, crop.y as u16, crop.width as u16, crop.height as u16];
        if let Some(n) = &x.as_shot_neutral {
            for (i, v) in n.iter().take(4).enumerate() {
                l.asshotneutral[i] = *v as f32;
            }
        }
        for (i, v) in info.color_matrices.analog_balance.iter().take(4).enumerate() {
            l.analogbalance[i] = *v;
        }
        l.baseline_exposure = x.baseline_exposure.unwrap_or(0.0) as f32;
        l.LinearResponseLimit = x.linear_response_limit.unwrap_or(1.0) as f32;
        for (k, cm) in info.color_matrices.color.iter().take(2).enumerate() {
            let dc = &mut c.dng_color[k];
            dc.illuminant = cm.illuminant.0;
            for r in 0..(cm.rows as usize).min(4) {
                for col in 0..3 {
                    dc.colormatrix[r][col] = cm.values.get(r * 3 + col).copied().unwrap_or(0.0);
                }
            }
        }
    }

    // ---- other ----
    let o = &mut d.other;
    zero(o);
    let e = &m.exposure;
    o.iso_speed = e.iso.unwrap_or(0) as f32;
    o.shutter = e.exposure_time.unwrap_or(0.0) as f32;
    o.aperture = e.f_number.unwrap_or(0.0);
    o.focal_len = e.focal_length.unwrap_or(0.0);
    o.timestamp = m.capture_time.as_deref().and_then(timestamp).unwrap_or(0);
    set_cstr(&mut o.artist, m.artist.as_deref().unwrap_or(""));
    for (i, v) in info.color_matrices.analog_balance.iter().take(4).enumerate() {
        o.analogbalance[i] = *v;
    }
    if let Some(g) = &m.gps {
        let pg = &mut o.parsed_gps;
        if let Some(lat) = g.latitude {
            pg.latitude = dms(lat);
            pg.latref = if lat < 0.0 { b'S' } else { b'N' } as std::ffi::c_char;
        }
        if let Some(lon) = g.longitude {
            pg.longitude = dms(lon);
            pg.longref = if lon < 0.0 { b'W' } else { b'E' } as std::ffi::c_char;
        }
        if let Some(alt) = g.altitude {
            pg.altitude = alt.abs() as f32;
            pg.altref = (alt < 0.0) as std::ffi::c_char;
        }
        if let Some(ts) = g.timestamp.as_deref().filter(|t| t.len() >= 19) {
            let p = |a: usize| ts.get(a..a + 2).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            pg.gpstimestamp = [p(11), p(14), p(17)];
        }
        pg.gpsstatus = b'A' as std::ffi::c_char;
        pg.gpsparsed = 1;
    }

    // ---- lens / shooting info ----
    let l = &mut d.lens;
    zero(l);
    if let Some((a, b)) = m.lens.focal_range {
        (l.MinFocal, l.MaxFocal) = (a, b);
        (l.dng.MinFocal, l.dng.MaxFocal) = (a, b);
        (l.makernotes.MinFocal, l.makernotes.MaxFocal) = (a, b);
    }
    if let Some((a, b)) = m.lens.aperture_range {
        (l.MaxAp4MinFocal, l.MaxAp4MaxFocal) = (a, b);
        (l.dng.MaxAp4MinFocal, l.dng.MaxAp4MaxFocal) = (a, b);
        (l.makernotes.MaxAp4MinFocal, l.makernotes.MaxAp4MaxFocal) = (a, b);
    }
    set_cstr(&mut l.LensMake, m.lens.make.as_deref().unwrap_or(""));
    set_cstr(&mut l.Lens, m.lens.model.as_deref().unwrap_or(""));
    set_cstr(&mut l.makernotes.Lens, m.lens.model.as_deref().unwrap_or(""));
    set_cstr(&mut l.LensSerial, m.lens.serial.as_deref().unwrap_or(""));
    l.FocalLengthIn35mmFormat = e.focal_length_35mm.unwrap_or(0).min(u16::MAX as u32) as u16;
    l.makernotes.FocalLengthIn35mmFormat = l.FocalLengthIn35mmFormat as f32;
    l.makernotes.CurFocal = e.focal_length.unwrap_or(0.0);
    l.makernotes.CurAp = e.f_number.unwrap_or(0.0);
    let si = &mut d.shootinginfo;
    zero(si);
    set_cstr(&mut si.BodySerial, m.serial.as_deref().unwrap_or(""));
    si.MeteringMode = e.metering_mode.map_or(-1, |v| v as i16);
    si.ExposureProgram = e.exposure_program.map_or(-1, |v| v as i16);

    // ---- thumbnails ----
    fill_thumb_info(d, raw.previews(), raw.preview());
    d.progress_flags = progress::OPEN | progress::IDENTIFY | progress::SIZE_ADJUST;
    // Backups restored by the processing calls.
    d.rawdata.iparams = d.idata;
    d.rawdata.sizes = d.sizes;
    d.rawdata.color = d.color;
    err::LIBRAW_SUCCESS
}

fn fill_thumb_info(d: &mut libraw_data_t, previews: &[PreviewInfo], default: Option<&PreviewInfo>) {
    zero(&mut d.thumbnail);
    zero(&mut d.thumbs_list);
    if let Some(p) = default {
        // The format is reported once the thumbnail is unpacked.
        let t = &mut d.thumbnail;
        t.twidth = p.width.min(65535) as u16;
        t.theight = p.height.min(65535) as u16;
        t.tlength = p.length.min(u32::MAX as u64) as u32;
        t.tcolors = 3;
    }
    let l = &mut d.thumbs_list;
    l.thumbcount = previews.len().min(LIBRAW_THUMBNAIL_MAXCOUNT) as c_int;
    for (i, p) in previews.iter().take(LIBRAW_THUMBNAIL_MAXCOUNT).enumerate() {
        l.thumblist[i] = libraw_thumbnail_item_t {
            tformat: internal_thumb_format(p.encoding),
            twidth: p.width.min(65535) as u16,
            theight: p.height.min(65535) as u16,
            tflip: color::flip_from_orientation(p.orientation) as u16,
            tlength: p.length.min(u32::MAX as u64) as u32,
            tmisc: 0,
            toffset: p.offset as i64,
        };
    }
}

/// `iwidth` / `iheight` for the current `half_size` setting.
pub fn set_output_size(d: &mut libraw_data_t, has_cfa: bool) {
    let shrink = (d.params.half_size != 0 && has_cfa && d.idata.filters != LIBRAW_XTRANS) as u32;
    let s = &mut d.sizes;
    s.iheight = ((s.height as u32 + shrink) >> shrink) as u16;
    s.iwidth = ((s.width as u32 + shrink) >> shrink) as u16;
}

/// Decode the sensor data into `rawdata`.
pub fn unpack(d: &mut libraw_data_t, st: &mut State) -> c_int {
    let Some(raw) = st.raw.as_ref() else {
        return if st.info.is_some() { err::LIBRAW_INPUT_CLOSED } else { err::LIBRAW_OUT_OF_ORDER_CALL };
    };
    let opts =
        DecodeOptions { apply_opcodes: false, corruption_policy: CorruptionPolicy::Recover, ..Default::default() };
    let frame = match raw.decode(opts) {
        Ok(f) => f,
        Err(e) => {
            let code = decode_error(&e);
            st.set_error(&e);
            return code;
        }
    };
    let (w, h, cpp) = (frame.width as usize, frame.height as usize, frame.cpp as usize);
    if w != d.sizes.raw_width as usize || h != d.sizes.raw_height as usize {
        st.set_error(&format!("decoded {w}x{h}, expected {}x{}", d.sizes.raw_width, d.sizes.raw_height));
        return err::LIBRAW_DATA_ERROR;
    }
    if frame.is_incomplete() {
        st.set_error(&format!("{} recovered data error(s): {}", frame.warnings.len(), frame.warnings[0]));
    }
    // Measured black (masked areas) and the white level of the 16-bit data.
    let cfa = st.cfa(&d.sizes);
    let (black, cblack) =
        color::black_to_libraw(&frame.black_level, &cfa, st.cpp, d.sizes.left_margin as u32, d.sizes.top_margin as u32);
    d.color.black = black;
    d.color.cblack.copy_from_slice(&cblack);
    d.color.maximum = frame.white_level.max();
    if st.info.as_ref().is_some_and(|i| i.sample_format == flashraw::SampleFormat::F32) {
        d.color.fmaximum = d.color.maximum as f32;
        d.color.fnorm = 1.0;
    }
    let samples = match frame.data {
        flashraw::RawPixelData::U16(v) => v,
        _ => {
            st.set_error(&"decoder returned no 16-bit samples");
            return err::LIBRAW_UNSPECIFIED_ERROR;
        }
    };
    st.image_buf = Vec::new();
    d.image = std::ptr::null_mut();
    if cpp == 1 {
        st.raw_buf = samples;
        st.raw_buf_cpp = 1;
    } else if cpp == 3 && d.idata.is_foveon != 0 {
        // Layered sensors use the 3-sample layout.
        st.raw_buf = samples;
        st.raw_buf_cpp = 3;
    } else {
        let mut v = vec![0u16; w * h * 4];
        for (dst, src) in v.chunks_exact_mut(4).zip(samples.chunks_exact(cpp)) {
            dst[..cpp.min(4)].copy_from_slice(&src[..cpp.min(4)]);
        }
        st.raw_buf = v;
        st.raw_buf_cpp = 4;
    }
    point_rawdata(d, st);
    d.rawdata.iparams = d.idata;
    d.rawdata.sizes = d.sizes;
    d.rawdata.color = d.color;
    d.progress_flags |= progress::LOAD_RAW;
    err::LIBRAW_SUCCESS
}

/// Point `rawdata` at `st.raw_buf`.
pub fn point_rawdata(d: &mut libraw_data_t, st: &mut State) {
    let r = &mut d.rawdata;
    r.raw_image = std::ptr::null_mut();
    r.color4_image = std::ptr::null_mut();
    r.color3_image = std::ptr::null_mut();
    r.float_image = std::ptr::null_mut();
    if st.raw_buf.is_empty() {
        r.raw_alloc = std::ptr::null_mut();
        return;
    }
    let p = st.raw_buf.as_mut_ptr();
    r.raw_alloc = p.cast();
    match st.raw_buf_cpp {
        1 => r.raw_image = p,
        3 => r.color3_image = p.cast(),
        _ => r.color4_image = p.cast(),
    }
}

/// Restore the per-file values the processing calls may have changed (as LibRaw does at
/// the start of each processing pass), then apply the user overrides from `params`.
pub fn restore_and_apply_params(d: &mut libraw_data_t) {
    d.idata = d.rawdata.iparams;
    d.sizes = d.rawdata.sizes;
    d.color = d.rawdata.color;
    let p = d.params;
    if p.user_black >= 0 {
        d.color.black = p.user_black as u32;
    }
    for i in 0..4 {
        if p.user_cblack[i] > -1_000_000 {
            d.color.cblack[i] = p.user_cblack[i].max(0) as u32;
        }
    }
    if p.user_sat > 0 {
        d.color.maximum = p.user_sat as u32;
    }
}

/// Maximum of `value - black - cblack[c]` over the visible area (LibRaw's `data_maximum`).
pub fn data_maximum(d: &libraw_data_t, st: &State) -> u32 {
    let s = &d.sizes;
    let cfa = st.cfa(s);
    let (rw, cpp) = (s.raw_width as usize, st.raw_buf_cpp as usize);
    let mut mx = 0u32;
    for row in 0..s.height as usize {
        let y = row + s.top_margin as usize;
        for col in 0..s.width as usize {
            let x = col + s.left_margin as usize;
            let base = (y * rw + x) * cpp;
            for k in 0..cpp.min(st.cpp as usize) {
                let Some(&v) = st.raw_buf.get(base + k) else { continue };
                let ci = if cpp == 1 {
                    cfa.color(row as i64, col as i64, s.left_margin as u32, s.top_margin as u32)
                } else {
                    k as i32
                };
                let cb = d.color.black + if (0..4).contains(&ci) { d.color.cblack[ci as usize] } else { 0 };
                mx = mx.max((v as u32).saturating_sub(cb));
            }
        }
    }
    mx
}

/// `libraw_raw2image`: fill `imgdata.image` (4 samples per pixel, colour index order) from
/// the raw data, at half size when `params.half_size` is set.
pub fn raw2image(d: &mut libraw_data_t, st: &mut State) -> c_int {
    if st.raw_buf.is_empty() {
        return err::LIBRAW_OUT_OF_ORDER_CALL;
    }
    restore_and_apply_params(d);
    let cfa = st.cfa(&d.sizes);
    set_output_size(d, cfa.filters != 0);
    let s = d.sizes;
    let shrink = (s.iwidth < s.width) as usize;
    let (iw, ih) = (s.iwidth as usize, s.iheight as usize);
    let mut img = vec![[0u16; 4]; iw * ih];
    let (rw, cpp) = (s.raw_width as usize, st.raw_buf_cpp as usize);
    for row in 0..s.height as usize {
        let y = row + s.top_margin as usize;
        for col in 0..s.width as usize {
            let x = col + s.left_margin as usize;
            let o = (row >> shrink) * iw + (col >> shrink);
            let Some(px) = img.get_mut(o) else { continue };
            if cpp == 1 {
                let v = st.raw_buf[y * rw + x];
                let c = match cfa.color(row as i64, col as i64, s.left_margin as u32, s.top_margin as u32) {
                    6 => 0,
                    c => c.clamp(0, 3) as usize,
                };
                px[c] = v;
            } else {
                let base = (y * rw + x) * cpp;
                let n = cpp.min(4);
                px[..n].copy_from_slice(&st.raw_buf[base..base + n]);
            }
        }
    }
    st.image_buf = img;
    d.image = st.image_buf.as_mut_ptr();
    d.color.data_maximum = data_maximum(d, st);
    d.progress_flags |= progress::RAW2_IMAGE;
    err::LIBRAW_SUCCESS
}

/// `libraw_subtract_black`: subtract the black level from `imgdata.image`.
pub fn subtract_black(d: &mut libraw_data_t, st: &mut State) {
    if st.image_buf.is_empty() {
        return;
    }
    let (black, cb) = (d.color.black, d.color.cblack);
    let (pr, pc) = (cb[4] as usize, cb[5] as usize);
    let iw = d.sizes.iwidth.max(1) as usize;
    if black == 0 && cb[..4].iter().all(|&v| v == 0) && (pr == 0 || pc == 0) {
        return;
    }
    let pattern = pr > 0 && pc > 0 && pr * pc + 6 <= cb.len();
    for (i, px) in st.image_buf.iter_mut().enumerate() {
        let (r, c) = (i / iw, i % iw);
        let extra = if pattern { cb[6 + (r % pr) * pc + (c % pc)] } else { 0 };
        for (k, v) in px.iter_mut().enumerate() {
            if *v != 0 {
                *v = (*v as u32).saturating_sub(black + cb[k] + extra) as u16;
            }
        }
    }
    let sub = black + cb[..4].iter().copied().min().unwrap_or(0);
    d.color.maximum = d.color.maximum.saturating_sub(sub);
    d.color.data_maximum = d.color.data_maximum.saturating_sub(sub);
    d.color.black = 0;
    d.color.cblack = [0; LIBRAW_CBLACK_SIZE];
}
