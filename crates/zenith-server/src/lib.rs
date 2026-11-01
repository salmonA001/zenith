//! The authoritative game. This crate knows nothing of windows or GPUs: the dedicated server and a
//! single-player world are this same code, reached over the same protocol.

pub mod command;
pub mod entity;
pub mod inventory;
pub mod persist;
pub mod player;
pub mod rules;
pub mod world;

use entity::{Damage, Data, Entities, Grid, PlayerView, Sent};
use persist::{Disk, ItemRemap, LevelMeta};
use player::Player;
use std::collections::VecDeque;
use std::net::{TcpListener, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use world::ServerWorld;
use zenith_content::{ItemStack, Pack, Registry, Stable};
use zenith_core::glam::DVec3;
use zenith_core::hash::{fnv64, fnv64_more};
use zenith_core::pos::SEA_LEVEL;
use zenith_core::{Aabb, BlockPos, ChunkPos, FxMap, FxSet};
use zenith_protocol::*;
use zenith_world::physics::Body;
use zenith_world::{Generator, GENERATOR_VERSION};

pub const TICKS_PER_SECOND: u64 = 20;
const TICK: Duration = Duration::from_millis(1000 / TICKS_PER_SECOND);
/// How far a player sees other entities, in chunks.
const TRACK_RADIUS: i32 = 6;
/// A connection has this long to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Most messages taken from one player in one tick; the rest wait, so nobody can stall a tick.
const MAX_MESSAGES: usize = 512;
const MAX_SUBSCRIBE: usize = 4096;

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub name: String,
    /// Where the world lives. `None` keeps it in memory.
    pub world_dir: Option<PathBuf>,
    /// Used only when a new world is created.
    pub seed: u64,
    pub default_mode: GameMode,
    /// The radius around each player that is simulated, in chunks.
    pub sim_distance: i32,
    pub max_players: usize,
    pub password: Option<String>,
    pub ops: Vec<String>,
    /// Single player: the one player in the world can do anything.
    pub everyone_op: bool,
    /// Background threads for chunks. 0 keeps everything on the tick, exactly reproducibly.
    pub threads: usize,
    pub autosave_ticks: u64,
    pub packs: Vec<Pack>,
    pub spawn_mobs: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "A Zenith server".into(),
            world_dir: None,
            seed: 0,
            default_mode: GameMode::Survival,
            sim_distance: 6,
            max_players: 200,
            password: None,
            ops: Vec::new(),
            everyone_op: false,
            threads: 2,
            autosave_ticks: 20 * 60,
            packs: Vec::new(),
            spawn_mobs: true,
        }
    }
}

/// How long ticks have been taking.
#[derive(Default)]
pub struct TickStats {
    durations: VecDeque<Duration>,
    started: Option<Instant>,
    ticks: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StatsSummary {
    pub tps: f64,
    pub mean_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl TickStats {
    fn record(&mut self, d: Duration) {
        self.started.get_or_insert_with(Instant::now);
        self.ticks += 1;
        self.durations.push_back(d);
        if self.durations.len() > 600 {
            self.durations.pop_front();
        }
    }
    pub fn summary(&self) -> StatsSummary {
        if self.durations.is_empty() {
            return StatsSummary::default();
        }
        let mut sorted: Vec<f64> = self.durations.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        sorted.sort_by(f64::total_cmp);
        let p99 = sorted[((sorted.len() as f64 * 0.99) as usize).min(sorted.len() - 1)];
        let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
        let elapsed = self.started.map_or(0.0, |s| s.elapsed().as_secs_f64()).max(1e-3);
        StatsSummary { tps: self.ticks as f64 / elapsed, mean_ms: mean, p99_ms: p99, max_ms: sorted[sorted.len() - 1] }
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

struct Pending {
    peer: Peer,
    deadline: Instant,
}

struct DiskDelta {
    player: u32,
    pos: ChunkPos,
    sections: Vec<(u8, Vec<u8>)>,
}

pub struct Server {
    pub cfg: ServerConfig,
    pub world: ServerWorld,
    pub entities: Entities,
    pub players: Vec<Player>,
    pending: Vec<Pending>,
    pub clock: Clock,
    pub rain_target: u8,
    pub next_weather: u64,
    pub spawn: BlockPos,
    grid: Grid,
    damage: Vec<Damage>,
    pub stats: TickStats,
    delta_tx: Sender<DiskDelta>,
    delta_rx: Receiver<DiskDelta>,
    stable: Stable,
    next_ping: u32,
    chunks_buf: Vec<ChunkPos>,
}

impl Server {
    /// Opens the world in `cfg.world_dir`, or makes a new one.
    pub fn new(cfg: ServerConfig) -> Result<Self, String> {
        let dir = cfg.world_dir.clone();
        let level = match &dir {
            Some(d) => persist::read_level(d).map_err(|e| format!("cannot read the level file: {e}"))?,
            None => None,
        };
        if let Some(l) = &level {
            if l.generator_version != GENERATOR_VERSION {
                return Err(format!(
                    "this world was made by terrain generator {}, and this build has generator {}: \
                     the land it never touched would come out different. Refusing to open it.",
                    l.generator_version, GENERATOR_VERSION
                ));
            }
        }
        let stable = level.as_ref().map_or_else(Stable::default, |l| l.stable.clone());
        let reg = Arc::new(Registry::build(&cfg.packs, &stable)?);
        let seed = level.as_ref().map_or(cfg.seed, |l| l.seed);
        let generator = Arc::new(Generator::new(seed));
        let (disk, saved) = match &dir {
            Some(d) => {
                let (disk, saved) = Disk::open(d).map_err(|e| format!("cannot open the world's regions: {e}"))?;
                (Some(disk), saved)
            }
            None => (None, Vec::new()),
        };
        let saved_names = level.as_ref().map_or_else(|| reg.items().iter().map(|i| i.name.clone()).collect(), |l| l.items.clone());
        let remap = ItemRemap::new(&saved_names, &reg);
        let mut world = ServerWorld::new(reg.clone(), generator.clone(), seed, cfg.threads, disk, saved, remap);

        let spawn = match &level {
            Some(l) => BlockPos::new(l.spawn[0], l.spawn[1], l.spawn[2]),
            None => {
                let (x, y, z) = generator.find_spawn();
                BlockPos::new(x, y, z)
            }
        };
        let clock = level.as_ref().map_or(Clock { tick: 0, time: 1000, rain: 0 }, |l| Clock { tick: l.tick, time: l.time, rain: l.rain });
        world.tick = clock.tick;
        let (delta_tx, delta_rx) = mpsc::channel();
        let server = Self {
            world,
            entities: Entities::default(),
            players: Vec::new(),
            pending: Vec::new(),
            clock,
            rain_target: clock.rain,
            next_weather: clock.tick + 20 * 60 * 10,
            spawn,
            grid: Grid::default(),
            damage: Vec::new(),
            stats: TickStats::default(),
            delta_tx,
            delta_rx,
            stable: reg.stable().clone(),
            next_ping: 1,
            chunks_buf: Vec::new(),
            cfg,
        };
        if level.is_none() {
            server.write_level().map_err(|e| format!("cannot write the level file: {e}"))?;
        }
        Ok(server)
    }

    fn write_level(&self) -> std::io::Result<()> {
        let Some(dir) = &self.cfg.world_dir else { return Ok(()) };
        let level = LevelMeta {
            name: self.cfg.name.clone(),
            seed: self.world.generator.seed,
            generator_version: GENERATOR_VERSION,
            tick: self.clock.tick,
            time: self.clock.time,
            rain: self.clock.rain,
            spawn: [self.spawn.x, self.spawn.y, self.spawn.z],
            default_mode: self.cfg.default_mode,
            stable: self.stable.clone(),
            items: self.world.reg.items().iter().map(|i| i.name.clone()).collect(),
        };
        persist::write_level(dir, &level)
    }

    /// A place to stand at the spawn point: above whatever is built or grown there.
    pub fn spawn_point(&mut self) -> DVec3 {
        let pos = self.spawn.chunk();
        self.world.request(pos, 0);
        let reg = self.world.reg.clone();
        let c = self.world.map.get(pos).cloned().unwrap_or_else(|| self.world.generator.chunk(pos));
        let top = c.top(self.spawn.x & 15, self.spawn.z & 15, |s| reg.lut.collides[s.kind() as usize]).unwrap_or(SEA_LEVEL);
        DVec3::new(self.spawn.x as f64 + 0.5, top as f64 + 1.0, self.spawn.z as f64 + 0.5)
    }

    /// Takes in a new connection. It joins when it has said hello.
    pub fn add_peer(&mut self, peer: Peer) {
        self.pending.push(Pending { peer, deadline: Instant::now() + HELLO_TIMEOUT });
    }

    fn admit(&mut self) {
        let mut i = 0;
        while i < self.pending.len() {
            let frame = self.pending[i].peer.try_recv_frame();
            if self.pending[i].peer.is_closed() || Instant::now() > self.pending[i].deadline {
                self.pending.swap_remove(i);
                continue;
            }
            let Some(frame) = frame else {
                i += 1;
                continue;
            };
            let pending = self.pending.swap_remove(i);
            match decode::<C2S>(&frame) {
                Ok(C2S::Hello { protocol, name, password }) => self.greet(pending.peer, protocol, name, password),
                _ => {
                    pending.peer.send(&S2C::Reject("Say hello first.".into()));
                }
            }
        }
    }

    fn greet(&mut self, peer: Peer, protocol: u32, name: String, password: Option<String>) {
        let refusal = if protocol != PROTOCOL {
            Some(format!("This server speaks protocol {PROTOCOL}, and you speak {protocol}."))
        } else if !persist::valid_name(&name) {
            Some("A name is 1 to 24 letters, digits, - or _.".to_string())
        } else if self.cfg.password.is_some() && self.cfg.password != password {
            Some("Wrong password.".to_string())
        } else if self.players.iter().any(|p| p.name == name) {
            Some(format!("{name} is already playing."))
        } else if self.players.len() >= self.cfg.max_players {
            Some("The server is full.".to_string())
        } else {
            None
        };
        if let Some(reason) = refusal {
            peer.send(&S2C::Reject(reason));
            // Let the frame go out; the close comes when the peer is dropped by the far end.
            return;
        }

        let id = self.entities.new_id();
        let op = self.cfg.everyone_op || self.cfg.ops.contains(&name);
        let saved = self.cfg.world_dir.as_deref().and_then(|d| persist::read_player(d, &name));
        let mut player = match saved {
            Some(mut save) => {
                if let Some(dir) = &self.cfg.world_dir {
                    if let Ok(Some(level)) = persist::read_level(dir) {
                        ItemRemap::new(&level.items, &self.world.reg).inventory(&mut save.inventory);
                    }
                }
                let mut p = Player::from_save(id, name.clone(), peer, save);
                p.op |= op;
                p
            }
            None => {
                let at = self.spawn_point();
                Player::new(id, name.clone(), peer, Body::at(at), self.cfg.default_mode, op)
            }
        };
        player.sent = Sent::of(player.body.pos, 0, 0);
        let registry = lz4_flex::compress_prepend_size(&self.world.reg.to_bytes());
        player.peer.send(&S2C::Welcome(Box::new(Welcome {
            entity_id: id,
            seed: self.world.generator.seed,
            generator_version: GENERATOR_VERSION,
            registry,
            body: player.body,
            mode: player.mode,
            clock: self.clock,
            server_name: self.cfg.name.clone(),
            sim_distance: self.cfg.sim_distance as u8,
        })));
        self.players.push(player);
        self.announce_players();
        self.broadcast_chat(None, format!("{name} joined."));
    }

    fn announce_players(&self) {
        let list: Vec<PlayerEntry> = self.players.iter().map(|p| PlayerEntry { name: p.name.clone(), ping_ms: p.ping_ms }).collect();
        let frame = encode(&S2C::Players(list));
        for p in &self.players {
            p.peer.send_frame(frame.clone());
        }
    }

    pub fn broadcast_chat(&self, from: Option<String>, text: String) {
        let frame = encode(&S2C::Chat(ChatLine { from, text }));
        for p in &self.players {
            p.peer.send_frame(frame.clone());
        }
    }

    /// Moves a player, telling their client.
    pub fn teleport(&mut self, i: usize, at: DVec3) {
        let p = &mut self.players[i];
        p.body.pos = at;
        p.body.vel = DVec3::ZERO;
        p.body.fall = 0.0;
        p.inputs.clear();
    }

    /// Runs one tick of the whole game.
    pub fn tick(&mut self) {
        let started = Instant::now();
        self.admit();
        self.world.pump();
        self.read_messages();
        self.load_around_players();

        self.world.tick += 1;
        self.clock.tick = self.world.tick;
        self.clock.time = (self.clock.time + 1) % 24000;
        self.weather();

        for i in 0..self.players.len() {
            self.players[i].tick(&mut self.world);
        }
        rules::run(&mut self.world);
        self.tick_furnaces();

        let views: Vec<PlayerView> = self.players.iter().map(Player::view).collect();
        let mut damage = std::mem::take(&mut self.damage);
        entity::tick_all(&mut self.entities, &mut self.world, &views, &mut damage);
        self.apply_damage(&mut damage);
        self.damage = damage;
        self.entities.take_world_events(&mut self.world);
        self.grid.rebuild(&self.entities);
        self.pick_up_items();

        if self.cfg.spawn_mobs && self.world.tick % 20 == 0 {
            let night = (12500..23500).contains(&self.clock.time);
            entity::natural_spawns(&mut self.entities, &mut self.world, &views, night);
        }
        self.random_ticks();
        self.send_updates();
        self.drop_gone();

        if self.world.tick % self.cfg.autosave_ticks == 0 {
            if let Err(e) = self.save() {
                eprintln!("zenith: autosave failed: {e}");
            }
        }
        self.stats.record(started.elapsed());
    }

    fn read_messages(&mut self) {
        for i in 0..self.players.len() {
            for _ in 0..MAX_MESSAGES {
                let Some(frame) = self.players[i].peer.try_recv_frame() else { break };
                match decode::<C2S>(&frame) {
                    Ok(msg) => self.handle(i, msg),
                    Err(_) => {
                        // A client speaking nonsense gets no second chance.
                        self.players[i].peer.send(&S2C::Bye("Bad message.".into()));
                        self.players[i].peer.close();
                        break;
                    }
                }
            }
        }
    }

    fn handle(&mut self, i: usize, msg: C2S) {
        match msg {
            C2S::Hello { .. } => {}
            C2S::Input { seq, input } => {
                let p = &mut self.players[i];
                // Out of order or replayed inputs are nonsense from a reliable stream.
                if seq > p.ack && p.inputs.back().is_none_or(|&(last, _)| seq > last) {
                    p.inputs.push_back((seq, input));
                    if p.inputs.len() > 16 {
                        p.inputs.pop_front();
                    }
                }
            }
            C2S::Subscribe(list) => self.subscribe(i, list),
            C2S::Unsubscribe(list) => {
                let p = &mut self.players[i];
                for pos in list {
                    p.subscribed.remove(&pos);
                }
            }
            C2S::Dig { pos, .. } => self.players[i].start_dig(&mut self.world, pos),
            C2S::CancelDig => self.players[i].cancel_dig(&mut self.world),
            C2S::UseOn { pos, face } => {
                let others: Vec<Aabb> = self
                    .players
                    .iter()
                    .enumerate()
                    .filter(|&(j, p)| j != i && !p.dead)
                    .map(|(_, p)| p.body.aabb(0.6, 1.8))
                    .collect();
                self.players[i].use_on(&mut self.world, &others, &self.entities, pos, face);
            }
            C2S::UseItem => self.players[i].use_item(&mut self.world),
            C2S::Attack { entity } => {
                let views: Vec<PlayerView> = self.players.iter().map(Player::view).collect();
                if let Some(d) = self.players[i].attack(&mut self.world, &mut self.entities, &views, entity) {
                    self.damage.push(d);
                }
            }
            C2S::SelectSlot(slot) => self.players[i].select(slot),
            C2S::DropItem { all } => {
                if let Some((pos, vel, stack)) = self.players[i].throw_held(all) {
                    let id = self.entities.spawn(EntityKind::Item, pos, Data::Item { stack, delay: 30 });
                    if let Some(e) = self.entities.list.get_mut(&id) {
                        e.body.vel = vel;
                    }
                }
            }
            C2S::Click { slot, button } => self.players[i].click(&mut self.world, slot, button),
            C2S::CloseScreen => self.players[i].close_screen(&mut self.world),
            C2S::Pick { item } => {
                let reg = self.world.reg.clone();
                let p = &mut self.players[i];
                if p.mode == GameMode::Creative {
                    if let Some(def) = reg.item(item) {
                        p.inv.cursor = Some(ItemStack::new(item, def.max_stack.max(1)));
                    }
                }
            }
            C2S::SetFlying(on) => {
                let p = &mut self.players[i];
                if p.mode == GameMode::Creative {
                    p.body.flying = on;
                }
            }
            C2S::Chat(text) => self.chat(i, text),
            C2S::Respawn => {
                let at = self.spawn_point();
                self.players[i].respawn(at);
            }
            C2S::Pong(n) => {
                let p = &mut self.players[i];
                if let Some((sent, at)) = p.ping_sent {
                    if sent == n {
                        p.ping_ms = at.elapsed().as_millis().min(u16::MAX as u128) as u16;
                        p.ping_sent = None;
                    }
                }
            }
        }
    }

    fn chat(&mut self, i: usize, text: String) {
        let tick = self.world.tick;
        if !self.players[i].may_chat(tick) {
            return;
        }
        let text: String = text.chars().take(256).filter(|c| !c.is_control()).collect();
        if let Some(line) = text.strip_prefix('/') {
            let replies = command::run(self, Some(i), line);
            if let Some(p) = self.players.get(i) {
                for reply in replies {
                    p.peer.send(&S2C::Chat(ChatLine { from: None, text: reply }));
                }
            }
        } else if !text.trim().is_empty() {
            let from = self.players[i].name.clone();
            self.broadcast_chat(Some(from), text);
        }
    }

    fn subscribe(&mut self, i: usize, list: Vec<ChunkPos>) {
        let player_id = self.players[i].id;
        for pos in list.into_iter().take(MAX_SUBSCRIBE) {
            self.players[i].subscribed.insert(pos);
            if let Some(sections) = self.world.modified_sections(pos) {
                self.players[i].out_deltas.push(ChunkDelta::pack(pos, &sections));
            } else if self.world.saved.contains(&pos) {
                if let Some(disk) = &self.world.disk {
                    let tx = self.delta_tx.clone();
                    disk.read(pos, move |bytes| {
                        let sections = bytes.as_deref().and_then(persist::ChunkRecord::from_bytes).map(|r| r.sections).unwrap_or_default();
                        let _ = tx.send(DiskDelta { player: player_id, pos, sections });
                    });
                } else {
                    self.players[i].out_deltas.push(ChunkDelta::pack(pos, &[]));
                }
            } else {
                // Exactly what the generator makes, which the client can do for itself.
                self.players[i].out_deltas.push(ChunkDelta::pack(pos, &[]));
            }
        }
    }

    fn load_around_players(&mut self) {
        // What finished reading from disk goes out, newest state first: if the chunk has since
        // come into memory, that copy is the current one.
        while let Ok(d) = self.delta_rx.try_recv() {
            let current = self.world.modified_sections(d.pos).unwrap_or(d.sections);
            if let Some(p) = self.players.iter_mut().find(|p| p.id == d.player) {
                if p.subscribed.contains(&d.pos) {
                    p.out_deltas.push(ChunkDelta::pack(d.pos, &current));
                }
            }
        }

        let radius = self.cfg.sim_distance;
        for p in &self.players {
            let c = p.chunk();
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    let priority = (dx * dx + dz * dz) as i64;
                    if self.world.map.chunks.contains_key(&ChunkPos::new(c.x + dx, c.z + dz)) {
                        continue;
                    }
                    self.world.request(ChunkPos::new(c.x + dx, c.z + dz), priority);
                }
            }
        }
        if self.world.tick % 40 == 0 {
            let keep = radius + 2;
            let centers: Vec<ChunkPos> = self.players.iter().map(Player::chunk).collect();
            let far: Vec<ChunkPos> =
                self.world.map.chunks.keys().copied().filter(|pos| centers.iter().all(|c| c.distance(*pos) > keep)).collect();
            for pos in far {
                if self.world.can_unload(pos) {
                    self.world.unload(pos);
                }
            }
            let (world, ents) = (&self.world, &mut self.entities);
            // What was in a chunk goes with it: nothing simulates where nobody is.
            ents.list.retain(|_, e| world.map.chunks.contains_key(&BlockPos::from_vec(e.body.pos).chunk()) || e.age < 40);
        }
    }

    fn weather(&mut self) {
        if self.world.tick >= self.next_weather {
            if self.rain_target == 0 {
                self.rain_target = 255;
                self.next_weather = self.world.tick + self.world.rng.range(20 * 120, 20 * 300) as u64;
            } else {
                self.rain_target = 0;
                self.next_weather = self.world.tick + self.world.rng.range(20 * 300, 20 * 1200) as u64;
            }
        }
        if self.clock.rain < self.rain_target && self.world.tick % 2 == 0 {
            self.clock.rain += 1;
        } else if self.clock.rain > self.rain_target && self.world.tick % 2 == 0 {
            self.clock.rain -= 1;
        }
    }

    fn tick_furnaces(&mut self) {
        let reg = self.world.reg.clone();
        let mut changed = Vec::new();
        for (pos, be) in &mut self.world.block_entities {
            if let persist::BlockEntity::Furnace(f) = be {
                if f.tick(&reg) {
                    changed.push(pos.chunk());
                }
            }
        }
        for c in changed {
            self.world.mark_dirty(c);
        }
    }

    fn random_ticks(&mut self) {
        self.chunks_buf.clear();
        let radius = self.cfg.sim_distance;
        let centers: Vec<ChunkPos> = self.players.iter().filter(|p| !p.dead).map(Player::chunk).collect();
        self.chunks_buf.extend(self.world.map.chunks.keys().copied().filter(|pos| centers.iter().any(|c| c.distance(*pos) <= radius)));
        // The map's iteration order depends on its history. The simulation must not.
        self.chunks_buf.sort_unstable();
        let chunks = std::mem::take(&mut self.chunks_buf);
        rules::random_ticks(&mut self.world, &chunks);
        self.chunks_buf = chunks;
    }

    fn apply_damage(&mut self, damage: &mut Vec<Damage>) {
        for d in damage.drain(..) {
            if let Some(p) = self.players.iter_mut().find(|p| p.id == d.target) {
                p.take_damage(&mut self.world, d.amount, Some(d.knock));
            }
        }
    }

    fn pick_up_items(&mut self) {
        let reg = self.world.reg.clone();
        for player in &mut self.players {
            if player.dead {
                continue;
            }
            let c = player.chunk();
            let mut near = Vec::new();
            for dz in -1..=1 {
                for dx in -1..=1 {
                    if let Some(ids) = self.grid.cells.get(&ChunkPos::new(c.x + dx, c.z + dz)) {
                        near.extend_from_slice(ids);
                    }
                }
            }
            for id in near {
                let Some(e) = self.entities.list.get_mut(&id) else { continue };
                let Data::Item { stack, delay } = &mut e.data else { continue };
                if *delay > 0 {
                    continue;
                }
                let to = player.body.pos + DVec3::Y * 0.5 - e.body.pos;
                let distance2 = to.length_squared();
                if distance2 > 2.25 {
                    // Close by, it drifts over: nobody should have to stand on a drop to get it.
                    if distance2 < 16.0 {
                        e.body.vel += to.normalize_or_zero() * 0.03;
                    }
                    continue;
                }
                match player.pick_up(&reg, *stack) {
                    None => {
                        self.world.effects.push(Effect {
                            kind: EffectKind::Pickup,
                            pos: [player.body.pos.x as f32, player.body.pos.y as f32, player.body.pos.z as f32],
                        });
                        self.entities.list.remove(&id);
                    }
                    Some(left) => *stack = left,
                }
            }
        }
    }

    fn send_updates(&mut self) {
        // Every entity moves once, whoever is watching.
        let mut moves: FxMap<u32, EntityMove> = FxMap::default();
        let mut jumped: Vec<u32> = Vec::new();
        for e in self.entities.list.values_mut() {
            let now = Sent::of(e.body.pos, e.yaw, e.pitch);
            match e.sent.move_to(e.id, now) {
                Ok(Some(m)) => {
                    moves.insert(e.id, m);
                }
                Ok(None) => {}
                Err(()) => jumped.push(e.id),
            }
            e.sent = now;
        }
        for p in &mut self.players {
            let now = Sent::of(p.body.pos, p.last_input.yaw, p.last_input.pitch);
            match p.sent.move_to(p.id, now) {
                Ok(Some(m)) => {
                    moves.insert(p.id, m);
                }
                Ok(None) => {}
                Err(()) => jumped.push(p.id),
            }
            p.sent = now;
        }

        let changes = std::mem::take(&mut self.world.changes);
        let effects = std::mem::take(&mut self.world.effects);
        let clock_frame = (self.world.tick % 20 == 0).then(|| encode(&S2C::Clock(self.clock)));
        let cracks: Vec<(u32, BlockPos, u8)> =
            self.players.iter().filter_map(|p| p.dig_stage().map(|(pos, stage)| (p.id, pos, stage))).collect();
        let ping = (self.world.tick % 40 == 0).then(|| {
            self.next_ping += 1;
            self.next_ping
        });

        let player_infos: Vec<(u32, DVec3, EntityInfo, bool)> =
            self.players.iter().map(|p| (p.id, p.body.pos, p.info(), p.dead)).collect();
        let mut visible: FxSet<u32> = FxSet::default();
        let mut spawns = Vec::new();
        let mut gone = Vec::new();
        let mut moved = Vec::new();
        for p in &mut self.players {
            p.sync(&mut self.world);

            for c in &changes {
                if p.subscribed.contains(&c.pos.chunk()) {
                    p.out_blocks.push(*c);
                }
            }
            if !p.out_blocks.is_empty() {
                p.peer.send(&S2C::Blocks(std::mem::take(&mut p.out_blocks)));
            }
            if !p.out_deltas.is_empty() {
                p.peer.send(&S2C::Deltas(std::mem::take(&mut p.out_deltas)));
            }
            p.peer.send(&S2C::Me { ack: p.ack, body: p.body });

            // What this player can see.
            visible.clear();
            spawns.clear();
            moved.clear();
            let center = p.chunk();
            for dz in -TRACK_RADIUS..=TRACK_RADIUS {
                for dx in -TRACK_RADIUS..=TRACK_RADIUS {
                    let Some(ids) = self.grid.cells.get(&ChunkPos::new(center.x + dx, center.z + dz)) else { continue };
                    for &id in ids {
                        visible.insert(id);
                        if p.known.insert(id) {
                            if let Some(e) = self.entities.list.get(&id) {
                                spawns.push(e.info());
                            }
                        } else if let Some(m) = moves.get(&id) {
                            moved.push(*m);
                        }
                    }
                }
            }
            for (id, pos, info, dead) in &player_infos {
                if *id == p.id || *dead {
                    continue;
                }
                let c = BlockPos::from_vec(*pos).chunk();
                if c.distance(center) <= TRACK_RADIUS {
                    visible.insert(*id);
                    if p.known.insert(*id) {
                        spawns.push(info.clone());
                    } else if let Some(m) = moves.get(id) {
                        moved.push(*m);
                    }
                }
            }
            gone.clear();
            gone.extend(p.known.iter().copied().filter(|id| !visible.contains(id)));
            for id in &jumped {
                if visible.contains(id) && p.known.contains(id) && !gone.contains(id) {
                    gone.push(*id);
                }
            }
            for id in &gone {
                p.known.remove(id);
            }
            if !gone.is_empty() {
                gone.sort_unstable();
                p.peer.send(&S2C::Despawn(gone.clone()));
            }
            if !spawns.is_empty() {
                p.peer.send(&S2C::Spawn(std::mem::take(&mut spawns)));
            }
            if !moved.is_empty() {
                moved.sort_unstable_by_key(|m| m.id);
                p.peer.send(&S2C::Moves(moved.clone()));
            }

            for e in &effects {
                let d = p.body.pos - DVec3::new(e.pos[0] as f64, e.pos[1] as f64, e.pos[2] as f64);
                if d.length_squared() < 64.0 * 64.0 {
                    p.peer.send(&S2C::Effect(*e));
                }
            }
            for (who, pos, stage) in &cracks {
                if *who != p.id && pos.center().distance_squared(p.body.pos) < 40.0 * 40.0 {
                    p.peer.send(&S2C::Cracks { pos: *pos, stage: Some(*stage) });
                }
            }
            if let Some(frame) = &clock_frame {
                p.peer.send_frame(frame.clone());
            }
            if let Some(n) = ping {
                p.ping_sent = Some((n, Instant::now()));
                p.peer.send(&S2C::Ping(n));
            }
        }
    }

    fn drop_gone(&mut self) {
        let mut left = false;
        let mut i = 0;
        while i < self.players.len() {
            if self.players[i].peer.is_closed() {
                let mut p = self.players.remove(i);
                p.close_screen(&mut self.world);
                if let Some(dir) = &self.cfg.world_dir {
                    if let Err(e) = persist::write_player(dir, &p.name, &p.save()) {
                        eprintln!("zenith: could not save {}: {e}", p.name);
                    }
                }
                self.broadcast_chat(None, format!("{} left.", p.name));
                left = true;
            } else {
                i += 1;
            }
        }
        if left {
            self.announce_players();
        }
    }

    /// Saves everything. Returns how many chunks were written. The tick pays only for snapshots.
    pub fn save(&mut self) -> std::io::Result<usize> {
        let n = self.world.save_dirty();
        self.write_level()?;
        if let Some(dir) = &self.cfg.world_dir {
            for p in &self.players {
                persist::write_player(dir, &p.name, &p.save())?;
            }
        }
        Ok(n)
    }

    /// Saves everything and waits for the disk.
    pub fn save_and_wait(&mut self) -> std::io::Result<()> {
        self.save()?;
        if let Some(disk) = &self.world.disk {
            disk.commit_wait()?;
        }
        Ok(())
    }

    /// A digest of the whole simulation state, for proving two runs did the same thing.
    pub fn state_hash(&self) -> u64 {
        let mut h = fnv64(&self.clock.tick.to_le_bytes());
        let mut positions: Vec<ChunkPos> = self.world.map.chunks.keys().copied().collect();
        positions.sort_unstable();
        for pos in positions {
            let chunk = &self.world.map.chunks[&pos];
            if chunk.modified != 0 {
                for (i, bytes) in chunk.encode_modified() {
                    h = fnv64_more(h, &[i]);
                    h = fnv64_more(h, &bytes);
                }
            }
        }
        for p in &self.players {
            for v in [p.body.pos.x, p.body.pos.y, p.body.pos.z, p.body.vel.x, p.body.vel.y, p.body.vel.z] {
                h = fnv64_more(h, &v.to_bits().to_le_bytes());
            }
            h = fnv64_more(h, &[p.health, p.air]);
        }
        for e in self.entities.list.values() {
            h = fnv64_more(h, &e.id.to_le_bytes());
            for v in [e.body.pos.x, e.body.pos.y, e.body.pos.z] {
                h = fnv64_more(h, &v.to_bits().to_le_bytes());
            }
        }
        h
    }

    /// Runs at 20 ticks a second until `stop` is set, taking new connections from `peers`.
    pub fn run(mut self, stop: Arc<AtomicBool>, peers: Receiver<Peer>) -> Result<(), String> {
        let mut next = Instant::now();
        while !stop.load(Ordering::Acquire) {
            while let Ok(peer) = peers.try_recv() {
                self.add_peer(peer);
            }
            self.tick();
            next += TICK;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > Duration::from_secs(2) {
                // Far behind: skip ahead rather than racing to catch up, like a clock that stops.
                eprintln!("zenith: the server fell {:.1} s behind", (now - next).as_secs_f64());
                next = now;
            }
        }
        self.shutdown()
    }

    /// Says goodbye to everyone and saves.
    pub fn shutdown(&mut self) -> Result<(), String> {
        for p in &self.players {
            p.peer.send(&S2C::Bye("The server stopped.".into()));
        }
        // Screens hand their items back before the save.
        for i in 0..self.players.len() {
            self.players[i].close_screen(&mut self.world);
        }
        self.save_and_wait().map_err(|e| e.to_string())
    }
}

/// Accepts connections on a socket and hands them over as peers, until `stop` is set.
pub fn listen(addr: impl ToSocketAddrs, stop: Arc<AtomicBool>) -> std::io::Result<(std::net::SocketAddr, Receiver<Peer>)> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("accept".into()).spawn(move || {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let ready = stream.set_nonblocking(false).and_then(|()| tcp_peer(stream));
                    match ready {
                        Ok(peer) => {
                            if tx.send(peer).is_err() {
                                return;
                            }
                        }
                        Err(e) => eprintln!("zenith: could not accept a connection: {e}"),
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    eprintln!("zenith: accept failed: {e}");
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    })?;
    Ok((local, rx))
}

/// Reads every `*.json` content pack in a folder, in name order.
pub fn load_packs(dir: &Path) -> Result<Vec<Pack>, String> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect(),
        Err(_) => return Ok(Vec::new()),
    };
    files.sort();
    files
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            Registry::parse_pack(&text).map_err(|e| format!("{}: {e}", path.display()))
        })
        .collect()
}

#[cfg(test)]
mod tests;
