//! Where the image lies inside the stored buffer, and the colour filter pattern that
//! goes with it. All margins are plain signed integers until they are clamped into a
//! `Rect`.

use flashraw_core::{CfaColor, CfaPattern, Rect, SensorLayout};

/// Everything the active-area rules look at.
pub(super) struct Inputs<'a> {
    /// Normalised model name.
    pub model: &'a str,
    /// Sensor size as the container states it `(width, height)`.
    pub nominal: (i64, i64),
    /// Final stored buffer size `(width, height)`.
    pub stored: (i64, i64),
    /// Recommended image size `(width, height)` of the directory.
    pub image_size: Option<(i64, i64)>,
    pub generation: u32,
    /// Size table of the 0xC000 blob `(width, height)`.
    pub size_table: Option<(i64, i64)>,
    pub crop_mode: u32,
    /// Vendor crop `(left, top, width, height)` exactly as recorded.
    pub vendor_crop: Option<(i64, i64, i64, i64)>,
    pub layout: bool,
    pub wide: bool,
    pub xtrans: bool,
    pub packed: bool,
}

/// Result of the active-area rules.
pub(super) struct Area {
    pub left: i64,
    pub top: i64,
    pub width: i64,
    pub height: i64,
    /// The pattern at the first visible sample is BGGR instead of RGGB.
    pub bggr: bool,
    /// Unpacked samples that span 14 bits whatever depth the file states (the S100FS sample is
    /// stored as 12-bit and clips at 16382, SuperCCD samples reach 15896).
    pub fourteen_bit: bool,
    /// The buffer holds a rotated SuperCCD lattice.
    pub superccd: bool,
    /// Photosite columns of the SuperCCD image.
    pub columns: i64,
}

#[derive(Clone, Copy)]
struct Margins {
    left: i64,
    top: i64,
    width: i64,
    height: i64,
}

/// Centred margins: a quarter of the slack, rounded down to an even number.
fn centred(total: i64, size: i64) -> i64 {
    if total >= size { ((total - size) >> 2) << 1 } else { 0 }
}

fn round_up(v: i64, to: i64) -> i64 {
    (v.max(0) + to - 1) / to * to
}

pub(super) fn active_area(inp: &Inputs) -> Area {
    let (rw, rh) = inp.nominal;
    let model = inp.model;
    let mut bggr = false;
    let mut fourteen_bit = false;

    let (mut width, mut height) = inp.image_size.unwrap_or((0, 0));
    if inp.generation == 0 && inp.layout {
        height *= 2;
        width >>= 1;
    }
    if height == 0 {
        height = rh;
    }
    if width == 0 {
        width = rw;
    }

    // The vendor crop counts as an inset only when it fits inside the sensor.
    let inset = inp.vendor_crop.filter(|&(_, _, w, h)| w > 0 && w <= rw && h > 0 && h <= rh);

    if inp.generation == 0 && rw == 2944 {
        height = 2144;
        width = 2880;
    } else if !inp.packed && !model.starts_with("X-") && !inp.xtrans {
        fourteen_bit = true;
    }

    let modern = inp.generation != 0 && inp.generation != 4096;
    match inp.crop_mode {
        0 if modern && inp.size_table.is_some() => (width, height) = inp.size_table.unwrap_or_default(),
        1 => (width, height) = (rw, rh),
        4 => height = rh,
        _ => {}
    }
    let mut m = Margins { left: centred(rw, width), top: centred(rh, height), width, height };

    let apply_inset = |m: &mut Margins| {
        if let Some((l, t, w, h)) = inset {
            *m = Margins { left: l, top: t, width: w, height: h };
        }
    };

    if modern {
        let mode = inp.crop_mode;
        match rw {
            2944 => bggr = true,
            4096 | 5120 | 6048 | 6160 => m.left = 0,
            4992 => m.left = 4,
            6912 | 9216 => (m.left, m.top) = (0, 0),
            8472 => {
                (m.left, m.top) = (0, 0);
                m.width = rw - 192;
            }
            9696 | 11808 => {
                m.left = 0;
                m.top = 2;
                m.width = rw - 146;
                m.height = rh - 8;
            }
            7872 if mode == 0 => m = Margins { left: 0, top: 6, width: 7752, height: 5178 },
            7872 => apply_inset(&mut m),
            6336 if mode == 0 => {
                let wide = if model.eq_ignore_ascii_case("X-S20") { 6252 } else { 6264 };
                m = Margins { left: 0, top: 6, width: wide, height: 4176 };
            }
            6336 if mode == 2 || mode == 4 => m = Margins { left: 630, top: 0, width: 5004, height: 3348 },
            6336 => apply_inset(&mut m),
            6384 => {
                m.top = 0;
                match mode {
                    0 => m = Margins { left: 0, top: 6, width: 6246, height: 4170 },
                    2 => {
                        m.left = 624;
                        m.width = 5004;
                        m.height = rh;
                    }
                    4 => {
                        m.left = 624;
                        m.width = 5004;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    } else if inp.generation == 0 {
        match rw {
            2304 => {
                m.top = 6;
                m.height -= 6;
            }
            3328 => {
                m.width = rw - 66;
                m.left = 34;
                if inset.is_some_and(|(l, ..)| l == 8) {
                    m.left = 0;
                    m.width += 2;
                    bggr = true;
                }
            }
            3664 => bggr = true,
            5504 => {
                m = Margins { left: 32, top: 8, width: rw - 64, height: rh - 16 };
                bggr = true;
            }
            _ if model.starts_with("S6000") || model.starts_with("S6500") => {}
            _ => apply_inset(&mut m),
        }
    }

    // Margins snap to the repeat period of the pattern; the image loses what is added.
    let align = if inp.xtrans {
        6
    } else if inp.wide {
        1
    } else {
        2
    };
    if align > 1 {
        let (top, left) = (round_up(m.top, align), round_up(m.left, align));
        m.height -= top - m.top.max(0);
        m.width -= left - m.left.max(0);
        m.top = top;
        m.left = left;
    }

    let superccd = inp.wide && inp.stored.0 != 5504;
    let mut columns = 0;
    if superccd {
        let shift = if inp.layout { 0 } else { 1 };
        columns = m.width >> shift;
        m.width = columns << shift;
        m.height = inp.stored.1 - 2 * m.top;
    }
    Area { left: m.left, top: m.top, width: m.width, height: m.height, bggr, fourteen_bit, superccd, columns }
}

fn clamp_u32(v: i64) -> u32 {
    v.clamp(0, i64::from(u32::MAX)) as u32
}

impl Area {
    /// The area as a rectangle inside a `width x height` buffer.
    pub fn rect(&self, width: u32, height: u32) -> Rect {
        Rect::new(clamp_u32(self.left), clamp_u32(self.top), clamp_u32(self.width), clamp_u32(self.height))
            .clamp_to(width, height)
    }
}

/// The colour filter pattern of the stored buffer and its layout class.
pub(super) fn pattern(
    xtrans: Option<&[u8; 36]>,
    area: &Area,
    layout: bool,
    visible: Rect,
) -> Option<(CfaPattern, SensorLayout)> {
    if let Some(p) = xtrans {
        let colors: Vec<CfaColor> = p.iter().map(|&c| CfaColor::from_index(c)).collect();
        return CfaPattern::new(6, 6, &colors).map(|c| (c, SensorLayout::XTrans));
    }
    if area.superccd {
        return superccd_pattern(area, layout).map(|c| (c, SensorLayout::OtherCfa));
    }
    let base = if area.bggr { CfaPattern::BGGR } else { CfaPattern::RGGB };
    Some((base.shifted((visible.x % 2) as usize, (visible.y % 2) as usize), SensorLayout::Bayer))
}

/// The 45-degree rotated photosite lattice read line by line: green rows alternate with
/// red/blue rows, 2 wide and 4 tall for layout 1, the transpose for layout 0.
fn superccd_pattern(area: &Area, layout: bool) -> Option<CfaPattern> {
    let (pw, ph) = if layout { (2usize, 4usize) } else { (4, 2) };
    let fw = area.columns;
    let quad = if fw.rem_euclid(2) == 1 { CfaPattern::RGGB } else { CfaPattern::GBRG };
    let mut colors = Vec::with_capacity(pw * ph);
    for y in 0..ph as i64 {
        for x in 0..pw as i64 {
            let row = (y - area.top).rem_euclid(8);
            let col = (x - area.left).rem_euclid(8);
            let (r, c) = if layout {
                (fw - 1 - col + (row >> 1), col + ((row + 1) >> 1))
            } else {
                (fw - 1 + row - (col >> 1), row + ((col + 1) >> 1))
            };
            colors.push(quad.color_at(c.rem_euclid(2) as usize, r.rem_euclid(2) as usize));
        }
    }
    CfaPattern::new(pw, ph, &colors)
}
