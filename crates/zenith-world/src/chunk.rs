//! Chunks: columns of sections that share memory until they are written to.
use std::sync::{Arc, OnceLock};
use zenith_content::BlockState;
use zenith_core::palette::{DecodeError, Paletted};
use zenith_core::pos::{MAX_Y, MIN_Y, SECTIONS};
use zenith_core::{BlockPos, ChunkPos, FxMap};

fn empty_section() -> Arc<Paletted> {
    static EMPTY: OnceLock<Arc<Paletted>> = OnceLock::new();
    EMPTY.get_or_init(|| Arc::new(Paletted::filled(0))).clone()
}

/// Cloning a chunk clones 24 `Arc`s: a snapshot for a worker thread costs next to nothing, and a
/// write copies only the section it lands in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub pos: ChunkPos,
    pub sections: [Arc<Paletted>; SECTIONS],
    /// Bit `i`: section `i` differs from what the generator makes. Only these are saved or sent.
    pub modified: u32,
}

impl Chunk {
    pub fn empty(pos: ChunkPos) -> Self {
        Self { pos, sections: std::array::from_fn(|_| empty_section()), modified: 0 }
    }

    /// `x` and `z` in `0..16`; `y` in world coordinates.
    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> BlockState {
        if !(MIN_Y..=MAX_Y).contains(&y) {
            return BlockState::AIR;
        }
        let section = &self.sections[((y - MIN_Y) >> 4) as usize];
        BlockState(section.get(zenith_core::pos::cell_index(x, y & 15, z)))
    }

    /// Writes a cell and marks its section as different from the generator's. Returns what it held.
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) -> BlockState {
        if !(MIN_Y..=MAX_Y).contains(&y) {
            return BlockState::AIR;
        }
        let index = ((y - MIN_Y) >> 4) as usize;
        let cell = zenith_core::pos::cell_index(x, y & 15, z);
        if self.sections[index].get(cell) == state.0 {
            return state;
        }
        self.modified |= 1 << index;
        BlockState(Arc::make_mut(&mut self.sections[index]).set(cell, state.0))
    }

    /// The highest cell in this column for which `test` holds.
    pub fn top(&self, x: i32, z: i32, test: impl Fn(BlockState) -> bool) -> Option<i32> {
        for index in (0..SECTIONS).rev() {
            let section = &self.sections[index];
            let base = MIN_Y + (index as i32) * 16;
            if let Some(v) = section.single() {
                if test(BlockState(v)) {
                    return Some(base + 15);
                }
                continue;
            }
            for dy in (0..16).rev() {
                if test(BlockState(section.get(zenith_core::pos::cell_index(x, dy, z)))) {
                    return Some(base + dy);
                }
            }
        }
        None
    }

    /// The sections that differ from the generator's, in the form saves and the network use.
    pub fn encode_modified(&self) -> Vec<(u8, Vec<u8>)> {
        (0..SECTIONS)
            .filter(|i| self.modified & (1 << i) != 0)
            .map(|i| {
                let mut section = (*self.sections[i]).clone();
                section.compact();
                let mut bytes = Vec::new();
                section.encode(&mut bytes);
                (i as u8, bytes)
            })
            .collect()
    }

    /// Puts sections from `encode_modified` over this chunk.
    pub fn apply_encoded(&mut self, sections: &[(u8, Vec<u8>)]) -> Result<(), DecodeError> {
        for (index, bytes) in sections {
            let index = *index as usize;
            if index >= SECTIONS {
                return Err(DecodeError("section index"));
            }
            let mut input = bytes.as_slice();
            self.sections[index] = Arc::new(Paletted::decode(&mut input)?);
            self.modified |= 1 << index;
        }
        Ok(())
    }

    /// Heap memory only this chunk holds (shared empty sections do not count).
    pub fn heap_bytes(&self) -> usize {
        self.sections.iter().map(|s| s.heap_bytes()).sum()
    }
}

/// Reads blocks by world position: what physics, picking and AI need to know of a world.
pub trait BlockAccess {
    fn block(&self, p: BlockPos) -> BlockState;
    /// Whether this position's chunk is here at all. Bodies freeze rather than fall into chunks
    /// that have not arrived.
    fn is_loaded(&self, p: BlockPos) -> bool;
}

#[derive(Clone, Debug, Default)]
pub struct ChunkMap {
    pub chunks: FxMap<ChunkPos, Chunk>,
}

impl ChunkMap {
    pub fn get(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos)
    }
    /// Writes a block if its chunk is here. Returns what it held.
    pub fn set(&mut self, p: BlockPos, state: BlockState) -> Option<BlockState> {
        let c = self.chunks.get_mut(&p.chunk())?;
        Some(c.set(p.x & 15, p.y, p.z & 15, state))
    }
}

impl BlockAccess for ChunkMap {
    #[inline]
    fn block(&self, p: BlockPos) -> BlockState {
        self.chunks.get(&p.chunk()).map_or(BlockState::AIR, |c| c.get(p.x & 15, p.y, p.z & 15))
    }
    #[inline]
    fn is_loaded(&self, p: BlockPos) -> bool {
        self.chunks.contains_key(&p.chunk())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_content::id;

    #[test]
    fn writes_are_visible_and_mark_only_their_section() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        assert_eq!(c.set(3, 70, 9, BlockState::of(id::STONE)), BlockState::AIR);
        assert_eq!(c.get(3, 70, 9), BlockState::of(id::STONE));
        assert_eq!(c.get(3, 71, 9), BlockState::AIR);
        assert_eq!(c.modified, 1 << ((70 - MIN_Y) >> 4));
    }

    #[test]
    fn writing_the_same_value_changes_nothing() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        c.set(1, 1, 1, BlockState::AIR);
        assert_eq!(c.modified, 0);
    }

    #[test]
    fn a_snapshot_does_not_see_later_writes() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        c.set(1, 1, 1, BlockState::of(id::DIRT));
        let snapshot = c.clone();
        c.set(1, 1, 1, BlockState::of(id::STONE));
        c.set(2, 2, 2, BlockState::of(id::STONE));
        assert_eq!(snapshot.get(1, 1, 1), BlockState::of(id::DIRT));
        assert_eq!(snapshot.get(2, 2, 2), BlockState::AIR);
    }

    #[test]
    fn out_of_world_reads_and_writes_are_harmless() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        assert_eq!(c.set(0, MAX_Y + 1, 0, BlockState::of(id::STONE)), BlockState::AIR);
        assert_eq!(c.get(0, MAX_Y + 1, 0), BlockState::AIR);
        assert_eq!(c.get(0, MIN_Y - 1, 0), BlockState::AIR);
        assert_eq!(c.modified, 0);
    }

    #[test]
    fn top_finds_the_highest_match_across_sections() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        c.set(4, -30, 4, BlockState::of(id::STONE));
        c.set(4, 100, 4, BlockState::of(id::STONE));
        c.set(5, 100, 4, BlockState::of(id::DIRT));
        let solid = |s: BlockState| s.kind() == id::STONE;
        assert_eq!(c.top(4, 4, solid), Some(100));
        assert_eq!(c.top(5, 4, solid), None);
    }

    #[test]
    fn modified_sections_round_trip_onto_a_fresh_chunk() {
        let mut c = Chunk::empty(ChunkPos::new(2, 3));
        c.set(1, 5, 1, BlockState::new(id::WATER, 3));
        c.set(1, 200, 1, BlockState::of(id::TORCH));
        let sent = c.encode_modified();
        assert_eq!(sent.len(), 2);
        let mut copy = Chunk::empty(ChunkPos::new(2, 3));
        copy.apply_encoded(&sent).expect("applies");
        assert_eq!(copy.get(1, 5, 1), BlockState::new(id::WATER, 3));
        assert_eq!(copy.get(1, 200, 1), BlockState::of(id::TORCH));
        assert_eq!(copy.modified, c.modified);
        assert!(copy.apply_encoded(&[(99, vec![0, 0, 0])]).is_err());
    }

    #[test]
    fn the_map_knows_which_chunks_are_loaded() {
        let mut map = ChunkMap::default();
        map.chunks.insert(ChunkPos::new(-1, 0), Chunk::empty(ChunkPos::new(-1, 0)));
        let inside = BlockPos::new(-1, 10, 5);
        assert!(map.is_loaded(inside));
        assert!(!map.is_loaded(BlockPos::new(0, 10, 5)));
        assert_eq!(map.set(inside, BlockState::of(id::STONE)), Some(BlockState::AIR));
        assert_eq!(map.block(inside), BlockState::of(id::STONE));
        assert_eq!(map.set(BlockPos::new(99, 10, 5), BlockState::of(id::STONE)), None);
    }
}
