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

use crate::process::{self, DevelopParams, Developed, RgbEncoding};
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
            PixelData::U16(v) if dng_opcodes::u16_exact(ops) => {
                // Tables and bad-pixel repairs work on the stored integers directly.
                let mut pi = PlaneImage {
                    data: &mut v[..],
                    width: w,
                    height: h,
                    planes: cpp,
                    stride: w,
                    x0: origin.0 as usize,
                    y0: origin.1 as usize,
                    full_width: info.width as usize,
                    full_height: info.height as usize,
                };
                dng_opcodes::apply_u16(ops, &mut pi, period, parallel).unwrap_or_default()
            }
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
    /// list 3, then white balance, colour conversion, levels, crop and encoding.
    ///
    /// The stages stream: rows are normalised (and list 2 applied) as the demosaic reads
    /// them, and list 3 runs on each task's band of camera RGB, so no full-frame
    /// intermediate is needed. A list holding bad-pixel repairs (which read neighbours)
    /// runs on a whole-frame buffer instead. Both forms give identical results. Returns
    /// `(width, height, channels, pixels)`.
    pub fn develop(
        &mut self,
        img: &RawImage,
        info: &FrameInfo,
        origin: (u32, u32),
        params: &DevelopParams,
        enc: RgbEncoding,
        parallel: bool,
    ) -> Result<(u32, u32, u32, RawPixelData)> {
        let (w, cpp) = (img.width as usize, img.cpp as usize);
        // After list 2 the develop sees linear reference values scaled to 16 bits (black 0,
        // white 65535) and normalises them again: `(v - 0) * (1 / 65535)`.
        let mut linfo = info.clone();
        linfo.black_level = BlackLevel::uniform(0.0);
        linfo.black_from_masked = false;
        linfo.white_level = WhiteLevel::uniform(65535);
        let lin_scale = 1.0f32 / (65535.0f32 - 0.0);
        let window = active_window(info, origin, img.width, img.height);
        // ---- Stage 1: normalised samples with OpcodeList2 ----
        let streamed2 = self.ops[1].iter().all(|o| o.is_row_local());
        let full: Option<Vec<f32>> = if streamed2 {
            if !self.ops[1].is_empty() {
                let skipped: Vec<&str> = match window {
                    Some(_) => self.ops[1].iter().filter(|o| !o.is_pixel_opcode()).map(|o| o.name()).collect(),
                    None => Vec::new(),
                };
                self.record(1, &skipped);
            }
            None
        } else {
            let mut norm = process::normalize(img, info, origin, parallel);
            self.apply_list2(&mut norm, img.width, img.cpp, info, origin, parallel);
            par::for_each_row_chunk(&mut norm, w * cpp, 64, parallel, |_, rows| {
                for v in rows {
                    *v *= 65535.0;
                }
            });
            Some(norm)
        };
        let normalizer = process::normalizer(img, info, origin);
        let (full_w, full_h) = (info.active_area.width as usize, info.active_area.height as usize);
        // List 2, prepared once for the rows of the active-area window.
        let ops2 = self.ops[1].clone();
        let list2 = match (window, full.is_none()) {
            (Some((l, x0, y0)), true) => {
                dng_opcodes::RowOps::new(&ops2, l.width as usize, cpp, x0, full_w, full_h, 65535.0)
                    .filter(|r| !r.is_empty())
                    .map(|r| (l, y0, r))
            }
            _ => None,
        };
        let rows = |y: usize, out: &mut [f32]| {
            if let Some(n) = &full {
                for (o, &v) in out.iter_mut().zip(&n[y * w * cpp..(y + 1) * w * cpp]) {
                    *o = v * lin_scale;
                }
                return;
            }
            normalizer.normalize_row(y, out);
            for v in out.iter_mut() {
                *v = v.max(0.0);
            }
            if let Some((local, y0, ops)) =
                list2.as_ref().filter(|(l, _, _)| y >= l.y as usize && y < l.bottom() as usize)
            {
                let s = local.x as usize * cpp;
                ops.apply(y0 + (y - local.y as usize), &mut out[s..s + local.width as usize * cpp]);
            }
            for v in out.iter_mut() {
                *v = (*v * 65535.0) * lin_scale;
            }
        };
        let mut lparams = params.clone();
        let staged3 = self.has_pixel_ops(2) && params.demosaic != Demosaic::HalfSize && window.is_some();
        if !staged3 {
            if self.has_pixel_ops(2) {
                self.record_unused(2, "half-size output");
            }
            return process::develop_staged(img, &linfo, origin, &lparams, enc, parallel, Some(&rows), None);
        }
        // ---- Stage 2: OpcodeList3 on camera RGB of the active-area window ----
        let (win, x0, y0) = window.unwrap_or_default();
        // The output is the requested crop inside the window.
        let c = params.crop.intersect(&win).unwrap_or(win);
        lparams.crop = c;
        let ops3 = self.ops[2].clone();
        let skipped3: Vec<&'static str> = ops3.iter().filter(|o| !o.is_pixel_opcode()).map(|o| o.name()).collect();
        let (bx, by) = (x0 + (c.x - win.x) as usize, y0 + (c.y - win.y) as usize);
        let cw = c.width as usize;
        if let Some(ops) = dng_opcodes::RowOps::new(&ops3, cw, 3, bx, full_w, full_h, 65535.0) {
            let list3 = |first: usize, band: &mut [[f32; 3]]| {
                for (j, row) in band.chunks_exact_mut(cw).enumerate() {
                    ops.apply(by + first + j, row.as_flattened_mut());
                }
            };
            let r = process::develop_staged(img, &linfo, origin, &lparams, enc, parallel, Some(&rows), Some(&list3));
            self.record(2, &skipped3);
            return r;
        }
        // Bad-pixel repairs in list 3: camera RGB of the whole window, list 3, then the rest.
        let cam = DevelopParams { crop: win, ..lparams.camera_rgb() };
        let (dw, dh, _, px) =
            process::develop_staged(img, &linfo, origin, &cam, RgbEncoding::LinearF32, parallel, Some(&rows), None)?;
        let RawPixelData::F32(mut dev) = px else { unreachable!("f32 develop") };
        self.apply_list3(&mut dev, dw, dh, info, x0, y0, parallel);
        let (cx, cy) = ((c.x - win.x) as usize, (c.y - win.y) as usize);
        let (cw, ch) = (c.width as usize, c.height as usize);
        let mut rgb = vec![0f32; cw * ch * 3];
        par::for_each_row_chunk(&mut rgb, cw * 3, 16, parallel, |first, out| {
            for (i, row) in out.chunks_exact_mut(cw * 3).enumerate() {
                let src = &dev[((cy + first + i) * dw as usize + cx) * 3..][..cw * 3];
                for (o, p) in row.chunks_exact_mut(3).zip(src.chunks_exact(3)) {
                    let v = process::finish_rgb([p[0], p[1], p[2]], &lparams.wb, &lparams.cam_to_rgb, lparams.levels);
                    o.copy_from_slice(&v);
                }
            }
        });
        drop(dev);
        let out = Developed { width: c.width, height: c.height, rgb };
        Ok(process::encode_image(out, enc, &params.curve, params.orientation, parallel))
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

#[cfg(test)]
mod tests {
    use super::*;
    use flashraw_codecs::dng_opcodes::Region;
    use flashraw_core::{CfaPattern, ColorMatrices, ColorMatrix, Illuminant, Limits, Orientation, WhiteBalance};

    fn region(top: u32, left: u32, bottom: u32, right: u32, planes: u32) -> Region {
        Region { top, left, bottom, right, plane: 0, planes, row_pitch: 1, col_pitch: 1 }
    }

    fn gain_map(r: Region, map_planes: u32) -> Opcode {
        let (pv, ph) = (3u32, 4u32);
        let gains = (0..pv * ph * map_planes).map(|i| 0.8 + (i * 37 % 11) as f32 * 0.05).collect();
        Opcode::GainMap {
            region: r,
            points_v: pv,
            points_h: ph,
            spacing_v: 0.5,
            spacing_h: 1.0 / 3.0,
            origin_v: 0.0,
            origin_h: 0.0,
            map_planes,
            gains,
        }
    }

    /// The staged develop computed stage by stage on whole-frame buffers (the
    /// definition the streamed form must reproduce bit for bit).
    fn reference(
        ops: &[Vec<Opcode>; 3],
        img: &RawImage,
        info: &FrameInfo,
        origin: (u32, u32),
        params: &DevelopParams,
        enc: RgbEncoding,
    ) -> (u32, u32, u32, RawPixelData) {
        let mut l = Lists { ops: ops.clone(), notes: Vec::new() };
        let mut norm = process::normalize(img, info, origin, false);
        l.apply_list2(&mut norm, img.width, img.cpp, info, origin, false);
        norm.iter_mut().for_each(|v| *v *= 65535.0);
        let lin = RawImage {
            width: img.width,
            height: img.height,
            cpp: img.cpp,
            data: PixelData::F32(norm),
            float_max: None,
        };
        let mut linfo = info.clone();
        linfo.black_level = BlackLevel::uniform(0.0);
        linfo.white_level = WhiteLevel::uniform(65535);
        let window = active_window(info, origin, img.width, img.height);
        if !l.has_pixel_ops(2) || params.demosaic == Demosaic::HalfSize || window.is_none() {
            let d = process::develop(&lin, &linfo, origin, params, false).unwrap();
            return process::encode_image(d, enc, &params.curve, params.orientation, false);
        }
        let (win, x0, y0) = window.unwrap();
        let cam = DevelopParams { crop: win, ..params.camera_rgb() };
        let mut dev = process::develop(&lin, &linfo, origin, &cam, false).unwrap();
        l.apply_list3(&mut dev.rgb, dev.width, dev.height, info, x0, y0, false);
        let c = params.crop.intersect(&win).unwrap_or(win);
        let mut rgb = Vec::new();
        for y in c.y - win.y..c.bottom() - win.y {
            for x in c.x - win.x..c.right() - win.x {
                let p = &dev.rgb[(y * dev.width + x) as usize * 3..][..3];
                rgb.extend(process::finish_rgb([p[0], p[1], p[2]], &params.wb, &params.cam_to_rgb, params.levels));
            }
        }
        process::encode_image(
            Developed { width: c.width, height: c.height, rgb },
            enc,
            &params.curve,
            params.orientation,
            false,
        )
    }

    #[test]
    fn streamed_stages_match_whole_frame_stages() {
        let (w, h) = (61u32, 45u32);
        let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
        for (i, v) in img.u16_mut().iter_mut().enumerate() {
            *v = ((i * 7919) % 4000 + 40) as u16;
        }
        let mut info = FrameInfo::new(w, h, 12);
        info.cfa = Some(CfaPattern::RGGB);
        info.black_level = BlackLevel::pattern2x2([60.0, 64.0, 62.0, 66.0]);
        info.white_level = WhiteLevel::uniform(4095);
        info.white_balance = WhiteBalance::from_multipliers(2.0, 1.0, 1.5, 1.0);
        info.color_matrices = ColorMatrices {
            color: vec![
                ColorMatrix::new(Illuminant::D65, 3, vec![0.6, -0.1, -0.05, -0.4, 1.2, 0.2, -0.1, 0.25, 0.6]).unwrap(),
            ],
            ..Default::default()
        };
        info.active_area = Rect::new(2, 3, 55, 40);
        let (aw, ah) = (55, 40);
        let list2 = vec![
            gain_map(region(0, 0, ah, aw, 1), 1),
            Opcode::ScalePerRow {
                region: region(4, 0, 30, aw, 1),
                scales: (0..26).map(|i| 1.0 + i as f32 * 0.01).collect(),
            },
            Opcode::MapPolynomial { region: region(0, 0, ah, aw, 1), coeffs: vec![0.001, 0.98, 0.02] },
        ];
        let list3 = vec![
            gain_map(region(0, 0, ah, aw, 3), 3),
            Opcode::FixVignetteRadial { k: [0.2, -0.05, 0.01, 0.0, 0.0], cx: 0.45, cy: 0.55 },
            Opcode::DeltaPerColumn {
                region: region(0, 5, ah, 40, 3),
                deltas: (0..35).map(|i| i as f32 * 1e-4).collect(),
            },
            Opcode::WarpRectilinear { planes: vec![([1.0, 0.0, 0.0, 0.0], [0.0, 0.0])], cx: 0.5, cy: 0.5 },
        ];
        let bad = Opcode::FixBadPixelsList { bayer_phase: 0, points: vec![(7, 9), (20, 30)], rects: vec![] };
        let bad3 = Opcode::FixBadPixelsList { bayer_phase: 0, points: vec![(5, 6)], rects: vec![] };
        let variants: Vec<[Vec<Opcode>; 3]> = vec![
            // Streamed list 2 and list 3.
            [vec![], list2.clone(), list3.clone()],
            // List 2 only (also: list 3 without pixel opcodes).
            [vec![], list2.clone(), vec![]],
            // List 3 only.
            [vec![], vec![], list3.clone()],
            // Bad-pixel repairs: whole-frame list 2, whole-window list 3.
            [vec![], [list2.clone(), vec![bad]].concat(), [list3.clone(), vec![bad3]].concat()],
        ];
        let levels = process::Levels::new(0.7, 0.02, 0.9).unwrap();
        for ops in &variants {
            for origin in [(0, 0), (1, 2)] {
                for (o, demosaic, crop, lv) in [
                    (1, Demosaic::Bilinear, Rect::new(4, 5, 49, 33), None),
                    (6, Demosaic::Bilinear, Rect::new(4, 5, 49, 33), levels),
                    (3, Demosaic::Bilinear, Rect::new(0, 0, w, h), None),
                    (8, Demosaic::HalfSize, Rect::new(4, 5, 49, 33), levels),
                ] {
                    let mut info = info.clone();
                    info.orientation = Orientation::from_exif(o);
                    let mut params = DevelopParams::new(&info, demosaic, crop);
                    params.levels = lv;
                    for enc in [
                        RgbEncoding::Rgba8,
                        RgbEncoding::LinearF32,
                        RgbEncoding::Srgb16,
                        RgbEncoding::Linear16,
                        RgbEncoding::LinearF16,
                    ] {
                        let want = reference(ops, &img, &info, origin, &params, enc);
                        for parallel in [false, true] {
                            let mut l = Lists { ops: ops.clone(), notes: Vec::new() };
                            let got = l.develop(&img, &info, origin, &params, enc, parallel).unwrap();
                            assert_eq!(got, want, "{enc:?} orientation {o} {demosaic:?} origin {origin:?}");
                        }
                    }
                }
            }
        }
    }
}
