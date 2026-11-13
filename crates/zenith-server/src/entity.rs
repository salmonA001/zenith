//! Everything that moves and is not a player: items, falling blocks, charges, arrows and mobs.
use crate::rails;
use crate::rules;
use crate::world::{ServerWorld, WorldEvent};
use std::collections::BTreeMap;
use zenith_content::{BlockState, ItemKind, ItemStack, Registry, id, it};
use zenith_core::glam::DVec3;
use zenith_core::{Aabb, BlockPos, ChunkPos, Dir, FxMap, Rng, dmath};
use zenith_protocol::{EffectKind, EntityInfo, EntityKind, EntityMove, POS_SCALE};
use zenith_world::physics::{self, Body};
use zenith_world::{BlockAccess, path, pick};

/// What an entity last told its viewers, so that a tick only sends what changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    pub pos: [i32; 3],
    pub yaw: u8,
    pub pitch: i8,
}

/// A move too long to send as one: the entity is sent again where it now is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooFar;

impl Sent {
    pub fn of(pos: DVec3, yaw: u16, pitch: i16) -> Self {
        let q = |v: f64| (v * POS_SCALE).round() as i32;
        Self { pos: [q(pos.x), q(pos.y), q(pos.z)], yaw: (yaw >> 8) as u8, pitch: (pitch >> 8) as i8 }
    }
    /// The move from `self` to `now`, if there is one. `Err` if it is too far to say in a move.
    pub fn move_to(&self, id: u32, now: Sent) -> Result<Option<EntityMove>, TooFar> {
        if *self == now {
            return Ok(None);
        }
        let d = [now.pos[0] - self.pos[0], now.pos[1] - self.pos[1], now.pos[2] - self.pos[2]];
        if d.iter().any(|v| v.unsigned_abs() > i16::MAX as u32) {
            return Err(TooFar);
        }
        Ok(Some(EntityMove { id, d: [d[0] as i16, d[1] as i16, d[2] as i16], yaw: now.yaw, pitch: now.pitch }))
    }
}

#[derive(Clone, Debug)]
pub enum Data {
    Item {
        stack: ItemStack,
        delay: u16,
    },
    Falling(BlockState),
    Tnt {
        fuse: i16,
    },
    Arrow {
        owner: u32,
        stuck: bool,
        damage: i16,
    },
    /// A fishing float: whose it is, how long until something bites, and for how much longer it
    /// is biting now.
    Float {
        owner: u32,
        wait: u16,
        bite: u8,
    },
    Mob(Box<Brain>),
    Vehicle(Box<Ride>),
}

/// A boat or a cart: who rides it and what they want, and for a cart on rails, where along them.
#[derive(Clone, Debug, Default)]
pub struct Ride {
    pub rider: Option<u32>,
    /// The rider's wishes this tick: ahead (1) or back (-1), and turning left (1) or right (-1).
    pub ahead: f64,
    pub turn: f64,
    /// Which way the rider looks: a cart at rest sets off the way it is closest to.
    pub look: DVec3,
    pub track: Option<Track>,
    /// How fast a cart runs along its rails, in blocks a tick.
    pub speed: f64,
}

/// Where a cart is on its rails: the rail's cell and shape, the end it came in by, and how far
/// along it is, 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub cell: BlockPos,
    pub meta: u8,
    pub from: usize,
    pub t: f64,
}

/// The fastest a cart goes, pushed or boosted, and the fastest it runs at all.
const CART_PUSHED: f64 = 0.35;
const CART_FASTEST: f64 = 0.8;

#[derive(Clone, Debug, Default)]
pub struct Brain {
    pub target: Option<u32>,
    path: Vec<BlockPos>,
    path_at: usize,
    repath_in: u16,
    idle: u16,
    attack_cool: u16,
    pub panic: u16,
    /// Ticks left looking for a mate, after being fed.
    pub love: u16,
    /// Ticks before feeding can do that again.
    pub breed_cool: u16,
    /// Ticks until a young one is grown; nothing for a grown one.
    pub grow: u32,
    /// The player who hurt it last, who has the credit if it dies.
    pub hit_by: Option<u32>,
}

/// How long a bite lasts: the time there is to pull the line in.
pub const BITE_TICKS: u8 = 30;

/// How long a float waits for a bite, in ticks: five to twenty seconds.
pub fn bite_wait(rng: &mut Rng) -> u16 {
    100 + rng.below(300) as u16
}

/// How long a young animal takes to grow up, and a parent to be ready again: five minutes.
pub const GROW_TICKS: u32 = 20 * 60 * 5;
pub const BREED_COOL: u16 = 20 * 60 * 5;
/// How long a fed animal looks for a mate.
pub const LOVE_TICKS: u16 = 20 * 30;

#[derive(Clone, Debug)]
pub struct Entity {
    pub id: u32,
    pub kind: EntityKind,
    pub body: Body,
    pub yaw: u16,
    pub pitch: i16,
    pub age: u32,
    pub health: i16,
    pub hurt_cool: u8,
    pub data: Data,
    pub sent: Sent,
}

impl Entity {
    pub fn info(&self) -> EntityInfo {
        EntityInfo {
            id: self.id,
            kind: self.kind,
            pos: self.sent.pos,
            yaw: self.sent.yaw,
            pitch: self.sent.pitch,
            name: None,
            stack: if let Data::Item { stack, .. } = self.data { Some(stack) } else { None },
            block: if let Data::Falling(b) = self.data { b } else { BlockState::AIR },
            baby: self.is_baby(),
            owner: if let Data::Float { owner, .. } = self.data { owner } else { 0 },
        }
    }
    pub fn is_baby(&self) -> bool {
        matches!(&self.data, Data::Mob(b) if b.grow > 0)
    }
    /// Its body: half the size while young.
    pub fn aabb(&self) -> Aabb {
        let (w, h) = self.kind.size();
        let s = if self.is_baby() { 0.5 } else { 1.0 };
        self.body.aabb(w * s, h * s)
    }
}

/// What the entity system needs to know about each player, copied once a tick.
#[derive(Clone, Copy, Debug)]
pub struct PlayerView {
    pub id: u32,
    pub pos: DVec3,
    pub vulnerable: bool,
    /// The item in hand, which an animal may follow.
    pub holding: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Damage {
    pub target: u32,
    pub amount: i16,
    pub knock: DVec3,
    pub attacker: Option<u32>,
}

#[derive(Default)]
pub struct Entities {
    pub list: BTreeMap<u32, Entity>,
    next_id: u32,
    /// Bodies already deleted this tick, for telling their viewers.
    pub boxes: Vec<Aabb>,
}

fn health_of(kind: EntityKind) -> i16 {
    match kind {
        EntityKind::Sheep => 8,
        EntityKind::Pig => 10,
        EntityKind::Chicken => 4,
        EntityKind::Zombie => 20,
        EntityKind::Skeleton => 16,
        EntityKind::Gloomcrawler => 14,
        EntityKind::Lurker => 28,
        EntityKind::Boat | EntityKind::Cart => 6,
        _ => 1,
    }
}

impl Entities {
    pub fn new_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    pub fn spawn(&mut self, kind: EntityKind, pos: DVec3, data: Data) -> u32 {
        let id = self.new_id();
        let sent = Sent::of(pos, 0, 0);
        self.list.insert(
            id,
            Entity {
                id,
                kind,
                body: Body::at(pos),
                yaw: 0,
                pitch: 0,
                age: 0,
                health: health_of(kind),
                hurt_cool: 0,
                data,
                sent,
            },
        );
        id
    }

    pub fn spawn_item(&mut self, pos: DVec3, stack: ItemStack, rng: &mut Rng) -> u32 {
        let id = self.spawn(EntityKind::Item, pos, Data::Item { stack, delay: 10 });
        if let Some(e) = self.list.get_mut(&id) {
            e.body.vel = DVec3::new((rng.f64() - 0.5) * 0.2, 0.2, (rng.f64() - 0.5) * 0.2);
        }
        id
    }

    pub fn spawn_mob(&mut self, kind: EntityKind, pos: DVec3) -> u32 {
        self.spawn(kind, pos, Data::Mob(Box::default()))
    }

    /// Puts a boat or cart down; a cart on a rail goes onto it, in the middle.
    pub fn spawn_vehicle(&mut self, w: &ServerWorld, kind: EntityKind, pos: DVec3) -> u32 {
        let mut ride = Ride::default();
        let cell = BlockPos::from_vec(pos);
        let state = w.block(cell);
        if kind == EntityKind::Cart && rails::is_rail(state.kind()) {
            let meta = state.meta() % rails::SHAPES;
            ride.track = Some(Track { cell, meta, from: 0, t: 0.5 });
        }
        let at = match ride.track {
            Some(t) => cell_origin(t.cell) + rails::point(rails::ends(t.meta)[0], rails::ends(t.meta)[1], 0.5),
            None => pos,
        };
        self.spawn(kind, at, Data::Vehicle(Box::new(ride)))
    }

    pub fn count_mobs(&self, hostile: bool) -> usize {
        self.list.values().filter(|e| e.kind.is_mob() && is_hostile(e.kind) == hostile).count()
    }

    /// Takes in what the world asked for.
    pub fn take_world_events(&mut self, w: &mut ServerWorld) {
        for event in std::mem::take(&mut w.events) {
            match event {
                WorldEvent::DropItem { pos, stack } => {
                    self.spawn_item(pos, stack, &mut w.rng);
                }
                WorldEvent::Falling { pos, state } => {
                    let at = DVec3::new(pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5);
                    self.spawn(EntityKind::FallingBlock, at, Data::Falling(state));
                }
                WorldEvent::Prime { pos } => {
                    let at = DVec3::new(pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5);
                    self.spawn(EntityKind::Tnt, at, Data::Tnt { fuse: 60 });
                }
                WorldEvent::Vehicle { kind, pos } => {
                    self.spawn_vehicle(w, kind, pos);
                }
            }
        }
    }
}

pub const fn is_hostile(kind: EntityKind) -> bool {
    matches!(kind, EntityKind::Zombie | EntityKind::Skeleton | EntityKind::Gloomcrawler | EntityKind::Lurker)
}

/// What an animal eats, by item name.
pub const fn food_of(kind: EntityKind) -> Option<&'static str> {
    match kind {
        EntityKind::Sheep => Some(it::GRAIN),
        EntityKind::Pig => Some(it::APPLE),
        EntityKind::Chicken => Some(it::SEEDS),
        _ => None,
    }
}

/// Feeds an animal from a hand: a grown one goes looking for a mate, a young one grows faster.
/// Returns whether it took the food.
pub fn feed(e: &mut Entity, reg: &Registry, item: u16) -> bool {
    let Some(food) = food_of(e.kind) else { return false };
    if reg.item_by_name(food) != Some(item) {
        return false;
    }
    let Data::Mob(brain) = &mut e.data else { return false };
    if brain.grow > 0 {
        brain.grow = brain.grow.saturating_sub(GROW_TICKS / 10).max(1);
        return true;
    }
    if brain.love > 0 || brain.breed_cool > 0 {
        return false;
    }
    brain.love = LOVE_TICKS;
    true
}

/// Runs every entity for one tick. Returns the damage they dealt, for the caller to apply.
pub fn tick_all(ents: &mut Entities, w: &mut ServerWorld, players: &[PlayerView], damage: &mut Vec<Damage>) {
    let mut boxes = std::mem::take(&mut ents.boxes);
    let mut remove: Vec<u32> = Vec::new();
    let mut arrows: Vec<(u32, DVec3, DVec3)> = Vec::new();
    let mut blasts: Vec<(DVec3, f64)> = Vec::new();
    // Every creature's body, for arrows to strike and mates to find, as the tick begins.
    let bodies: Vec<(u32, EntityKind, Aabb, bool)> = ents
        .list
        .values()
        .filter(|e| e.kind.is_mob())
        .map(|e| (e.id, e.kind, e.aabb(), matches!(&e.data, Data::Mob(b) if b.love > 0 && b.grow == 0)))
        .collect();
    let mut struck: Vec<(u32, i16, DVec3, u32)> = Vec::new();
    // Path searches are the expensive part of AI: a tick only pays for a few.
    let mut searches = 8u32;
    for e in ents.list.values_mut() {
        e.age += 1;
        e.hurt_cool = e.hurt_cool.saturating_sub(1);
        let foot = BlockPos::from_vec(e.body.pos);
        if foot.y < zenith_core::pos::MIN_Y - 64 {
            remove.push(e.id);
            continue;
        }
        if !w.is_loaded(foot) {
            continue;
        }
        match &mut e.data {
            Data::Item { delay, .. } => {
                *delay = delay.saturating_sub(1);
                if e.age > 20 * 60 * 5 {
                    remove.push(e.id);
                    continue;
                }
                let (bw, bh) = e.kind.size();
                physics::sense(w, &w.reg.clone(), &mut e.body, bh);
                e.body.vel.y = if e.body.in_water { e.body.vel.y * 0.8 + 0.02 } else { (e.body.vel.y - 0.04) * 0.98 };
                let grip = if e.body.on_ground { 0.6 } else { 0.98 };
                e.body.vel.x *= grip;
                e.body.vel.z *= grip;
                let delta = e.body.vel;
                let reg = w.reg.clone();
                physics::move_body(w, &reg, &mut e.body, delta, bw, bh, false, &mut boxes);
            }
            Data::Falling(state) => {
                let state = *state;
                let (bw, bh) = e.kind.size();
                e.body.vel.y = (e.body.vel.y - 0.04) * 0.98;
                let delta = e.body.vel;
                let reg = w.reg.clone();
                physics::move_body(w, &reg, &mut e.body, delta, bw, bh, false, &mut boxes);
                if e.body.on_ground || e.age > 600 {
                    remove.push(e.id);
                    let cell = BlockPos::from_vec(e.body.pos);
                    let here = w.block(cell);
                    if here.is_air() || reg.block(here).replaceable {
                        w.set(cell, state);
                    } else if let Some(item) = reg.item_of_block(state.kind()) {
                        w.events.push(WorldEvent::DropItem { pos: e.body.pos, stack: ItemStack::new(item, 1) });
                    }
                }
            }
            Data::Tnt { fuse } => {
                *fuse -= 1;
                let (bw, bh) = e.kind.size();
                e.body.vel.y = (e.body.vel.y - 0.04) * 0.98;
                e.body.vel.x *= 0.9;
                e.body.vel.z *= 0.9;
                let delta = e.body.vel;
                let reg = w.reg.clone();
                physics::move_body(w, &reg, &mut e.body, delta, bw, bh, false, &mut boxes);
                if *fuse <= 0 {
                    remove.push(e.id);
                    blasts.push((e.body.pos + DVec3::Y * 0.5, 4.0));
                }
            }
            Data::Arrow { owner, stuck, damage: hit } => {
                let (owner, hit) = (*owner, *hit);
                if *stuck {
                    if e.age > 200 {
                        remove.push(e.id);
                    }
                    continue;
                }
                if e.age > 400 {
                    remove.push(e.id);
                    continue;
                }
                e.body.vel *= 0.99;
                e.body.vel.y -= 0.05;
                let reg = w.reg.clone();
                let v = e.body.vel;
                let reach = v.length();
                let hit_block = pick::pick(w, &reg, e.body.pos, v, reach);
                let block_t = hit_block.map_or(f64::INFINITY, |h| h.distance);
                let dir = v.normalize_or_zero();
                // The nearest player or creature in its way, short of any block: never its archer.
                let mut nearest: Option<(f64, u32, bool)> = None;
                let mut consider = |t: f64, id: u32, player: bool| {
                    if t <= reach && t < block_t && nearest.is_none_or(|n| t < n.0) {
                        nearest = Some((t, id, player));
                    }
                };
                for p in players.iter().filter(|p| p.id != owner && p.vulnerable) {
                    let b = Aabb::standing(p.pos, physics::PLAYER_WIDTH, physics::PLAYER_HEIGHT).inflate(0.1);
                    if let Some((t, _)) = b.ray(e.body.pos, dir) {
                        consider(t, p.id, true);
                    }
                }
                for &(id, _, b, _) in bodies.iter().filter(|b| b.0 != owner) {
                    if let Some((t, _)) = b.inflate(0.1).ray(e.body.pos, dir) {
                        consider(t, id, false);
                    }
                }
                if let Some((_, target, true)) = nearest {
                    damage.push(Damage { target, amount: hit, knock: dir * 0.3, attacker: Some(owner) });
                    remove.push(e.id);
                } else if let Some((_, target, false)) = nearest {
                    struck.push((target, hit, dir * 0.3, owner));
                    remove.push(e.id);
                } else if let Some(h) = hit_block {
                    e.body.pos = h.point;
                    e.body.vel = DVec3::ZERO;
                    if let Data::Arrow { stuck, .. } = &mut e.data {
                        *stuck = true;
                    }
                    e.age = 0;
                } else {
                    e.body.pos += v;
                }
                let (dy, horizontal) = (v.y, (v.x * v.x + v.z * v.z).sqrt());
                e.yaw = yaw_towards(v.x, v.z);
                e.pitch = (-(dy / horizontal.max(1e-3)).clamp(-1.0, 1.0) * 8192.0) as i16;
            }
            Data::Float { owner, wait, bite } => {
                let reg = w.reg.clone();
                // It stays out for as long as its angler holds the rod, not too far off.
                let fishing = players.iter().find(|p| p.id == *owner).is_some_and(|p| {
                    p.pos.distance_squared(e.body.pos) < 32.0 * 32.0
                        && p.holding.and_then(|i| reg.item(i)).is_some_and(|i| matches!(i.kind, ItemKind::Rod { .. }))
                });
                if !fishing || e.age > 20 * 60 * 5 {
                    remove.push(e.id);
                    continue;
                }
                let (bw, bh) = e.kind.size();
                physics::sense(w, &reg, &mut e.body, bh);
                // Afloat, it rides at the surface, bobbing in and out of it, and dips when something
                // takes the bait.
                let under = w.block(BlockPos::from_vec(e.body.pos - DVec3::Y * 0.3));
                let afloat = e.body.in_water || reg.block(under).fluid == zenith_content::Fluid::Water;
                if e.body.in_water {
                    e.body.vel = DVec3::new(e.body.vel.x * 0.5, e.body.vel.y * 0.6 + 0.02, e.body.vel.z * 0.5);
                } else {
                    let grip = if e.body.on_ground { 0.5 } else { 0.99 };
                    e.body.vel = DVec3::new(e.body.vel.x * grip, (e.body.vel.y - 0.04) * 0.98, e.body.vel.z * grip);
                }
                if afloat {
                    if *bite > 0 {
                        *bite -= 1;
                        if *bite == 0 {
                            *wait = bite_wait(&mut w.rng);
                        }
                    } else if *wait == 0 {
                        *bite = BITE_TICKS;
                        e.body.vel.y = -0.14;
                        w.effect(EffectKind::Splash, e.body.pos);
                    } else {
                        *wait -= 1;
                    }
                }
                let delta = e.body.vel;
                physics::move_body(w, &reg, &mut e.body, delta, bw, bh, false, &mut boxes);
            }
            Data::Vehicle(_) => {
                if e.kind == EntityKind::Boat {
                    tick_boat(e, w, &mut boxes);
                } else {
                    tick_cart(e, w, &mut boxes);
                }
            }
            Data::Mob(_) => {
                let grown = tick_mob(e, w, players, &bodies, &mut searches, damage, &mut arrows, &mut boxes);
                // Grown up: everyone watching is told about it afresh, at its new size.
                if grown {
                    e.sent.pos = [i32::MIN / 4; 3];
                }
                if e.health <= 0 {
                    remove.push(e.id);
                    die(e, w);
                }
                // Hostile mobs do not linger where nobody is.
                if is_hostile(e.kind) {
                    let nearest = players.iter().map(|p| p.pos.distance_squared(e.body.pos)).fold(f64::MAX, f64::min);
                    if nearest > 96.0 * 96.0 || (nearest > 40.0 * 40.0 && w.rng.below(400) == 0) {
                        remove.push(e.id);
                    }
                }
            }
        }
    }
    ents.boxes = boxes;
    for id in remove {
        ents.list.remove(&id);
    }
    // Arrows that found a creature.
    for (target, amount, knock, owner) in struck {
        if let Some(m) = ents.list.get_mut(&target)
            && hurt(m, amount, knock)
        {
            w.effect(EffectKind::Hurt, m.body.pos);
            // The credit is a player's: a skeleton's arrow takes none of it.
            if let Data::Mob(brain) = &mut m.data
                && players.iter().any(|p| p.id == owner)
            {
                brain.hit_by = Some(owner);
            }
        }
    }
    breed(ents, w);
    for (owner, pos, vel) in arrows {
        let id = ents.spawn(EntityKind::Arrow, pos, Data::Arrow { owner, stuck: false, damage: 3 });
        if let Some(e) = ents.list.get_mut(&id) {
            e.body.vel = vel;
        }
        w.effect(EffectKind::Shoot, pos);
    }
    for (at, power) in blasts {
        explode(ents, w, players, at, power, damage);
    }
}

fn cell_origin(p: BlockPos) -> DVec3 {
    DVec3::new(p.x as f64, p.y as f64, p.z as f64)
}

/// The height of the top of the water a body at `pos` is in, if it is in any.
fn water_surface(w: &ServerWorld, pos: DVec3) -> Option<f64> {
    let water = |p: BlockPos| w.reg.block(w.block(p)).fluid == zenith_content::Fluid::Water;
    let base = BlockPos::from_vec(pos + DVec3::Y * 0.3);
    for start in [base, base.offset(Dir::Down)] {
        if water(start) {
            let mut top = start;
            while water(top.offset(Dir::Up)) && top.y < start.y + 3 {
                top = top.offset(Dir::Up);
            }
            return Some(top.y as f64 + 0.9);
        }
    }
    None
}

/// A boat floats at the surface, drifts, and goes where its rower rows: along its bow, turning
/// as they turn. On land it barely moves.
fn tick_boat(e: &mut Entity, w: &mut ServerWorld, boxes: &mut Vec<Aabb>) {
    let reg = w.reg.clone();
    let (bw, bh) = e.kind.size();
    let Data::Vehicle(ride) = &e.data else { return };
    let (ahead, turn) = (ride.ahead, ride.turn);
    let surface = water_surface(w, e.body.pos);
    let body = &mut e.body;
    match surface {
        Some(top) => {
            // Lifted by how deep it sits below its line, and damped so that it settles with its floor
            // just clear of the water.
            let sink = top - body.pos.y;
            body.vel.y = body.vel.y * 0.6 + sink * 0.25;
            body.vel.x *= 0.93;
            body.vel.z *= 0.93;
        }
        None => {
            body.vel.y = (body.vel.y - 0.04) * 0.98;
            let grip = if body.on_ground { 0.5 } else { 0.95 };
            body.vel.x *= grip;
            body.vel.z *= grip;
        }
    }
    // Turning to the left is turning against the way a mouse to the right does.
    let delta = (turn * 640.0).round() as i32;
    e.yaw = (i32::from(e.yaw) - delta).rem_euclid(65536) as u16;
    let (s, c) = dmath::sin_cos(e.yaw);
    let bow = DVec3::new(-s, 0.0, c);
    let row = if surface.is_some() { 0.032 } else { 0.006 };
    body.vel += bow * ahead * row;
    let delta = body.vel;
    physics::move_body(&*w, &reg, body, delta, bw, bh, false, boxes);
}

/// A cart on rails runs along them: faster downhill, slower up, sped by booster rails, pushed by
/// its rider, and slowly losing speed. Off the end of the line, or off rails altogether, it rolls
/// like anything else until it lands on a rail again.
fn tick_cart(e: &mut Entity, w: &mut ServerWorld, boxes: &mut Vec<Aabb>) {
    let reg = w.reg.clone();
    let (bw, bh) = e.kind.size();
    let Data::Vehicle(ride) = &mut e.data else { return };
    let Some(mut track) = ride.track.filter(|t| rails::is_rail(w.block(t.cell).kind())) else {
        ride.track = None;
        let body = &mut e.body;
        body.vel.y = (body.vel.y - 0.04) * 0.98;
        let grip = if body.on_ground { 0.6 } else { 0.98 };
        body.vel.x *= grip;
        body.vel.z *= grip;
        let delta = body.vel;
        physics::move_body(&*w, &reg, body, delta, bw, bh, false, boxes);
        // Landed on a rail: onto it, heading the way it was rolling.
        let cell = BlockPos::from_vec(body.pos + DVec3::Y * 0.1);
        let state = w.block(cell);
        if rails::is_rail(state.kind()) {
            let meta = state.meta() % rails::SHAPES;
            let [a, b] = rails::ends(meta);
            let flat = DVec3::new(body.vel.x, 0.0, body.vel.z);
            let toward_b = rails::heading(a, b, 0.5).dot(flat) >= 0.0;
            ride.track = Some(Track { cell, meta, from: usize::from(!toward_b), t: 0.5 });
            ride.speed = flat.length();
        }
        return;
    };
    let here = w.block(track.cell);
    track.meta = here.meta() % rails::SHAPES;
    let ends = |t: &Track| {
        let e = rails::ends(t.meta);
        (e[t.from], e[1 - t.from])
    };
    let (mut from, mut to) = ends(&track);
    let heading = rails::heading(from, to, track.t);
    // A push from the rider: a cart at rest sets off along the way they look, and Back brakes.
    if ride.ahead > 0.0 {
        let look = DVec3::new(ride.look.x, 0.0, ride.look.z).normalize_or_zero();
        if ride.speed < 0.02 && heading.dot(look) < 0.0 {
            track.from = 1 - track.from;
            track.t = 1.0 - track.t;
            (from, to) = ends(&track);
        }
        if ride.speed < CART_PUSHED {
            ride.speed = (ride.speed + 0.01).min(CART_PUSHED);
        }
    } else if ride.ahead < 0.0 {
        ride.speed *= 0.85;
    }
    if from.up != to.up {
        ride.speed += if to.up { -0.025 } else { 0.025 };
    }
    if here.kind() == id::BOOST_RAIL {
        ride.speed = (ride.speed.max(0.08) + 0.05).min(CART_FASTEST);
    }
    ride.speed *= if ride.rider.is_some() { 0.996 } else { 0.99 };
    ride.speed = ride.speed.min(CART_FASTEST);
    // Rolled to a stop going uphill: it comes back down.
    if ride.speed < 0.0 {
        track.from = 1 - track.from;
        track.t = 1.0 - track.t;
        (from, to) = ends(&track);
        ride.speed = -ride.speed;
    }
    // Along the rails, from one onto the next, as far as it goes this tick. Where the line ends at
    // something solid, the cart stops short of it with its nose at the edge of the rail; where it
    // ends in the open, it rolls off.
    let mut left = ride.speed;
    let mut off_the_end = false;
    let mut buffered = false;
    for _ in 0..4 {
        let length = rails::length(from, to);
        let onward = rails::next(&*w, track.cell, to);
        let stop = if onward.is_none() && blocked_beyond(&*w, &reg, track.cell, to) {
            1.0 - (bw * 0.5 + 0.01) / length
        } else {
            1.0
        };
        let room = (stop - track.t).max(0.0) * length;
        if left <= room {
            track.t += left / length;
            break;
        }
        left -= room;
        match onward {
            Some((cell, meta, enter)) => {
                track = Track { cell, meta, from: enter, t: 0.0 };
                (from, to) = ends(&track);
            }
            None if stop < 1.0 => {
                track.t = track.t.max(stop);
                buffered = true;
                break;
            }
            None => {
                track.t = 1.0;
                off_the_end = true;
                break;
            }
        }
    }
    if buffered {
        ride.speed = 0.0;
    }
    let heading = rails::heading(from, to, track.t);
    e.body.pos = cell_origin(track.cell) + rails::point(from, to, track.t);
    let climb = if from.up == to.up {
        0.0
    } else if to.up {
        1.0
    } else {
        -1.0
    };
    e.body.vel = (heading + DVec3::Y * climb).normalize_or_zero() * ride.speed;
    e.yaw = yaw_towards(heading.x, heading.z);
    e.pitch = (-climb * 8192.0) as i16;
    ride.track = if off_the_end && ride.speed > 0.01 { None } else { Some(track) };
    if off_the_end {
        ride.speed = 0.0;
    }
}

/// Whether nothing solid stands between two points: what a blow needs.
fn clear_between(w: &impl BlockAccess, reg: &Registry, from: DVec3, to: DVec3) -> bool {
    let d = to - from;
    let steps = (d.length() / 0.2).ceil().max(1.0) as i32;
    (1..steps).all(|i| {
        let p = from + d * (f64::from(i) / f64::from(steps));
        !reg.lut.collides[w.block(BlockPos::from_vec(p)).kind() as usize]
    })
}

/// Whether a step into `cell` would end in lava, or in a fall of more than three blocks with no
/// water to land in.
fn dangerous_step(w: &impl BlockAccess, reg: &Registry, cell: BlockPos) -> bool {
    let fluid = |p: BlockPos| reg.block(w.block(p)).fluid;
    if fluid(cell) == zenith_content::Fluid::Lava || fluid(cell.offset(Dir::Down)) == zenith_content::Fluid::Lava {
        return true;
    }
    !(0..=4).any(|down| {
        let p = cell.add(0, -down, 0);
        reg.lut.collides[w.block(p).kind() as usize] || fluid(p) == zenith_content::Fluid::Water
    })
}

/// Whether something solid stands where a cart leaving `cell` by `out` would go.
fn blocked_beyond(w: &impl BlockAccess, reg: &Registry, cell: BlockPos, out: rails::End) -> bool {
    let beyond = cell.offset(out.side);
    let y = cell.y + i32::from(out.up);
    reg.lut.collides[w.block(BlockPos::new(beyond.x, y, beyond.z)).kind() as usize]
}

/// Two grown animals of a kind that took to each other, close together, have a young one, and
/// are not ready again for a while.
fn breed(ents: &mut Entities, w: &mut ServerWorld) {
    let lovers: Vec<(u32, EntityKind, DVec3)> = ents
        .list
        .values()
        .filter(|e| matches!(&e.data, Data::Mob(b) if b.love > 0 && b.grow == 0))
        .map(|e| (e.id, e.kind, e.body.pos))
        .collect();
    let mut paired: Vec<u32> = Vec::new();
    for (i, &(a, kind, at_a)) in lovers.iter().enumerate() {
        if paired.contains(&a) {
            continue;
        }
        let Some(&(b, _, at_b)) =
            lovers[i + 1..].iter().find(|&&(b, k, at)| k == kind && !paired.contains(&b) && at.distance(at_a) < 1.6)
        else {
            continue;
        };
        paired.extend([a, b]);
        for id in [a, b] {
            if let Some(Data::Mob(brain)) = ents.list.get_mut(&id).map(|e| &mut e.data) {
                brain.love = 0;
                brain.breed_cool = BREED_COOL;
            }
        }
        let at = (at_a + at_b) * 0.5;
        let young = ents.spawn_mob(kind, at);
        if let Some(Data::Mob(brain)) = ents.list.get_mut(&young).map(|e| &mut e.data) {
            brain.grow = GROW_TICKS;
        }
        w.effect(EffectKind::Love, at + DVec3::Y * 0.6);
    }
}

fn die(e: &Entity, w: &mut ServerWorld) {
    w.effect(EffectKind::Death, e.body.pos);
    // The young leave nothing behind.
    if e.is_baby() {
        return;
    }
    // Whoever brought it down learns something by it.
    if let Data::Mob(brain) = &e.data
        && let Some(player) = brain.hit_by
    {
        let xp = if is_hostile(e.kind) { 5 + w.rng.below(3) } else { 1 + w.rng.below(3) };
        w.xp_awards.push((player, xp));
    }
    let loot: &[(&str, i32, i32)] = match e.kind {
        EntityKind::Sheep => &[("wool_white", 1, 2), ("raw_meat", 1, 2)],
        EntityKind::Pig => &[("raw_meat", 1, 3)],
        EntityKind::Chicken => &[("raw_meat", 1, 1), (it::FEATHER, 0, 2)],
        EntityKind::Zombie => &[("raw_meat", 0, 2), ("copper_ingot", 0, 1)],
        EntityKind::Skeleton => &[("stick", 0, 3), ("coal", 0, 2), (it::ARROW, 0, 2)],
        EntityKind::Gloomcrawler => &[(it::SINEW, 0, 2)],
        EntityKind::Lurker => &[(it::EMBER_DUST, 0, 2), (it::IRON_INGOT, 0, 1)],
        _ => &[],
    };
    for &(name, lo, hi) in loot {
        let count = w.rng.range(lo, hi);
        if count > 0
            && let Some(item) = w.reg.item_by_name(name)
        {
            w.events.push(WorldEvent::DropItem {
                pos: e.body.pos + DVec3::Y * 0.5,
                stack: ItemStack::new(item, count as u8),
            });
        }
    }
}

/// Turn to face along `(dx, dz)`, in the same convention physics uses: 0 faces +z.
pub fn yaw_towards(dx: f64, dz: f64) -> u16 {
    // An approximation is plenty for looking the right way, and a search keeps this deterministic.
    let mut best = 0u16;
    let mut best_dot = f64::MIN;
    for i in 0..256u32 {
        let turn = (i * 256) as u16;
        let (s, c) = dmath::sin_cos(turn);
        let dot = -s * dx + c * dz;
        if dot > best_dot {
            best_dot = dot;
            best = turn;
        }
    }
    best
}

/// One tick of a creature: what it wants, and the walk there. Returns whether it grew up just now.
fn tick_mob(
    e: &mut Entity,
    w: &mut ServerWorld,
    players: &[PlayerView],
    bodies: &[(u32, EntityKind, Aabb, bool)],
    searches: &mut u32,
    damage: &mut Vec<Damage>,
    arrows: &mut Vec<(u32, DVec3, DVec3)>,
    boxes: &mut Vec<Aabb>,
) -> bool {
    let reg = w.reg.clone();
    let baby = e.is_baby();
    let (bw, bh) = if baby { (e.kind.size().0 * 0.5, e.kind.size().1 * 0.5) } else { e.kind.size() };
    let Data::Mob(brain) = &mut e.data else { return false };
    brain.attack_cool = brain.attack_cool.saturating_sub(1);
    brain.panic = brain.panic.saturating_sub(1);
    brain.repath_in = brain.repath_in.saturating_sub(1);
    brain.love = brain.love.saturating_sub(1);
    brain.breed_cool = brain.breed_cool.saturating_sub(1);
    let grown = brain.grow == 1;
    brain.grow = brain.grow.saturating_sub(1);
    let at = BlockPos::from_vec(e.body.pos + DVec3::Y * 0.2);
    let hostile = is_hostile(e.kind);
    if brain.love > 0 && brain.love.is_multiple_of(15) {
        w.effect(EffectKind::Love, e.body.pos + DVec3::Y * (bh + 0.2));
    }
    // A lurker shies from light: lit up, it gives up the chase and backs off into the dark.
    if e.kind == EntityKind::Lurker && e.age.is_multiple_of(10) && !dark_enough(w, at, false) {
        brain.panic = 60;
        brain.repath_in = 0;
    }

    // Hazards.
    physics::sense(&*w, &reg, &mut e.body, bh);
    if e.body.in_lava && e.age.is_multiple_of(10) {
        e.health -= 2;
        e.hurt_cool = 10;
    }

    // What it wants.
    let mut wish_cell: Option<BlockPos> = None;
    // Where to go straight for when there is no path there: a hostile one's prey.
    let mut straight_at: Option<DVec3> = None;
    if hostile && brain.panic == 0 {
        let best = players
            .iter()
            .filter(|p| p.vulnerable)
            .map(|p| (p.pos.distance_squared(e.body.pos), p))
            .min_by(|a, b| a.0.total_cmp(&b.0));
        brain.target = best.filter(|(d, _)| *d < 24.0 * 24.0).map(|(_, p)| p.id);
        if let Some(target) = brain.target.and_then(|t| players.iter().find(|p| p.id == t)) {
            let d = target.pos.distance(e.body.pos);
            let to = (target.pos - e.body.pos).normalize_or_zero();
            e.yaw = yaw_towards(to.x, to.z);
            // Reach, blow and the pause after it, for those that fight hand to hand.
            let melee = match e.kind {
                EntityKind::Zombie => Some((1.9, 3, 20)),
                EntityKind::Gloomcrawler => Some((1.7, 2, 14)),
                EntityKind::Lurker => Some((2.1, 5, 26)),
                _ => None,
            };
            if let Some((reach, amount, cool)) = melee
                && d < reach
                && brain.attack_cool == 0
                && clear_between(&*w, &reg, e.body.pos + DVec3::Y * (e.kind.size().1 * 0.85), target.pos + DVec3::Y)
            {
                brain.attack_cool = cool;
                damage.push(Damage { target: target.id, amount, knock: to * 0.4, attacker: Some(e.id) });
            }
            if e.kind == EntityKind::Skeleton && d < 14.0 && brain.attack_cool == 0 {
                let eye = e.body.pos + DVec3::Y * 1.5;
                let aim = target.pos + DVec3::Y * 1.3 - eye;
                let clear = pick::pick(&*w, &reg, eye, aim, aim.length()).is_none();
                if clear {
                    brain.attack_cool = 50;
                    let flat = (aim.x * aim.x + aim.z * aim.z).sqrt();
                    let v = DVec3::new(aim.x, aim.y + flat * 0.12, aim.z).normalize_or_zero() * 1.1;
                    arrows.push((e.id, eye + v.normalize_or_zero() * 0.8, v));
                }
            }
            let keep_away = e.kind == EntityKind::Skeleton && d < 8.0;
            if !keep_away {
                wish_cell = Some(BlockPos::from_vec(target.pos));
                straight_at = Some(target.pos);
            } else {
                brain.path.clear();
            }
        }
    }
    // An animal goes to a mate it is looking for, or else follows whoever holds its food.
    if !hostile && brain.panic == 0 {
        let me = e.body.pos;
        let closest = |list: &mut dyn Iterator<Item = DVec3>| {
            list.filter(|p| p.distance(me) < 8.0).min_by(|a, b| a.distance(me).total_cmp(&b.distance(me)))
        };
        let mate = if brain.love > 0 {
            closest(&mut bodies.iter().filter(|b| b.0 != e.id && b.1 == e.kind && b.3).map(|b| b.2.center()))
        } else {
            None
        };
        let food = food_of(e.kind).and_then(|f| reg.item_by_name(f));
        let tempting =
            food.and_then(|food| closest(&mut players.iter().filter(|p| p.holding == Some(food)).map(|p| p.pos)));
        if let Some(goal) = mate.or(tempting) {
            if goal.distance(me) > 1.2 {
                wish_cell = Some(BlockPos::from_vec(goal));
                straight_at = Some(goal);
            } else {
                brain.path.clear();
            }
        }
    }

    // Walk there, or wander.
    if wish_cell.is_some() || brain.panic > 0 || brain.path_at >= brain.path.len() {
        let goal = match wish_cell {
            Some(cell) => Some(cell),
            None => {
                if brain.idle > 0 && brain.panic == 0 {
                    brain.idle -= 1;
                    None
                } else if brain.repath_in == 0 {
                    let (dx, dz) = (w.rng.range(-8, 8), w.rng.range(-8, 8));
                    brain.idle = w.rng.range(40, 200) as u16;
                    Some(at.add(dx, 0, dz))
                } else {
                    None
                }
            }
        };
        if let Some(goal) = goal
            && brain.repath_in == 0
            && *searches > 0
        {
            *searches -= 1;
            brain.repath_in = 20 + (e.id % 8) as u16;
            brain.path = path::find(&*w, &reg, at, goal, 200).unwrap_or_default();
            brain.path_at = 0;
        }
    }

    let mut wish = DVec3::ZERO;
    let mut jump = false;
    // No path to what it wants: straight at it, over or up whatever is in the way.
    if brain.path_at >= brain.path.len()
        && let Some(goal) = straight_at
    {
        let to = DVec3::new(goal.x - e.body.pos.x, 0.0, goal.z - e.body.pos.z);
        if to.length_squared() > 0.25 {
            wish = to.normalize_or_zero();
            e.yaw = yaw_towards(wish.x, wish.z);
            // The search went round what was in the way for a reason: lava, or a long drop.
            if dangerous_step(&*w, &reg, BlockPos::from_vec(e.body.pos + wish * 0.7)) {
                wish = DVec3::ZERO;
            }
        }
    }
    if let Some(&next) = brain.path.get(brain.path_at) {
        let target = DVec3::new(next.x as f64 + 0.5, next.y as f64, next.z as f64 + 0.5);
        let mut to = target - e.body.pos;
        let climb = to.y;
        to.y = 0.0;
        if to.length_squared() < 0.15 && climb.abs() < 1.2 {
            brain.path_at += 1;
        } else {
            wish = to.normalize_or_zero();
            e.yaw = yaw_towards(wish.x, wish.z);
            jump = climb > 0.4;
        }
    }
    let speed = match e.kind {
        EntityKind::Zombie => 0.11,
        EntityKind::Skeleton => 0.12,
        EntityKind::Chicken => 0.07,
        EntityKind::Gloomcrawler => 0.14,
        EntityKind::Lurker => 0.085,
        _ => 0.09,
    } * if brain.panic > 0 { 1.6 } else { 1.0 };

    let body = &mut e.body;
    if body.in_water || body.in_lava {
        body.vel.x = body.vel.x * 0.8 + wish.x * speed * 0.4;
        body.vel.z = body.vel.z * 0.8 + wish.z * speed * 0.4;
        body.vel.y = body.vel.y * 0.8 + 0.03;
    } else {
        let grip = if body.on_ground { 0.546 } else { 0.91 };
        let accel = if body.on_ground { speed * 0.454 / 0.546 } else { 0.015 };
        body.vel.x = body.vel.x * grip + wish.x * accel;
        body.vel.z = body.vel.z * grip + wish.z * accel;
        if jump && body.on_ground {
            body.vel.y = 0.42;
            body.on_ground = false;
        }
    }
    let delta = body.vel;
    let falling = delta.y < 0.0;
    let was_blocked_before = (body.vel.x, body.vel.z);
    physics::move_body(&*w, &reg, body, delta, bw, bh, true, boxes);
    if !(body.in_water || body.in_lava) {
        body.vel.y = (body.vel.y - 0.08) * 0.98;
    }
    let light_footed = matches!(e.kind, EntityKind::Chicken | EntityKind::Gloomcrawler);
    if body.on_ground {
        if falling && body.fall > 4.0 && !light_footed {
            e.health -= (body.fall - 3.0) as i16;
        }
        body.fall = 0.0;
        body.vel.y = -0.0784;
    } else if delta.y < 0.0 {
        body.fall -= delta.y;
    }
    // Pushed against something with nowhere to go: a crawler goes up it, whatever its height; the
    // rest try hopping over.
    let blocked = wish != DVec3::ZERO
        && (body.vel.x, body.vel.z) != was_blocked_before
        && (body.vel.x == 0.0 || body.vel.z == 0.0);
    if blocked && e.kind == EntityKind::Gloomcrawler {
        body.vel.y = 0.2;
        body.on_ground = false;
        body.fall = 0.0;
    } else if blocked && body.on_ground {
        body.vel.y = 0.42;
        body.on_ground = false;
    }
    let cactus = w.block(BlockPos::from_vec(body.pos)).kind() == id::CACTUS;
    if cactus && e.age.is_multiple_of(20) {
        e.health -= 1;
    }
    grown
}

/// Hits an entity. Returns whether it took the blow.
pub fn hurt(e: &mut Entity, amount: i16, knock: DVec3) -> bool {
    if e.hurt_cool > 0 {
        return false;
    }
    e.hurt_cool = 10;
    e.health -= amount;
    e.body.vel += DVec3::new(knock.x, 0.3, knock.z);
    if let Data::Mob(brain) = &mut e.data
        && !is_hostile(e.kind)
    {
        brain.panic = 80;
        brain.repath_in = 0;
        brain.idle = 0;
    }
    true
}

/// A blast: rays from the middle chew through blocks by how tough they are, and anything nearby is
/// hurt and thrown. A charge caught in it goes off after a moment.
pub fn explode(
    ents: &mut Entities,
    w: &mut ServerWorld,
    players: &[PlayerView],
    at: DVec3,
    power: f64,
    damage: &mut Vec<Damage>,
) {
    w.effect(EffectKind::Explosion, at);
    let center = BlockPos::from_vec(at);
    let mut destroyed: Vec<BlockPos> = Vec::new();
    let reg = w.reg.clone();
    const RAYS: i32 = 8;
    for ix in 0..RAYS {
        for iy in 0..RAYS {
            for iz in 0..RAYS {
                let on_shell = ix == 0 || ix == RAYS - 1 || iy == 0 || iy == RAYS - 1 || iz == 0 || iz == RAYS - 1;
                if !on_shell {
                    continue;
                }
                let dir =
                    DVec3::new(ix as f64 / 3.5 - 1.0, iy as f64 / 3.5 - 1.0, iz as f64 / 3.5 - 1.0).normalize_or_zero();
                let mut strength = power * (0.7 + w.rng.f64() * 0.6);
                let mut p = at;
                while strength > 0.0 {
                    let cell = BlockPos::from_vec(p);
                    let state = w.block(cell);
                    if !state.is_air() {
                        let def = reg.block(state);
                        if def.hardness < 0.0 || def.fluid != zenith_content::Fluid::None {
                            strength -= 100.0;
                        } else {
                            strength -= (def.hardness as f64 + 0.3) * 0.3;
                            if strength > 0.0 && !destroyed.contains(&cell) {
                                destroyed.push(cell);
                            }
                        }
                    }
                    p += dir * 0.5;
                    strength -= 0.225;
                }
            }
        }
    }
    destroyed.sort_unstable();
    for p in destroyed {
        let state = w.block(p);
        if state.kind() == id::TNT {
            w.set(p, BlockState::AIR);
            let e = ents.spawn(
                EntityKind::Tnt,
                p.center() - DVec3::Y * 0.5,
                Data::Tnt { fuse: 10 + w.rng.below(20) as i16 },
            );
            let _ = e;
            continue;
        }
        if w.rng.chance(0.3) {
            rules::drop_block(w, p, state);
        }
        w.set(p, BlockState::AIR);
    }
    let _ = center;
    let reach = power * 2.0;
    for p in players.iter().filter(|p| p.vulnerable) {
        let d = p.pos.distance(at);
        if d < reach {
            let falloff = 1.0 - d / reach;
            let to = (p.pos - at).normalize_or_zero();
            damage.push(Damage {
                target: p.id,
                amount: (falloff * 24.0) as i16,
                knock: to * falloff * 1.5,
                attacker: None,
            });
        }
    }
    for e in ents.list.values_mut() {
        let d = e.body.pos.distance(at);
        if d < reach {
            let falloff = 1.0 - d / reach;
            let to = (e.body.pos - at).normalize_or_zero();
            if e.kind.is_mob() {
                hurt(e, (falloff * 24.0) as i16, to * falloff);
            } else if matches!(e.kind, EntityKind::Item | EntityKind::Tnt) {
                e.body.vel += to * falloff * 0.6;
            }
        }
    }
}

/// How dark a spot is, roughly: whether the sky reaches it, and whether a light is near.
pub fn dark_enough(w: &ServerWorld, p: BlockPos, night: bool) -> bool {
    let Some(chunk) = w.map.get(p.chunk()) else { return false };
    let top = chunk
        .top(p.x & 15, p.z & 15, |s| w.reg.lut.opacity[s.kind() as usize] >= 15)
        .unwrap_or(zenith_core::pos::MIN_Y);
    if p.y > top && !night {
        return false;
    }
    for dy in -6..=6i32 {
        for dz in -6..=6i32 {
            for dx in -6..=6i32 {
                let q = p.add(dx, dy, dz);
                let emission = w.reg.lut.emission[w.block(q).kind() as usize] as i32;
                if emission - dx.abs() - dy.abs() - dz.abs() > 6 {
                    return false;
                }
            }
        }
    }
    true
}

/// Tries to bring a mob into the world around each player.
pub fn natural_spawns(ents: &mut Entities, w: &mut ServerWorld, players: &[PlayerView], night: bool) {
    if players.is_empty() {
        return;
    }
    const HOSTILE_CAP: usize = 10;
    const PASSIVE_CAP: usize = 10;
    let reg = w.reg.clone();
    let hostile_room = ents.count_mobs(true) < HOSTILE_CAP * players.len();
    let passive_room = ents.count_mobs(false) < PASSIVE_CAP * players.len();
    for p in players {
        let around = BlockPos::from_vec(p.pos);
        let (dx, dz) = (w.rng.range(-44, 44), w.rng.range(-44, 44));
        if dx.abs() < 14 && dz.abs() < 14 {
            continue;
        }
        let (x, z) = (around.x + dx, around.z + dz);
        let Some(chunk) = w.map.get(ChunkPos::new(x >> 4, z >> 4)) else { continue };
        let lut = &reg.lut;
        let Some(top) = chunk.top(x & 15, z & 15, |s| lut.collides[s.kind() as usize]) else { continue };
        let surface = BlockPos::new(x, top + 1, z);
        let ground = chunk.get(x & 15, top, z & 15).kind();
        // A dark spot can be anywhere under the surface; hostile mobs get to try one of those too.
        let cave = BlockPos::new(x, w.rng.range((top - 40).max(-50), top), z);
        if hostile_room && w.rng.below(2) == 0 {
            let spot = if w.rng.below(2) == 0 { surface } else { cave };
            if path::can_stand(&*w, &reg, spot)
                && w.reg.lut.collides[w.block(spot.offset(Dir::Down)).kind() as usize]
                && dark_enough(w, spot, night)
            {
                // Lurkers keep to the deep caves; the rest come wherever it is dark.
                let deep = spot == cave && spot.y < 20;
                let kind = match w.rng.below(6) {
                    0 | 1 if deep => EntityKind::Lurker,
                    0 => EntityKind::Skeleton,
                    1 | 2 => EntityKind::Gloomcrawler,
                    _ => EntityKind::Zombie,
                };
                ents.spawn_mob(kind, DVec3::new(spot.x as f64 + 0.5, spot.y as f64, spot.z as f64 + 0.5));
            }
        } else if passive_room && ground == id::GRASS && !night && path::can_stand(&*w, &reg, surface) {
            let kind = match w.rng.below(3) {
                0 => EntityKind::Sheep,
                1 => EntityKind::Pig,
                _ => EntityKind::Chicken,
            };
            for _ in 0..w.rng.range(1, 3) {
                let (ox, oz) = (w.rng.f64() * 3.0 - 1.5, w.rng.f64() * 3.0 - 1.5);
                ents.spawn_mob(
                    kind,
                    DVec3::new(surface.x as f64 + 0.5 + ox, surface.y as f64, surface.z as f64 + 0.5 + oz),
                );
            }
        }
    }
}

/// The entities around each chunk, rebuilt once a tick so that viewers can find what is near them.
#[derive(Default)]
pub struct Grid {
    pub cells: FxMap<ChunkPos, Vec<u32>>,
}

impl Grid {
    pub fn rebuild(&mut self, ents: &Entities) {
        for v in self.cells.values_mut() {
            v.clear();
        }
        for e in ents.list.values() {
            self.cells.entry(BlockPos::from_vec(e.body.pos).chunk()).or_default().push(e.id);
        }
        self.cells.retain(|_, v| !v.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::tests::flat_world;

    fn drop_stack(ents: &mut Entities, w: &mut ServerWorld, name: &str) -> u32 {
        let item = w.reg.item_by_name(name).expect("item");
        ents.spawn_item(DVec3::new(0.5, 70.0, 0.5), ItemStack::new(item, 1), &mut w.rng)
    }

    fn run(ents: &mut Entities, w: &mut ServerWorld, players: &[PlayerView], ticks: u32) -> Vec<Damage> {
        let mut damage = Vec::new();
        for _ in 0..ticks {
            w.tick += 1;
            tick_all(ents, w, players, &mut damage);
            ents.take_world_events(w);
            rules::run(w);
        }
        damage
    }

    #[test]
    fn items_fall_to_the_ground_and_rest_there() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let id = drop_stack(&mut ents, &mut w, "dirt");
        run(&mut ents, &mut w, &[], 100);
        let e = &ents.list[&id];
        assert!((e.body.pos.y - 61.0).abs() < 1e-6, "{:?}", e.body.pos);
    }

    #[test]
    fn items_expire_after_five_minutes() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let id = drop_stack(&mut ents, &mut w, "dirt");
        ents.list.get_mut(&id).expect("there").age = 6000;
        run(&mut ents, &mut w, &[], 2);
        assert!(!ents.list.contains_key(&id));
    }

    #[test]
    fn a_falling_block_lands_as_a_block() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let p = BlockPos::new(2, 70, 2);
        w.set(p, BlockState::of(id::SAND));
        for _ in 0..3 {
            w.tick += 1;
            rules::run(&mut w);
        }
        ents.take_world_events(&mut w);
        assert_eq!(ents.list.len(), 1, "it became an entity");
        assert!(w.block(p).is_air());
        run(&mut ents, &mut w, &[], 80);
        assert_eq!(w.block(BlockPos::new(2, 61, 2)).kind(), id::SAND);
        assert!(ents.list.is_empty());
    }

    #[test]
    fn a_falling_block_with_nowhere_to_go_turns_into_an_item() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        // A torch cannot be replaced by falling sand: it lands on top of the floor beside it.
        let at = DVec3::new(0.5, 61.0, 0.5);
        w.set(BlockPos::new(0, 61, 0), BlockState::of(id::STONE_SLAB));
        ents.spawn(EntityKind::FallingBlock, at + DVec3::Y * 0.6, Data::Falling(BlockState::of(id::SAND)));
        run(&mut ents, &mut w, &[], 3);
        assert_eq!(w.block(BlockPos::new(0, 61, 0)).kind(), id::STONE_SLAB);
        assert!(ents.list.values().any(|e| e.kind == EntityKind::Item));
    }

    #[test]
    fn a_blast_digs_a_crater_but_not_through_bedrock() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        w.set(BlockPos::new(0, 59, 0), BlockState::of(id::BEDROCK));
        let id = ents.spawn(EntityKind::Tnt, DVec3::new(0.5, 61.0, 0.5), Data::Tnt { fuse: 1 });
        run(&mut ents, &mut w, &[], 2);
        assert!(!ents.list.contains_key(&id));
        assert!(w.block(BlockPos::new(0, 60, 0)).is_air(), "the floor under it is gone");
        assert_eq!(w.block(BlockPos::new(0, 59, 0)).kind(), id::BEDROCK);
        assert_eq!(w.block(BlockPos::new(15, 60, 15)).kind(), id::STONE, "far away is untouched");
    }

    #[test]
    fn a_blast_hurts_who_is_near_and_not_who_is_far() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let players = [
            PlayerView { id: 100, pos: DVec3::new(2.5, 61.0, 0.5), vulnerable: true, holding: None },
            PlayerView { id: 101, pos: DVec3::new(40.5, 61.0, 0.5), vulnerable: true, holding: None },
        ];
        ents.spawn(EntityKind::Tnt, DVec3::new(0.5, 61.0, 0.5), Data::Tnt { fuse: 1 });
        let damage = run(&mut ents, &mut w, &players, 2);
        assert!(damage.iter().any(|d| d.target == 100 && d.amount > 0));
        assert!(damage.iter().all(|d| d.target != 101));
    }

    #[test]
    fn charges_set_each_other_off() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let other = BlockPos::new(2, 61, 0);
        w.set(other, BlockState::of(id::TNT));
        ents.spawn(EntityKind::Tnt, DVec3::new(0.5, 61.0, 0.5), Data::Tnt { fuse: 1 });
        run(&mut ents, &mut w, &[], 2);
        assert!(w.block(other).is_air());
        assert!(ents.list.values().any(|e| e.kind == EntityKind::Tnt), "the second charge is lit");
    }

    fn player_at(x: f64) -> [PlayerView; 1] {
        [PlayerView { id: 1000, pos: DVec3::new(x, 61.0, 0.5), vulnerable: true, holding: None }]
    }

    #[test]
    fn a_zombie_walks_to_a_player_and_hits_them() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let id = ents.spawn_mob(EntityKind::Zombie, DVec3::new(12.5, 61.0, 0.5));
        let players = player_at(0.5);
        let damage = run(&mut ents, &mut w, &players, 200);
        assert!(damage.iter().any(|d| d.target == 1000), "never reached the player");
        assert!(ents.list[&id].body.pos.x < 3.0, "{:?}", ents.list[&id].body.pos);
    }

    fn brain(ents: &Entities, id: u32) -> &Brain {
        match &ents.list[&id].data {
            Data::Mob(b) => b,
            _ => panic!("not a creature"),
        }
    }

    #[test]
    fn an_arrow_from_a_player_hits_a_creature_and_the_archer_has_the_credit() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let pig = ents.spawn_mob(EntityKind::Pig, DVec3::new(6.5, 61.0, 0.5));
        let arrow = ents.spawn(
            EntityKind::Arrow,
            DVec3::new(0.5, 61.4, 0.5),
            Data::Arrow { owner: 1000, stuck: false, damage: 4 },
        );
        ents.list.get_mut(&arrow).expect("arrow").body.vel = DVec3::new(2.0, 0.0, 0.0);
        let archer = [PlayerView { id: 1000, pos: DVec3::new(0.5, 61.0, -20.0), vulnerable: false, holding: None }];
        run(&mut ents, &mut w, &archer, 6);
        assert!(!ents.list.contains_key(&arrow), "the arrow is spent");
        assert_eq!(ents.list[&pig].health, 6);
        assert_eq!(brain(&ents, pig).hit_by, Some(1000));
        // Killed, it leaves its archer something to learn.
        ents.list.get_mut(&pig).expect("pig").health = 0;
        run(&mut ents, &mut w, &archer, 1);
        assert!(w.xp_awards.iter().any(|&(who, xp)| who == 1000 && xp > 0), "{:?}", w.xp_awards);
    }

    #[test]
    fn feeding_wants_the_right_food_and_hurries_the_young() {
        let (mut ents, w) = (Entities::default(), flat_world());
        let sheep = ents.spawn_mob(EntityKind::Sheep, DVec3::new(0.5, 61.0, 0.5));
        let (apple, grain) =
            (w.reg.item_by_name("apple").expect("apples"), w.reg.item_by_name("grain").expect("grain"));
        let e = ents.list.get_mut(&sheep).expect("sheep");
        assert!(!feed(e, &w.reg, apple), "sheep eat grain");
        assert!(feed(e, &w.reg, grain));
        assert!(!feed(e, &w.reg, grain), "one helping at a time");
        if let Data::Mob(b) = &mut e.data {
            b.love = 0;
            b.grow = GROW_TICKS;
        }
        assert!(feed(e, &w.reg, grain));
        assert!(matches!(&e.data, Data::Mob(b) if b.grow < GROW_TICKS && b.love == 0));
        assert!(e.info().baby);
        assert!(e.aabb().max.y - e.aabb().min.y < 0.5, "young and small");
    }

    #[test]
    fn two_fed_animals_find_each_other_and_have_a_young_one() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let grain = w.reg.item_by_name("grain").expect("grain");
        let parents = [
            ents.spawn_mob(EntityKind::Sheep, DVec3::new(0.5, 61.0, 0.5)),
            ents.spawn_mob(EntityKind::Sheep, DVec3::new(4.5, 61.0, 0.5)),
        ];
        for id in parents {
            assert!(feed(ents.list.get_mut(&id).expect("sheep"), &w.reg, grain));
        }
        run(&mut ents, &mut w, &[], 300);
        let young: Vec<&Entity> = ents.list.values().filter(|e| e.is_baby()).collect();
        assert_eq!(young.len(), 1, "one young sheep");
        assert_eq!(young[0].kind, EntityKind::Sheep);
        for id in parents {
            let b = brain(&ents, id);
            assert!(b.love == 0 && b.breed_cool > 0);
        }
    }

    #[test]
    fn animals_follow_whoever_holds_their_food() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let pig = ents.spawn_mob(EntityKind::Pig, DVec3::new(6.5, 61.0, 0.5));
        let apple = w.reg.item_by_name("apple").expect("apples");
        let players =
            [PlayerView { id: 1000, pos: DVec3::new(0.5, 61.0, 0.5), vulnerable: true, holding: Some(apple) }];
        run(&mut ents, &mut w, &players, 160);
        assert!(ents.list[&pig].body.pos.x < 3.0, "{:?}", ents.list[&pig].body.pos);
    }

    #[test]
    fn a_fence_keeps_animals_in_even_when_they_are_tempted_out() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        for i in -3..=3 {
            for (x, z) in [(i, -3), (i, 3), (-3, i), (3, i)] {
                let p = BlockPos::new(x, 61, z);
                w.set(p, BlockState::of(id::FENCE));
            }
        }
        rules::drain_updates(&mut w);
        let pig = ents.spawn_mob(EntityKind::Pig, DVec3::new(0.5, 61.0, 0.5));
        let apple = w.reg.item_by_name("apple").expect("apples");
        let players =
            [PlayerView { id: 1000, pos: DVec3::new(6.5, 61.0, 0.5), vulnerable: true, holding: Some(apple) }];
        run(&mut ents, &mut w, &players, 400);
        let at = ents.list[&pig].body.pos;
        assert!(at.x.abs() < 3.0 && at.z.abs() < 3.0, "got out to {at:?}");
    }

    fn ride(ents: &mut Entities, id: u32) -> &mut Ride {
        match &mut ents.list.get_mut(&id).expect("there").data {
            Data::Vehicle(r) => r,
            _ => panic!("not a vehicle"),
        }
    }

    #[test]
    fn a_boat_floats_at_the_surface_and_goes_where_it_is_rowed() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        for x in -2..12 {
            for z in -2..30 {
                for y in 61..=62 {
                    w.set(BlockPos::new(x, y, z), BlockState::of(id::WATER));
                }
            }
        }
        let boat = ents.spawn_vehicle(&w, EntityKind::Boat, DVec3::new(5.5, 64.0, 5.5));
        let watcher = [PlayerView { id: 1000, pos: DVec3::new(5.5, 70.0, 5.5), vulnerable: false, holding: None }];
        run(&mut ents, &mut w, &watcher, 60);
        let y = ents.list[&boat].body.pos.y;
        assert!((62.6..63.0).contains(&y), "afloat at {y}");
        for _ in 0..60 {
            ride(&mut ents, boat).ahead = 1.0;
            run(&mut ents, &mut w, &watcher, 1);
        }
        let at = ents.list[&boat].body.pos;
        assert!(at.z > 12.0 && (at.x - 5.5).abs() < 0.5, "rowed along its bow to {at}");
        // Turning, then rowing, takes it off to one side.
        for _ in 0..30 {
            let r = ride(&mut ents, boat);
            (r.ahead, r.turn) = (1.0, 1.0);
            run(&mut ents, &mut w, &watcher, 1);
        }
        assert!((ents.list[&boat].body.pos.x - 5.5).abs() > 0.5, "it turned");
    }

    #[test]
    fn a_cart_runs_round_a_corner_and_off_the_end_of_the_line() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        for x in 0..6 {
            w.set(BlockPos::new(x, 61, 0), BlockState::new(id::RAIL, 1));
        }
        w.set(BlockPos::new(6, 61, 0), BlockState::new(id::RAIL, 7));
        for z in 1..4 {
            w.set(BlockPos::new(6, 61, z), BlockState::new(id::RAIL, 0));
        }
        let cart = ents.spawn_vehicle(&w, EntityKind::Cart, DVec3::new(0.5, 61.2, 0.5));
        assert!(ride(&mut ents, cart).track.is_some(), "put down on its rail");
        ride(&mut ents, cart).speed = 0.3;
        let mut furthest_south = 0.0f64;
        for _ in 0..120 {
            run(&mut ents, &mut w, &[], 1);
            let at = ents.list[&cart].body.pos;
            if at.x > 6.0 {
                furthest_south = furthest_south.max(at.z);
            }
        }
        assert!(furthest_south > 3.5, "came round the corner and down the line, to {furthest_south}");
        let at = ents.list[&cart].body.pos;
        assert!(ride(&mut ents, cart).track.is_none(), "off the end, it rolls free: {at}");
    }

    #[test]
    fn a_cart_sent_up_a_slope_too_slowly_rolls_back_down() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        w.set(BlockPos::new(0, 61, 0), BlockState::new(id::RAIL, 1));
        w.set(BlockPos::new(1, 61, 0), BlockState::new(id::RAIL, 2));
        w.set(BlockPos::new(2, 62, 0), BlockState::new(id::RAIL, 1));
        w.set(BlockPos::new(2, 61, 0), BlockState::of(id::STONE));
        let cart = ents.spawn_vehicle(&w, EntityKind::Cart, DVec3::new(0.5, 61.1, 0.5));
        ride(&mut ents, cart).speed = 0.1;
        let mut highest = 0.0f64;
        for _ in 0..60 {
            run(&mut ents, &mut w, &[], 1);
            highest = highest.max(ents.list[&cart].body.pos.y);
        }
        assert!(highest > 61.2 && highest < 62.0, "partway up: {highest}");
        assert!(ents.list[&cart].body.pos.x < 1.0, "and back down to the start");
    }

    #[test]
    fn a_cart_stops_short_of_a_wall_at_the_end_of_the_line() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        for x in 0..6 {
            w.set(BlockPos::new(x, 61, 0), BlockState::new(id::RAIL, 1));
        }
        w.set(BlockPos::new(6, 61, 0), BlockState::of(id::STONE));
        let cart = ents.spawn_vehicle(&w, EntityKind::Cart, DVec3::new(0.5, 61.2, 0.5));
        ride(&mut ents, cart).speed = 0.45;
        run(&mut ents, &mut w, &[], 40);
        let (bw, _) = EntityKind::Cart.size();
        let at = ents.list[&cart].body.pos;
        assert!(at.x + bw * 0.5 <= 6.0, "not into the wall: {at}");
        assert!(at.x > 5.0, "right up to it: {at}");
        let r = ride(&mut ents, cart);
        assert!(r.track.is_some() && r.speed == 0.0, "still on its rail, at rest");
    }

    #[test]
    fn a_zombie_cannot_hit_through_a_wall() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        // A hut one block wide around the player: walls two high, and a roof.
        let me = BlockPos::new(3, 61, 0);
        for d in Dir::HORIZONTAL {
            for up in 0..2 {
                w.set(me.offset(d).add(0, up, 0), BlockState::of(id::STONE));
            }
        }
        w.set(me.add(0, 2, 0), BlockState::of(id::STONE));
        ents.spawn_mob(EntityKind::Zombie, DVec3::new(5.35, 61.0, 0.5));
        let players = player_at(3.5);
        let damage = run(&mut ents, &mut w, &players, 100);
        assert!(damage.iter().all(|d| d.target != 1000), "no blows through stone: {damage:?}");
    }

    #[test]
    fn a_zombie_with_no_way_round_lava_does_not_walk_into_it() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        // A moat of lava, too wide to cross, all the way along.
        for z in -20..=20 {
            for x in 5..=7 {
                w.set(BlockPos::new(x, 60, z), BlockState::of(id::LAVA));
            }
        }
        let zombie = ents.spawn_mob(EntityKind::Zombie, DVec3::new(10.5, 61.0, 0.5));
        let players = player_at(0.5);
        run(&mut ents, &mut w, &players, 200);
        let at = ents.list.get(&zombie).map(|e| e.body.pos);
        assert!(at.is_some_and(|p| p.x > 7.5), "it stays on its own side: {at:?}");
    }

    #[test]
    fn a_skeleton_arrow_gives_no_credit_and_the_young_teach_nothing() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let pig = ents.spawn_mob(EntityKind::Pig, DVec3::new(3.5, 61.0, 0.5));
        let skeleton = ents.spawn_mob(EntityKind::Skeleton, DVec3::new(12.5, 61.0, 0.5));
        let arrow = ents.spawn(
            EntityKind::Arrow,
            DVec3::new(1.5, 61.5, 0.5),
            Data::Arrow { owner: skeleton, stuck: false, damage: 3 },
        );
        ents.list.get_mut(&arrow).expect("arrow").body.vel = DVec3::new(1.0, 0.05, 0.0);
        run(&mut ents, &mut w, &[], 10);
        assert_eq!(brain(&ents, pig).hit_by, None);
        // A young one brought down by a player leaves no lesson behind.
        if let Some(Data::Mob(b)) = ents.list.get_mut(&pig).map(|e| &mut e.data) {
            (b.hit_by, b.grow) = (Some(1000), GROW_TICKS);
        }
        let young = ents.list[&pig].clone();
        die(&young, &mut w);
        assert!(w.xp_awards.is_empty());
    }

    #[test]
    fn a_float_rides_the_water_gets_a_bite_and_goes_when_the_rod_is_put_away() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        for x in 2..=6 {
            for z in -2..=2 {
                w.set(BlockPos::new(x, 60, z), BlockState::of(id::WATER));
            }
        }
        let rod = w.reg.item_by_name(it::FISHING_ROD);
        let angler = |holding| [PlayerView { id: 1000, pos: DVec3::new(0.5, 61.0, 0.5), vulnerable: true, holding }];
        let float =
            ents.spawn(EntityKind::Float, DVec3::new(4.5, 62.0, 0.5), Data::Float { owner: 1000, wait: 40, bite: 0 });
        let biting = |ents: &Entities| matches!(ents.list[&float].data, Data::Float { bite, .. } if bite > 0);
        run(&mut ents, &mut w, &angler(rod), 30);
        let y = ents.list[&float].body.pos.y;
        assert!((60.5..61.2).contains(&y), "it rides at the surface: {y}");
        assert!(!biting(&ents), "nothing yet");
        run(&mut ents, &mut w, &angler(rod), 40);
        assert!(biting(&ents), "something takes the bait");
        run(&mut ents, &mut w, &angler(rod), u32::from(BITE_TICKS) + 1);
        assert!(!biting(&ents), "and gets away if the line is not pulled");
        // With the rod put away, the float comes in by itself.
        run(&mut ents, &mut w, &angler(None), 2);
        assert!(!ents.list.contains_key(&float));
    }

    #[test]
    fn a_gloomcrawler_climbs_a_wall_to_reach_its_prey() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        for y in 61..=65 {
            for z in -3..=3 {
                w.set(BlockPos::new(4, y, z), BlockState::of(id::STONE));
            }
        }
        let crawler = ents.spawn_mob(EntityKind::Gloomcrawler, DVec3::new(1.5, 61.0, 0.5));
        let players = [PlayerView { id: 1000, pos: DVec3::new(4.5, 66.0, 0.5), vulnerable: true, holding: None }];
        let mut highest = 0.0f64;
        for _ in 0..80 {
            run(&mut ents, &mut w, &players, 1);
            highest = highest.max(ents.list[&crawler].body.pos.y);
        }
        assert!(highest > 64.0, "climbed only to {highest}");
    }

    #[test]
    fn a_lurker_backs_away_from_light() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        // A roof, so it is dark underneath.
        for x in -12..=12 {
            for z in -12..=12 {
                w.set(BlockPos::new(x, 64, z), BlockState::of(id::STONE));
            }
        }
        let lurker = ents.spawn_mob(EntityKind::Lurker, DVec3::new(0.5, 61.0, 0.5));
        // Someone near, so that it stays, but nobody it can hurt.
        let watcher = [PlayerView { id: 1000, pos: DVec3::new(6.5, 61.0, 0.5), vulnerable: false, holding: None }];
        run(&mut ents, &mut w, &watcher, 20);
        assert_eq!(brain(&ents, lurker).panic, 0, "at home in the dark");
        let at = BlockPos::from_vec(ents.list[&lurker].body.pos);
        w.set(at.offset(Dir::East), BlockState::of(id::TORCH));
        run(&mut ents, &mut w, &watcher, 20);
        assert!(brain(&ents, lurker).panic > 0, "a torch sends it away");
    }

    #[test]
    fn a_zombie_ignores_a_player_who_cannot_be_hurt() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        ents.spawn_mob(EntityKind::Zombie, DVec3::new(5.5, 61.0, 0.5));
        let players = [PlayerView { id: 1000, pos: DVec3::new(0.5, 61.0, 0.5), vulnerable: false, holding: None }];
        assert!(run(&mut ents, &mut w, &players, 100).is_empty());
    }

    #[test]
    fn a_skeleton_shoots_and_the_arrow_hurts() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        ents.spawn_mob(EntityKind::Skeleton, DVec3::new(10.5, 61.0, 0.5));
        let players = player_at(0.5);
        let damage = run(&mut ents, &mut w, &players, 200);
        assert!(damage.iter().any(|d| d.target == 1000 && d.amount == 3), "{damage:?}");
    }

    #[test]
    fn a_skeleton_does_not_shoot_through_a_wall() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        for z in -12..=12 {
            for y in 61..66 {
                w.set(BlockPos::new(6, y, z), BlockState::of(id::STONE));
            }
        }
        ents.spawn_mob(EntityKind::Skeleton, DVec3::new(9.5, 61.0, 0.5));
        let players = player_at(3.5);
        let damage = run(&mut ents, &mut w, &players, 60);
        assert!(damage.is_empty(), "{damage:?}");
        assert!(ents.list.values().all(|e| e.kind != EntityKind::Arrow));
    }

    #[test]
    fn animals_wander_and_flee_when_hit() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let id = ents.spawn_mob(EntityKind::Sheep, DVec3::new(0.5, 61.0, 0.5));
        run(&mut ents, &mut w, &[], 600);
        let moved = ents.list[&id].body.pos;
        assert!(moved.distance(DVec3::new(0.5, 61.0, 0.5)) > 0.5, "stood still at {moved:?}");
        let e = ents.list.get_mut(&id).expect("sheep");
        assert!(hurt(e, 1, DVec3::X));
        assert!(!hurt(e, 1, DVec3::X), "a moment of protection after a hit");
        let Data::Mob(brain) = &e.data else { panic!("a mob") };
        assert!(brain.panic > 0);
    }

    #[test]
    fn mobs_leave_their_loot_when_they_die() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let id = ents.spawn_mob(EntityKind::Pig, DVec3::new(0.5, 61.0, 0.5));
        ents.list.get_mut(&id).expect("pig").health = 0;
        run(&mut ents, &mut w, &[], 2);
        assert!(!ents.list.contains_key(&id));
        assert!(ents.list.values().any(|e| e.kind == EntityKind::Item));
    }

    #[test]
    fn hostile_mobs_vanish_when_nobody_is_near() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let id = ents.spawn_mob(EntityKind::Zombie, DVec3::new(0.5, 61.0, 0.5));
        let far = [PlayerView { id: 1000, pos: DVec3::new(500.0, 61.0, 0.5), vulnerable: true, holding: None }];
        run(&mut ents, &mut w, &far, 2);
        assert!(!ents.list.contains_key(&id));
    }

    #[test]
    fn a_torch_keeps_monsters_from_spawning_around_it() {
        let mut w = flat_world();
        let spot = BlockPos::new(3, 61, 3);
        assert!(dark_enough(&w, spot, true));
        assert!(!dark_enough(&w, spot, false), "open sky in daytime is too bright");
        w.set(BlockPos::new(5, 61, 3), BlockState::of(id::TORCH));
        assert!(!dark_enough(&w, spot, true));
    }

    #[test]
    fn natural_spawning_respects_the_cap() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        let players = [PlayerView { id: 1000, pos: DVec3::new(0.5, 61.0, 0.5), vulnerable: true, holding: None }];
        for _ in 0..5000 {
            natural_spawns(&mut ents, &mut w, &players, true);
        }
        assert!(ents.count_mobs(true) > 0, "nothing ever spawned at night on open ground");
        assert!(ents.count_mobs(true) <= 10);
    }

    #[test]
    fn moves_only_exist_when_something_moved() {
        let a = Sent::of(DVec3::new(1.0, 2.0, 3.0), 100, 0);
        assert_eq!(a.move_to(1, a), Ok(None));
        let b = Sent::of(DVec3::new(1.5, 2.0, 3.0), 100, 0);
        let m = a.move_to(1, b).expect("close").expect("moved");
        assert_eq!(m.d, [128, 0, 0]);
        assert!(a.move_to(1, Sent::of(DVec3::new(500.0, 2.0, 3.0), 0, 0)).is_err(), "too far to say in a move");
    }

    #[test]
    fn yaw_points_the_way_physics_walks() {
        for (dx, dz) in [(0.0, 1.0), (1.0, 0.0), (-1.0, 0.0), (0.0, -1.0), (0.7, -0.7)] {
            let (s, c) = dmath::sin_cos(yaw_towards(dx, dz));
            let (fx, fz) = (-s, c);
            let dot = fx * dx + fz * dz;
            assert!(dot > 0.99 * (dx * dx + dz * dz).sqrt(), "{dx},{dz}: {fx},{fz}");
        }
    }
}
