//! Light as a pure function of the blocks around a chunk.
//!
//! Most engines store light with the blocks and repair it incrementally, which makes seams between
//! chunks and stale values a whole class of bugs. Here a worker solves a chunk's light from a
//! snapshot of the 3x3 chunks around it, each time it meshes. Any light that can reach the middle
//! chunk starts within 15 blocks of it, so the 3x3 holds every source that matters, and the answer
//! cannot depend on the order chunks arrived in.
use crate::chunk::Chunk;
use zenith_content::Lut;
use zenith_core::pos::{MAX_Y, MIN_Y, SECTIONS, WORLD_HEIGHT, cell_index};

/// The solved area is three chunks wide.
pub const AREA: usize = 48;
pub const LAYER: usize = AREA * AREA;
const HEIGHT: usize = WORLD_HEIGHT as usize;

/// Index into a solved volume. `x` and `z` run from -16 to 31 relative to the middle chunk's corner.
#[inline]
pub const fn index(x: i32, y: i32, z: i32) -> usize {
    let (ax, az) = ((x + 16) as usize, (z + 16) as usize);
    ((y - MIN_Y) as usize) * LAYER + az * AREA + ax
}

/// The 3x3 chunks around the one being solved, row by row: `[z][x]`, the middle at `[1][1]`.
pub type Hood<'a> = [[Option<&'a Chunk>; 3]; 3];

/// A solved volume, and the buffers a worker keeps between solves. A cell is `sky << 4 | block`.
pub struct LightVolume {
    pub cells: Vec<u8>,
    opacity: Vec<u8>,
    levels: Vec<u8>,
    block_queue: [Vec<u32>; 16],
    sky_queue: [Vec<u32>; 16],
}

impl Default for LightVolume {
    fn default() -> Self {
        Self {
            cells: vec![0; LAYER * HEIGHT],
            opacity: vec![0; LAYER * HEIGHT],
            levels: vec![0; LAYER],
            block_queue: std::array::from_fn(|_| Vec::new()),
            sky_queue: std::array::from_fn(|_| Vec::new()),
        }
    }
}

impl LightVolume {
    /// Sky and block light packed together; above the world is open sky, below it is dark.
    #[inline]
    pub fn packed(&self, x: i32, y: i32, z: i32) -> u8 {
        if y > MAX_Y {
            0xF0
        } else if y < MIN_Y {
            0
        } else {
            self.cells[index(x, y, z)]
        }
    }
    #[inline]
    pub fn sky(&self, x: i32, y: i32, z: i32) -> u8 {
        self.packed(x, y, z) >> 4
    }
    #[inline]
    pub fn block(&self, x: i32, y: i32, z: i32) -> u8 {
        self.packed(x, y, z) & 15
    }
}

/// Solves the light of the area around `hood`'s middle chunk into `v`. A missing neighbour counts
/// as open sky with no lights in it, which errs on the bright side until it arrives.
pub fn solve(hood: &Hood, lut: &Lut, v: &mut LightVolume) {
    v.opacity.fill(0);
    for q in v.block_queue.iter_mut().chain(v.sky_queue.iter_mut()) {
        q.clear();
    }

    // Flatten opacity and collect the emitters. Sections that hold one value, which is nearly all
    // of the sky and the deep rock, are written in one go.
    let mut top_section = None;
    for (rz, row) in hood.iter().enumerate() {
        for (rx, chunk) in row.iter().enumerate() {
            let Some(chunk) = chunk else { continue };
            for s in 0..SECTIONS {
                let section = &chunk.sections[s];
                let base = s * 16;
                if let Some(value) = section.single() {
                    if value >> 4 == 0 {
                        continue;
                    }
                    top_section = Some(top_section.map_or(s, |t: usize| t.max(s)));
                    let kind = (value >> 4) as usize;
                    let (op, em) = (lut.opacity[kind], lut.emission[kind]);
                    for dy in 0..16usize {
                        for dz in 0..16usize {
                            let start = (base + dy) * LAYER + (rz * 16 + dz) * AREA + rx * 16;
                            v.opacity[start..start + 16].fill(op);
                            if em > 0 {
                                v.block_queue[em as usize].extend((start..start + 16).map(|i| i as u32));
                            }
                        }
                    }
                    continue;
                }
                top_section = Some(top_section.map_or(s, |t: usize| t.max(s)));
                let has_emitter = section.values().iter().any(|&val| lut.emission[(val >> 4) as usize] > 0);
                for dy in 0..16i32 {
                    for dz in 0..16i32 {
                        let start = (base + dy as usize) * LAYER + (rz * 16 + dz as usize) * AREA + rx * 16;
                        for dx in 0..16i32 {
                            let kind = (section.get(cell_index(dx, dy, dz)) >> 4) as usize;
                            v.opacity[start + dx as usize] = lut.opacity[kind];
                            if has_emitter && lut.emission[kind] > 0 {
                                v.block_queue[lut.emission[kind] as usize].push((start + dx as usize) as u32);
                            }
                        }
                    }
                }
            }
        }
    }

    // Open sky down to the first thing in the way, column by column, a layer at a time.
    let top_layer = top_section.map_or(0, |s| (s + 1) * 16).min(HEIGHT);
    v.cells[top_layer * LAYER..].fill(0xF0);
    v.levels.fill(15);
    let (mut lo_layer, mut hi_layer) = (top_layer, 0usize);
    for y in (0..top_layer).rev() {
        let start = y * LAYER;
        let (mut lo, mut hi) = (15u8, 0u8);
        for i in 0..LAYER {
            let l = v.levels[i].saturating_sub(v.opacity[start + i]);
            v.levels[i] = l;
            v.cells[start + i] = l << 4;
            lo = lo.min(l);
            hi = hi.max(l);
        }
        if lo != hi {
            lo_layer = lo_layer.min(y);
            hi_layer = hi_layer.max(y);
        }
    }

    // Sky light only spreads sideways where a column is brighter than the one next to it.
    if lo_layer <= hi_layer {
        for y in lo_layer..=hi_layer {
            let start = y * LAYER;
            for z in 0..AREA {
                for x in 0..AREA {
                    let at = start + z * AREA + x;
                    let l = v.cells[at] >> 4;
                    if l < 2 {
                        continue;
                    }
                    let spreads = (x > 0 && (v.cells[at - 1] >> 4) + 1 < l)
                        || (x + 1 < AREA && (v.cells[at + 1] >> 4) + 1 < l)
                        || (z > 0 && (v.cells[at - AREA] >> 4) + 1 < l)
                        || (z + 1 < AREA && (v.cells[at + AREA] >> 4) + 1 < l);
                    if spreads {
                        v.sky_queue[l as usize].push(at as u32);
                    }
                }
            }
        }
    }
    flood(&mut v.cells, &v.opacity, &mut v.sky_queue, 4);

    for level in 1..16usize {
        for i in 0..v.block_queue[level].len() {
            let at = v.block_queue[level][i] as usize;
            v.cells[at] = (v.cells[at] & 0xF0) | (v.cells[at] & 15).max(level as u8);
        }
    }
    flood(&mut v.cells, &v.opacity, &mut v.block_queue, 0);
}

/// Spreads light outwards from the brightest cells down, so that no cell is worked on twice.
fn flood(cells: &mut [u8], opacity: &[u8], queue: &mut [Vec<u32>; 16], shift: u32) {
    let mask = 15u8 << shift;
    for level in (2..16usize).rev() {
        while let Some(at) = queue[level].pop() {
            let at = at as usize;
            // A cell can be queued at a level it has since been raised above.
            if ((cells[at] & mask) >> shift) as usize != level {
                continue;
            }
            let (x, z) = (at % AREA, (at / AREA) % AREA);
            let neighbours = [
                (x > 0).then(|| at - 1),
                (x + 1 < AREA).then(|| at + 1),
                (z > 0).then(|| at - AREA),
                (z + 1 < AREA).then(|| at + AREA),
                (at >= LAYER).then(|| at - LAYER),
                (at + LAYER < cells.len()).then(|| at + LAYER),
            ];
            for n in neighbours.into_iter().flatten() {
                let op = opacity[n];
                if op >= 15 {
                    continue;
                }
                let reached = (level as u8).saturating_sub(op.max(1));
                if reached > (cells[n] & mask) >> shift {
                    cells[n] = (cells[n] & !mask) | (reached << shift);
                    if reached >= 2 {
                        queue[reached as usize].push(n as u32);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_content::{BlockState, Registry, id};
    use zenith_core::{ChunkPos, Rng};

    fn lut() -> Lut {
        Registry::builtin().lut.clone()
    }

    /// A world of copies of `c`, seen from the middle one.
    fn tiled(c: &Chunk) -> LightVolume {
        let hood: Hood = [[Some(c); 3]; 3];
        let mut v = LightVolume::default();
        solve(&hood, &lut(), &mut v);
        v
    }

    fn floor(chunk: &mut Chunk, y: i32) {
        for x in 0..16 {
            for z in 0..16 {
                chunk.set(x, y, z, BlockState::of(id::STONE));
            }
        }
    }

    #[test]
    fn open_sky_reaches_the_ground_and_not_under_it() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        floor(&mut c, 60);
        let v = tiled(&c);
        assert_eq!(v.sky(5, 61, 5), 15);
        assert_eq!(v.sky(5, 200, 5), 15);
        assert_eq!(v.sky(5, 60, 5), 0);
        assert_eq!(v.sky(5, 59, 5), 0);
    }

    #[test]
    fn a_torch_fades_by_one_per_block_of_walking_distance() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        floor(&mut c, 60);
        floor(&mut c, 70);
        c.set(8, 61, 8, BlockState::of(id::TORCH));
        let v = tiled(&c);
        assert_eq!(v.block(8, 61, 8), 14);
        assert_eq!(v.block(9, 61, 8), 13);
        assert_eq!(v.block(8, 62, 8), 13);
        assert_eq!(v.block(10, 62, 9), 10);
        assert_eq!(v.block(8, 60, 8), 0, "light does not enter stone");
    }

    #[test]
    fn light_goes_around_walls_not_through_them() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        floor(&mut c, 60);
        floor(&mut c, 80);
        // A wall at x = 9, floor to ceiling, with a gap at z = 1.
        for z in (0..16).filter(|&z| z != 1) {
            for y in 61..80 {
                c.set(9, y, z, BlockState::of(id::STONE));
            }
        }
        c.set(8, 61, 8, BlockState::of(id::TORCH));
        let v = tiled(&c);
        assert_eq!(v.block(8, 61, 8), 14);
        assert_eq!(v.block(10, 61, 1), 14 - 9, "around the wall through the gap");
        // Straight through the wall would be 12 here; the way around is 16 steps, too far.
        assert_eq!(v.block(10, 61, 8), 0);
    }

    #[test]
    fn caves_under_an_overhang_get_sky_light_from_the_side() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        floor(&mut c, 60);
        for x in 8..16 {
            for z in 0..16 {
                c.set(x, 62, z, BlockState::of(id::STONE));
            }
        }
        let v = tiled(&c);
        assert_eq!(v.sky(7, 61, 5), 15);
        assert_eq!(v.sky(8, 61, 5), 14);
        assert_eq!(v.sky(11, 61, 5), 11);
    }

    #[test]
    fn water_dims_with_depth() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        floor(&mut c, 40);
        for y in 41..=62 {
            for x in 0..16 {
                for z in 0..16 {
                    c.set(x, y, z, BlockState::of(id::WATER));
                }
            }
        }
        let v = tiled(&c);
        assert_eq!(v.sky(5, 63, 5), 15);
        assert_eq!(v.sky(5, 62, 5), 14);
        assert_eq!(v.sky(5, 58, 5), 10);
        assert_eq!(v.sky(5, 41, 5), 0);
    }

    #[test]
    fn light_crosses_chunk_borders() {
        let mut a = Chunk::empty(ChunkPos::new(0, 0));
        let mut b = Chunk::empty(ChunkPos::new(1, 0));
        for c in [&mut a, &mut b] {
            floor(c, 60);
            floor(c, 70);
        }
        a.set(15, 61, 8, BlockState::of(id::TORCH));
        let mut hood: Hood = [[None; 3]; 3];
        hood[1][1] = Some(&b);
        hood[1][0] = Some(&a);
        let mut v = LightVolume::default();
        solve(&hood, &lut(), &mut v);
        // Solved from b's side; x = -1 is the torch's column in a.
        assert_eq!(v.block(-1, 61, 8), 14);
        assert_eq!(v.block(0, 61, 8), 13);
        assert_eq!(v.block(1, 61, 8), 12);
    }

    /// The defining property: the border cells two neighbouring solves share come out the same,
    /// whichever chunk's point of view solved them. Meshing relies on exactly these cells.
    #[test]
    fn both_sides_of_a_border_agree_on_generated_terrain() {
        let g = crate::Generator::new(11);
        let chunks: Vec<Vec<Chunk>> =
            (-1..=2).map(|z| (-1..=2).map(|x| g.chunk(ChunkPos::new(x, z))).collect()).collect();
        let hood_around = |cx: usize, cz: usize| -> Hood {
            let mut h: Hood = [[None; 3]; 3];
            for rz in 0..3 {
                for rx in 0..3 {
                    h[rz][rx] = chunks.get(cz + rz - 1).and_then(|r| r.get(cx + rx - 1));
                }
            }
            h
        };
        let lut = lut();
        let (mut left, mut right) = (LightVolume::default(), LightVolume::default());
        solve(&hood_around(1, 1), &lut, &mut left);
        solve(&hood_around(2, 1), &lut, &mut right);
        let mut varied = false;
        for y in MIN_Y..250 {
            for z in 0..16 {
                for x in [15, 16] {
                    let l = left.packed(x, y, z);
                    assert_eq!(l, right.packed(x - 16, y, z), "{x},{y},{z}");
                    varied |= l != 0 && l != 0xF0;
                }
            }
        }
        assert!(varied, "the border was all dark or all sky: this compared nothing");
    }

    #[test]
    fn every_lit_cell_on_random_rubble_has_a_reason_to_be_lit() {
        let mut rng = Rng::new(7);
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        for _ in 0..2000 {
            let (x, y, z) = (rng.range(0, 15), rng.range(50, 90), rng.range(0, 15));
            let kind = match rng.below(10) {
                0 => id::TORCH,
                1..=5 => id::STONE,
                6 => id::OAK_LEAVES,
                _ => id::AIR,
            };
            c.set(x, y, z, BlockState::of(kind));
        }
        let v = tiled(&c);
        for y in 50..90 {
            for z in 0..16 {
                for x in 0..16 {
                    let here = v.block(x, y, z);
                    if here > 0 && c.get(x, y, z).kind() != id::TORCH {
                        let neighbours = [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)];
                        let brighter = neighbours.iter().any(|&(dx, dy, dz)| v.block(x + dx, y + dy, z + dz) > here);
                        assert!(brighter, "light at {x},{y},{z} came from nowhere");
                    }
                }
            }
        }
    }

    #[test]
    fn a_light_block_inside_opaque_stone_still_lights_the_room_next_to_it() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        floor(&mut c, 60);
        floor(&mut c, 70);
        c.set(8, 61, 8, BlockState::of(id::GLOW_BLOCK));
        let v = tiled(&c);
        assert_eq!(v.block(8, 61, 8), 15);
        assert_eq!(v.block(9, 61, 8), 14);
    }
}
