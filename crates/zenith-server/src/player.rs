//! A connected player, and what they can do. Every player is one of these: the host of a world is
//! no different from a stranger who joined it, and no code ever pretends to be anyone else.
use crate::entity::{self, Damage, Data, Entities, PlayerView, Sent, hurt, yaw_towards};
use crate::inventory::{self, Holder, Inventory, Slot};
use crate::persist::{BlockEntity, PlayerSave};
use crate::rules;
use crate::world::ServerWorld;
use std::collections::VecDeque;
use zenith_content::{BlockState, ItemKind, ItemStack, Registry, Shape, Tool, id, it};
use zenith_core::glam::DVec3;
use zenith_core::pos::MIN_Y;
use zenith_core::{Aabb, BlockPos, ChunkPos, Dir, FxSet};
use zenith_protocol::{
    BlockChange, ChunkDelta, ClickButton, EffectKind, EntityInfo, EntityKind, GameMode, Peer, S2C, Screen, ScreenKind,
    Vitals,
};
use zenith_world::BlockAccess;
use zenith_world::physics::{self, Body, Keys, MoveInput};

/// How long a campfire takes to cook something: twenty seconds, twice a furnace's time.
pub const CAMPFIRE_TICKS: u16 = 400;
/// How far a player can reach, from the eye to the middle of a block, with some forgiveness.
const REACH: f64 = 6.5;
const ATTACK_REACH: f64 = 5.0;
/// A client may run ahead by this many ticks, to ride out a network hiccup. No more: over time it
/// gets one physics step per server tick, so speed hacks do not exist.
const MAX_CREDITS: u8 = 4;
/// How long eating takes, holding the food to the mouth.
pub const EAT_TICKS: u8 = 32;
/// Every this much effort, a point comes off saturation, or once that is gone, off the food bar.
const EXHAUSTED: f32 = 4.0;
/// At or under this much food there is no strength left to sprint.
pub const TOO_HUNGRY_TO_SPRINT: u8 = 6;

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

/// Food on its way down: which slot, what it was, how long to go. The client keeps asking while
/// the button is held; when it stops, so does eating.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Eating {
    slot: u8,
    item: u16,
    left: u8,
    held_until: u64,
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
    /// The food bar, 0-20, and the hidden store that empties before it does.
    pub food: u8,
    pub saturation: f32,
    /// Effort spent since the last point came off.
    pub exhaustion: f32,
    pub eating: Option<Eating>,
    /// The foot of the bed this player is asleep in, and for how many ticks.
    pub sleeping: Option<BlockPos>,
    pub sleep_ticks: u16,
    /// The foot of the bed this player comes back to after dying.
    pub bed: Option<BlockPos>,
    /// Experience earned, in points: see `level_of`.
    pub xp: u32,
    /// Ticks before the fishing rod answers again: a cast and a pull are two pulls of the arm.
    pub fish_cool: u8,
    /// Holding a shield up.
    pub guarding: bool,
    /// Ticks since the bow in hand last let an arrow go, or came to hand: as long as it can have
    /// been drawn, whatever the client says.
    pub bow_ticks: u8,
    /// The boat or cart this player sits in.
    pub riding: Option<u32>,
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
    sent_armor: [Slot; 4],
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
            food: 20,
            saturation: 5.0,
            exhaustion: 0.0,
            eating: None,
            sleeping: None,
            sleep_ticks: 0,
            bed: None,
            xp: 0,
            fish_cool: 0,
            guarding: false,
            bow_ticks: 0,
            riding: None,
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
            sent_armor: [None; 4],
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
        p.food = save.food.min(20);
        p.saturation = save.saturation.clamp(0.0, 20.0);
        p.bed = save.bed;
        p.xp = save.xp;
        p
    }

    pub fn save(&self, reg: &Registry) -> PlayerSave {
        PlayerSave {
            pos: self.body.pos,
            health: self.health,
            air: self.air,
            mode: self.mode,
            flying: self.body.flying,
            op: self.op,
            inventory: self.inv.clone(),
            food: self.food,
            saturation: self.saturation,
            bed: self.bed,
            xp: self.xp,
            items: reg.items().iter().map(|i| i.name.clone()).collect(),
        }
    }

    pub fn view(&self) -> PlayerView {
        PlayerView {
            id: self.id,
            pos: self.body.pos,
            vulnerable: !self.dead && self.mode == GameMode::Survival,
            holding: self.inv.held().map(|s| s.item),
        }
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
            baby: false,
            owner: 0,
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
        self.bow_ticks = self.bow_ticks.saturating_add(1);
        self.fish_cool = self.fish_cool.saturating_sub(1);
        // A guard needs a shield to hold up: one that broke, or left the hand, lowers it.
        if self.guarding && !self.holds_shield(&w.reg) {
            self.guarding = false;
        }
        // Asleep, a body lies still: what the client sends is taken and ignored.
        if self.sleeping.is_some() {
            self.sleep_ticks = self.sleep_ticks.saturating_add(1);
            while let Some((seq, _)) = self.inputs.pop_front() {
                self.ack = seq;
            }
            self.body.vel = DVec3::ZERO;
            return;
        }
        // Riding, a body goes where its boat or cart takes it: the inputs only steer, and Sneak gets off.
        if self.riding.is_some() {
            while let Some((seq, input)) = self.inputs.pop_front() {
                self.ack = seq;
                self.last_input = input;
            }
            if self.last_input.keys.has(Keys::SNEAK) {
                self.get_off(w);
            }
            // Water and lava are where the seat is now, not where it was when the ride began.
            let reg = w.reg.clone();
            physics::sense(&*w, &reg, &mut self.body, physics::PLAYER_HEIGHT);
            if self.mode == GameMode::Survival {
                self.hazards(w);
                self.hunger(w);
            }
            self.tick_eating(w);
            return;
        }
        let reg = w.reg.clone();
        let mut landed = None;
        let mut any = false;
        let was_in_water = self.body.in_water;
        let survival = self.mode == GameMode::Survival;
        while self.credits > 0 {
            let Some((seq, mut input)) = self.inputs.pop_front() else { break };
            self.credits -= 1;
            self.ack = seq;
            if survival && self.food <= TOO_HUNGRY_TO_SPRINT {
                input.keys.0 &= !Keys::SPRINT;
            }
            // Behind a shield, a body crouches and cannot run.
            if self.guarding {
                input.keys.0 = (input.keys.0 | Keys::SNEAK) & !Keys::SPRINT;
            }
            self.last_input = input;
            any = true;
            let jumped = self.body.on_ground && input.keys.has(Keys::JUMP) && !self.body.flying;
            let result = physics::step(&mut self.body, input, &*w, &reg, &mut self.boxes);
            landed = landed.or(result.landed_after);
            // What moving costs: sprinting by the block, and each jump, more at a sprint.
            if survival {
                let sprinting = input.keys.has(Keys::SPRINT) && input.keys.0 & Keys::FORWARD != 0;
                if sprinting {
                    self.exhaustion += 0.1 * result.moved as f32;
                }
                if jumped {
                    self.exhaustion += if sprinting { 0.2 } else { 0.05 };
                }
            }
        }
        // A silent client still falls, once it has used up its grace.
        if !any && self.credits == MAX_CREDITS {
            let idle = MoveInput { keys: Keys(0), ..self.last_input };
            let result = physics::step(&mut self.body, idle, &*w, &reg, &mut self.boxes);
            landed = landed.or(result.landed_after);
        }
        if self.body.in_water && !was_in_water {
            w.effect(EffectKind::Splash, self.body.pos);
        }
        if self.mode == GameMode::Survival {
            if self.body.flying {
                self.body.flying = false;
            }
            if let Some(distance) = landed
                && distance > 3.5
                && !self.body.in_water
            {
                self.take_damage(w, (distance - 3.0) as i16, None);
            }
            self.hazards(w);
            self.hunger(w);
        }
        self.tick_dig(w);
        self.tick_eating(w);
    }

    /// The food bar: effort empties it, and it decides whether health comes back or goes.
    fn hunger(&mut self, w: &mut ServerWorld) {
        if self.dead {
            return;
        }
        while self.exhaustion >= EXHAUSTED {
            self.exhaustion -= EXHAUSTED;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else {
                self.food = self.food.saturating_sub(1);
            }
        }
        if self.food >= 18 && self.health < 20 {
            self.regen += 1;
            if self.regen >= 80 {
                self.regen = 0;
                self.health += 1;
                self.exhaustion += 3.0;
            }
        } else if self.food == 0 {
            // Starving hurts, but leaves half a heart: hunger alone never kills.
            self.regen += 1;
            if self.regen >= 80 {
                self.regen = 0;
                if self.health > 1 {
                    self.take_damage(w, 1, None);
                }
            }
        } else {
            self.regen = 0;
        }
    }

    fn tick_eating(&mut self, w: &mut ServerWorld) {
        let Some(mut e) = self.eating else { return };
        let still = w.tick <= e.held_until
            && self.inv.selected == e.slot
            && self.inv.held().map(|s| s.item) == Some(e.item)
            && !self.dead;
        if !still {
            self.eating = None;
            return;
        }
        e.left = e.left.saturating_sub(1);
        if e.left.is_multiple_of(4) && e.left > 0 {
            w.effect(EffectKind::Eat, self.eye());
        }
        if e.left > 0 {
            self.eating = Some(e);
            return;
        }
        self.eating = None;
        let heal = match w.reg.item(e.item).map(|i| i.kind) {
            Some(ItemKind::Food { heal }) => heal,
            _ => return,
        };
        self.food = (self.food + heal).min(20);
        self.saturation = (self.saturation + heal as f32 * 0.6).min(self.food as f32);
        self.inv.consume_held(1);
        w.effect(EffectKind::Eat, self.eye());
    }

    fn hazards(&mut self, w: &mut ServerWorld) {
        if self.dead {
            return;
        }
        let reg = w.reg.clone();
        if self.body.in_lava && w.tick.is_multiple_of(10) {
            self.take_damage(w, 4, None);
        }
        if self.body.pos.y < MIN_Y as f64 - 40.0 && w.tick.is_multiple_of(10) {
            self.take_damage(w, 4, None);
        }
        // What the body is touching.
        if w.tick.is_multiple_of(10) {
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
            if w.tick.is_multiple_of(15) {
                self.air = self.air.saturating_sub(1);
            }
            if self.air == 0 && w.tick.is_multiple_of(20) {
                self.take_damage(w, 2, None);
            }
        } else if self.air < 20 && w.tick.is_multiple_of(2) {
            self.air += 1;
        }
    }

    /// Takes a hit. Survival only, and with a moment of protection after each hit. Armour softens a
    /// blow that knocks back, from a creature, a player or a blast, and wears for it; it does nothing
    /// against falling, drowning, burning or hunger.
    pub fn take_damage(&mut self, w: &mut ServerWorld, amount: i16, knock: Option<DVec3>) -> bool {
        if self.dead || self.mode != GameMode::Survival || amount <= 0 || self.hurt_cool > 0 {
            return false;
        }
        // A raised shield takes a blow from in front: one that pushes back the way the player faces.
        if self.guarding
            && self.holds_shield(&w.reg)
            && let Some(k) = knock
        {
            let look = self.last_input.look();
            let facing = DVec3::new(look.x, 0.0, look.z).normalize_or_zero();
            if facing.dot(DVec3::new(k.x, 0.0, k.z).normalize_or_zero()) < -0.3 {
                self.inv.damage_held(&w.reg.clone());
                self.hurt_cool = 10;
                w.effect(EffectKind::Blocked, self.body.pos);
                return false;
            }
        }
        let amount = if knock.is_some() && amount < i16::MAX {
            let reg = w.reg.clone();
            let points = self.inv.armor_points(&reg).min(20);
            self.inv.wear_armor(&reg);
            ((amount as f32 * (1.0 - points as f32 * 0.04)).round() as i16).max(1)
        } else {
            amount
        };
        self.hurt_cool = 10;
        self.regen = 0;
        self.exhaustion += 0.1;
        self.sleeping = None;
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
        self.eating = None;
        self.sleeping = None;
        self.guarding = false;
        self.riding = None;
        // Dying costs half of what was learned.
        self.xp /= 2;
        w.effect(EffectKind::Death, self.body.pos);
        self.close_screen(w);
        for stack in self.inv.slots.iter_mut().chain(self.inv.armor.iter_mut()).filter_map(Option::take) {
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
        self.food = 20;
        self.saturation = 5.0;
        self.exhaustion = 0.0;
        self.body = Body::at(spawn);
        self.inputs.clear();
    }

    /// Whether the held item is a tool of this kind.
    fn holding(&self, reg: &Registry, kind: Tool) -> bool {
        matches!(
            self.inv.held().and_then(|s| reg.item(s.item)).map(|i| i.kind),
            Some(ItemKind::Tool { tool, .. }) if tool == kind
        )
    }

    /// Whether a block position is in reach, and the chunk around it is here.
    fn in_reach(&self, w: &ServerWorld, pos: BlockPos) -> bool {
        pos.in_world() && w.is_loaded(pos) && self.eye().distance(pos.center()) <= REACH
    }

    fn held_tool(&self, reg: &Registry) -> (Tool, u8, u8, u8) {
        if let Some(stack) = self.inv.held()
            && let Some(ItemKind::Tool { tool, tier, speed, damage, .. }) = reg.item(stack.item).map(|i| i.kind)
        {
            return (tool, tier, speed, damage);
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
        self.exhaustion += 0.005;
        if reg.can_harvest(state, self.inv.held().map(|s| s.item)) {
            rules::drop_block(w, pos, state);
            // Ore that gives up its riches teaches something too.
            let xp = match state.kind() {
                id::COAL_ORE => 1,
                id::EMBER_ORE => w.rng.range(2, 5) as u32,
                id::CRYSTAL_ORE => w.rng.range(3, 7) as u32,
                _ => 0,
            };
            if xp > 0 && self.mode == GameMode::Survival {
                self.add_xp(w, xp);
            }
            if state.kind() == id::OAK_LEAVES
                && w.rng.chance(0.06)
                && let Some(apple) = reg.item_by_name("apple")
            {
                w.events
                    .push(crate::world::WorldEvent::DropItem { pos: pos.center(), stack: ItemStack::new(apple, 1) });
            }
        }
        // The right tool wears, and so does any tool used for the wrong job.
        let holding_a_tool = matches!(
            reg.item(self.inv.held().map_or(u16::MAX, |s| s.item)).map(|i| i.kind),
            Some(ItemKind::Tool { .. })
        );
        if (def.tool == tool && tool != Tool::Any) || holding_a_tool {
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
        // A hoe tills grass or dirt with open air above.
        let reg = w.reg.clone();
        // A cart goes onto a rail.
        if crate::rails::is_rail(target.kind())
            && self.inv.held().is_some_and(|s| Some(s.item) == reg.item_by_name(it::CART))
        {
            if self.mode == GameMode::Survival {
                self.inv.consume_held(1);
            }
            w.events.push(crate::world::WorldEvent::Vehicle { kind: EntityKind::Cart, pos: pos.center() });
            return;
        }
        if self.holding(&reg, Tool::Hoe)
            && matches!(target.kind(), id::GRASS | id::DIRT)
            && face != Dir::Down
            && w.block(pos.offset(Dir::Up)).is_air()
        {
            let soil = BlockState::of(id::FARMLAND);
            w.set(pos, soil);
            w.effect(EffectKind::BlockPlaced(soil), pos.center());
            if self.mode == GameMode::Survival {
                self.inv.damage_held(&reg);
            }
            return;
        }
        // A full bucket pours out against whatever it is used on, unless that opens or works.
        let pours = match self.inv.held().and_then(|s| reg.item(s.item)).map(|i| i.kind) {
            Some(ItemKind::Bucket { holds }) if holds != 0 => Some(holds),
            _ => None,
        };
        if let Some(fluid) = pours
            && (sneaking || !works_when_used(target.kind()))
        {
            self.pour_bucket(w, pos, target, face, fluid);
            return;
        }
        // A tinderbox lights a fire against whatever it is used on.
        let lights =
            matches!(self.inv.held().and_then(|s| reg.item(s.item)).map(|i| i.kind), Some(ItemKind::Lighter { .. }));
        if lights && (sneaking || !works_when_used(target.kind())) {
            self.light_fire(w, pos, face);
            return;
        }
        if !sneaking {
            match target.kind() {
                // Food laid on a campfire cooks there.
                id::CAMPFIRE if self.cook_on_campfire(w, pos) => return,
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
                id::DOOR | id::TRAPDOOR | id::GATE => {
                    rules::swing(w, pos);
                    return;
                }
                // A sign can be written on again.
                id::SIGN => {
                    self.peer.send(&S2C::EditSign(pos));
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

    fn place(
        &mut self,
        w: &mut ServerWorld,
        others: &[Aabb],
        ents: &Entities,
        clicked: BlockPos,
        clicked_state: BlockState,
        face: Dir,
    ) {
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
        let meta = if reg.block(BlockState::of(kind)).tint == zenith_content::Tint::Foliage
            && reg.block(BlockState::of(kind)).shape == Shape::Cube
        {
            1
        } else {
            meta
        };
        // A campfire sits on the ground, whichever face it was put against.
        let meta = if kind == id::CAMPFIRE { 0 } else { meta };
        let state = BlockState::new(kind, meta);
        if reg.block(state).fragile && !rules::has_support(w, cell, state) {
            reject(self, w);
            return;
        }
        let shape = reg.block(state).shape;
        // A bed takes two cells, its head the far one, and a door two, one above the other: both need
        // room, and a bed floor under both.
        let head = match kind {
            id::BED => Some(cell.offset(bed_toward(meta))),
            id::DOOR => Some(cell.offset(Dir::Up)),
            _ => None,
        };
        if let Some(head) = head {
            let floor = |p: BlockPos| reg.lut.collides[w.block(p.offset(Dir::Down)).kind() as usize];
            let footing = if kind == id::BED { floor(cell) && floor(head) } else { floor(cell) };
            if !self.in_reach(w, head) || !reg.block(w.block(head)).replaceable || !footing {
                reject(self, w);
                self.resend_block(w, head);
                return;
            }
        }
        for at in std::iter::once(cell).chain(head) {
            let base = DVec3::new(at.x as f64, at.y as f64, at.z as f64);
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
        }
        if self.mode == GameMode::Survival {
            self.inv.consume_held(1);
        }
        w.set(cell, state);
        if let Some(head) = head {
            let other = if kind == id::DOOR {
                BlockState::new(id::DOOR, meta | 8)
            } else {
                BlockState::new(id::BED_HEAD, meta)
            };
            w.set(head, other);
        }
        match kind {
            // Placed by a player, so it holds what they put in it and nothing else.
            id::CHEST => {
                w.block_entities.insert(cell, BlockEntity::Chest(vec![None; inventory::CHEST_SIZE]));
            }
            id::FURNACE => {
                w.block_entities.insert(cell, BlockEntity::Furnace(inventory::Furnace::default()));
            }
            id::FENCE => {
                let joined = rules::fence_joins(w, cell);
                w.set(cell, BlockState::new(id::FENCE, joined));
            }
            id::RAIL | id::BOOST_RAIL => {
                let shape = crate::rails::shape_for(&*w, cell, 0);
                w.set(cell, BlockState::new(kind, shape));
            }
            // A new sign is blank, and its writer is asked what it says.
            id::SIGN => {
                w.block_entities.insert(cell, BlockEntity::Sign(Vec::new()));
                w.signs_changed.push(cell);
                self.peer.send(&S2C::EditSign(cell));
            }
            _ => {}
        }
        w.effect(EffectKind::BlockPlaced(state), cell.center());
    }

    /// Eats what is in hand, if it is food and there is room for it. Eating takes a moment, and the
    /// client asks again every few ticks while the button is held: the first ask starts it, the rest
    /// keep it going.
    pub fn use_item(&mut self, w: &mut ServerWorld) {
        if self.dead || self.sleeping.is_some() {
            return;
        }
        let Some(stack) = self.inv.held() else { return };
        if Some(stack.item) == w.reg.item_by_name(it::BOAT) {
            self.launch_boat(w);
            return;
        }
        let kind = w.reg.item(stack.item).map(|i| i.kind);
        if kind == Some(ItemKind::Bucket { holds: 0 }) {
            self.fill_bucket(w);
            return;
        }
        let Some(ItemKind::Food { .. }) = kind else { return };
        if self.food >= 20 {
            return;
        }
        let held_until = w.tick + 6;
        match &mut self.eating {
            Some(e) if e.item == stack.item && e.slot == self.inv.selected => e.held_until = held_until,
            _ => self.eating = Some(Eating { slot: self.inv.selected, item: stack.item, left: EAT_TICKS, held_until }),
        }
    }

    /// Swings at an entity. Returns damage to deal to another player, if that is who was hit.
    pub fn attack(
        &mut self,
        w: &mut ServerWorld,
        ents: &mut Entities,
        others: &[PlayerView],
        target: u32,
    ) -> Option<Damage> {
        if self.dead || self.attack_cool > 0 || target == self.id {
            return None;
        }
        self.attack_cool = 8;
        let reg = w.reg.clone();
        let (tool, _, _, damage) = self.held_tool(&reg);
        let eye = self.eye();
        let amount = damage as i16;
        self.exhaustion += 0.1;
        if let Some(e) = ents.list.get_mut(&target) {
            let close = e.aabb().center().distance(eye) <= ATTACK_REACH + e.kind.size().0;
            if !close {
                return None;
            }
            // Knocked about enough, a boat or a cart comes apart into what it was made as.
            if e.kind.is_vehicle() {
                e.health -= amount.max(2);
                w.effect(EffectKind::Hurt, e.body.pos);
                if e.health > 0 {
                    return None;
                }
                let (kind, at) = (e.kind, e.body.pos + DVec3::Y * 0.4);
                ents.list.remove(&target);
                let name = if kind == EntityKind::Boat { it::BOAT } else { it::CART };
                if self.mode == GameMode::Survival
                    && let Some(item) = reg.item_by_name(name)
                {
                    w.events.push(crate::world::WorldEvent::DropItem { pos: at, stack: ItemStack::new(item, 1) });
                }
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
                if let Data::Mob(brain) = &mut e.data {
                    brain.hit_by = Some(self.id);
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
        self.guarding = false;
        self.bow_ticks = 0;
    }

    /// Climbs out of a boat or cart: up out of it, or else off to a side with room, never into a wall.
    pub fn get_off(&mut self, w: &ServerWorld) {
        self.riding = None;
        self.body.vel = DVec3::ZERO;
        self.body.fall = 0.0;
        let here = self.body.pos;
        let reg = w.reg.clone();
        let spots = [
            DVec3::Y * 1.15,
            DVec3::new(1.0, 0.65, 0.0),
            DVec3::new(-1.0, 0.65, 0.0),
            DVec3::new(0.0, 0.65, 1.0),
            DVec3::new(0.0, 0.65, -1.0),
        ];
        let width = physics::PLAYER_WIDTH;
        let spot = spots
            .into_iter()
            .find(|&d| physics::fits(w, &reg, here + d, width, physics::PLAYER_HEIGHT, &mut self.boxes));
        self.body.pos = here + spot.unwrap_or(DVec3::Y * 0.45);
    }

    pub fn holds_rod(&self, reg: &Registry) -> bool {
        self.inv.held().and_then(|s| reg.item(s.item)).is_some_and(|i| matches!(i.kind, ItemKind::Rod { .. }))
    }

    fn holds_shield(&self, reg: &Registry) -> bool {
        self.inv.held().and_then(|s| reg.item(s.item)).is_some_and(|i| matches!(i.kind, ItemKind::Shield { .. }))
    }

    /// Uses what is in hand on a creature: an animal takes its food from a hand.
    pub fn use_entity(&mut self, w: &mut ServerWorld, ents: &mut Entities, target: u32) {
        if self.dead || self.sleeping.is_some() {
            return;
        }
        let Some(stack) = self.inv.held() else { return };
        let eye = self.eye();
        let Some(e) = ents.list.get_mut(&target) else { return };
        if e.aabb().center().distance(eye) > ATTACK_REACH + e.kind.size().0 {
            return;
        }
        if entity::feed(e, &w.reg, stack.item) {
            let at = e.body.pos + DVec3::Y * 0.8;
            if self.mode == GameMode::Survival {
                self.inv.consume_held(1);
            }
            w.effect(EffectKind::Love, at);
        }
    }

    /// Lets an arrow go from the bow in hand, drawn for `drawn` ticks: the longer, up to a second,
    /// the faster and harder it flies. It takes an arrow, unless in creative, and wears the bow.
    /// Returns where the arrow starts, how it moves and how hard it hits.
    pub fn shoot(&mut self, w: &mut ServerWorld, drawn: u8) -> Option<(DVec3, DVec3, i16)> {
        // No longer than the server has seen pass since the last arrow, give or take a message on
        // the way: so arrows cannot come faster, or harder, than drawing allows.
        let drawn = drawn.min(self.bow_ticks.saturating_add(2));
        if self.dead || self.sleeping.is_some() || drawn < 3 {
            return None;
        }
        let reg = w.reg.clone();
        let bow = self.inv.held()?;
        if !matches!(reg.item(bow.item).map(|i| i.kind), Some(ItemKind::Bow { .. })) {
            return None;
        }
        if self.mode == GameMode::Survival {
            let arrow = reg.item_by_name(it::ARROW)?;
            if !self.inv.take(arrow) {
                return None;
            }
            self.inv.damage_held(&reg);
        }
        self.bow_ticks = 0;
        let power = f64::from(drawn.min(20)) / 20.0;
        let dir = self.last_input.look();
        let hit = (2.0 + power * 7.0).round() as i16;
        Some((self.eye() + dir * 0.4, dir * (0.5 + power * 2.5), hit))
    }

    /// Scoops up the spring of water or lava the empty bucket in hand points at, within reach: the
    /// first one along the look, through any flow, short of anything solid.
    fn fill_bucket(&mut self, w: &mut ServerWorld) {
        let reg = w.reg.clone();
        let (eye, look) = (self.eye(), self.last_input.look());
        let mut spring = None;
        for i in 1..=50 {
            let cell = BlockPos::from_vec(eye + look * (f64::from(i) * 0.1));
            let state = w.block(cell);
            if reg.block(state).fluid != zenith_content::Fluid::None {
                if state.meta() == 0 {
                    spring = Some((cell, state.kind()));
                    break;
                }
            } else if reg.lut.collides[state.kind() as usize] {
                break;
            }
        }
        let Some((cell, fluid)) = spring else { return };
        let Some(full) = reg.items().iter().position(|i| i.kind == ItemKind::Bucket { holds: fluid }) else { return };
        w.set(cell, BlockState::AIR);
        w.effect(EffectKind::Splash, cell.center());
        if self.mode == GameMode::Survival {
            self.turn_held_into(w, full as u16);
        }
    }

    /// Pours the full bucket in hand out against a block: a spring of what it held, in the cell on
    /// that side.
    fn pour_bucket(
        &mut self,
        w: &mut ServerWorld,
        clicked: BlockPos,
        clicked_state: BlockState,
        face: Dir,
        fluid: u16,
    ) {
        let reg = w.reg.clone();
        let cell = if reg.block(clicked_state).replaceable { clicked } else { clicked.offset(face) };
        let there = w.block(cell);
        if !self.in_reach(w, cell) || !reg.block(there).replaceable || there == BlockState::of(fluid) {
            self.resend_block(w, cell);
            return;
        }
        rules::flow_into(w, cell, BlockState::of(fluid));
        w.effect(if fluid == id::LAVA { EffectKind::Fizz } else { EffectKind::Splash }, cell.center());
        if self.mode == GameMode::Survival
            && let Some(empty) = reg.item_by_name(it::BUCKET)
        {
            self.turn_held_into(w, empty);
        }
    }

    /// Lights a fire in the cell against a face of a block, if there is something under it to burn
    /// on or something beside it to burn.
    fn light_fire(&mut self, w: &mut ServerWorld, clicked: BlockPos, face: Dir) {
        let reg = w.reg.clone();
        let cell = clicked.offset(face);
        let there = w.block(cell);
        let empty =
            there.is_air() || (reg.block(there).replaceable && reg.block(there).fluid == zenith_content::Fluid::None);
        let holds = reg.lut.collides[w.block(cell.offset(Dir::Down)).kind() as usize]
            || Dir::ALL.iter().any(|&d| reg.block(w.block(cell.offset(d))).flammable > 0);
        if !self.in_reach(w, cell) || !empty || !holds || there.kind() == id::FIRE {
            return;
        }
        w.set(cell, BlockState::of(id::FIRE));
        w.effect(EffectKind::Fizz, cell.center());
        if self.mode == GameMode::Survival {
            self.inv.damage_held(&reg);
        }
    }

    /// Lays the food in hand on a campfire to cook, if it is something that cooks and there is room:
    /// four things at a time.
    fn cook_on_campfire(&mut self, w: &mut ServerWorld, pos: BlockPos) -> bool {
        let reg = w.reg.clone();
        let Some(stack) = self.inv.held() else { return false };
        let cooks = reg
            .smelt_result(stack.item)
            .and_then(|out| reg.item(out))
            .is_some_and(|out| matches!(out.kind, ItemKind::Food { .. }));
        if !cooks {
            return false;
        }
        let be = w.block_entities.entry(pos).or_insert_with(|| BlockEntity::Campfire(Vec::new()));
        let BlockEntity::Campfire(cooking) = be else { return false };
        if cooking.len() >= 4 {
            return true;
        }
        cooking.push((stack.item, CAMPFIRE_TICKS));
        w.mark_dirty(pos.chunk());
        w.effect(EffectKind::Fizz, pos.center());
        if self.mode == GameMode::Survival {
            self.inv.consume_held(1);
        }
        true
    }

    /// Turns one of what is in hand into something else: where it was if it was the last of its
    /// stack, or else wherever there is room, or on the ground.
    fn turn_held_into(&mut self, w: &mut ServerWorld, item: u16) {
        let reg = w.reg.clone();
        let Some(held) = self.inv.held() else { return };
        let made = ItemStack::new(item, 1);
        if held.count <= 1 {
            self.inv.slots[self.inv.selected as usize % 9] = Some(made);
        } else {
            self.inv.consume_held(1);
            if let Some(left) = self.inv.add(&reg, made) {
                w.events.push(crate::world::WorldEvent::DropItem { pos: self.eye(), stack: left });
            }
        }
    }

    /// Puts the boat in hand down on the water looked at, or on the ground, within reach.
    fn launch_boat(&mut self, w: &mut ServerWorld) {
        if self.riding.is_some() {
            return;
        }
        let (eye, look) = (self.eye(), self.last_input.look());
        let reg = w.reg.clone();
        let (bw, bh) = EntityKind::Boat.size();
        let mut last = BlockPos::from_vec(eye);
        for i in 1..=50 {
            let p = eye + look * (f64::from(i) * 0.1);
            let cell = BlockPos::from_vec(p);
            let state = w.block(cell);
            let def = reg.block(state);
            let top = if def.fluid == zenith_content::Fluid::Water {
                cell.y as f64 + 0.8
            } else if reg.lut.collides[state.kind() as usize] {
                // Only on top of what was looked at, not into its side.
                if last != cell.offset(Dir::Up) {
                    return;
                }
                cell.y as f64 + 1.0
            } else {
                last = cell;
                continue;
            };
            // Where it was looked at, or else in the middle of the block, if there is room for it.
            let at = [DVec3::new(p.x, top, p.z), DVec3::new(cell.x as f64 + 0.5, top, cell.z as f64 + 0.5)]
                .into_iter()
                .find(|&at| physics::fits(&*w, &reg, at, bw, bh, &mut self.boxes));
            if let Some(at) = at {
                if self.mode == GameMode::Survival {
                    self.inv.consume_held(1);
                }
                w.events.push(crate::world::WorldEvent::Vehicle { kind: EntityKind::Boat, pos: at });
            }
            return;
        }
    }

    /// Raises or lowers the shield, if there is one in hand.
    pub fn guard(&mut self, on: bool, reg: &Registry) {
        self.guarding = on && self.holds_shield(reg) && !self.dead;
    }

    /// Adds experience, with a fanfare for each new level.
    pub fn add_xp(&mut self, w: &mut ServerWorld, amount: u32) {
        let before = level_of(self.xp).0;
        self.xp = self.xp.saturating_add(amount);
        if level_of(self.xp).0 > before {
            w.effect(EffectKind::LevelUp, self.body.pos);
        }
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
        let back: Vec<ItemStack> =
            self.craft.iter_mut().filter_map(Option::take).chain(self.inv.cursor.take()).collect();
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
                Some(BlockEntity::Furnace(f)) => Some(Screen {
                    kind: ScreenKind::Furnace,
                    slots: vec![f.input, f.fuel, f.output],
                    progress: f.progress(),
                }),
                _ => None,
            },
            OpenScreen::Chest(p) => match w.block_entities.get(&p) {
                Some(BlockEntity::Chest(cells)) => {
                    Some(Screen { kind: ScreenKind::Chest, slots: cells.clone(), progress: [0, 0] })
                }
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
        if self.inv.slots != self.sent_inv
            || self.inv.selected != self.sent_selected
            || self.inv.armor != self.sent_armor
        {
            self.sent_inv.clone_from(&self.inv.slots);
            self.sent_selected = self.inv.selected;
            self.sent_armor = self.inv.armor;
            self.peer.send(&S2C::Inventory {
                slots: self.inv.slots.clone(),
                selected: self.inv.selected,
                armor: self.inv.armor,
            });
        }
        if self.inv.cursor != self.sent_cursor {
            self.sent_cursor = self.inv.cursor;
            self.peer.send(&S2C::Cursor(self.inv.cursor));
        }
        let (level, progress) = level_of(self.xp);
        let vitals = Vitals {
            health: self.health,
            air: self.air,
            mode: self.mode,
            food: self.food,
            armor: self.inv.armor_points(&w.reg).min(20),
            sleeping: self.sleeping.is_some(),
            level,
            xp: (progress * 255.0) as u8,
            riding: self.riding,
        };
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

    /// Takes what a player wrote on a sign: no more lines or letters than a sign holds, nothing that
    /// is not plain text, and only on a sign within reach.
    pub fn write_sign(&mut self, w: &mut ServerWorld, pos: BlockPos, lines: Vec<String>) -> bool {
        if self.dead || !self.in_reach(w, pos) || w.block(pos).kind() != id::SIGN {
            return false;
        }
        let lines: Vec<String> = lines
            .into_iter()
            .take(zenith_protocol::SIGN_LINES)
            .map(|l| l.chars().filter(|c| !c.is_control()).take(zenith_protocol::SIGN_LINE_LENGTH).collect())
            .collect();
        w.block_entities.insert(pos, BlockEntity::Sign(lines));
        w.signs_changed.push(pos);
        w.mark_dirty(pos.chunk());
        true
    }

    pub fn dig_stage(&self) -> Option<(BlockPos, u8)> {
        self.dig.map(|d| (d.pos, d.stage))
    }
}

/// Whether using something on this block does a thing of the block's own: opens it, flips it, sets
/// it off.
fn works_when_used(kind: u16) -> bool {
    matches!(
        kind,
        id::BENCH | id::FURNACE | id::CHEST | id::LEVER | id::TNT | id::DOOR | id::TRAPDOOR | id::GATE | id::SIGN
    )
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
        // A door closes the near side of its cell, a trapdoor hinges there, a gate faces that way,
        // and a sign on a post faces whoever put it there.
        Shape::Door | Shape::Gate => near_side(facing),
        Shape::Trapdoor => near_side(facing) | if face == Dir::Down { 8 } else { 0 },
        Shape::Sign => match face {
            Dir::Up => near_side(facing),
            Dir::Down => return None,
            // On a wall, its words face out from it.
            Dir::North => 4,
            Dir::South => 5,
            Dir::West => 6,
            Dir::East => 7,
        },
        // A bed's head goes away from whoever places it.
        Shape::Bed => match facing {
            0 => 1, // facing south
            1 => 2, // facing west
            2 => 0, // facing north
            _ => 3, // facing east
        },
        _ => 0,
    })
}

/// The level reached with this much experience, and how far through the next one, 0 to 1. Each
/// level asks two points more than the one before: seven for the first.
pub fn level_of(xp: u32) -> (u16, f32) {
    let (mut level, mut left) = (0u32, xp);
    loop {
        let need = 7 + 2 * level;
        if left < need {
            return (level.min(u32::from(u16::MAX)) as u16, left as f32 / need as f32);
        }
        left -= need;
        level += 1;
    }
}

/// The side of a cell nearest someone facing this way (0 south, then west, north, east), as a meta:
/// 0 north, 1 south, 2 west, 3 east.
fn near_side(facing: u32) -> u8 {
    match facing {
        0 => 0,
        1 => 3,
        2 => 1,
        _ => 2,
    }
}

/// The way from a bed's foot to its head, by its meta.
pub fn bed_toward(meta: u8) -> Dir {
    match meta & 3 {
        0 => Dir::North,
        1 => Dir::South,
        2 => Dir::West,
        _ => Dir::East,
    }
}

/// The foot of the bed that a half of one at `pos` belongs to.
pub fn bed_foot(pos: BlockPos, state: BlockState) -> BlockPos {
    if state.kind() == id::BED_HEAD { pos.offset(bed_toward(state.meta()).opposite()) } else { pos }
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
    fn eating_takes_a_moment_fills_the_food_bar_and_stops_when_let_go() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        give(&mut p, &w, "apple", 2);
        p.use_item(&mut w);
        assert!(p.eating.is_none(), "a full stomach takes nothing");
        p.food = 10;
        // Held: one apple goes down, in its own time.
        for _ in 0..EAT_TICKS - 1 {
            w.tick += 1;
            p.use_item(&mut w);
            p.tick(&mut w);
        }
        assert_eq!(p.food, 10, "not swallowed yet");
        w.tick += 1;
        p.use_item(&mut w);
        p.tick(&mut w);
        assert_eq!((p.food, p.inv.held().map(|s| s.count)), (14, Some(1)));
        // Let go halfway: nothing is eaten.
        p.use_item(&mut w);
        for _ in 0..EAT_TICKS {
            w.tick += 1;
            p.tick(&mut w);
        }
        assert_eq!((p.food, p.inv.held().map(|s| s.count), p.eating), (14, Some(1), None));
    }

    #[test]
    fn effort_empties_the_food_bar_which_decides_whether_health_comes_back() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        // Saturation goes first, then the bar.
        p.saturation = 1.0;
        p.exhaustion = EXHAUSTED * 3.0;
        p.hunger(&mut w);
        assert_eq!((p.saturation, p.food), (0.0, 18));
        // Fed: health comes back, and that costs effort.
        p.health = 10;
        for _ in 0..80 {
            p.hunger(&mut w);
        }
        assert_eq!(p.health, 11);
        assert!(p.exhaustion > 0.0);
        // Peckish: no healing.
        p.food = 10;
        for _ in 0..200 {
            p.hunger(&mut w);
        }
        assert_eq!(p.health, 11);
        // Starving: it hurts, but never past half a heart.
        p.food = 0;
        for _ in 0..80 {
            p.hurt_cool = 0;
            p.hunger(&mut w);
        }
        assert_eq!(p.health, 10);
        p.health = 1;
        for _ in 0..400 {
            p.hurt_cool = 0;
            p.hunger(&mut w);
        }
        assert_eq!((p.health, p.dead), (1, false));
    }

    #[test]
    fn sprinting_costs_food_and_a_hungry_player_cannot_sprint() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        settle(&mut w);
        let sprint = MoveInput { keys: Keys(Keys::FORWARD | Keys::SPRINT), yaw: 0, pitch: 0 };
        for seq in 1..=40 {
            p.inputs.push_back((seq, sprint));
            p.tick(&mut w);
        }
        assert!(p.exhaustion > 0.5 || p.saturation < 5.0, "sprinting cost nothing");
        p.food = TOO_HUNGRY_TO_SPRINT;
        p.inputs.push_back((41, sprint));
        p.tick(&mut w);
        assert!(!p.last_input.keys.has(Keys::SPRINT));
    }

    #[test]
    fn armour_softens_blows_and_wears_but_does_not_break_a_fall() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let plate = w.reg.item_by_name("iron_body").expect("an iron breastplate");
        p.inv.armor[1] = Some(ItemStack::new(plate, 1));
        assert_eq!(p.inv.armor_points(&w.reg), 6);
        // A blow of 10 less 24 per cent.
        assert!(p.take_damage(&mut w, 10, Some(DVec3::X)));
        assert_eq!(p.health, 12);
        assert_eq!(p.inv.armor[1].map(|s| s.damage), Some(1));
        p.hurt_cool = 0;
        assert!(p.take_damage(&mut w, 4, None), "a fall");
        assert_eq!(p.health, 8);
        assert_eq!(p.inv.armor[1].map(|s| s.damage), Some(1));
        // Dying drops what is worn too.
        p.hurt_cool = 0;
        p.take_damage(&mut w, 100, None);
        assert!(p.inv.armor.iter().all(Option::is_none));
    }

    #[test]
    fn a_hoe_tills_soil_seeds_grow_on_it_and_a_ripe_crop_gives_grain() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let ents = Entities::default();
        let soil = BlockPos::new(2, 60, 0);
        w.set(soil, BlockState::of(id::GRASS));
        give(&mut p, &w, "wood_hoe", 1);
        p.use_on(&mut w, &[], &ents, soil, Dir::Up as u8);
        assert_eq!(w.block(soil).kind(), id::FARMLAND);
        assert_eq!(p.inv.held().map(|s| s.damage), Some(1), "the hoe wears");
        // Seeds go on farmland, and nowhere else.
        p.inv.slots[0] = None;
        give(&mut p, &w, "seeds", 2);
        p.use_on(&mut w, &[], &ents, BlockPos::new(3, 60, 0), Dir::Up as u8);
        assert!(w.block(BlockPos::new(3, 61, 0)).is_air(), "not on stone");
        p.use_on(&mut w, &[], &ents, soil, Dir::Up as u8);
        let crop = soil.offset(Dir::Up);
        assert_eq!(w.block(crop), BlockState::new(id::GRAIN, 0));
        // It grows, a stage at a time, to ripe and no further.
        for _ in 0..400 {
            random_tick_at(&mut w, crop);
        }
        assert_eq!(w.block(crop), BlockState::new(id::GRAIN, 7));
        // Ripe, it gives grain and seeds; unripe, a seed.
        w.events.clear();
        rules::drop_block(&mut w, crop, BlockState::new(id::GRAIN, 7));
        let dropped = |w: &ServerWorld, name: &str| {
            let item = w.reg.item_by_name(name).expect("item");
            w.events
                .iter()
                .map(|e| match e {
                    crate::world::WorldEvent::DropItem { stack, .. } if stack.item == item => stack.count as u32,
                    _ => 0,
                })
                .sum::<u32>()
        };
        assert_eq!(dropped(&w, "grain"), 1);
        assert!((1..=3).contains(&dropped(&w, "seeds")));
        w.events.clear();
        rules::drop_block(&mut w, crop, BlockState::new(id::GRAIN, 3));
        assert_eq!((dropped(&w, "grain"), dropped(&w, "seeds")), (0, 1));
        // Something solid on farmland packs it down, and the crop on it goes.
        w.set(crop, BlockState::of(id::STONE));
        rules::drain_updates(&mut w);
        assert_eq!(w.block(soil).kind(), id::DIRT);
    }

    #[test]
    fn farmland_soaks_up_water_nearby_and_dries_out_without_it() {
        let mut w = flat_world();
        let soil = BlockPos::new(5, 60, 5);
        w.set(soil, BlockState::of(id::FARMLAND));
        w.set(BlockPos::new(8, 60, 5), BlockState::of(id::WATER));
        random_tick_at(&mut w, soil);
        assert_eq!(w.block(soil).kind(), id::FARMLAND_WET);
        w.set(BlockPos::new(8, 60, 5), BlockState::of(id::STONE));
        random_tick_at(&mut w, soil);
        assert_eq!(w.block(soil).kind(), id::FARMLAND);
    }

    #[test]
    fn a_bed_takes_two_cells_and_breaking_either_half_takes_the_whole_bed() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let ents = Entities::default();
        give(&mut p, &w, "bed", 1);
        // Facing south, its head is the far, southern cell.
        p.use_on(&mut w, &[], &ents, BlockPos::new(2, 60, 0), Dir::Up as u8);
        let (foot, head) = (BlockPos::new(2, 61, 0), BlockPos::new(2, 61, 1));
        assert_eq!(w.block(foot), BlockState::new(id::BED, 1));
        assert_eq!(w.block(head), BlockState::new(id::BED_HEAD, 1));
        assert_eq!(bed_foot(head, w.block(head)), foot);
        assert!(p.inv.held().is_none());
        // Broken at the head, it drops one bed and leaves nothing.
        w.events.clear();
        rules::break_block(&mut w, head, true);
        rules::drain_updates(&mut w);
        assert!(w.block(foot).is_air() && w.block(head).is_air());
        let beds = w.events.iter().filter(|e| matches!(e, crate::world::WorldEvent::DropItem { .. })).count();
        assert_eq!(beds, 1);
        // No room for the head: refused, and the bed kept.
        give(&mut p, &w, "bed", 1);
        w.set(BlockPos::new(2, 61, 1), BlockState::of(id::STONE));
        p.use_on(&mut w, &[], &ents, BlockPos::new(2, 60, 0), Dir::Up as u8);
        assert!(w.block(foot).is_air());
        assert!(p.inv.held().is_some());
    }

    #[test]
    fn each_level_asks_a_little_more_than_the_last() {
        assert_eq!(level_of(0), (0, 0.0));
        assert_eq!(level_of(7), (1, 0.0));
        assert_eq!(level_of(7 + 9), (2, 0.0));
        let (level, part) = level_of(7 + 9 + 5);
        assert_eq!(level, 2);
        assert!((part - 5.0 / 11.0).abs() < 1e-6);
    }

    #[test]
    fn a_raised_shield_takes_a_blow_from_in_front_but_not_from_behind() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        give(&mut p, &w, "shield", 1);
        p.guard(true, &w.reg.clone());
        assert!(p.guarding);
        // Facing +z, with the blow coming from +z: pushed back towards -z.
        assert!(!p.take_damage(&mut w, 5, Some(DVec3::new(0.0, 0.0, -0.4))));
        assert_eq!(p.health, 20);
        assert_eq!(p.inv.held().map(|s| s.damage), Some(1), "the shield wears");
        p.hurt_cool = 0;
        assert!(p.take_damage(&mut w, 5, Some(DVec3::new(0.0, 0.0, 0.4))), "from behind");
        assert_eq!(p.health, 15);
        // Down, nothing is guarded; and nothing but a shield guards.
        p.inv.slots[0] = None;
        p.guard(true, &w.reg.clone());
        assert!(!p.guarding);
        // A guard raised and then left without its shield guards nothing, and drops.
        give(&mut p, &w, "shield", 1);
        p.guard(true, &w.reg.clone());
        p.inv.slots[0] = None;
        p.hurt_cool = 0;
        assert!(p.take_damage(&mut w, 5, Some(DVec3::new(0.0, 0.0, -0.4))), "an empty hand");
        p.tick(&mut w);
        assert!(!p.guarding);
    }

    #[test]
    fn a_bow_needs_arrows_and_shoots_harder_the_longer_it_is_drawn() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        give(&mut p, &w, "bow", 1);
        assert!(p.shoot(&mut w, 20).is_none(), "no arrows");
        give(&mut p, &w, "arrow", 3);
        p.bow_ticks = 20;
        assert!(p.shoot(&mut w, 1).is_none(), "not drawn");
        let (_, slow, weak) = p.shoot(&mut w, 4).expect("a short shot");
        p.bow_ticks = 20;
        let (_, fast, strong) = p.shoot(&mut w, 20).expect("a full shot");
        assert!(fast.length() > slow.length() * 2.0 && strong > weak);
        assert!(fast.z > 0.0, "along the way it faces");
        // However long the client says it drew, the next arrow waits for the drawing.
        assert!(p.shoot(&mut w, 20).is_none(), "no time to draw");
        p.bow_ticks = 4;
        let (_, quick, _) = p.shoot(&mut w, 20).expect("a hurried shot");
        assert!(quick.length() < fast.length() * 0.6, "as weak as the time it had");
        p.bow_ticks = 20;
        assert!(p.shoot(&mut w, 20).is_none(), "all arrows gone");
        assert_eq!(p.inv.held().map(|s| s.damage), Some(3), "the bow wears");
    }

    #[test]
    fn digging_coal_teaches_a_little_and_dying_costs_half() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let ore = BlockPos::new(2, 61, 0);
        w.set(ore, BlockState::of(id::COAL_ORE));
        give(&mut p, &w, "iron_pick", 1);
        p.finish_dig(&mut w, ore, BlockState::of(id::COAL_ORE));
        assert_eq!(p.xp, 1);
        p.xp = 40;
        p.take_damage(&mut w, 100, None);
        assert_eq!(p.xp, 20);
    }

    fn random_tick_at(w: &mut ServerWorld, p: BlockPos) {
        rules::tick_randomly(w, p);
    }

    #[test]
    fn a_tinderbox_lights_a_fire_where_one_can_burn() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let ents = Entities::default();
        give(&mut p, &w, "tinderbox", 1);
        let stone = BlockPos::new(1, 60, 1);
        p.use_on(&mut w, &[], &ents, stone, Dir::Up as u8);
        assert_eq!(w.block(stone.offset(Dir::Up)).kind(), id::FIRE);
        assert_eq!(p.inv.held().map(|s| s.damage), Some(1), "the tinderbox wears");
        // Not in mid air, with nothing to burn on or beside.
        w.set(BlockPos::new(1, 63, 2), BlockState::of(id::STONE));
        p.use_on(&mut w, &[], &ents, BlockPos::new(1, 63, 2), Dir::North as u8);
        assert!(w.block(BlockPos::new(1, 63, 1)).is_air());
        // But against wood, anywhere.
        w.set(BlockPos::new(1, 63, 2), BlockState::of(id::OAK_PLANKS));
        p.use_on(&mut w, &[], &ents, BlockPos::new(1, 63, 2), Dir::North as u8);
        assert_eq!(w.block(BlockPos::new(1, 63, 1)).kind(), id::FIRE);
        // Standing in fire hurts.
        p.mode = GameMode::Survival;
        w.set(BlockPos::new(0, 61, 0), BlockState::of(id::FIRE));
        w.tick = 10;
        p.hazards(&mut w);
        assert!(p.health < 20);
    }

    #[test]
    fn a_bucket_scoops_up_a_spring_and_pours_it_out_again() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let ents = Entities::default();
        // A pool one block deep, two blocks ahead, and the player looking down into it.
        let pool = BlockPos::new(0, 60, 2);
        w.set(pool, BlockState::of(id::WATER));
        p.last_input.pitch = (40.0f64 * 65536.0 / 360.0) as i16;
        give(&mut p, &w, "bucket", 2);
        p.use_item(&mut w);
        assert!(w.block(pool).is_air(), "the spring is in the bucket");
        let held = |p: &Player, w: &ServerWorld, name: &str| {
            let item = w.reg.item_by_name(name).expect("item");
            p.inv.slots.iter().flatten().filter(|s| s.item == item).map(|s| u32::from(s.count)).sum::<u32>()
        };
        assert_eq!((held(&p, &w, "bucket"), held(&p, &w, "water_bucket")), (1, 1), "one filled, one still empty");
        // Nothing to scoop: nothing happens.
        p.use_item(&mut w);
        assert_eq!((held(&p, &w, "bucket"), held(&p, &w, "water_bucket")), (1, 1));
        // Poured out on the stone beside the hole, it is a spring there, and the bucket is empty.
        let slot =
            p.inv.slots.iter().position(|s| s.is_some_and(|s| Some(s.item) == w.reg.item_by_name("water_bucket")));
        p.select(slot.expect("a full bucket") as u8);
        let stone = BlockPos::new(1, 60, 1);
        p.use_on(&mut w, &[], &ents, stone, Dir::Up as u8);
        assert_eq!(w.block(stone.offset(Dir::Up)), BlockState::of(id::WATER));
        assert_eq!((held(&p, &w, "bucket"), held(&p, &w, "water_bucket")), (2, 0));
        // Running water is not a spring: a bucket leaves it be.
        w.set(BlockPos::new(0, 61, 2), BlockState::new(id::WATER, 3));
        w.set(stone.offset(Dir::Up), BlockState::AIR);
        p.select(p.inv.slots.iter().position(|s| s.is_some()).expect("buckets") as u8);
        p.use_item(&mut w);
        assert_eq!(held(&p, &w, "water_bucket"), 0);
    }

    #[test]
    fn lava_from_a_bucket_meets_water_and_hardens() {
        let mut w = flat_world();
        let (mut p, _peer) = player_at(0.5, 0.5);
        let ents = Entities::default();
        give(&mut p, &w, "lava_bucket", 1);
        w.set(BlockPos::new(2, 61, 1), BlockState::of(id::WATER));
        let stone = BlockPos::new(1, 60, 1);
        p.use_on(&mut w, &[], &ents, stone, Dir::Up as u8);
        let lava = stone.offset(Dir::Up);
        assert_eq!(w.block(lava), BlockState::of(id::LAVA));
        assert_eq!(p.inv.held().map(|s| s.item), w.reg.item_by_name("bucket"), "and the bucket is empty");
        for _ in 0..80 {
            w.tick += 1;
            rules::run(&mut w);
        }
        assert_eq!(w.block(lava).kind(), id::OBSIDIAN, "a spring of lava touched by water");
        // In creative, a bucket is as full after as before.
        let (mut c, _peer) = player_at(0.5, 0.5);
        c.mode = GameMode::Creative;
        give(&mut c, &w, "water_bucket", 1);
        c.use_on(&mut w, &[], &ents, BlockPos::new(-1, 60, 1), Dir::Up as u8);
        assert_eq!(w.block(BlockPos::new(-1, 61, 1)), BlockState::of(id::WATER));
        assert_eq!(c.inv.held().map(|s| s.item), w.reg.item_by_name("water_bucket"));
    }

    #[test]
    fn a_crop_in_the_dark_waits_for_light() {
        let mut w = flat_world();
        let soil = BlockPos::new(2, 60, 0);
        let crop = soil.offset(Dir::Up);
        w.set(soil, BlockState::of(id::FARMLAND_WET));
        w.set(crop, BlockState::new(id::GRAIN, 0));
        // A stone hut over it, walls and roof, and no light inside.
        for x in 0..=4 {
            for z in -2..=2 {
                for y in 61..=63 {
                    let q = BlockPos::new(x, y, z);
                    if x == 0 || x == 4 || z == -2 || z == 2 || y == 63 {
                        w.set(q, BlockState::of(id::STONE));
                    }
                }
            }
        }
        for _ in 0..400 {
            random_tick_at(&mut w, crop);
        }
        assert_eq!(w.block(crop), BlockState::new(id::GRAIN, 0), "nothing grows in the dark");
        w.set(BlockPos::new(1, 61, 1), BlockState::of(id::GLOW_BLOCK));
        for _ in 0..400 {
            random_tick_at(&mut w, crop);
        }
        assert_eq!(w.block(crop), BlockState::new(id::GRAIN, 7), "a light is as good as the sky");
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
