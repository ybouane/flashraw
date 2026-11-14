//! Canon CRX, the raw codec of CR3 files.
//!
//! A sample holds the sensor image as four colour planes (or one 8-bit plane), cut into tiles. Every
//! (tile, plane) pair is coded independently: either as one entropy-coded plane (lossless) or as a
//! 5/3 integer wavelet whose subbands are entropy coded separately and quantised (C-RAW).
//!
//! * [`header`]: the `CMP1` codec header.
//! * `layout`: tile grid, per-sample header records and subband sizes.
//! * `prim`, `lines`: Golomb-Rice code words, runs and the three line coders; `quant`: dequantisation.
//! * `synth`: inverse wavelet; `unit`, `emit`: per-unit decoding and CFA interleaving.

#[allow(dead_code)]
mod bits;
mod emit;
mod header;
mod layout;
mod lines;
mod prim;
mod quant;
mod synth;
mod unit;

pub use header::CrxHeader;

use flashraw_core::{Error, Result, par};

/// Decode a CRX media sample into `out`, a `header.width x header.height` buffer.
///
/// `data` starts at the sample (its per-sample header) and may extend to the end of the file. `out` is only
/// modified when the whole sample decodes.
pub fn decode(h: &CrxHeader, data: &[u8], out: &mut [u16], parallel: bool) -> Result<()> {
    if (out.len() as u64) < h.width as u64 * h.height as u64 {
        return Err(Error::InvalidArgument("CRX: output buffer too small".into()));
    }
    let layout = layout::parse(h, data)?;

    // Quantisation maps of version 0x200 tiles.
    let mut tables: Vec<Option<Vec<quant::StepTable>>> = Vec::with_capacity(layout.tiles.len());
    for tile in &layout.tiles {
        tables.push(match tile.map {
            Some((start, len)) if h.version == 0x200 => {
                if start > data.len() as u64 {
                    return Err(Error::corrupt("CRX: quantisation map beyond end of file"));
                }
                let end = start.saturating_add(len).min(data.len() as u64);
                Some(quant::step_tables(
                    &data[start as usize..end as usize],
                    tile.width,
                    tile.height,
                    h.levels as usize,
                )?)
            }
            _ => None,
        });
    }

    let planes = layout.plane_count;
    let jobs: Vec<usize> = (0..layout.tiles.len() * planes).collect();
    let results = par::map(jobs, parallel, |job| {
        let (t, p) = (job / planes, job % planes);
        let tile = &layout.tiles[t];
        unit::decode_unit(h, data, tile, &tile.planes[p], tables[t].as_deref())
    });
    let units = results.into_iter().collect::<Result<Vec<_>>>()?;
    emit::interleave(h, &layout, &units, out, parallel);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> CrxHeader {
        CrxHeader {
            version: 0x100,
            width: 128,
            height: 64,
            tile_width: 128,
            tile_height: 64,
            bits: 14,
            planes: 4,
            cfa_layout: 0,
            enc_type: 0,
            levels: 0,
            mdat_header_size: 112,
            median_bits: 14,
        }
    }

    #[test]
    fn garbage_never_panics_and_always_fails() {
        let h = header();
        let mut seed = 77u32;
        for len in [0usize, 10, 112, 200, 4096] {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 24) as u8
                })
                .collect();
            let mut out = vec![0x1234u16; 128 * 64];
            assert!(decode(&h, &data, &mut out, false).is_err(), "len {len}");
            assert!(out.iter().all(|&v| v == 0x1234), "failed decode wrote to the output (len {len})");
        }
    }

    #[test]
    fn short_output_is_rejected_first() {
        let mut out = vec![0u16; 128 * 64 - 1];
        match decode(&header(), &[], &mut out, false) {
            Err(Error::InvalidArgument(_)) => {}
            other => panic!("{other:?}"),
        }
    }
}
