//! World coordinates.
use glam::DVec3;
use serde::{Deserialize, Serialize};

pub const CHUNK_SIZE: i32 = 16;
pub const MIN_Y: i32 = -64;
pub const MAX_Y: i32 = 319;
pub const WORLD_HEIGHT: i32 = MAX_Y - MIN_Y + 1;
pub const SECTIONS: usize = (WORLD_HEIGHT / 16) as usize;
pub const SEA_LEVEL: i32 = 62;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

impl ChunkPos {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }
    /// Chebyshev distance in chunks: the shape of a view radius.
    pub fn distance(self, other: Self) -> i32 {
        (self.x - other.x).abs().max((self.z - other.z).abs())
    }
    /// Squared distance in chunks, for closest-first ordering.
    pub fn distance_sq(self, other: Self) -> i64 {
        let (dx, dz) = ((self.x - other.x) as i64, (self.z - other.z) as i64);
        dx * dx + dz * dz
    }
    pub fn min_block(self) -> BlockPos {
        BlockPos::new(self.x << 4, MIN_Y, self.z << 4)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
    pub fn from_vec(v: DVec3) -> Self {
        Self::new(v.x.floor() as i32, v.y.floor() as i32, v.z.floor() as i32)
    }
    pub const fn chunk(self) -> ChunkPos {
        ChunkPos::new(self.x >> 4, self.z >> 4)
    }
    pub const fn add(self, dx: i32, dy: i32, dz: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }
    pub fn offset(self, d: Dir) -> Self {
        let [dx, dy, dz] = d.offset();
        self.add(dx, dy, dz)
    }
    pub fn center(self) -> DVec3 {
        DVec3::new(self.x as f64 + 0.5, self.y as f64 + 0.5, self.z as f64 + 0.5)
    }
    pub const fn in_world(self) -> bool {
        self.y >= MIN_Y && self.y <= MAX_Y
    }
    pub fn manhattan(self, o: Self) -> i32 {
        (self.x - o.x).abs() + (self.y - o.y).abs() + (self.z - o.z).abs()
    }
    /// This position's cell index inside its 16^3 section.
    pub const fn cell(self) -> usize {
        cell_index(self.x & 15, self.y & 15, self.z & 15)
    }
    /// This position's section index inside its chunk, if it is in the world.
    pub const fn section(self) -> Option<usize> {
        if self.in_world() {
            Some(((self.y - MIN_Y) >> 4) as usize)
        } else {
            None
        }
    }
}

/// The layout of a section: y major, then z, then x.
#[inline]
pub const fn cell_index(x: i32, y: i32, z: i32) -> usize {
    ((y << 8) | (z << 4) | x) as usize
}

/// The six faces of a block. North is -z, east is +x.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Dir {
    Down = 0,
    Up = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

impl Dir {
    pub const ALL: [Dir; 6] = [Dir::Down, Dir::Up, Dir::North, Dir::South, Dir::West, Dir::East];
    pub const HORIZONTAL: [Dir; 4] = [Dir::North, Dir::South, Dir::West, Dir::East];

    pub const fn offset(self) -> [i32; 3] {
        match self {
            Dir::Down => [0, -1, 0],
            Dir::Up => [0, 1, 0],
            Dir::North => [0, 0, -1],
            Dir::South => [0, 0, 1],
            Dir::West => [-1, 0, 0],
            Dir::East => [1, 0, 0],
        }
    }
    pub const fn opposite(self) -> Dir {
        match self {
            Dir::Down => Dir::Up,
            Dir::Up => Dir::Down,
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::West => Dir::East,
            Dir::East => Dir::West,
        }
    }
    pub const fn from_u8(v: u8) -> Option<Dir> {
        match v {
            0 => Some(Dir::Down),
            1 => Some(Dir::Up),
            2 => Some(Dir::North),
            3 => Some(Dir::South),
            4 => Some(Dir::West),
            5 => Some(Dir::East),
            _ => None,
        }
    }
    pub const fn is_horizontal(self) -> bool {
        !matches!(self, Dir::Down | Dir::Up)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_coordinates_land_in_the_right_chunk_and_cell() {
        let p = BlockPos::new(-1, -64, -17);
        assert_eq!(p.chunk(), ChunkPos::new(-1, -2));
        assert_eq!(p.section(), Some(0));
        assert_eq!(p.cell(), cell_index(15, 0, 15));
    }

    #[test]
    fn positions_outside_the_world_have_no_section() {
        assert_eq!(BlockPos::new(0, MIN_Y - 1, 0).section(), None);
        assert_eq!(BlockPos::new(0, MAX_Y + 1, 0).section(), None);
        assert_eq!(BlockPos::new(0, MAX_Y, 0).section(), Some(SECTIONS - 1));
    }

    #[test]
    fn opposite_faces_cancel_out() {
        for d in Dir::ALL {
            let p = BlockPos::new(3, 4, 5);
            assert_eq!(p.offset(d).offset(d.opposite()), p);
        }
    }
}
