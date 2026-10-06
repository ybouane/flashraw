//! `libraw_dcraw_process` and the output stage (`libraw_dcraw_make_mem_image`, writers).
//!
//! Processing is FlashRaw's fused develop kernel (black subtraction, white-level scaling,
//! bilinear or half-size demosaic, white balance, colour matrix), fed from the unpacked
//! raw data so caller edits to `rawdata.raw_image` are honoured. The output stage follows
//! the documented conventions of the API: optional automatic brightness from the histogram (1% of pixels
//! clipped by default), a power-law curve with a linear toe (`params.gamm`), 8 or 16 bits,
//! and the `flip` orientation.

use crate::color::{self, CfaModel};
use crate::processor::{State, data_maximum, restore_and_apply_params};
use crate::types::*;
use flashraw::low_level::core::{PixelData, RawImage};
use flashraw::process::{DevelopParams, develop};
use flashraw::{Demosaic, Orientation, Rect, WhiteBalance, WhiteLevel};
use std::ffi::c_int;

/// Grey-world white balance: per colour, the inverse mean of the black-subtracted samples
/// of the visible area, skipping 8x8 blocks that contain a clipped sample.
fn grey_world(d: &libraw_data_t, st: &State, cfa: &CfaModel) -> Option<[f32; 4]> {
    let s = &d.sizes;
    let (rw, k, cpp) = (s.raw_width as usize, st.raw_buf_cpp as usize, (st.cpp as usize).min(4));
    let (left, top) = (s.left_margin as usize, s.top_margin as usize);
    let (w, h) = (s.width as usize, s.height as usize);
    let clip = d.color.maximum.saturating_sub(25).min(u16::MAX as u32) as u16;
    let black = |c: usize| d.color.black + d.color.cblack[c];
    let (mut sum, mut cnt) = ([0f64; 4], [0f64; 4]);
    for by in (0..h).step_by(8) {
        for bx in (0..w).step_by(8) {
            let (mut bs, mut bc) = ([0f64; 4], [0f64; 4]);
            let mut clipped = false;
            'block: for y in by..(by + 8).min(h) {
                for x in bx..(bx + 8).min(w) {
                    let base = ((y + top) * rw + x + left) * k;
                    let n = if k == 1 { 1 } else { cpp };
                    for i in 0..n {
                        let v = st.raw_buf.get(base + i).copied().unwrap_or(0);
                        if v > clip {
                            clipped = true;
                            break 'block;
                        }
                        let c = if k == 1 {
                            match cfa.color(y as i64, x as i64, left as u32, top as u32) {
                                6 => 0,
                                c => c.clamp(0, 3) as usize,
                            }
                        } else {
                            i
                        };
                        bs[c] += (v as u32).saturating_sub(black(c)) as f64;
                        bc[c] += 1.0;
                    }
                }
            }
            if !clipped {
                for c in 0..4 {
                    sum[c] += bs[c];
                    cnt[c] += bc[c];
                }
            }
        }
    }
    let mut m = [0f32; 4];
    for c in 0..4 {
        if sum[c] > 0.0 {
            m[c] = (cnt[c] / sum[c]) as f32;
        }
    }
    (m[0] > 0.0 && m[1] > 0.0 && m[2] > 0.0).then_some(m)
}

/// White-balance multipliers per LibRaw colour index for the current parameters.
fn multipliers(d: &libraw_data_t, auto: Option<[f32; 4]>) -> [f32; 4] {
    let p = &d.params;
    let c = &d.color;
    let valid = |m: &[f32; 4]| m[0] > 0.0 && m[1] > 0.0 && m[2] > 0.0 && m.iter().all(|v| v.is_finite());
    let mut m = if p.user_mul[0] > 0.0 && valid(&p.user_mul) {
        p.user_mul
    } else if let Some(a) = auto {
        a
    } else if p.use_camera_wb != 0 && valid(&c.cam_mul) {
        c.cam_mul
    } else if valid(&c.pre_mul) {
        c.pre_mul
    } else if valid(&c.cam_mul) {
        c.cam_mul
    } else {
        [1.0; 4]
    };
    if m[3] <= 0.0 {
        m[3] = if d.idata.colors < 4 { m[1] } else { 1.0 };
    }
    let n = (d.idata.colors.clamp(1, 4)) as usize;
    let min = m[..n.max(3)].iter().copied().fold(f32::INFINITY, f32::min);
    if min > 0.0 && min.is_finite() {
        m.iter_mut().for_each(|v| *v /= min);
    }
    m
}

/// `libraw_dcraw_process`.
pub fn dcraw_process(d: &mut libraw_data_t, st: &mut State) -> c_int {
    if st.raw_buf.is_empty() {
        return err::LIBRAW_OUT_OF_ORDER_CALL;
    }
    let Some(mut info) = st.info.clone() else { return err::LIBRAW_OUT_OF_ORDER_CALL };
    let Some(out) = color::output_matrix(d.params.output_color) else {
        st.set_error(&format!("output_color {} is not supported (0, 1, 2, 5 and 8 are)", d.params.output_color));
        return err::LIBRAW_NOT_IMPLEMENTED;
    };
    restore_and_apply_params(d);
    let cfa = st.cfa(&d.sizes);
    // Automatic maximum adjustment (params.adjust_maximum_thr).
    d.color.data_maximum = data_maximum(d, st);
    let thr = d.params.adjust_maximum_thr;
    let real = d.color.data_maximum;
    if thr > 0.000_01 && thr < 2.0 && real > 0 && real < d.color.maximum && real as f32 > d.color.maximum as f32 * thr {
        d.color.maximum = real;
    }
    let s = d.sizes;
    let (left, top) = (s.left_margin as u32, s.top_margin as u32);
    info.black_level = color::black_from_libraw(d.color.black, &d.color.cblack, &cfa, st.cpp, left, top);
    info.white_level = WhiteLevel::uniform(d.color.maximum.max(1));
    let auto = if d.params.use_auto_wb != 0 && d.params.user_mul[0] <= 0.0 { grey_world(d, st, &cfa) } else { None };
    let mul = multipliers(d, auto);
    // FlashRaw indexes multipliers by CFA colour (R, G, B, other); for RGB sensors the
    // LibRaw indices coincide except the second green, which shares green's multiplier.
    info.white_balance = Some(WhiteBalance { coeffs: [mul[0], mul[1], mul[2], mul[3]] });
    info.orientation = Orientation::Normal;

    let cpp = st.cpp as usize;
    let stored = st.raw_buf_cpp as usize;
    let data = if stored == cpp {
        st.raw_buf.clone()
    } else {
        st.raw_buf.chunks_exact(stored).flat_map(|px| px[..cpp.min(stored)].iter().copied()).collect()
    };
    let img = RawImage {
        width: s.raw_width as u32,
        height: s.raw_height as u32,
        cpp: st.cpp,
        data: PixelData::U16(data),
        float_max: None,
    };
    let demosaic = if d.params.half_size != 0 && cfa.filters != 0 { Demosaic::HalfSize } else { Demosaic::Bilinear };
    let crop = Rect::new(left, top, s.width as u32, s.height as u32);
    let mut params = DevelopParams::new(&info, demosaic, crop);
    params.orientation = Orientation::Normal;
    params.wb = info.white_balance.map_or([1.0; 4], |w| w.coeffs);
    let rgb_cam = if d.params.output_color == 0 || d.idata.colors > 3 {
        if d.params.output_color == 0 { [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] } else { params.cam_to_rgb }
    } else {
        let r = d.color.rgb_cam;
        [[r[0][0], r[0][1], r[0][2]], [r[1][0], r[1][1], r[1][2]], [r[2][0], r[2][1], r[2][2]]]
    };
    let mut m = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = (0..3).map(|k| out[i][k] * rgb_cam[k][j]).sum();
        }
    }
    params.cam_to_rgb = m;
    // Four-colour sensors: the same output conversion after the 3x4 camera matrix.
    params.cam4_to_rgb = params
        .cam4_to_rgb
        .filter(|_| d.params.output_color != 0)
        .map(|m4| std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| out[i][k] * m4[k][j]).sum())));
    let dev = match develop(&img, &info, (0, 0), &params, true) {
        Ok(v) => v,
        Err(e) => {
            st.set_error(&e);
            return err::LIBRAW_UNSPECIFIED_ERROR;
        }
    };
    let (ow, oh) = (dev.width as usize, dev.height as usize);
    let (Ok(ow16), Ok(oh16)) = (u16::try_from(ow), u16::try_from(oh)) else { return err::LIBRAW_TOO_BIG };
    let mut image = vec![[0u16; 4]; ow * oh];
    for (px, rgb) in image.iter_mut().zip(dev.rgb.chunks_exact(3)) {
        for c in 0..3 {
            px[c] = (rgb[c].clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
        }
    }
    st.image_buf = image;
    d.image = st.image_buf.as_mut_ptr();
    d.sizes.iwidth = ow16;
    d.sizes.iheight = oh16;
    d.sizes.width = ow16;
    d.sizes.height = oh16;
    d.progress_flags |= progress::RAW2_IMAGE
        | progress::SCALE_COLORS
        | progress::PRE_INTERPOLATE
        | progress::INTERPOLATE
        | progress::CONVERT_RGB;
    err::LIBRAW_SUCCESS
}

/// Geometry of the bitmap `make_mem_image` produces: (width, height, colors, bits).
pub fn mem_image_format(d: &libraw_data_t) -> (usize, usize, usize, usize) {
    let s = &d.sizes;
    let flip = if d.params.user_flip >= 0 { d.params.user_flip } else { s.flip };
    let (w, h) = (s.iwidth as usize, s.iheight as usize);
    let (w, h) = if flip & 4 != 0 { (h, w) } else { (w, h) };
    let colors = if d.idata.colors == 1 { 1 } else { 3 };
    let bits = if d.params.output_bps == 16 { 16 } else { 8 };
    (w, h, colors, bits)
}

/// The processed image as interleaved 8- or 16-bit samples (native byte order), with
/// automatic brightness, the output curve and the orientation applied.
pub fn render(d: &libraw_data_t, st: &State) -> Result<(usize, usize, usize, usize, Vec<u8>), c_int> {
    if st.image_buf.is_empty() {
        return Err(err::LIBRAW_OUT_OF_ORDER_CALL);
    }
    let p = &d.params;
    let (iw, ih) = (d.sizes.iwidth as usize, d.sizes.iheight as usize);
    if iw * ih != st.image_buf.len() {
        return Err(err::LIBRAW_OUT_OF_ORDER_CALL);
    }
    let (ow, oh, colors, bits) = mem_image_format(d);
    let flip = if p.user_flip >= 0 { p.user_flip } else { d.sizes.flip };

    // Automatic brightness: the level at which `auto_bright_thr` of the pixels clip.
    let mut white = 0x2000usize;
    if (p.highlight & !2) == 0 && p.no_auto_bright == 0 {
        let perc = (iw * ih) as f64 * p.auto_bright_thr as f64;
        white = 0;
        for c in 0..colors {
            let mut hist = vec![0u32; 0x2000];
            for px in &st.image_buf {
                hist[(px[c] >> 3) as usize] += 1;
            }
            let (mut val, mut total) = (0x2000usize, 0f64);
            loop {
                val -= 1;
                if val <= 32 {
                    break;
                }
                total += hist[val] as f64;
                if total > perc {
                    break;
                }
            }
            white = white.max(val);
        }
    }
    let bright = if p.bright > 0.0 { p.bright as f64 } else { 1.0 };
    let curve = color::gamma_curve(p.gamm[0], p.gamm[1], (white << 3) as f64 / bright);

    let bytes = bits / 8;
    let mut out = vec![0u8; ow * oh * colors * bytes];
    let row_len = ow * colors * bytes;
    for (orow, line) in out.chunks_exact_mut(row_len.max(1)).enumerate() {
        for ocol in 0..ow {
            let (mut r, mut c) = (orow, ocol);
            if flip & 4 != 0 {
                std::mem::swap(&mut r, &mut c);
            }
            if flip & 2 != 0 {
                r = ih - 1 - r;
            }
            if flip & 1 != 0 {
                c = iw - 1 - c;
            }
            let px = &st.image_buf[r * iw + c];
            for k in 0..colors {
                let v = curve[px[k] as usize];
                let o = (ocol * colors + k) * bytes;
                if bytes == 1 {
                    line[o] = (v >> 8) as u8;
                } else {
                    line[o..o + 2].copy_from_slice(&v.to_ne_bytes());
                }
            }
        }
    }
    Ok((ow, oh, colors, bits, out))
}

/// PPM/PGM file contents (16-bit samples big-endian, as the format requires).
pub fn ppm(width: usize, height: usize, colors: usize, bits: usize, native: &[u8]) -> Vec<u8> {
    let magic = if colors == 1 { "P5" } else { "P6" };
    let mut v = format!("{magic}\n{width} {height}\n{}\n", (1u32 << bits) - 1).into_bytes();
    if bits == 16 {
        v.extend(native.chunks_exact(2).flat_map(|b| u16::from_ne_bytes([b[0], b[1]]).to_be_bytes()));
    } else {
        v.extend_from_slice(native);
    }
    v
}
