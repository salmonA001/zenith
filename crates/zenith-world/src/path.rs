//! A* over the blocks a body can stand in. Mobs and autopilots share it.
use crate::chunk::BlockAccess;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use zenith_content::{Fluid, Registry};
use zenith_core::{BlockPos, FxMap};

/// Whether a two-tall body fits at `p` with its feet there.
pub fn fits(w: &impl BlockAccess, reg: &Registry, p: BlockPos) -> bool {
    let (feet, head) = (w.block(p), w.block(p.add(0, 1, 0)));
    w.is_loaded(p) && open(reg, feet) && open(reg, head)
}

fn open(reg: &Registry, state: zenith_content::BlockState) -> bool {
    let def = reg.block(state);
    !reg.lut.collides[state.kind() as usize] && def.fluid != Fluid::Lava && def.contact_damage == 0
}

/// Whether a block stands taller than its cell, as a fence does: nothing steps or jumps up onto it.
fn too_tall(reg: &Registry, state: zenith_content::BlockState) -> bool {
    reg.block(state).shape.collision(state.meta()).iter().any(|b| b[1][1] > 16)
}

/// Whether a body can stay at `p`: it fits, and something holds it up.
pub fn can_stand(w: &impl BlockAccess, reg: &Registry, p: BlockPos) -> bool {
    if !fits(w, reg, p) {
        return false;
    }
    let (here, below) = (w.block(p), w.block(p.add(0, -1, 0)));
    let (d_here, d_below) = (reg.block(here), reg.block(below));
    (reg.lut.collides[below.kind() as usize] && d_below.contact_damage == 0 && !too_tall(reg, below))
        || d_here.fluid == Fluid::Water
        || d_below.fluid == Fluid::Water
        || d_here.climbable
}

fn neighbours(w: &impl BlockAccess, reg: &Registry, p: BlockPos, out: &mut Vec<BlockPos>) {
    out.clear();
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let flat = p.add(dx, 0, dz);
        if can_stand(w, reg, flat) {
            out.push(flat);
            continue;
        }
        // One up, if there is headroom to jump.
        let up = flat.add(0, 1, 0);
        if fits(w, reg, p.add(0, 2, 0)) && can_stand(w, reg, up) {
            out.push(up);
            continue;
        }
        // Down a drop of up to three, if the way over the edge is clear.
        if fits(w, reg, flat) {
            for drop in 1..=3 {
                let down = flat.add(0, -drop, 0);
                if can_stand(w, reg, down) {
                    out.push(down);
                    break;
                }
                if !fits(w, reg, down) {
                    break;
                }
            }
        }
    }
    // Ladders and water go straight up and down.
    let def = reg.block(w.block(p));
    if def.climbable || def.fluid == Fluid::Water {
        for dy in [1, -1] {
            let v = p.add(0, dy, 0);
            if can_stand(w, reg, v) {
                out.push(v);
            }
        }
    }
}

fn cost_to(a: BlockPos, b: BlockPos) -> i32 {
    a.manhattan(b)
}

/// The cells from `start` (not included) to `goal`. If `goal` cannot be reached within `max_nodes`
/// cells of searching, the path ends at the cell that got closest, or is `None` if that is `start`.
pub fn find(
    w: &impl BlockAccess,
    reg: &Registry,
    start: BlockPos,
    goal: BlockPos,
    max_nodes: usize,
) -> Option<Vec<BlockPos>> {
    if start == goal {
        return Some(Vec::new());
    }
    let mut open_set = BinaryHeap::new();
    // (came from, cost so far)
    let mut seen: FxMap<BlockPos, (BlockPos, i32)> = FxMap::default();
    seen.insert(start, (start, 0));
    open_set.push(Reverse((cost_to(start, goal), 0i32, start)));
    let (mut best, mut best_h) = (start, cost_to(start, goal));
    let mut next = Vec::with_capacity(6);
    let mut visited = 0;
    while let Some(Reverse((_, g, p))) = open_set.pop() {
        if p == goal {
            best = goal;
            break;
        }
        if seen.get(&p).is_some_and(|&(_, known)| known < g) {
            continue;
        }
        visited += 1;
        if visited > max_nodes {
            break;
        }
        neighbours(w, reg, p, &mut next);
        for &n in &next {
            let step = 1 + (n.y - p.y).abs();
            let tentative = g + step;
            if seen.get(&n).is_none_or(|&(_, known)| tentative < known) {
                seen.insert(n, (p, tentative));
                let h = cost_to(n, goal);
                if h < best_h {
                    best_h = h;
                    best = n;
                }
                open_set.push(Reverse((tentative + h, tentative, n)));
            }
        }
    }
    if best == start {
        return None;
    }
    let mut path = vec![best];
    let mut at = best;
    while let Some(&(from, _)) = seen.get(&at) {
        if from == start {
            break;
        }
        path.push(from);
        at = from;
    }
    path.reverse();
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunk, ChunkMap};
    use zenith_content::{BlockState, id};
    use zenith_core::ChunkPos;

    fn flat() -> ChunkMap {
        let mut map = ChunkMap::default();
        for cx in -2..3 {
            for cz in -2..3 {
                let mut c = Chunk::empty(ChunkPos::new(cx, cz));
                for x in 0..16 {
                    for z in 0..16 {
                        c.set(x, 60, z, BlockState::of(id::STONE));
                    }
                }
                map.chunks.insert(c.pos, c);
            }
        }
        map
    }

    fn walls(map: &mut ChunkMap, cells: impl Iterator<Item = (i32, i32)>, height: i32) {
        for (x, z) in cells {
            for y in 61..61 + height {
                map.set(BlockPos::new(x, y, z), BlockState::of(id::STONE));
            }
        }
    }

    #[test]
    fn a_straight_walk_takes_the_short_way() {
        let (w, reg) = (flat(), Registry::builtin());
        let path = find(&w, &reg, BlockPos::new(0, 61, 0), BlockPos::new(5, 61, 0), 500).expect("path");
        assert_eq!(path.len(), 5);
        assert_eq!(*path.last().expect("end"), BlockPos::new(5, 61, 0));
    }

    #[test]
    fn it_goes_around_a_wall_too_high_to_jump() {
        let mut w = flat();
        walls(&mut w, (-6..=6).map(|z| (3, z)), 3);
        let reg = Registry::builtin();
        let path = find(&w, &reg, BlockPos::new(0, 61, 0), BlockPos::new(6, 61, 0), 5000).expect("path");
        assert_eq!(*path.last().expect("end"), BlockPos::new(6, 61, 0));
        assert!(path.iter().all(|p| p.x != 3 || p.z.abs() > 6), "went through the wall: {path:?}");
    }

    #[test]
    fn it_climbs_a_one_block_step_rather_than_walking_around() {
        let mut w = flat();
        walls(&mut w, (-20..=20).map(|z| (3, z)), 1);
        let reg = Registry::builtin();
        let path = find(&w, &reg, BlockPos::new(0, 61, 0), BlockPos::new(6, 61, 0), 500).expect("path");
        assert!(path.len() <= 8, "{path:?}");
        assert!(path.contains(&BlockPos::new(3, 62, 0)));
    }

    #[test]
    fn it_will_not_walk_through_lava() {
        let mut w = flat();
        for z in -40i32..=40 {
            for x in 2..=4 {
                if z.abs() < 32 {
                    w.set(BlockPos::new(x, 61, z), BlockState::of(id::LAVA));
                }
            }
        }
        let reg = Registry::builtin();
        // Too narrow a search to get around, and no way across: the closest it can get is the shore.
        let path = find(&w, &reg, BlockPos::new(0, 61, 0), BlockPos::new(6, 61, 0), 200).expect("partial");
        assert!(path.iter().all(|p| p.x < 2 || p.x > 4), "{path:?}");
    }

    #[test]
    fn an_unreachable_goal_gets_as_close_as_it_can() {
        let mut w = flat();
        // A sealed box around the goal.
        walls(
            &mut w,
            (4..=8).flat_map(|x| (-2i32..=2).map(move |z| (x, z))).filter(|&(x, z)| x == 4 || x == 8 || z.abs() == 2),
            4,
        );
        let reg = Registry::builtin();
        let path = find(&w, &reg, BlockPos::new(0, 61, 0), BlockPos::new(6, 61, 0), 300).expect("partial");
        assert_eq!(*path.last().expect("end"), BlockPos::new(3, 61, 0));
    }

    #[test]
    fn nowhere_to_go_is_none() {
        let mut w = flat();
        walls(&mut w, [(1, 0), (-1, 0), (0, 1), (0, -1)].into_iter(), 3);
        let reg = Registry::builtin();
        assert!(find(&w, &reg, BlockPos::new(0, 61, 0), BlockPos::new(9, 61, 0), 300).is_none());
    }

    #[test]
    fn it_never_paths_into_a_chunk_that_is_not_loaded() {
        let (w, reg) = (flat(), Registry::builtin());
        let path = find(&w, &reg, BlockPos::new(0, 61, 0), BlockPos::new(500, 61, 0), 5000).expect("partial");
        assert!(path.iter().all(|p| w.is_loaded(*p)));
    }
}
