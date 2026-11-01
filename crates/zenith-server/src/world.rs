//! The server's world: the chunks being simulated, and everything that changes them.
use crate::inventory::{CHEST_SIZE, Furnace, Slot};
use crate::persist::{BlockEntity, ChunkRecord, Disk, ItemRemap};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use zenith_content::{BlockState, ItemStack, Registry, id};
use zenith_core::glam::DVec3;
use zenith_core::pool::Pool;
use zenith_core::{BlockPos, ChunkPos, Dir, FxMap, FxSet, Rng};
use zenith_protocol::{BlockChange, Effect, EffectKind};
use zenith_world::{BlockAccess, Chunk, ChunkMap, Generator};

/// Something the world wants done that only the entity system can do.
#[derive(Clone, Debug, PartialEq)]
pub enum WorldEvent {
    DropItem { pos: DVec3, stack: ItemStack },
    Falling { pos: BlockPos, state: BlockState },
    Prime { pos: BlockPos },
}

struct Loaded {
    chunk: Chunk,
    block_entities: Vec<(BlockPos, BlockEntity)>,
}

pub struct ServerWorld {
    pub reg: Arc<Registry>,
    pub generator: Arc<Generator>,
    pub map: ChunkMap,
    pub block_entities: FxMap<BlockPos, BlockEntity>,
    pub tick: u64,
    pub rng: Rng,
    /// What changed since the last time the server told its players.
    pub changes: Vec<BlockChange>,
    pub effects: Vec<Effect>,
    pub events: Vec<WorldEvent>,
    /// Positions that have just seen a neighbour change.
    pub(crate) updates: VecDeque<BlockPos>,
    scheduled: BinaryHeap<Reverse<(u64, u64, BlockPos)>>,
    scheduled_set: FxSet<BlockPos>,
    scheduled_order: u64,
    /// Chunks with changes not saved yet.
    dirty: FxSet<ChunkPos>,
    /// Chunks that have a record on disk. Everything else is exactly what the generator makes.
    pub saved: FxSet<ChunkPos>,
    pending: FxSet<ChunkPos>,
    loaded_tx: Sender<Loaded>,
    loaded_rx: Receiver<Loaded>,
    pool: Option<Arc<Pool>>,
    pub disk: Option<Disk>,
    remap: Arc<ItemRemap>,
}

impl BlockAccess for ServerWorld {
    #[inline]
    fn block(&self, p: BlockPos) -> BlockState {
        self.map.block(p)
    }
    #[inline]
    fn is_loaded(&self, p: BlockPos) -> bool {
        self.map.is_loaded(p)
    }
}

fn build_chunk(generator: &Generator, pos: ChunkPos, bytes: Option<Vec<u8>>, remap: &ItemRemap) -> Loaded {
    let mut chunk = generator.chunk(pos);
    let mut block_entities = Vec::new();
    if let Some(record) = bytes.as_deref().and_then(ChunkRecord::from_bytes) {
        if let Err(e) = chunk.apply_encoded(&record.sections) {
            eprintln!("zenith: chunk {pos:?} has a damaged section ({e}); using the generated one");
        }
        block_entities = record.block_entities;
        for (_, be) in &mut block_entities {
            remap.block_entity(be);
        }
    }
    Loaded { chunk, block_entities }
}

impl ServerWorld {
    /// `threads` of 0 means no background work at all: every chunk loads during the call that asks
    /// for it, which is what makes a run exactly reproducible.
    pub fn new(reg: Arc<Registry>, generator: Arc<Generator>, seed: u64, threads: usize, disk: Option<Disk>, saved: Vec<ChunkPos>, remap: ItemRemap) -> Self {
        let (loaded_tx, loaded_rx) = mpsc::channel();
        Self {
            reg,
            generator,
            map: ChunkMap::default(),
            block_entities: FxMap::default(),
            tick: 0,
            rng: Rng::new(seed ^ 0x5352_5652),
            changes: Vec::new(),
            effects: Vec::new(),
            events: Vec::new(),
            updates: VecDeque::new(),
            scheduled: BinaryHeap::new(),
            scheduled_set: FxSet::default(),
            scheduled_order: 0,
            dirty: FxSet::default(),
            saved: saved.into_iter().collect(),
            pending: FxSet::default(),
            loaded_tx,
            loaded_rx,
            pool: (threads > 0).then(|| Arc::new(Pool::new("chunk", threads))),
            disk,
            remap: Arc::new(remap),
        }
    }

    /// Starts loading a chunk unless it is here or on its way.
    pub fn request(&mut self, pos: ChunkPos, priority: i64) {
        if self.map.chunks.contains_key(&pos) || self.pending.contains(&pos) {
            return;
        }
        let needs_disk = self.saved.contains(&pos);
        let (generator, remap) = (self.generator.clone(), self.remap.clone());
        let Some(pool) = self.pool.clone() else {
            let bytes = if needs_disk { self.read_now(pos) } else { None };
            let loaded = build_chunk(&generator, pos, bytes, &remap);
            self.accept(loaded);
            return;
        };
        self.pending.insert(pos);
        let tx = self.loaded_tx.clone();
        match (&self.disk, needs_disk) {
            (Some(disk), true) => disk.read(pos, move |bytes| {
                let worker_pool = pool.clone();
                worker_pool.submit(priority, move || {
                    let _keep_alive = pool;
                    let _ = tx.send(build_chunk(&generator, pos, bytes, &remap));
                });
            }),
            _ => pool.submit(priority, move || {
                let _ = tx.send(build_chunk(&generator, pos, None, &remap));
            }),
        }
    }

    fn read_now(&self, pos: ChunkPos) -> Option<Vec<u8>> {
        let disk = self.disk.as_ref()?;
        let (tx, rx) = mpsc::channel();
        disk.read(pos, move |bytes| {
            let _ = tx.send(bytes);
        });
        rx.recv().ok().flatten()
    }

    fn accept(&mut self, loaded: Loaded) {
        self.pending.remove(&loaded.chunk.pos);
        for (pos, be) in loaded.block_entities {
            self.block_entities.insert(pos, be);
        }
        self.map.chunks.insert(loaded.chunk.pos, loaded.chunk);
    }

    /// Takes in the chunks that finished loading.
    pub fn pump(&mut self) {
        while let Ok(loaded) = self.loaded_rx.try_recv() {
            self.accept(loaded);
        }
    }

    pub fn pending_chunks(&self) -> usize {
        self.pending.len()
    }

    /// Whether forgetting `pos` loses nothing: the disk has it, or the generator can make it again.
    pub fn can_unload(&self, pos: ChunkPos) -> bool {
        self.disk.is_some()
            || (!self.dirty.contains(&pos) && self.map.chunks.get(&pos).is_none_or(|c| c.modified == 0))
    }

    /// Saves what changed in `pos` and forgets it.
    pub fn unload(&mut self, pos: ChunkPos) {
        self.save_chunk(pos);
        self.map.chunks.remove(&pos);
        self.block_entities.retain(|p, _| p.chunk() != pos);
    }

    fn save_chunk(&mut self, pos: ChunkPos) {
        if !self.dirty.remove(&pos) {
            return;
        }
        let Some(disk) = &self.disk else { return };
        let Some(chunk) = self.map.chunks.get(&pos) else { return };
        let bes: Vec<_> = self.block_entities.iter().filter(|(p, _)| p.chunk() == pos).map(|(p, b)| (*p, b.clone())).collect();
        disk.save(chunk.clone(), bes);
        self.saved.insert(pos);
    }

    /// Hands every changed chunk to the disk thread. Costs the tick almost nothing: a snapshot is a
    /// few `Arc` clones, and encoding and writing happen over there.
    pub fn save_dirty(&mut self) -> usize {
        let dirty: Vec<ChunkPos> = self.dirty.iter().copied().collect();
        for &pos in &dirty {
            self.save_chunk(pos);
        }
        if let Some(disk) = &self.disk {
            disk.commit();
        }
        dirty.len()
    }

    pub fn has_unsaved(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// The sections of `pos` the generator would not make, if it is in memory.
    pub fn modified_sections(&self, pos: ChunkPos) -> Option<Vec<(u8, Vec<u8>)>> {
        self.map.chunks.get(&pos).map(|c| c.encode_modified())
    }

    /// Writes a block, and lets its neighbours know. Returns whether anything changed.
    pub fn set(&mut self, p: BlockPos, state: BlockState) -> bool {
        if !p.in_world() {
            return false;
        }
        let Some(chunk) = self.map.chunks.get_mut(&p.chunk()) else { return false };
        let old = chunk.set(p.x & 15, p.y, p.z & 15, state);
        if old == state {
            return false;
        }
        self.changes.push(BlockChange { pos: p, state });
        self.dirty.insert(p.chunk());
        self.updates.push_back(p);
        for d in Dir::ALL {
            self.updates.push_back(p.offset(d));
        }
        if old.kind() != state.kind() {
            self.drop_block_entity(p);
        }
        true
    }

    fn drop_block_entity(&mut self, p: BlockPos) {
        let Some(be) = self.block_entities.remove(&p) else { return };
        let items: Vec<Slot> = match be {
            BlockEntity::Chest(cells) => cells,
            BlockEntity::Furnace(f) => vec![f.input, f.fuel, f.output],
        };
        for stack in items.into_iter().flatten() {
            self.events.push(WorldEvent::DropItem { pos: p.center(), stack });
        }
    }

    /// Marks a chunk as needing a save, for changes that are not block changes.
    pub fn mark_dirty(&mut self, pos: ChunkPos) {
        self.dirty.insert(pos);
    }

    /// Asks for `tick_block` at `p` in `delay` ticks. Asking again before then does nothing.
    pub fn schedule(&mut self, p: BlockPos, delay: u64) {
        if self.scheduled_set.insert(p) {
            self.scheduled_order += 1;
            self.scheduled.push(Reverse((self.tick + delay, self.scheduled_order, p)));
        }
    }

    #[cfg(test)]
    pub(crate) fn scheduled_len(&self) -> usize {
        self.scheduled.len()
    }

    pub(crate) fn pop_due(&mut self) -> Option<BlockPos> {
        match self.scheduled.peek() {
            Some(Reverse((due, _, _))) if *due <= self.tick => {
                let Reverse((_, _, p)) = self.scheduled.pop()?;
                self.scheduled_set.remove(&p);
                Some(p)
            }
            _ => None,
        }
    }

    pub fn effect(&mut self, kind: EffectKind, pos: DVec3) {
        self.effects.push(Effect { kind, pos: [pos.x as f32, pos.y as f32, pos.z as f32] });
    }

    /// The chest at `p`, filling a generated one with its treasure the first time it is asked for.
    pub fn chest(&mut self, p: BlockPos) -> &mut Vec<Slot> {
        let seed = self.generator.seed;
        let reg = self.reg.clone();
        let be = self.block_entities.entry(p).or_insert_with(|| BlockEntity::Chest(chest_contents(seed, p, &reg)));
        if !matches!(be, BlockEntity::Chest(_)) {
            *be = BlockEntity::Chest(vec![None; CHEST_SIZE]);
        }
        let BlockEntity::Chest(cells) = be else { unreachable!("just made sure") };
        cells
    }

    pub fn furnace(&mut self, p: BlockPos) -> &mut Furnace {
        let be = self.block_entities.entry(p).or_insert_with(|| BlockEntity::Furnace(Furnace::default()));
        if !matches!(be, BlockEntity::Furnace(_)) {
            *be = BlockEntity::Furnace(Furnace::default());
        }
        let BlockEntity::Furnace(f) = be else { unreachable!("just made sure") };
        f
    }
}

/// What a chest found in the world holds, decided by the seed and its position. A chest placed by a
/// player has its block entity made first, so it starts empty.
fn chest_contents(seed: u64, p: BlockPos, reg: &Registry) -> Vec<Slot> {
    let mut rng = Rng::new(zenith_core::rng::mix(seed ^ 0x4C4F_4F54, ((p.x as u64) << 32) ^ ((p.y as u64) << 16) ^ p.z as u32 as u64));
    let mut cells = vec![None; CHEST_SIZE];
    const LOOT: [(&str, u32, i32); 12] = [
        ("stick", 8, 8),
        ("coal", 10, 8),
        ("copper_ingot", 8, 5),
        ("iron_ingot", 7, 4),
        ("gold_ingot", 3, 3),
        ("crystal", 1, 2),
        ("ember_dust", 4, 8),
        ("apple", 7, 4),
        ("cooked_meat", 5, 3),
        ("torch", 8, 12),
        ("iron_pick", 1, 1),
        ("stone_sword", 2, 1),
    ];
    let weight_total: u32 = LOOT.iter().map(|l| l.1).sum();
    for _ in 0..rng.range(3, 8) {
        let mut roll = rng.below(weight_total);
        for (name, weight, most) in LOOT {
            if roll < weight {
                if let Some(item) = reg.item_by_name(name) {
                    let cell = rng.below(CHEST_SIZE as u32) as usize;
                    cells[cell] = Some(ItemStack::new(item, rng.range(1, most) as u8));
                }
                break;
            }
            roll -= weight;
        }
    }
    cells
}

/// Whether a block is a kind that keeps a block entity.
pub fn holds_items(state: BlockState) -> bool {
    matches!(state.kind(), id::CHEST | id::FURNACE)
}
