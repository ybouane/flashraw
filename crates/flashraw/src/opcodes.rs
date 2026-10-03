//! Application of DNG opcode lists (parsing and kernels in
//! [`flashraw_codecs::dng_opcodes`]).
//!
//! Each list is applied in the domain the DNG specification defines for it:
//!
//! | list | applied to | outputs affected |
//! |---|---|---|
//! | OpcodeList1 | stored sample values, whole-buffer coordinates | all |
//! | OpcodeList2 | linear reference values (black 0, white 1), active-area coordinates | normalized, RGB |
//! | OpcodeList3 | demosaiced camera RGB, active-area coordinates | RGB |
//!
//! Geometric opcodes (warps, trims) and unknown ones are not applied;
//! they are listed in [`crate::DecodeStatistics::opcodes`] like everything that was.

use crate::process::{self, DevelopParams, Developed};
use crate::{Demosaic, OutputFormat, RawPixelData};
use flashraw_codecs::dng_opcodes::{self, Opcode, PlaneImage};
use flashraw_core::{BlackLevel, FrameInfo, PixelData, RawImage, Rect, Result, WhiteLevel, par};

/// Parsed opcode lists of a frame plus notes for the statistics.
pub(crate) struct Lists {
    ops: [Vec<Opcode>; 3],
    pub notes: Vec<String>,
}

impl Lists {
    /// Parse the lists of `info`; `None` when the frame carries none.
    pub fn of(info: &FrameInfo) -> Option<Lists> {
        let raw = info.opcodes.as_ref()?;
        let mut notes = Vec::new();
        let ops = std::array::from_fn(|i| {
            let b = &raw.lists[i];
            if b.len() <= 4 {
                return Vec::new();
            }
            dng_opcodes::parse(b).unwrap_or_else(|e| {
                notes.push(format!("OpcodeList{}: not applied ({e})", i + 1));
                Vec::new()
            })
        });
        Some(Lists { ops, notes })
    }

    /// Whether list `i` (0-based) holds an opcode that would change pixels.
    pub fn has_pixel_ops(&self, i: usize) -> bool {
        self.ops[i].iter().any(|o| o.is_pixel_opcode())
    }

    /// Whether any list changes the given output.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub fn affects(&self, output: OutputFormat) -> bool {
        match output.host_equivalent() {
            OutputFormat::RawU16 | OutputFormat::RawF32 => self.has_pixel_ops(0),
            OutputFormat::NormalizedF32 => self.has_pixel_ops(0) || self.has_pixel_ops(1),
            _ => (0..3).any(|i| self.has_pixel_ops(i)),
        }
    }

    fn record(&mut self, i: usize, skipped: &[&str]) {
        let mut applied: Vec<&str> = Vec::new();
        for op in &self.ops[i] {
            if op.is_pixel_opcode() {
                applied.push(op.name());
            }
        }
        if !applied.is_empty() {
            self.notes.push(format!("OpcodeList{}: applied {}", i + 1, summarize(&applied)));
        }
        if !skipped.is_empty() {
            self.notes.push(format!("OpcodeList{}: not applied {}", i + 1, summarize(skipped)));
        }
    }

    /// Note list `i` as not applied to this output.
    pub(crate) fn record_unused(&mut self, i: usize, why: &str) {
        let names: Vec<&str> = self.ops[i].iter().map(|o| o.name()).collect();
        if !names.is_empty() {
            self.notes.push(format!("OpcodeList{}: not applied {} ({why})", i + 1, summarize(&names)));
        }
    }

    /// OpcodeList1 on the decoded sensor buffer (stored values). `origin` is the
    /// buffer's position in the full sensor buffer (region decodes).
    pub fn apply_list1(&mut self, img: &mut RawImage, info: &FrameInfo, origin: (u32, u32), parallel: bool) {
        if self.ops[0].is_empty() {
            return;
        }
        let (w, h, cpp) = (img.width as usize, img.height as usize, img.cpp as usize);
        let period = cfa_period(info);
        let ops = &self.ops[0];
        let window = |data: &mut [f32], scale: f32| {
            let mut pi = PlaneImage {
                data,
                width: w,
                height: h,
                planes: cpp,
                stride: w,
                x0: origin.0 as usize,
                y0: origin.1 as usize,
                full_width: info.width as usize,
                full_height: info.height as usize,
            };
            dng_opcodes::apply(ops, &mut pi, period, scale, parallel)
        };
        if !self.has_pixel_ops(0) {
            self.record_unused(0, "no pixel opcodes");
            return;
        }
        let skipped = match &mut img.data {
            PixelData::U16(v) => {
                let mut f = vec![0f32; v.len()];
                par::for_each_row_chunk(&mut f, w * cpp, 64, parallel, |y0, rows| {
                    let src = &v[y0 * w * cpp..y0 * w * cpp + rows.len()];
                    for (d, &s) in rows.iter_mut().zip(src) {
                        *d = s as f32;
                    }
                });
                let skipped = window(&mut f, 1.0);
                par::for_each_row_chunk(v, w * cpp, 64, parallel, |y0, rows| {
                    let src = &f[y0 * w * cpp..y0 * w * cpp + rows.len()];
                    for (d, &s) in rows.iter_mut().zip(src) {
                        *d = s.round().clamp(0.0, 65535.0) as u16;
                    }
                });
                skipped
            }
            // Float samples: tables and constants address the 16-bit range of [0, white].
            PixelData::F32(v) => window(v, 65535.0 / info.white_level.max().max(1) as f32),
        };
        // The samples changed: a maximum measured during decoding no longer holds.
        img.float_max = None;
        self.record(0, &skipped);
    }

    /// OpcodeList2 on normalised samples (`data`: the frame buffer, `width` pixels of
    /// `cpp` samples per row, at `origin` in the sensor buffer).
    pub fn apply_list2(
        &mut self,
        data: &mut [f32],
        width: u32,
        cpp: u32,
        info: &FrameInfo,
        origin: (u32, u32),
        parallel: bool,
    ) {
        if self.ops[1].is_empty() {
            return;
        }
        let height = (data.len() / (width as usize * cpp as usize).max(1)) as u32;
        let skipped = match active_window(info, origin, width, height) {
            Some((local, x0, y0)) => {
                let start = (local.y as usize * width as usize + local.x as usize) * cpp as usize;
                let mut pi = PlaneImage {
                    data: &mut data[start..],
                    width: local.width as usize,
                    height: local.height as usize,
                    planes: cpp as usize,
                    stride: width as usize,
                    x0,
                    y0,
                    full_width: info.active_area.width as usize,
                    full_height: info.active_area.height as usize,
                };
                dng_opcodes::apply(&self.ops[1], &mut pi, cfa_period(info), 65535.0, parallel)
            }
            None => Vec::new(),
        };
        self.record(1, &skipped);
    }

    /// OpcodeList3 on demosaiced camera RGB covering the active-area window returned by
    /// [`active_window`] (`rgb`: `width x height` pixels of 3 samples).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_list3(
        &mut self,
        rgb: &mut [f32],
        width: u32,
        height: u32,
        info: &FrameInfo,
        x0: usize,
        y0: usize,
        parallel: bool,
    ) {
        let mut pi = PlaneImage {
            data: rgb,
            width: width as usize,
            height: height as usize,
            planes: 3,
            stride: width as usize,
            x0,
            y0,
            full_width: info.active_area.width as usize,
            full_height: info.active_area.height as usize,
        };
        let skipped = dng_opcodes::apply(&self.ops[2], &mut pi, 1, 65535.0, parallel);
        self.record(2, &skipped);
    }
}

impl Lists {
    /// RGB development with lists 2 and 3 applied in their stages (instead of the fused
    /// single pass): normalise, list 2, demosaic to camera RGB over the active area,
    /// list 3, then white balance, colour conversion, crop and (RGBA8) orientation.
    /// Returns `(width, height, channels, pixels)`.
    #[allow(clippy::too_many_arguments)]
    pub fn develop(
        &mut self,
        img: &RawImage,
        info: &FrameInfo,
        origin: (u32, u32),
        demosaic: Demosaic,
        crop: Rect,
        rgba8: bool,
        parallel: bool,
    ) -> Result<(u32, u32, u32, RawPixelData)> {
        let mut norm = process::normalize(img, info, origin, parallel);
        self.apply_list2(&mut norm, img.width, img.cpp, info, origin, parallel);
        par::for_each_row_chunk(&mut norm, img.width as usize * img.cpp as usize, 64, parallel, |_, rows| {
            for v in rows {
                *v *= 65535.0;
            }
        });
        let lin = RawImage {
            width: img.width,
            height: img.height,
            cpp: img.cpp,
            data: PixelData::F32(norm),
            float_max: None,
        };
        let mut linfo = info.clone();
        linfo.black_level = BlackLevel::uniform(0.0);
        linfo.black_from_masked = false;
        linfo.white_level = WhiteLevel::uniform(65535);
        let params = DevelopParams::new(&linfo, demosaic, crop);
        let window = active_window(info, origin, img.width, img.height);
        let staged = self.has_pixel_ops(2) && demosaic != Demosaic::HalfSize && window.is_some();
        if !staged {
            if self.has_pixel_ops(2) {
                self.record_unused(2, "half-size output");
            }
            return Ok(if rgba8 {
                let (w, h, px) = process::develop_rgba8(&lin, &linfo, origin, &params, parallel)?;
                (w, h, 4, RawPixelData::U8(px))
            } else {
                let d = process::develop(&lin, &linfo, origin, &params, parallel)?;
                (d.width, d.height, 3, RawPixelData::F32(d.rgb))
            });
        }
        let (win, x0, y0) = window.unwrap_or_default();
        let mut cam = DevelopParams::new(&linfo, demosaic, win);
        cam.wb = [1.0; 4];
        cam.cam_to_rgb = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut dev = process::develop(&lin, &linfo, origin, &cam, parallel)?;
        self.apply_list3(&mut dev.rgb, dev.width, dev.height, info, x0, y0, parallel);
        // Crop to the requested rectangle (inside the active-area window).
        let c = crop.intersect(&win).unwrap_or(win);
        let (cx, cy) = ((c.x - win.x) as usize, (c.y - win.y) as usize);
        let (cw, ch) = (c.width as usize, c.height as usize);
        let mut rgb = vec![0f32; cw * ch * 3];
        let (wb, m) = (params.wb, params.cam_to_rgb);
        let dw = dev.width as usize;
        par::for_each_row_chunk(&mut rgb, cw * 3, 16, parallel, |first, rows| {
            for (i, row) in rows.chunks_exact_mut(cw * 3).enumerate() {
                let src = &dev.rgb[((cy + first + i) * dw + cx) * 3..][..cw * 3];
                for (o, p) in row.chunks_exact_mut(3).zip(src.chunks_exact(3)) {
                    let r = (p[0] * wb[0]).clamp(0.0, 1.0);
                    let g = (p[1] * wb[1]).clamp(0.0, 1.0);
                    let b = (p[2] * wb[2]).clamp(0.0, 1.0);
                    for k in 0..3 {
                        o[k] = m[k][0] * r + m[k][1] * g + m[k][2] * b;
                    }
                }
            }
        });
        let out = Developed { width: c.width, height: c.height, rgb };
        Ok(if rgba8 {
            let (w, h, px) = process::to_rgba8(&out, params.orientation, parallel);
            (w, h, 4, RawPixelData::U8(px))
        } else {
            (out.width, out.height, 3, RawPixelData::F32(out.rgb))
        })
    }

    pub fn into_notes(self) -> Vec<String> {
        self.notes
    }
}

/// The part of the active area inside a frame at `origin` of `width x height`: the
/// rectangle in frame coordinates and its offset inside the active area.
pub(crate) fn active_window(
    info: &FrameInfo,
    origin: (u32, u32),
    width: u32,
    height: u32,
) -> Option<(Rect, usize, usize)> {
    let frame = Rect::new(origin.0, origin.1, width, height);
    let a = info.active_area;
    let i = a.intersect(&frame)?;
    Some((Rect::new(i.x - origin.0, i.y - origin.1, i.width, i.height), (i.x - a.x) as usize, (i.y - a.y) as usize))
}

fn cfa_period(info: &FrameInfo) -> usize {
    info.cfa.map(|c| c.width().max(c.height())).unwrap_or(1)
}

/// `["GainMap", "GainMap", "MapTable"]` -> `"GainMap x2, MapTable"`.
fn summarize(names: &[&str]) -> String {
    let mut out: Vec<(String, usize)> = Vec::new();
    for n in names {
        match out.iter_mut().find(|(m, _)| m == n) {
            Some((_, c)) => *c += 1,
            None => out.push((n.to_string(), 1)),
        }
    }
    out.iter().map(|(n, c)| if *c > 1 { format!("{n} x{c}") } else { n.clone() }).collect::<Vec<_>>().join(", ")
}
