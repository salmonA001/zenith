//! xoshiro256++ seeded through SplitMix64. Simulation and generation use only this.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rng {
    s: [u64; 4],
}

#[inline]
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One-way mix of two words, used to derive independent seeds.
#[inline]
pub fn mix(a: u64, b: u64) -> u64 {
    let mut state = a ^ b.rotate_left(29).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    splitmix(&mut state)
}

/// The seed of one feature kind in one chunk: independent of every other chunk and of the order
/// chunks are generated in.
#[inline]
pub fn chunk_seed(seed: u64, x: i32, z: i32, salt: u64) -> u64 {
    mix(mix(seed ^ salt, x as u32 as u64), z as u32 as u64)
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut state = seed;
        Self { s: [splitmix(&mut state), splitmix(&mut state), splitmix(&mut state), splitmix(&mut state)] }
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[0].wrapping_add(s[3]).rotate_left(23).wrapping_add(s[0]);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }
    /// Uniform in `[0, 1)`.
    #[inline]
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
    /// Uniform in `0..n`; `n` must not be zero.
    #[inline]
    pub fn below(&mut self, n: u32) -> u32 {
        (((self.next_u64() >> 32) * n as u64) >> 32) as u32
    }
    /// Uniform in `lo..=hi`.
    #[inline]
    pub fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u32) as i32
    }
    #[inline]
    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_matches_the_reference_vector() {
        // The published first output of SplitMix64 for seed 1234567.
        let mut state = 1234567u64;
        assert_eq!(splitmix(&mut state), 6457827717110365317);
        assert_eq!(splitmix(&mut state), 3203168211198807973);
    }

    #[test]
    fn range_stays_inside_its_bounds_and_reaches_them() {
        let mut r = Rng::new(7);
        let mut seen = [false; 5];
        for _ in 0..1000 {
            let v = r.range(-2, 2);
            assert!((-2..=2).contains(&v));
            seen[(v + 2) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn neighbouring_chunks_get_unrelated_seeds() {
        let a = chunk_seed(1, 0, 0, 0);
        let b = chunk_seed(1, 1, 0, 0);
        let c = chunk_seed(1, 0, 1, 0);
        assert!((a ^ b).count_ones() > 16 && (a ^ c).count_ones() > 16 && (b ^ c).count_ones() > 16);
    }

    #[test]
    fn f64_stays_in_the_unit_interval() {
        let mut r = Rng::new(3);
        for _ in 0..10_000 {
            let v = r.f64();
            assert!((0.0..1.0).contains(&v));
        }
    }
}
