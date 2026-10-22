//! Running a clock faster or slower than real time, the way Cheat Engine's speedhack does.
//!
//! A scaled clock is a straight line through an anchor: `scaled = anchor.scaled + (real -
//! anchor.real) * factor`. Changing the factor does not move the line, it starts a new one at
//! the point where the old one is now, so the scaled time never jumps.
//!
//! The factor is fixed point ([`ONE`] is 1.0), so the arithmetic is exact integer arithmetic:
//! at 1.0 a clock reads exactly real time plus the anchor's offset, and every mapping is
//! monotonic in `real`.

/// Bits after the binary point of a fixed point factor.
const SHIFT: u32 = 20;

/// A factor of exactly 1.0.
pub const ONE: u64 = 1 << SHIFT;

/// `factor` as fixed point, rounded to the nearest representable value. Anything not
/// positive and finite is 1.0, because a clock that stops or runs backwards breaks every game.
#[must_use]
pub fn fixed(factor: f64) -> u64 {
    if !factor.is_finite() || factor <= 0.0 {
        return ONE;
    }
    // Bounded above by the setting's range; a factor of 2^43 is not a game speed.
    let scaled = (factor * f64::from(1u32 << SHIFT)).round().min(1e18);
    // Positive, finite and clamped above, so the conversion cannot truncate or change sign.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = scaled as u64;
    value.max(1)
}

/// One straight line from real time to scaled time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// Real clock reading where the line starts.
    pub real: i64,
    /// Scaled clock reading at that point.
    pub scaled: i64,
    /// Slope, fixed point.
    pub factor: u64,
}

impl Anchor {
    /// The identity line through `now`: real and scaled time agree from here on.
    #[must_use]
    pub fn identity(now: i64) -> Self {
        Anchor {
            real: now,
            scaled: now,
            factor: ONE,
        }
    }

    /// Scaled time for a real clock reading.
    ///
    /// A reading before the anchor maps to the anchor itself. That happens to a thread that
    /// read the real clock just before another thread re-anchored, and holding still for that
    /// instant is what keeps the clock monotonic for it.
    #[must_use]
    pub fn map(&self, real: i64) -> i64 {
        let elapsed = real.saturating_sub(self.real).max(0);
        let scaled = (i128::from(elapsed) * i128::from(self.factor)) >> SHIFT;
        self.scaled
            .saturating_add(i64::try_from(scaled).unwrap_or(i64::MAX))
    }

    /// The line for `factor` that continues this one, starting `margin` real ticks after `now`.
    ///
    /// The margin is for readers that are mid-call while the anchor changes: they read the
    /// real clock, then the old anchor, so they can return the old line's value for a reading
    /// up to the moment the new anchor is published. Starting the new line a little in the
    /// future, at the old line's value there, keeps those readings below everything the new
    /// line returns, as long as publishing takes less than the margin. Until `now + margin`
    /// the new line holds still.
    #[must_use]
    pub fn rebase(&self, now: i64, margin: i64, factor: u64) -> Self {
        let at = now.saturating_add(margin.max(0));
        Anchor {
            real: at,
            scaled: self.map(at),
            factor,
        }
    }
}

/// A 32-bit millisecond reading (`timeGetTime`, `GetTickCount`) as a 64-bit value near
/// `reference`, so that one wrap of the counter is not a jump of 49 days.
///
/// Correct while the reading is within 24 days of the reference in either direction.
#[must_use]
pub fn widen_u32(reference: i64, value: u32) -> i64 {
    // Only the low 32 bits take part: this is the reference's position on the same dial.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let low = reference as u32;
    // Reinterpreting the wrapped difference as signed is the point: it is the shortest way
    // round the dial, forwards or backwards.
    #[allow(clippy::cast_possible_wrap)]
    let delta = value.wrapping_sub(low) as i32;
    reference.saturating_add(i64::from(delta))
}

/// The low 32 bits of a scaled 64-bit millisecond count, which is what a 32-bit tick
/// function returns: it wraps, as the real one does.
#[must_use]
pub fn narrow_u32(value: i64) -> u32 {
    // Truncation is the 32-bit counter's own behaviour.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let low = value as u32;
    low
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_is_the_identity() {
        let anchor = Anchor::identity(1_000);
        for real in [1_000, 1_001, 123_456_789, i64::MAX / 2] {
            assert_eq!(anchor.map(real), real);
        }
        assert_eq!(fixed(1.0), ONE);
        // Back at 1.0 after a detour through another speed: the rate is real time again.
        let fast = anchor.rebase(2_000, 0, fixed(2.0));
        let back = fast.rebase(3_000, 0, fixed(1.0));
        assert_eq!(back.map(4_000) - back.map(3_500), 500);
    }

    #[test]
    fn the_factor_scales_elapsed_time() {
        let anchor = Anchor {
            real: 100,
            scaled: 50,
            factor: fixed(2.0),
        };
        assert_eq!(anchor.map(100), 50);
        assert_eq!(anchor.map(110), 70);
        let slow = Anchor {
            factor: fixed(0.5),
            ..anchor
        };
        assert_eq!(slow.map(120), 60);
        // 1.1 is not exact in binary; over a minute of milliseconds it is still within one.
        let slight = Anchor::identity(0).rebase(0, 0, fixed(1.1));
        assert!((slight.map(60_000) - 66_000).abs() <= 1);
    }

    #[test]
    fn rebasing_continues_the_line_without_a_jump() {
        let first = Anchor::identity(0).rebase(1_000, 0, fixed(3.0));
        assert_eq!(first.map(1_000), 1_000);
        let second = first.rebase(2_000, 0, fixed(0.25));
        assert_eq!(second.map(2_000), first.map(2_000));
        assert_eq!(second.map(2_400), first.map(2_000) + 100);
    }

    #[test]
    fn readings_stay_monotonic_across_changes() {
        let factors = [1.0, 4.0, 0.1, 2.5, 0.1, 1.0, 3.9, 0.5];
        let mut anchor = Anchor::identity(0);
        let mut last = i64::MIN;
        let mut real = 0i64;
        for (step, factor) in factors.iter().enumerate() {
            // A reader that read the real clock before the change and saw the old anchor:
            // the highest value it can return is at the moment of publishing.
            let margin = 10;
            let published = real + margin;
            let stale = anchor.map(published);
            anchor = anchor.rebase(real, margin, fixed(*factor));
            assert!(anchor.map(real) >= stale, "step {step}: jumped backwards");
            // And a reader that read the clock earlier still but saw the new anchor.
            assert!(anchor.map(real - 5) >= stale);
            for _ in 0..100 {
                let value = anchor.map(real);
                assert!(value >= last, "step {step}: {value} < {last}");
                last = value;
                real += 7;
            }
        }
    }

    #[test]
    fn readings_before_the_anchor_hold_still() {
        let anchor = Anchor {
            real: 500,
            scaled: 900,
            factor: fixed(2.0),
        };
        assert_eq!(anchor.map(0), 900);
        assert_eq!(anchor.map(499), 900);
    }

    #[test]
    fn nonsense_factors_are_one() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(fixed(bad), ONE);
        }
        assert!(fixed(1e-9) >= 1);
    }

    #[test]
    fn thirty_two_bit_ticks_widen_across_the_wrap() {
        let reference = i64::from(u32::MAX) - 10;
        assert_eq!(widen_u32(reference, u32::MAX - 20), reference - 10);
        assert_eq!(widen_u32(reference, 5), i64::from(u32::MAX) + 6);
        assert_eq!(narrow_u32(i64::from(u32::MAX) + 6), 5);
        let later = (1i64 << 33) + 7;
        assert_eq!(widen_u32(later, 9), later + 2);
    }
}
