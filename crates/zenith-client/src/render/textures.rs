//! Paints every texture in the game when it starts. There are no image files.
//!
//! A registry's tile list says what each texture should look like; this turns those descriptions
//! into pixels, and into item icons. Content packs get new textures the same way, with nothing to
//! download but a few numbers.
use zenith_content::{Icon, Pattern, Registry, Tex, Tool};
use zenith_core::Rng;
use zenith_core::noise::Fbm;

pub const TILE: usize = 32;
/// The side of a tile as the graphics card holds it: each painted tile drawn out finer, its edges
/// kept and its surfaces given a grain. Eight times the painting on a desktop, four on a phone,
/// which has a fraction of the memory.
pub const FINE: usize = if cfg!(target_os = "android") { 128 } else { 256 };
pub const CRACK_STAGES: usize = 10;

/// One tile: `TILE` x `TILE` RGBA. The alpha of an opaque block's tile says how much of the biome's
/// colour it takes (the band of grass on a grass block's side); of a cutout tile, where its holes are.
pub type Pixels = Vec<[u8; 4]>;

fn mix(a: [u8; 3], b: [u8; 3], t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t) as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

fn shade(c: [u8; 3], by: f64) -> [u8; 3] {
    [
        (c[0] as f64 * by).clamp(0.0, 255.0) as u8,
        (c[1] as f64 * by).clamp(0.0, 255.0) as u8,
        (c[2] as f64 * by).clamp(0.0, 255.0) as u8,
    ]
}

fn rgba(c: [u8; 3], a: u8) -> [u8; 4] {
    [c[0], c[1], c[2], a]
}

/// A field of noise that repeats across the tile, so that textures tile without a seam.
struct Field {
    fbm: Fbm,
    freq: f64,
}

impl Field {
    fn new(seed: u32, freq: f64, octaves: u32) -> Self {
        Self { fbm: Fbm::new(seed as u64 * 7919 + 13, octaves), freq }
    }
    /// About 0 to 1 at `(x, y)` in pixels. Blended across the edges so that it wraps.
    fn at(&self, x: usize, y: usize) -> f64 {
        let t = TILE as f64;
        let (fx, fy) = (x as f64 / t, y as f64 / t);
        let s = |u: f64, v: f64| self.fbm.get2(u * self.freq, v * self.freq);
        let n = (s(fx, fy) * (1.0 - fx) * (1.0 - fy)
            + s(fx - 1.0, fy) * fx * (1.0 - fy)
            + s(fx, fy - 1.0) * (1.0 - fx) * fy
            + s(fx - 1.0, fy - 1.0) * fx * fy)
            * 1.6;
        (n * 0.5 + 0.5).clamp(0.0, 1.0)
    }
}

fn each(mut f: impl FnMut(usize, usize) -> [u8; 4]) -> Pixels {
    let mut out = Vec::with_capacity(TILE * TILE);
    for y in 0..TILE {
        for x in 0..TILE {
            out.push(f(x, y));
        }
    }
    out
}

fn noise_tile(t: &Tex, freq: f64, octaves: u32, contrast: f64) -> Pixels {
    let field = Field::new(t.seed, freq, octaves);
    each(|x, y| {
        let n = ((field.at(x, y) - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
        rgba(mix(t.a, t.b, n), 255)
    })
}

/// Sprinkles dark and light grains over a base.
fn grains(mut px: Pixels, seed: u32, density: f64, strength: f64) -> Pixels {
    let mut rng = Rng::new(seed as u64 * 104_729 + 3);
    for p in &mut px {
        if rng.chance(density) {
            let by = if rng.chance(0.5) { 1.0 - strength } else { 1.0 + strength };
            let c = shade([p[0], p[1], p[2]], by);
            *p = [c[0], c[1], c[2], p[3]];
        }
    }
    px
}

/// Distance from a pixel to the nearest of a set of wrapped points, and to the next nearest.
fn voronoi(points: &[(f64, f64)], x: usize, y: usize) -> (f64, f64) {
    let t = TILE as f64;
    let (mut d1, mut d2) = (f64::MAX, f64::MAX);
    for &(px, py) in points {
        let dx = (px - x as f64).abs();
        let dy = (py - y as f64).abs();
        let (dx, dy) = (dx.min(t - dx), dy.min(t - dy));
        let d = dx * dx + dy * dy;
        if d < d1 {
            d2 = d1;
            d1 = d;
        } else if d < d2 {
            d2 = d;
        }
    }
    (d1.sqrt(), d2.sqrt())
}

fn scatter(seed: u32, count: usize) -> Vec<(f64, f64)> {
    let mut rng = Rng::new(seed as u64 * 15_485_863 + 5);
    (0..count).map(|_| (rng.f64() * TILE as f64, rng.f64() * TILE as f64)).collect()
}

fn cobble(t: &Tex) -> Pixels {
    let points = scatter(t.seed, 11);
    let field = Field::new(t.seed, 3.0, 2);
    each(|x, y| {
        let (d1, d2) = voronoi(&points, x, y);
        let edge = d2 - d1;
        if edge < 1.4 {
            rgba(t.b, 255)
        } else {
            let round = (1.0 - d1 / 9.0).clamp(0.0, 1.0);
            rgba(shade(t.a, 0.82 + round * 0.3 + (field.at(x, y) - 0.5) * 0.25), 255)
        }
    })
}

fn bricks(t: &Tex, rows: usize, cols: usize, mortar: usize, bevel: bool) -> Pixels {
    let field = Field::new(t.seed, 8.0, 3);
    let row_h = TILE / rows;
    let col_w = TILE / cols;
    each(|x, y| {
        let row = y / row_h;
        let shifted = (x + if row % 2 == 1 { col_w / 2 } else { 0 }) % TILE;
        let (ry, rx) = (y % row_h, shifted % col_w);
        if ry < mortar || rx < mortar {
            return rgba(t.b, 255);
        }
        let id = (shifted / col_w + row * 7) as u32;
        let tone = 0.92 + (zenith_core::rng::mix(t.seed as u64, id as u64) % 16) as f64 * 0.012;
        let light = if bevel && ry == mortar {
            1.14
        } else if bevel && ry == row_h - 1 {
            0.82
        } else {
            1.0
        };
        rgba(shade(t.a, tone * light * (0.92 + field.at(x, y) * 0.16)), 255)
    })
}

fn planks(t: &Tex) -> Pixels {
    let grain = Field::new(t.seed, 6.0, 3);
    let board = TILE / 4;
    each(|x, y| {
        let b = y / board;
        if y % board == 0 {
            return rgba(shade(t.b, 0.72), 255);
        }
        let offset = (b * 11) % TILE;
        // Stretched sideways, as wood grain is.
        let n = grain.at((x / 4 + offset) % TILE, (y * 2) % TILE);
        let seam = (x + b * 13).is_multiple_of(TILE);
        let c = mix(t.a, t.b, n * 1.2 - 0.1);
        rgba(if seam { shade(t.b, 0.8) } else { c }, 255)
    })
}

fn log_side(t: &Tex) -> Pixels {
    let bark = Field::new(t.seed, 7.0, 3);
    let is_birch = t.a[0] > 200;
    let marks = scatter(t.seed + 1, 7);
    each(|x, y| {
        if is_birch {
            let dash = marks.iter().any(|&(mx, my)| {
                let dy = (my - y as f64).abs();
                let dx = (mx - x as f64).abs();
                dy.min(TILE as f64 - dy) < 1.0 && dx.min(TILE as f64 - dx) < 4.0
            });
            let n = bark.at(x, y);
            return rgba(if dash { t.b } else { shade(t.a, 0.92 + n * 0.14) }, 255);
        }
        // Vertical furrows.
        let n = bark.at((x * 3) % TILE, y / 2);
        rgba(mix(t.a, t.b, (n - 0.35) * 2.2), 255)
    })
}

fn log_top(t: &Tex) -> Pixels {
    let field = Field::new(t.seed, 4.0, 2);
    let c = (TILE as f64 - 1.0) / 2.0;
    each(|x, y| {
        let d = ((x as f64 - c).powi(2) + (y as f64 - c).powi(2)).sqrt();
        if d > c - 1.5 {
            return rgba(t.b, 255);
        }
        let ring = ((d + field.at(x, y) * 2.0) / 2.6).floor() % 2.0;
        rgba(shade(t.a, 0.92 + ring * 0.12), 255)
    })
}

fn leaves(t: &Tex) -> Pixels {
    let field = Field::new(t.seed, 9.0, 3);
    let holes = Field::new(t.seed + 9, 14.0, 2);
    each(|x, y| {
        let n = field.at(x, y);
        let a = if holes.at(x, y) > 0.67 { 0 } else { 255 };
        rgba(shade(t.a, 0.6 + n * 0.5), a)
    })
}

fn sand(t: &Tex) -> Pixels {
    grains(noise_tile(t, 5.0, 2, 1.0), t.seed, 0.22, 0.045)
}

fn gravel(t: &Tex) -> Pixels {
    let points = scatter(t.seed, 26);
    each(|x, y| {
        let (d1, d2) = voronoi(&points, x, y);
        if d2 - d1 < 0.8 {
            return rgba(shade(t.b, 0.75), 255);
        }
        let id = points.iter().position(|&(px, py)| {
            let (dx, dy) = ((px - x as f64).abs(), (py - y as f64).abs());
            let (dx, dy) = (dx.min(TILE as f64 - dx), dy.min(TILE as f64 - dy));
            (dx * dx + dy * dy).sqrt() == d1
        });
        let tone = id.map_or(0.5, |i| (zenith_core::rng::mix(t.seed as u64, i as u64) % 100) as f64 / 100.0);
        rgba(mix(t.a, t.b, tone), 255)
    })
}

fn ore(t: &Tex) -> Pixels {
    let base = Tex { pattern: Pattern::Speckle, a: t.a, b: shade(t.a, 0.84), seed: 1 };
    let mut px = speckle(&base);
    let blobs = scatter(t.seed, 5);
    let edge = Field::new(t.seed, 10.0, 2);
    for y in 0..TILE {
        for x in 0..TILE {
            let (d, _) = voronoi(&blobs, x, y);
            let r = d + (edge.at(x, y) - 0.5) * 2.5;
            if r < 2.8 {
                let highlight = if r < 1.2 { 1.2 } else { 1.0 };
                px[y * TILE + x] = rgba(shade(t.b, highlight * (0.9 + edge.at(x, y) * 0.2)), 255);
            } else if r < 3.5 {
                px[y * TILE + x] = rgba(shade(t.a, 0.62), 255);
            }
        }
    }
    px
}

fn speckle(t: &Tex) -> Pixels {
    grains(noise_tile(t, 5.0, 3, 1.3), t.seed, 0.14, 0.09)
}

fn strata(t: &Tex) -> Pixels {
    let wobble = Field::new(t.seed, 3.0, 2);
    let tone = Field::new(t.seed + 1, 7.0, 2);
    each(|x, y| {
        let band = ((y as f64 + wobble.at(x, y) * 3.0) / 5.0).floor();
        let n = ((band * 2.399).fract() * 0.8 + tone.at(x, y) * 0.3).clamp(0.0, 1.0);
        rgba(mix(t.a, t.b, n), 255)
    })
}

fn grass(t: &Tex) -> Pixels {
    let blades = Field::new(t.seed, 12.0, 3);
    let patch = Field::new(t.seed + 2, 4.0, 2);
    each(|x, y| {
        let n = blades.at(x, y) * 0.75 + patch.at(x, y) * 0.25;
        rgba(shade(t.a, 0.74 + n * 0.42), 255)
    })
}

/// Soil with a band of grass over the top, its edge hanging in uneven tufts. The band's alpha is
/// full, so only it takes the biome's colour.
fn grass_side(t: &Tex) -> Pixels {
    let soil = grains(noise_tile(t, 5.0, 3, 1.2), t.seed, 0.1, 0.08);
    let blades = Field::new(t.seed + 1, 12.0, 3);
    let mut heights = [0usize; TILE];
    let mut rng = Rng::new(t.seed as u64 + 77);
    for h in &mut heights {
        *h = 4 + rng.below(5) as usize;
    }
    each(|x, y| {
        if y < heights[x] {
            let c = shade([255, 255, 255], 0.74 + blades.at(x, y) * 0.42);
            return rgba(c, 255);
        }
        // Everything under the band shows its own colour, with no tint: alpha zero.
        let s = soil[y * TILE + x];
        [s[0], s[1], s[2], 0]
    })
}

fn tuft(t: &Tex) -> Pixels {
    let mut rng = Rng::new(t.seed as u64 * 31 + 1);
    let mut px = vec![[0u8, 0, 0, 0]; TILE * TILE];
    for _ in 0..18 {
        let mut x = rng.range(6, 25) as f64;
        let height = rng.range(12, 30);
        let lean = (rng.f64() - 0.5) * 0.35;
        let tone = 0.68 + rng.f64() * 0.5;
        for dy in 0..height {
            x += lean;
            let (ix, iy) = (x as usize, TILE - 1 - dy as usize);
            if ix < TILE {
                let fade = 1.0 - dy as f64 / height as f64 * 0.25;
                px[iy * TILE + ix] = rgba(shade(t.a, tone * fade), 255);
            }
        }
    }
    px
}

fn stem(px: &mut Pixels, c: [u8; 3], x: usize, from: usize) {
    for y in from..TILE {
        for dx in 0..2 {
            px[y * TILE + x + dx] = rgba(shade(c, 0.85 + (dx as f64) * 0.2), 255);
        }
    }
}

fn disc(px: &mut Pixels, cx: f64, cy: f64, r: f64, c: [u8; 3]) {
    for y in 0..TILE {
        for x in 0..TILE {
            let d = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
            if d <= r {
                px[y * TILE + x] = rgba(shade(c, 1.05 - d / r * 0.3), 255);
            }
        }
    }
}

fn flower(t: &Tex) -> Pixels {
    let mut px = vec![[0u8, 0, 0, 0]; TILE * TILE];
    stem(&mut px, t.b, 15, 13);
    let mut rng = Rng::new(t.seed as u64);
    for i in 0..3usize {
        // A pair of leaves on the stem.
        let y = 20 + i * 3;
        for d in 1..4 + rng.below(2) as usize {
            px[y * TILE + 15 - d] = rgba(t.b, 255);
            px[(y - 1) * TILE + 16 + d] = rgba(shade(t.b, 0.85), 255);
        }
    }
    for k in 0..5 {
        let a = k as f64 / 5.0 * std::f64::consts::TAU;
        disc(&mut px, 15.5 + a.cos() * 4.5, 9.0 + a.sin() * 4.5, 3.2, t.a);
    }
    disc(&mut px, 15.5, 9.0, 2.2, [250, 214, 70]);
    px
}

fn mushroom(t: &Tex) -> Pixels {
    let mut px = vec![[0u8, 0, 0, 0]; TILE * TILE];
    for y in 16..TILE {
        for x in 13..18 {
            px[y * TILE + x] = rgba(shade(t.b, if x == 13 { 0.8 } else { 1.0 }), 255);
        }
    }
    for y in 7..18 {
        for x in 6..26 {
            let (dx, dy) = ((x as f64 - 15.5) / 10.0, (y as f64 - 17.0) / 10.0);
            if dx * dx + dy * dy < 1.0 {
                px[y * TILE + x] = rgba(shade(t.a, 0.9 + (17 - y) as f64 * 0.02), 255);
            }
        }
    }
    let spots = scatter(t.seed, 5);
    for &(sx, sy) in &spots {
        let (sx, sy) = (8.0 + sx * 0.5, 9.0 + sy * 0.2);
        let (ix, iy) = (sx as usize, sy as usize);
        if px[iy * TILE + ix][3] > 0 {
            px[iy * TILE + ix] = rgba(t.b, 255);
            px[iy * TILE + ix + 1] = rgba(t.b, 255);
        }
    }
    px
}

fn reed(t: &Tex) -> Pixels {
    let mut px = vec![[0u8, 0, 0, 0]; TILE * TILE];
    let mut rng = Rng::new(t.seed as u64);
    for x in [7usize, 13, 19, 25] {
        let top = rng.range(1, 9) as usize;
        stem(&mut px, if x % 2 == 0 { t.a } else { t.b }, x, top);
        for y in top..top + 5 {
            px[y * TILE + x] = rgba([116, 84, 52], 255);
            px[y * TILE + x + 1] = rgba([96, 68, 42], 255);
        }
    }
    px
}

fn sapling(t: &Tex) -> Pixels {
    let mut px = vec![[0u8, 0, 0, 0]; TILE * TILE];
    stem(&mut px, t.b, 15, 16);
    let mut rng = Rng::new(t.seed as u64);
    for _ in 0..8 {
        disc(
            &mut px,
            15.5 + (rng.f64() - 0.5) * 12.0,
            11.0 + (rng.f64() - 0.5) * 9.0,
            2.6,
            shade(t.a, 0.8 + rng.f64() * 0.4),
        );
    }
    px
}

fn dead_bush(t: &Tex) -> Pixels {
    let mut px = vec![[0u8, 0, 0, 0]; TILE * TILE];
    let mut rng = Rng::new(t.seed as u64);
    for _ in 0..7 {
        let (mut x, mut y) = (15.0, 31.0);
        let (dx, dy) = ((rng.f64() - 0.5) * 1.6, -0.5 - rng.f64() * 0.8);
        for _ in 0..rng.range(10, 24) {
            x += dx + (rng.f64() - 0.5) * 0.5;
            y += dy;
            if x > 0.0 && (x as usize) < TILE && y > 0.0 {
                px[y as usize * TILE + x as usize] = rgba(shade(t.a, 0.75 + rng.f64() * 0.4), 255);
            }
        }
    }
    px
}

fn glass(t: &Tex) -> Pixels {
    each(|x, y| {
        let edge = x == 0 || y == 0 || x == TILE - 1 || y == TILE - 1;
        if edge {
            return rgba(t.b, 255);
        }
        // A couple of streaks of reflection; the rest is clear.
        let streak = (x + y) % 19 < 2 && (6..26).contains(&x);
        if streak { rgba(t.a, 255) } else { [0, 0, 0, 0] }
    })
}

fn wool(t: &Tex) -> Pixels {
    let fibres = Field::new(t.seed, 14.0, 3);
    each(|x, y| rgba(mix(t.a, t.b, fibres.at(x, y) * 1.3 - 0.15), 255))
}

fn ice(t: &Tex) -> Pixels {
    let points = scatter(t.seed, 7);
    let n = Field::new(t.seed, 4.0, 2);
    each(|x, y| {
        let (d1, d2) = voronoi(&points, x, y);
        let crack = d2 - d1 < 0.7;
        let c = if crack { t.b } else { mix(t.a, t.b, n.at(x, y) * 0.5) };
        rgba(c, if crack { 230 } else { 190 })
    })
}

/// A torch's box is ten sixteenths tall and takes the bottom of the tile, so its tip is row 12.
fn torch(t: &Tex) -> Pixels {
    each(|x, y| {
        if !(14..18).contains(&x) || y < 12 {
            return [0, 0, 0, 0];
        }
        if (12..18).contains(&y) {
            let hot = 1.0 - (y - 12) as f64 / 6.0;
            return rgba(mix(t.a, [255, 250, 210], hot), 255);
        }
        rgba(shade(t.b, if x == 14 { 0.8 } else { 1.0 }), 255)
    })
}

/// Tongues of fire: each wide at the foot and a point at the tip, hottest low down in the middle.
fn flame(t: &Tex) -> Pixels {
    let mut rng = Rng::new(t.seed as u64 * 17 + 3);
    let tongues: Vec<(f64, f64, f64)> = (0..6)
        .map(|i| (3.0 + i as f64 * 5.2 + rng.f64() * 2.0, 13.0 + rng.f64() * 17.0, 2.6 + rng.f64() * 1.8))
        .collect();
    each(|x, y| {
        let up = (TILE - 1 - y) as f64;
        let mut heat: f64 = 0.0;
        for &(middle, height, width) in &tongues {
            if up < height {
                let half = width * (1.0 - up / height).powf(0.7) + 0.4;
                let off = (x as f64 - middle - (up * 0.35).sin() * 1.2).abs();
                if off < half {
                    heat = heat.max((1.0 - up / height) * (1.0 - off / half * 0.5));
                }
            }
        }
        if heat <= 0.0 {
            return [0, 0, 0, 0];
        }
        let c = if heat > 0.7 { mix(t.a, [255, 250, 204], (heat - 0.7) / 0.3) } else { mix(t.b, t.a, heat / 0.7) };
        rgba(c, 255)
    })
}

/// A campfire from above: charred logs laid across, and coals glowing between them.
fn embers(t: &Tex) -> Pixels {
    let n = Field::new(t.seed, 5.0, 2);
    each(|x, y| {
        let v = n.at(x, y);
        let log = (y + 2) / 5 % 2 == 0 && (2..TILE - 2).contains(&x);
        if log {
            rgba(shade(t.a, 0.7 + v * 0.6), 255)
        } else if v > 0.42 {
            rgba(mix(t.b, [255, 236, 150], ((v - 0.42) * 1.8).min(1.0)), 255)
        } else {
            rgba(shade(t.a, 0.4), 255)
        }
    })
}

fn ladder(t: &Tex) -> Pixels {
    each(|x, y| {
        let rail = (3..7).contains(&x) || (25..29).contains(&x);
        let rung = (y + 4) % 8 < 3 && (3..29).contains(&x);
        if rail {
            rgba(shade(t.a, if x == 3 || x == 25 { 0.8 } else { 1.0 }), 255)
        } else if rung {
            rgba(shade(t.b, if (y + 4) % 8 == 2 { 0.75 } else { 1.0 }), 255)
        } else {
            [0, 0, 0, 0]
        }
    })
}

fn bench_top(t: &Tex) -> Pixels {
    let wood = planks(&Tex { b: shade(t.a, 0.82), ..*t });
    each(|x, y| {
        let line = x % 16 < 1 || y % 16 < 1;
        if line || x == TILE - 1 || y == TILE - 1 { rgba(t.b, 255) } else { wood[y * TILE + x] }
    })
}

fn bench_side(t: &Tex) -> Pixels {
    let wood = planks(&Tex { b: shade(t.a, 0.82), ..*t });
    each(|x, y| {
        let top = y < 5;
        let saw = (6..14).contains(&x) && (9..21).contains(&y) && (x + y) % 3 != 0;
        let hammer =
            (19..27).contains(&x) && (((10..14).contains(&y)) || ((22..24).contains(&x) && (14..24).contains(&y)));
        if top {
            rgba(t.b, 255)
        } else if saw {
            rgba([170, 174, 180], 255)
        } else if hammer {
            rgba([96, 98, 104], 255)
        } else {
            wood[y * TILE + x]
        }
    })
}

fn furnace_side(t: &Tex) -> Pixels {
    let base = Tex { pattern: Pattern::Cobble, ..*t };
    cobble(&base)
}

fn furnace_front(t: &Tex) -> Pixels {
    let base = furnace_side(&Tex { b: shade(t.a, 0.68), ..*t });
    each(|x, y| {
        let mouth = (7..25).contains(&x) && (14..28).contains(&y);
        let frame = (5..27).contains(&x) && (12..30).contains(&y);
        if mouth {
            let glow = ((y as f64 - 14.0) / 14.0).clamp(0.0, 1.0);
            rgba(mix(t.b, [120, 60, 28], glow * 0.8), 255)
        } else if frame {
            rgba(shade(t.a, 0.62), 255)
        } else {
            base[y * TILE + x]
        }
    })
}

fn chest_side(t: &Tex) -> Pixels {
    let grain = Field::new(t.seed, 6.0, 2);
    each(|x, y| {
        let edge = !(2..TILE - 2).contains(&x) || !(2..TILE - 2).contains(&y);
        let lid = (11..13).contains(&y);
        let n = grain.at(x / 3, y);
        if edge || lid { rgba(t.b, 255) } else { rgba(shade(t.a, 0.86 + n * 0.24), 255) }
    })
}

fn chest_front(t: &Tex) -> Pixels {
    let side = chest_side(&Tex { b: shade(t.a, 0.62), ..*t });
    each(|x, y| if (13..19).contains(&x) && (9..17).contains(&y) { rgba(t.b, 255) } else { side[y * TILE + x] })
}

fn chest_top(t: &Tex) -> Pixels {
    chest_side(&Tex { ..*t })
}

fn lamp(t: &Tex) -> Pixels {
    let cells = scatter(t.seed, 9);
    let n = Field::new(t.seed, 8.0, 2);
    each(|x, y| {
        let (d1, d2) = voronoi(&cells, x, y);
        let seam = d2 - d1 < 1.1;
        let c = if seam { t.b } else { mix(t.a, t.b, n.at(x, y) * 0.4) };
        rgba(c, 255)
    })
}

fn wire(t: &Tex) -> Pixels {
    let c = TILE as f64 / 2.0 - 0.5;
    let mut rng = Rng::new(t.seed as u64);
    let dust: Vec<(usize, usize)> =
        (0..70).map(|_| (rng.below(TILE as u32) as usize, rng.below(TILE as u32) as usize)).collect();
    each(|x, y| {
        let along = (x as f64 - c).abs().min((y as f64 - c).abs());
        if along < 2.2 {
            rgba(t.a, 255)
        } else if dust.contains(&(x, y)) {
            rgba(t.b, 255)
        } else {
            [0, 0, 0, 0]
        }
    })
}

fn lever(t: &Tex) -> Pixels {
    each(|x, y| {
        let handle = (14..18).contains(&x) && y < 16;
        if handle { rgba(t.b, 255) } else { rgba(shade(t.a, if (x + y) % 5 == 0 { 0.85 } else { 1.0 }), 255) }
    })
}

fn inverter(t: &Tex) -> Pixels {
    each(|x, y| {
        let mid = (14..18).contains(&x);
        if !mid {
            return [0, 0, 0, 0];
        }
        if y < 8 { rgba(t.a, 255) } else { rgba(shade(t.b, if x == 14 { 0.8 } else { 1.0 }), 255) }
    })
}

fn cactus(t: &Tex) -> Pixels {
    let n = Field::new(t.seed, 8.0, 2);
    each(|x, y| {
        let rib = x % 8 < 2;
        let spine = (x + y * 3) % 11 == 0 && !rib;
        let c = if rib { t.b } else { shade(t.a, 0.9 + n.at(x, y) * 0.22) };
        rgba(if spine { [226, 226, 188] } else { c }, 255)
    })
}

fn cactus_top(t: &Tex) -> Pixels {
    let c = TILE as f64 / 2.0 - 0.5;
    each(|x, y| {
        let d = ((x as f64 - c).powi(2) + (y as f64 - c).powi(2)).sqrt();
        let ring = (d / 4.0) as i32 % 2 == 0;
        rgba(if ring { t.a } else { t.b }, 255)
    })
}

fn tnt(t: &Tex) -> Pixels {
    each(|x, y| {
        let band = (11..21).contains(&y);
        let tube = x % 8 < 7;
        if band {
            rgba(shade(t.b, if (x + y) % 7 == 0 { 0.7 } else { 1.0 }), 255)
        } else if tube {
            rgba(shade(t.a, 0.8 + ((x % 8) as f64 / 8.0) * 0.4), 255)
        } else {
            rgba(shade(t.a, 0.55), 255)
        }
    })
}

/// Tilled soil: furrows across, their ridges catching the light, with a few pale pebbles.
fn farmland(t: &Tex) -> Pixels {
    let clods = Field::new(t.seed, 9.0, 3);
    let mut rng = Rng::new(t.seed as u64 * 13 + 5);
    let pebbles: Vec<(usize, usize)> =
        (0..12).map(|_| (rng.below(TILE as u32) as usize, rng.below(TILE as u32) as usize)).collect();
    each(|x, y| {
        let across = (y % 8) as f64 / 8.0;
        let ridge = (across * std::f64::consts::TAU).sin() * 0.5 + 0.5;
        let c = mix(t.b, t.a, ridge * 0.75 + clods.at(x, y) * 0.35);
        rgba(if pebbles.contains(&(x, y)) { shade(c, 1.3) } else { c }, 255)
    })
}

/// Grain: thin stalks (colour `a`) with fat heads (colour `b`) at their tops, and a leaf here and
/// there. A young crop shows only the bottom of this, so the heads come in last.
fn crop(t: &Tex) -> Pixels {
    let mut rng = Rng::new(t.seed as u64 * 17 + 3);
    let mut px = vec![[0u8, 0, 0, 0]; TILE * TILE];
    for i in 0..7 {
        let mut x = 2.5 + i as f64 * 4.4 + rng.f64() * 1.5;
        let height = rng.range(24, 31) as usize;
        let lean = (rng.f64() - 0.5) * 0.12;
        for dy in 0..height {
            x += lean;
            let (ix, iy) = (x as usize, TILE - 1 - dy);
            if ix >= TILE {
                break;
            }
            let head = dy + 9 > height;
            let c = if head {
                shade(t.b, 0.8 + rng.f64() * 0.4)
            } else {
                shade(t.a, 0.75 + dy as f64 / height as f64 * 0.35)
            };
            px[iy * TILE + ix] = rgba(c, 255);
            if head && dy % 2 == 0 {
                for side in [ix.wrapping_sub(1), ix + 1] {
                    if side < TILE {
                        px[iy * TILE + side] = rgba(shade(t.b, 0.7 + rng.f64() * 0.3), 255);
                    }
                }
            }
            if !head && dy % 9 == 5 && ix + 2 < TILE {
                px[iy * TILE + ix + 1] = rgba(shade(t.a, 0.9), 255);
                px[(iy - 1) * TILE + ix + 2] = rgba(shade(t.a, 0.8), 255);
            }
        }
    }
    px
}

/// A patchwork quilt: squares of two colours, stitched where they meet.
fn quilt(t: &Tex) -> Pixels {
    let weave = Field::new(t.seed, 16.0, 2);
    each(|x, y| {
        let light = (x / 8 + y / 8) % 2 == 0;
        let base = if light { mix(t.a, t.b, 0.55) } else { t.a };
        let c = shade(base, 0.9 + weave.at(x, y) * 0.2);
        rgba(if x % 8 == 0 || y % 8 == 0 { shade(c, 0.78) } else { c }, 255)
    })
}

/// A pillow (colour `a`) lying on the end of the quilt (colour `b`).
fn pillow(t: &Tex) -> Pixels {
    let soft = Field::new(t.seed, 10.0, 2);
    each(|x, y| {
        // A rounded rectangle, by its distance from the edge.
        let (qx, qy) = ((x as f64 - 15.5).abs() - 9.0, (y as f64 - 15.5).abs() - 7.0);
        let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - 3.0;
        if d > 0.0 {
            rgba(shade(t.b, 0.85 + soft.at(x, y) * 0.2), 255)
        } else {
            rgba(shade(t.a, 0.84 + soft.at(x, y) * 0.16 + (-d / 6.0).min(1.0) * 0.08), 255)
        }
    })
}

/// A bed's side: the quilt (colour `a`) hanging over the wooden frame (colour `b`). A bed is nine
/// sixteenths tall, so only the bottom of this is ever seen on one.
fn bed_side(t: &Tex) -> Pixels {
    let grain = Field::new(t.seed, 6.0, 3);
    each(|x, y| match y {
        0..=21 => rgba(shade(t.a, 0.86 + grain.at(x, y) * 0.16 - if y == 21 { 0.2 } else { 0.0 }), 255),
        22 => rgba([238, 232, 220], 255),
        _ => rgba(mix(t.b, shade(t.b, 0.75), grain.at(x / 3, y * 2 % TILE)), 255),
    })
}

/// A panelled door: a frame, and two sunken panels with their grain running up them, and a handle.
fn door(t: &Tex) -> Pixels {
    let grain = Field::new(t.seed, 6.0, 3);
    each(|x, y| {
        if (24..27).contains(&x) && (14..17).contains(&y) {
            return rgba([58, 54, 50], 255);
        }
        let frame = !(4..28).contains(&x) || !(3..29).contains(&y) || (14..17).contains(&y);
        let wood = mix(t.a, shade(t.a, 0.8), grain.at(x / 2, (y * 2) % TILE));
        let c = if frame {
            shade(mix(t.a, t.b, 0.45), 0.94 + grain.at(x, y) * 0.12)
        } else if x == 4 || y == 3 || y == 17 {
            shade(wood, 0.72)
        } else {
            wood
        };
        rgba(c, 255)
    })
}

/// A trapdoor: boards across a frame, with dark gaps between them.
fn trapdoor(t: &Tex) -> Pixels {
    let grain = Field::new(t.seed, 6.0, 3);
    each(|x, y| {
        let frame = !(3..29).contains(&x) || !(3..29).contains(&y);
        let c = if frame {
            shade(t.b, 0.95 + grain.at(x, y) * 0.15)
        } else if y % 6 == 2 {
            shade(t.b, 0.6)
        } else {
            mix(t.a, shade(t.a, 0.85), grain.at(x / 3, y))
        };
        rgba(c, 255)
    })
}

/// Rails: two bars (colour `a`) running north to south along wooden ties (colour `b`), with
/// nothing between.
fn rail(t: &Tex) -> Pixels {
    let grain = Field::new(t.seed, 6.0, 3);
    let mut px = vec![[0u8, 0, 0, 0]; TILE * TILE];
    for y in 0..TILE {
        for x in 0..TILE {
            let bar = (6..9).contains(&x) || (23..26).contains(&x);
            let tie = (2..6).contains(&(y % 8)) && (3..29).contains(&x);
            if bar {
                px[y * TILE + x] = rgba(shade(t.a, if x == 6 || x == 23 { 1.2 } else { 0.85 }), 255);
            } else if tie {
                px[y * TILE + x] = rgba(mix(t.b, shade(t.b, 0.78), grain.at(x, y)), 255);
            }
        }
    }
    px
}

/// The same, round a corner: the bars curve from the south edge to the east edge, about the
/// south-east corner, over ties fanned out between them.
fn rail_curve(t: &Tex) -> Pixels {
    let grain = Field::new(t.seed, 6.0, 3);
    let mut px = vec![[0u8, 0, 0, 0]; TILE * TILE];
    let corner = TILE as f64;
    for y in 0..TILE {
        for x in 0..TILE {
            let (dx, dy) = (corner - x as f64 - 0.5, corner - y as f64 - 0.5);
            let r = (dx * dx + dy * dy).sqrt();
            let angle = dy.atan2(dx) / std::f64::consts::FRAC_PI_2;
            let bar = (r - 24.5).abs() < 1.5 || (r - 7.5).abs() < 1.5;
            let tie = (3.0..29.0).contains(&r) && (0.25..0.6).contains(&(angle * 5.0).fract());
            if bar {
                px[y * TILE + x] = rgba(shade(t.a, 0.85 + ((r * 2.0) as usize % 3) as f64 * 0.12), 255);
            } else if tie {
                px[y * TILE + x] = rgba(mix(t.b, shade(t.b, 0.78), grain.at(x, y)), 255);
            }
        }
    }
    px
}

/// Paints one tile.
pub fn paint(t: &Tex) -> Pixels {
    match t.pattern {
        Pattern::Noise => noise_tile(t, 5.0, 3, 1.1),
        Pattern::Speckle => speckle(t),
        Pattern::Strata => strata(t),
        Pattern::Cobble => cobble(t),
        Pattern::Bricks => bricks(t, 4, 2, 1, false),
        Pattern::Blocks => bricks(t, 2, 1, 1, true),
        Pattern::Planks => planks(t),
        Pattern::LogSide => log_side(t),
        Pattern::LogTop => log_top(t),
        Pattern::Leaves => leaves(t),
        Pattern::Grass => grass(t),
        Pattern::GrassSide => grass_side(t),
        Pattern::Sand => sand(t),
        Pattern::Gravel => gravel(t),
        Pattern::Ore => ore(t),
        Pattern::Tuft => tuft(t),
        Pattern::Flower => flower(t),
        Pattern::Mushroom => mushroom(t),
        Pattern::Reed => reed(t),
        Pattern::Sapling => sapling(t),
        Pattern::DeadBush => dead_bush(t),
        Pattern::Glass => glass(t),
        Pattern::Wool => wool(t),
        Pattern::Water => noise_tile(t, 5.0, 2, 0.8),
        Pattern::Lava => noise_tile(t, 4.0, 3, 1.6),
        Pattern::Ice => ice(t),
        Pattern::Snow => grains(noise_tile(t, 5.0, 2, 0.7), t.seed, 0.1, 0.02),
        Pattern::Torch => torch(t),
        Pattern::Ladder => ladder(t),
        Pattern::BenchTop => bench_top(t),
        Pattern::BenchSide => bench_side(t),
        Pattern::FurnaceFront => furnace_front(t),
        Pattern::FurnaceSide => furnace_side(t),
        Pattern::ChestFront => chest_front(t),
        Pattern::ChestSide => chest_side(t),
        Pattern::ChestTop => chest_top(t),
        Pattern::Lamp => lamp(t),
        Pattern::Wire => wire(t),
        Pattern::Lever => lever(t),
        Pattern::Inverter => inverter(t),
        Pattern::Cactus => cactus(t),
        Pattern::CactusTop => cactus_top(t),
        Pattern::Tnt => tnt(t),
        Pattern::Farmland => farmland(t),
        Pattern::Crop => crop(t),
        Pattern::Quilt => quilt(t),
        Pattern::Pillow => pillow(t),
        Pattern::BedSide => bed_side(t),
        Pattern::Door => door(t),
        Pattern::Trapdoor => trapdoor(t),
        Pattern::Rail => rail(t),
        Pattern::RailCurve => rail_curve(t),
        Pattern::Flame => flame(t),
        Pattern::Embers => embers(t),
    }
}

/// How bumpy a tile's surface is, how rough, and how metal, by what it shows: stone and brick are
/// craggy and dull, wood a little smoother, glass and ice glossy. Dark noise is obsidian, which
/// shines.
fn surface(t: &Tex) -> (f64, f64, f64) {
    let dark = (t.a[0] as f64 + t.a[1] as f64 + t.a[2] as f64) / 765.0 < 0.25;
    match t.pattern {
        Pattern::Glass | Pattern::Ice => (0.0, 0.06, 0.0),
        Pattern::Water | Pattern::Lava => (0.0, 0.1, 0.0),
        Pattern::Noise if dark => (0.6, 0.14, 0.0),
        Pattern::Cobble | Pattern::Bricks | Pattern::Blocks | Pattern::Gravel => (3.2, 0.85, 0.0),
        Pattern::Speckle | Pattern::Strata | Pattern::Noise | Pattern::FurnaceSide | Pattern::FurnaceFront => {
            (2.2, 0.8, 0.0)
        }
        Pattern::Ore => (2.6, 0.75, 0.0),
        Pattern::Planks
        | Pattern::LogSide
        | Pattern::LogTop
        | Pattern::BenchTop
        | Pattern::BenchSide
        | Pattern::ChestFront
        | Pattern::ChestSide
        | Pattern::ChestTop
        | Pattern::Door
        | Pattern::Trapdoor
        | Pattern::Ladder => (1.6, 0.62, 0.0),
        Pattern::Sand | Pattern::Snow | Pattern::Wool | Pattern::Quilt | Pattern::Pillow | Pattern::BedSide => {
            (1.0, 0.95, 0.0)
        }
        Pattern::Farmland | Pattern::Grass | Pattern::GrassSide => (2.0, 0.9, 0.0),
        Pattern::Lamp => (1.0, 0.25, 0.0),
        Pattern::Rail | Pattern::RailCurve => (1.0, 0.6, 0.0),
        _ => (1.2, 0.75, 0.0),
    }
}

fn luminance(p: [u8; 4]) -> f64 {
    (p[0] as f64 * 0.3 + p[1] as f64 * 0.59 + p[2] as f64 * 0.11) / 255.0
}

fn near(p: [u8; 4], c: [u8; 3]) -> f64 {
    let d = |i: usize| p[i] as f64 - c[i] as f64;
    (d(0) * d(0) + d(1) * d(1) + d(2) * d(2)).sqrt()
}

/// A tile's material, as ray traced resource packs give their blocks one: which way each texel
/// faces, worked out from its brightness taken as height, then how rough it is and how metal. RGBA
/// in a plain (not sRGB) texture: the facing's lean across and down the tile, roughness, metalness.
/// The classic look's big pixels give big, blocky bumps.
pub fn material(px: &Pixels, t: &Tex) -> Pixels {
    let n = side(px);
    let (strength, rough, metal) = surface(t);
    // A finer picture changes less from one texel to the next for the same slope.
    let strength = strength * (n / TILE).max(1) as f64 * if n > TILE { 0.6 } else { 1.0 };
    let height = |x: usize, y: usize| {
        let p = px[(y % n) * n + x % n];
        if p[3] == 0 && alpha_means_holes(t.pattern) { 0.0 } else { luminance(p) }
    };
    // A vein of ore is a jewel, a seam of coal, or bright metal, by its colour.
    let vein = if t.b[2] > t.b[0] && t.b[2] > 150 {
        (0.12, 0.0)
    } else if (t.b[0] as u32 + t.b[1] as u32 + t.b[2] as u32) < 240 {
        (0.5, 0.0)
    } else {
        (0.3, 0.85)
    };
    let mut out = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            let (left, right) = (height(x + n - 1, y), height(x + 1, y));
            let (up, down) = (height(x, y + n - 1), height(x, y + 1));
            let (nx, ny) = (-(right - left) * 0.5 * strength, -(down - up) * 0.5 * strength);
            let length = (nx * nx + ny * ny + 1.0).sqrt();
            let p = px[y * n + x];
            let (r, m) = match t.pattern {
                Pattern::Ore if near(p, t.b) < near(p, t.a) => vein,
                Pattern::Rail | Pattern::RailCurve if near(p, t.a) < near(p, t.b) => (0.35, 1.0),
                _ => (rough, metal),
            };
            let encode = |v: f64| ((v / length) * 127.5 + 127.5).round().clamp(0.0, 255.0) as u8;
            out.push([encode(nx), encode(ny), (r * 255.0) as u8, (m * 255.0) as u8]);
        }
    }
    out
}

/// A material with no bumps: for cracks, and anything without a picture of its own.
pub fn plain_material() -> Pixels {
    vec![[128, 128, 200, 0]; FINE * FINE]
}

/// Halves a material down to one texel, each level the average of the one above.
pub fn material_chain(px: &Pixels) -> Vec<Pixels> {
    let mut chain = vec![px.clone()];
    let mut size = side(px);
    while size > 1 {
        let (prev, half) = (chain.last().cloned().unwrap_or_default(), size / 2);
        let mut next = Vec::with_capacity(half * half);
        for y in 0..half {
            for x in 0..half {
                let mut sum = [0u32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let p = prev[(y * 2 + dy) * size + x * 2 + dx];
                    for c in 0..4 {
                        sum[c] += p[c] as u32;
                    }
                }
                next.push(sum.map(|s| (s / 4) as u8));
            }
        }
        chain.push(next);
        size = half;
    }
    chain
}

/// How many texels across a square picture is.
fn side(px: &Pixels) -> usize {
    (px.len() as f64).sqrt().round() as usize
}

/// A painted tile at the size the graphics card holds, each of its pixels a square of texels: the
/// classic look, whose pixels are the point.
pub fn enlarge(px: &Pixels) -> Pixels {
    let scale = FINE / TILE;
    (0..FINE * FINE).map(|i| px[(i / FINE / scale) * TILE + (i % FINE) / scale]).collect()
}

/// How much grain a surface has when looked at closely, and how hard the lines in its picture
/// stay: rock is gritty and wood has a little tooth, glass and lamplight are clean, and anything
/// drawn in lines keeps them sharp.
fn fineness(p: Pattern) -> (f64, f64) {
    match p {
        Pattern::Glass | Pattern::Ice | Pattern::Water | Pattern::Lava | Pattern::Lamp | Pattern::Flame => (0.012, 2.0),
        Pattern::Torch
        | Pattern::Ladder
        | Pattern::Wire
        | Pattern::Lever
        | Pattern::Inverter
        | Pattern::Rail
        | Pattern::RailCurve
        | Pattern::Tnt
        | Pattern::Door
        | Pattern::Trapdoor => (0.03, 4.0),
        Pattern::Planks
        | Pattern::LogSide
        | Pattern::LogTop
        | Pattern::BenchTop
        | Pattern::BenchSide
        | Pattern::ChestFront
        | Pattern::ChestSide
        | Pattern::ChestTop => (0.05, 3.0),
        Pattern::Leaves
        | Pattern::Tuft
        | Pattern::Flower
        | Pattern::Mushroom
        | Pattern::Reed
        | Pattern::Sapling
        | Pattern::DeadBush
        | Pattern::Crop => (0.06, 2.5),
        Pattern::Wool | Pattern::Quilt | Pattern::Pillow | Pattern::Snow => (0.05, 1.6),
        Pattern::Bricks | Pattern::Blocks => (0.07, 3.0),
        _ => (0.085, 2.0),
    }
}

/// A number from -1 to 1 for a point, the same every time, that repeats every `period` cells.
fn lattice(x: usize, y: usize, period: usize, seed: u32) -> f64 {
    let (x, y) = ((x % period) as u32, (y % period) as u32);
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ seed.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    f64::from(h & 0xFFFF) / 32767.5 - 1.0
}

/// Smooth noise over a tile, from -1 to 1, in cells `cell` texels wide, that tiles.
fn tiled_noise(x: usize, y: usize, cell: usize, seed: u32) -> f64 {
    let period = FINE / cell;
    let (cx, cy) = (x / cell, y / cell);
    let ease = |t: f64| t * t * (3.0 - 2.0 * t);
    let (fx, fy) = (ease((x % cell) as f64 / cell as f64), ease((y % cell) as f64 / cell as f64));
    let at = |dx: usize, dy: usize| lattice(cx + dx, cy + dy, period, seed);
    let (top, bottom) = (at(0, 0) + (at(1, 0) - at(0, 0)) * fx, at(0, 1) + (at(1, 1) - at(0, 1)) * fx);
    top + (bottom - top) * fy
}

/// A painted tile drawn out to the size the graphics card holds, for the modern look. Where the
/// painting has an edge, the fine picture has it too, a clean line and not a staircase; where it
/// is all one surface, the colours run into each other; and over everything goes the grain of the
/// stuff it is, which the painting was too coarse to hold, and which the light then catches.
pub fn refine(px: &Pixels, pattern: Pattern, seed: u32) -> Pixels {
    let scale = FINE / TILE;
    let holes = alpha_means_holes(pattern);
    let (grain, crisp) = fineness(pattern);
    // Blocks repeat across a wall; plants and the like end at their own edges.
    let at = |x: isize, y: isize| -> [f64; 4] {
        let wrap = |v: isize| if holes { v.clamp(0, TILE as isize - 1) } else { v.rem_euclid(TILE as isize) } as usize;
        let p = px[wrap(y) * TILE + wrap(x)];
        let a = f64::from(p[3]) / 255.0;
        // The colour under a hole is nothing: it must not darken what is beside it.
        let w = if holes { a } else { 1.0 };
        [f64::from(p[0]) * w, f64::from(p[1]) * w, f64::from(p[2]) * w, a]
    };
    let sharpen = |t: f64| {
        let t = ((t - 0.5) * crisp + 0.5).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let mut out = Vec::with_capacity(FINE * FINE);
    for y in 0..FINE {
        for x in 0..FINE {
            let (u, v) = ((x as f64 + 0.5) / scale as f64 - 0.5, (y as f64 + 0.5) / scale as f64 - 0.5);
            let (x0, y0) = (u.floor(), v.floor());
            let (fx, fy) = (sharpen(u - x0), sharpen(v - y0));
            let (x0, y0) = (x0 as isize, y0 as isize);
            let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
            let mut p = [0.0; 4];
            for i in 0..4 {
                let (top, bottom) = (a[i] + (b[i] - a[i]) * fx, c[i] + (d[i] - c[i]) * fx);
                p[i] = top + (bottom - top) * fy;
            }
            let alpha = p[3];
            if holes && alpha < 0.5 {
                out.push([0, 0, 0, 0]);
                continue;
            }
            // Grain at three sizes, the finest the strongest.
            let g = tiled_noise(x, y, 2, seed) * 0.5
                + tiled_noise(x, y, 4, seed + 7) * 0.3
                + tiled_noise(x, y, 8, seed + 13) * 0.2;
            // And what the stuff itself has: wood its grain, running the way the wood does, and
            // stone its pits.
            let own = match pattern {
                Pattern::Planks
                | Pattern::BenchTop
                | Pattern::BenchSide
                | Pattern::ChestFront
                | Pattern::ChestSide
                | Pattern::ChestTop
                | Pattern::Trapdoor => tiled_noise(x, (y * 8) % FINE, 16, seed + 21) * 0.07,
                Pattern::LogSide | Pattern::Door | Pattern::Ladder => {
                    tiled_noise((x * 8) % FINE, y, 16, seed + 21) * 0.08
                }
                Pattern::Noise
                | Pattern::Speckle
                | Pattern::Strata
                | Pattern::Cobble
                | Pattern::Ore
                | Pattern::Blocks
                | Pattern::Bricks => {
                    let pit = tiled_noise(x, y, 2, seed + 31) * 0.6 + tiled_noise(x, y, 4, seed + 37) * 0.4;
                    if pit > 0.5 { -(pit - 0.5) * 0.5 } else { 0.0 }
                }
                _ => 0.0,
            };
            let tone = (1.0 + g * grain + own) / if holes { alpha.max(0.5) } else { 1.0 };
            let channel = |v: f64| (v * tone).round().clamp(0.0, 255.0) as u8;
            let a = if holes { 255 } else { (alpha * 255.0).round() as u8 };
            out.push([channel(p[0]), channel(p[1]), channel(p[2]), a]);
        }
    }
    out
}

/// How the mip chain of a tile should treat its alpha.
fn alpha_means_holes(p: Pattern) -> bool {
    matches!(
        p,
        Pattern::Leaves
            | Pattern::Tuft
            | Pattern::Flower
            | Pattern::Mushroom
            | Pattern::Reed
            | Pattern::Sapling
            | Pattern::DeadBush
            | Pattern::Glass
            | Pattern::Torch
            | Pattern::Ladder
            | Pattern::Wire
            | Pattern::Inverter
            | Pattern::Ice
            | Pattern::Crop
            | Pattern::Rail
            | Pattern::RailCurve
            | Pattern::Flame
    )
}

/// The classic look: a tile redrawn at half the resolution in a handful of colours, each art pixel
/// two texels square, so that up close the world is made of hard-edged pixels. The design stays the
/// painted one; only its detail and its palette go.
pub fn pixelate(px: &Pixels, pattern: Pattern, seed: u32) -> Pixels {
    const ART: usize = TILE / 2;
    const COLOURS: usize = 6;
    let holes = alpha_means_holes(pattern);
    // Natural materials get a grain of their own pixel by pixel, as pixel art has; things drawn with
    // clean lines keep them.
    let grain = !matches!(
        pattern,
        Pattern::Torch
            | Pattern::Wire
            | Pattern::Lever
            | Pattern::Inverter
            | Pattern::Lamp
            | Pattern::Tnt
            | Pattern::Glass
            | Pattern::Ladder
    );
    let mut small = Vec::with_capacity(ART * ART);
    for y in 0..ART {
        for x in 0..ART {
            let block = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(ox, oy)| px[(y * 2 + oy) * TILE + x * 2 + ox]);
            // A cutout pixel is there or it is not. An opaque one's alpha says how much biome colour it
            // takes, which is crisp too: a band of grass, not a blur. Either way the pixel takes its
            // colour only from the texels on the side that wins, so no grey from under the grass shows.
            let (high, low): (Vec<[u8; 4]>, Vec<[u8; 4]>) = block.iter().partition(|p| p[3] >= 128);
            let wins = high.len() >= 2;
            let from = if wins { &high } else { &low };
            let n = from.len().max(1) as u32;
            let by = if grain { 1.0 + (pixel_hash(x, y, seed) - 0.5) * 0.18 } else { 1.0 };
            let mean = |c: usize| {
                let m = from.iter().map(|p| p[c] as u32).sum::<u32>() as f64 / n as f64;
                (m * by).clamp(0.0, 255.0) as u8
            };
            let alpha = match (holes, wins) {
                (true, true) => high.iter().map(|p| p[3]).max().unwrap_or(255),
                (false, true) => 255,
                (_, false) => 0,
            };
            small.push([mean(0), mean(1), mean(2), alpha]);
        }
    }
    let shown: Vec<[u8; 3]> = small.iter().filter(|p| !holes || p[3] > 0).map(|p| [p[0], p[1], p[2]]).collect();
    let palette = punchier(palette(shown.iter().copied(), COLOURS), &shown);
    let mut out = vec![[0u8; 4]; TILE * TILE];
    for y in 0..ART {
        for x in 0..ART {
            let p = small[y * ART + x];
            // What does not show is left black and clear, as the mip chain expects of holes.
            let c = if holes && p[3] == 0 { [0, 0, 0, 0] } else { nearest(&palette, [p[0], p[1], p[2]], p[3]) };
            for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                out[(y * 2 + oy) * TILE + x * 2 + ox] = c;
            }
        }
    }
    out
}

/// Up to `count` colours that stand for all of `colours`: groups found by repeatedly moving each
/// colour to its closest centre, starting from centres spread out by brightness.
fn palette(colours: impl Iterator<Item = [u8; 3]>, count: usize) -> Vec<[f64; 3]> {
    let mut all: Vec<[f64; 3]> = colours.map(|c| [c[0] as f64, c[1] as f64, c[2] as f64]).collect();
    if all.is_empty() {
        return vec![[0.0; 3]];
    }
    all.sort_by(|a, b| brightness(*a).total_cmp(&brightness(*b)));
    let k = count.min(all.len());
    let mut centres: Vec<[f64; 3]> = (0..k).map(|i| all[(i * 2 + 1) * all.len() / (k * 2)]).collect();
    for _ in 0..10 {
        let mut sums = vec![([0.0; 3], 0usize); k];
        for c in &all {
            let i = closest(&centres, *c);
            for (s, v) in sums[i].0.iter_mut().zip(c) {
                *s += v;
            }
            sums[i].1 += 1;
        }
        for (centre, (sum, n)) in centres.iter_mut().zip(&sums) {
            if *n > 0 {
                *centre = sum.map(|s| s / *n as f64);
            }
        }
    }
    centres
}

/// 0 to 1, the same every time for the same pixel of the same tile.
fn pixel_hash(x: usize, y: usize, seed: u32) -> f64 {
    let mut h =
        (x as u32).wrapping_mul(0x9e37_79b1) ^ (y as u32).wrapping_mul(0x85eb_ca77) ^ seed.wrapping_mul(0xc2b2_ae3d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    (h >> 8) as f64 / (1u32 << 24) as f64
}

/// Halving a painted tile averages away most of its grain. This wins it back: each colour moves away
/// from the tile's average brightness, keeping its hue, so the pattern of pixels reads at a glance.
fn punchier(palette: Vec<[f64; 3]>, colours: &[[u8; 3]]) -> Vec<[f64; 3]> {
    const SPREAD: f64 = 2.0;
    if colours.is_empty() {
        return palette;
    }
    let average =
        colours.iter().map(|c| brightness([c[0] as f64, c[1] as f64, c[2] as f64])).sum::<f64>() / colours.len() as f64;
    palette
        .into_iter()
        .map(|c| {
            let b = brightness(c);
            let wanted = (average + (b - average) * SPREAD).clamp(0.0, 255.0);
            let by = if b > 0.5 { wanted / b } else { 1.0 };
            c.map(|v| (v * by).clamp(0.0, 255.0))
        })
        .collect()
}

fn brightness(c: [f64; 3]) -> f64 {
    c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11
}

/// The centre a colour is closest to, weighting green as the eye does.
fn closest(centres: &[[f64; 3]], c: [f64; 3]) -> usize {
    let distance =
        |p: &[f64; 3]| 2.0 * (p[0] - c[0]).powi(2) + 4.0 * (p[1] - c[1]).powi(2) + 3.0 * (p[2] - c[2]).powi(2);
    (0..centres.len()).min_by(|&a, &b| distance(&centres[a]).total_cmp(&distance(&centres[b]))).unwrap_or(0)
}

fn nearest(palette: &[[f64; 3]], c: [u8; 3], alpha: u8) -> [u8; 4] {
    let p = palette[closest(palette, [c[0] as f64, c[1] as f64, c[2] as f64])];
    [p[0].round() as u8, p[1].round() as u8, p[2].round() as u8, alpha]
}

/// The cracks that spread across a block being broken: white on clear, one tile per stage.
pub fn crack_tiles() -> Vec<Pixels> {
    let points = scatter(4242, 9);
    (0..CRACK_STAGES)
        .map(|stage| {
            let reach = 2.0 + stage as f64 * 2.1;
            let width = 0.7 + stage as f64 * 0.08;
            each(|x, y| {
                let (d1, d2) = voronoi(&points, x, y);
                let from_middle = ((x as f64 - 15.5).powi(2) + (y as f64 - 15.5).powi(2)).sqrt();
                if d2 - d1 < width && from_middle < reach { [20, 16, 14, 215] } else { [0, 0, 0, 0] }
            })
        })
        .collect()
}

/// A tile with its mip levels, as one run of bytes per level.
pub fn mip_chain(px: &Pixels, pattern: Pattern) -> Vec<Vec<[u8; 4]>> {
    let holes = alpha_means_holes(pattern);
    let n = side(px);
    // Fill the colour under transparent pixels from what is around them, or mips darken the edges
    // of leaves and plants with the black of their holes.
    let mut base = px.clone();
    if holes {
        for _ in 0..4 {
            let copy = base.clone();
            for y in 0..n {
                for x in 0..n {
                    if copy[y * n + x][3] != 0 || base[y * n + x][0..3] != [0, 0, 0] {
                        continue;
                    }
                    let (mut sum, mut count) = ([0u32; 3], 0u32);
                    for (dx, dy) in [(1isize, 0isize), (-1, 0), (0, 1), (0, -1)] {
                        let (nx, ny) = (
                            ((x as isize + dx).rem_euclid(n as isize)) as usize,
                            ((y as isize + dy).rem_euclid(n as isize)) as usize,
                        );
                        let o = copy[ny * n + nx];
                        if o[3] != 0 || o[0..3] != [0, 0, 0] {
                            for c in 0..3 {
                                sum[c] += o[c] as u32;
                            }
                            count += 1;
                        }
                    }
                    if let [Some(r), Some(g), Some(b)] = sum.map(|c| c.checked_div(count)) {
                        let p = &mut base[y * n + x];
                        p[0] = r as u8;
                        p[1] = g as u8;
                        p[2] = b as u8;
                    }
                }
            }
        }
    }
    let mut chain = vec![base];
    let mut size = n;
    while size > 1 {
        let last = chain.last().map_or(&[][..], |v| v.as_slice());
        let half = size / 2;
        let mut next = Vec::with_capacity(half * half);
        for y in 0..half {
            for x in 0..half {
                let (mut sum, mut a) = ([0u32; 3], 0u32);
                for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let p = last[(y * 2 + oy) * size + x * 2 + ox];
                    for c in 0..3 {
                        sum[c] += p[c] as u32;
                    }
                    a += p[3] as u32;
                }
                // Holes survive shrinking: a tree far away stays about as full as one up close.
                let alpha = if holes { ((a as f32 / 4.0 / 255.0).powf(0.6) * 255.0) as u8 } else { (a / 4) as u8 };
                next.push([(sum[0] / 4) as u8, (sum[1] / 4) as u8, (sum[2] / 4) as u8, alpha]);
            }
        }
        chain.push(next);
        size = half;
    }
    chain
}

/// How big an icon is in the UI atlas.
pub const ICON: usize = 32;

fn sample_tile(px: &Pixels, u: f64, v: f64) -> [u8; 4] {
    let (x, y) = ((u * TILE as f64) as usize % TILE, (v * TILE as f64) as usize % TILE);
    px[y * TILE + x]
}

/// A little isometric cube made of a block's three faces, seen from above and to the south-east.
fn cube_icon(top: &Pixels, left: &Pixels, right: &Pixels, tint: [u8; 3], holes: bool) -> Pixels {
    let mut out = vec![[0u8, 0, 0, 0]; ICON * ICON];
    let n = ICON as f64;
    let (cx, top_y, half) = (n / 2.0, 2.0, n / 2.0 - 3.0);
    let (rise, drop) = (half / 2.0, half);
    // A face's alpha is its holes if it has any, and otherwise says where the biome's colour goes.
    let face = |px: &Pixels, u: f64, v: f64, light: f64| -> Option<[u8; 4]> {
        let s = sample_tile(px, u, v);
        if holes && s[3] < 40 {
            return None;
        }
        let tinted = if s[3] > 0 {
            [
                (s[0] as u32 * tint[0] as u32 / 255) as u8,
                (s[1] as u32 * tint[1] as u32 / 255) as u8,
                (s[2] as u32 * tint[2] as u32 / 255) as u8,
            ]
        } else {
            [s[0], s[1], s[2]]
        };
        Some(rgba(shade(tinted, light), 255))
    };
    for y in 0..ICON {
        for x in 0..ICON {
            let (px, py) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - top_y);
            // The top diamond: px = (u - v) * half, py = (u + v) * rise.
            let (a, b) = (px / half, py / rise);
            let (u, v) = ((b + a) / 2.0, (b - a) / 2.0);
            let hit = if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                face(top, u, v, 1.0)
            } else if px < 0.0 {
                // The left side runs down from the diamond's lower left edge.
                let u = px / half + 1.0;
                let v = (py - (rise + u * rise)) / drop;
                ((0.0..1.0).contains(&u) && (0.0..1.0).contains(&v)).then(|| face(left, u, v, 0.74)).flatten()
            } else {
                // The right side runs down from its lower right edge.
                let u = px / half;
                let v = (py - (2.0 * rise - u * rise)) / drop;
                ((0.0..1.0).contains(&u) && (0.0..1.0).contains(&v)).then(|| face(right, u, v, 0.56)).flatten()
            };
            if let Some(c) = hit {
                out[y * ICON + x] = c;
            }
        }
    }
    out
}

fn line(out: &mut Pixels, x0: i32, y0: i32, x1: i32, y1: i32, thick: i32, c: [u8; 3]) {
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    for i in 0..=steps {
        let (x, y) = (x0 + (x1 - x0) * i / steps, y0 + (y1 - y0) * i / steps);
        for dy in 0..thick {
            for dx in 0..thick {
                let (px, py) = (x + dx, y + dy);
                if (0..ICON as i32).contains(&px) && (0..ICON as i32).contains(&py) {
                    out[py as usize * ICON + px as usize] = rgba(c, 255);
                }
            }
        }
    }
}

fn blob(out: &mut Pixels, cx: f64, cy: f64, r: f64, c: [u8; 3], seed: u64) {
    let mut rng = Rng::new(seed);
    for y in 0..ICON {
        for x in 0..ICON {
            let d = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt() + rng.f64() * 1.2;
            if d < r {
                out[y * ICON + x] = rgba(shade(c, 1.25 - d / r * 0.6), 255);
            }
        }
    }
}

const HANDLE: [u8; 3] = [132, 96, 58];

fn fill(out: &mut Pixels, x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 3]) {
    for y in y0..y1 {
        for x in x0..x1 {
            out[y * ICON + x] = rgba(shade(c, 0.9 + ((x + y) % 5) as f64 * 0.04), 255);
        }
    }
}

const WOOD: [u8; 3] = [176, 142, 92];
const WOOD_DARK: [u8; 3] = [124, 96, 58];

/// A fence seen from the side: two posts, joined by two rails.
fn fence_icon() -> Pixels {
    let mut o = vec![[0u8, 0, 0, 0]; ICON * ICON];
    fill(&mut o, 2, 9, 30, 12, WOOD_DARK);
    fill(&mut o, 2, 19, 30, 22, WOOD_DARK);
    fill(&mut o, 6, 3, 11, 30, WOOD);
    fill(&mut o, 21, 3, 26, 30, WOOD);
    o
}

/// A gate: posts at its ends, two bars and a brace between them.
fn gate_icon() -> Pixels {
    let mut o = vec![[0u8, 0, 0, 0]; ICON * ICON];
    fill(&mut o, 2, 4, 6, 28, WOOD);
    fill(&mut o, 26, 4, 30, 28, WOOD);
    fill(&mut o, 6, 9, 26, 12, WOOD_DARK);
    fill(&mut o, 6, 19, 26, 22, WOOD_DARK);
    fill(&mut o, 14, 12, 18, 19, WOOD_DARK);
    o
}

/// A sign on its post, with a few lines on it.
fn sign_icon() -> Pixels {
    let mut o = vec![[0u8, 0, 0, 0]; ICON * ICON];
    fill(&mut o, 14, 18, 18, 30, WOOD_DARK);
    fill(&mut o, 3, 4, 29, 19, WOOD_DARK);
    fill(&mut o, 4, 5, 28, 18, [196, 162, 108]);
    for y in [8, 11, 14] {
        fill(&mut o, 7, y, 25, y + 1, [92, 70, 42]);
    }
    o
}

/// A bed seen from the side: its frame and legs, the quilt over it, the pillow at the head.
fn bed_icon() -> Pixels {
    let mut o = vec![[0u8, 0, 0, 0]; ICON * ICON];
    fill(&mut o, 2, 14, 30, 20, [190, 50, 46]);
    fill(&mut o, 2, 20, 30, 24, [150, 112, 70]);
    fill(&mut o, 2, 24, 5, 28, [120, 86, 50]);
    fill(&mut o, 27, 24, 30, 28, [120, 86, 50]);
    fill(&mut o, 21, 10, 30, 14, [242, 238, 228]);
    o
}

/// Draws an icon for an item that is not a block.
pub fn item_icon(icon: Icon, seed: u64) -> Pixels {
    let mut o = vec![[0u8, 0, 0, 0]; ICON * ICON];
    match icon {
        Icon::Block => {}
        Icon::Stick => line(&mut o, 7, 26, 23, 6, 3, HANDLE),
        Icon::Lump(c) => blob(&mut o, 16.0, 16.0, 11.0, c, seed),
        Icon::Dust(c) => {
            let mut rng = Rng::new(seed);
            for _ in 0..70 {
                let (x, y) = (rng.range(7, 24), rng.range(9, 26));
                let (dx, dy) = (x - 16, y - 18);
                if dx * dx + dy * dy < 80 {
                    o[y as usize * ICON + x as usize] = rgba(shade(c, 0.8 + rng.f64() * 0.5), 255);
                }
            }
        }
        Icon::Ingot(c) => {
            for y in 11..22 {
                let inset = (22 - y) / 3;
                for x in 5 + inset..27 - inset {
                    let tone = if y < 14 {
                        1.2
                    } else if y > 19 {
                        0.72
                    } else {
                        1.0
                    };
                    o[y * ICON + x] = rgba(shade(c, tone), 255);
                }
            }
        }
        Icon::Gem(c) => {
            for y in 6..27i32 {
                let half = if y < 13 { (y - 4) * 2 } else { (27 - y) * 3 / 4 };
                for x in 16 - half..=16 + half {
                    if (0..ICON as i32).contains(&x) {
                        let tone = 0.75 + ((x + y) % 7) as f64 * 0.09;
                        o[y as usize * ICON + x as usize] = rgba(shade(c, tone), 255);
                    }
                }
            }
        }
        Icon::Apple => {
            blob(&mut o, 16.0, 18.0, 10.0, [206, 44, 40], seed);
            line(&mut o, 16, 8, 18, 4, 2, HANDLE);
            line(&mut o, 18, 7, 23, 5, 2, [76, 150, 58]);
        }
        Icon::Meat(c) => {
            blob(&mut o, 14.0, 17.0, 10.0, c, seed);
            line(&mut o, 20, 22, 27, 26, 3, [236, 228, 208]);
        }
        Icon::Brick => {
            for y in 12..21 {
                for x in 6..26 {
                    let tone = if y < 14 {
                        1.15
                    } else if y > 18 {
                        0.75
                    } else {
                        1.0
                    };
                    o[y * ICON + x] = rgba(shade([166, 88, 70], tone), 255);
                }
            }
        }
        Icon::Seeds(c) => {
            let mut rng = Rng::new(seed);
            for _ in 0..9 {
                let (cx, cy) = (rng.range(8, 23) as f64, rng.range(10, 24) as f64);
                for y in 0..ICON {
                    for x in 0..ICON {
                        let (dx, dy) = ((x as f64 - cx) / 2.3, (y as f64 - cy) / 1.5);
                        if dx * dx + dy * dy <= 1.0 {
                            o[y * ICON + x] = rgba(shade(c, 1.15 - dy * 0.3), 255);
                        }
                    }
                }
            }
        }
        Icon::Sheaf(c) => {
            for i in 0..7i32 {
                let top = 16 + (i - 3) * 3;
                line(&mut o, 13 + i, 28, top, 7, 1, shade([118, 150, 64], 0.85 + i as f64 * 0.04));
                blob(&mut o, top as f64, 7.0, 2.4, c, seed + i as u64);
            }
            line(&mut o, 11, 20, 22, 20, 2, [150, 108, 60]);
        }
        Icon::Bread => {
            for y in 9..25 {
                for x in 4..28 {
                    let (dx, dy) = ((x as f64 - 15.5) / 12.0, (y as f64 - 17.0) / 7.5);
                    let d = dx * dx + dy * dy;
                    if d <= 1.0 {
                        let crust = mix([204, 140, 66], [150, 92, 40], (dy + 1.0) / 2.0);
                        o[y * ICON + x] = rgba(shade(crust, 1.15 - d * 0.25), 255);
                    }
                }
            }
            for k in 0..3 {
                line(&mut o, 9 + k * 6, 12, 12 + k * 6, 17, 1, [238, 204, 140]);
            }
        }
        Icon::Armor(slot, c) => {
            let dark = shade(c, 0.68);
            match slot {
                // A helmet: a dome with a brim.
                0 => {
                    for y in 7..20 {
                        for x in 6..26 {
                            let (dx, dy) = ((x as f64 - 15.5) / 10.0, (y as f64 - 19.0) / 12.0);
                            if dx * dx + dy * dy <= 1.0 {
                                o[y * ICON + x] = rgba(shade(c, 1.2 - dy.abs() * 0.4), 255);
                            }
                        }
                    }
                    fill(&mut o, 4, 19, 28, 22, dark);
                }
                // A breastplate: shoulders, the body, a seam down it.
                1 => {
                    fill(&mut o, 5, 6, 13, 12, c);
                    fill(&mut o, 19, 6, 27, 12, c);
                    fill(&mut o, 9, 10, 23, 26, c);
                    fill(&mut o, 15, 12, 17, 26, dark);
                }
                // Greaves: a band at the waist, and a leg each side.
                2 => {
                    fill(&mut o, 8, 6, 24, 10, dark);
                    fill(&mut o, 8, 10, 15, 27, c);
                    fill(&mut o, 17, 10, 24, 27, c);
                }
                // Boots, toes out.
                _ => {
                    fill(&mut o, 7, 13, 13, 26, c);
                    fill(&mut o, 3, 22, 13, 26, dark);
                    fill(&mut o, 19, 13, 25, 26, c);
                    fill(&mut o, 19, 22, 29, 26, dark);
                }
            }
        }
        Icon::Boat(c) => {
            // A hull from the side, wider at the top, and its seat.
            for y in 12..24 {
                let inset = (y - 12) / 2;
                for x in 3 + inset..29 - inset {
                    let rim = y < 14;
                    o[y * ICON + x] = rgba(shade(c, if rim { 0.75 } else { 0.9 + ((x / 5) % 2) as f64 * 0.1 }), 255);
                }
            }
            fill(&mut o, 13, 9, 19, 12, shade(c, 0.7));
        }
        Icon::Cart => {
            fill(&mut o, 4, 9, 28, 22, [132, 134, 140]);
            fill(&mut o, 6, 11, 26, 14, [86, 88, 94]);
            for x in [7, 21] {
                fill(&mut o, x, 21, x + 5, 26, [40, 40, 44]);
            }
        }
        Icon::Bucket(contents) => {
            // A copper pail from the side, wider at the top: its handle over it, its rim, and at
            // the mouth whatever it holds, or the dark inside.
            const COPPER: [u8; 3] = [206, 138, 96];
            for t in 0..40 {
                let a = std::f64::consts::PI * t as f64 / 39.0;
                let (x, y) = ((16.0 - a.cos() * 9.0) as usize, (11.0 - a.sin() * 8.0) as usize);
                fill(&mut o, x, y, x + 1, y + 1, [150, 152, 158]);
            }
            for y in 12..28 {
                let inset = (y - 12) / 5;
                for x in 6 + inset..26 - inset {
                    let band = y == 17 || y == 23;
                    let tone = if band { 0.66 } else { 0.82 + ((x + 2) / 4 % 3) as f64 * 0.12 };
                    o[y * ICON + x] = rgba(shade(COPPER, tone), 255);
                }
            }
            fill(&mut o, 5, 10, 27, 12, shade(COPPER, 1.2));
            fill(&mut o, 7, 11, 25, 13, contents.unwrap_or([52, 34, 26]));
        }
        Icon::Tinderbox => {
            // A small tin with its lid, and a spark struck over it.
            fill(&mut o, 7, 15, 25, 26, [150, 152, 158]);
            fill(&mut o, 6, 13, 26, 16, [112, 114, 120]);
            fill(&mut o, 14, 19, 18, 22, [92, 94, 100]);
            for (x, y) in [(16, 5), (13, 8), (19, 8), (16, 10), (11, 5), (21, 5)] {
                fill(&mut o, x, y, x + 2, y + 2, [255, 214, 92]);
            }
        }
        Icon::Rod => {
            // A pole, its line hanging from the tip, and a float at the end of it.
            line(&mut o, 4, 27, 24, 5, 2, HANDLE);
            line(&mut o, 4, 27, 8, 23, 3, shade(HANDLE, 0.7));
            line(&mut o, 25, 5, 25, 19, 1, [232, 232, 226]);
            fill(&mut o, 23, 19, 27, 21, [238, 236, 228]);
            fill(&mut o, 23, 21, 27, 24, [208, 44, 40]);
        }
        Icon::Fish(c) => {
            // A fish swimming left: its body, a tail, a fin, an eye.
            for y in 8..25 {
                for x in 3..23 {
                    let (dx, dy) = ((x as f64 - 12.5) / 9.5, (y as f64 - 16.0) / 6.0);
                    let d = dx * dx + dy * dy;
                    if d <= 1.0 {
                        let belly = if dy > 0.25 { 1.25 } else { 0.95 - dy * 0.2 };
                        o[y * ICON + x] = rgba(shade(c, belly - d * 0.15), 255);
                    }
                }
            }
            for x in 21..29usize {
                let half = (x - 20) * 7 / 8;
                for y in 16 - half..16 + half {
                    o[y * ICON + x] = rgba(shade(c, 0.72), 255);
                }
            }
            line(&mut o, 10, 9, 15, 6, 2, shade(c, 0.7));
            fill(&mut o, 7, 13, 9, 15, [24, 24, 28]);
        }
        Icon::Arrow => {
            line(&mut o, 7, 25, 23, 9, 2, [168, 132, 86]);
            line(&mut o, 21, 6, 26, 11, 3, [196, 200, 206]);
            line(&mut o, 4, 24, 8, 28, 2, [236, 232, 220]);
            line(&mut o, 6, 22, 10, 26, 2, [236, 232, 220]);
        }
        Icon::Bow => {
            // An arc of wood, and its string straight across.
            for t in 0..48 {
                let a = std::f64::consts::PI * (0.25 + t as f64 / 48.0 * 1.0);
                let (x, y) = ((16.0 + a.cos() * 13.0) as usize, (16.0 - a.sin() * 13.0 + 0.0) as usize);
                if x + 1 < ICON && y + 1 < ICON {
                    fill(&mut o, x, y, x + 2, y + 2, [132, 96, 58]);
                }
            }
            line(&mut o, 25, 7, 7, 25, 1, [230, 226, 214]);
        }
        Icon::Shield(c) => {
            for y in 4..28 {
                for x in 6..26 {
                    let below = y as f64 - 18.0;
                    let taper = if below > 0.0 { below * 0.9 } else { 0.0 };
                    if (x as f64 - 15.5).abs() + taper < 10.0 {
                        let rim = (x as f64 - 15.5).abs() + taper > 8.0 || y < 6;
                        let col = if rim { [92, 70, 44] } else { shade(c, 0.9 + ((x / 4 + y) % 3) as f64 * 0.05) };
                        o[y * ICON + x] = rgba(col, 255);
                    }
                }
            }
            fill(&mut o, 14, 13, 18, 17, [196, 198, 204]);
        }
        Icon::Feather => {
            line(&mut o, 8, 26, 24, 6, 1, [200, 196, 186]);
            for k in 0..10i32 {
                let (x, y) = (10 + k * 3 / 2, 22 - k * 2);
                line(&mut o, x, y, x - 4, y - 3, 1, [240, 240, 234]);
                line(&mut o, x + 1, y, x + 5, y - 1, 1, [226, 226, 220]);
            }
        }
        Icon::Sinew => {
            // A coil of fibre.
            for t in 0..120 {
                let a = t as f64 / 120.0 * std::f64::consts::TAU * 3.0;
                let r = 4.0 + t as f64 / 120.0 * 7.0;
                let (x, y) = ((16.0 + a.cos() * r) as usize, (16.0 + a.sin() * r * 0.8) as usize);
                if x + 1 < ICON && y + 1 < ICON {
                    fill(&mut o, x, y, x + 2, y + 1, [214, 196, 160]);
                }
            }
        }
        Icon::Tool(tool, c) => match tool {
            Tool::Sword => {
                line(&mut o, 7, 25, 12, 20, 3, HANDLE);
                line(&mut o, 7, 18, 14, 25, 2, shade(c, 0.7));
                line(&mut o, 12, 19, 26, 5, 3, c);
                line(&mut o, 14, 19, 26, 7, 1, shade(c, 1.3));
            }
            Tool::Pick => {
                line(&mut o, 6, 27, 22, 9, 3, HANDLE);
                line(&mut o, 11, 5, 19, 5, 3, c);
                line(&mut o, 19, 5, 26, 12, 3, c);
                line(&mut o, 26, 12, 26, 19, 3, c);
            }
            Tool::Axe => {
                line(&mut o, 7, 27, 21, 9, 3, HANDLE);
                for y in 5..17 {
                    for x in 17..28 {
                        if (x as i32 - 19) * (x as i32 - 19) + (y as i32 - 10) * (y as i32 - 10) < 49 {
                            o[y * ICON + x] = rgba(shade(c, 0.85 + (x - 17) as f64 * 0.03), 255);
                        }
                    }
                }
            }
            Tool::Hoe => {
                line(&mut o, 7, 27, 21, 9, 3, HANDLE);
                line(&mut o, 15, 7, 26, 9, 3, c);
                line(&mut o, 25, 9, 26, 15, 3, c);
            }
            Tool::Shovel | Tool::Any => {
                line(&mut o, 7, 27, 20, 12, 3, HANDLE);
                blob(&mut o, 23.0, 9.0, 6.5, c, seed);
            }
        },
    }
    o
}

/// One icon for every item in the registry, in item order.
pub fn all_icons(reg: &Registry, tiles: &[Pixels]) -> Vec<Pixels> {
    reg.items()
        .iter()
        .enumerate()
        .map(|(i, item)| match item.kind {
            zenith_content::ItemKind::Block(kind) if matches!(item.icon, Icon::Block) => {
                let def = &reg.blocks()[kind as usize];
                let tile = |face: usize| &tiles[reg.face_tile(kind, face) as usize];
                match def.shape {
                    zenith_content::Shape::Bed => bed_icon(),
                    zenith_content::Shape::Fence => fence_icon(),
                    zenith_content::Shape::Gate => gate_icon(),
                    zenith_content::Shape::Sign => sign_icon(),
                    // Things that are not cubes show their texture flat.
                    zenith_content::Shape::Cross
                    | zenith_content::Shape::Rail
                    | zenith_content::Shape::Door
                    | zenith_content::Shape::Trapdoor
                    | zenith_content::Shape::Torch
                    | zenith_content::Shape::Ladder
                    | zenith_content::Shape::Flat
                    | zenith_content::Shape::Button => {
                        let t = tile(2);
                        let tint: [u8; 3] = match def.tint {
                            zenith_content::Tint::Grass | zenith_content::Tint::Foliage => [106, 170, 84],
                            _ => [255, 255, 255],
                        };
                        (0..ICON * ICON)
                            .map(|p| {
                                let c = t[p];
                                [
                                    (c[0] as u32 * tint[0] as u32 / 255) as u8,
                                    (c[1] as u32 * tint[1] as u32 / 255) as u8,
                                    (c[2] as u32 * tint[2] as u32 / 255) as u8,
                                    c[3],
                                ]
                            })
                            .collect()
                    }
                    _ => {
                        let tint: [u8; 3] = match def.tint {
                            zenith_content::Tint::Grass | zenith_content::Tint::Foliage => [116, 178, 88],
                            _ => [255, 255, 255],
                        };
                        cube_icon(tile(1), tile(3), tile(5), tint, def.layer != zenith_content::Layer::Opaque)
                    }
                }
            }
            _ => item_icon(item.icon, i as u64 + 1),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_classic_look_paints_hard_two_texel_pixels_in_a_few_colours() {
        let reg = Registry::builtin();
        for t in reg.tiles() {
            let px = pixelate(&paint(t), t.pattern, t.seed);
            assert_eq!(px.len(), TILE * TILE);
            for y in (0..TILE).step_by(2) {
                for x in (0..TILE).step_by(2) {
                    let p = px[y * TILE + x];
                    assert!(
                        [(1, 0), (0, 1), (1, 1)].iter().all(|(ox, oy)| px[(y + oy) * TILE + x + ox] == p),
                        "{:?}: the pixel at {x}, {y} is not one colour",
                        t.pattern
                    );
                }
            }
            let mut colours: Vec<[u8; 3]> = px.iter().filter(|p| p[3] > 0).map(|p| [p[0], p[1], p[2]]).collect();
            colours.sort_unstable();
            colours.dedup();
            assert!(colours.len() <= 6, "{:?} has {} colours", t.pattern, colours.len());
            if alpha_means_holes(t.pattern) {
                assert!(px.iter().all(|p| p[3] == 0 || p[3] >= 128), "{:?} has soft holes", t.pattern);
            }
        }
    }

    #[test]
    fn every_tile_in_the_game_paints_the_right_size_and_is_not_blank() {
        let reg = Registry::builtin();
        for t in reg.tiles() {
            let px = paint(t);
            assert_eq!(px.len(), TILE * TILE, "{:?}", t.pattern);
            assert!(px.iter().any(|p| p[3] > 0), "{:?} is fully clear", t.pattern);
            // The empty description is what kinds with no block yet are given.
            if t.pattern != Pattern::Glass && *t != Tex::default() {
                let distinct: std::collections::HashSet<_> =
                    px.iter().filter(|p| p[3] > 0).map(|p| [p[0], p[1], p[2]]).collect();
                assert!(distinct.len() > 1, "{:?} is one flat colour", t.pattern);
            }
        }
    }

    #[test]
    fn painting_is_repeatable() {
        let reg = Registry::builtin();
        for t in reg.tiles() {
            assert_eq!(paint(t), paint(t));
        }
    }

    #[test]
    fn a_mip_chain_goes_all_the_way_down_to_one_pixel() {
        let t = Tex::new(Pattern::Leaves, [255, 255, 255], [200, 200, 200], 1);
        let chain = mip_chain(&paint(&t), t.pattern);
        assert_eq!(chain.len(), 6);
        assert_eq!(chain[0].len(), TILE * TILE);
        assert_eq!(chain[5].len(), 1);
    }

    #[test]
    fn mips_of_leaves_do_not_fade_their_edges_to_black() {
        let t = Tex::new(Pattern::Leaves, [255, 255, 255], [200, 200, 200], 1);
        let chain = mip_chain(&paint(&t), t.pattern);
        let avg = chain[3].iter().map(|p| p[1] as u32).sum::<u32>() / chain[3].len() as u32;
        assert!(avg > 120, "average brightness {avg}");
    }

    #[test]
    fn every_item_gets_a_visible_icon() {
        let reg = Registry::builtin();
        let tiles: Vec<Pixels> = reg.tiles().iter().map(paint).collect();
        let icons = all_icons(&reg, &tiles);
        assert_eq!(icons.len(), reg.items().len());
        for (icon, item) in icons.iter().zip(reg.items()) {
            assert_eq!(icon.len(), ICON * ICON, "{}", item.name);
            let visible = icon.iter().filter(|p| p[3] > 0).count();
            assert!(visible > 12, "{} has only {visible} pixels", item.name);
        }
    }

    #[test]
    fn materials_lean_where_a_tile_changes_and_lie_flat_where_it_does_not() {
        let flat = Tex::new(Pattern::Noise, [120, 120, 120], [120, 120, 120], 3);
        let m = material(&paint(&flat), &flat);
        assert!(m.iter().all(|p| p[0] == 128 && p[1] == 128), "one colour, no bumps");
        let bricks = Tex::new(Pattern::Bricks, [156, 82, 66], [190, 180, 170], 61);
        let m = material(&paint(&bricks), &bricks);
        let leaning = m.iter().filter(|p| p[0].abs_diff(128) > 12 || p[1].abs_diff(128) > 12).count();
        assert!(leaning > 50, "mortar lines stand out: {leaning}");
        assert!(m.iter().all(|p| p[2] > 150 && p[3] == 0), "brick is rough and no metal");
        let chain = material_chain(&m);
        assert_eq!(chain.len(), 6);
        assert_eq!(chain[5].len(), 1);
    }

    #[test]
    fn metal_ore_and_rails_shine_and_glass_is_smooth() {
        let reg = Registry::builtin();
        let tex = |name: &str| reg.tiles()[reg.face_tile(reg.block_by_name(name).expect("block"), 2) as usize];
        let iron = tex("iron_ore");
        let m = material(&paint(&iron), &iron);
        assert!(m.iter().any(|p| p[3] > 200), "iron in the ore");
        assert!(m.iter().any(|p| p[3] == 0), "and plain stone around it");
        let rail = tex("rail");
        assert!(material(&paint(&rail), &rail).iter().any(|p| p[3] == 255));
        let glass = tex("glass");
        assert!(material(&paint(&glass), &glass).iter().all(|p| p[2] < 40));
    }

    #[test]
    fn cracks_grow_stage_by_stage() {
        let tiles = crack_tiles();
        let covered = |t: &Pixels| t.iter().filter(|p| p[3] > 0).count();
        assert_eq!(tiles.len(), CRACK_STAGES);
        assert!(covered(&tiles[9]) > covered(&tiles[1]));
    }
}
