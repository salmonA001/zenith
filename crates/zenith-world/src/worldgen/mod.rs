//! World generation: a pure function from a seed and a chunk position to a chunk.
mod features;
mod shape;

pub use features::REACH;
pub use shape::{Biome, Column, FarSample, foliage_tint, grass_tint, water_tint};

use crate::chunk::Chunk;
use features::Stamp;
use shape::{lerp, smoothstep};
use std::cell::RefCell;
use std::sync::Arc;
use zenith_content::{BlockState, id};
use zenith_core::noise::{Fbm, Perlin};
use zenith_core::palette::{CELLS, Paletted};
use zenith_core::pos::{MAX_Y, MIN_Y, SEA_LEVEL, SECTIONS, WORLD_HEIGHT};
use zenith_core::rng::chunk_seed;
use zenith_core::{ChunkPos, Rng};

/// Bump when any change makes the same seed generate different terrain. A save records the version
/// it was made with, because everything it did not modify is regenerated, not stored.
pub const GENERATOR_VERSION: u32 = 1;

const SALT_TREES: u64 = 0x7472_6565;
const SALT_ORE: u64 = 0x6F72_6573;
const SALT_RUIN: u64 = 0x7275_696E;
const SALT_VAULT: u64 = 0x7661_756C;
const SALT_PLANTS: u64 = 0x706C_616E;
const SALT_ROCKS: u64 = 0x726F_636B;

/// Ruins are spaced on a grid of this many chunks.
const RUIN_GRID: i32 = 12;

pub struct Generator {
    pub seed: u64,
    warp_x: Fbm,
    warp_z: Fbm,
    continent: Fbm,
    erosion: Fbm,
    ridge: Fbm,
    hills: Fbm,
    detail: Fbm,
    river: Fbm,
    temp: Fbm,
    humid: Fbm,
    overhang: Fbm,
    cave_a: Perlin,
    cave_b: Perlin,
    cavern: Perlin,
    entrance: Perlin,
    patch: Perlin,
}

impl Generator {
    pub fn new(seed: u64) -> Self {
        let n = |salt: u64| zenith_core::rng::mix(seed, salt);
        Self {
            seed,
            warp_x: Fbm::new(n(1), 2),
            warp_z: Fbm::new(n(2), 2),
            continent: Fbm::new(n(3), 5),
            erosion: Fbm::new(n(4), 3),
            ridge: Fbm::new(n(5), 4),
            hills: Fbm::new(n(6), 4),
            detail: Fbm::new(n(7), 3),
            river: Fbm::new(n(8), 3),
            temp: Fbm::new(n(9), 3),
            humid: Fbm::new(n(10), 3),
            overhang: Fbm::new(n(11), 3),
            cave_a: Perlin::new(n(12)),
            cave_b: Perlin::new(n(13)),
            cavern: Perlin::new(n(14)),
            entrance: Perlin::new(n(15)),
            patch: Perlin::new(n(16)),
        }
    }

    /// A place to stand near the origin: dry land, not on a cliff.
    pub fn find_spawn(&self) -> (i32, i32, i32) {
        for ring in 0..200i32 {
            let r = ring * 24;
            for step in 0..(1 + ring * 8) {
                let angle_turn = (step as u32 * 65536 / (1 + ring as u32 * 8)) as u16;
                let (s, c) = zenith_core::dmath::sin_cos(angle_turn);
                let (x, z) = ((c * r as f64) as i32, (s * r as f64) as i32);
                let col = self.column(x as f64, z as f64);
                let biome = Self::biome(&col);
                let good = matches!(
                    biome,
                    Biome::Plains | Biome::Forest | Biome::BirchForest | Biome::Savanna | Biome::Taiga
                );
                if good && col.height > SEA_LEVEL as f64 + 2.0 && col.height < 110.0 && col.rugged < 0.1 {
                    return (x, col.height.floor() as i32 + 2, z);
                }
            }
        }
        (0, 120, 0)
    }

    /// The chunk at `pos`. Depends on nothing but the seed and `pos`.
    pub fn chunk(&self, pos: ChunkPos) -> Chunk {
        SCRATCH.with(|s| {
            let mut scratch = s.borrow_mut();
            self.build(pos, &mut scratch)
        })
    }

    fn build(&self, pos: ChunkPos, work: &mut Work) -> Chunk {
        let grid = HeightGrid::new(self, pos);
        work.cells.fill(0);
        let (ox, oz) = (pos.x * 16, pos.z * 16);

        let mut columns = [Col::default(); 256];
        for lz in 0..16 {
            for lx in 0..16 {
                let col = grid.at(ox + lx, oz + lz);
                columns[(lz * 16 + lx) as usize] = Col {
                    col,
                    biome: Self::biome(&col),
                    open: self.cave_ceiling(ox + lx, oz + lz, &col),
                };
            }
        }

        self.fill_terrain(pos, &columns, work);
        self.carve(pos, &columns, work);
        let tops = self.dress_surface(pos, &columns, work);

        let mut stamp = ChunkStamp { x0: ox, z0: oz, cells: &mut work.cells };
        self.stamp_rock(pos, &mut stamp);
        self.stamp_trees(pos, &grid, &mut stamp);
        self.stamp_structures(pos, &grid, &mut stamp);
        self.plant(pos, &columns, &tops, &mut stamp);

        let mut chunk = Chunk::empty(pos);
        for i in 0..SECTIONS {
            let cells = &work.cells[i * CELLS..(i + 1) * CELLS];
            if cells.iter().all(|&c| c == 0) {
                continue;
            }
            chunk.sections[i] = Arc::new(Paletted::from_raw(cells));
        }
        chunk
    }

    /// The highest cell caves may reach in a column.
    fn cave_ceiling(&self, x: i32, z: i32, col: &Column) -> i32 {
        let h = col.height.floor() as i32;
        if h <= SEA_LEVEL + 2 {
            // Never under the sea or a river: that would flood half the underground.
            return h - 14;
        }
        let e = self.entrance.get2(x as f64 / 96.0, z as f64 / 96.0);
        if e > 0.34 { h + 8 } else { h - 7 }
    }

    /// Solid rock up to the surface, with overhangs where the land is rugged, then the sea.
    fn fill_terrain(&self, pos: ChunkPos, columns: &[Col; 256], work: &mut Work) {
        let rock = BlockState::of(id::STONE).0;
        let (ox, oz) = (pos.x * 16, pos.z * 16);
        let max_rugged = columns.iter().map(|c| c.col.rugged).fold(0.0, f64::max);

        for lz in 0..16usize {
            for lx in 0..16usize {
                let h = columns[lz * 16 + lx].col.height.floor() as i32;
                for y in MIN_Y..=h.min(MAX_Y) {
                    work.cells[cell(lx, y, lz)] = rock;
                }
            }
        }

        if max_rugged > 0.02 {
            // Density on a coarse lattice: height minus y, disturbed by 3D noise. Where it says so,
            // rock is added above the surface or air cut below it, which is what makes overhangs.
            let lo = columns.iter().map(|c| c.col.height).fold(f64::MAX, f64::min) as i32 - 40;
            let hi = columns.iter().map(|c| c.col.height).fold(f64::MIN, f64::max) as i32 + 40;
            let (lo, hi) = (lo.max(MIN_Y) & !7, hi.min(MAX_Y));
            let layers = ((hi - lo) / 8 + 2) as usize;
            let mut lattice = vec![0.0f64; 25 * layers];
            for layer in 0..layers {
                let y = lo + layer as i32 * 8;
                for lz in 0..5 {
                    for lx in 0..5 {
                        let (x, z) = ((ox + lx * 4) as f64, (oz + lz * 4) as f64);
                        lattice[layer * 25 + (lz * 5 + lx) as usize] =
                            self.overhang.get3(x / 54.0, y as f64 / 34.0, z / 54.0);
                    }
                }
            }
            for lz in 0..16usize {
                for lx in 0..16usize {
                    let c = &columns[lz * 16 + lx].col;
                    let amp = c.rugged * 34.0;
                    if amp < 0.8 {
                        continue;
                    }
                    let (fx, fz) = (lx as f64 / 4.0, lz as f64 / 4.0);
                    let (ix, iz) = (lx / 4, lz / 4);
                    let (tx, tz) = (fx - ix as f64, fz - iz as f64);
                    for y in (c.height - amp).floor() as i32..=(c.height + amp).ceil() as i32 {
                        if !(lo..hi).contains(&y) {
                            continue;
                        }
                        let layer = ((y - lo) / 8) as usize;
                        let ty = ((y - lo) % 8) as f64 / 8.0;
                        let sample = |ly: usize, x: usize, z: usize| lattice[ly * 25 + z * 5 + x];
                        let bilinear = |ly: usize| {
                            lerp(
                                lerp(sample(ly, ix, iz), sample(ly, ix + 1, iz), tx),
                                lerp(sample(ly, ix, iz + 1), sample(ly, ix + 1, iz + 1), tx),
                                tz,
                            )
                        };
                        let n = lerp(bilinear(layer), bilinear(layer + 1), ty);
                        let solid = c.height - y as f64 + n * amp * 2.2 > 0.0;
                        work.cells[cell(lx, y, lz)] = if solid { rock } else { 0 };
                    }
                }
            }
        }
    }

    /// Tubes where two noise fields both cross zero, caverns where a third runs high.
    fn carve(&self, pos: ChunkPos, columns: &[Col; 256], work: &mut Work) {
        let (ox, oz) = (pos.x * 16, pos.z * 16);
        let top = columns.iter().map(|c| c.open).max().unwrap_or(MIN_Y).min(MAX_Y);
        let layers = (((top - MIN_Y) / 4 + 2) as usize).min(work.field.len() / 25);
        for layer in 0..layers {
            let y = MIN_Y + layer as i32 * 4;
            for lz in 0..5 {
                for lx in 0..5 {
                    let (x, z) = ((ox + lx * 4) as f64, (oz + lz * 4) as f64);
                    let yf = y as f64;
                    let a = self.cave_a.get3(x / 62.0, yf / 38.0, z / 62.0);
                    let b = self.cave_b.get3(x / 62.0, yf / 38.0, z / 62.0);
                    // Tubes widen with depth.
                    let radius = 0.0105 + 0.0085 * smoothstep(40.0, -30.0, yf);
                    let tubes = a * a + b * b - radius;
                    let hall = self.cavern.get3(x / 130.0, yf / 62.0, z / 130.0);
                    let halls = (0.47 + smoothstep(-20.0, 30.0, yf) * 0.4 - hall) * 0.3;
                    work.field[layer * 25 + (lz * 5 + lx) as usize] = tubes.min(halls) as f32;
                }
            }
        }
        for layer in 0..layers.saturating_sub(1) {
            let y0 = MIN_Y + layer as i32 * 4;
            for lz in 0..4usize {
                for lx in 0..4usize {
                    let f = |l: usize, x: usize, z: usize| work.field[l * 25 + z * 5 + x];
                    let corners = [
                        f(layer, lx, lz),
                        f(layer, lx + 1, lz),
                        f(layer, lx, lz + 1),
                        f(layer, lx + 1, lz + 1),
                        f(layer + 1, lx, lz),
                        f(layer + 1, lx + 1, lz),
                        f(layer + 1, lx, lz + 1),
                        f(layer + 1, lx + 1, lz + 1),
                    ];
                    // Most of the underground is far from any cave: skip it in one comparison.
                    if corners.iter().all(|&c| c > 0.0) {
                        continue;
                    }
                    for dy in 0..4usize {
                        let ty = dy as f32 / 4.0;
                        let y = y0 + dy as i32;
                        if y <= MIN_Y + 4 {
                            continue;
                        }
                        for dz in 0..4usize {
                            let tz = dz as f32 / 4.0;
                            for dx in 0..4usize {
                                let tx = dx as f32 / 4.0;
                                let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
                                let v = mix(
                                    mix(mix(corners[0], corners[1], tx), mix(corners[2], corners[3], tx), tz),
                                    mix(mix(corners[4], corners[5], tx), mix(corners[6], corners[7], tx), tz),
                                    ty,
                                );
                                if v < 0.0 {
                                    let (cx, cz) = (lx * 4 + dx, lz * 4 + dz);
                                    if y <= columns[cz * 16 + cx].open {
                                        // The deep caves hold lava lakes.
                                        work.cells[cell(cx, y, cz)] =
                                            if y <= -52 { BlockState::of(id::LAVA).0 } else { 0 };
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Soil, sand and snow on the surface, the sea, deep rock and bedrock. Returns the top of each
    /// column: the highest cell that is not air, before anything is stamped on it.
    fn dress_surface(&self, pos: ChunkPos, columns: &[Col; 256], work: &mut Work) -> [i16; 256] {
        let (ox, oz) = (pos.x * 16, pos.z * 16);
        let mut rng = Rng::new(chunk_seed(self.seed, pos.x, pos.z, SALT_ROCKS));
        let rock = BlockState::of(id::STONE).0;
        let mut tops = [0i16; 256];
        for lz in 0..16usize {
            for lx in 0..16usize {
                let c = &columns[lz * 16 + lx];
                let (x, z) = (ox + lx as i32, oz + lz as i32);
                let h = c.col.height.floor() as i32;
                let scan_top = (h + (c.col.rugged * 34.0) as i32 + 1).min(MAX_Y);

                // The sea fills everything open above the ground, and below the waterline.
                let mut y = scan_top;
                while y > MIN_Y && work.cells[cell(lx, y, lz)] == 0 {
                    y -= 1;
                }
                let ground = y;
                let water = BlockState::of(id::WATER).0;
                for wy in ground + 1..=SEA_LEVEL {
                    work.cells[cell(lx, wy, lz)] = water;
                }

                let slope = c.col.rugged;
                let patch = self.patch.get2(x as f64 / 11.0, z as f64 / 11.0);
                let (top, filler, depth) = surface_blocks(c.biome, ground, &c.col, slope, patch, &mut rng);
                // Every exposed rock face near the top is dressed, under overhangs too.
                let mut d = -1;
                let mut remaining = 0;
                let floor = (ground - 24).max(MIN_Y);
                let mut y = ground;
                while y >= floor {
                    let v = work.cells[cell(lx, y, lz)];
                    if v == rock {
                        if d < 0 {
                            let above = work.cells[cell(lx, (y + 1).min(MAX_Y), lz)];
                            if above == 0 || above == water {
                                d = 0;
                                remaining = depth;
                                work.cells[cell(lx, y, lz)] = if y == ground || above == 0 { top } else { filler };
                            }
                        } else if remaining > 0 {
                            remaining -= 1;
                            work.cells[cell(lx, y, lz)] = filler;
                        } else {
                            d = -1;
                        }
                    } else {
                        d = -1;
                    }
                    y -= 1;
                }

                // Deep rock, with a ragged border.
                let border = rng.range(-3, 3);
                for y in MIN_Y..(border).min(ground) {
                    let v = &mut work.cells[cell(lx, y, lz)];
                    if *v == rock {
                        *v = BlockState::of(id::SLATE).0;
                    }
                }
                // Sandstone under deserts.
                if c.biome == Biome::Desert {
                    for y in (ground - 12).max(MIN_Y)..(ground - 3) {
                        let v = &mut work.cells[cell(lx, y, lz)];
                        if *v == rock {
                            *v = BlockState::of(id::SANDSTONE).0;
                        }
                    }
                }

                // Frozen seas.
                if Self::surface_temperature(&c.col) < -0.48 && ground < SEA_LEVEL {
                    work.cells[cell(lx, SEA_LEVEL, lz)] = BlockState::of(id::ICE).0;
                }

                // Bedrock, rough on its top side.
                work.cells[cell(lx, MIN_Y, lz)] = BlockState::of(id::BEDROCK).0;
                for dy in 1..=4 {
                    if rng.below(5) >= dy as u32 {
                        work.cells[cell(lx, MIN_Y + dy, lz)] = BlockState::of(id::BEDROCK).0;
                    }
                }
                tops[lz * 16 + lx] = ground.max(SEA_LEVEL) as i16;
            }
        }
        tops
    }

    fn stamp_rock(&self, pos: ChunkPos, out: &mut ChunkStamp) {
        // (kind, attempts per chunk, size, lowest y, highest y)
        const ORES: [(u16, u32, i32, i32, i32); 6] = [
            (id::COAL_ORE, 14, 14, -20, 140),
            (id::COPPER_ORE, 9, 10, -16, 96),
            (id::IRON_ORE, 8, 9, -64, 72),
            (id::GOLD_ORE, 3, 8, -64, 28),
            (id::EMBER_ORE, 4, 8, -64, 12),
            (id::CRYSTAL_ORE, 2, 6, -64, -8),
        ];
        for cz in pos.z - 1..=pos.z + 1 {
            for cx in pos.x - 1..=pos.x + 1 {
                let mut rng = Rng::new(chunk_seed(self.seed, cx, cz, SALT_ORE));
                let place = |rng: &mut Rng| (cx * 16 + rng.range(0, 15), cz * 16 + rng.range(0, 15));
                for _ in 0..rng.range(0, 2) {
                    let (x, z) = place(&mut rng);
                    let y = rng.range(-16, 90);
                    features::rock_blob(out, id::GRANITE, x, y, z, rng.range(3, 6), &mut rng);
                }
                if rng.chance(0.3) {
                    let (x, z) = place(&mut rng);
                    let y = rng.range(-50, 60);
                    features::rock_blob(out, id::GRAVEL, x, y, z, rng.range(2, 3), &mut rng);
                }
                for (kind, attempts, size, lo, hi) in ORES {
                    for _ in 0..attempts {
                        let (x, z) = place(&mut rng);
                        let y = rng.range(lo, hi);
                        features::vein(out, kind, x, y, z, size, &mut rng);
                    }
                }
            }
        }
    }

    fn stamp_trees(&self, pos: ChunkPos, grid: &HeightGrid, out: &mut ChunkStamp) {
        for cz in pos.z - 1..=pos.z + 1 {
            for cx in pos.x - 1..=pos.x + 1 {
                let mut rng = Rng::new(chunk_seed(self.seed, cx, cz, SALT_TREES));
                let (mid_x, mid_z) = (cx * 16 + 8, cz * 16 + 8);
                let biome = Self::biome(&grid.at(mid_x, mid_z));
                // Rare rocks in open country: they show as much as a tree.
                if rng.chance(0.03) && matches!(biome, Biome::Plains | Biome::Taiga | Biome::Tundra | Biome::Savanna) {
                    let (x, z) = (cx * 16 + rng.range(0, 15), cz * 16 + rng.range(0, 15));
                    let col = grid.at(x, z);
                    if col.height > SEA_LEVEL as f64 + 1.0 {
                        features::boulder(out, x, col.height.floor() as i32, z, &mut rng);
                    }
                }
                if let Some((attempts, _)) = features::tree_for(biome, &mut rng) {
                    for _ in 0..attempts {
                        let (x, z) = (cx * 16 + rng.range(0, 15), cz * 16 + rng.range(0, 15));
                        let col = grid.at(x, z);
                        // Each tree rolls its own kind, from its own column's biome.
                        let here = Self::biome(&col);
                        let Some((_, kind)) = features::tree_for(here, &mut rng) else { continue };
                        let tree_seed = rng.next_u64();
                        let high_enough = col.height >= SEA_LEVEL as f64 + 1.0 || (here == Biome::Swamp && col.height >= SEA_LEVEL as f64 - 1.0);
                        let open = self.cave_ceiling(x, z, &col) < col.height.floor() as i32;
                        let tree_line = Self::surface_temperature(&col) > -0.95;
                        if high_enough && col.rugged < 0.42 && open && tree_line {
                            let mut tree_rng = Rng::new(tree_seed);
                            features::tree(out, kind, x, col.height.floor() as i32 + 1, z, &mut tree_rng);
                        }
                    }
                }
                // Cacti only ever stand in deserts.
                if biome == Biome::Desert {
                    for _ in 0..rng.range(0, 3) {
                        let (x, z) = (cx * 16 + rng.range(0, 15), cz * 16 + rng.range(0, 15));
                        let col = grid.at(x, z);
                        if Self::biome(&col) == Biome::Desert && col.height > SEA_LEVEL as f64 + 1.0 {
                            let y = col.height.floor() as i32 + 1;
                            for dy in 0..rng.range(1, 3) {
                                out.put(x, y + dy, z, BlockState::of(id::CACTUS), false);
                            }
                        }
                    }
                }
            }
        }
    }

    fn stamp_structures(&self, pos: ChunkPos, grid: &HeightGrid, out: &mut ChunkStamp) {
        // Ruins: at most one per grid cell, on land that can carry it.
        for gz in (pos.z - 1).div_euclid(RUIN_GRID)..=(pos.z + 1).div_euclid(RUIN_GRID) {
            for gx in (pos.x - 1).div_euclid(RUIN_GRID)..=(pos.x + 1).div_euclid(RUIN_GRID) {
                let mut rng = Rng::new(chunk_seed(self.seed, gx, gz, SALT_RUIN));
                let (cx, cz) = (gx * RUIN_GRID + rng.range(0, RUIN_GRID - 1), gz * RUIN_GRID + rng.range(0, RUIN_GRID - 1));
                if (cx - pos.x).abs() > 1 || (cz - pos.z).abs() > 1 {
                    continue;
                }
                let (x, z) = (cx * 16 + rng.range(0, 15), cz * 16 + rng.range(0, 15));
                let col = grid.at(x, z);
                if col.height > SEA_LEVEL as f64 + 2.0 && col.rugged < 0.25 && col.height < 150.0 && rng.chance(0.7) {
                    features::ruin(out, x, col.height.floor() as i32 + 1, z, &mut rng);
                }
            }
        }
        // Vaults: rare, deep and out of sight.
        for cz in pos.z - 1..=pos.z + 1 {
            for cx in pos.x - 1..=pos.x + 1 {
                let mut rng = Rng::new(chunk_seed(self.seed, cx, cz, SALT_VAULT));
                if rng.chance(0.045) {
                    let (x, z) = (cx * 16 + rng.range(0, 15), cz * 16 + rng.range(0, 15));
                    let y = rng.range(-44, 36);
                    if (y as f64) < grid.at(x, z).height - 12.0 {
                        features::vault(out, x, y, z, &mut rng);
                    }
                }
            }
        }
    }

    /// Grass, flowers and snow: the last layer, and only ever inside this chunk.
    fn plant(&self, pos: ChunkPos, columns: &[Col; 256], tops: &[i16; 256], out: &mut ChunkStamp) {
        let (ox, oz) = (pos.x * 16, pos.z * 16);
        let mut rng = Rng::new(chunk_seed(self.seed, pos.x, pos.z, SALT_PLANTS));
        for lz in 0..16usize {
            for lx in 0..16usize {
                let c = &columns[lz * 16 + lx];
                let (x, z) = (ox + lx as i32, oz + lz as i32);
                let roll = rng.f64();
                let flowers = self.patch.get2(x as f64 / 42.0, z as f64 / 42.0);

                // Trees grow above the terrain's top, so look again from a little higher.
                let mut y = (tops[lz * 16 + lx] as i32 + 22).min(MAX_Y - 1);
                let index = |y: i32| cell(lx, y, lz);
                while y > MIN_Y && out.cells[index(y)] == 0 {
                    y -= 1;
                }
                let below = out.cells[index(y)];
                let air = y < MAX_Y && out.cells[index(y + 1)] == 0;
                if !air {
                    continue;
                }
                let kind = BlockState(below).kind();

                if kind == id::GRASS {
                    let choice = match c.biome {
                        Biome::Plains => {
                            if flowers > 0.32 && roll < 0.3 {
                                Some(if flowers > 0.42 { id::POPPY } else { id::DAISY })
                            } else if roll < 0.32 {
                                Some(id::TALL_GRASS)
                            } else {
                                None
                            }
                        }
                        Biome::Forest | Biome::BirchForest => {
                            if flowers > 0.36 && roll < 0.1 {
                                Some(id::BLUEBELL)
                            } else if roll < 0.14 {
                                Some(id::TALL_GRASS)
                            } else if roll < 0.145 {
                                Some(if flowers > 0.0 { id::RED_MUSHROOM } else { id::BROWN_MUSHROOM })
                            } else {
                                None
                            }
                        }
                        Biome::Rainforest => {
                            if roll < 0.28 {
                                Some(id::FERN)
                            } else if roll < 0.42 {
                                Some(id::TALL_GRASS)
                            } else {
                                None
                            }
                        }
                        Biome::Taiga => (roll < 0.1).then_some(id::FERN),
                        Biome::Savanna => (roll < 0.26).then_some(id::TALL_GRASS),
                        Biome::Swamp => (roll < 0.2).then_some(if flowers > 0.1 { id::REEDS } else { id::FERN }),
                        Biome::Beach | Biome::River => (roll < 0.05).then_some(id::REEDS),
                        _ => (roll < 0.05).then_some(id::TALL_GRASS),
                    };
                    if let Some(plant) = choice {
                        out.put(x, y + 1, z, BlockState::of(plant), false);
                    }
                } else if kind == id::SAND && c.biome == Biome::Desert && roll < 0.012 {
                    out.put(x, y + 1, z, BlockState::of(id::DEAD_BUSH), false);
                } else if kind == id::SAND && c.biome == Biome::Beach && roll < 0.015 && y <= SEA_LEVEL + 1 {
                    out.put(x, y + 1, z, BlockState::of(id::REEDS), false);
                }

                // Snow settles on anything solid where it is cold enough.
                if Self::surface_temperature(&c.col) < -0.42 && y >= SEA_LEVEL {
                    let settles = kind != id::WATER && kind != id::ICE && kind != id::SNOW_LAYER && kind != id::CACTUS;
                    if settles && out.cells[index(y + 1)] == 0 {
                        out.put(x, y + 1, z, BlockState::new(id::SNOW_LAYER, rng.below(2) as u8), false);
                    }
                }
            }
        }
    }
}

/// What a column's surface is made of: top block, what lies under it, and how deep that goes.
fn surface_blocks(biome: Biome, ground: i32, col: &Column, slope: f64, patch: f64, rng: &mut Rng) -> (u16, u16, i32) {
    let s = |kind| BlockState::of(kind).0;
    let depth = rng.range(3, 4);
    let underwater = ground < SEA_LEVEL;
    if underwater {
        let t = if ground < SEA_LEVEL - 10 {
            if patch > 0.25 { id::GRAVEL } else if patch < -0.3 { id::CLAY } else { id::SAND }
        } else if patch > 0.3 {
            id::GRAVEL
        } else {
            id::SAND
        };
        return (s(t), s(if t == id::CLAY { id::CLAY } else { id::SAND }), depth);
    }
    match biome {
        Biome::Beach => (s(id::SAND), s(id::SAND), depth + 1),
        Biome::Desert => (s(id::SAND), s(id::SAND), depth + 2),
        Biome::River | Biome::Ocean | Biome::DeepOcean => (s(id::SAND), s(id::SAND), depth),
        Biome::SnowyPeaks => {
            if slope > 0.75 && patch > 0.1 {
                (s(id::STONE), s(id::STONE), 0)
            } else {
                (s(id::SNOW_BLOCK), s(id::STONE), 1)
            }
        }
        Biome::Mountains => {
            let bare = Generator::surface_temperature(col) < -0.25 || slope > 0.62 + patch * 0.3;
            if bare { (s(id::STONE), s(id::STONE), 0) } else { (s(id::GRASS), s(id::DIRT), 2) }
        }
        _ => (s(id::GRASS), s(id::DIRT), depth),
    }
}

/// Per-thread buffers: generation allocates nothing for each chunk.
struct Work {
    cells: Vec<u16>,
    field: Vec<f32>,
}

thread_local! {
    static SCRATCH: RefCell<Work> = RefCell::new(Work {
        cells: vec![0; (WORLD_HEIGHT as usize) * 256],
        field: vec![0.0; 25 * ((WORLD_HEIGHT as usize) / 4 + 2)],
    });
}

#[inline]
fn cell(lx: usize, y: i32, lz: usize) -> usize {
    ((y - MIN_Y) as usize) * 256 + lz * 16 + lx
}

/// One line per column of everything later steps ask about it.
#[derive(Clone, Copy, Default)]
struct Col {
    col: Column,
    biome: Biome,
    /// The highest cell caves may reach.
    open: i32,
}

/// The shape of the world over a chunk and its eight neighbours, sampled every 4 blocks at lattice
/// points fixed in world space. Reading it from any chunk gives the same value for the same place,
/// which is what lets neighbours agree on where a tree stands.
struct HeightGrid {
    x0: i32,
    z0: i32,
    nodes: Vec<Column>,
}

const GRID_NODES: usize = 13;

impl HeightGrid {
    fn new(g: &Generator, pos: ChunkPos) -> Self {
        let (x0, z0) = ((pos.x - 1) * 16, (pos.z - 1) * 16);
        let mut nodes = Vec::with_capacity(GRID_NODES * GRID_NODES);
        for nz in 0..GRID_NODES as i32 {
            for nx in 0..GRID_NODES as i32 {
                nodes.push(g.column((x0 + nx * 4) as f64, (z0 + nz * 4) as f64));
            }
        }
        Self { x0, z0, nodes }
    }

    fn at(&self, x: i32, z: i32) -> Column {
        let (dx, dz) = (x - self.x0, z - self.z0);
        debug_assert!(dx >= 0 && dz >= 0 && dx < 48 && dz < 48, "{dx},{dz}");
        let (ix, iz) = ((dx / 4) as usize, (dz / 4) as usize);
        let (tx, tz) = ((dx % 4) as f64 / 4.0, (dz % 4) as f64 / 4.0);
        let n = |x: usize, z: usize| &self.nodes[z * GRID_NODES + x];
        let (a, b, c, d) = (n(ix, iz), n(ix + 1, iz), n(ix, iz + 1), n(ix + 1, iz + 1));
        let mix = |f: fn(&Column) -> f64| lerp(lerp(f(a), f(b), tx), lerp(f(c), f(d), tx), tz);
        Column {
            height: mix(|c| c.height),
            temperature: mix(|c| c.temperature),
            humidity: mix(|c| c.humidity),
            rugged: mix(|c| c.rugged),
            river: mix(|c| c.river),
        }
    }
}

/// Keeps the writes that land in one chunk.
struct ChunkStamp<'a> {
    x0: i32,
    z0: i32,
    cells: &'a mut [u16],
}

impl ChunkStamp<'_> {
    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (lx, lz) = (x - self.x0, z - self.z0);
        if (0..16).contains(&lx) && (0..16).contains(&lz) && (MIN_Y..=MAX_Y).contains(&y) {
            Some(cell(lx as usize, y, lz as usize))
        } else {
            None
        }
    }
}

impl Stamp for ChunkStamp<'_> {
    #[inline]
    fn put(&mut self, x: i32, y: i32, z: i32, state: BlockState, force: bool) {
        if let Some(i) = self.index(x, y, z) {
            if force || self.cells[i] == 0 {
                self.cells[i] = state.0;
            }
        }
    }
    #[inline]
    fn put_in_rock(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        if let Some(i) = self.index(x, y, z) {
            let kind = BlockState(self.cells[i]).kind();
            if matches!(kind, id::STONE | id::SLATE | id::GRANITE) {
                self.cells[i] = state.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn hash_chunk(c: &Chunk) -> u64 {
        let mut bytes = Vec::new();
        for s in &c.sections {
            let mut raw = vec![0u16; CELLS];
            s.to_raw(&mut raw);
            for v in raw {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        zenith_core::hash::fnv64(&bytes)
    }

    #[test]
    fn generation_order_cannot_change_a_chunk() {
        let g = Generator::new(42);
        let positions: Vec<ChunkPos> =
            (-4..4).flat_map(|x| (-4..4).map(move |z| ChunkPos::new(x * 3, z * 3))).collect();
        let forward: HashMap<_, _> = positions.iter().map(|&p| (p, hash_chunk(&g.chunk(p)))).collect();
        for &p in positions.iter().rev() {
            assert_eq!(hash_chunk(&g.chunk(p)), forward[&p], "{p:?}");
        }
    }

    #[test]
    fn threads_and_separate_generators_agree() {
        let positions: Vec<ChunkPos> = (0..24).map(|i| ChunkPos::new(i * 7 - 50, 31 - i * 5)).collect();
        let g = Generator::new(77);
        let expected: Vec<u64> = positions.iter().map(|&p| hash_chunk(&g.chunk(p))).collect();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let positions = positions.clone();
                std::thread::spawn(move || {
                    let g = Generator::new(77);
                    positions.iter().rev().map(|&p| hash_chunk(&g.chunk(p))).rev().collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().expect("thread"), expected);
        }
    }

    #[test]
    fn a_different_seed_makes_a_different_world() {
        let p = ChunkPos::new(5, 5);
        assert_ne!(hash_chunk(&Generator::new(1).chunk(p)), hash_chunk(&Generator::new(2).chunk(p)));
    }

    /// Trees and ruins must look the same from either side of a chunk border: the writes that land
    /// in a chunk may not depend on anything but their origin.
    #[test]
    fn features_cross_chunk_borders_without_being_cut() {
        let g = Generator::new(1234);
        let (mut logs_at_edge, mut leaves_touching) = (0, 0);
        for cx in -16..16 {
            for cz in -16..16 {
                let (a, b) = (g.chunk(ChunkPos::new(cx, cz)), g.chunk(ChunkPos::new(cx + 1, cz)));
                for z in 0..16 {
                    for y in 50..170 {
                        let (left, right) = (a.get(15, y, z).kind(), b.get(0, y, z).kind());
                        if left == id::OAK_LOG {
                            logs_at_edge += 1;
                        }
                        // A leaf hanging over the edge means the crown's other half is over there.
                        if left == id::OAK_LEAVES && right == id::OAK_LEAVES {
                            leaves_touching += 1;
                        }
                    }
                }
            }
        }
        assert!(logs_at_edge > 0, "no trees near chunk edges in 32x32 chunks");
        assert!(leaves_touching > 0, "crowns are being cut at chunk borders");
    }

    #[test]
    fn bedrock_floors_the_world_and_the_sea_is_level() {
        let g = Generator::new(8);
        let mut saw_water = false;
        for x in -8..8 {
            for z in -8..8 {
                let c = g.chunk(ChunkPos::new(x * 40, z * 40));
                assert_eq!(c.get(3, MIN_Y, 3).kind(), id::BEDROCK);
                // Above the waterline water only exists in carved-out flows, which we do not make.
                assert_ne!(c.get(3, SEA_LEVEL + 1, 3).kind(), id::WATER);
                saw_water |= c.get(3, SEA_LEVEL, 3).kind() == id::WATER;
            }
        }
        assert!(saw_water);
    }

    #[test]
    fn caves_and_ore_exist_underground() {
        let g = Generator::new(3);
        let (mut air, mut total, mut coal, mut iron, mut crystal, mut lava) = (0u64, 0u64, 0, 0, 0, 0);
        for x in -8..8 {
            for z in -8..8 {
                let c = g.chunk(ChunkPos::new(x * 4, z * 4));
                for lx in 0..16 {
                    for lz in 0..16 {
                        for y in -60..30 {
                            total += 1;
                            match c.get(lx, y, lz).kind() {
                                id::AIR => air += 1,
                                id::COAL_ORE => coal += 1,
                                id::IRON_ORE => iron += 1,
                                id::CRYSTAL_ORE => crystal += 1,
                                id::LAVA => lava += 1,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        let open = air as f64 / total as f64;
        assert!((0.02..0.35).contains(&open), "underground is {open:.3} air");
        assert!(coal > 0 && iron > 0 && crystal > 0 && lava > 0, "{coal} {iron} {crystal} {lava}");
    }

    #[test]
    fn spawn_is_on_dry_land() {
        for seed in 0..8 {
            let g = Generator::new(seed);
            let (x, y, z) = g.find_spawn();
            let c = g.chunk(ChunkPos::new(x >> 4, z >> 4));
            let top = c.top(x & 15, z & 15, |s| s.kind() != id::AIR && s.kind() != id::TALL_GRASS);
            assert!(top.expect("the column has ground") >= SEA_LEVEL - 1, "seed {seed}");
            assert!(y > SEA_LEVEL);
        }
    }

    /// Worlds are regenerated from the seed, not stored, so terrain must never change by accident.
    /// If this fails after a deliberate change to generation, bump `GENERATOR_VERSION` too.
    #[test]
    fn terrain_is_pinned_to_this_generator_version() {
        let g = Generator::new(2026);
        let digest = [(0, 0), (40, -17), (-300, 251), (1000, 1000)]
            .iter()
            .fold(0u64, |acc, &(x, z)| acc.rotate_left(7) ^ hash_chunk(&g.chunk(ChunkPos::new(x, z))));
        assert_eq!(GENERATOR_VERSION, 1);
        assert_eq!(digest, GOLDEN_TERRAIN, "terrain changed: got {digest:#x}");
    }

    const GOLDEN_TERRAIN: u64 = 0x4238ccbfba91c43a;
}
