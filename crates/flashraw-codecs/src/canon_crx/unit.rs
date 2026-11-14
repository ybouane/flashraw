//! Decoding of one unit: the samples of one plane of one tile.

use super::header::CrxHeader;
use super::layout::{Band, Plane, Tile};
use super::lines::{BandReader, Dequant, Kind};
use super::quant::{MapScale, StepTable};
use super::synth::{Edges, Level, next_row};
use flashraw_core::{Error, Result};

/// How decoded values become `u16` samples.
#[derive(Clone, Copy)]
pub(super) enum Convert {
    /// Offset by the median and clamped to the sample range.
    Unsigned { median: i32, max: i32 },
    /// Two's complement, clamped to the signed range of the bit depth.
    Signed { median: i32 },
    /// The low 16 bits (decorrelated planes are combined later).
    Low16,
}

impl Convert {
    pub(super) fn for_header(h: &CrxHeader) -> Convert {
        let median = 1i32 << (h.bits.saturating_sub(1) & 31);
        match h.enc_type {
            1 => Convert::Signed { median },
            3 if h.planes == 4 => Convert::Low16,
            _ => Convert::Unsigned { median, max: ((1i64 << h.bits.min(31)) - 1) as i32 },
        }
    }

    #[inline]
    pub(super) fn row(self, src: &[i32], dst: &mut [u16]) {
        match self {
            Convert::Unsigned { median, max } => {
                for (d, &v) in dst.iter_mut().zip(src) {
                    *d = median.wrapping_add(v).clamp(0, max) as u16;
                }
            }
            Convert::Signed { median } => {
                for (d, &v) in dst.iter_mut().zip(src) {
                    *d = v.clamp(-median, median - 1) as i16 as u16;
                }
            }
            Convert::Low16 => {
                for (d, &v) in dst.iter_mut().zip(src) {
                    *d = v as i16 as u16;
                }
            }
        }
    }
}

/// The bytes of a band, checked against the data that is actually there.
fn stream_of<'a>(data: &'a [u8], band: &Band) -> Result<&'a [u8]> {
    if band.len == 0 {
        return Ok(&[]);
    }
    match band.start.checked_add(band.len) {
        Some(end) if end <= data.len() as u64 => Ok(&data[band.start as usize..end as usize]),
        _ => Err(Error::corrupt("CRX: subband data beyond end of file")),
    }
}

struct UnitSource<'a> {
    data: &'a [u8],
    plane: &'a Plane,
    tables: Option<&'a [StepTable]>,
    lossy: bool,
}

impl<'a> UnitSource<'a> {
    fn reader(&self, index: usize) -> Result<BandReader<'a>> {
        let band = &self.plane.bands[index];
        let kind = match (index, self.plane.predictive) {
            (0, true) if self.plane.rounded_bits != 0 => Kind::Near(self.plane.rounded_bits),
            (0, true) => Kind::Predictive,
            _ => Kind::Detail,
        };
        let dequant = match (self.lossy, self.tables) {
            (false, _) => Dequant::None,
            (true, Some(tables)) => Dequant::Mapped(MapScale {
                table: &tables[if index == 0 { 0 } else { (index - 1) / 3 }],
                margins: band.margins,
                mult: band.step_mult,
                base: band.step_base,
                band_width: band.width,
                band_height: band.height,
            }),
            (true, None) => Dequant::Uniform { q: band.q, deltas: band.q_per_line.then_some(0) },
        };
        Ok(BandReader::new(stream_of(self.data, band)?, band.width, band.height, kind, dequant))
    }
}

/// Decode plane `plane` of `tile` into a `tile.width x tile.height` buffer.
pub(super) fn decode_unit(
    h: &CrxHeader,
    data: &[u8],
    tile: &Tile,
    plane: &Plane,
    tables: Option<&[StepTable]>,
) -> Result<Vec<u16>> {
    let (w, ht) = (tile.width, tile.height);
    let mut out = vec![0u16; w * ht];
    let convert = Convert::for_header(h);
    let levels = h.levels as usize;
    let src = UnitSource { data, plane, tables, lossy: levels > 0 };

    if levels == 0 {
        if plane.bands[0].len == 0 {
            return Ok(out);
        }
        let mut band = src.reader(0)?;
        for row in out.chunks_exact_mut(w) {
            convert.row(band.next_unscaled()?, row);
        }
        return Ok(out);
    }

    let mut ll = src.reader(0)?;
    let mut pyramid: Vec<Level> = Vec::with_capacity(levels);
    let mut source_width = plane.bands[0].width;
    let edges =
        Edges { left: tile.sides.left, right: tile.sides.right, top: tile.sides.top, bottom: tile.sides.bottom };
    for l in 0..levels {
        let b = &plane.bands[3 * l + 1..3 * l + 4];
        let (lw, lh) = if l + 1 < levels {
            (plane.bands[3 * (l + 1) + 2].width, plane.bands[3 * (l + 1) + 1].height)
        } else {
            (w, ht)
        };
        let level = Level::new(
            edges,
            lw,
            lh,
            source_width,
            src.reader(3 * l + 1)?,
            src.reader(3 * l + 2)?,
            src.reader(3 * l + 3)?,
            [b[0].width, b[1].width, b[2].width],
        )?;
        pyramid.push(level);
        source_width = lw;
    }
    let mut row = vec![0i32; w];
    for out_row in out.chunks_exact_mut(w) {
        next_row(&mut ll, &mut pyramid, &mut row)?;
        convert.row(&row, out_row);
    }
    Ok(out)
}
