//! What gets stamped onto the terrain: ore, trees, ruins.
//!
//! A feature is a list of writes computed from its origin alone. A chunk stamps the features of
//! itself and its eight neighbours and keeps only the writes that land inside it. So a tree that
//! crosses a chunk border comes out the same from both sides, no matter which side is made first,
//! as long as nothing here reaches further than one chunk from its origin: `REACH` holds them to it.
use super::Biome;
use zenith_content::{BlockState, id};
use zenith_core::Rng;

/// No feature may write further than this from its origin, horizontally.
pub const REACH: i32 = 15;

/// Receives the writes of a feature: world coordinates, and whether it may replace solid ground.
pub trait Stamp {
    fn put(&mut self, x: i32, y: i32, z: i32, state: BlockState, force: bool);
    /// Replaces only stone-like ground, for ore.
    fn put_in_rock(&mut self, x: i32, y: i32, z: i32, state: BlockState);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tree {
    Oak,
    BigOak,
    Birch,
    Pine,
    Acacia,
    Jungle,
    Swamp,
    Bush,
}

const fn s(kind: u16) -> BlockState {
    BlockState::of(kind)
}

fn blob(out: &mut impl Stamp, cx: i32, cy: i32, cz: i32, rx: i32, ry: i32, rz: i32, leaf: BlockState, rng: &mut Rng) {
    let (rx2, ry2, rz2) = ((rx * rx).max(1) as f64, (ry * ry).max(1) as f64, (rz * rz).max(1) as f64);
    for dy in -ry..=ry {
        for dz in -rz..=rz {
            for dx in -rx..=rx {
                let d = (dx * dx) as f64 / rx2 + (dy * dy) as f64 / ry2 + (dz * dz) as f64 / rz2;
                // The rim is thinned so a crown never looks like a sphere.
                if d <= 1.0 && !(d > 0.72 && rng.chance(0.4)) {
                    out.put(cx + dx, cy + dy, cz + dz, leaf, false);
                }
            }
        }
    }
}

fn trunk(out: &mut impl Stamp, x: i32, y: i32, z: i32, height: i32, log: BlockState) {
    for dy in 0..height {
        out.put(x, y + dy, z, log, true);
    }
}

/// Grows a tree of the given kind on top of `(x, y - 1, z)`.
pub fn tree(out: &mut impl Stamp, kind: Tree, x: i32, y: i32, z: i32, rng: &mut Rng) {
    match kind {
        Tree::Oak => {
            let h = rng.range(4, 6);
            let (log, leaf) = (s(id::OAK_LOG), s(id::OAK_LEAVES));
            blob(out, x, y + h - 1, z, 3, 2, 3, leaf, rng);
            blob(out, x, y + h + 1, z, 2, 1, 2, leaf, rng);
            trunk(out, x, y, z, h, log);
        }
        Tree::BigOak => {
            let h = rng.range(7, 11);
            let (log, leaf) = (s(id::OAK_LOG), s(id::OAK_LEAVES));
            trunk(out, x, y, z, h, log);
            blob(out, x, y + h, z, 4, 3, 4, leaf, rng);
            for _ in 0..rng.range(3, 5) {
                let (dx, dz) = (rng.range(-4, 4), rng.range(-4, 4));
                let by = y + rng.range(h / 2 + 1, h - 1);
                let steps = dx.abs().max(dz.abs()).max(1);
                for i in 1..=steps {
                    let (bx, bz) = (x + dx * i / steps, z + dz * i / steps);
                    out.put(bx, by + i / 3, bz, log, false);
                }
                blob(out, x + dx, by + steps / 3 + 1, z + dz, 3, 2, 3, leaf, rng);
            }
        }
        Tree::Birch => {
            let h = rng.range(6, 8);
            let (log, leaf) = (s(id::BIRCH_LOG), s(id::BIRCH_LEAVES));
            blob(out, x, y + h - 1, z, 2, 3, 2, leaf, rng);
            trunk(out, x, y, z, h, log);
        }
        Tree::Pine => {
            let h = rng.range(8, 14);
            let (log, leaf) = (s(id::PINE_LOG), s(id::PINE_LEAVES));
            let first = rng.range(2, 3);
            // Tiers, each reaching a little wider than the one above it.
            let (mut radius, mut widest) = (0i32, 1i32);
            for dy in (first..=h + 1).rev() {
                for dz in -radius..=radius {
                    for dx in -radius..=radius {
                        if dx.abs() + dz.abs() <= radius + i32::from(radius > 1) {
                            out.put(x + dx, y + dy, z + dz, leaf, false);
                        }
                    }
                }
                radius += 1;
                if radius > widest {
                    radius = 1;
                    widest = (widest + 1).min(3);
                }
            }
            trunk(out, x, y, z, h, log);
        }
        Tree::Acacia => {
            let h = rng.range(5, 7);
            let (log, leaf) = (s(id::ACACIA_LOG), s(id::ACACIA_LEAVES));
            let dir = [(1, 0), (-1, 0), (0, 1), (0, -1)][rng.below(4) as usize];
            let bend = rng.range(2, 3);
            let (mut tx, mut tz) = (x, z);
            for dy in 0..h {
                if dy >= h - bend {
                    tx += dir.0;
                    tz += dir.1;
                }
                out.put(tx, y + dy, tz, log, true);
            }
            blob(out, tx, y + h, tz, 4, 1, 4, leaf, rng);
            blob(out, tx, y + h + 1, tz, 2, 0, 2, leaf, rng);
        }
        Tree::Jungle => {
            let h = rng.range(10, 16);
            let (log, leaf) = (s(id::OAK_LOG), s(id::ACACIA_LEAVES));
            trunk(out, x, y, z, h, log);
            blob(out, x, y + h, z, 5, 3, 5, leaf, rng);
            blob(out, x, y + h - 4, z, 3, 2, 3, leaf, rng);
            for _ in 0..rng.range(1, 3) {
                let (dx, dz) = (rng.range(-3, 3), rng.range(-3, 3));
                blob(out, x + dx, y + rng.range(h / 2, h - 3), z + dz, 2, 1, 2, leaf, rng);
            }
        }
        Tree::Swamp => {
            let h = rng.range(4, 6);
            let (log, leaf) = (s(id::OAK_LOG), s(id::OAK_LEAVES));
            trunk(out, x, y, z, h, log);
            blob(out, x, y + h, z, 4, 1, 4, leaf, rng);
        }
        Tree::Bush => {
            out.put(x, y, z, s(id::OAK_LOG), true);
            blob(out, x, y + 1, z, 2, 1, 2, s(id::OAK_LEAVES), rng);
        }
    }
}

/// How many tree attempts a chunk of this biome makes, and what grows.
pub fn tree_for(biome: Biome, rng: &mut Rng) -> Option<(u32, Tree)> {
    let roll = rng.f64();
    let pick = match biome {
        Biome::Plains => (1, if roll < 0.15 { Tree::Oak } else { Tree::Bush }),
        Biome::Forest => (
            11,
            if roll < 0.06 {
                Tree::BigOak
            } else if roll < 0.25 {
                Tree::Birch
            } else {
                Tree::Oak
            },
        ),
        Biome::BirchForest => (10, if roll < 0.9 { Tree::Birch } else { Tree::Oak }),
        Biome::Taiga => (9, Tree::Pine),
        Biome::Tundra => (1, Tree::Pine),
        Biome::Savanna => (2, if roll < 0.8 { Tree::Acacia } else { Tree::Bush }),
        Biome::Rainforest => (
            16,
            if roll < 0.35 {
                Tree::Jungle
            } else if roll < 0.7 {
                Tree::Bush
            } else {
                Tree::Oak
            },
        ),
        Biome::Swamp => (4, Tree::Swamp),
        Biome::Mountains => (2, Tree::Pine),
        _ => return None,
    };
    Some(pick)
}

/// A blob of ore as a short random walk of small clusters.
pub fn vein(out: &mut impl Stamp, kind: u16, x: i32, y: i32, z: i32, size: i32, rng: &mut Rng) {
    let (mut px, mut py, mut pz) = (x, y, z);
    let state = BlockState::of(kind);
    let radius = size.min(6);
    for _ in 0..size {
        let (dx, dy, dz) = (rng.range(-1, 1), rng.range(-1, 1), rng.range(-1, 1));
        px = (px + dx).clamp(x - radius, x + radius);
        py = (py + dy).clamp(y - radius, y + radius);
        pz = (pz + dz).clamp(z - radius, z + radius);
        out.put_in_rock(px, py, pz, state);
    }
}

/// A patch of a different rock.
pub fn rock_blob(out: &mut impl Stamp, kind: u16, x: i32, y: i32, z: i32, radius: i32, rng: &mut Rng) {
    let state = BlockState::of(kind);
    let r2 = (radius * radius) as f64;
    for dy in -radius..=radius {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let d = (dx * dx + dy * dy + dz * dz) as f64;
                if d <= r2 && !(d > r2 * 0.65 && rng.chance(0.5)) {
                    out.put_in_rock(x + dx, y + dy, z + dz, state);
                }
            }
        }
    }
}

/// Mossy rocks lying on the ground.
pub fn boulder(out: &mut impl Stamp, x: i32, y: i32, z: i32, rng: &mut Rng) {
    let r = rng.range(1, 3);
    for dy in -1..=r {
        for dz in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy + dz * dz <= r * r + 1 && !rng.chance(0.08) {
                    let kind = if rng.chance(0.35) { id::COBBLE } else { id::STONE };
                    out.put(x + dx, y + dy, z + dz, s(kind), true);
                }
            }
        }
    }
}

/// A broken tower on the surface, with a chest in its base.
pub fn ruin(out: &mut impl Stamp, x: i32, y: i32, z: i32, rng: &mut Rng) {
    let (wall, floor) = (s(id::STONE_BRICK), s(id::COBBLE));
    let r = rng.range(3, 5);
    let height = rng.range(5, 11);
    for dz in -r..=r {
        for dx in -r..=r {
            let edge = dx.abs() == r || dz.abs() == r;
            // A footing so the tower does not hang over a slope.
            for dy in -5..=-1 {
                out.put(x + dx, y + dy, z + dz, if dy == -1 { floor } else { s(id::STONE) }, true);
            }
            let top = height - rng.range(0, 4) - (dx.abs() + dz.abs()) / 3;
            for dy in 0..=height {
                if edge && dy <= top && !rng.chance(0.06) {
                    out.put(x + dx, y + dy, z + dz, wall, true);
                } else {
                    out.put(x + dx, y + dy, z + dz, BlockState::AIR, true);
                }
            }
        }
    }
    // A doorway, always on the south side.
    for dy in 0..3 {
        out.put(x, y + dy, z + r, BlockState::AIR, true);
    }
    out.put(x, y, z, s(id::CHEST), true);
    out.put(x + 1, y, z, s(id::TORCH), true);
}

/// A sealed room underground with a chest, lit by glowing stone.
pub fn vault(out: &mut impl Stamp, x: i32, y: i32, z: i32, rng: &mut Rng) {
    let r = rng.range(3, 4);
    let h = 4;
    for dy in -1..=h {
        for dz in -r..=r {
            for dx in -r..=r {
                let shell = dy == -1 || dy == h || dx.abs() == r || dz.abs() == r;
                let state = if shell {
                    if rng.chance(0.3) { s(id::STONE_BRICK) } else { s(id::COBBLE) }
                } else {
                    BlockState::AIR
                };
                out.put(x + dx, y + dy, z + dz, state, true);
            }
        }
    }
    out.put(x, y + h, z, s(id::GLOW_BLOCK), true);
    out.put(x, y, z, s(id::CHEST), true);
}
