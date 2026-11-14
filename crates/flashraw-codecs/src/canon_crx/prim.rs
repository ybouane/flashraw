//! Small pieces shared by the entropy decoders: zig-zag mapping, Rice-parameter adaptation, the
//! median-edge predictor and the adaptive run-length code.

use super::bits::CrxBits;
use flashraw_core::{Error, Result};

/// Number of raw bits that finish a run in each run state; the run grows by `1 << exponent`
/// per continuation bit. States come in groups: four states each of exponents 0 to 3, two
/// each of 4 to 7, then one state for every exponent 8 to 15.
const RUN_EXPONENT: [u8; 32] = {
    let mut table = [0u8; 32];
    let mut state = 0;
    let mut exponent = 0u8;
    while state < 32 {
        let width = if exponent < 4 {
            4
        } else if exponent < 8 {
            2
        } else {
            1
        };
        let mut i = 0;
        while i < width {
            table[state] = exponent;
            state += 1;
            i += 1;
        }
        exponent += 1;
    }
    table
};

/// Prefix length from which a sample code is followed by a fixed-width value, and that width.
pub(super) const SAMPLE_ESCAPE: (u32, u32) = (41, 21);
/// The same for quantisation-parameter codes.
pub(super) const QP_ESCAPE: (u32, u32) = (23, 8);

/// Upper bound of the Rice parameter of sample codes.
pub(super) const K_MAX: u32 = 15;

/// Signed value of a zig-zag coded unsigned one: 0, 1, 2, 3, 4 become 0, -1, 1, -2, 2.
#[inline(always)]
pub(super) fn unzigzag(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

/// Rice parameter after coding `c` with parameter `k`, without an upper bound.
#[inline(always)]
pub(super) fn adapt_free(k: u32, c: u32) -> u32 {
    let q = c >> k;
    let down = (c < ((1u32 << k) >> 1)) as u32;
    k + (q > 2) as u32 + (q > 5) as u32 - down
}

/// Rice parameter after coding `c` with parameter `k`, bounded by `cap`.
#[inline(always)]
pub(super) fn adapt(k: u32, c: u32, cap: u32) -> u32 {
    adapt_free(k, c).min(cap)
}

/// Median-edge predictor of the LOCO-I family, in the wrapping `i32` arithmetic of the format.
#[inline(always)]
pub(super) fn predict(left: i32, top: i32, top_left: i32) -> i32 {
    let d = top.wrapping_sub(top_left);
    let neg = d < 0;
    let gradient = left.wrapping_add(d);
    let edge = if (left < top) != neg { top } else { left };
    if (top_left < left) != neg { edge } else { gradient }
}

/// Run-length decoder state of one band.
#[derive(Clone, Copy, Default)]
pub(super) struct RunState(usize);

impl RunState {
    /// Read a run length in `1..=left` (`left >= 2`).
    ///
    /// Every continuation bit lengthens the run by the growth of the current state and moves
    /// to the next state (the last state repeats). The run is cut at `left`; the state moves
    /// on for the cutting bit only when it lands exactly on `left`. A run that ended before
    /// `left` is finished by the state's raw bits, and the state then steps back.
    #[inline]
    pub(super) fn read(&mut self, bits: &mut CrxBits, left: usize) -> Result<usize> {
        let mut state = self.0;
        // How much longer than one the run may still become.
        let mut room = left - 1;
        while room > 0 && bits.bit() {
            let growth = 1usize << RUN_EXPONENT[state];
            if growth >= room {
                if growth == room {
                    state = (state + 1).min(31);
                }
                room = 0;
                break;
            }
            room -= growth;
            state = (state + 1).min(31);
        }
        let mut n = left - room;
        if room > 0 {
            n += match RUN_EXPONENT[state] {
                0 => 0,
                e => bits.get(u32::from(e)) as usize,
            };
            state = state.saturating_sub(1);
        }
        self.0 = state;
        if n > left {
            return Err(Error::corrupt("CRX: run past end of line"));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_order() {
        assert_eq!([0u32, 1, 2, 3, 4, 5].map(unzigzag), [0, -1, 1, -2, 2, -3]);
        assert_eq!(unzigzag(u32::MAX), i32::MIN);
    }

    #[test]
    fn adaptation_moves_by_at_most_two() {
        assert_eq!(adapt(0, 0, 15), 0);
        assert_eq!(adapt(0, 3, 15), 1);
        assert_eq!(adapt(0, 6, 15), 2);
        assert_eq!(adapt(4, 0, 15), 3);
        assert_eq!(adapt(4, 8, 15), 4);
        assert_eq!(adapt(15, 1 << 20, 15), 15);
        assert_eq!(adapt_free(15, 1 << 20), 17);
    }

    #[test]
    fn predictor_is_the_median_edge_detector() {
        for (l, t, tl) in [(3, 5, 4), (5, 3, 4), (10, 2, 20), (2, 10, 1), (7, 7, 7), (0, 100, 50), (100, 0, 50)] {
            let expect = if tl >= l.max(t) {
                l.min(t)
            } else if tl <= l.min(t) {
                l.max(t)
            } else {
                l + t - tl
            };
            assert_eq!(predict(l, t, tl), expect, "{l} {t} {tl}");
        }
    }

    #[test]
    fn run_lengths() {
        // flag bits: "0" ends at once with n = 1 and, since 1 < left, state 0 has no tail bits.
        let data = [0u8; 16];
        let mut b = CrxBits::new(&data);
        let mut r = RunState::default();
        assert_eq!(r.read(&mut b, 10).unwrap(), 1);
        // three continuation bits: n = 1 + 1 + 1 + 1 = 4, state 3, then stop bit, state 3 has no tail.
        let data = [0b1110_0000u8, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut b = CrxBits::new(&data);
        let mut r = RunState::default();
        assert_eq!(r.read(&mut b, 10).unwrap(), 4);
        // a run is cut at the line end
        let data = [0xffu8; 16];
        let mut b = CrxBits::new(&data);
        let mut r = RunState::default();
        assert_eq!(r.read(&mut b, 3).unwrap(), 3);
    }

    const OLD_GROWTH: [u32; 32] = [
        1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 8, 8, 8, 8, 16, 16, 32, 32, 64, 64, 128, 128, 256, 512, 1024, 2048, 4096,
        8192, 16384, 32768,
    ];
    const OLD_TAIL: [u32; 32] =
        [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    fn old_read(state: &mut usize, bits: &mut CrxBits, left: usize) -> Result<usize> {
        let mut s = *state;
        let mut n = 1usize;
        while bits.bit() {
            n += OLD_GROWTH[s] as usize;
            if n > left {
                n = left;
                break;
            }
            if s < 31 {
                s += 1;
            }
            if n == left {
                break;
            }
        }
        if n < left {
            let e = OLD_TAIL[s];
            if e != 0 {
                n += bits.get(e) as usize;
            }
            s = s.saturating_sub(1);
        }
        *state = s;
        if n > left {
            return Err(Error::corrupt("CRX: run past end of line"));
        }
        Ok(n)
    }

    #[test]
    fn exponent_table_matches_the_previous_growth_and_tail_tables() {
        for s in 0..32 {
            assert_eq!(1u32 << RUN_EXPONENT[s], OLD_GROWTH[s]);
            assert_eq!(u32::from(RUN_EXPONENT[s]), OLD_TAIL[s]);
        }
    }

    #[test]
    fn run_reader_matches_the_previous_one_on_random_streams() {
        let mut x = 0x1234_5678_9abc_def1u64;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for case in 0..1_500_000u32 {
            let len = 4 + (rnd() % 40) as usize;
            let mut data: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
            match case % 3 {
                0 => data.iter_mut().for_each(|b| *b |= 0xf0),
                1 => data.iter_mut().for_each(|b| *b |= (rnd() as u8) & 0xcc),
                _ => {}
            }
            let (mut a, mut b) = (CrxBits::new(&data), CrxBits::new(&data));
            let mut sa = (rnd() % 32) as usize;
            let mut rs = RunState(sa);
            for _ in 0..4 {
                let left = match rnd() % 3 {
                    0 => 2 + (rnd() % 6) as usize,
                    1 => 2 + (rnd() % 300) as usize,
                    _ => 2 + (rnd() % 70_000) as usize,
                };
                let (ra, rb) = (old_read(&mut sa, &mut a, left), rs.read(&mut b, left));
                assert_eq!(ra.is_ok(), rb.is_ok());
                if let (Ok(x), Ok(y)) = (&ra, &rb) {
                    assert_eq!(x, y);
                }
                assert_eq!(sa, rs.0);
                assert_eq!(a.get(24), b.get(24));
                assert_eq!(a.exhausted(), b.exhausted());
                if ra.is_err() {
                    break;
                }
            }
        }
    }
}
