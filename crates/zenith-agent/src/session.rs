//! A live connection to a server, and the client's picture of the world.
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use zenith_content::{BlockState, ItemStack, Registry};
use zenith_core::glam::DVec3;
use zenith_core::pool::Pool;
use zenith_core::{Aabb, BlockPos, ChunkPos, FxMap};
use zenith_protocol::*;
use zenith_world::physics::{self, Body, MoveInput};
use zenith_world::{BlockAccess, Chunk, ChunkMap, GENERATOR_VERSION, Generator};

/// Where the client's prediction differs from the server's by more than this, it is corrected.
const CORRECTION_EPSILON: f64 = 1e-3;
/// Most chunks asked for in one tick, so a teleport cannot flood the connection.
const MAX_NEW_PER_TICK: usize = 256;

/// Something a window (or an agent) may want to react to.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    ChunkReady(ChunkPos),
    ChunkGone(ChunkPos),
    BlockChanged { pos: BlockPos, old: BlockState, new: BlockState },
    Effect(Effect),
    Chat(ChatLine),
    Disconnected(String),
    Hurt,
    Died,
    /// The server opened a station on this client's screen.
    ScreenOpened(ScreenKind),
    ScreenClosed,
    ItemGained { item: u16, count: u32 },
}

#[derive(Clone, Debug)]
pub struct RemoteEntity {
    pub id: u32,
    pub kind: EntityKind,
    /// In `POS_SCALE`ths of a block, exactly as the server has it.
    quantized: [i32; 3],
    pub pos: DVec3,
    /// Where it was a tick ago, for drawing between the two.
    pub prev: DVec3,
    pub yaw: u8,
    pub pitch: i8,
    pub prev_yaw: u8,
    pub name: Option<String>,
    pub stack: Option<ItemStack>,
    pub block: BlockState,
}

fn unquantize(q: [i32; 3]) -> DVec3 {
    DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64) / POS_SCALE
}

struct PendingChunk {
    generated: Option<Chunk>,
    delta: Option<Vec<(u8, Vec<u8>)>>,
    blocks: Vec<BlockChange>,
}

pub struct Session {
    peer: Peer,
    pub reg: Arc<Registry>,
    pub generator: Arc<Generator>,
    pub entity_id: u32,
    pub world: ChunkMap,
    pub body: Body,
    pub prev_pos: DVec3,
    pub mode: GameMode,
    pub server_name: String,
    pub sim_distance: u8,
    pub clock: Clock,
    pub health: u8,
    pub air: u8,
    pub inventory: Vec<Option<ItemStack>>,
    pub selected: u8,
    pub cursor: Option<ItemStack>,
    pub screen: Option<Screen>,
    pub entities: FxMap<u32, RemoteEntity>,
    pub players: Vec<PlayerEntry>,
    pub cracks: FxMap<BlockPos, u8>,
    pub chat: VecDeque<ChatLine>,
    pub events: Vec<Event>,
    pub corrections: u64,
    pub disconnected: Option<String>,
    /// How far around the player chunks are wanted.
    pub view_radius: i32,

    // Prediction.
    seq: u32,
    /// What was sent and not acknowledged yet, with the body it led to here.
    history: VecDeque<(u32, MoveInput, Body)>,
    boxes: Vec<Aabb>,
    pub last_input: MoveInput,

    // Streaming.
    pool: Pool,
    gen_tx: Sender<(ChunkPos, Option<Chunk>)>,
    gen_rx: Receiver<(ChunkPos, Option<Chunk>)>,
    pending: FxMap<ChunkPos, PendingChunk>,
    subscribed: FxMap<ChunkPos, ()>,
    in_flight: usize,
    center: Option<ChunkPos>,
    /// Shared with generation jobs: packed centre, so stale jobs can give up.
    shared_center: Arc<AtomicI64>,
    spiral: Vec<(i32, i32)>,
    spiral_for: i32,
}

fn pack_center(c: ChunkPos) -> i64 {
    ((c.x as i64) << 32) | (c.z as u32 as i64)
}

fn unpack_center(v: i64) -> ChunkPos {
    ChunkPos::new((v >> 32) as i32, v as i32)
}

impl Session {
    /// Says hello on `peer` and waits for the server's answer.
    pub fn connect(peer: Peer, name: &str, password: Option<String>, view_radius: i32, threads: usize, timeout: Duration) -> Result<Self, String> {
        if !peer.send(&C2S::Hello { protocol: PROTOCOL, name: name.into(), password }) {
            return Err("The connection closed straight away.".into());
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("The server did not answer.".into());
            }
            let Some(frame) = peer.recv_frame_timeout(left) else {
                if peer.is_closed() {
                    return Err("The server closed the connection.".into());
                }
                continue;
            };
            match decode::<S2C>(&frame)? {
                S2C::Welcome(w) => return Self::welcomed(peer, *w, view_radius, threads),
                S2C::Reject(reason) => return Err(reason),
                _ => {}
            }
        }
    }

    fn welcomed(peer: Peer, w: Welcome, view_radius: i32, threads: usize) -> Result<Self, String> {
        if w.generator_version != GENERATOR_VERSION {
            return Err(format!(
                "This server uses terrain generator {}, and this client has generator {GENERATOR_VERSION}. Update the older one.",
                w.generator_version
            ));
        }
        let raw = decompress(&w.registry)?;
        let reg = Arc::new(Registry::from_bytes(&raw)?);
        let (gen_tx, gen_rx) = mpsc::channel();
        Ok(Self {
            peer,
            reg,
            generator: Arc::new(Generator::new(w.seed)),
            entity_id: w.entity_id,
            world: ChunkMap::default(),
            body: w.body,
            prev_pos: w.body.pos,
            mode: w.mode,
            server_name: w.server_name,
            sim_distance: w.sim_distance,
            clock: w.clock,
            health: 20,
            air: 20,
            inventory: vec![None; slots::INVENTORY as usize],
            selected: 0,
            cursor: None,
            screen: None,
            entities: FxMap::default(),
            players: Vec::new(),
            cracks: FxMap::default(),
            chat: VecDeque::new(),
            events: Vec::new(),
            corrections: 0,
            disconnected: None,
            view_radius,
            seq: 0,
            history: VecDeque::new(),
            boxes: Vec::new(),
            last_input: MoveInput::default(),
            pool: Pool::new("gen", threads.max(1)),
            gen_tx,
            gen_rx,
            pending: FxMap::default(),
            subscribed: FxMap::default(),
            in_flight: 0,
            center: None,
            shared_center: Arc::new(AtomicI64::new(0)),
            spiral: Vec::new(),
            spiral_for: -1,
        })
    }

    pub fn is_connected(&self) -> bool {
        self.disconnected.is_none()
    }

    pub fn send(&self, msg: &C2S) {
        self.peer.send(msg);
    }

    pub fn held(&self) -> Option<ItemStack> {
        self.inventory.get(self.selected as usize % 9).copied().flatten()
    }

    pub fn loaded_chunks(&self) -> usize {
        self.world.chunks.len()
    }

    /// Chunks wanted and not here yet.
    pub fn pending_chunks(&self) -> usize {
        self.pending.len()
    }

    /// Handles everything that has arrived. Never blocks.
    pub fn poll(&mut self) {
        while let Some(frame) = self.peer.try_recv_frame() {
            match decode::<S2C>(&frame) {
                Ok(msg) => self.handle(msg),
                Err(e) => {
                    self.lost(format!("The server sent something unreadable: {e}"));
                    return;
                }
            }
        }
        if self.peer.is_closed() && self.disconnected.is_none() {
            self.lost("The connection was lost.".into());
        }
        while let Ok((pos, chunk)) = self.gen_rx.try_recv() {
            self.in_flight = self.in_flight.saturating_sub(1);
            let Some(chunk) = chunk else {
                // Given up on as stale; asked for again if it matters after all.
                self.pending.remove(&pos);
                if self.subscribed.remove(&pos).is_some() {
                    self.send(&C2S::Unsubscribe(vec![pos]));
                }
                continue;
            };
            if let Some(p) = self.pending.get_mut(&pos) {
                p.generated = Some(chunk);
                self.try_finish(pos);
            }
        }
    }

    fn lost(&mut self, why: String) {
        self.disconnected = Some(why.clone());
        self.events.push(Event::Disconnected(why));
    }

    fn try_finish(&mut self, pos: ChunkPos) {
        let ready = self.pending.get(&pos).is_some_and(|p| p.generated.is_some() && p.delta.is_some());
        if !ready {
            return;
        }
        let Some(p) = self.pending.remove(&pos) else { return };
        let Some(mut chunk) = p.generated else { return };
        if let Some(sections) = &p.delta {
            if let Err(e) = chunk.apply_encoded(sections) {
                self.lost(format!("The server sent a bad chunk: {e}"));
                return;
            }
        }
        for c in p.blocks {
            chunk.set(c.pos.x & 15, c.pos.y, c.pos.z & 15, c.state);
        }
        self.world.chunks.insert(pos, chunk);
        self.events.push(Event::ChunkReady(pos));
    }

    fn handle(&mut self, msg: S2C) {
        match msg {
            S2C::Welcome(_) | S2C::Reject(_) => {}
            S2C::Deltas(list) => {
                for d in list {
                    let sections = match d.unpack() {
                        Ok(s) => s,
                        Err(e) => {
                            self.lost(e);
                            return;
                        }
                    };
                    match self.pending.get_mut(&d.pos) {
                        Some(p) => {
                            p.delta = Some(sections);
                            self.try_finish(d.pos);
                        }
                        // It arrived for a chunk already here (a resubscribe): apply it over.
                        None => {
                            if let Some(c) = self.world.chunks.get_mut(&d.pos) {
                                let _ = c.apply_encoded(&sections);
                            }
                        }
                    }
                }
            }
            S2C::Blocks(list) => {
                for c in list {
                    self.block_changed(c);
                }
            }
            S2C::Me { ack, body } => self.reconcile(ack, body),
            S2C::Spawn(list) => {
                for e in list {
                    let pos = unquantize(e.pos);
                    self.entities.insert(
                        e.id,
                        RemoteEntity {
                            id: e.id,
                            kind: e.kind,
                            quantized: e.pos,
                            pos,
                            prev: pos,
                            yaw: e.yaw,
                            pitch: e.pitch,
                            prev_yaw: e.yaw,
                            name: e.name,
                            stack: e.stack,
                            block: e.block,
                        },
                    );
                }
            }
            S2C::Moves(list) => {
                for m in list {
                    if let Some(e) = self.entities.get_mut(&m.id) {
                        for axis in 0..3 {
                            e.quantized[axis] += m.d[axis] as i32;
                        }
                        e.prev = e.pos;
                        e.prev_yaw = e.yaw;
                        e.pos = unquantize(e.quantized);
                        e.yaw = m.yaw;
                        e.pitch = m.pitch;
                    }
                }
            }
            S2C::Despawn(list) => {
                for id in list {
                    self.entities.remove(&id);
                }
            }
            S2C::Vitals(v) => {
                if v.health < self.health {
                    self.events.push(Event::Hurt);
                }
                if v.health == 0 && self.health > 0 {
                    self.events.push(Event::Died);
                }
                self.health = v.health;
                self.air = v.air;
                self.mode = v.mode;
            }
            S2C::Inventory { slots, selected } => {
                let count = |list: &[Option<ItemStack>], item: u16| -> u32 {
                    list.iter().flatten().filter(|s| s.item == item).map(|s| s.count as u32).sum()
                };
                let mut seen: Vec<u16> = slots.iter().flatten().map(|s| s.item).collect();
                seen.sort_unstable();
                seen.dedup();
                for item in seen {
                    let (now, then) = (count(&slots, item), count(&self.inventory, item));
                    if now > then {
                        self.events.push(Event::ItemGained { item, count: now - then });
                    }
                }
                self.inventory = slots;
                self.selected = selected;
            }
            S2C::Cursor(c) => self.cursor = c,
            S2C::Screen(s) => {
                let was_station = self.screen.as_ref().is_some_and(|x| x.kind != ScreenKind::Inventory);
                let is_station = s.as_ref().is_some_and(|x| x.kind != ScreenKind::Inventory);
                if let (Some(new), false) = (&s, was_station) {
                    if is_station {
                        self.events.push(Event::ScreenOpened(new.kind));
                    }
                }
                if was_station && !is_station {
                    self.events.push(Event::ScreenClosed);
                }
                self.screen = s;
            }
            S2C::Chat(line) => {
                self.chat.push_back(line.clone());
                if self.chat.len() > 200 {
                    self.chat.pop_front();
                }
                self.events.push(Event::Chat(line));
            }
            S2C::Clock(c) => self.clock = c,
            S2C::Effect(e) => self.events.push(Event::Effect(e)),
            S2C::Cracks { pos, stage } => match stage {
                Some(s) => {
                    self.cracks.insert(pos, s);
                }
                None => {
                    self.cracks.remove(&pos);
                }
            },
            S2C::Players(list) => self.players = list,
            S2C::Ping(n) => self.send(&C2S::Pong(n)),
            S2C::Bye(why) => self.lost(why),
        }
    }

    fn block_changed(&mut self, c: BlockChange) {
        let pos = c.pos.chunk();
        if let Some(p) = self.pending.get_mut(&pos) {
            p.blocks.push(c);
            return;
        }
        if let Some(chunk) = self.world.chunks.get_mut(&pos) {
            let old = chunk.set(c.pos.x & 15, c.pos.y, c.pos.z & 15, c.state);
            if old != c.state {
                self.events.push(Event::BlockChanged { pos: c.pos, old, new: c.state });
            }
            self.cracks.remove(&c.pos);
        }
    }

    /// Lines the client's prediction up with what the server says happened.
    fn reconcile(&mut self, ack: u32, server: Body) {
        // Everything up to `ack` is settled.
        let mut predicted = None;
        while self.history.front().is_some_and(|&(seq, _, _)| seq <= ack) {
            if let Some((seq, _, body)) = self.history.pop_front() {
                if seq == ack {
                    predicted = Some(body);
                }
            }
        }
        let wrong = match predicted {
            Some(p) => p.pos.distance_squared(server.pos) > CORRECTION_EPSILON * CORRECTION_EPSILON || p.flying != server.flying,
            // No record of it: only a server move (a teleport, a respawn) gets us here.
            None => self.history.is_empty() && self.body.pos.distance_squared(server.pos) > CORRECTION_EPSILON,
        };
        if wrong {
            self.corrections += 1;
            self.body = server;
            let reg = self.reg.clone();
            let inputs: Vec<MoveInput> = self.history.iter().map(|h| h.1).collect();
            for (i, input) in inputs.into_iter().enumerate() {
                physics::step(&mut self.body, input, &self.world, &reg, &mut self.boxes);
                self.history[i].2 = self.body;
            }
        }
    }

    /// One fixed tick: moves the predicted body, tells the server, keeps the world streaming.
    pub fn tick(&mut self, input: MoveInput) {
        self.prev_pos = self.body.pos;
        // Entities that did not move this tick stop interpolating.
        for e in self.entities.values_mut() {
            e.prev = e.pos;
            e.prev_yaw = e.yaw;
        }
        self.last_input = input;
        if self.health > 0 {
            self.seq += 1;
            let reg = self.reg.clone();
            physics::step(&mut self.body, input, &self.world, &reg, &mut self.boxes);
            self.history.push_back((self.seq, input, self.body));
            if self.history.len() > 200 {
                self.history.pop_front();
            }
            self.send(&C2S::Input { seq: self.seq, input });
        }
        self.stream();
    }

    /// Where the camera's feet are between ticks.
    pub fn interpolated_pos(&self, alpha: f64) -> DVec3 {
        self.prev_pos.lerp(self.body.pos, alpha)
    }

    fn stream(&mut self) {
        let center = BlockPos::from_vec(self.body.pos).chunk();
        self.shared_center.store(pack_center(center), Ordering::Relaxed);
        if self.spiral_for != self.view_radius {
            self.spiral = spiral(self.view_radius);
            self.spiral_for = self.view_radius;
        }

        // What is too far away goes.
        if self.center != Some(center) {
            self.center = Some(center);
            let keep = self.view_radius + 2;
            let far: Vec<ChunkPos> = self.world.chunks.keys().copied().filter(|p| p.distance(center) > keep).collect();
            let mut gone = Vec::with_capacity(far.len());
            for p in far {
                self.world.chunks.remove(&p);
                self.subscribed.remove(&p);
                gone.push(p);
                self.events.push(Event::ChunkGone(p));
            }
            self.pending.retain(|p, _| {
                let keep_it = p.distance(center) <= keep;
                if !keep_it {
                    gone.push(*p);
                }
                keep_it
            });
            if !gone.is_empty() {
                for p in &gone {
                    self.subscribed.remove(p);
                }
                self.send(&C2S::Unsubscribe(gone));
            }
        }

        // What is missing gets asked for, closest first, as fast as the workers can use it.
        let limit = self.pool.thread_count() * 4;
        let mut asked = Vec::new();
        let mut started = 0;
        for i in 0..self.spiral.len() {
            if self.in_flight >= limit || started >= MAX_NEW_PER_TICK {
                break;
            }
            let (dx, dz) = self.spiral[i];
            let pos = ChunkPos::new(center.x + dx, center.z + dz);
            if self.world.chunks.contains_key(&pos) || self.pending.contains_key(&pos) {
                continue;
            }
            self.pending.insert(pos, PendingChunk { generated: None, delta: None, blocks: Vec::new() });
            self.subscribed.insert(pos, ());
            asked.push(pos);
            self.in_flight += 1;
            started += 1;
            let (generator, tx, shared, radius) = (self.generator.clone(), self.gen_tx.clone(), self.shared_center.clone(), self.view_radius);
            self.pool.submit((dx * dx + dz * dz) as i64, move || {
                // The player may have flown off while this waited.
                let now = unpack_center(shared.load(Ordering::Relaxed));
                let chunk = (pos.distance(now) <= radius + 2).then(|| generator.chunk(pos));
                let _ = tx.send((pos, chunk));
            });
        }
        if !asked.is_empty() {
            self.send(&C2S::Subscribe(asked));
        }
    }

    pub fn dig(&self, pos: BlockPos, face: u8) {
        self.send(&C2S::Dig { pos, face });
    }

    /// Whether everything within `radius` chunks of the player has arrived.
    pub fn ready_within(&self, radius: i32) -> bool {
        let c = BlockPos::from_vec(self.body.pos).chunk();
        (-radius..=radius).all(|dx| (-radius..=radius).all(|dz| self.world.chunks.contains_key(&ChunkPos::new(c.x + dx, c.z + dz))))
    }

    /// The block under the crosshair for this input's view.
    pub fn target(&self, look: DVec3, reach: f64) -> Option<zenith_world::pick::Hit> {
        zenith_world::pick::pick(&self.world, &self.reg, self.body.eye(), look, reach)
    }

    pub fn block(&self, p: BlockPos) -> BlockState {
        self.world.block(p)
    }
}

/// Offsets within `radius`, closest first.
fn spiral(radius: i32) -> Vec<(i32, i32)> {
    let mut v: Vec<(i32, i32)> = Vec::with_capacity(((radius * 2 + 1) * (radius * 2 + 1)) as usize);
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dz * dz <= radius * radius + radius {
                v.push((dx, dz));
            }
        }
    }
    v.sort_by_key(|&(dx, dz)| dx * dx + dz * dz);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spiral_starts_at_the_middle_and_only_goes_outwards() {
        let s = spiral(5);
        assert_eq!(s[0], (0, 0));
        let mut last = 0;
        for &(dx, dz) in &s {
            let d = dx * dx + dz * dz;
            assert!(d >= last);
            last = d;
        }
        assert!(s.len() > 70);
    }

    #[test]
    fn the_shared_centre_packs_both_coordinates() {
        for c in [ChunkPos::new(0, 0), ChunkPos::new(-5, 7), ChunkPos::new(100_000, -100_000)] {
            assert_eq!(unpack_center(pack_center(c)), c);
        }
    }
}
