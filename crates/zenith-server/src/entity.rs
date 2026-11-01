//! Everything that moves and is not a player: items, falling blocks, charges, arrows and mobs.
use crate::rules;
use crate::world::{ServerWorld, WorldEvent};
use std::collections::BTreeMap;
use zenith_content::{BlockState, ItemStack, id};
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

impl Sent {
    pub fn of(pos: DVec3, yaw: u16, pitch: i16) -> Self {
        let q = |v: f64| (v * POS_SCALE).round() as i32;
        Self { pos: [q(pos.x), q(pos.y), q(pos.z)], yaw: (yaw >> 8) as u8, pitch: (pitch >> 8) as i8 }
    }
    /// The move from `self` to `now`, if there is one. `Err` if it is too far to say in a move.
    pub fn move_to(&self, id: u32, now: Sent) -> Result<Option<EntityMove>, ()> {
        if *self == now {
            return Ok(None);
        }
        let d = [now.pos[0] - self.pos[0], now.pos[1] - self.pos[1], now.pos[2] - self.pos[2]];
        if d.iter().any(|v| v.unsigned_abs() > i16::MAX as u32) {
            return Err(());
        }
        Ok(Some(EntityMove { id, d: [d[0] as i16, d[1] as i16, d[2] as i16], yaw: now.yaw, pitch: now.pitch }))
    }
}

#[derive(Clone, Debug)]
pub enum Data {
    Item { stack: ItemStack, delay: u16 },
    Falling(BlockState),
    Tnt { fuse: i16 },
    Arrow { owner: u32, stuck: bool },
    Mob(Box<Brain>),
}

#[derive(Clone, Debug, Default)]
pub struct Brain {
    pub target: Option<u32>,
    path: Vec<BlockPos>,
    path_at: usize,
    repath_in: u16,
    idle: u16,
    attack_cool: u16,
    pub panic: u16,
}

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
        }
    }
    pub fn aabb(&self) -> Aabb {
        let (w, h) = self.kind.size();
        self.body.aabb(w, h)
    }
}

/// What the entity system needs to know about each player, copied once a tick.
#[derive(Clone, Copy, Debug)]
pub struct PlayerView {
    pub id: u32,
    pub pos: DVec3,
    pub vulnerable: bool,
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
            Entity { id, kind, body: Body::at(pos), yaw: 0, pitch: 0, age: 0, health: health_of(kind), hurt_cool: 0, data, sent },
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
            }
        }
    }
}

pub const fn is_hostile(kind: EntityKind) -> bool {
    matches!(kind, EntityKind::Zombie | EntityKind::Skeleton)
}

/// Runs every entity for one tick. Returns the damage they dealt, for the caller to apply.
pub fn tick_all(ents: &mut Entities, w: &mut ServerWorld, players: &[PlayerView], damage: &mut Vec<Damage>) {
    let mut boxes = std::mem::take(&mut ents.boxes);
    let mut remove: Vec<u32> = Vec::new();
    let mut arrows: Vec<(u32, DVec3, DVec3)> = Vec::new();
    let mut blasts: Vec<(DVec3, f64)> = Vec::new();
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
            Data::Arrow { owner, stuck } => {
                let owner = *owner;
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
                let mut struck = None;
                for p in players {
                    if p.id == owner || !p.vulnerable {
                        continue;
                    }
                    let b = Aabb::standing(p.pos, physics::PLAYER_WIDTH, physics::PLAYER_HEIGHT).inflate(0.1);
                    if let Some((t, _)) = b.ray(e.body.pos, v.normalize_or_zero()) {
                        if t <= reach && t < block_t {
                            struck = Some(p.id);
                            break;
                        }
                    }
                }
                if let Some(target) = struck {
                    damage.push(Damage { target, amount: 3, knock: v.normalize_or_zero() * 0.3, attacker: None });
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
            Data::Mob(_) => {
                tick_mob(e, w, players, &mut searches, damage, &mut arrows, &mut boxes);
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
    for (owner, pos, vel) in arrows {
        let id = ents.spawn(EntityKind::Arrow, pos, Data::Arrow { owner, stuck: false });
        if let Some(e) = ents.list.get_mut(&id) {
            e.body.vel = vel;
        }
        w.effect(EffectKind::Shoot, pos);
    }
    for (at, power) in blasts {
        explode(ents, w, players, at, power, damage);
    }
}

fn die(e: &Entity, w: &mut ServerWorld) {
    w.effect(EffectKind::Death, e.body.pos);
    let loot: &[(&str, i32, i32)] = match e.kind {
        EntityKind::Sheep => &[("wool_white", 1, 2), ("raw_meat", 1, 2)],
        EntityKind::Pig => &[("raw_meat", 1, 3)],
        EntityKind::Chicken => &[("raw_meat", 1, 1)],
        EntityKind::Zombie => &[("raw_meat", 0, 2), ("copper_ingot", 0, 1)],
        EntityKind::Skeleton => &[("stick", 0, 3), ("coal", 0, 2)],
        _ => &[],
    };
    for &(name, lo, hi) in loot {
        let count = w.rng.range(lo, hi);
        if count > 0 {
            if let Some(item) = w.reg.item_by_name(name) {
                w.events.push(WorldEvent::DropItem { pos: e.body.pos + DVec3::Y * 0.5, stack: ItemStack::new(item, count as u8) });
            }
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

fn tick_mob(
    e: &mut Entity,
    w: &mut ServerWorld,
    players: &[PlayerView],
    searches: &mut u32,
    damage: &mut Vec<Damage>,
    arrows: &mut Vec<(u32, DVec3, DVec3)>,
    boxes: &mut Vec<Aabb>,
) {
    let reg = w.reg.clone();
    let (bw, bh) = e.kind.size();
    let Data::Mob(brain) = &mut e.data else { return };
    brain.attack_cool = brain.attack_cool.saturating_sub(1);
    brain.panic = brain.panic.saturating_sub(1);
    brain.repath_in = brain.repath_in.saturating_sub(1);
    let at = BlockPos::from_vec(e.body.pos + DVec3::Y * 0.2);
    let hostile = is_hostile(e.kind);

    // Hazards.
    physics::sense(&*w, &reg, &mut e.body, bh);
    if e.body.in_lava && e.age % 10 == 0 {
        e.health -= 2;
        e.hurt_cool = 10;
    }

    // What it wants.
    let mut wish_cell: Option<BlockPos> = None;
    if hostile {
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
            if e.kind == EntityKind::Zombie && d < 1.9 && brain.attack_cool == 0 {
                brain.attack_cool = 20;
                damage.push(Damage { target: target.id, amount: 3, knock: to * 0.4, attacker: Some(e.id) });
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
        if let Some(goal) = goal {
            if brain.repath_in == 0 && *searches > 0 {
                *searches -= 1;
                brain.repath_in = 20 + (e.id % 8) as u16;
                brain.path = path::find(&*w, &reg, at, goal, 200).unwrap_or_default();
                brain.path_at = 0;
            }
        }
    }

    let mut wish = DVec3::ZERO;
    let mut jump = false;
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
    if body.on_ground {
        if falling && body.fall > 4.0 && e.kind != EntityKind::Chicken {
            e.health -= (body.fall - 3.0) as i16;
        }
        body.fall = 0.0;
        body.vel.y = -0.0784;
    } else if delta.y < 0.0 {
        body.fall -= delta.y;
    }
    // Pushed against something with nowhere to go: try hopping over it.
    if wish != DVec3::ZERO && body.on_ground && (body.vel.x, body.vel.z) != was_blocked_before && (body.vel.x == 0.0 || body.vel.z == 0.0) {
        body.vel.y = 0.42;
        body.on_ground = false;
    }
    let cactus = w.block(BlockPos::from_vec(body.pos)).kind() == id::CACTUS;
    if cactus && e.age % 20 == 0 {
        e.health -= 1;
    }
}

/// Hits an entity. Returns whether it took the blow.
pub fn hurt(e: &mut Entity, amount: i16, knock: DVec3) -> bool {
    if e.hurt_cool > 0 {
        return false;
    }
    e.hurt_cool = 10;
    e.health -= amount;
    e.body.vel += DVec3::new(knock.x, 0.3, knock.z);
    if let Data::Mob(brain) = &mut e.data {
        if !is_hostile(e.kind) {
            brain.panic = 80;
            brain.repath_in = 0;
            brain.idle = 0;
        }
    }
    true
}

/// A blast: rays from the middle chew through blocks by how tough they are, and anything nearby is
/// hurt and thrown. A charge caught in it goes off after a moment.
pub fn explode(ents: &mut Entities, w: &mut ServerWorld, players: &[PlayerView], at: DVec3, power: f64, damage: &mut Vec<Damage>) {
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
                let dir = DVec3::new(ix as f64 / 3.5 - 1.0, iy as f64 / 3.5 - 1.0, iz as f64 / 3.5 - 1.0).normalize_or_zero();
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
            let e = ents.spawn(EntityKind::Tnt, p.center() - DVec3::Y * 0.5, Data::Tnt { fuse: 10 + w.rng.below(20) as i16 });
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
            damage.push(Damage { target: p.id, amount: (falloff * 24.0) as i16, knock: to * falloff * 1.5, attacker: None });
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
    let top = chunk.top(p.x & 15, p.z & 15, |s| w.reg.lut.opacity[s.kind() as usize] >= 15).unwrap_or(zenith_core::pos::MIN_Y);
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
            if path::can_stand(&*w, &reg, spot) && w.reg.lut.collides[w.block(spot.offset(Dir::Down)).kind() as usize] && dark_enough(w, spot, night) {
                let kind = if w.rng.below(3) == 0 { EntityKind::Skeleton } else { EntityKind::Zombie };
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
                ents.spawn_mob(kind, DVec3::new(surface.x as f64 + 0.5 + ox, surface.y as f64, surface.z as f64 + 0.5 + oz));
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
            PlayerView { id: 100, pos: DVec3::new(2.5, 61.0, 0.5), vulnerable: true },
            PlayerView { id: 101, pos: DVec3::new(40.5, 61.0, 0.5), vulnerable: true },
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
        [PlayerView { id: 1000, pos: DVec3::new(x, 61.0, 0.5), vulnerable: true }]
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

    #[test]
    fn a_zombie_ignores_a_player_who_cannot_be_hurt() {
        let (mut ents, mut w) = (Entities::default(), flat_world());
        ents.spawn_mob(EntityKind::Zombie, DVec3::new(5.5, 61.0, 0.5));
        let players = [PlayerView { id: 1000, pos: DVec3::new(0.5, 61.0, 0.5), vulnerable: false }];
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
        let far = [PlayerView { id: 1000, pos: DVec3::new(500.0, 61.0, 0.5), vulnerable: true }];
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
        let players = [PlayerView { id: 1000, pos: DVec3::new(0.5, 61.0, 0.5), vulnerable: true }];
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
            assert!(dot > 0.99 * (dx * dx + dz * dz as f64).sqrt(), "{dx},{dz}: {fx},{fz}");
        }
    }
}
