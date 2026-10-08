//! X3F CAMF calibration block: entry parsing and the as-shot colour calibration.
//!
//! The decoded block (see [`flashraw_codecs::x3f::CamfStream`]) is a sequence of
//! entries, each starting with a 20-byte header: `CMb` + kind (`T` text, `P` property
//! list, `M` matrix), version, entry size, name offset and value offset (offsets
//! relative to the entry start, names NUL-terminated).
//! - property list value: count, offset of the string area, then `count` pairs of
//!   (name, value) string offsets relative to that area;
//! - matrix value: element type (0 i16, 1/2 u32, 3 f32, 5 u8, 6 u16), number of
//!   dimensions, data offset, then per dimension (size, name offset, index).
//!
//! ## Colour calibration
//! The as-shot white balance names one preset (`Auto`, `Sunlight`, ...) whose
//! calibration is looked up through property lists keyed by that name:
//! - DP1 … Quattro: `WhiteBalanceGains` (or `DP1_WhiteBalanceGains`) gives the raw
//!   channel gains, further scaled by the `TempGainFact`, `FNumberGainFact` and
//!   `SensorAdjustmentGainFact` vectors when present, and
//!   `WhiteBalanceColorCorrections` (or `DP1_…`) a matrix from white-balanced raw to
//!   linear sRGB;
//! - SD9 … SD14: `WhiteBalanceIlluminants` gives a camera → XYZ matrix and
//!   `WhiteBalanceCorrections` an XYZ correction applied after it; the neutral is the
//!   `<preset>RGBNeutral` vector when present, otherwise the camera response to D65.

use flashraw_codecs::x3f::CamfStream;
use flashraw_core::{ColorMatrix, Illuminant, Rect};

/// Property list naming every entry of the block.
const INCLUDE_BLOCKS: &str = "IncludeBlocks";

/// One parsed entry: kind, name and the entry's byte range in the block.
#[derive(Debug, Clone)]
struct EntryRef {
    kind: u8,
    name: String,
    start: usize,
    end: usize,
    value: usize,
}

/// CAMF block, decoded and parsed on demand: lookups decode entries in order until the
/// name is found, the `IncludeBlocks` list (when already parsed) shows it is absent, or
/// the block ends. The colour calibration sits in the first few hundred KiB of blocks
/// that run to several MiB.
#[derive(Default)]
pub(crate) struct Camf<'a> {
    stream: Option<CamfStream<'a>>,
    /// The block when it was given decoded.
    fixed: Vec<u8>,
    entries: Vec<EntryRef>,
    /// Offset of the next entry.
    next: usize,
    /// No further entries.
    ended: bool,
}

/// A CAMF matrix converted to `f64`, with its dimensions (outermost first).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Matrix {
    pub dims: Vec<u32>,
    pub values: Vec<f64>,
}

fn le32(d: &[u8], o: usize) -> Option<u32> {
    let b = d.get(o..o.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn cstr(d: &[u8], o: usize) -> Option<&str> {
    let s = d.get(o..)?;
    let end = s.iter().position(|&b| b == 0)?;
    std::str::from_utf8(&s[..end]).ok()
}

/// Bytes decoded ahead of the entry being parsed.
const CHUNK: usize = 32 << 10;

impl<'a> Camf<'a> {
    /// A block decoded from a `SECc` section as entries are needed.
    pub(crate) fn from_section(section: &'a [u8]) -> Option<Camf<'a>> {
        Some(Camf { stream: Some(CamfStream::new(section).ok()?), ..Camf::default() })
    }

    /// An already decoded block.
    #[cfg(test)]
    pub(crate) fn parse(data: Vec<u8>) -> Camf<'static> {
        Camf { fixed: data, ..Camf::default() }
    }

    fn bytes(&self) -> &[u8] {
        self.stream.as_ref().map(|s| s.decoded()).unwrap_or(&self.fixed)
    }

    /// Make `len` bytes available; false when the block is shorter (or corrupt).
    fn ensure(&mut self, len: usize) -> bool {
        if let Some(s) = &mut self.stream {
            if s.decoded().len() < len && !s.is_done() && s.fill(len.saturating_add(CHUNK)).is_err() {
                return false;
            }
        }
        self.bytes().len() >= len
    }

    /// Parse the next entry; false at the end of the block or at a malformed entry.
    fn parse_next(&mut self) -> bool {
        if self.ended || self.entries.len() >= 1 << 16 {
            return false;
        }
        let p = self.next;
        let parsed = (|| {
            if !self.ensure(p.checked_add(20)?) {
                return None;
            }
            let h = &self.bytes()[p..p + 20];
            if &h[..3] != b"CMb" || !matches!(h[3], b'T' | b'P' | b'M') {
                return None;
            }
            let kind = h[3];
            let (size, no, vo) = (le32(h, 8)? as usize, le32(h, 12)? as usize, le32(h, 16)? as usize);
            let end = p.checked_add(size).filter(|_| size >= 20 && vo < size)?;
            if !self.ensure(end) {
                return None;
            }
            let name = cstr(&self.bytes()[p..end], no)?.to_string();
            Some(EntryRef { kind, name, start: p, end, value: vo })
        })();
        match parsed {
            Some(e) => {
                self.next = e.end;
                self.entries.push(e);
                true
            }
            None => {
                self.ended = true;
                false
            }
        }
    }

    /// Index of entry `name` of `kind`, parsing further entries as needed.
    fn find(&mut self, kind: u8, name: &str) -> Option<usize> {
        let mut checked = 0;
        loop {
            if let Some(i) = self.entries[checked..].iter().position(|e| e.kind == kind && e.name == name) {
                return Some(checked + i);
            }
            checked = self.entries.len();
            if name != INCLUDE_BLOCKS {
                let listed = self
                    .entries
                    .iter()
                    .position(|e| e.kind == b'P' && e.name == INCLUDE_BLOCKS)
                    .map(|i| self.list_has(i, name));
                if listed == Some(false) {
                    return None;
                }
            }
            if !self.parse_next() {
                return None;
            }
        }
    }

    fn entry(&mut self, kind: u8, name: &str) -> Option<(&[u8], usize)> {
        let i = self.find(kind, name)?;
        let e = &self.entries[i];
        Some((&self.bytes()[e.start..e.end], e.value))
    }

    /// `(name, value)` string offsets of property-list entry `i` (relative to the entry).
    fn list_pairs(e: &[u8], v: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let n = le32(e, v).unwrap_or(0) as usize;
        let area = le32(e, v + 4).unwrap_or(u32::MAX) as usize;
        (0..n.min(4096)).map_while(move |i| {
            let k = area.checked_add(le32(e, v + 8 + 8 * i)? as usize)?;
            let val = area.checked_add(le32(e, v + 12 + 8 * i)? as usize)?;
            Some((k, val))
        })
    }

    /// True when property list entry `i` has key `key`.
    fn list_has(&self, i: usize, key: &str) -> bool {
        let r = &self.entries[i];
        let e = &self.bytes()[r.start..r.end];
        Self::list_pairs(e, r.value).any(|(k, _)| cstr(e, k) == Some(key))
    }

    /// Value of `key` in property list `block`.
    pub(crate) fn property(&mut self, block: &str, key: &str) -> Option<String> {
        let (e, v) = self.entry(b'P', block)?;
        Self::list_pairs(e, v)
            .find(|&(k, _)| cstr(e, k) == Some(key))
            .and_then(|(_, val)| cstr(e, val))
            .map(String::from)
    }

    /// Matrix `name` (at most 2^20 elements).
    pub(crate) fn matrix(&mut self, name: &str) -> Option<Matrix> {
        let (e, v) = self.entry(b'M', name)?;
        let (typ, ndim, off) = (le32(e, v)?, le32(e, v + 4)? as usize, le32(e, v + 8)? as usize);
        if ndim == 0 || ndim > 4 {
            return None;
        }
        let mut dims = Vec::with_capacity(ndim);
        let mut n = 1usize;
        for i in 0..ndim {
            let d = le32(e, v + 12 + 12 * i)?;
            n = n.checked_mul(d as usize).filter(|&n| n <= 1 << 20)?;
            dims.push(d);
        }
        let size = match typ {
            0 | 6 => 2,
            1..=3 => 4,
            5 => 1,
            _ => return None,
        };
        let raw = e.get(off..off.checked_add(n.checked_mul(size)?)?)?;
        let values = raw
            .chunks_exact(size)
            .map(|c| match typ {
                0 => i16::from_le_bytes([c[0], c[1]]) as f64,
                6 => u16::from_le_bytes([c[0], c[1]]) as f64,
                1 | 2 => u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
                3 => f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
                _ => c[0] as f64,
            })
            .collect();
        Some(Matrix { dims, values })
    }

    /// Matrix `name` when it holds exactly `n` finite values.
    fn values(&mut self, name: &str, n: usize) -> Option<Vec<f64>> {
        self.matrix(name).map(|m| m.values).filter(|v| v.len() == n && v.iter().all(|x| x.is_finite()))
    }

    /// Matrix `name` when all its values are finite.
    fn values_any(&mut self, name: &str) -> Option<Vec<f64>> {
        self.matrix(name).map(|m| m.values).filter(|v| !v.is_empty() && v.iter().all(|x| x.is_finite()))
    }

    /// Matrix named by the property `wb` of list `block`.
    fn preset(&mut self, block: &str, wb: &str, n: usize) -> Option<Vec<f64>> {
        let name = self.property(block, wb)?;
        self.values(&name, n)
    }

    /// Rectangle stored as inclusive `[left, top, right, bottom]` coordinates.
    pub(crate) fn rect(&mut self, name: &str) -> Option<Rect> {
        let v = self.values(name, 4)?;
        if v.iter().any(|&x| !(0.0..=65535.0).contains(&x)) {
            return None;
        }
        Rect::from_ltrb(v[0] as u32, v[1] as u32, v[2] as u32 + 1, v[3] as u32 + 1)
    }

    /// Name of the white-balance preset recorded by Quattro bodies (no property list).
    pub(crate) fn white_balance_name(&mut self) -> Option<&'static str> {
        let v = *self.values("WhiteBalance", 1)?.first()?;
        Some(match v as u32 {
            1 => "Auto",
            2 => "Sunlight",
            3 => "Shade",
            4 => "Overcast",
            5 => "Incandescent",
            6 => "Fluorescent",
            7 => "Flash",
            8 => "Custom",
            12 => "AutoLSP",
            _ => return None,
        })
    }
}

/// One layer's spatial gain: a `rows x cols` grid of multipliers whose corner points sit on
/// the corners of the raw buffer (bilinear between points).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GainGrid {
    pub rows: usize,
    pub cols: usize,
    pub gains: Vec<f32>,
}

impl GainGrid {
    /// `self + w * other` (same dimensions).
    fn add_scaled(&mut self, other: &GainGrid, w: f64) {
        for (a, b) in self.gains.iter_mut().zip(&other.gains) {
            *a += (*b as f64 * w) as f32;
        }
    }
}

/// Weights of the two calibration points bracketing `x` (clamped to the ends):
/// `[(index, weight)]`, interpolated linearly in `f(x)`.
fn bracket(points: &[f64], x: f64, f: impl Fn(f64) -> f64) -> Vec<(usize, f64)> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 || x <= points[0] {
        return vec![(0, 1.0)];
    }
    if x >= points[n - 1] {
        return vec![(n - 1, 1.0)];
    }
    let i = points.windows(2).position(|p| x >= p[0] && x <= p[1]).unwrap_or(0);
    let (a, b) = (f(points[i]), f(points[i + 1]));
    let t = if b > a { ((f(x) - a) / (b - a)).clamp(0.0, 1.0) } else { 0.0 };
    vec![(i, 1.0 - t), (i + 1, t)]
}

impl Camf<'_> {
    /// Keys of property list `block`.
    pub(crate) fn property_keys(&mut self, block: &str) -> Vec<String> {
        let Some((e, v)) = self.entry(b'P', block) else { return Vec::new() };
        Self::list_pairs(e, v).filter_map(|(k, _)| cstr(e, k).map(String::from)).collect()
    }

    /// Scalar matrix `name`.
    pub(crate) fn scalar(&mut self, name: &str) -> Option<f64> {
        self.values(name, 1).map(|v| v[0])
    }

    /// Grid of `[rows, cols, 3]` float gains (classic and TRUE I bodies), one per layer.
    fn float_grids(&mut self, name: &str) -> Option<[GainGrid; 3]> {
        let m = self.matrix(name)?;
        let [rows, cols, 3] = m.dims[..] else { return None };
        let (rows, cols) = (rows as usize, cols as usize);
        if rows < 2 || cols < 2 || m.values.iter().any(|v| !v.is_finite() || *v <= 0.0 || *v > 16.0) {
            return None;
        }
        Some(std::array::from_fn(|c| GainGrid {
            rows,
            cols,
            gains: m.values.iter().skip(c).step_by(3).map(|&v| v as f32).collect(),
        }))
    }

    /// Merrill-type layer table: `MinGains<C> + Delta<C> * GainsTable<C>` from property
    /// list `props`.
    fn coded_grid(&mut self, props: &str, layer: char) -> Option<GainGrid> {
        let min: f64 = self.property(props, &format!("MinGains{layer}"))?.trim().parse().ok()?;
        let delta: f64 = self.property(props, &format!("Delta{layer}"))?.trim().parse().ok()?;
        let table = self.property(props, &format!("GainsTable{layer}"))?;
        let m = self.matrix(&table)?;
        let [rows, cols] = m.dims[..] else { return None };
        let (rows, cols) = (rows as usize, cols as usize);
        if rows < 2 || cols < 2 || !min.is_finite() || !delta.is_finite() {
            return None;
        }
        let gains: Vec<f32> = m.values.iter().map(|&v| (min + delta * v) as f32).collect();
        gains.iter().all(|g| *g > 0.0 && *g < 16.0).then_some(GainGrid { rows, cols, gains })
    }

    /// The weighted sum of the Merrill-type tables `props(key)` for each layer.
    fn coded_mix(&mut self, parts: &[(String, f64)]) -> Option<[GainGrid; 3]> {
        let mut out: [Option<GainGrid>; 3] = Default::default();
        for (props, w) in parts.iter().filter(|p| p.1 > 0.0) {
            for (c, layer) in ['R', 'G', 'B'].into_iter().enumerate() {
                let g = self.coded_grid(props, layer)?;
                match &mut out[c] {
                    Some(acc) if acc.rows == g.rows && acc.cols == g.cols => acc.add_scaled(&g, *w),
                    Some(_) => return None,
                    None => {
                        let mut z = GainGrid { rows: g.rows, cols: g.cols, gains: vec![0.0; g.gains.len()] };
                        z.add_scaled(&g, *w);
                        out[c] = Some(z);
                    }
                }
            }
        }
        let [a, b, c] = out;
        Some([a?, b?, c?])
    }
}

/// Lens shading of the shot: per-layer gain grids whose corner points sit on the corners
/// of the `width x height` raw buffer, when the block describes it.
///
/// - Spatial gains ([`spatial_gain`]). They are stored rotated by 180 degrees relative to
///   the decoded image: applied that way, the corner-to-centre colour of the renders
///   follows the embedded previews (on the corpus and real-world DP Merrill files the mean
///   |log| of the corner R/G and B/G error drops from 0.34 / 0.33 without the tables to
///   0.22 / 0.15; applied unturned, 0.30 / 0.24).
/// - Merrill colour shading (`WhiteBalanceColorShadingFactor` of the preset, a 2x2 matrix):
///   the bottom (R) and top (B) layers are scaled by `1 + a r + b r^2`, `r` being the
///   distance from the buffer centre over the half diagonal, one row of coefficients per
///   layer (the error drops further to 0.14 / 0.12).
pub(crate) fn shading(camf: &mut Camf, preset: &str, width: u32, height: u32) -> Option<[GainGrid; 3]> {
    let mut grids = spatial_gain(camf, preset);
    if let Some(gs) = &mut grids {
        // Reversing a row-major grid turns it by 180 degrees.
        gs.iter_mut().for_each(|g| g.gains.reverse());
    }
    if let Some(f) = camf.preset("WhiteBalanceColorShadingFactor", preset, 4) {
        let unit = |rows: usize, cols: usize| GainGrid { rows, cols, gains: vec![1.0; rows * cols] };
        let gs = grids.get_or_insert_with(|| std::array::from_fn(|_| unit(33, 49)));
        let (w, h) = (width as f64, height as f64);
        let half_diag = (w * w + h * h).sqrt() / 2.0;
        for (layer, k) in [(0, 0), (2, 2)] {
            let g = &mut gs[layer];
            for i in 0..g.rows {
                for j in 0..g.cols {
                    let x = j as f64 * (w - 1.0) / (g.cols - 1) as f64;
                    let y = i as f64 * (h - 1.0) / (g.rows - 1) as f64;
                    let r = (x - w / 2.0).hypot(y - h / 2.0) / half_diag.max(1.0);
                    g.gains[i * g.cols + j] *= (1.0 + f[k] * r + f[k + 1] * r * r) as f32;
                }
            }
        }
    }
    grids.filter(|gs| gs.iter().all(|g| g.gains.iter().all(|v| v.is_finite() && *v > 0.0)))
}

/// Spatial gains of the shot, per layer, when the block has them:
/// - classic bodies: `SpatialGain` (`[rows, cols, 3]`);
/// - TRUE I bodies (DP1/DP2): the `SpatialGainTables` entry of the white-balance preset;
/// - DP Merrill: the infinity-focus tables `SpatialGain{R,G,B}_<i>_INF` (coded, see
///   `SpatialGainsProps_<i>_INF`) for the apertures listed in `SpatialGain_Fstop`,
///   interpolated over the aperture in stops;
/// - SD1 Merrill: `SpatialGainsProps_<f-number>_<focal length>` coded tables, interpolated
///   over the aperture (in stops) and the focal length; shots outside the calibrated focal
///   lengths (other lenses) get none.
fn spatial_gain(camf: &mut Camf, preset: &str) -> Option<[GainGrid; 3]> {
    let aperture = camf.scalar("CaptureAperture").filter(|a| a.is_finite() && *a > 0.0);
    let stops = |f: f64| f.max(0.1).log2();
    if let Some(fstops) = camf.values_any("SpatialGain_Fstop") {
        let parts: Vec<(String, f64)> = bracket(&fstops, aperture?, stops)
            .into_iter()
            .map(|(i, w)| (format!("SpatialGainsProps_{i}_INF"), w))
            .collect();
        return camf.coded_mix(&parts);
    }
    let keys: Vec<(f64, f64, String)> = camf
        .property_keys("IncludeBlocks")
        .into_iter()
        .filter_map(|k| {
            let rest = k.strip_prefix("SpatialGainsProps_")?;
            let (f, focal) = rest.split_once('_')?;
            Some((f.parse().ok()?, focal.parse().ok()?, k.clone()))
        })
        .collect();
    if !keys.is_empty() {
        let ap = aperture?;
        let mut fls: Vec<f64> = keys.iter().map(|k| k.1).collect();
        fls.sort_by(f64::total_cmp);
        fls.dedup();
        let focal = camf.scalar("FocalLength").filter(|f| *f >= fls[0] && *f <= fls[fls.len() - 1])?;
        let mut parts = Vec::new();
        for (fi, wf) in bracket(&fls, focal, |x| x) {
            let mut aps: Vec<f64> = keys.iter().filter(|k| k.1 == fls[fi]).map(|k| k.0).collect();
            aps.sort_by(f64::total_cmp);
            for (ai, wa) in bracket(&aps, ap, stops) {
                let name = keys.iter().find(|k| k.1 == fls[fi] && k.0 == aps[ai])?.2.clone();
                parts.push((name, wf * wa));
            }
        }
        return camf.coded_mix(&parts);
    }
    if let Some(table) = camf.property("SpatialGainTables", preset) {
        if let Some(g) = camf.float_grids(&table) {
            return Some(g);
        }
    }
    camf.float_grids("SpatialGain")
}

/// As-shot colour calibration: raw channel gains and the XYZ → camera matrix.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Calibration {
    /// The white-balance preset used.
    pub preset: String,
    /// Multipliers that make the as-shot neutral equal in all channels.
    pub gains: [f64; 3],
    /// XYZ → camera (raw, before the gains), row-major.
    pub xyz_to_cam: [f64; 9],
}

impl Calibration {
    /// The XYZ → camera matrix as frame colour data.
    pub(crate) fn color_matrix(&self) -> Option<ColorMatrix> {
        let illuminant = match self.preset.as_str() {
            "Sunlight" => Illuminant(1),
            "Fluorescent" => Illuminant(2),
            "Incandescent" => Illuminant(3),
            "Flash" => Illuminant(4),
            "Overcast" => Illuminant(10),
            "Shade" => Illuminant(11),
            _ => Illuminant::UNKNOWN,
        };
        ColorMatrix::new(illuminant, 3, self.xyz_to_cam.iter().map(|&v| v as f32).collect())
    }
}

/// Linear sRGB (D65) → XYZ.
const SRGB_TO_XYZ: [f64; 9] =
    [0.412_456_4, 0.357_576_1, 0.180_437_5, 0.212_672_9, 0.715_152_2, 0.072_175, 0.019_333_9, 0.119_192, 0.950_304_1];

/// D65 white point (XYZ, Y = 1).
const D65_XYZ: [f64; 3] = [0.950_47, 1.0, 1.088_83];

pub(crate) fn mul3(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut r = [0.0; 9];
    for i in 0..3 {
        for j in 0..3 {
            r[i * 3 + j] = (0..3).map(|k| a[i * 3 + k] * b[k * 3 + j]).sum();
        }
    }
    r
}

pub(crate) fn inv3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let k = 1.0 / det;
    Some([
        (e * i - f * h) * k,
        (c * h - b * i) * k,
        (b * f - c * e) * k,
        (f * g - d * i) * k,
        (a * i - c * g) * k,
        (c * d - a * f) * k,
        (d * h - e * g) * k,
        (b * g - a * h) * k,
        (a * e - b * d) * k,
    ])
}

fn mat_vec(m: &[f64; 9], v: &[f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (0..3).map(|k| m[i * 3 + k] * v[k]).sum())
}

fn arr9(v: Vec<f64>) -> Option<[f64; 9]> {
    v.try_into().ok()
}

/// The calibration for preset `wb` (falling back to `Auto` when the preset is unknown).
pub(crate) fn calibration(camf: &mut Camf, wb: &str) -> Option<Calibration> {
    // Some bodies name the daylight preset "Daylight" while the calibration uses "Sunlight".
    let wb = if wb == "Daylight" { "Sunlight" } else { wb };
    calibration_for(camf, wb).or_else(|| calibration_for(camf, "Auto"))
}

fn calibration_for(camf: &mut Camf, wb: &str) -> Option<Calibration> {
    let mut gains: [f64; 3];
    let raw_to_xyz: [f64; 9];
    let gain_list = ["WhiteBalanceGains", "DP1_WhiteBalanceGains"];
    let cc_list = ["WhiteBalanceColorCorrections", "DP1_WhiteBalanceColorCorrections"];
    if let (Some(g), Some(cc)) = (
        gain_list.iter().find_map(|b| camf.preset(b, wb, 3)),
        cc_list.iter().find_map(|b| camf.preset(b, wb, 9)).and_then(arr9),
    ) {
        // Gains bring the raw neutral to (1, 1, 1); the correction maps balanced raw
        // to linear sRGB.
        gains = [g[0], g[1], g[2]];
        for fact in ["TempGainFact", "FNumberGainFact", "SensorAdjustmentGainFact", "DigitalISOGain"] {
            if let Some(f) = camf.values(fact, 3) {
                for c in 0..3 {
                    gains[c] *= f[c];
                }
            }
        }
        let diag = [gains[0], 0.0, 0.0, 0.0, gains[1], 0.0, 0.0, 0.0, gains[2]];
        raw_to_xyz = mul3(&mul3(&SRGB_TO_XYZ, &cc), &diag);
    } else {
        let cam_to_xyz = arr9(camf.preset("WhiteBalanceIlluminants", wb, 9)?)?;
        let correction = camf
            .preset("WhiteBalanceCorrections", wb, 9)
            .and_then(arr9)
            .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        raw_to_xyz = mul3(&correction, &cam_to_xyz);
        let neutral = match camf.values(&format!("{wb}RGBNeutral"), 3) {
            Some(n) => [n[0], n[1], n[2]],
            None => mat_vec(&inv3(&raw_to_xyz)?, &D65_XYZ),
        };
        gains = neutral.map(|n| 1.0 / n);
    }
    let xyz_to_cam = inv3(&raw_to_xyz)?;
    if gains.iter().any(|g| !g.is_finite() || *g <= 0.0) || xyz_to_cam.iter().any(|v| !v.is_finite()) {
        return None;
    }
    Some(Calibration { preset: wb.to_string(), gains, xyz_to_cam })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builder of synthetic CAMF blocks.
    #[derive(Default)]
    pub(crate) struct Block(pub Vec<u8>);

    impl Block {
        fn entry(&mut self, kind: u8, name: &str, value: Vec<u8>, value_align: usize) {
            let mut e = b"CMb".to_vec();
            e.push(kind);
            e.extend_from_slice(&2u32.to_le_bytes());
            e.extend_from_slice(&[0; 12]); // size, name offset, value offset
            let name_off = e.len();
            e.extend_from_slice(name.as_bytes());
            e.push(0);
            while e.len() % value_align != 0 {
                e.push(0);
            }
            let value_off = e.len();
            e.extend_from_slice(&value);
            let size = e.len() as u32;
            e[8..12].copy_from_slice(&size.to_le_bytes());
            e[12..16].copy_from_slice(&(name_off as u32).to_le_bytes());
            e[16..20].copy_from_slice(&(value_off as u32).to_le_bytes());
            self.0.extend_from_slice(&e);
        }

        pub(crate) fn property(mut self, name: &str, pairs: &[(&str, &str)]) -> Self {
            // Value: count, string-area offset (relative to the entry), pairs, strings.
            let mut strings = Vec::new();
            let mut offs = Vec::new();
            for (k, v) in pairs {
                let ko = strings.len() as u32;
                strings.extend_from_slice(k.as_bytes());
                strings.push(0);
                let vo = strings.len() as u32;
                strings.extend_from_slice(v.as_bytes());
                strings.push(0);
                offs.push((ko, vo));
            }
            // The string area follows the value header; its entry offset is only known
            // once the name is placed: name is at 20, value at the next 4-aligned offset.
            let value_off = (20 + name.len() + 1).div_ceil(4) * 4;
            let area = value_off + 8 + 8 * pairs.len();
            let mut v = Vec::new();
            v.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
            v.extend_from_slice(&(area as u32).to_le_bytes());
            for (k, val) in offs {
                v.extend_from_slice(&k.to_le_bytes());
                v.extend_from_slice(&val.to_le_bytes());
            }
            v.extend_from_slice(&strings);
            self.entry(b'P', name, v, 4);
            self
        }

        pub(crate) fn matrix(mut self, name: &str, dims: &[u32], values: &[f32]) -> Self {
            let value_off = (20 + name.len() + 1).div_ceil(4) * 4;
            let data_off = value_off + 12 + 12 * dims.len();
            let mut v = Vec::new();
            v.extend_from_slice(&3u32.to_le_bytes());
            v.extend_from_slice(&(dims.len() as u32).to_le_bytes());
            v.extend_from_slice(&(data_off as u32).to_le_bytes());
            for (i, d) in dims.iter().enumerate() {
                v.extend_from_slice(&d.to_le_bytes());
                v.extend_from_slice(&0u32.to_le_bytes());
                v.extend_from_slice(&(i as u32).to_le_bytes());
            }
            for x in values {
                v.extend_from_slice(&x.to_le_bytes());
            }
            self.entry(b'M', name, v, 4);
            self
        }

        pub(crate) fn text(mut self, name: &str, text: &str) -> Self {
            let mut v = (text.len() as u32 + 1).to_le_bytes().to_vec();
            v.extend_from_slice(text.as_bytes());
            v.push(0);
            self.entry(b'T', name, v, 4);
            self
        }
    }

    /// Parse every entry; returns how many there are.
    fn count(c: &mut Camf) -> usize {
        while c.parse_next() {}
        c.entries.len()
    }

    const IDENTITY: [f32; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

    #[test]
    fn entries_properties_and_matrices() {
        let b = Block::default()
            .text("CalibrationInfo", "test")
            .property("WhiteBalanceGains", &[("Auto", "AutoWBGain"), ("Sunlight", "SunWBGain")])
            .matrix("AutoWBGain", &[3], &[2.0, 1.0, 0.5])
            .matrix("Grid", &[2, 3], &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        let mut c = Camf::parse(b.0);
        assert_eq!(count(&mut c), 4);
        assert_eq!(c.property("WhiteBalanceGains", "Sunlight").as_deref(), Some("SunWBGain"));
        assert_eq!(c.property("WhiteBalanceGains", "Shade"), None);
        assert_eq!(c.property("Missing", "Auto"), None);
        assert_eq!(c.matrix("AutoWBGain").unwrap().values, vec![2.0, 1.0, 0.5]);
        let g = c.matrix("Grid").unwrap();
        assert_eq!((g.dims.clone(), g.values.len()), (vec![2, 3], 6));
        assert!(c.matrix("CalibrationInfo").is_none());
    }

    #[test]
    fn malformed_blocks_stop_parsing() {
        let b = Block::default().matrix("A", &[1], &[1.0]).0;
        // Truncated in the second entry, garbage after the first, oversized sizes.
        let mut two = b.clone();
        two.extend_from_slice(&b[..30]);
        assert_eq!(count(&mut Camf::parse(two)), 1);
        let mut garbage = b.clone();
        garbage.extend_from_slice(b"CMbX\0\0\0\0");
        assert_eq!(count(&mut Camf::parse(garbage)), 1);
        let mut huge = b.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(count(&mut Camf::parse(huge)), 0);
        for cut in 0..b.len() {
            let mut c = Camf::parse(b[..cut].to_vec());
            assert!(c.matrix("A").is_none());
        }
        // A matrix whose dimensions overflow is rejected.
        let big = Block::default().matrix("B", &[1 << 16, 1 << 16], &[]).0;
        assert!(Camf::parse(big).matrix("B").is_none());
    }

    #[test]
    fn gains_and_colour_correction() {
        let cc = [2.0, -0.5, -0.5, -0.25, 1.5, -0.25, 0.0, -1.0, 2.0];
        let b = Block::default()
            .property("WhiteBalanceGains", &[("Auto", "AutoWBGain"), ("Flash", "FlashWBGain")])
            .property("WhiteBalanceColorCorrections", &[("Auto", "AutoCC"), ("Flash", "AutoCC")])
            .matrix("AutoWBGain", &[3], &[2.0, 1.0, 0.5])
            .matrix("FlashWBGain", &[3], &[1.5, 1.0, 0.75])
            .matrix("AutoCC", &[3, 3], &cc)
            .matrix("TempGainFact", &[3], &[1.0, 1.0, 2.0]);
        let mut c = Camf::parse(b.0);
        let cal = calibration(&mut c, "Flash").unwrap();
        assert_eq!(cal.gains, [1.5, 1.0, 1.5]);
        assert_eq!(cal.color_matrix().unwrap().illuminant, Illuminant(4));
        // Unknown presets fall back to Auto.
        let cal = calibration(&mut c, "Bulb").unwrap();
        assert_eq!((cal.preset.as_str(), cal.gains), ("Auto", [2.0, 1.0, 1.0]));
        // The balanced neutral maps to D65 white: XYZ->cam * D65 = 1 / gains.
        let n = mat_vec(&cal.xyz_to_cam, &mat_vec(&SRGB_TO_XYZ, &[1.0, 1.0, 1.0]));
        for (v, g) in n.iter().zip(cal.gains) {
            assert!((v * g - 1.0).abs() < 1e-5, "{n:?}");
        }
    }

    #[test]
    fn illuminant_matrices() {
        let cam_to_xyz = [0.4, 0.4, 0.2, 0.2, 0.7, 0.1, 0.0, 0.1, 1.0];
        let b = Block::default()
            .property("WhiteBalanceIlluminants", &[("Auto", "CamToXYZ"), ("Sunlight", "CamToXYZ")])
            .property("WhiteBalanceCorrections", &[("Auto", "Identity"), ("Sunlight", "SunCorr")])
            .matrix("CamToXYZ", &[3, 3], &cam_to_xyz)
            .matrix("Identity", &[3, 3], &IDENTITY)
            .matrix("SunCorr", &[3, 3], &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.5])
            .matrix("AutoRGBNeutral", &[3], &[1.0, 0.5, 0.25]);
        let mut c = Camf::parse(b.0);
        let auto = calibration(&mut c, "Auto").unwrap();
        assert_eq!(auto.gains, [1.0, 2.0, 4.0]);
        // Without a stored neutral the camera response to D65 is used.
        // "Daylight" names the "Sunlight" preset.
        let sun = calibration(&mut c, "Daylight").unwrap();
        let m: [f64; 9] = std::array::from_fn(|i| cam_to_xyz[i] as f64);
        let corr = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.5];
        let neutral = mat_vec(&inv3(&mul3(&corr, &m)).unwrap(), &D65_XYZ);
        for c in 0..3 {
            assert!((sun.gains[c] * neutral[c] - 1.0).abs() < 1e-9);
        }
        assert_eq!(sun.color_matrix().unwrap().illuminant, Illuminant(1));
        assert!(calibration(&mut Camf::default(), "Auto").is_none());
    }

    #[test]
    fn lookups_stop_at_names_missing_from_the_include_list() {
        let b = Block::default()
            .matrix("TempGainFact", &[3], &[1.0, 1.0, 1.0])
            .property("IncludeBlocks", &[("IncludeBlocks", ""), ("TempGainFact", ""), ("Late", "")])
            .matrix("Late", &[1], &[5.0])
            .matrix("Unlisted", &[1], &[6.0]);
        let mut c = Camf::parse(b.0);
        assert!(c.matrix("Missing").is_none());
        // Parsing stopped at the list: the entries after it were not visited.
        assert_eq!(c.entries.len(), 2);
        assert_eq!(c.values("Late", 1), Some(vec![5.0]));
        assert_eq!(c.entries.len(), 3);
        // Without the list every entry is searched; with it, unlisted names are absent.
        assert!(c.matrix("Unlisted").is_none());
        let mut c = Camf::parse(Block::default().matrix("A", &[1], &[1.0]).matrix("B", &[1], &[2.0]).0);
        assert_eq!(c.values("B", 1), Some(vec![2.0]));
    }

    #[test]
    fn bracketing() {
        let pts = [2.8, 4.0, 8.0];
        assert_eq!(bracket(&pts, 2.0, |x| x), vec![(0, 1.0)]);
        assert_eq!(bracket(&pts, 11.0, |x| x), vec![(2, 1.0)]);
        let b = bracket(&pts, 5.656854, |x: f64| x.log2());
        assert_eq!((b[0].0, b[1].0), (1, 2));
        assert!((b[0].1 - 0.5).abs() < 1e-6 && (b[1].1 - 0.5).abs() < 1e-6);
        assert!(bracket(&[], 1.0, |x| x).is_empty());
    }

    #[test]
    fn classic_spatial_gain_is_turned() {
        // 2 x 3 points, 3 layers; the stored grid is turned by 180 degrees.
        let v: Vec<f32> = (0..18).map(|i| 1.0 + i as f32 / 100.0).collect();
        let mut c = Camf::parse(Block::default().matrix("SpatialGain", &[2, 3, 3], &v).0);
        let g = shading(&mut c, "Auto", 100, 50).unwrap();
        assert_eq!((g[0].rows, g[0].cols), (2, 3));
        let layer0: Vec<f32> = v.iter().step_by(3).rev().copied().collect();
        assert_eq!(g[0].gains, layer0);
        // TRUE I bodies: the preset's table wins.
        let w: Vec<f32> = vec![1.5; 18];
        let b = Block::default()
            .property("SpatialGainTables", &[("Sunlight", "SpatialGain_Sunlight")])
            .matrix("SpatialGain_Sunlight", &[2, 3, 3], &w)
            .matrix("SpatialGain", &[2, 3, 3], &v);
        let g = shading(&mut Camf::parse(b.0), "Sunlight", 100, 50).unwrap();
        assert!(g.iter().all(|g| g.gains.iter().all(|&x| x == 1.5)));
        // Implausible gains are rejected.
        let mut c = Camf::parse(Block::default().matrix("SpatialGain", &[2, 3, 3], &[0.0; 18]).0);
        assert!(shading(&mut c, "Auto", 100, 50).is_none());
    }

    fn coded(b: Block, name: &str, min: &str, table: &[f32]) -> Block {
        let t = format!("T_{name}");
        b.property(
            name,
            &[
                ("MinGainsR", min),
                ("DeltaR", "0.01"),
                ("GainsTableR", t.as_str()),
                ("MinGainsG", min),
                ("DeltaG", "0.01"),
                ("GainsTableG", t.as_str()),
                ("MinGainsB", min),
                ("DeltaB", "0.01"),
                ("GainsTableB", t.as_str()),
            ],
        )
        .matrix(&t, &[2, 2], table)
    }

    #[test]
    fn merrill_spatial_gain() {
        let b = Block::default().matrix("SpatialGain_Fstop", &[3], &[2.8, 4.0, 8.0]).matrix(
            "CaptureAperture",
            &[1],
            &[5.656854],
        );
        let b = coded(b, "SpatialGainsProps_1_INF", "1", &[0.0, 0.0, 0.0, 10.0]);
        let b = coded(b, "SpatialGainsProps_2_INF", "1", &[20.0, 0.0, 0.0, 10.0]);
        // Closest-focus tables are not used.
        let b = coded(b, "SpatialGainsProps_2_MOD", "5", &[0.0; 4]);
        let g = shading(&mut Camf::parse(b.0), "Auto", 10, 10).unwrap();
        // Half way (in stops) between f/4 and f/8, turned by 180 degrees.
        for layer in &g {
            let want = [1.1, 1.0, 1.0, 1.1];
            assert!(layer.gains.iter().zip(want).all(|(a, b)| (*a - b).abs() < 1e-5), "{:?}", layer.gains);
        }
    }

    #[test]
    fn sd1_spatial_gain_by_focal_length() {
        // The include list names every entry (lookups stop at names it does not list).
        let keys = [
            ("IncludeBlocks", ""),
            ("CaptureAperture", ""),
            ("FocalLength", ""),
            ("SpatialGainsProps_2.8_30", ""),
            ("T_SpatialGainsProps_2.8_30", ""),
            ("SpatialGainsProps_2.8_50", ""),
            ("T_SpatialGainsProps_2.8_50", ""),
        ];
        let base = || {
            let b = Block::default().property("IncludeBlocks", &keys).matrix("CaptureAperture", &[1], &[2.8]);
            let b = coded(b, "SpatialGainsProps_2.8_30", "1", &[10.0; 4]);
            coded(b, "SpatialGainsProps_2.8_50", "1", &[30.0; 4])
        };
        let g = shading(&mut Camf::parse(base().matrix("FocalLength", &[1], &[40.0]).0), "Auto", 10, 10).unwrap();
        assert!(g[1].gains.iter().all(|v| (v - 1.2).abs() < 1e-5));
        // Outside the calibrated focal lengths (another lens): none.
        assert!(shading(&mut Camf::parse(base().matrix("FocalLength", &[1], &[105.0]).0), "Auto", 10, 10).is_none());
    }

    #[test]
    fn merrill_colour_shading() {
        let b = Block::default().property("WhiteBalanceColorShadingFactor", &[("Auto", "AutoCSF")]).matrix(
            "AutoCSF",
            &[2, 2],
            &[0.1, 0.2, -0.1, 0.0],
        );
        let g = shading(&mut Camf::parse(b.0), "Auto", 1000, 600).unwrap();
        // Corners: r = 1 (bottom layer x1.3, top layer x0.9); the middle layer is untouched.
        let (r, m, t) = (&g[0], &g[1], &g[2]);
        assert!((r.gains[0] - 1.3).abs() < 1e-3 && (t.gains[0] - 0.9).abs() < 1e-3);
        assert!(m.gains.iter().all(|&v| v == 1.0));
        let centre = r.gains[(r.rows / 2) * r.cols + r.cols / 2];
        assert!((centre - 1.0).abs() < 0.01, "{centre}");
    }

    #[test]
    fn quattro_preset_index() {
        let b = Block::default().matrix("WhiteBalance", &[1], &[7.0]);
        assert_eq!(Camf::parse(b.0).white_balance_name(), Some("Flash"));
        assert_eq!(Camf::default().white_balance_name(), None);
    }
}
