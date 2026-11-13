//! Keeps meshes on the GPU in step with the world a session knows.
//!
//! The session says which chunks arrived and which blocks changed. This schedules the meshing on
//! worker threads, closest first, and uploads what comes back within a budget per frame, so that no
//! amount of terrain arriving can ever cost a dropped frame.
use crate::render::Renderer;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use zenith_content::{BlockState, Registry};
use zenith_core::pool::Pool;
use zenith_core::pos::{MIN_Y, SECTIONS};
use zenith_core::{BlockPos, ChunkPos, FxMap, FxSet};
use zenith_mesh::{HoodOwned, MeshResult, Mesher, SectionMesh, hood_ref};
use zenith_world::{ChunkMap, Generator};

const ALL_SECTIONS: u32 = (1 << SECTIONS) - 1;
/// How far around the player a chunk's light is kept, in chunks, for lighting entities.
const KEEP_LIGHT_RADIUS: i32 = 4;
/// How many bytes of mesh go to the GPU in one frame.
const UPLOAD_BUDGET: usize = 24 * 1024 * 1024;
/// However little room the GPU has, meshes are kept at least this far out, in chunks.
const MIN_REACH: i32 = 2;
/// How many frames reach holds still before it may grow back.
const SETTLE_FRAMES: u32 = 120;
/// How full the room for meshes may get, growing reach back.
const GROW_UNTIL: f32 = 0.85;

/// Where meshes go: room for them, and taking them away again. The renderer, or in tests a stand-in.
pub trait MeshStore {
    /// Puts a section's mesh up, replacing what it had: false, leaving it with nothing, if there was
    /// no room for it.
    fn upload_section(&mut self, pos: ChunkPos, y: u8, mesh: &SectionMesh) -> bool;
    /// Takes all of a chunk's meshes away, and leaves its place to far terrain.
    fn remove_chunk(&mut self, pos: ChunkPos);
    /// Says real meshes stand at a chunk, so far terrain stays out of it.
    fn set_chunk_covered(&mut self, pos: ChunkPos, covered: bool);
    /// How much of the room for meshes is taken, 0 to 1.
    fn vertex_use(&self) -> f32;
}

impl MeshStore for Renderer {
    fn upload_section(&mut self, pos: ChunkPos, y: u8, mesh: &SectionMesh) -> bool {
        Renderer::upload_section(self, pos, y, mesh)
    }
    fn remove_chunk(&mut self, pos: ChunkPos) {
        Renderer::remove_chunk(self, pos);
    }
    fn set_chunk_covered(&mut self, pos: ChunkPos, covered: bool) {
        Renderer::set_chunk_covered(self, pos, covered);
    }
    fn vertex_use(&self) -> f32 {
        Renderer::vertex_use(self)
    }
}

thread_local! {
    static MESHER: RefCell<Mesher> = RefCell::new(Mesher::default());
}

#[derive(Default)]
struct Entry {
    /// Sections that must be meshed again.
    dirty: u32,
    /// Light may have changed, so it must be solved again; sections it touched get meshed too.
    relight: bool,
    in_flight: bool,
    hashes: [u64; SECTIONS],
    meshed: bool,
    light: Option<Box<[u8]>>,
}

pub struct Streamer {
    pool: Pool,
    tx: Sender<MeshResult>,
    rx: Receiver<MeshResult>,
    reg: Arc<Registry>,
    generator: Arc<Generator>,
    entries: FxMap<ChunkPos, Entry>,
    wanting: FxSet<ChunkPos>,
    in_flight: usize,
    /// Of those wanting meshes, how many still lack a neighbour. The ring at the edge of the view
    /// always does, so these are not work that is pending.
    waiting_for_neighbours: usize,
    /// How many chunks have meshes on the GPU.
    pub meshed_chunks: usize,
    pub meshes_done: u64,
    /// How far from the player, in chunks, meshes are kept. Where the GPU runs out of room for
    /// them, this draws in, the farthest going first so what is near always fits, and far terrain
    /// stands in beyond it. It grows back while there is room to spare.
    reach: i32,
    /// Of those wanting meshes, how many are out of reach.
    beyond_reach: usize,
    /// Frames since reach last changed.
    settled: u32,
    /// Where the player was, and how far reach went, when meshes were last kept to it.
    kept_for: Option<(ChunkPos, i32)>,
}

impl Streamer {
    pub fn new(reg: Arc<Registry>, generator: Arc<Generator>, threads: usize) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            pool: Pool::new("mesh", threads.max(1)),
            tx,
            rx,
            reg,
            generator,
            entries: FxMap::default(),
            wanting: FxSet::default(),
            in_flight: 0,
            waiting_for_neighbours: 0,
            meshed_chunks: 0,
            meshes_done: 0,
            reach: i32::MAX,
            beyond_reach: 0,
            settled: 0,
            kept_for: None,
        }
    }

    /// Meshing still to do, of what can be done now.
    pub fn pending(&self) -> usize {
        self.wanting.len().saturating_sub(self.waiting_for_neighbours + self.beyond_reach) + self.in_flight
    }

    /// How far out meshes are kept, if the GPU's room for them sets a limit.
    pub fn reach(&self) -> Option<i32> {
        (self.reach < i32::MAX).then_some(self.reach)
    }

    pub fn chunk_arrived(&mut self, pos: ChunkPos) {
        let e = self.entries.entry(pos).or_default();
        e.dirty = ALL_SECTIONS;
        e.relight = true;
        self.wanting.insert(pos);
        // A neighbour coming back changes what shows along the border.
        for dz in -1..=1 {
            for dx in -1..=1 {
                let n = ChunkPos::new(pos.x + dx, pos.z + dz);
                if n != pos
                    && let Some(e) = self.entries.get_mut(&n)
                    && e.meshed
                {
                    e.dirty = ALL_SECTIONS;
                    e.relight = true;
                    self.wanting.insert(n);
                }
            }
        }
    }

    pub fn chunk_left(&mut self, pos: ChunkPos, store: &mut impl MeshStore) {
        if self.entries.remove(&pos).is_some_and(|e| e.meshed) {
            self.meshed_chunks -= 1;
        }
        self.wanting.remove(&pos);
        store.remove_chunk(pos);
    }

    /// A block changed: its section needs meshing, as do neighbours it touches, and maybe light.
    pub fn block_changed(&mut self, pos: BlockPos, old: BlockState, new: BlockState) {
        let (lut_old, lut_new) = (old.kind() as usize, new.kind() as usize);
        let lut = &self.reg.lut;
        let lights_change =
            lut.opacity[lut_old] != lut.opacity[lut_new] || lut.emission[lut_old] != lut.emission[lut_new];
        let (cx, cz) = (pos.x >> 4, pos.z >> 4);
        let (lx, ly, lz) = (pos.x & 15, pos.y & 15, pos.z & 15);
        let section = ((pos.y - MIN_Y) >> 4) as u32;
        let mark = |entries: &mut FxMap<ChunkPos, Entry>,
                    wanting: &mut FxSet<ChunkPos>,
                    c: ChunkPos,
                    s: u32,
                    relight: bool| {
            if let Some(e) = entries.get_mut(&c) {
                if (s as usize) < SECTIONS {
                    e.dirty |= 1 << s;
                }
                e.relight |= relight;
                wanting.insert(c);
            }
        };
        for dz in -1..=1i32 {
            for dx in -1..=1i32 {
                // Corners and edges matter too: a corner cell shades its diagonal neighbour's faces.
                let touches_x = dx == 0 || (dx < 0 && lx == 0) || (dx > 0 && lx == 15);
                let touches_z = dz == 0 || (dz < 0 && lz == 0) || (dz > 0 && lz == 15);
                let c = ChunkPos::new(cx + dx, cz + dz);
                if touches_x && touches_z {
                    mark(&mut self.entries, &mut self.wanting, c, section, lights_change);
                    if ly == 0 && section > 0 {
                        mark(&mut self.entries, &mut self.wanting, c, section - 1, lights_change);
                    }
                    if ly == 15 {
                        mark(&mut self.entries, &mut self.wanting, c, section + 1, lights_change);
                    }
                } else if lights_change {
                    mark(&mut self.entries, &mut self.wanting, c, section, true);
                }
            }
        }
    }

    /// Hands out work: chunks in reach that want meshing and have their neighbours here, closest
    /// first.
    pub fn schedule(&mut self, world: &ChunkMap, center: ChunkPos) {
        self.waiting_for_neighbours = 0;
        self.beyond_reach = 0;
        if self.wanting.is_empty() {
            return;
        }
        let limit = self.pool.thread_count() * 3;
        let mut list: Vec<(i64, ChunkPos)> = self.wanting.iter().map(|&p| (p.distance_sq(center), p)).collect();
        list.sort_unstable();
        for (distance, pos) in list {
            let Some(entry) = self.entries.get_mut(&pos) else {
                self.wanting.remove(&pos);
                continue;
            };
            if entry.in_flight {
                continue;
            }
            if pos.distance(center) > self.reach {
                self.beyond_reach += 1;
                continue;
            }
            // Any neighbour missing, and the light and faces along that side would be wrong.
            if !(-1..=1).all(|dz| (-1..=1).all(|dx| world.get(ChunkPos::new(pos.x + dx, pos.z + dz)).is_some())) {
                self.waiting_for_neighbours += 1;
                continue;
            }
            if self.in_flight >= limit {
                continue;
            }
            let hood: HoodOwned = std::array::from_fn(|rz| {
                std::array::from_fn(|rx| {
                    world.get(ChunkPos::new(pos.x + rx as i32 - 1, pos.z + rz as i32 - 1)).cloned()
                })
            });
            entry.in_flight = true;
            let dirty = std::mem::take(&mut entry.dirty);
            entry.relight = false;
            let hashes = entry.hashes;
            self.wanting.remove(&pos);
            self.in_flight += 1;
            let (reg, generator, tx) = (self.reg.clone(), self.generator.clone(), self.tx.clone());
            let keep_light = pos.distance(center) <= KEEP_LIGHT_RADIUS;
            self.pool.submit(distance, move || {
                let result = MESHER
                    .with(|m| m.borrow_mut().mesh(&hood_ref(&hood), &reg, &generator, dirty, &hashes, keep_light));
                if let Some(result) = result {
                    let _ = tx.send(result);
                }
            });
        }
    }

    /// Puts finished meshes on the GPU, as many as this frame's budget allows. Short of room, the
    /// farthest meshes give theirs up to nearer ones.
    pub fn upload(&mut self, store: &mut impl MeshStore, center: ChunkPos) {
        self.keep_within_reach(store, center);
        let mut budget = UPLOAD_BUDGET;
        while budget > 0 {
            let Ok(result) = self.rx.try_recv() else { break };
            self.in_flight = self.in_flight.saturating_sub(1);
            let pos = result.pos;
            let Some(entry) = self.entries.get_mut(&pos) else { continue };
            entry.in_flight = false;
            entry.hashes = result.light_hashes;
            if result.light.is_some() {
                entry.light = result.light;
            }
            let distance = pos.distance(center);
            // Meshed while in reach, and out of it now: it waits until it is in reach again.
            if distance > self.reach {
                self.park(pos, store);
                continue;
            }
            let mut fitted = true;
            for s in &result.sections {
                let bytes = (s.mesh.opaque.len() + s.mesh.cutout.len() + s.mesh.translucent.len())
                    * std::mem::size_of::<zenith_mesh::Vertex>();
                budget = budget.saturating_sub(bytes);
                while !store.upload_section(pos, s.index, &s.mesh) {
                    if !self.draw_in(store, center, distance) {
                        fitted = false;
                        break;
                    }
                }
                if !fitted {
                    break;
                }
                self.meshes_done += 1;
            }
            if !fitted {
                // Nothing farther is left to give up its room: this chunk is the edge of reach now.
                // Nearer than the least reach, there is nothing to do but go without the section.
                if distance > MIN_REACH {
                    self.set_reach(distance - 1);
                    self.park(pos, store);
                    continue;
                }
            }
            let Some(entry) = self.entries.get_mut(&pos) else { continue };
            if !entry.meshed {
                entry.meshed = true;
                self.meshed_chunks += 1;
                store.set_chunk_covered(pos, true);
            }
            if entry.dirty != 0 || entry.relight {
                self.wanting.insert(pos);
            }
        }
    }

    /// Makes room by pulling reach in past the farthest meshes, if any are farther than `distance`.
    fn draw_in(&mut self, store: &mut impl MeshStore, center: ChunkPos, distance: i32) -> bool {
        let farthest = self.entries.iter().filter(|(_, e)| e.meshed).map(|(p, _)| p.distance(center)).max();
        match farthest {
            Some(far) if far > distance && far > MIN_REACH => {
                self.set_reach(far - 1);
                self.keep_within_reach(store, center);
                true
            }
            _ => false,
        }
    }

    fn set_reach(&mut self, reach: i32) {
        self.reach = reach.max(MIN_REACH);
        self.settled = 0;
    }

    /// Takes meshes off the GPU that are out of reach, and lets reach grow back, a ring at a time,
    /// once there is clearly room for the next ring and something waiting for it.
    fn keep_within_reach(&mut self, store: &mut impl MeshStore, center: ChunkPos) {
        self.settled = self.settled.saturating_add(1);
        if self.reach < i32::MAX && self.settled > SETTLE_FRAMES && self.beyond_reach > 0 && self.meshed_chunks > 0 {
            let used = store.vertex_use();
            let ring = 8.0 * (self.reach + 1) as f32;
            if used + used / self.meshed_chunks as f32 * ring < GROW_UNTIL {
                self.reach += 1;
                self.settled = 0;
            }
        }
        if self.kept_for == Some((center, self.reach)) {
            return;
        }
        self.kept_for = Some((center, self.reach));
        let out: Vec<ChunkPos> =
            self.entries.iter().filter(|(p, e)| e.meshed && p.distance(center) > self.reach).map(|(p, _)| *p).collect();
        for pos in out {
            self.park(pos, store);
        }
    }

    /// Takes a chunk's meshes off the GPU, and leaves it wanting them for when it is in reach.
    fn park(&mut self, pos: ChunkPos, store: &mut impl MeshStore) {
        if let Some(e) = self.entries.get_mut(&pos) {
            if e.meshed {
                e.meshed = false;
                self.meshed_chunks -= 1;
            }
            e.dirty = ALL_SECTIONS;
            e.relight = true;
            self.wanting.insert(pos);
        }
        store.remove_chunk(pos);
    }

    /// Sky and block light at a position, if it is near enough to be known.
    pub fn light_at(&self, x: i32, y: i32, z: i32) -> Option<(u8, u8)> {
        if !(MIN_Y..=zenith_core::pos::MAX_Y).contains(&y) {
            return None;
        }
        let e = self.entries.get(&ChunkPos::new(x >> 4, z >> 4))?;
        let light = e.light.as_ref()?;
        let packed = light[((y - MIN_Y) as usize) * 256 + ((z & 15) as usize) * 16 + (x & 15) as usize];
        Some((packed >> 4, packed & 15))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_mesh::{MeshedSection, Vertex};

    /// Room for so many vertices, and nothing more: a GPU, as far as the streamer can tell.
    #[derive(Default)]
    struct Room {
        capacity: usize,
        sections: FxMap<(ChunkPos, u8), usize>,
        covered: FxSet<ChunkPos>,
    }

    impl Room {
        fn used(&self) -> usize {
            self.sections.values().sum()
        }
    }

    impl MeshStore for Room {
        fn upload_section(&mut self, pos: ChunkPos, y: u8, mesh: &SectionMesh) -> bool {
            self.sections.remove(&(pos, y));
            let n = mesh.opaque.len() + mesh.cutout.len() + mesh.translucent.len();
            if self.used() + n > self.capacity {
                return false;
            }
            self.sections.insert((pos, y), n);
            true
        }
        fn remove_chunk(&mut self, pos: ChunkPos) {
            self.sections.retain(|(p, _), _| *p != pos);
            self.covered.remove(&pos);
        }
        fn set_chunk_covered(&mut self, pos: ChunkPos, covered: bool) {
            if covered {
                self.covered.insert(pos);
            } else {
                self.covered.remove(&pos);
            }
        }
        fn vertex_use(&self) -> f32 {
            self.used() as f32 / self.capacity as f32
        }
    }

    fn streamer() -> Streamer {
        Streamer::new(Arc::new(Registry::builtin()), Arc::new(Generator::new(1)), 1)
    }

    /// A chunk's mesh as the workers hand it back: one section of `n` vertices.
    fn meshed(s: &Streamer, pos: ChunkPos, n: usize) {
        let mesh = SectionMesh { opaque: vec![Vertex::default(); n], cutout: Vec::new(), translucent: Vec::new() };
        let sections = vec![MeshedSection { index: 4, mesh }];
        s.tx.send(MeshResult { pos, sections, light_hashes: [0; SECTIONS], light: None }).expect("sends");
    }

    /// Every chunk out to 4 from the middle has arrived and been meshed, the farthest first, as
    /// happens when a player turns to face what was behind them: 81 meshes, with room for 30.
    fn crowded() -> (Streamer, Room, Vec<ChunkPos>) {
        let mut s = streamer();
        let mut room = Room { capacity: 3000, ..Default::default() };
        let all: Vec<ChunkPos> = (-4..=4).flat_map(|z| (-4..=4).map(move |x| ChunkPos::new(x, z))).collect();
        for &p in &all {
            s.chunk_arrived(p);
        }
        let mut order = all.clone();
        order.sort_by_key(|p| std::cmp::Reverse(p.distance(ChunkPos::new(0, 0))));
        for &p in &order {
            meshed(&s, p, 100);
        }
        s.upload(&mut room, ChunkPos::new(0, 0));
        (s, room, all)
    }

    #[test]
    fn with_no_room_left_near_meshes_push_far_ones_out_and_far_terrain_stands_in() {
        let (mut s, room, all) = crowded();
        // Out to 2 is 25 chunks, which fit; out to 3 would be 49, which do not.
        assert_eq!(s.reach(), Some(2));
        assert!(room.used() <= room.capacity);
        for &p in &all {
            let near = p.distance(ChunkPos::new(0, 0)) <= 2;
            assert_eq!(room.covered.contains(&p), near, "{p:?}");
            assert_eq!(room.sections.contains_key(&(p, 4)), near, "{p:?}");
        }
        assert_eq!(s.meshed_chunks, 25);
        // What is out of reach waits, and is not work to be done.
        s.schedule(&ChunkMap::default(), ChunkPos::new(0, 0));
        assert_eq!(s.pending(), 0);
    }

    #[test]
    fn walking_on_frees_the_room_behind_for_what_is_ahead() {
        let (mut s, mut room, _) = crowded();
        let ahead = ChunkPos::new(2, 0);
        s.upload(&mut room, ahead);
        assert!(room.covered.iter().all(|p| p.distance(ahead) <= 2), "{:?}", room.covered);
        assert_eq!(s.meshed_chunks, room.covered.len());
        // And what comes into reach is meshed again.
        s.schedule(&ChunkMap::default(), ahead);
        assert!(s.wanting.iter().any(|p| p.distance(ahead) <= 2 && p.x == 4));
    }

    #[test]
    fn reach_grows_back_a_ring_at_a_time_once_there_is_room() {
        let (mut s, mut room, _) = crowded();
        room.capacity = 100_000;
        s.schedule(&ChunkMap::default(), ChunkPos::new(0, 0));
        for _ in 0..SETTLE_FRAMES / 2 {
            s.upload(&mut room, ChunkPos::new(0, 0));
        }
        assert_eq!(s.reach(), Some(2), "it waits a moment first");
        for _ in 0..SETTLE_FRAMES {
            s.upload(&mut room, ChunkPos::new(0, 0));
        }
        assert_eq!(s.reach(), Some(3), "then one ring, and a moment before the next");
        // While nothing waits beyond reach, it has no reason to grow.
        let mut quiet = streamer();
        let mut plenty = Room { capacity: 100_000, ..Default::default() };
        for _ in 0..SETTLE_FRAMES * 2 {
            quiet.upload(&mut plenty, ChunkPos::new(0, 0));
        }
        assert_eq!(quiet.reach(), None);
    }
}
