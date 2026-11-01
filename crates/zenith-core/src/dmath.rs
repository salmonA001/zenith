//! Sine and cosine computed with nothing but IEEE addition and multiplication, so they give the
//! same bits everywhere. Angles are `u16` turns: 65536 steps make a full circle.
use std::f64::consts::FRAC_PI_2;

/// `(sin, cos)` of `turn / 65536` of a circle.
pub fn sin_cos(turn: u16) -> (f64, f64) {
    let quadrant = turn >> 14;
    let fraction = (turn & 0x3FFF) as f64 / 16384.0;
    // x in [0, pi/2): reflect about pi/4 so the series only ever sees [0, pi/4].
    let (s, c) = if fraction <= 0.5 {
        kernel(fraction * FRAC_PI_2)
    } else {
        let (s, c) = kernel((1.0 - fraction) * FRAC_PI_2);
        (c, s)
    };
    match quadrant {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

/// Taylor series, accurate to about 1e-13 on [0, pi/4].
#[inline]
fn kernel(x: f64) -> (f64, f64) {
    const SIN: [f64; 6] = [-1.0 / 6.0, 1.0 / 120.0, -1.0 / 5040.0, 1.0 / 362_880.0, -1.0 / 39_916_800.0, 1.0 / 6_227_020_800.0];
    const COS: [f64; 7] = [
        -1.0 / 2.0,
        1.0 / 24.0,
        -1.0 / 720.0,
        1.0 / 40_320.0,
        -1.0 / 3_628_800.0,
        1.0 / 479_001_600.0,
        -1.0 / 87_178_291_200.0,
    ];
    let x2 = x * x;
    let s = x * (1.0 + x2 * SIN.iter().rev().fold(0.0, |acc, &k| k + x2 * acc));
    let c = 1.0 + x2 * COS.iter().rev().fold(0.0, |acc, &k| k + x2 * acc);
    (s, c)
}

/// Degrees to turns (wrapping).
pub fn deg_to_turn(deg: f64) -> u16 {
    let t = deg / 360.0;
    let t = t - t.floor();
    ((t * 65536.0) as u32 & 0xFFFF) as u16
}

pub fn turn_to_deg(turn: u16) -> f64 {
    turn as f64 * 360.0 / 65536.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agrees_with_the_platform_sine_across_the_whole_circle() {
        for turn in (0..=u16::MAX).step_by(97) {
            let angle = turn as f64 / 65536.0 * std::f64::consts::TAU;
            let (s, c) = sin_cos(turn);
            assert!((s - angle.sin()).abs() < 1e-12, "sin at {turn}: {s}");
            assert!((c - angle.cos()).abs() < 1e-12, "cos at {turn}: {c}");
        }
    }

    #[test]
    fn cardinal_directions_are_exact() {
        assert_eq!(sin_cos(0), (0.0, 1.0));
        assert_eq!(sin_cos(16384), (1.0, 0.0));
        assert_eq!(sin_cos(32768), (0.0, -1.0));
    }

    #[test]
    fn degrees_wrap_into_turns() {
        assert_eq!(deg_to_turn(90.0), 16384);
        assert_eq!(deg_to_turn(-90.0), 49152);
        assert_eq!(deg_to_turn(450.0), 16384);
    }
}
