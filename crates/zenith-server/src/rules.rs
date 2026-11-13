//! How blocks behave: fluids, gravity, things that need support, growth, and signal circuits.
use crate::world::{ServerWorld, WorldEvent};
use zenith_content::{BlockState, Fluid, ItemStack, Shape, id};
use zenith_core::pos::{MAX_Y, MIN_Y, SECTIONS};
use zenith_core::{BlockPos, ChunkPos, Dir, FxMap, FxSet, Rng};
use zenith_protocol::EffectKind;
use zenith_world::BlockAccess;
use zenith_world::worldgen::{Stamp, Tree, features};

const WATER_DELAY: u64 = 5;
const LAVA_DELAY: u64 = 30;
/// How many block updates one tick will process. The rest wait for the next tick.
const MAX_UPDATES: usize = 20_000;
const MAX_SCHEDULED: usize = 4_000;
/// A wire signal fades out after this many blocks, so no network is ever bigger than this reaches.
const MAX_NETWORK: usize = 2048;

/// Runs what is due this tick: scheduled block ticks, then the updates they and players caused.
pub fn run(w: &mut ServerWorld) {
    for _ in 0..MAX_SCHEDULED {
        let Some(p) = w.pop_due() else { break };
        tick_block(w, p);
    }
    drain_updates(w);
}

pub fn drain_updates(w: &mut ServerWorld) {
    for _ in 0..MAX_UPDATES {
        let Some(p) = w.updates.pop_front() else { break };
        neighbour_changed(w, p);
    }
}

fn neighbour_changed(w: &mut ServerWorld, p: BlockPos) {
    let state = w.block(p);
    if state.is_air() {
        return;
    }
    let def = w.reg.block(state);
    let (fluid, gravity, fragile, kind) = (def.fluid, def.gravity, def.fragile, state.kind());
    match fluid {
        Fluid::Water => w.schedule(p, WATER_DELAY),
        Fluid::Lava => w.schedule(p, LAVA_DELAY),
        Fluid::None => {}
    }
    if gravity && falls_into(w, p.offset(Dir::Down)) {
        w.schedule(p, 2);
    }
    if fragile && !has_support(w, p, state) {
        break_block(w, p, true);
        return;
    }
    match kind {
        // Anything solid on farmland packs it back down to dirt.
        id::FARMLAND | id::FARMLAND_WET if w.reg.lut.collides[w.block(p.offset(Dir::Up)).kind() as usize] => {
            w.set(p, BlockState::of(id::DIRT));
        }
        // Half a bed without its other half goes too, leaving nothing: the half that was broken
        // left the bed.
        id::BED | id::BED_HEAD => {
            let toward = crate::player::bed_toward(state.meta());
            let (other, want) =
                if kind == id::BED { (p.offset(toward), id::BED_HEAD) } else { (p.offset(toward.opposite()), id::BED) };
            if w.block(other).kind() != want && w.is_loaded(other) {
                w.set(p, BlockState::AIR);
            }
        }
        // A rail turns to join the rails around it.
        id::RAIL | id::BOOST_RAIL => {
            let shape = crate::rails::shape_for(&*w, p, state.meta());
            if shape != state.meta() {
                w.set(p, BlockState::new(kind, shape));
            }
        }
        // A fence joins what it can, whenever a neighbour changes.
        id::FENCE => {
            let joined = fence_joins(w, p);
            if joined != state.meta() {
                w.set(p, BlockState::new(id::FENCE, joined));
            }
        }
        // A door's halves go together, and the bottom one needs a floor.
        id::DOOR => {
            let top = state.meta() & 8 != 0;
            let other = if top { p.offset(Dir::Down) } else { p.offset(Dir::Up) };
            let partner = w.block(other);
            let whole = partner.kind() == id::DOOR && (partner.meta() & 8 != 0) != top;
            if !whole && w.is_loaded(other) {
                w.set(p, BlockState::AIR);
            } else if !top && !w.reg.lut.collides[w.block(p.offset(Dir::Down)).kind() as usize] {
                break_block(w, p, true);
            }
        }
        id::WIRE | id::LEVER | id::LAMP | id::LAMP_LIT | id::INVERTER | id::INVERTER_OFF => w.schedule(p, 1),
        id::FIRE => w.schedule(p, FIRE_FLARE),
        id::TNT if powered(w, p) => {
            w.set(p, BlockState::AIR);
            w.events.push(WorldEvent::Prime { pos: p });
        }
        _ => {}
    }
}

fn tick_block(w: &mut ServerWorld, p: BlockPos) {
    let state = w.block(p);
    let def = w.reg.block(state);
    let (fluid, gravity) = (def.fluid, def.gravity);
    if fluid != Fluid::None {
        tick_fluid(w, p, state, fluid);
    } else if gravity {
        if falls_into(w, p.offset(Dir::Down)) && w.is_loaded(p.offset(Dir::Down)) {
            w.set(p, BlockState::AIR);
            w.events.push(WorldEvent::Falling { pos: p, state });
        }
    } else {
        match state.kind() {
            id::WIRE => update_wires(w, p),
            id::LEVER => {
                for d in Dir::ALL {
                    w.updates.push_back(p.offset(d));
                }
            }
            id::LAMP | id::LAMP_LIT => {
                let lit = powered(w, p);
                let kind = if lit { id::LAMP_LIT } else { id::LAMP };
                w.set(p, BlockState::of(kind));
            }
            id::FIRE => tick_fire(w, p, state),
            id::INVERTER | id::INVERTER_OFF => {
                let input = powered(w, p.offset(Dir::Down));
                let kind = if input { id::INVERTER_OFF } else { id::INVERTER };
                w.set(p, BlockState::new(kind, state.meta()));
            }
            _ => {}
        }
    }
}

/// How long a fire burns between flares, in ticks, give or take.
const FIRE_FLARE: u64 = 25;
/// How many flares a fire lasts.
const FIRE_LIFE: u8 = 15;

/// One flare of a fire. It takes what will burn of the blocks around it, often moving in where they
/// were; catches in the air beside anything that burns, close by; and dies with nothing under it
/// and nothing to feed on, with nothing to feed on before long, or when it has burned its time.
/// So it runs through a wood and along a wall of planks, and never crosses bare ground.
fn tick_fire(w: &mut ServerWorld, p: BlockPos, state: BlockState) {
    let burns = |w: &ServerWorld, q: BlockPos| w.reg.block(w.block(q)).flammable;
    let age = state.meta();
    let fed = Dir::ALL.iter().any(|&d| burns(w, p.offset(d)) > 0);
    let floor = w.reg.lut.collides[w.block(p.offset(Dir::Down)).kind() as usize];
    if (!floor && !fed) || age >= FIRE_LIFE || (!fed && w.rng.chance(0.4)) {
        w.set(p, BlockState::AIR);
        return;
    }
    for d in Dir::ALL {
        let q = p.offset(d);
        let chance = burns(w, q);
        if chance == 0 || w.rng.below(100) >= u32::from(chance) {
            continue;
        }
        if w.block(q).kind() == id::TNT {
            w.set(q, BlockState::AIR);
            w.events.push(WorldEvent::Prime { pos: q });
        } else {
            let after = if w.rng.chance(0.7) { BlockState::of(id::FIRE) } else { BlockState::AIR };
            w.set(q, after);
        }
    }
    for _ in 0..3 {
        let q = p.add(w.rng.range(-1, 1), w.rng.range(-1, 2), w.rng.range(-1, 1));
        if q != p
            && w.is_loaded(q)
            && w.block(q).is_air()
            && Dir::ALL.iter().any(|&d| burns(w, q.offset(d)) > 0)
            && w.rng.chance(0.35)
        {
            w.set(q, BlockState::of(id::FIRE));
        }
    }
    w.set(p, BlockState::new(id::FIRE, age + 1));
    let wait = FIRE_FLARE + u64::from(w.rng.below(FIRE_FLARE as u32));
    w.schedule(p, wait);
}

/// Whether a falling block would keep falling through this cell.
fn falls_into(w: &ServerWorld, p: BlockPos) -> bool {
    let state = w.block(p);
    let def = w.reg.block(state);
    p.y >= MIN_Y
        && !w.reg.lut.collides[state.kind() as usize]
        && (state.is_air() || def.fluid != Fluid::None || def.replaceable)
}

/// Whether the thing at `p` still has what it rests on.
pub fn has_support(w: &ServerWorld, p: BlockPos, state: BlockState) -> bool {
    let solid = |q: BlockPos| w.reg.lut.collides[w.block(q).kind() as usize];
    let def = w.reg.block(state);
    match def.shape {
        Shape::Torch => match state.meta() {
            0 => solid(p.offset(Dir::Down)),
            1 => solid(p.offset(Dir::North)),
            2 => solid(p.offset(Dir::South)),
            3 => solid(p.offset(Dir::West)),
            _ => solid(p.offset(Dir::East)),
        },
        Shape::Ladder => match state.meta() & 3 {
            0 => solid(p.offset(Dir::North)),
            1 => solid(p.offset(Dir::South)),
            2 => solid(p.offset(Dir::West)),
            _ => solid(p.offset(Dir::East)),
        },
        Shape::Inset => {
            let below = w.block(p.offset(Dir::Down)).kind();
            below == id::SAND || below == state.kind()
        }
        Shape::Layer => {
            let below = w.block(p.offset(Dir::Down));
            solid(p.offset(Dir::Down)) || w.reg.block(below).tint == zenith_content::Tint::Foliage
        }
        Shape::Crop => matches!(w.block(p.offset(Dir::Down)).kind(), id::FARMLAND | id::FARMLAND_WET),
        _ => solid(p.offset(Dir::Down)),
    }
}

/// Which sides a fence at `p` joins: other fences, gates, and whole blocks. Bits north, south, west, east.
pub fn fence_joins(w: &ServerWorld, p: BlockPos) -> u8 {
    let mut joined = 0;
    for (bit, d) in Dir::HORIZONTAL.into_iter().enumerate() {
        let n = w.block(p.offset(d));
        if matches!(n.kind(), id::FENCE | id::GATE) || w.reg.lut.opaque[n.kind() as usize] {
            joined |= 1 << bit;
        }
    }
    joined
}

/// Opens what is shut or shuts what is open: a door (both halves), a trapdoor or a gate. Returns
/// whether there was anything to swing.
pub fn swing(w: &mut ServerWorld, p: BlockPos) -> bool {
    let state = w.block(p);
    let flipped = |s: BlockState| BlockState::new(s.kind(), s.meta() ^ 4);
    match state.kind() {
        id::DOOR => {
            let other = if state.meta() & 8 != 0 { p.offset(Dir::Down) } else { p.offset(Dir::Up) };
            let partner = w.block(other);
            w.set(p, flipped(state));
            if partner.kind() == id::DOOR {
                w.set(other, flipped(partner));
            }
        }
        id::TRAPDOOR | id::GATE => {
            w.set(p, flipped(state));
        }
        _ => return false,
    }
    w.effect(EffectKind::Open, p.center());
    true
}

/// Breaks the block at `p`, leaving its drop if `drops` is set.
pub fn break_block(w: &mut ServerWorld, p: BlockPos, drops: bool) {
    let state = w.block(p);
    if drops {
        drop_block(w, p, state);
    }
    w.effect(EffectKind::BlockBroken(state), p.center());
    w.set(p, BlockState::AIR);
}

/// Leaves what a block drops when broken without a tool in question.
pub fn drop_block(w: &mut ServerWorld, p: BlockPos, state: BlockState) {
    let give = |w: &mut ServerWorld, name: &str, count: i32| {
        if let Some(item) = w.reg.item_by_name(name)
            && count > 0
        {
            w.events.push(WorldEvent::DropItem { pos: p.center(), stack: ItemStack::new(item, count as u8) });
        }
    };
    match state.kind() {
        // Wild grass now and then has seeds in it.
        id::TALL_GRASS | id::FERN => {
            if w.rng.chance(0.125) {
                give(w, zenith_content::it::SEEDS, 1);
            }
            return;
        }
        // A ripe crop gives its grain and seeds to sow again; an unripe one, the seed it grew from.
        id::GRAIN => {
            if state.meta() >= 7 {
                give(w, zenith_content::it::GRAIN, 1);
                let seeds = w.rng.range(1, 3);
                give(w, zenith_content::it::SEEDS, seeds);
            } else {
                give(w, zenith_content::it::SEEDS, 1);
            }
            return;
        }
        _ => {}
    }
    let def = w.reg.block(state);
    let (lo, hi) = (def.drop_min as i32, def.drop_max as i32);
    let name = def.drop.clone().unwrap_or_else(|| def.name.clone());
    if name.is_empty() {
        return;
    }
    let Some(item) = w.reg.item_by_name(&name) else { return };
    let count = if lo >= hi { lo } else { w.rng.range(lo, hi) };
    if count > 0 {
        w.events.push(WorldEvent::DropItem { pos: p.center(), stack: ItemStack::new(item, count as u8) });
    }
}

fn fluid_at(w: &ServerWorld, p: BlockPos, kind: u16) -> Option<u8> {
    let s = w.block(p);
    (s.kind() == kind).then(|| s.meta())
}

/// Whether a fluid can move into this cell, washing away what is there.
fn can_flow_into(w: &ServerWorld, p: BlockPos, kind: u16) -> bool {
    if !p.in_world() || !w.is_loaded(p) {
        return false;
    }
    let s = w.block(p);
    if s.kind() == kind {
        return false;
    }
    let def = w.reg.block(s);
    s.is_air()
        || (def.fluid == Fluid::None
            && (def.replaceable || def.shape == Shape::Cross || def.shape == Shape::Flat || def.shape == Shape::Torch))
}

/// Puts a fluid in a cell, washing out whatever small thing was there.
pub(crate) fn flow_into(w: &mut ServerWorld, p: BlockPos, state: BlockState) {
    let there = w.block(p);
    if !there.is_air() {
        drop_block(w, p, there);
    }
    w.set(p, state);
}

fn tick_fluid(w: &mut ServerWorld, p: BlockPos, state: BlockState, fluid: Fluid) {
    let kind = state.kind();
    let (level, falling) = (state.meta() & 7, state.meta() & 8 != 0);
    let (step, other) = if fluid == Fluid::Lava { (2, id::WATER) } else { (1, id::LAVA) };

    // Lava touching water hardens.
    let touches_other = Dir::ALL.iter().any(|&d| w.block(p.offset(d)).kind() == other);
    if touches_other && fluid == Fluid::Lava {
        let hard = if level == 0 && !falling { id::OBSIDIAN } else { id::COBBLE };
        w.effect(EffectKind::Fizz, p.center());
        w.set(p, BlockState::of(hard));
        return;
    }

    // Flowing fluid is only ever as strong as what feeds it.
    if level != 0 || falling {
        let above_same = fluid_at(w, p.offset(Dir::Up), kind).is_some();
        let (mut best, mut sources) = (None::<u8>, 0);
        for d in Dir::HORIZONTAL {
            if let Some(m) = fluid_at(w, p.offset(d), kind) {
                let l = m & 7;
                if l == 0 && m & 8 == 0 {
                    sources += 1;
                }
                best = Some(best.map_or(l, |b: u8| b.min(l)));
            }
        }
        let below = w.block(p.offset(Dir::Down));
        let on_solid_or_source =
            w.reg.lut.collides[below.kind() as usize] || (below.kind() == kind && below.meta() == 0);
        let wanted = if fluid == Fluid::Water && sources >= 2 && on_solid_or_source {
            BlockState::new(kind, 0)
        } else if above_same {
            BlockState::new(kind, 8 | 1)
        } else {
            match best {
                Some(l) if l + step <= 7 => BlockState::new(kind, l + step),
                _ => BlockState::AIR,
            }
        };
        if wanted != state {
            w.set(p, wanted);
            if wanted.is_air() {
                return;
            }
        }
    }

    let now = w.block(p);
    if now.kind() != kind {
        return;
    }
    let (level, falling) = (now.meta() & 7, now.meta() & 8 != 0);
    let below = p.offset(Dir::Down);
    if can_flow_into(w, below, kind) {
        flow_into(w, below, BlockState::new(kind, 8 | 1));
        // A fluid with open air below it does not spread sideways as well, unless it is a source.
        if level != 0 || falling {
            return;
        }
    } else if fluid_at(w, below, kind).is_some() && (level != 0 || falling) {
        return;
    }
    let delay = if fluid == Fluid::Lava { LAVA_DELAY } else { WATER_DELAY };
    let next = if falling { 1 + step } else { level + step };
    if next > 7 {
        return;
    }
    for d in Dir::HORIZONTAL {
        let n = p.offset(d);
        if !w.is_loaded(n) {
            // Try again when that chunk may have arrived.
            w.schedule(p, delay * 8);
            continue;
        }
        if can_flow_into(w, n, kind) {
            flow_into(w, n, BlockState::new(kind, next));
        } else if let Some(m) = fluid_at(w, n, kind)
            && m & 8 == 0
            && m & 7 > next
        {
            w.schedule(n, delay);
        }
    }
}

/// The signal strength a wire is at.
fn wire_power(state: BlockState) -> u8 {
    if state.kind() == id::WIRE { state.meta() } else { 0 }
}

/// Whether anything next to `p` is feeding it a signal.
pub fn powered(w: &ServerWorld, p: BlockPos) -> bool {
    if wire_power(w.block(p)) > 0 {
        return true;
    }
    Dir::ALL.iter().any(|&d| {
        let s = w.block(p.offset(d));
        wire_power(s) > 0 || (s.kind() == id::LEVER && s.meta() & 1 == 1)
    })
}

/// How much signal a source next to a wire gives it.
fn source_power(w: &ServerWorld, p: BlockPos) -> u8 {
    let feeding = Dir::ALL.iter().any(|&d| {
        let s = w.block(p.offset(d));
        (s.kind() == id::LEVER && s.meta() & 1 == 1) || s.kind() == id::INVERTER
    });
    if feeding { 15 } else { 0 }
}

fn wire_links(p: BlockPos) -> [BlockPos; 12] {
    [
        p.add(1, 0, 0),
        p.add(-1, 0, 0),
        p.add(0, 0, 1),
        p.add(0, 0, -1),
        p.add(1, 1, 0),
        p.add(-1, 1, 0),
        p.add(0, 1, 1),
        p.add(0, 1, -1),
        p.add(1, -1, 0),
        p.add(-1, -1, 0),
        p.add(0, -1, 1),
        p.add(0, -1, -1),
    ]
}

/// Recomputes the whole wire network around `start` in one pass: the signal is the source's
/// strength minus the steps from it.
fn update_wires(w: &mut ServerWorld, start: BlockPos) {
    let mut network: Vec<BlockPos> = vec![start];
    let mut seen: FxSet<BlockPos> = network.iter().copied().collect();
    let mut i = 0;
    while i < network.len() && network.len() < MAX_NETWORK {
        for n in wire_links(network[i]) {
            if w.block(n).kind() == id::WIRE && seen.insert(n) {
                network.push(n);
            }
        }
        i += 1;
    }
    let mut level: FxMap<BlockPos, u8> = FxMap::default();
    let mut frontier: Vec<Vec<BlockPos>> = vec![Vec::new(); 16];
    for &p in &network {
        let s = source_power(w, p);
        if s > 0 {
            level.insert(p, s);
            frontier[s as usize].push(p);
        }
    }
    for l in (2..16usize).rev() {
        while let Some(p) = frontier[l].pop() {
            for n in wire_links(p) {
                if seen.contains(&n) && level.get(&n).is_none_or(|&known| known < l as u8 - 1) {
                    level.insert(n, l as u8 - 1);
                    frontier[l - 1].push(n);
                }
            }
        }
    }
    for p in network {
        if w.set(p, BlockState::new(id::WIRE, level.get(&p).copied().unwrap_or(0))) {
            wake_consumers(w, p);
        }
    }
}

/// A signal passes through the block next to a wire or lever, so what uses it can be two steps away.
fn wake_consumers(w: &mut ServerWorld, p: BlockPos) {
    for d in Dir::ALL {
        let n = p.offset(d);
        for d2 in Dir::ALL {
            let q = n.offset(d2);
            if matches!(w.block(q).kind(), id::INVERTER | id::INVERTER_OFF | id::LAMP | id::LAMP_LIT) {
                w.schedule(q, 2);
            }
        }
    }
}

/// Flips a lever.
pub fn toggle_lever(w: &mut ServerWorld, p: BlockPos) {
    let s = w.block(p);
    if s.kind() != id::LEVER {
        return;
    }
    w.effect(EffectKind::Lever, p.center());
    w.set(p, BlockState::new(id::LEVER, s.meta() ^ 1));
    wake_consumers(w, p);
}

/// Writes a feature into the live world, for saplings. Only ever fills what is open.
struct WorldStamp<'a>(&'a mut ServerWorld);

impl Stamp for WorldStamp<'_> {
    fn put(&mut self, x: i32, y: i32, z: i32, state: BlockState, force: bool) {
        let p = BlockPos::new(x, y, z);
        let here = self.0.block(p);
        let open = here.is_air() || self.0.reg.block(here).replaceable || here.kind() == id::SAPLING;
        if open || (force && !self.0.reg.lut.opaque[here.kind() as usize]) {
            self.0.set(p, state);
        }
    }
    fn put_in_rock(&mut self, _x: i32, _y: i32, _z: i32, _state: BlockState) {}
}

fn grow_sapling(w: &mut ServerWorld, p: BlockPos) {
    let kind = match w.rng.below(4) {
        0 => Tree::Birch,
        1 => Tree::Pine,
        _ => Tree::Oak,
    };
    let mut rng = Rng::new(w.rng.next_u64());
    features::tree(&mut WorldStamp(w), kind, p.x, p.y, p.z, &mut rng);
}

fn has_log_nearby(w: &ServerWorld, start: BlockPos) -> bool {
    let is_log = |k: u16| matches!(k, id::OAK_LOG | id::BIRCH_LOG | id::PINE_LOG | id::ACACIA_LOG);
    let mut frontier = vec![(start, 0u8)];
    let mut seen: FxSet<BlockPos> = frontier.iter().map(|&(p, _)| p).collect();
    let mut i = 0;
    while i < frontier.len() && frontier.len() < 256 {
        let (p, depth) = frontier[i];
        i += 1;
        for d in Dir::ALL {
            let n = p.offset(d);
            let s = w.block(n);
            if is_log(s.kind()) {
                return true;
            }
            if depth < 5
                && w.reg.block(s).tint == zenith_content::Tint::Foliage
                && s.kind() != id::FERN
                && seen.insert(n)
            {
                frontier.push((n, depth + 1));
            }
        }
    }
    false
}

/// Gives each simulated section a few random ticks: grass spreads, leaves decay, saplings grow.
pub fn random_ticks(w: &mut ServerWorld, chunks: &[ChunkPos]) {
    const PER_SECTION: u32 = 3;
    for &pos in chunks {
        for s in 0..SECTIONS {
            let Some(chunk) = w.map.chunks.get(&pos) else { break };
            let section = &chunk.sections[s];
            let wants_ticks = section.values().iter().take(64).any(|&v| {
                matches!(
                    BlockState(v).kind(),
                    id::GRASS
                        | id::SAPLING
                        | id::GRAIN
                        | id::FARMLAND
                        | id::FARMLAND_WET
                        | id::OAK_LEAVES
                        | id::BIRCH_LEAVES
                        | id::PINE_LEAVES
                        | id::ACACIA_LEAVES
                        | id::DIRT
                )
            });
            if !wants_ticks {
                continue;
            }
            for _ in 0..PER_SECTION {
                let (x, y, z) = (w.rng.below(16) as i32, w.rng.below(16) as i32, w.rng.below(16) as i32);
                let p = BlockPos::new(pos.x * 16 + x, MIN_Y + s as i32 * 16 + y, pos.z * 16 + z);
                random_tick(w, p);
            }
        }
    }
}

/// Whether there is water within four blocks across, level with this one or a block up.
fn near_water(w: &ServerWorld, p: BlockPos) -> bool {
    (-4..=4)
        .any(|dx| (-4..=4).any(|dz| (0..=1).any(|dy| w.reg.block(w.block(p.add(dx, dy, dz))).fluid == Fluid::Water)))
}

fn random_tick(w: &mut ServerWorld, p: BlockPos) {
    tick_randomly(w, p);
}

/// Whether a plant at `p` has the light it needs: the sky, overhead or past the side of a roof,
/// through little enough in the way, or a lamp, torch or glowing block a few steps off. Light is
/// never kept on the server, so this works out the part of it that matters, the way the mesher
/// works out all of it: falling one a step, and nine of fifteen is enough.
fn has_light_to_grow(w: &ServerWorld, p: BlockPos) -> bool {
    const ENOUGH: i32 = 9;
    let lut = &w.reg.lut;
    let sky = |c: BlockPos, mut lost: i32| {
        for y in c.y + 1..=MAX_Y {
            lost += i32::from(lut.opacity[w.block(BlockPos::new(c.x, y, c.z)).kind() as usize]);
            if 15 - lost < ENOUGH {
                return false;
            }
        }
        true
    };
    if sky(p, 0)
        || Dir::HORIZONTAL.into_iter().any(|d| {
            let side = p.offset(d);
            !lut.opaque[w.block(side).kind() as usize] && sky(side, 1)
        })
    {
        return true;
    }
    const REACH: i32 = 15 - ENOUGH;
    for dy in -REACH..=REACH {
        for dz in -REACH..=REACH {
            for dx in -REACH..=REACH {
                let steps = dx.abs() + dy.abs() + dz.abs();
                if steps <= REACH {
                    let glow = lut.emission[w.block(p.add(dx, dy, dz)).kind() as usize];
                    if i32::from(glow) - steps >= ENOUGH {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// One random tick of the block at `p`.
pub(crate) fn tick_randomly(w: &mut ServerWorld, p: BlockPos) {
    let state = w.block(p);
    match state.kind() {
        // Crops grow under open sky or a light, faster in watered soil.
        id::GRAIN if state.meta() < 7 => {
            let wet = w.block(p.offset(Dir::Down)).kind() == id::FARMLAND_WET;
            if has_light_to_grow(w, p) && w.rng.chance(if wet { 0.6 } else { 0.25 }) {
                w.set(p, BlockState::new(id::GRAIN, state.meta() + 1));
            }
        }
        // Farmland soaks up water nearby, and dries out without it; dry and bare, it goes back to dirt.
        id::FARMLAND | id::FARMLAND_WET => {
            let wet = near_water(w, p);
            let planted = w.block(p.offset(Dir::Up)).kind() == id::GRAIN;
            match (state.kind(), wet) {
                (id::FARMLAND, true) => {
                    w.set(p, BlockState::of(id::FARMLAND_WET));
                }
                (id::FARMLAND_WET, false) => {
                    w.set(p, BlockState::of(id::FARMLAND));
                }
                (id::FARMLAND, false) if !planted && w.rng.chance(0.3) => {
                    w.set(p, BlockState::of(id::DIRT));
                }
                _ => {}
            }
        }
        id::GRASS => {
            let above = w.block(p.offset(Dir::Up));
            if w.reg.lut.opaque[above.kind() as usize] {
                w.set(p, BlockState::of(id::DIRT));
                return;
            }
            let (dx, dy, dz) = (w.rng.range(-1, 1), w.rng.range(-2, 2), w.rng.range(-1, 1));
            let q = p.add(dx, dy, dz);
            if w.block(q).kind() == id::DIRT && !w.reg.lut.opaque[w.block(q.offset(Dir::Up)).kind() as usize] {
                w.set(q, BlockState::of(id::GRASS));
            }
        }
        id::SAPLING => {
            if w.rng.chance(0.06) && w.block(p.offset(Dir::Up)).is_air() {
                w.set(p, BlockState::AIR);
                grow_sapling(w, p);
            }
        }
        id::OAK_LEAVES | id::BIRCH_LEAVES | id::PINE_LEAVES | id::ACACIA_LEAVES
            // Bit 0 marks leaves a player placed, which stay.
            if state.meta() & 1 == 0 && w.rng.chance(0.2) && !has_log_nearby(w, p) => {
                if w.rng.chance(0.05) {
                    if let Some(item) = w.reg.item_by_name("sapling") {
                        w.events.push(WorldEvent::DropItem { pos: p.center(), stack: ItemStack::new(item, 1) });
                    }
                } else if w.rng.chance(0.04)
                    && let Some(item) = w.reg.item_by_name("apple") {
                        w.events.push(WorldEvent::DropItem { pos: p.center(), stack: ItemStack::new(item, 1) });
                    }
                w.set(p, BlockState::AIR);
            }
        _ => {}
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::persist::ItemRemap;
    use std::sync::Arc;
    use zenith_content::Registry;
    use zenith_core::glam::DVec3;
    use zenith_world::{Chunk, Generator};

    /// A flat stone world at y = 60, a few chunks wide, with nothing else happening.
    pub(crate) fn flat_world() -> ServerWorld {
        let reg = Arc::new(Registry::builtin());
        let g = Arc::new(Generator::new(1));
        let mut w = ServerWorld::new(reg.clone(), g, 1, 0, None, Vec::new(), ItemRemap::new(&[], &reg));
        for cx in -3..3 {
            for cz in -3..3 {
                let mut c = Chunk::empty(ChunkPos::new(cx, cz));
                for x in 0..16 {
                    for z in 0..16 {
                        c.set(x, 60, z, BlockState::of(id::STONE));
                    }
                }
                w.map.chunks.insert(c.pos, c);
            }
        }
        w
    }

    fn settle(w: &mut ServerWorld, ticks: u64) {
        for _ in 0..ticks {
            w.tick += 1;
            run(w);
        }
    }

    fn at(x: i32, z: i32) -> BlockPos {
        BlockPos::new(x, 61, z)
    }

    #[test]
    fn water_spreads_seven_blocks_and_falls_over_edges() {
        let mut w = flat_world();
        w.set(at(0, 0), BlockState::of(id::WATER));
        settle(&mut w, 200);
        assert_eq!(w.block(at(0, 0)).meta(), 0, "the source stays a source");
        assert_eq!(w.block(at(3, 0)), BlockState::new(id::WATER, 3));
        assert_eq!(w.block(at(7, 0)), BlockState::new(id::WATER, 7));
        assert!(w.block(at(8, 0)).is_air(), "it stops after seven");

        let mut w = flat_world();
        w.set(BlockPos::new(0, 65, 0), BlockState::of(id::WATER));
        settle(&mut w, 100);
        let falling = w.block(BlockPos::new(0, 63, 0));
        assert_eq!(falling.kind(), id::WATER);
        assert_eq!(falling.meta() & 8, 8, "water with nothing under it falls straight down");
    }

    #[test]
    fn water_dries_up_when_its_source_is_gone() {
        let mut w = flat_world();
        w.set(at(0, 0), BlockState::of(id::WATER));
        settle(&mut w, 200);
        w.set(at(0, 0), BlockState::AIR);
        settle(&mut w, 300);
        for x in 0..8 {
            assert!(w.block(at(x, 0)).is_air(), "still water at x = {x}");
        }
    }

    #[test]
    fn two_sources_make_a_third_between_them() {
        let mut w = flat_world();
        w.set(at(0, 0), BlockState::of(id::WATER));
        w.set(at(2, 0), BlockState::of(id::WATER));
        settle(&mut w, 40);
        assert_eq!(w.block(at(1, 0)), BlockState::of(id::WATER));
    }

    #[test]
    fn walls_hold_water_back() {
        let mut w = flat_world();
        for z in -9..=9 {
            w.set(at(2, z), BlockState::of(id::STONE));
        }
        w.set(at(0, 0), BlockState::of(id::WATER));
        settle(&mut w, 200);
        assert_eq!(w.block(at(1, 0)).kind(), id::WATER);
        assert_eq!(w.block(at(3, 0)), BlockState::AIR);
    }

    #[test]
    fn water_washes_away_plants_and_leaves_their_drop() {
        let mut w = flat_world();
        w.set(at(1, 0), BlockState::of(id::POPPY));
        w.events.clear();
        w.set(at(0, 0), BlockState::of(id::WATER));
        settle(&mut w, 30);
        assert_eq!(w.block(at(1, 0)).kind(), id::WATER);
        assert!(w.events.iter().any(|e| matches!(e, WorldEvent::DropItem { .. })));
    }

    #[test]
    fn lava_flows_slower_and_only_three_blocks() {
        let mut w = flat_world();
        w.set(at(0, 0), BlockState::of(id::LAVA));
        settle(&mut w, 10);
        assert!(w.block(at(1, 0)).is_air(), "lava is slow");
        settle(&mut w, 400);
        assert_eq!(w.block(at(3, 0)).kind(), id::LAVA);
        assert!(w.block(at(4, 0)).is_air());
    }

    #[test]
    fn water_meeting_lava_makes_rock() {
        let mut w = flat_world();
        w.set(at(0, 0), BlockState::of(id::LAVA));
        settle(&mut w, 5);
        w.set(at(1, 0), BlockState::of(id::WATER));
        settle(&mut w, 60);
        assert_eq!(w.block(at(0, 0)).kind(), id::OBSIDIAN, "a lava source hardens to obsidian");
    }

    #[test]
    fn sand_falls_and_stone_does_not() {
        let mut w = flat_world();
        let high = BlockPos::new(0, 70, 0);
        w.set(high, BlockState::of(id::SAND));
        w.set(high.offset(Dir::East), BlockState::of(id::STONE));
        w.events.clear();
        settle(&mut w, 5);
        assert!(w.block(high).is_air());
        assert!(matches!(w.events[..], [WorldEvent::Falling { .. }]), "{:?}", w.events);
        assert_eq!(w.block(high.offset(Dir::East)).kind(), id::STONE);
    }

    #[test]
    fn a_torch_comes_off_when_its_wall_goes() {
        let mut w = flat_world();
        let wall = at(5, 5);
        w.set(wall, BlockState::of(id::STONE));
        // On the wall to the north of the torch.
        let torch = wall.offset(Dir::South);
        w.set(torch, BlockState::new(id::TORCH, 1));
        settle(&mut w, 2);
        assert_eq!(w.block(torch).kind(), id::TORCH);
        w.set(wall, BlockState::AIR);
        w.events.clear();
        settle(&mut w, 2);
        assert!(w.block(torch).is_air());
        assert!(w.events.iter().any(|e| matches!(e, WorldEvent::DropItem { .. })), "and leaves its item");
    }

    #[test]
    fn a_flower_needs_ground_under_it() {
        let mut w = flat_world();
        w.set(BlockPos::new(1, 80, 1), BlockState::of(id::POPPY));
        settle(&mut w, 2);
        assert!(w.block(BlockPos::new(1, 80, 1)).is_air());
    }

    #[test]
    fn a_lever_lights_a_lamp_down_a_wire_and_turns_it_off_again() {
        let mut w = flat_world();
        let (lever, lamp) = (at(0, 0), at(5, 0));
        w.set(lever, BlockState::of(id::LEVER));
        for x in 1..5 {
            w.set(at(x, 0), BlockState::of(id::WIRE));
        }
        w.set(lamp, BlockState::of(id::LAMP));
        settle(&mut w, 5);
        assert_eq!(w.block(lamp).kind(), id::LAMP);
        toggle_lever(&mut w, lever);
        settle(&mut w, 8);
        assert_eq!(w.block(at(1, 0)), BlockState::new(id::WIRE, 15));
        assert_eq!(w.block(at(4, 0)), BlockState::new(id::WIRE, 12), "one weaker per block");
        assert_eq!(w.block(lamp).kind(), id::LAMP_LIT);
        toggle_lever(&mut w, lever);
        settle(&mut w, 8);
        assert_eq!(w.block(at(1, 0)), BlockState::new(id::WIRE, 0));
        assert_eq!(w.block(lamp).kind(), id::LAMP);
    }

    #[test]
    fn a_signal_dies_out_after_fifteen_blocks() {
        let mut w = flat_world();
        w.set(at(-1, 0), BlockState::new(id::LEVER, 1));
        for x in 0..20 {
            w.set(at(x, 0), BlockState::of(id::WIRE));
        }
        settle(&mut w, 8);
        assert_eq!(wire_power(w.block(at(14, 0))), 1);
        assert_eq!(wire_power(w.block(at(15, 0))), 0);
    }

    #[test]
    fn an_inverter_turns_off_when_the_block_under_it_is_powered() {
        let mut w = flat_world();
        // Lever, wire, stone with an inverter on top, and a lamp fed by the inverter through a wire.
        let lever = at(0, 0);
        w.set(lever, BlockState::of(id::LEVER));
        w.set(at(1, 0), BlockState::of(id::WIRE));
        let block = at(2, 0);
        w.set(block, BlockState::of(id::STONE));
        let inverter = block.offset(Dir::Up);
        w.set(inverter, BlockState::of(id::INVERTER));
        w.set(at(3, 0), BlockState::of(id::STONE));
        w.set(at(4, 0), BlockState::of(id::STONE));
        w.set(inverter.add(1, 0, 0), BlockState::of(id::WIRE));
        let lamp = inverter.add(2, 0, 0);
        w.set(lamp, BlockState::of(id::LAMP));
        settle(&mut w, 8);
        assert_eq!(w.block(inverter).kind(), id::INVERTER);
        assert_eq!(w.block(lamp).kind(), id::LAMP_LIT, "not (off) is on");
        toggle_lever(&mut w, lever);
        settle(&mut w, 10);
        assert_eq!(w.block(inverter).kind(), id::INVERTER_OFF);
        assert_eq!(w.block(lamp).kind(), id::LAMP, "not (on) is off");
    }

    #[test]
    fn a_powered_charge_primes_itself() {
        let mut w = flat_world();
        let tnt = at(2, 0);
        w.set(tnt, BlockState::of(id::TNT));
        w.set(at(0, 0), BlockState::new(id::LEVER, 1));
        w.set(at(1, 0), BlockState::of(id::WIRE));
        w.events.clear();
        settle(&mut w, 8);
        assert!(w.block(tnt).is_air());
        assert!(w.events.iter().any(|e| matches!(e, WorldEvent::Prime { .. })));
    }

    #[test]
    fn grass_dies_under_stone_and_spreads_to_open_dirt() {
        let mut w = flat_world();
        let (covered, grass, dirt) = (at(0, 0), at(5, 5), at(6, 5));
        w.set(covered, BlockState::of(id::GRASS));
        w.set(covered.offset(Dir::Up), BlockState::of(id::STONE));
        w.set(grass, BlockState::of(id::GRASS));
        w.set(dirt, BlockState::of(id::DIRT));
        for _ in 0..4000 {
            random_tick(&mut w, covered);
            random_tick(&mut w, grass);
            if w.block(dirt).kind() == id::GRASS {
                break;
            }
        }
        assert_eq!(w.block(covered).kind(), id::DIRT);
        assert_eq!(w.block(dirt).kind(), id::GRASS);
    }

    #[test]
    fn leaves_decay_without_a_log_but_not_with_one() {
        let mut w = flat_world();
        let (near, far, log) = (at(0, 0), at(9, 9), at(1, 0));
        w.set(log, BlockState::of(id::OAK_LOG));
        w.set(near, BlockState::of(id::OAK_LEAVES));
        w.set(far, BlockState::of(id::OAK_LEAVES));
        for _ in 0..200 {
            random_tick(&mut w, near);
            random_tick(&mut w, far);
        }
        assert_eq!(w.block(near).kind(), id::OAK_LEAVES);
        assert!(w.block(far).is_air());
    }

    #[test]
    fn fire_takes_what_burns_dies_out_and_never_crosses_bare_stone() {
        let mut w = flat_world();
        let at = |x: i32| BlockPos::new(x, 61, 0);
        // A row of wool, a gap of bare stone, and one block of wool beyond it.
        for x in 0..=4 {
            w.set(at(x), BlockState::of(id::WOOL_WHITE));
        }
        w.set(at(9), BlockState::of(id::WOOL_WHITE));
        w.set(at(0).offset(Dir::Up), BlockState::of(id::FIRE));
        let mut burning = false;
        for _ in 0..6000 {
            w.tick += 1;
            run(&mut w);
            burning |= (1..=4).any(|x| w.block(at(x)).kind() == id::FIRE);
        }
        assert!(burning, "it ran along the row");
        let left = (0..=4).filter(|&x| w.block(at(x)).kind() == id::WOOL_WHITE).count();
        assert!(left <= 1, "{left} of five blocks of wool are left");
        assert_eq!(w.block(at(9)).kind(), id::WOOL_WHITE, "the far one was out of reach");
        let fires = (-3..14)
            .flat_map(|x| (60..66).map(move |y| (x, y)))
            .filter(|&(x, y)| (-2..=2).any(|z| w.block(BlockPos::new(x, y, z)).kind() == id::FIRE));
        assert_eq!(fires.count(), 0, "and it has all gone out");
        // On bare stone a fire burns a while, and goes out by itself.
        w.set(BlockPos::new(-6, 61, 5), BlockState::of(id::FIRE));
        for _ in 0..2000 {
            w.tick += 1;
            run(&mut w);
        }
        assert!(w.block(BlockPos::new(-6, 61, 5)).is_air());
    }

    #[test]
    fn a_sapling_grows_into_a_tree() {
        let mut w = flat_world();
        let p = at(4, 4);
        w.set(p, BlockState::of(id::SAPLING));
        for _ in 0..2000 {
            random_tick(&mut w, p);
            if w.block(p).kind() != id::SAPLING {
                break;
            }
        }
        let k = w.block(p).kind();
        assert!(matches!(k, id::OAK_LOG | id::BIRCH_LOG | id::PINE_LOG), "{k}");
        assert!(w.block(p.add(0, 2, 0)).kind() == k);
    }

    #[test]
    fn breaking_a_chest_spills_its_contents() {
        let mut w = flat_world();
        let p = at(3, 3);
        w.set(p, BlockState::of(id::CHEST));
        let dirt = w.reg.item_by_name("dirt").expect("dirt");
        *w.chest(p) = vec![Some(ItemStack::new(dirt, 5)); 2];
        w.events.clear();
        w.set(p, BlockState::AIR);
        let drops = w.events.iter().filter(|e| matches!(e, WorldEvent::DropItem { .. })).count();
        assert_eq!(drops, 2);
        assert!(w.block_entities.is_empty());
    }

    #[test]
    fn a_generated_chest_always_holds_the_same_treasure() {
        let mut a = flat_world();
        let mut b = flat_world();
        let p = at(1, 1);
        assert_eq!(a.chest(p).clone(), b.chest(p).clone());
        assert!(a.chest(p).iter().flatten().count() > 0, "treasure chests are not empty");
        let _ = DVec3::ZERO;
    }

    #[test]
    fn a_flood_cannot_stall_a_tick() {
        let mut w = flat_world();
        for x in -40..40 {
            for z in -40..40 {
                w.set(at(x, z), BlockState::of(id::WATER));
            }
        }
        let started = std::time::Instant::now();
        w.tick += 1;
        run(&mut w);
        assert!(started.elapsed().as_millis() < 1000, "a tick took {:?}", started.elapsed());
        assert!(w.updates.len() + w.scheduled_len() > 0, "work was left for later ticks");
    }
}
