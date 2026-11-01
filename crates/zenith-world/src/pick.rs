//! Finding the block under the crosshair.
use crate::chunk::BlockAccess;
use zenith_content::{BlockState, Registry, Shape};
use zenith_core::glam::DVec3;
use zenith_core::{Aabb, BlockPos, Dir};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub pos: BlockPos,
    pub state: BlockState,
    pub face: Dir,
    /// How far along the ray, in blocks.
    pub distance: f64,
    pub point: DVec3,
}

/// Walks the ray block by block and returns the first thing it hits. Fluids are not targets.
pub fn pick(w: &impl BlockAccess, reg: &Registry, origin: DVec3, dir: DVec3, reach: f64) -> Option<Hit> {
    let dir = dir.normalize_or_zero();
    if dir == DVec3::ZERO {
        return None;
    }
    let mut cell = BlockPos::from_vec(origin);
    let step = [
        if dir.x > 0.0 { 1 } else { -1 },
        if dir.y > 0.0 { 1 } else { -1 },
        if dir.z > 0.0 { 1 } else { -1 },
    ];
    let boundary = |o: f64, d: f64, c: i32| -> f64 {
        if d == 0.0 {
            f64::INFINITY
        } else if d > 0.0 {
            (c as f64 + 1.0 - o) / d
        } else {
            (c as f64 - o) / d
        }
    };
    let mut t_next = [boundary(origin.x, dir.x, cell.x), boundary(origin.y, dir.y, cell.y), boundary(origin.z, dir.z, cell.z)];
    let t_delta = [
        if dir.x != 0.0 { 1.0 / dir.x.abs() } else { f64::INFINITY },
        if dir.y != 0.0 { 1.0 / dir.y.abs() } else { f64::INFINITY },
        if dir.z != 0.0 { 1.0 / dir.z.abs() } else { f64::INFINITY },
    ];
    loop {
        if let Some(hit) = test_cell(w, reg, cell, origin, dir, reach) {
            return Some(hit);
        }
        let axis = if t_next[0] < t_next[1] {
            if t_next[0] < t_next[2] { 0 } else { 2 }
        } else if t_next[1] < t_next[2] {
            1
        } else {
            2
        };
        if t_next[axis] > reach {
            return None;
        }
        t_next[axis] += t_delta[axis];
        match axis {
            0 => cell.x += step[0],
            1 => cell.y += step[1],
            _ => cell.z += step[2],
        }
    }
}

fn test_cell(w: &impl BlockAccess, reg: &Registry, cell: BlockPos, origin: DVec3, dir: DVec3, reach: f64) -> Option<Hit> {
    let state = w.block(cell);
    if state.is_air() {
        return None;
    }
    let def = reg.block(state);
    if def.shape == Shape::Fluid || def.shape == Shape::Empty {
        return None;
    }
    let base = DVec3::new(cell.x as f64, cell.y as f64, cell.z as f64);
    let mut best: Option<Hit> = None;
    for b in def.shape.outline(state.meta()) {
        let bx = Aabb::new(
            base + DVec3::new(b[0][0] as f64, b[0][1] as f64, b[0][2] as f64) / 16.0,
            base + DVec3::new(b[1][0] as f64, b[1][1] as f64, b[1][2] as f64) / 16.0,
        );
        if let Some((t, face)) = bx.ray(origin, dir) {
            if t <= reach && best.is_none_or(|h| t < h.distance) {
                best = Some(Hit { pos: cell, state, face, distance: t, point: origin + dir * t });
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunk, ChunkMap};
    use zenith_content::id;
    use zenith_core::ChunkPos;

    fn world() -> ChunkMap {
        let mut map = ChunkMap::default();
        map.chunks.insert(ChunkPos::new(0, 0), Chunk::empty(ChunkPos::new(0, 0)));
        map.set(BlockPos::new(5, 70, 5), BlockState::of(id::STONE));
        map
    }

    #[test]
    fn hits_the_near_face_of_the_block_in_the_way() {
        let (w, reg) = (world(), Registry::builtin());
        let hit = pick(&w, &reg, DVec3::new(5.5, 70.5, 1.0), DVec3::Z, 6.0).expect("hit");
        assert_eq!(hit.pos, BlockPos::new(5, 70, 5));
        assert_eq!(hit.face, Dir::North);
        assert!((hit.distance - 4.0).abs() < 1e-9);

        let hit = pick(&w, &reg, DVec3::new(5.5, 75.0, 5.5), DVec3::NEG_Y, 6.0).expect("hit");
        assert_eq!(hit.face, Dir::Up);
    }

    #[test]
    fn reach_is_a_hard_limit() {
        let (w, reg) = (world(), Registry::builtin());
        assert!(pick(&w, &reg, DVec3::new(5.5, 70.5, 1.0), DVec3::Z, 3.9).is_none());
    }

    #[test]
    fn a_ray_that_misses_finds_nothing() {
        let (w, reg) = (world(), Registry::builtin());
        assert!(pick(&w, &reg, DVec3::new(0.5, 70.5, 1.0), DVec3::Z, 6.0).is_none());
    }

    #[test]
    fn slabs_can_be_missed_through_their_empty_half() {
        let mut w = world();
        let reg = Registry::builtin();
        let p = BlockPos::new(8, 70, 8);
        w.set(p, BlockState::of(id::STONE_SLAB));
        assert!(pick(&w, &reg, DVec3::new(8.5, 70.75, 6.0), DVec3::Z, 6.0).is_none(), "over the slab");
        let hit = pick(&w, &reg, DVec3::new(8.5, 70.25, 6.0), DVec3::Z, 6.0).expect("into the slab");
        assert_eq!(hit.pos, p);
    }

    #[test]
    fn water_does_not_stop_the_crosshair() {
        let mut w = world();
        let reg = Registry::builtin();
        for z in 2..5 {
            w.set(BlockPos::new(5, 70, z), BlockState::of(id::WATER));
        }
        let hit = pick(&w, &reg, DVec3::new(5.5, 70.5, 1.0), DVec3::Z, 6.0).expect("hit");
        assert_eq!(hit.pos, BlockPos::new(5, 70, 5));
    }

    #[test]
    fn diagonal_rays_visit_the_right_cells() {
        let (w, reg) = (world(), Registry::builtin());
        let origin = DVec3::new(2.5, 67.5, 2.5);
        let hit = pick(&w, &reg, origin, DVec3::new(1.0, 1.0, 1.0), 10.0).expect("hit");
        assert_eq!(hit.pos, BlockPos::new(5, 70, 5));
    }
}
