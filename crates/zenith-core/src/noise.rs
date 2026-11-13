//! Perlin gradient noise and fractal sums of it, with `f64` arithmetic only.
use crate::rng::Rng;

#[derive(Clone)]
pub struct Perlin {
    perm: Box<[u8; 512]>,
    ox: f64,
    oy: f64,
    oz: f64,
}

#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

#[inline]
fn grad3(hash: u8, x: f64, y: f64, z: f64) -> f64 {
    let h = hash & 15;
    let u = if h < 8 { x } else { y };
    let v = if h < 4 {
        y
    } else if h == 12 || h == 14 {
        x
    } else {
        z
    };
    (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
}

#[inline]
fn grad2(hash: u8, x: f64, y: f64) -> f64 {
    // Eight directions: the axes, and diagonals scaled to the same length.
    const D: f64 = std::f64::consts::FRAC_1_SQRT_2;
    match hash & 7 {
        0 => x,
        1 => -x,
        2 => y,
        3 => -y,
        4 => (x + y) * D,
        5 => (x - y) * D,
        6 => (-x + y) * D,
        _ => (-x - y) * D,
    }
}

impl Perlin {
    pub fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let mut table = [0u8; 256];
        for (i, v) in table.iter_mut().enumerate() {
            *v = i as u8;
        }
        for i in (1..256usize).rev() {
            let j = rng.below(i as u32 + 1) as usize;
            table.swap(i, j);
        }
        let mut perm = Box::new([0u8; 512]);
        for i in 0..512 {
            perm[i] = table[i & 255];
        }
        // An offset keeps every lattice from having a zero at the world origin.
        Self { perm, ox: rng.f64() * 256.0, oy: rng.f64() * 256.0, oz: rng.f64() * 256.0 }
    }

    /// About `[-1, 1]`.
    #[inline]
    pub fn get2(&self, x: f64, z: f64) -> f64 {
        let (x, z) = (x + self.ox, z + self.oz);
        let (fx, fz) = (x.floor(), z.floor());
        let (xi, zi) = ((fx as i64 & 255) as usize, (fz as i64 & 255) as usize);
        let (dx, dz) = (x - fx, z - fz);
        let (u, v) = (fade(dx), fade(dz));
        let p = &self.perm;
        let (a, b) = (p[xi] as usize + zi, p[xi + 1] as usize + zi);
        lerp(
            v,
            lerp(u, grad2(p[a], dx, dz), grad2(p[b], dx - 1.0, dz)),
            lerp(u, grad2(p[a + 1], dx, dz - 1.0), grad2(p[b + 1], dx - 1.0, dz - 1.0)),
        ) * std::f64::consts::SQRT_2
    }

    /// About `[-1, 1]`.
    #[inline]
    pub fn get3(&self, x: f64, y: f64, z: f64) -> f64 {
        let (x, y, z) = (x + self.ox, y + self.oy, z + self.oz);
        let (fx, fy, fz) = (x.floor(), y.floor(), z.floor());
        let (xi, yi, zi) = ((fx as i64 & 255) as usize, (fy as i64 & 255) as usize, (fz as i64 & 255) as usize);
        let (dx, dy, dz) = (x - fx, y - fy, z - fz);
        let (u, v, w) = (fade(dx), fade(dy), fade(dz));
        let p = &self.perm;
        let a = p[xi] as usize + yi;
        let (aa, ab) = (p[a] as usize + zi, p[a + 1] as usize + zi);
        let b = p[xi + 1] as usize + yi;
        let (ba, bb) = (p[b] as usize + zi, p[b + 1] as usize + zi);
        lerp(
            w,
            lerp(
                v,
                lerp(u, grad3(p[aa], dx, dy, dz), grad3(p[ba], dx - 1.0, dy, dz)),
                lerp(u, grad3(p[ab], dx, dy - 1.0, dz), grad3(p[bb], dx - 1.0, dy - 1.0, dz)),
            ),
            lerp(
                v,
                lerp(u, grad3(p[aa + 1], dx, dy, dz - 1.0), grad3(p[ba + 1], dx - 1.0, dy, dz - 1.0)),
                lerp(u, grad3(p[ab + 1], dx, dy - 1.0, dz - 1.0), grad3(p[bb + 1], dx - 1.0, dy - 1.0, dz - 1.0)),
            ),
        )
    }
}

/// A sum of octaves, each twice the frequency and half the amplitude of the last, scaled to about
/// `[-1, 1]` (the extremes are rare: most values sit within +-0.5).
#[derive(Clone)]
pub struct Fbm {
    octaves: Vec<Perlin>,
    norm: f64,
}

impl Fbm {
    pub fn new(seed: u64, octaves: u32) -> Self {
        let mut rng = Rng::new(seed);
        let mut norm = 0.0;
        let mut amp = 1.0;
        let list = (0..octaves)
            .map(|_| {
                norm += amp;
                amp *= 0.5;
                Perlin::new(rng.next_u64())
            })
            .collect();
        Self { octaves: list, norm: 1.0 / norm }
    }
    #[inline]
    pub fn get2(&self, x: f64, z: f64) -> f64 {
        let (mut sum, mut amp, mut freq) = (0.0, 1.0, 1.0);
        for o in &self.octaves {
            sum += o.get2(x * freq, z * freq) * amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum * self.norm
    }
    #[inline]
    pub fn get3(&self, x: f64, y: f64, z: f64) -> f64 {
        let (mut sum, mut amp, mut freq) = (0.0, 1.0, 1.0);
        for o in &self.octaves {
            sum += o.get3(x * freq, y * freq, z * freq) * amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum * self.norm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_gives_the_same_field_and_another_seed_differs() {
        let (a, b, c) = (Perlin::new(1), Perlin::new(1), Perlin::new(2));
        let mut differs = false;
        for i in 0..200 {
            let (x, z) = (i as f64 * 0.37 - 30.0, i as f64 * 0.11);
            assert_eq!(a.get2(x, z).to_bits(), b.get2(x, z).to_bits());
            assert_eq!(a.get3(x, z, x).to_bits(), b.get3(x, z, x).to_bits());
            differs |= a.get2(x, z) != c.get2(x, z);
        }
        assert!(differs);
    }

    #[test]
    fn values_stay_in_range_and_spread_out() {
        let n = Perlin::new(9);
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for i in 0..20_000 {
            let (x, z) = (i as f64 * 0.173, i as f64 * 0.311);
            for v in [n.get2(x, z), n.get3(x, z, z - x)] {
                assert!(v.abs() <= 1.05, "{v}");
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        assert!(lo < -0.6 && hi > 0.6, "only reached {lo}..{hi}");
    }

    #[test]
    fn the_field_is_continuous_across_lattice_cells() {
        let n = Fbm::new(3, 4);
        let e = 1e-5;
        for i in 0..2000 {
            let (x, z) = (i as f64 * 0.5, i as f64 * 0.25);
            assert!((n.get2(x, z) - n.get2(x + e, z)).abs() < 1e-3);
        }
    }
}
