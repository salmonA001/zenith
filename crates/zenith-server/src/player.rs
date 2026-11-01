//! A connected player, and what they can do. Every player is one of these: the host of a world is
//! no different from a stranger who joined it, and no code ever pretends to be anyone else.
use crate::entity::{Damage, Data, Entities, PlayerView, Sent, hurt, yaw_towards};
use crate::inventory::{self, Holder, Inventory, Slot};
use crate::persist::{BlockEntity, PlayerSave};
use crate::rules;
use crate::world::ServerWorld;
use std::collections::VecDeque;
use zenith_content::{BlockState, ItemKind, ItemStack, Registry, Shape, Tool, id};
use zenith_core::glam::DVec3;
use zenith_core::pos::MIN_Y;
use zenith_core::{Aabb, BlockPos, ChunkPos, Dir, FxSet};
use zenith_protocol::{BlockChange, ChunkDelta, ClickButton, EffectKind, EntityInfo, EntityKind, GameMode, Peer, S2C, Screen, ScreenKind, Vitals};
use zenith_world::physics::{self, Body, Keys, MoveInput};
use zenith_world::BlockAccess;

/// How far a player can reach, from the eye to the middle of a block, with some forgiveness.
const REACH: f64 = 6.5;
const ATTACK_REACH: f64 = 5.0;
/// A client may run ahead by this many ticks, to ride out a network hiccup. No more: over time it
/// gets one physics step per server tick, so speed hacks do not exist.
const MAX_CREDITS: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenScreen {
    None,
    Bench,
    Furnace(BlockPos),
    Chest(BlockPos),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dig {
    pub pos: BlockPos,
    pub progress: f32,
    stage: u8,
}

pub struct Player {
    pub id: u32,
    pub name: String,
    pub peer: Peer,
    pub body: Body,
    pub mode: GameMode,
    pub op: bool,
    pub health: u8,
    pub air: u8,
    pub dead: bool,
    hurt_cool: u8,
    regen: u16,
    pub inv: Inventory,
    pub craft: Vec<Slot>,
    pub screen: OpenScreen,
    pub inputs: VecDeque<(u32, MoveInput)>,
    pub last_input: MoveInput,
    pub ack: u32,
    credits: u8,
    pub dig: Option<Dig>,
    attack_cool: u8,
    pub subscribed: FxSet<ChunkPos>,
    pub known: FxSet<u32>,
    pub sent: Sent,
    sent_inv: Vec<Slot>,
    sent_selected: u8,
    sent_cursor: Slot,
    sent_vitals: Option<Vitals>,
    sent_screen: Option<Screen>,
    pub out_blocks: Vec<BlockChange>,
    pub out_deltas: Vec<ChunkDelta>,
    pub boxes: Vec<Aabb>,
    pub ping_sent: Option<(u32, std::time::Instant)>,
    pub ping_ms: u16,
    /// Chat allowance: earned over time, spent per line. Enough for a burst, not for a flood.
    chat_credit: u32,
    chat_tick: u64,
}

impl Player {
    pub fn new(id: u32, name: String, peer: Peer, body: Body, mode: GameMode, op: bool) -> Self {
        Self {
            id,
            name,
            peer,
            body,
            mode,
            op,
            health: 20,
            air: 20,
            dead: false,
            hurt_cool: 0,
            regen: 0,
            inv: Inventory::default(),
            craft: vec![None; 9],
            screen: OpenScreen::None,
            inputs: VecDeque::new(),
            last_input: MoveInput::default(),
            ack: 0,
            credits: 0,
            dig: None,
            attack_cool: 0,
            subscribed: FxSet::default(),
            known: FxSet::default(),
            sent: Sent::of(body.pos, 0, 0),
            sent_inv: Vec::new(),
            sent_selected: u8::MAX,
            sent_cursor: None,
            sent_vitals: None,
            sent_screen: None,
            out_blocks: Vec::new(),
            out_deltas: Vec::new(),
            boxes: Vec::new(),
            ping_sent: None,
            ping_ms: 0,
            chat_credit: 80,
            chat_tick: 0,
        }
    }

    pub fn from_save(id: u32, name: String, peer: Peer, save: PlayerSave) -> Self {
        let mut body = Body::at(save.pos);
        body.flying = save.flying;
        let mut p = Self::new(id, name, peer, body, save.mode, save.op);
        p.health = save.health;
        p.air = save.air;
        p.dead = save.health == 0;
        p.inv = save.inventory;
        p
    }

    pub fn save(&self) -> PlayerSave {
        PlayerSave {
            pos: self.body.pos,
            health: self.health,
            air: self.air,
            mode: self.mode,
            flying: self.body.flying,
            op: self.op,
            inventory: self.inv.clone(),
        }
    }

    pub fn view(&self) -> PlayerView {
        PlayerView { id: self.id, pos: self.body.pos, vulnerable: !self.dead && self.mode == GameMode::Survival }
    }

    pub fn info(&self) -> EntityInfo {
        EntityInfo {
            id: self.id,
            kind: EntityKind::Player,
            pos: self.sent.pos,
            yaw: self.sent.yaw,
            pitch: self.sent.pitch,
            name: Some(self.name.clone()),
            stack: None,
            block: BlockState::AIR,
        }
    }

    pub fn chunk(&self) -> ChunkPos {
        BlockPos::from_vec(self.body.pos).chunk()
    }

    pub fn eye(&self) -> DVec3 {
        self.body.eye()
    }

    pub fn may_chat(&mut self, tick: u64) -> bool {
        // Five lines a second on average, with room for a burst of twenty.
        let earned = tick.saturating_sub(self.chat_tick).min(80) as u32;
        self.chat_tick = tick;
        self.chat_credit = (self.chat_credit + earned).min(80);
        if self.chat_credit < 4 {
            return false;
        }
        self.chat_credit -= 4;
        true
    }

    /// Runs this player's movement for one server tick, and what comes of it.
    pub fn tick(&mut self, w: &mut ServerWorld) {
        if self.dead {
            return;
        }
        self.attack_cool = self.attack_cool.saturating_sub(1);
        self.hurt_cool = self.hurt_cool.saturating_sub(1);
        self.credits = (self.credits + 1).min(MAX_CREDITS);
        let reg = w.reg.clone();
        let mut landed = None;
        let mut any = false;
        while self.credits > 0 {
            let Some((seq, input)) = self.inputs.pop_front() else { break };
            self.credits -= 1;
            self.ack = seq;
            self.last_input = input;
            any = true;
            let result = physics::step(&mut self.body, input, &*w, &reg, &mut self.boxes);
            landed = landed.or(result.landed_after);
        }
        // A silent client still falls, once it has used up its grace.
        if !any && self.credits == MAX_CREDITS {
            let idle = MoveInput { keys: Keys(0), ..self.last_input };
            let result = physics::step(&mut self.body, idle, &*w, &reg, &mut self.boxes);
            landed = landed.or(result.landed_after);
        }
        if self.mode == GameMode::Survival {
            if self.body.flying {
                self.body.flying = false;
            }
            if let Some(distance) = landed {
                if distance > 3.5 && !self.body.in_water {
                    self.take_damage(w, (distance - 3.0) as i16, None);
                }
            }
            self.hazards(w);
        }
        self.tick_dig(w);
    }

    fn hazards(&mut self, w: &mut ServerWorld) {
        if self.dead {
            return;
        }
        let reg = w.reg.clone();
        if self.body.in_lava && w.tick % 10 == 0 {
            self.take_damage(w, 4, None);
        }
        if self.body.pos.y < MIN_Y as f64 - 40.0 && w.tick % 10 == 0 {
            self.take_damage(w, 4, None);
        }
        // What the body is touching.
        if w.tick % 10 == 0 {
            let b = self.body.aabb(physics::PLAYER_WIDTH, physics::PLAYER_HEIGHT).inflate(0.05);
            let (lo, hi) = (BlockPos::from_vec(b.min), BlockPos::from_vec(b.max));
            'scan: for y in lo.y..=hi.y {
                for z in lo.z..=hi.z {
                    for x in lo.x..=hi.x {
                        let def = reg.block(w.block(BlockPos::new(x, y, z)));
                        if def.contact_damage > 0 && def.shape != Shape::Fluid {
                            let amount = def.contact_damage as i16;
                            self.take_damage(w, amount, None);
                            break 'scan;
                        }
                    }
                }
            }
        }
        if self.body.eyes_in_water {
            if w.tick % 15 == 0 {
                self.air = self.air.saturating_sub(1);
            }
            if self.air == 0 && w.tick % 20 == 0 {
                self.take_damage(w, 2, None);
            }
        } else if self.air < 20 && w.tick % 2 == 0 {
            self.air += 1;
        }
        if self.health < 20 && !self.dead {
            self.regen += 1;
            if self.regen >= 60 {
                self.regen = 0;
                self.health += 1;
            }
        }
    }

    /// Takes a hit. Survival only, and with a moment of protection after each hit.
    pub fn take_damage(&mut self, w: &mut ServerWorld, amount: i16, knock: Option<DVec3>) -> bool {
        if self.dead || self.mode != GameMode::Survival || amount <= 0 || self.hurt_cool > 0 {
            return false;
        }
        self.hurt_cool = 10;
        self.regen = 0;
        self.health = (self.health as i16 - amount).max(0) as u8;
        w.effect(EffectKind::Hurt, self.body.pos);
        if let Some(k) = knock {
            self.body.vel += DVec3::new(k.x, 0.3, k.z);
        }
        if self.health == 0 {
            self.die(w);
        }
        true
    }

    /// `/kill`: no protection, no mercy.
    pub fn take_damage_absolute(&mut self, w: &mut ServerWorld) {
        self.hurt_cool = 0;
        self.take_damage(w, i16::MAX, None);
    }

    fn die(&mut self, w: &mut ServerWorld) {
        self.dead = true;
        self.dig = None;
        w.effect(EffectKind::Death, self.body.pos);
        self.close_screen(w);
        for stack in self.inv.slots.iter_mut().filter_map(Option::take) {
            w.events.push(crate::world::WorldEvent::DropItem { pos: self.body.pos + DVec3::Y, stack });
        }
    }

    pub fn respawn(&mut self, spawn: DVec3) {
        if !self.dead {
            return;
        }
        self.dead = false;
        self.health = 20;
        self.air = 20;
        self.body = Body::at(spawn);
        self.inputs.clear();
    }

    /// Whether a block position is in reach, and the chunk around it is here.
    fn in_reach(&self, w: &ServerWorld, pos: BlockPos) -> bool {
        pos.in_world() && w.is_loaded(pos) && self.eye().distance(pos.center()) <= REACH
    }

    fn held_tool(&self, reg: &Registry) -> (Tool, u8, u8, u8) {
        if let Some(stack) = self.inv.held() {
            if let Some(ItemKind::Tool { tool, tier, speed, damage, .. }) = reg.item(stack.item).map(|i| i.kind) {
                return (tool, tier, speed, damage);
            }
        }
        (Tool::Any, 0, 1, 1)
    }

    /// Ticks to break a block with what is in hand, or `None` if it cannot be broken.
    fn dig_ticks(&self, reg: &Registry, state: BlockState) -> Option<f32> {
        reg.dig_ticks(state, self.inv.held().map(|s| s.item))
    }

    pub fn start_dig(&mut self, w: &mut ServerWorld, pos: BlockPos) {
        if self.dead || !self.in_reach(w, pos) {
            // Whatever the client thinks is there, tell it again.
            self.resend_block(w, pos);
            return;
        }
        let state = w.block(pos);
        if state.is_air() {
            return;
        }
        if self.mode == GameMode::Creative {
            let reg = w.reg.clone();
            if reg.block(state).hardness < 0.0 && !self.op {
                self.resend_block(w, pos);
                return;
            }
            w.effect(EffectKind::BlockBroken(state), pos.center());
            w.set(pos, BlockState::AIR);
            return;
        }
        match self.dig_ticks(&w.reg.clone(), state) {
            None => self.resend_block(w, pos),
            Some(t) if t <= 1.0 => self.finish_dig(w, pos, state),
            Some(_) => self.dig = Some(Dig { pos, progress: 0.0, stage: 0 }),
        }
    }

    fn resend_block(&mut self, w: &ServerWorld, pos: BlockPos) {
        self.out_blocks.push(BlockChange { pos, state: w.block(pos) });
    }

    fn tick_dig(&mut self, w: &mut ServerWorld) {
        let Some(mut dig) = self.dig else { return };
        let state = w.block(dig.pos);
        if state.is_air() || !self.in_reach(w, dig.pos) {
            self.cancel_dig(w);
            return;
        }
        let Some(ticks) = self.dig_ticks(&w.reg.clone(), state) else {
            self.cancel_dig(w);
            return;
        };
        dig.progress += 1.0 / ticks.max(1.0);
        if dig.progress >= 1.0 {
            self.dig = None;
            self.finish_dig(w, dig.pos, state);
            return;
        }
        dig.stage = (dig.progress * 10.0) as u8;
        self.dig = Some(dig);
    }

    pub fn cancel_dig(&mut self, _w: &mut ServerWorld) {
        self.dig = None;
    }

    fn finish_dig(&mut self, w: &mut ServerWorld, pos: BlockPos, state: BlockState) {
        let reg = w.reg.clone();
        let def = reg.block(state);
        let (tool, _, _, _) = self.held_tool(&reg);
        if reg.can_harvest(state, self.inv.held().map(|s| s.item)) {
            rules::drop_block(w, pos, state);
            if state.kind() == id::OAK_LEAVES && w.rng.chance(0.06) {
                if let Some(apple) = reg.item_by_name("apple") {
                    w.events.push(crate::world::WorldEvent::DropItem { pos: pos.center(), stack: ItemStack::new(apple, 1) });
                }
            }
        }
        if def.tool == tool && tool != Tool::Any {
            self.inv.damage_held(&reg);
        } else if matches!(reg.item(self.inv.held().map_or(u16::MAX, |s| s.item)).map(|i| i.kind), Some(ItemKind::Tool { .. })) {
            self.inv.damage_held(&reg);
        }
        w.effect(EffectKind::BlockBroken(state), pos.center());
        w.set(pos, BlockState::AIR);
    }

    /// Uses what is in hand on a face of a block: opens it if it opens, otherwise places.
    pub fn use_on(&mut self, w: &mut ServerWorld, others: &[Aabb], ents: &Entities, pos: BlockPos, face: u8) {
        let Some(face) = Dir::from_u8(face) else { return };
        if self.dead || !self.in_reach(w, pos) {
            self.resend_block(w, pos);
            self.resend_block(w, pos.offset(face));
            return;
        }
        let target = w.block(pos);
        let sneaking = self.last_input.keys.has(Keys::SNEAK);
        if !sneaking {
            match target.kind() {
                id::BENCH => {
                    self.open(w, OpenScreen::Bench);
                    return;
                }
                id::FURNACE => {
                    w.furnace(pos);
                    self.open(w, OpenScreen::Furnace(pos));
                    return;
                }
                id::CHEST => {
                    w.chest(pos);
                    w.mark_dirty(pos.chunk());
                    w.effect(EffectKind::Open, pos.center());
                    self.open(w, OpenScreen::Chest(pos));
                    return;
                }
                id::LEVER => {
                    rules::toggle_lever(w, pos);
                    return;
                }
                id::TNT => {
                    w.set(pos, BlockState::AIR);
                    w.events.push(crate::world::WorldEvent::Prime { pos });
                    return;
                }
                _ => {}
            }
        }
        self.place(w, others, ents, pos, target, face);
    }

    fn open(&mut self, w: &mut ServerWorld, screen: OpenScreen) {
        self.close_screen(w);
        self.screen = screen;
    }

    fn place(&mut self, w: &mut ServerWorld, others: &[Aabb], ents: &Entities, clicked: BlockPos, clicked_state: BlockState, face: Dir) {
        let reg = w.reg.clone();
        let Some(stack) = self.inv.held() else { return };
        let Some(ItemKind::Block(kind)) = reg.item(stack.item).map(|i| i.kind) else { return };
        let cell = if reg.block(clicked_state).replaceable { clicked } else { clicked.offset(face) };
        let reject = |me: &mut Self, w: &ServerWorld| {
            me.resend_block(w, cell);
            me.resend_block(w, clicked);
        };
        if !self.in_reach(w, cell) || !reg.block(w.block(cell)).replaceable {
            reject(self, w);
            return;
        }
        let Some(meta) = placement_meta(reg.block(BlockState::of(kind)).shape, face, self.last_input.yaw) else {
            reject(self, w);
            return;
        };
        // Leaves a player places stay put.
        let meta = if reg.block(BlockState::of(kind)).tint == zenith_content::Tint::Foliage && reg.block(BlockState::of(kind)).shape == Shape::Cube { 1 } else { meta };
        let state = BlockState::new(kind, meta);
        if reg.block(state).fragile && !rules::has_support(w, cell, state) {
            reject(self, w);
            return;
        }
        let shape = reg.block(state).shape;
        let base = DVec3::new(cell.x as f64, cell.y as f64, cell.z as f64);
        for b in shape.collision(state.meta()) {
            let solid = Aabb::new(
                base + DVec3::new(b[0][0] as f64, b[0][1] as f64, b[0][2] as f64) / 16.0,
                base + DVec3::new(b[1][0] as f64, b[1][1] as f64, b[1][2] as f64) / 16.0,
            );
            let mine = self.body.aabb(physics::PLAYER_WIDTH, physics::PLAYER_HEIGHT);
            let blocked = mine.intersects(&solid)
                || others.iter().any(|o| o.intersects(&solid))
                || ents.list.values().any(|e| e.kind.is_mob() && e.aabb().intersects(&solid));
            if blocked {
                reject(self, w);
                return;
            }
        }
        if self.mode == GameMode::Survival {
            self.inv.consume_held(1);
        }
        w.set(cell, state);
        match kind {
            // Placed by a player, so it holds what they put in it and nothing else.
            id::CHEST => {
                w.block_entities.insert(cell, BlockEntity::Chest(vec![None; inventory::CHEST_SIZE]));
            }
            id::FURNACE => {
                w.block_entities.insert(cell, BlockEntity::Furnace(inventory::Furnace::default()));
            }
            _ => {}
        }
        w.effect(EffectKind::BlockPlaced(state), cell.center());
    }

    /// Eats what is in hand, if it is food.
    pub fn use_item(&mut self, w: &mut ServerWorld) {
        if self.dead {
            return;
        }
        let reg = w.reg.clone();
        let Some(stack) = self.inv.held() else { return };
        let Some(ItemKind::Food { heal }) = reg.item(stack.item).map(|i| i.kind) else { return };
        if self.health >= 20 {
            return;
        }
        self.health = (self.health + heal).min(20);
        self.inv.consume_held(1);
        w.effect(EffectKind::Eat, self.body.pos);
    }

    /// Swings at an entity. Returns damage to deal to another player, if that is who was hit.
    pub fn attack(&mut self, w: &mut ServerWorld, ents: &mut Entities, others: &[PlayerView], target: u32) -> Option<Damage> {
        if self.dead || self.attack_cool > 0 || target == self.id {
            return None;
        }
        self.attack_cool = 8;
        let reg = w.reg.clone();
        let (tool, _, _, damage) = self.held_tool(&reg);
        let eye = self.eye();
        let amount = damage as i16;
        if let Some(e) = ents.list.get_mut(&target) {
            let close = e.aabb().center().distance(eye) <= ATTACK_REACH + e.kind.size().0;
            if !close {
                return None;
            }
            if !e.kind.is_mob() {
                return None;
            }
            let knock = (e.body.pos - self.body.pos).normalize_or_zero() * 0.4;
            if hurt(e, amount, knock) {
                w.effect(EffectKind::Hurt, e.body.pos);
                if tool != Tool::Any {
                    self.inv.damage_held(&reg);
                }
                if let Data::Mob(_) = e.data {
                    e.yaw = yaw_towards(self.body.pos.x - e.body.pos.x, self.body.pos.z - e.body.pos.z);
                }
            }
            return None;
        }
        let other = others.iter().find(|p| p.id == target && p.vulnerable)?;
        if other.pos.distance(self.body.pos) > ATTACK_REACH {
            return None;
        }
        let knock = (other.pos - self.body.pos).normalize_or_zero() * 0.4;
        Some(Damage { target, amount, knock, attacker: Some(self.id) })
    }

    pub fn select(&mut self, slot: u8) {
        self.inv.selected = slot % 9;
        self.dig = None;
    }

    /// Throws what is in hand. Returns where and what.
    pub fn throw_held(&mut self, all: bool) -> Option<(DVec3, DVec3, ItemStack)> {
        if self.dead {
            return None;
        }
        let stack = self.inv.held()?;
        let count = if all { stack.count } else { 1 };
        self.inv.consume_held(count);
        let dir = self.last_input.look();
        Some((self.eye() + dir * 0.3 - DVec3::Y * 0.3, dir * 0.3 + DVec3::Y * 0.1, ItemStack { count, ..stack }))
    }

    pub fn click(&mut self, w: &mut ServerWorld, slot: u16, button: ClickButton) {
        if self.dead {
            return;
        }
        let reg = w.reg.clone();
        match self.screen {
            OpenScreen::None => {
                self.craft.truncate(9);
                let mut grid = std::mem::take(&mut self.craft);
                inventory::click(&reg, &mut self.inv, &mut Holder::Crafting { size: 2, grid: &mut grid }, slot, button);
                self.craft = grid;
            }
            OpenScreen::Bench => {
                let mut grid = std::mem::take(&mut self.craft);
                inventory::click(&reg, &mut self.inv, &mut Holder::Crafting { size: 3, grid: &mut grid }, slot, button);
                self.craft = grid;
            }
            OpenScreen::Furnace(p) => {
                if self.in_reach(w, p) && w.block(p).kind() == id::FURNACE {
                    inventory::click(&reg, &mut self.inv, &mut Holder::Furnace(w.furnace(p)), slot, button);
                    w.mark_dirty(p.chunk());
                }
            }
            OpenScreen::Chest(p) => {
                if self.in_reach(w, p) && w.block(p).kind() == id::CHEST {
                    inventory::click(&reg, &mut self.inv, &mut Holder::Chest(w.chest(p)), slot, button);
                    w.mark_dirty(p.chunk());
                }
            }
        }
    }

    /// Closes the open screen. What was left on the grid and the cursor comes back, or falls.
    pub fn close_screen(&mut self, w: &mut ServerWorld) {
        self.screen = OpenScreen::None;
        let reg = w.reg.clone();
        let back: Vec<ItemStack> = self.craft.iter_mut().filter_map(Option::take).chain(self.inv.cursor.take()).collect();
        for stack in back {
            if let Some(left) = self.inv.add(&reg, stack) {
                w.events.push(crate::world::WorldEvent::DropItem { pos: self.body.pos + DVec3::Y, stack: left });
            }
        }
    }

    /// Picks up an item, as much as fits. Returns what is left of it.
    pub fn pick_up(&mut self, reg: &Registry, stack: ItemStack) -> Slot {
        if self.dead {
            return Some(stack);
        }
        self.inv.add(reg, stack)
    }

    fn screen_view(&self, w: &ServerWorld) -> Option<Screen> {
        match self.screen {
            OpenScreen::None => {
                let grid: Vec<Slot> = self.craft.iter().take(4).copied().collect();
                if grid.iter().all(Option::is_none) {
                    return None;
                }
                let result = inventory::craft_result(&w.reg, 2, &grid);
                let mut slots = grid;
                slots.push(result);
                Some(Screen { kind: ScreenKind::Inventory, slots, progress: [0, 0] })
            }
            OpenScreen::Bench => {
                let grid: Vec<Slot> = self.craft.iter().take(9).copied().collect();
                let result = inventory::craft_result(&w.reg, 3, &grid);
                let mut slots = grid;
                slots.push(result);
                Some(Screen { kind: ScreenKind::Bench, slots, progress: [0, 0] })
            }
            OpenScreen::Furnace(p) => match w.block_entities.get(&p) {
                Some(BlockEntity::Furnace(f)) => {
                    Some(Screen { kind: ScreenKind::Furnace, slots: vec![f.input, f.fuel, f.output], progress: f.progress() })
                }
                _ => None,
            },
            OpenScreen::Chest(p) => match w.block_entities.get(&p) {
                Some(BlockEntity::Chest(cells)) => Some(Screen { kind: ScreenKind::Chest, slots: cells.clone(), progress: [0, 0] }),
                _ => None,
            },
        }
    }

    /// Tells the client about whatever of its own state changed.
    pub fn sync(&mut self, w: &mut ServerWorld) {
        // A station that has gone, or is out of reach, closes.
        match self.screen {
            OpenScreen::Furnace(p) | OpenScreen::Chest(p) => {
                let gone = !self.in_reach(w, p) || w.block(p).is_air();
                if gone {
                    self.close_screen(w);
                }
            }
            _ => {}
        }
        if self.inv.slots != self.sent_inv || self.inv.selected != self.sent_selected {
            self.sent_inv.clone_from(&self.inv.slots);
            self.sent_selected = self.inv.selected;
            self.peer.send(&S2C::Inventory { slots: self.inv.slots.clone(), selected: self.inv.selected });
        }
        if self.inv.cursor != self.sent_cursor {
            self.sent_cursor = self.inv.cursor;
            self.peer.send(&S2C::Cursor(self.inv.cursor));
        }
        let vitals = Vitals { health: self.health, air: self.air, mode: self.mode };
        if self.sent_vitals != Some(vitals) {
            self.sent_vitals = Some(vitals);
            self.peer.send(&S2C::Vitals(vitals));
        }
        let view = self.screen_view(w);
        if view != self.sent_screen {
            self.peer.send(&S2C::Screen(view.clone()));
            self.sent_screen = view;
        }
    }

    pub fn dig_stage(&self) -> Option<(BlockPos, u8)> {
        self.dig.map(|d| (d.pos, d.stage))
    }
}

/// What a block's state is, placed against `face` by someone looking along `yaw`. `None` if a block
/// of this shape cannot go on that face.
fn placement_meta(shape: Shape, face: Dir, yaw: u16) -> Option<u8> {
    // Which way the player faces: 0 south (+z), then west, north, east, each a quarter turn.
    let facing = ((yaw as u32 + 8192) / 16384) % 4;
    Some(match shape {
        Shape::Slab => u8::from(face == Dir::Down),
        Shape::Stairs => {
            // The high side is the far one.
            let high = match facing {
                0 => 1, // facing south
                1 => 2, // facing west
                2 => 0, // facing north
                _ => 3, // facing east
            };
            high | if face == Dir::Down { 4 } else { 0 }
        }
        Shape::Torch => match face {
            Dir::Up => 0,
            // A block's north face: the wall is to the south of the torch.
            Dir::North => 2,
            Dir::South => 1,
            Dir::West => 4,
            Dir::East => 3,
            Dir::Down => return None,
        },
        Shape::Ladder => match face {
            Dir::North => 1,
            Dir::South => 0,
            Dir::West => 3,
            Dir::East => 2,
            _ => return None,
        },
        _ => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::tests::flat_world;
    use zenith_protocol::memory_pair;

    fn player_at(x: f64, z: f64) -> (Player, Peer) {
        let (a, b) = memory_pair();
        (Player::new(1000, "ann".into(), a, Body::at(DVec3::new(x, 61.0, z)), GameMode::Survival, false), b)
    }

    fn give(p: &mut Player, w: &ServerWorld, name: &str, count: u8) {
        let item = w.reg.item_by_name(name).expect("item");
        p.inv.add(&w.reg, ItemStack::new(item, count));
    }

    fn settle(w: &mut ServerWorld) {
        w.tick += 1;
        rules::run(w);
    }

    #[test]
    fn digging_stone_by_hand_is_slow_gives_nothing_and_a_pick_does_better() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let target = BlockPos::new(1, 60, 0);
        p.start_dig(&mut w, target);
        for _ in 0..100 {
            p.tick(&mut w);
        }
        assert_eq!(w.block(target).kind(), id::STONE, "bare hands take over 7 seconds");
        for _ in 0..60 {
            p.tick(&mut w);
        }
        assert!(w.block(target).is_air());
        assert!(w.events.is_empty(), "stone needs a pick to drop anything: {:?}", w.events);

        let target = BlockPos::new(0, 60, 1);
        give(&mut p, &w, "stone_pick", 1);
        p.start_dig(&mut w, target);
        for _ in 0..12 {
            p.tick(&mut w);
        }
        assert!(w.block(target).is_air());
        assert!(w.events.iter().any(|e| matches!(e, crate::world::WorldEvent::DropItem { .. })));
        assert_eq!(p.inv.held().map(|s| s.damage), Some(1), "and the pick wears");
    }

    #[test]
    fn a_block_out_of_reach_cannot_be_dug_and_the_client_is_corrected() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let far = BlockPos::new(30, 60, 0);
        p.start_dig(&mut w, far);
        assert!(p.dig.is_none());
        assert_eq!(p.out_blocks.len(), 1);
        assert_eq!(w.block(far).kind(), id::STONE);
    }

    #[test]
    fn bedrock_cannot_be_broken_in_survival() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let b = BlockPos::new(1, 60, 0);
        w.set(b, BlockState::of(id::BEDROCK));
        p.start_dig(&mut w, b);
        for _ in 0..300 {
            p.tick(&mut w);
        }
        assert_eq!(w.block(b).kind(), id::BEDROCK);
    }

    #[test]
    fn creative_breaks_at_once_and_drops_nothing() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        p.mode = GameMode::Creative;
        let t = BlockPos::new(1, 60, 0);
        p.start_dig(&mut w, t);
        assert!(w.block(t).is_air());
        assert!(w.events.is_empty());
    }

    #[test]
    fn placing_uses_up_a_block_and_needs_room() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        give(&mut p, &w, "dirt", 2);
        let ents = Entities::default();
        // On top of the floor, one block over.
        p.use_on(&mut w, &[], &ents, BlockPos::new(2, 60, 0), Dir::Up as u8);
        assert_eq!(w.block(BlockPos::new(2, 61, 0)).kind(), id::DIRT);
        assert_eq!(p.inv.held().map(|s| s.count), Some(1));
        // Right where the player stands: refused, and nothing used up.
        p.use_on(&mut w, &[], &ents, BlockPos::new(0, 60, 0), Dir::Up as u8);
        assert!(w.block(BlockPos::new(0, 61, 0)).is_air());
        assert_eq!(p.inv.held().map(|s| s.count), Some(1));
    }

    #[test]
    fn a_torch_goes_on_a_wall_or_a_floor_but_not_a_ceiling() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        give(&mut p, &w, "torch", 3);
        let ents = Entities::default();
        let wall = BlockPos::new(3, 61, 0);
        w.set(wall, BlockState::of(id::STONE));
        p.use_on(&mut w, &[], &ents, wall, Dir::West as u8);
        let torch = w.block(wall.offset(Dir::West));
        assert_eq!(torch, BlockState::new(id::TORCH, 4));
        settle(&mut w);
        assert_eq!(w.block(wall.offset(Dir::West)).kind(), id::TORCH, "it stays up");
        p.use_on(&mut w, &[], &ents, wall, Dir::Down as u8);
        assert!(w.block(wall.offset(Dir::Down)).kind() != id::TORCH);
    }

    #[test]
    fn stairs_rise_away_from_whoever_places_them() {
        // Facing south (+z): the high side is the south one, meta 1.
        assert_eq!(placement_meta(Shape::Stairs, Dir::Up, 0), Some(1));
        assert_eq!(placement_meta(Shape::Stairs, Dir::Up, 32768), Some(0));
        assert_eq!(placement_meta(Shape::Stairs, Dir::Down, 0), Some(5), "upside down from below");
    }

    #[test]
    fn a_slab_placed_from_below_hangs_high() {
        assert_eq!(placement_meta(Shape::Slab, Dir::Down, 0), Some(1));
        assert_eq!(placement_meta(Shape::Slab, Dir::Up, 0), Some(0));
    }

    #[test]
    fn right_clicking_a_bench_opens_it_and_sneaking_builds_on_it() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        give(&mut p, &w, "dirt", 2);
        let ents = Entities::default();
        let bench = BlockPos::new(2, 61, 0);
        w.set(bench, BlockState::of(id::BENCH));
        p.use_on(&mut w, &[], &ents, bench, Dir::Up as u8);
        assert_eq!(p.screen, OpenScreen::Bench);
        assert!(w.block(bench.offset(Dir::Up)).is_air());
        p.last_input.keys = Keys(Keys::SNEAK);
        p.close_screen(&mut w);
        p.use_on(&mut w, &[], &ents, bench, Dir::Up as u8);
        assert_eq!(w.block(bench.offset(Dir::Up)).kind(), id::DIRT);
    }

    #[test]
    fn a_placed_chest_is_empty_and_a_found_one_is_not() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        give(&mut p, &w, "chest", 1);
        let ents = Entities::default();
        p.use_on(&mut w, &[], &ents, BlockPos::new(2, 60, 0), Dir::Up as u8);
        let mine = BlockPos::new(2, 61, 0);
        assert!(w.chest(mine).iter().all(Option::is_none));
        let found = BlockPos::new(0, 61, 2);
        w.set(found, BlockState::of(id::CHEST));
        assert!(w.chest(found).iter().any(Option::is_some));
    }

    #[test]
    fn closing_a_screen_returns_what_was_left_on_the_grid() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let dirt = w.reg.item_by_name("dirt").expect("dirt");
        p.craft[2] = Some(ItemStack::new(dirt, 3));
        p.inv.cursor = Some(ItemStack::new(dirt, 2));
        p.close_screen(&mut w);
        assert_eq!(p.inv.slots[0], Some(ItemStack::new(dirt, 5)));
        assert!(p.craft.iter().all(Option::is_none) && p.inv.cursor.is_none());
    }

    #[test]
    fn a_long_fall_hurts_a_short_one_does_not_and_water_saves_you() {
        let mut w = flat_world();
        let fall_from = |w: &mut ServerWorld, height: f64| -> u8 {
            let (mut p, _peer) = player_at(0.5, 0.5);
            p.body.pos.y = 61.0 + height;
            for i in 0..60 {
                p.inputs.push_back((i, MoveInput::default()));
                p.tick(w);
            }
            p.health
        };
        assert_eq!(fall_from(&mut w, 2.5), 20);
        assert!(fall_from(&mut w, 12.0) < 20);
        for x in -2..3 {
            for z in -2..3 {
                w.set(BlockPos::new(x, 61, z), BlockState::of(id::WATER));
            }
        }
        assert_eq!(fall_from(&mut w, 15.0), 20, "landing in water");
    }

    #[test]
    fn the_server_gives_no_more_than_one_physics_step_per_tick_on_average() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, -40.0);
        let run = MoveInput { keys: Keys(Keys::FORWARD | Keys::SPRINT), yaw: 0, pitch: 0 };
        // Honest for 2 seconds.
        for i in 0..40 {
            p.inputs.push_back((i, run));
            p.tick(&mut w);
        }
        let honest = p.body.pos.z;
        // A cheat floods inputs at ten times the rate for the same 2 seconds.
        let mut seq = 40;
        for _ in 0..40 {
            for _ in 0..10 {
                seq += 1;
                p.inputs.push_back((seq, run));
            }
            p.tick(&mut w);
        }
        let cheating = p.body.pos.z - honest;
        assert!(cheating < 13.0, "moved {cheating} blocks in 2 seconds");
    }

    #[test]
    fn a_silent_client_still_falls() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        p.body.pos.y = 90.0;
        for _ in 0..200 {
            p.tick(&mut w);
        }
        assert!(p.body.pos.y < 62.0, "hovering at {}", p.body.pos.y);
    }

    #[test]
    fn dying_drops_everything_and_respawning_restores_health() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        give(&mut p, &w, "dirt", 5);
        assert!(p.take_damage(&mut w, 100, None));
        assert!(p.dead);
        assert!(p.inv.slots.iter().all(Option::is_none));
        assert!(w.events.iter().any(|e| matches!(e, crate::world::WorldEvent::DropItem { .. })));
        p.respawn(DVec3::new(5.0, 61.0, 5.0));
        assert_eq!((p.dead, p.health, p.body.pos.x), (false, 20, 5.0));
    }

    #[test]
    fn hits_come_with_a_moment_of_protection() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        assert!(p.take_damage(&mut w, 1, None));
        assert!(!p.take_damage(&mut w, 1, None));
        assert_eq!(p.health, 19);
    }

    #[test]
    fn food_heals_only_when_there_is_something_to_heal() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        give(&mut p, &w, "apple", 2);
        p.use_item(&mut w);
        assert_eq!(p.inv.held().map(|s| s.count), Some(2), "full health: not eaten");
        p.health = 10;
        p.use_item(&mut w);
        assert_eq!(p.health, 14);
        assert_eq!(p.inv.held().map(|s| s.count), Some(1));
    }

    #[test]
    fn a_mob_out_of_reach_cannot_be_hit() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let mut ents = Entities::default();
        let near = ents.spawn_mob(EntityKind::Zombie, DVec3::new(2.5, 61.0, 0.5));
        let far = ents.spawn_mob(EntityKind::Zombie, DVec3::new(30.5, 61.0, 0.5));
        p.attack(&mut w, &mut ents, &[], far);
        assert_eq!(ents.list[&far].health, 20);
        p.attack_cool = 0;
        p.attack(&mut w, &mut ents, &[], near);
        assert!(ents.list[&near].health < 20);
    }

    #[test]
    fn creative_players_cannot_be_hurt_and_mobs_ignore_them() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        p.mode = GameMode::Creative;
        assert!(!p.take_damage(&mut w, 100, None));
        assert!(!p.view().vulnerable);
    }

    #[test]
    fn only_what_changed_is_sent() {
        let mut w = flat_world();
        let (mut p, peer) = player_at(0.5, 0.5);
        p.sync(&mut w);
        let first = std::iter::from_fn(|| peer.try_recv_frame()).count();
        assert!(first >= 2, "inventory and vitals on the first sync");
        p.sync(&mut w);
        assert_eq!(std::iter::from_fn(|| peer.try_recv_frame()).count(), 0);
        give(&mut p, &w, "dirt", 1);
        p.sync(&mut w);
        assert_eq!(std::iter::from_fn(|| peer.try_recv_frame()).count(), 1);
    }
}
