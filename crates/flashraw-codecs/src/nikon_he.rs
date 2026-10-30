//! Nikon High Efficiency (HE and HE★) raw decoder (JPEG XS / intoPIX TicoRAW).
//!
//! Nikon mirrorless bodies (Z8, Z9, Z6 III, Z50 II, Zf) record high-efficiency RAWs as a
//! JPEG XS codestream (ISO/IEC 21122) with four components on a quarter-size grid (one per
//! 2x2 Bayer quad position).
//!
//! # Provenance
//!
//! This module is a port, onto FlashRaw's bit pumps, parallel helpers and error types, of the
//! Nikon High Efficiency decoder of RAWmakase (<https://github.com/pch/rawmakase>,
//! `crates/rawmakase-raw/src/codec/nikon_he.rs`, by Piotr Chmolowski), which is MIT-licensed:
//! Copyright (c) 2026 RAWmakase contributors. Its samples equal that decoder's on every
//! Nikon High Efficiency file of the raw.pixls.us corpus (`tests/regression/nikon.tsv`).
//!
//! # Codec Architecture
//!
//! 1. **Codestream Container**: Markers SOC (`0xff10`), CAP (`0xff50`), PIH (`0xff12`),
//!    CDT (`0xff13`), WGT (`0xff14`), SLH (`0xff20`), EOC (`0xff11`).
//! 2. **Entropy Decoding**: Slices are decoded in parallel. Each precinct decodes significance
//!    flags, Greatest Coded Line Indices (GCLI), bit-plane magnitudes, and signs.
//! 3. **Wavelet Inversion**: Reversible 5/3 lifting horizontally across bands and vertically
//!    across precinct line pairs.
//! 4. **Colour Decorrelation**: Inverse Star-Tetrix lifting (Y, Cr, Δ, Cb) -> (G1, R, G2, B).
//! 5. **Transfer Curve**: Companding inversion from the 13 vendor bytes into 14-bit raw values,
//!    interleaved directly into the output Bayer raster.

use flashraw_core::{Error, Result, par};

const SOC: u16 = 0xff10;
const EOC: u16 = 0xff11;
const PIH: u16 = 0xff12;
const CDT: u16 = 0xff13;
const WGT: u16 = 0xff14;
const SLH: u16 = 0xff20;
const CAP: u16 = 0xff50;

/// Decodes a Nikon High Efficiency codestream into `out`.
///
/// `out` must have at least `pitch * height` elements. `width` and `height` must match
/// the codestream's declared full sensor dimensions.
pub fn decode(data: &[u8], out: &mut [u16], width: usize, height: usize, pitch: usize, parallel: bool) -> Result<()> {
    let header = Header::parse(data)?;
    if (header.width, header.height) != (width, height) {
        return Err(Error::corrupt(format!(
            "high-efficiency codestream is {}x{}, expected {}x{}",
            header.width, header.height, width, height
        )));
    }
    if pitch < width || out.len() < pitch * height {
        return Err(Error::InvalidArgument("destination buffer too small for high-efficiency decode".into()));
    }

    let layout = Layout::new(&header)?;
    let precincts = header.precincts(data)?;
    let mut coefficients = Coefficients::new(&layout);

    decode_slices(data, &header, &layout, &precincts, &mut coefficients, parallel)?;

    let mut planes = coefficients.inverse_wavelet(&layout, parallel);
    inverse_star_tetrix(&mut planes, layout.width, layout.height, parallel);

    let transfer = header.transfer_curve();
    let [g1, red, g2, blue] = &planes;
    let w = layout.width;
    let lut = &transfer.table;

    let target_len = 2 * pitch * layout.height;
    par::for_each_row_chunk(&mut out[..target_len], 2 * pitch, 16, parallel, |y0, chunk| {
        for (dy, rows) in chunk.chunks_exact_mut(2 * pitch).enumerate() {
            let y = y0 + dy;
            let (top, bottom) = rows.split_at_mut(pitch);
            let plane = y * w..(y + 1) * w;
            let (r, g, h, b) = (&red[plane.clone()], &g1[plane.clone()], &g2[plane.clone()], &blue[plane]);
            for x in 0..w {
                top[2 * x] = lut[(r[x] + 32768).clamp(0, 65535) as usize];
                top[2 * x + 1] = lut[(g[x] + 32768).clamp(0, 65535) as usize];
                bottom[2 * x] = lut[(h[x] + 32768).clamp(0, 65535) as usize];
                bottom[2 * x + 1] = lut[(b[x] + 32768).clamp(0, 65535) as usize];
            }
        }
    });

    Ok(())
}

/// The picture header, component table and weights.
#[derive(Debug, Clone)]
pub struct Header {
    pub width: usize,
    pub height: usize,
    pub slice_height: usize,
    pub group: usize,
    pub significance_group: usize,
    pub fraction_bits: u32,
    pub raw_gcli_bits: u32,
    pub levels: Vec<u32>,
    pub weights: Vec<(u8, u8)>,
    pub black: i64,
    pub gain: i64,
    pub knee: i64,
    pub first_slice: usize,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let b = self
            .data
            .get(self.at..self.at + n)
            .ok_or_else(|| Error::corrupt("high-efficiency codestream truncated"))?;
        self.at += n;
        Ok(b)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
}

impl Header {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader { data, at: 0 };
        if r.u16()
            .map_err(|_| Error::unsupported_compression("NEF", "not a Nikon high-efficiency (JPEG XS) codestream"))?
            != SOC
        {
            return Err(Error::unsupported_compression("NEF", "not a Nikon high-efficiency (JPEG XS) codestream"));
        }
        let mut header: Option<Header> = None;
        let mut components = 0;
        loop {
            let at = r.at;
            let marker = match r.u16() {
                Ok(m) => m,
                Err(_) => {
                    return Err(Error::unsupported_compression(
                        "NEF",
                        "Nikon high-efficiency codestream truncated or missing picture header",
                    ));
                }
            };
            if marker == SLH {
                let mut h = header.ok_or_else(|| {
                    Error::unsupported_compression("NEF", "Nikon high-efficiency codestream without picture header")
                })?;
                if h.levels.len() != components {
                    return Err(Error::unsupported_compression(
                        "NEF",
                        "Nikon high-efficiency codestream without component table",
                    ));
                }
                h.first_slice = at;
                return Ok(h);
            }
            if marker == EOC {
                return Err(Error::unsupported_compression("NEF", "Nikon high-efficiency codestream without slices"));
            }
            let length = match r.u16() {
                Ok(l) => usize::from(l),
                Err(_) => {
                    return Err(Error::unsupported_compression(
                        "NEF",
                        "Nikon high-efficiency codestream truncated before picture header",
                    ));
                }
            };
            if length < 2 {
                return Err(Error::unsupported_compression("NEF", "bad Nikon high-efficiency marker length"));
            }
            let body = match r.bytes(length - 2) {
                Ok(b) => b,
                Err(_) => {
                    return Err(Error::unsupported_compression(
                        "NEF",
                        "Nikon high-efficiency codestream truncated before picture header",
                    ));
                }
            };
            let mut b = Reader { data: body, at: 0 };
            match marker {
                CAP => {}
                PIH => {
                    if body.len() != 37 {
                        return Err(Error::unsupported_compression(
                            "NEF",
                            "unknown Nikon high-efficiency picture header",
                        ));
                    }
                    let _codestream_length = b.bytes(4)?;
                    let _profile_level = b.bytes(4)?;
                    let width = usize::from(b.u16()?);
                    let height = usize::from(b.u16()?);
                    let precinct_width = b.u16()?;
                    let slice_height = usize::from(b.u16()?);
                    components = usize::from(b.u8()?);
                    let group = usize::from(b.u8()?);
                    let significance_group = usize::from(b.u8()?);
                    let _nominal_bits = b.u8()?;
                    let fq_br = b.u8()?;
                    let order = b.u8()?;
                    let decomposition = b.u8()?;
                    let flags = b.u8()?;
                    let t = b.bytes(13)?;

                    if precinct_width != 0
                        || components != 4
                        || group != 4
                        || significance_group == 0
                        || slice_height == 0
                        || (order >> 4 & 7) != 1
                        || (decomposition & 15) != 1
                    {
                        return Err(Error::unsupported_compression(
                            "NEF",
                            "Nikon high-efficiency layout is not supported",
                        ));
                    }
                    if width < 8 || height < 8 || width % 2 != 0 || height % 4 != 0 {
                        return Err(Error::corrupt(format!("bad Nikon high-efficiency dimensions {width}x{height}")));
                    }
                    if (flags & 0x3f) != 0b01_01_00 {
                        return Err(Error::unsupported_compression(
                            "NEF",
                            format!("Nikon high-efficiency coding flags {flags:#04x} are not supported"),
                        ));
                    }
                    if fq_br != 0x44 {
                        return Err(Error::unsupported_compression(
                            "NEF",
                            format!("Nikon high-efficiency precision {fq_br:#04x} is not supported"),
                        ));
                    }

                    header = Some(Header {
                        width,
                        height,
                        slice_height,
                        group,
                        significance_group,
                        fraction_bits: u32::from(fq_br >> 4),
                        raw_gcli_bits: u32::from(fq_br & 15),
                        levels: Vec::new(),
                        weights: Vec::new(),
                        black: (i64::from(t[3] & 15) << 8) | i64::from(t[4]),
                        gain: (i64::from(t[7] & 3) << 16) | (i64::from(t[8]) << 8) | i64::from(t[9]),
                        knee: (i64::from(t[10]) << 8) | i64::from(t[11]),
                        first_slice: 0,
                    });
                }
                CDT => {
                    let h = header
                        .as_mut()
                        .ok_or_else(|| Error::unsupported_compression("NEF", "missing PIH before CDT"))?;
                    if body.len() != 3 * components {
                        return Err(Error::unsupported_compression(
                            "NEF",
                            "unknown Nikon high-efficiency component table",
                        ));
                    }
                    for _ in 0..components {
                        let (depth, sampling, decomposition) = (b.u8()?, b.u8()?, b.u8()?);
                        if depth != 14 || sampling != 0x11 {
                            return Err(Error::unsupported_compression(
                                "NEF",
                                "unknown Nikon high-efficiency component depth or sampling",
                            ));
                        }
                        let (horizontal, vertical) = (decomposition >> 4, decomposition & 15);
                        if !((horizontal, vertical) == (0, 0) || (vertical == 1 && horizontal >= 1)) {
                            return Err(Error::unsupported_compression(
                                "NEF",
                                "Nikon high-efficiency decomposition is not supported",
                            ));
                        }
                        h.levels.push(u32::from(horizontal));
                    }
                }
                WGT => {
                    let h = header
                        .as_mut()
                        .ok_or_else(|| Error::unsupported_compression("NEF", "missing PIH before WGT"))?;
                    if body.len() % 2 != 0 {
                        return Err(Error::corrupt("bad JPEG XS weights"));
                    }
                    h.weights = body.chunks_exact(2).map(|w| (w[0], w[1])).collect();
                }
                _ => {}
            }
        }
    }

    fn precincts(&self, data: &[u8]) -> Result<Vec<usize>> {
        let count = self.height / 4;
        let mut at = self.first_slice;
        let mut offsets = Vec::with_capacity(count);
        for p in 0..count {
            if p % self.slice_height == 0 {
                if data.get(at..at + 2) != Some(&[0xff, 0x20][..]) {
                    return Err(Error::corrupt("missing JPEG XS slice header"));
                }
                at += 6;
            }
            let length_slice =
                data.get(at..at + 3).ok_or_else(|| Error::corrupt("high-efficiency codestream truncated"))?;
            offsets.push(at);
            let length = (usize::from(length_slice[0]) << 16)
                | (usize::from(length_slice[1]) << 8)
                | usize::from(length_slice[2]);
            at += self.precinct_header_length() + length;
        }
        if data.get(at..at + 2) != Some(&EOC.to_be_bytes()[..]) {
            return Err(Error::corrupt("trailing data after JPEG XS last precinct"));
        }
        Ok(offsets)
    }

    fn bands(&self) -> usize {
        self.levels.iter().map(|&l| if l == 0 { 1 } else { l as usize + 3 }).sum()
    }

    fn precinct_header_length(&self) -> usize {
        5 + (2 * self.bands()).div_ceil(8)
    }

    pub fn transfer_curve(&self) -> TransferCurve {
        TransferCurve::new(self.black, self.gain, self.knee)
    }
}

pub struct TransferCurve {
    pub table: Box<[u16; 65536]>,
}

impl TransferCurve {
    pub fn new(black: i64, gain: i64, knee: i64) -> Self {
        let mut table = Box::new([0u16; 65536]);
        for i in 0..65536usize {
            let d = i as i64 - knee;
            let q = if d < 0 {
                -d * d
            } else {
                let t = (d * gain + 32768) >> 16;
                t * t
            };
            table[i] = (black + ((q + 163_840) >> 18)).clamp(0, 16383) as u16;
        }
        Self { table }
    }

    #[inline(always)]
    pub fn apply(&self, v: i32) -> u16 {
        self.table[(v + 32768).clamp(0, 65535) as usize]
    }
}

#[derive(Debug, Clone)]
struct Band {
    component: usize,
    offset: usize,
    width: usize,
    high: bool,
    gain: u8,
    priority: u8,
}

#[derive(Debug)]
struct Packet {
    bands: std::ops::Range<usize>,
    line: usize,
}

struct Layout {
    width: usize,
    height: usize,
    levels: Vec<u32>,
    bands: Vec<Band>,
    packets: Vec<Packet>,
}

impl Layout {
    fn new(header: &Header) -> Result<Self> {
        let width = header.width / 2;
        let mut bands = Vec::new();
        let mut packets = Vec::new();

        for line in 0..2 {
            for (component, &levels) in header.levels.iter().enumerate() {
                let start = bands.len();
                let make_band = |offset, w, high| Band { component, offset, width: w, high, gain: 0, priority: 0 };
                let range = match (levels, line) {
                    (0, 0) => {
                        bands.push(make_band(0, width, false));
                        start..start + 1
                    }
                    (0, _) => {
                        let b = bands
                            .iter()
                            .position(|b| b.component == component)
                            .ok_or_else(|| Error::corrupt("JPEG XS band missing"))?;
                        b..b + 1
                    }
                    (_, 0) => {
                        let mut low = width;
                        let mut highs = Vec::with_capacity(levels as usize);
                        for _ in 0..levels {
                            highs.push(low / 2);
                            low -= low / 2;
                        }
                        let mut offset = 0;
                        for w in std::iter::once(low).chain(highs.into_iter().rev()) {
                            bands.push(make_band(offset, w, false));
                            offset += w;
                        }
                        start..bands.len()
                    }
                    _ => {
                        bands.push(make_band(0, width - width / 2, true));
                        bands.push(make_band(width - width / 2, width / 2, true));
                        start..bands.len()
                    }
                };
                packets.push(Packet { bands: range, line });
            }
        }

        if bands.len() != header.weights.len() {
            return Err(Error::corrupt("JPEG XS weights count does not match bands count"));
        }
        for (band, &(gain, priority)) in bands.iter_mut().zip(&header.weights) {
            band.gain = gain;
            band.priority = priority;
        }

        Ok(Layout { width, height: header.height / 2, levels: header.levels.clone(), bands, packets })
    }
}

struct Coefficients {
    width: usize,
    components: usize,
    rows: Vec<i32>,
}

impl Coefficients {
    fn new(layout: &Layout) -> Self {
        let precincts = layout.height / 2;
        Coefficients {
            width: layout.width,
            components: layout.levels.len(),
            rows: vec![0; precincts * 2 * layout.levels.len() * layout.width],
        }
    }

    #[inline(always)]
    fn precinct_len(&self) -> usize {
        2 * self.components * self.width
    }

    fn inverse_wavelet(mut self, layout: &Layout, parallel: bool) -> [Vec<i32>; 4] {
        let (w, n) = (self.width, self.components);
        let precinct_len = self.precinct_len();

        let process_precinct = |scratch: &mut Vec<i32>, precinct: &mut [i32]| {
            if scratch.len() < w {
                scratch.resize(w, 0);
            }
            for (c, &levels) in layout.levels.iter().enumerate() {
                if levels == 0 {
                    continue;
                }
                let low = &mut precinct[c * w..(c + 1) * w];
                let mut widths = [0usize; 8];
                let mut len = w;
                let levels_usize = (levels as usize).min(8);
                for i in 0..levels_usize {
                    widths[i] = len;
                    len -= len / 2;
                }
                for i in (0..levels_usize).rev() {
                    inverse_53(low, widths[i], scratch);
                }
                let high = &mut precinct[(n + c) * w..(n + c + 1) * w];
                inverse_53(high, w, scratch);
            }
        };

        #[cfg(feature = "rayon")]
        if parallel {
            use rayon::prelude::*;
            self.rows.par_chunks_mut(precinct_len).for_each_init(
                || vec![0i32; w],
                |scratch, precinct| {
                    process_precinct(scratch, precinct);
                },
            );
        } else {
            let mut scratch = vec![0i32; w];
            for precinct in self.rows.chunks_mut(precinct_len) {
                process_precinct(&mut scratch, precinct);
            }
        }
        #[cfg(not(feature = "rayon"))]
        {
            let _ = parallel;
            let mut scratch = vec![0i32; w];
            for precinct in self.rows.chunks_mut(precinct_len) {
                process_precinct(&mut scratch, precinct);
            }
        }

        let rows = &self.rows;
        let precincts = layout.height / 2;
        let row = |p: usize, c: usize, high: bool| {
            let at = p * precinct_len + (usize::from(high) * n + c) * w;
            &rows[at..at + w]
        };

        let build_plane = |c: usize| -> Vec<i32> {
            let mut plane = vec![0i32; w * layout.height];
            par::for_each_row_chunk(&mut plane, 2 * w, 16, parallel, |p0, chunk| {
                for (dp, pair) in chunk.chunks_exact_mut(2 * w).enumerate() {
                    let p = p0 + dp;
                    let (even, odd) = pair.split_at_mut(w);
                    if layout.levels[c] == 0 {
                        even.copy_from_slice(row(p, c, false));
                        odd.copy_from_slice(row(p, c, true));
                        continue;
                    }
                    let cur_high = row(p, c, true);
                    let cur_low = row(p, c, false);
                    let above_high = row(p.saturating_sub(1), c, true);
                    let below = (p + 1).min(precincts - 1);
                    let below_high = row(below, c, true);
                    let below_low = row(below, c, false);

                    for x in 0..w {
                        let e = cur_low[x] - ((above_high[x] + cur_high[x] + 2) >> 2);
                        let next = if below == p { e } else { below_low[x] - ((cur_high[x] + below_high[x] + 2) >> 2) };
                        even[x] = e;
                        odd[x] = cur_high[x] + ((e + next) >> 1);
                    }
                }
            });
            plane
        };

        [build_plane(0), build_plane(1), build_plane(2), build_plane(3)]
    }
}

#[inline(always)]
fn inverse_53(line: &mut [i32], len: usize, scratch: &mut [i32]) {
    if len < 2 {
        return;
    }
    let lows = len - len / 2;
    let highs = len / 2;
    let (low, high) = line[..len].split_at(lows);
    let out = &mut scratch[..len];

    let h0 = high[0];
    out[0] = low[0] - ((h0 * 2 + 2) >> 2);
    for k in 1..lows {
        let left = high[k - 1];
        let right = if k < highs { high[k] } else { high[highs - 1] };
        out[2 * k] = low[k] - ((left + right + 2) >> 2);
    }
    for k in 0..highs {
        let right = if 2 * k + 2 < len { out[2 * k + 2] } else { out[2 * k] };
        out[2 * k + 1] = high[k] + ((out[2 * k] + right) >> 1);
    }
    line[..len].copy_from_slice(out);
}

fn inverse_star_tetrix(planes: &mut [Vec<i32>; 4], w: usize, h: usize, parallel: bool) {
    let [y, cr, delta, cb] = planes;

    // Pass 1: Y -= mean of 4 diagonal deltas >> 3
    {
        let delta_slice = &*delta;
        par::for_each_row_chunk(y, w, 32, parallel, |r0, chunk| {
            for (dr, row) in chunk.chunks_exact_mut(w).enumerate() {
                let r = r0 + dr;
                let up_r = r.saturating_sub(1);
                let d_up = &delta_slice[up_r * w..(up_r + 1) * w];
                let d_cur = &delta_slice[r * w..(r + 1) * w];
                for (x, v) in row.iter_mut().enumerate() {
                    let rx = (x + 1).min(w - 1);
                    let s = d_up[x] + d_up[rx] + d_cur[x] + d_cur[rx];
                    *v -= s >> 3;
                }
            }
        });
    }

    // Pass 2: delta -= (chroma >> 3) + (greens >> 2)
    {
        let (y_slice, cr_slice, cb_slice) = (&*y, &*cr, &*cb);
        par::for_each_row_chunk(delta, w, 32, parallel, |r0, chunk| {
            for (dr, row) in chunk.chunks_exact_mut(w).enumerate() {
                let r = r0 + dr;
                let down_r = (r + 1).min(h - 1);
                let cr_cur = &cr_slice[r * w..(r + 1) * w];
                let cr_down = &cr_slice[down_r * w..(down_r + 1) * w];
                let cb_cur = &cb_slice[r * w..(r + 1) * w];
                let y_cur = &y_slice[r * w..(r + 1) * w];
                let y_down = &y_slice[down_r * w..(down_r + 1) * w];
                for (x, v) in row.iter_mut().enumerate() {
                    let lx = x.saturating_sub(1);
                    let chroma = cr_cur[x] + cr_down[x] + cb_cur[lx] + cb_cur[x];
                    let greens = y_cur[lx] + y_cur[x] + y_down[lx] + y_down[x];
                    *v = *v - (chroma >> 3) + (greens >> 2);
                }
            }
        });
    }

    // Pass 3: Y -= chroma >> 3
    {
        let (cr_slice, cb_slice) = (&*cr, &*cb);
        par::for_each_row_chunk(y, w, 32, parallel, |r0, chunk| {
            for (dr, row) in chunk.chunks_exact_mut(w).enumerate() {
                let r = r0 + dr;
                let up_r = r.saturating_sub(1);
                let cr_cur = &cr_slice[r * w..(r + 1) * w];
                let cb_up = &cb_slice[up_r * w..(up_r + 1) * w];
                let cb_cur = &cb_slice[r * w..(r + 1) * w];
                for (x, v) in row.iter_mut().enumerate() {
                    let rx = (x + 1).min(w - 1);
                    let chroma = cr_cur[x] + cr_cur[rx] + cb_up[x] + cb_cur[x];
                    *v -= chroma >> 3;
                }
            }
        });
    }

    // Pass 4: Cr += greens >> 2
    {
        let (g1, g2) = (&*y, &*delta);
        par::for_each_row_chunk(cr, w, 32, parallel, |r0, chunk| {
            for (dr, row) in chunk.chunks_exact_mut(w).enumerate() {
                let r = r0 + dr;
                let up_r = r.saturating_sub(1);
                let g1_cur = &g1[r * w..(r + 1) * w];
                let g2_up = &g2[up_r * w..(up_r + 1) * w];
                let g2_cur = &g2[r * w..(r + 1) * w];
                for (x, v) in row.iter_mut().enumerate() {
                    let lx = x.saturating_sub(1);
                    let greens = g1_cur[lx] + g1_cur[x] + g2_up[x] + g2_cur[x];
                    *v += greens >> 2;
                }
            }
        });
    }

    // Pass 5: Cb += greens >> 2
    {
        let (g1, g2) = (&*y, &*delta);
        par::for_each_row_chunk(cb, w, 32, parallel, |r0, chunk| {
            for (dr, row) in chunk.chunks_exact_mut(w).enumerate() {
                let r = r0 + dr;
                let down_r = (r + 1).min(h - 1);
                let g1_cur = &g1[r * w..(r + 1) * w];
                let g1_down = &g1[down_r * w..(down_r + 1) * w];
                let g2_cur = &g2[r * w..(r + 1) * w];
                for (x, v) in row.iter_mut().enumerate() {
                    let rx = (x + 1).min(w - 1);
                    let greens = g1_cur[x] + g1_down[x] + g2_cur[x] + g2_cur[rx];
                    *v += greens >> 2;
                }
            }
        });
    }
}

/// High-throughput MSB bit reader for JPEG XS codestreams.
struct HeBits<'a> {
    data: &'a [u8],
    at_bit: usize,
    end_bit: usize,
}

impl<'a> HeBits<'a> {
    #[inline(always)]
    fn new(data: &'a [u8], start_byte: usize, end_byte: usize) -> Self {
        Self { data, at_bit: start_byte * 8, end_bit: end_byte * 8 }
    }

    #[inline(always)]
    fn peek(&self) -> u64 {
        let byte = self.at_bit / 8;
        let end_byte = self.end_bit.div_ceil(8);
        if byte + 8 <= self.data.len() && byte + 8 <= end_byte {
            let w = u64::from_be_bytes(self.data[byte..byte + 8].try_into().unwrap());
            w << (self.at_bit & 7)
        } else {
            let mut word = [0u8; 8];
            if byte < self.data.len() && byte < end_byte {
                let avail = (self.data.len() - byte).min(end_byte - byte).min(8);
                word[..avail].copy_from_slice(&self.data[byte..byte + avail]);
            }
            u64::from_be_bytes(word) << (self.at_bit & 7)
        }
    }

    #[inline(always)]
    fn read(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        if self.at_bit + n as usize > self.end_bit {
            return Err(Error::corrupt("high-efficiency packet is truncated"));
        }
        let v = (self.peek() >> (64 - n)) as u32;
        self.at_bit += n as usize;
        Ok(v)
    }

    #[inline(always)]
    fn unary(&mut self) -> Result<u32> {
        let p = self.peek();
        let ones = p.leading_ones();
        if ones >= 32 {
            return Err(Error::corrupt("bad high-efficiency GCLI code"));
        }
        if self.at_bit + (ones as usize) >= self.end_bit {
            return Err(Error::corrupt("high-efficiency packet is truncated"));
        }
        self.at_bit += ones as usize + 1;
        Ok(ones)
    }

    #[inline(always)]
    fn align(&mut self) {
        self.at_bit = (self.at_bit + 7) & !7;
    }

    #[inline(always)]
    fn byte_pos(&self) -> usize {
        self.at_bit / 8
    }

    #[inline(always)]
    fn skip_to(&mut self, byte: usize) -> Result<()> {
        let bit = byte * 8;
        if bit < self.at_bit || bit > self.end_bit {
            return Err(Error::corrupt("high-efficiency sub-packet overruns its length"));
        }
        self.at_bit = bit;
        Ok(())
    }
}

struct SliceState {
    previous: Vec<Option<(Vec<u8>, u8)>>,
}

impl SliceState {
    fn new(layout: &Layout) -> Self {
        Self { previous: vec![None; layout.bands.len()] }
    }
}

fn decode_slices(
    data: &[u8],
    header: &Header,
    layout: &Layout,
    precincts: &[usize],
    coefficients: &mut Coefficients,
    parallel: bool,
) -> Result<()> {
    let precinct_len = coefficients.precinct_len();
    let (w, n) = (coefficients.width, coefficients.components);

    #[cfg(feature = "rayon")]
    if parallel {
        use rayon::prelude::*;
        return coefficients
            .rows
            .par_chunks_mut(precinct_len * header.slice_height)
            .zip(precincts.par_chunks(header.slice_height))
            .try_for_each(|(rows, offsets)| {
                let mut state = SliceState::new(layout);
                for (precinct, &offset) in rows.chunks_mut(precinct_len).zip(offsets) {
                    decode_precinct(data, offset, header, layout, &mut state, precinct, w, n)?;
                }
                Ok(())
            });
    }

    let _ = parallel;
    for (rows, offsets) in
        coefficients.rows.chunks_mut(precinct_len * header.slice_height).zip(precincts.chunks(header.slice_height))
    {
        let mut state = SliceState::new(layout);
        for (precinct, &offset) in rows.chunks_mut(precinct_len).zip(offsets) {
            decode_precinct(data, offset, header, layout, &mut state, precinct, w, n)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_precinct(
    data: &[u8],
    offset: usize,
    header: &Header,
    layout: &Layout,
    state: &mut SliceState,
    rows: &mut [i32],
    w: usize,
    n: usize,
) -> Result<()> {
    let head = data
        .get(offset..offset + header.precinct_header_length())
        .ok_or_else(|| Error::corrupt("truncated precinct header"))?;
    let length = (usize::from(head[0]) << 16) | (usize::from(head[1]) << 8) | usize::from(head[2]);
    let (quantisation, refinement) = (head[3], head[4]);
    let mode = |b: usize| (head[5 + b / 4] >> (6 - 2 * (b % 4))) & 3;
    let start = offset + head.len();
    let end = start + length;
    if end > data.len() {
        return Err(Error::corrupt("high-efficiency codestream is truncated"));
    }

    let mut bits = HeBits::new(data, start, end);
    let group = header.group;
    let ss = header.significance_group;
    let gtli = |band: &Band| {
        let refine = u8::from(band.priority < refinement);
        quantisation.saturating_sub(band.gain).saturating_sub(refine).min(15)
    };

    for packet in &layout.packets {
        let head_w = bits.read(32)?;
        let tail_w = bits.read(24)?;
        let raw = (head_w >> 31) != 0;
        let data_len = ((head_w >> 11) & 0xf_ffff) as usize;
        let gcli_len = (((head_w & 0x7ff) << 9) | (tail_w >> 15)) as usize;
        let sign_len = (tail_w & 0x7fff) as usize;

        let mut significance: Vec<Option<Vec<bool>>> = Vec::with_capacity(packet.bands.len());
        if !raw {
            for b in packet.bands.clone() {
                let band = &layout.bands[b];
                if (mode(b) & 2) != 0 {
                    let groups = band.width.div_ceil(group).div_ceil(ss);
                    let mut flags = Vec::with_capacity(groups);
                    for _ in 0..groups {
                        flags.push(bits.read(1)? != 0);
                    }
                    significance.push(Some(flags));
                } else {
                    significance.push(None);
                }
            }
            bits.align();
        }

        let gcli_start = bits.byte_pos();
        let mut gclis: Vec<Vec<u8>> = Vec::with_capacity(packet.bands.len());
        for (k, b) in packet.bands.clone().enumerate() {
            let band = &layout.bands[b];
            let t = gtli(band);
            let groups = band.width.div_ceil(group);
            let mut line = vec![0u8; groups];
            if raw {
                for g in line.iter_mut() {
                    *g = bits.read(header.raw_gcli_bits)? as u8;
                }
            } else {
                let is_insignificant = |g: usize| {
                    significance
                        .get(k)
                        .and_then(|s| s.as_ref())
                        .is_some_and(|s| s.get(g / ss).copied().unwrap_or(false))
                };
                match (&state.previous[b], (mode(b) & 1) != 0) {
                    (Some((top, top_gtli)), true) => {
                        let floor = (*top_gtli).max(t);
                        for (g, out_val) in line.iter_mut().enumerate() {
                            let m = top.get(g).copied().unwrap_or(0).max(floor);
                            *out_val = if is_insignificant(g) {
                                m
                            } else {
                                let x = bits.unary()? as i32;
                                let room = (i32::from(m) - i32::from(t)).max(0);
                                let delta = if x > 2 * room {
                                    x - room
                                } else if (x & 1) == 1 {
                                    -(x + 1) / 2
                                } else {
                                    x / 2
                                };
                                u8::try_from(i32::from(m) + delta)
                                    .map_err(|_| Error::corrupt("bad high-efficiency GCLI"))?
                            };
                        }
                    }
                    _ => {
                        for (g, out_val) in line.iter_mut().enumerate() {
                            *out_val = if is_insignificant(g) {
                                t
                            } else {
                                u8::try_from(u32::from(t) + bits.unary()?)
                                    .map_err(|_| Error::corrupt("bad high-efficiency GCLI"))?
                            };
                        }
                    }
                }
            }
            if line.iter().any(|&g| g > 15) {
                return Err(Error::corrupt("bad high-efficiency GCLI > 15"));
            }
            gclis.push(line);
        }
        bits.align();
        bits.skip_to(gcli_start + gcli_len)?;

        let data_start = bits.byte_pos();
        let mut values: Vec<Vec<u32>> = Vec::with_capacity(packet.bands.len());
        let mut negative: Vec<Vec<bool>> = Vec::with_capacity(packet.bands.len());
        for (k, b) in packet.bands.clone().enumerate() {
            let band = &layout.bands[b];
            let t = gtli(band);
            let groups = band.width.div_ceil(group);
            let mut v = vec![0u32; groups * group];
            for g in 0..groups {
                let gcli = gclis[k][g];
                if gcli <= t {
                    continue;
                }
                let planes = gcli - t;
                let mut v0 = 0u32;
                let mut v1 = 0u32;
                let mut v2 = 0u32;
                let mut v3 = 0u32;
                for _ in 0..planes {
                    let plane = bits.read(4)?;
                    v0 = (v0 << 1) | ((plane >> 3) & 1);
                    v1 = (v1 << 1) | ((plane >> 2) & 1);
                    v2 = (v2 << 1) | ((plane >> 1) & 1);
                    v3 = (v3 << 1) | (plane & 1);
                }
                let base = g * 4;
                v[base] = v0 << t;
                v[base + 1] = v1 << t;
                v[base + 2] = v2 << t;
                v[base + 3] = v3 << t;
            }
            negative.push(vec![false; v.len()]);
            values.push(v);
        }
        bits.align();
        bits.skip_to(data_start + data_len)?;

        let sign_start = bits.byte_pos();
        for k in 0..packet.bands.len() {
            for (val, sign) in values[k].iter().zip(negative[k].iter_mut()) {
                if *val != 0 {
                    *sign = bits.read(1)? != 0;
                }
            }
        }
        bits.align();
        bits.skip_to(sign_start + sign_len)?;

        for (k, b) in packet.bands.clone().enumerate() {
            let band = &layout.bands[b];
            let t = gtli(band);
            let row = usize::from(band.high || packet.line == 1) * n + band.component;
            let out_slice = &mut rows[row * w + band.offset..row * w + band.offset + band.width];
            for (i, o) in out_slice.iter_mut().enumerate() {
                let gcli = gclis[k][i / group];
                let mut m = values[k][i];
                if t > 0 && gcli > t && m != 0 {
                    let scale = u32::from(gcli - t) + 1;
                    let mut rest = m;
                    m = 0;
                    while rest > 0 {
                        m += rest;
                        rest >>= scale;
                    }
                }
                let val = (m as i32) << header.fraction_bits;
                *o = if negative[k][i] { -val } else { val };
            }
            state.previous[b] = Some((std::mem::take(&mut gclis[k]), t));
        }
    }

    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    const VENDOR: [u8; 13] = [0x50, 0x88, 0x70, 0x83, 0xf0, 0x15, 0x23, 0xd1, 0x49, 0xcd, 0x3f, 0x7f, 0x07];

    struct BitWriter {
        bytes: Vec<u8>,
        bits: usize,
    }

    impl BitWriter {
        fn new() -> Self {
            Self { bytes: Vec::new(), bits: 0 }
        }

        fn put(&mut self, value: u64, n: usize) {
            for i in (0..n).rev() {
                if self.bits % 8 == 0 {
                    self.bytes.push(0);
                }
                let bit = ((value >> i) & 1) as u8;
                *self.bytes.last_mut().unwrap() |= bit << (7 - (self.bits % 8));
                self.bits += 1;
            }
        }

        fn align(&mut self) {
            self.bits = (self.bits + 7) & !7;
        }
    }

    pub fn blank_codestream(width: u16, height: u16) -> Vec<u8> {
        blank(width, height, VENDOR)
    }

    pub fn blank_value() -> u16 {
        let t: i64 = (16513 * 84429 + 32768) >> 16;
        (1008 + ((t * t + 163_840) >> 18)) as u16
    }

    fn blank(width: u16, height: u16, vendor: [u8; 13]) -> Vec<u8> {
        coded(width, height, vendor, None)
    }

    fn coded(width: u16, height: u16, vendor: [u8; 13], first_gcli: Option<u32>) -> Vec<u8> {
        let mut s = vec![0xff, 0x10, 0xff, 0x50, 0, 34];
        s.extend(b"CONTACT_INTOPIX_");
        s.extend([0; 16]);
        s.extend([0xff, 0x12, 0, 39, 0, 0, 0, 0, 0, 0, 0, 0]);
        s.extend(width.to_be_bytes());
        s.extend(height.to_be_bytes());
        s.extend([0, 0, 0, 16, 4, 4, 8, 18, 0x44, 0x10, 0x51, 0x14]);
        s.extend(vendor);
        s.extend([0xff, 0x13, 0, 14]);
        s.extend([14, 0x11, 0x51, 14, 0x11, 0x51, 14, 0x11, 0x00, 14, 0x11, 0x51]);
        s.extend([0xff, 0x14, 0, 52]);
        s.extend((0..25u8).flat_map(|b| [0, b]));

        let header = Header::parse(&[s.clone(), vec![0xff, 0x20]].concat()).unwrap();
        let layout = Layout::new(&header).unwrap();
        let precincts = usize::from(height) / 4;
        for p in 0..precincts {
            if p % 16 == 0 {
                s.extend([0xff, 0x20, 0, 4]);
                s.extend(((p / 16) as u16).to_be_bytes());
            }
            let mut payload = BitWriter::new();
            for (k, packet) in layout.packets.iter().enumerate() {
                let gcli = first_gcli.filter(|_| p == 0 && k == 0);
                let gcli_len = gcli.map_or(0, |x| (x as u64 + 1).div_ceil(8));
                payload.put(gcli_len << 15, 56);
                for (i, band) in layout.bands[packet.bands.clone()].iter().enumerate() {
                    let groups = band.width.div_ceil(4).div_ceil(8);
                    let flags = if gcli.is_some() && i == 0 { (1u64 << (groups - 1)) - 1 } else { u64::MAX };
                    payload.put(flags, groups);
                }
                payload.align();
                if let Some(x) = gcli {
                    payload.put(u64::MAX, x as usize);
                    payload.put(0, 1);
                    payload.align();
                }
            }
            let length = payload.bytes.len() as u32;
            s.extend(&length.to_be_bytes()[1..]);
            s.extend([0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xc0]);
            s.extend(payload.bytes);
        }
        s.extend([0xff, 0x11]);
        s
    }

    #[test]
    fn a_blank_codestream_decodes_to_the_transfer_curve_at_zero() {
        let (width, height) = (64, 32);
        let stream = blank_codestream(width as u16, height as u16);
        let mut out = vec![0u16; width * height];
        decode(&stream, &mut out, width, height, width, true).unwrap();
        assert_eq!(blank_value(), 2734);
        assert!(out.iter().all(|&v| v == blank_value()), "{:?}", &out[..8]);
    }

    #[test]
    fn decoding_fills_only_the_image_columns_of_each_row() {
        let (width, height, pitch) = (64, 32, 70);
        let stream = blank(width as u16, height as u16, VENDOR);
        let mut out = vec![7u16; pitch * height];
        decode(&stream, &mut out, width, height, pitch, false).unwrap();
        for row in out.chunks(pitch) {
            assert!(row[..width].iter().all(|&v| v != 7));
            assert!(row[width..].iter().all(|&v| v == 7));
        }
        assert!(decode(&stream, &mut out, 62, height, pitch, false).is_err());
        assert!(decode(&stream, &mut out[..pitch * 31], width, height, pitch, false).is_err());
    }

    #[test]
    fn the_transfer_curve_rounds_the_gain_half_up() {
        let mut vendor = VENDOR;
        vendor[7..10].copy_from_slice(&[0xd1, 0x01, 0x71]);
        let header = Header::parse(&blank(64, 32, vendor)).unwrap();
        assert_eq!((header.black, header.gain, header.knee), (1008, 65905, 16255));
        let curve = header.transfer_curve();
        assert_eq!(curve.apply(49023 - 32768), 5151);
        assert_eq!(curve.apply(16255 - 32768 - 1000), 1008 - 4);
        assert_eq!(curve.apply(i32::MIN / 2), 0);
        assert_eq!(curve.apply(i32::MAX / 2), curve.apply(32767));
        assert_eq!(curve.apply(32767), 10377);
        let header = Header::parse(&blank(64, 32, VENDOR)).unwrap();
        assert_eq!(header.transfer_curve().apply(32767), 16383);
    }

    #[test]
    fn a_gcli_beyond_15_bits_is_refused() {
        let (width, height) = (64, 32);
        let mut out = vec![0u16; width * height];
        let gcli = |x| coded(width as u16, height as u16, VENDOR, Some(x));
        let error = decode(&gcli(15), &mut out, width, height, width, true).unwrap_err();
        assert!(!error.to_string().contains("GCLI"), "{error}");
        for x in [16, 31] {
            let error = decode(&gcli(x), &mut out, width, height, width, true).unwrap_err();
            assert!(error.to_string().contains("GCLI"), "{x}: {error}");
        }
    }

    #[test]
    fn damaged_codestreams_fail_without_panicking() {
        let (width, height) = (64, 32);
        let stream = blank(width as u16, height as u16, VENDOR);
        let mut out = vec![0u16; width * height];
        for len in 0..stream.len() {
            assert!(decode(&stream[..len], &mut out, width, height, width, true).is_err(), "decoded {len} bytes");
        }
        for at in 0..stream.len() {
            for value in [0x00, 0x7f, 0xff] {
                let mut damaged = stream.clone();
                damaged[at] = value;
                let _ = decode(&damaged, &mut out, width, height, width, false);
            }
        }
    }
}
