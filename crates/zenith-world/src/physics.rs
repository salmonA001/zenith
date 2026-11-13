//! How bodies move, one fixed tick at a time.
//!
//! The server runs this for every player from their inputs, and the client runs the same code to
//! predict its own player. Nothing here reads a clock or a platform-dependent float function, so
//! the two agree bit for bit as long as they agree on the blocks.
use crate::chunk::BlockAccess;
use serde::{Deserialize, Serialize};
use zenith_content::{Fluid, Registry};
use zenith_core::glam::DVec3;
use zenith_core::{Aabb, BlockPos, dmath};

pub const PLAYER_WIDTH: f64 = 0.6;
pub const PLAYER_HEIGHT: f64 = 1.8;
pub const EYE_HEIGHT: f64 = 1.62;
const STEP_UP: f64 = 0.6;
const GRAVITY: f64 = 0.08;
const AIR_DRAG: f64 = 0.98;
const JUMP: f64 = 0.42;
/// How much of its speed a body keeps each tick on the ground, and in the air.
const GROUND_GRIP: f64 = 0.546;
const AIR_GRIP: f64 = 0.91;
const WALK_ACCEL: f64 = 0.098;
const SPRINT_ACCEL: f64 = 0.1274;
const SNEAK_ACCEL: f64 = 0.03;
const AIR_ACCEL: f64 = 0.02;
const FLY_ACCEL: f64 = 0.1;
const FLY_GRIP: f64 = 0.6;
const FLY_VERTICAL: f64 = 0.375;
const WATER_GRIP: f64 = 0.8;
const WATER_ACCEL: f64 = 0.02;
const CLIMB_SPEED: f64 = 0.15;
/// Bodies stop falling at this speed.
const TERMINAL: f64 = 3.92;

/// The buttons a player holds in one tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Keys(pub u8);

impl Keys {
    pub const FORWARD: u8 = 1;
    pub const BACK: u8 = 2;
    pub const LEFT: u8 = 4;
    pub const RIGHT: u8 = 8;
    pub const JUMP: u8 = 16;
    pub const SNEAK: u8 = 32;
    pub const SPRINT: u8 = 64;
    #[inline]
    pub const fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

/// One tick of what a player wants. Angles are `dmath` turns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveInput {
    pub keys: Keys,
    pub yaw: u16,
    pub pitch: i16,
}

impl MoveInput {
    /// Pitch as a turn, positive looking down. Clamped so a client cannot ask for a backflip.
    pub fn pitch_turn(self) -> u16 {
        self.pitch.clamp(-16384, 16384) as u16
    }

    /// Where the player is looking, as a unit vector.
    pub fn look(self) -> DVec3 {
        let (sy, cy) = dmath::sin_cos(self.yaw);
        let (sp, cp) = dmath::sin_cos(self.pitch_turn());
        DVec3::new(-sy * cp, -sp, cy * cp)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Body {
    pub pos: DVec3,
    pub vel: DVec3,
    pub on_ground: bool,
    pub flying: bool,
    pub in_water: bool,
    pub eyes_in_water: bool,
    pub in_lava: bool,
    pub climbing: bool,
    /// How far this body has been falling.
    pub fall: f64,
}

/// What happened during a step that the caller may care about.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StepResult {
    /// The distance fallen, when this step ended a fall.
    pub landed_after: Option<f64>,
    pub moved: f64,
}

impl Body {
    pub fn at(pos: DVec3) -> Self {
        Self { pos, ..Default::default() }
    }
    pub fn aabb(&self, width: f64, height: f64) -> Aabb {
        Aabb::standing(self.pos, width, height)
    }
    pub fn eye(&self) -> DVec3 {
        self.pos + DVec3::Y * EYE_HEIGHT
    }
}

/// Every collision box a move of `area` could touch.
fn gather(w: &impl BlockAccess, reg: &Registry, area: &Aabb, out: &mut Vec<Aabb>) {
    out.clear();
    let min = BlockPos::from_vec(area.min);
    let max = BlockPos::from_vec(area.max);
    for y in (min.y - 1).max(zenith_core::pos::MIN_Y)..=max.y.min(zenith_core::pos::MAX_Y) {
        for z in min.z..=max.z {
            for x in min.x..=max.x {
                let p = BlockPos::new(x, y, z);
                let state = w.block(p);
                if !reg.lut.collides[state.kind() as usize] {
                    continue;
                }
                let origin = DVec3::new(x as f64, y as f64, z as f64);
                for b in reg.block(state).shape.collision(state.meta()) {
                    out.push(Aabb::new(
                        origin + DVec3::new(b[0][0] as f64, b[0][1] as f64, b[0][2] as f64) / 16.0,
                        origin + DVec3::new(b[1][0] as f64, b[1][1] as f64, b[1][2] as f64) / 16.0,
                    ));
                }
            }
        }
    }
}

/// Moves a box by `delta`, stopping at what is in the way. Returns the move actually made.
fn slide(boxes: &[Aabb], mut b: Aabb, delta: DVec3) -> DVec3 {
    let mut dy = delta.y;
    for o in boxes {
        dy = b.clip_y(o, dy);
    }
    b = b.offset(DVec3::new(0.0, dy, 0.0));
    let mut dx = delta.x;
    let mut dz = delta.z;
    // Whichever axis moves further goes first, so sliding along a corner does not catch on it.
    if dx.abs() >= dz.abs() {
        for o in boxes {
            dx = b.clip_x(o, dx);
        }
        b = b.offset(DVec3::new(dx, 0.0, 0.0));
        for o in boxes {
            dz = b.clip_z(o, dz);
        }
    } else {
        for o in boxes {
            dz = b.clip_z(o, dz);
        }
        b = b.offset(DVec3::new(0.0, 0.0, dz));
        for o in boxes {
            dx = b.clip_x(o, dx);
        }
    }
    DVec3::new(dx, dy, dz)
}

/// Whether a body this big could stand with its feet at `feet` without being inside anything solid.
pub fn fits(w: &impl BlockAccess, reg: &Registry, feet: DVec3, width: f64, height: f64, boxes: &mut Vec<Aabb>) -> bool {
    let b = Aabb::standing(feet, width, height);
    gather(w, reg, &b, boxes);
    !boxes.iter().any(|o| o.intersects(&b))
}

/// Moves a body of any size by `delta` through the world. Used by everything that moves.
pub fn move_body(
    w: &impl BlockAccess,
    reg: &Registry,
    body: &mut Body,
    delta: DVec3,
    width: f64,
    height: f64,
    can_step: bool,
    boxes: &mut Vec<Aabb>,
) {
    let b = body.aabb(width, height);
    gather(w, reg, &b.swept(delta).inflate(STEP_UP + 0.01), boxes);
    let mut applied = slide(boxes, b, delta);

    let blocked_side = applied.x != delta.x || applied.z != delta.z;
    let falling_onto_ground = delta.y <= 0.0 && applied.y != delta.y;
    if can_step && blocked_side && (body.on_ground || falling_onto_ground) {
        // Try again from up to a small step higher, and keep whichever got further.
        let up = slide(boxes, b, DVec3::new(0.0, STEP_UP, 0.0)).y;
        let raised = b.offset(DVec3::new(0.0, up, 0.0));
        let across = slide(boxes, raised, DVec3::new(delta.x, 0.0, delta.z));
        let settled = raised.offset(across);
        let down = slide(boxes, settled, DVec3::new(0.0, -up + delta.y.min(0.0), 0.0)).y;
        let stepped = DVec3::new(across.x, up + down, across.z);
        if stepped.x * stepped.x + stepped.z * stepped.z > applied.x * applied.x + applied.z * applied.z {
            applied = stepped;
        }
    }

    body.pos += applied;
    let hit_floor = delta.y < 0.0 && applied.y > delta.y && (applied.y - delta.y).abs() > 1e-9;
    body.on_ground = hit_floor;
    if applied.x != delta.x {
        body.vel.x = 0.0;
    }
    if applied.z != delta.z {
        body.vel.z = 0.0;
    }
    if (applied.y - delta.y).abs() > 1e-9 && !(can_step && applied.y > 0.0 && delta.y <= 0.0) {
        body.vel.y = 0.0;
    }
}

/// Refreshes what a body knows about the fluids and ladders it is in.
pub fn sense(w: &impl BlockAccess, reg: &Registry, body: &mut Body, height: f64) {
    let feet = BlockPos::from_vec(body.pos + DVec3::Y * 0.1);
    let middle = BlockPos::from_vec(body.pos + DVec3::Y * (height * 0.45));
    let eye = BlockPos::from_vec(body.pos + DVec3::Y * (height * 0.9));
    let (f, m, e) = (reg.block(w.block(feet)), reg.block(w.block(middle)), reg.block(w.block(eye)));
    body.in_water = f.fluid == Fluid::Water || m.fluid == Fluid::Water;
    body.eyes_in_water = e.fluid == Fluid::Water;
    body.in_lava = f.fluid == Fluid::Lava || m.fluid == Fluid::Lava;
    body.climbing = f.climbable || m.climbable;
}

/// One tick of a player. `boxes` is a buffer kept between calls.
pub fn step(
    body: &mut Body,
    input: MoveInput,
    w: &impl BlockAccess,
    reg: &Registry,
    boxes: &mut Vec<Aabb>,
) -> StepResult {
    // No ground has arrived under this body yet: hold still until it does.
    let here = BlockPos::from_vec(body.pos);
    if !w.is_loaded(here) || !w.is_loaded(here.add(1, 0, 1)) || !w.is_loaded(here.add(-1, 0, -1)) {
        return StepResult::default();
    }
    sense(w, reg, body, PLAYER_HEIGHT);

    let keys = input.keys;
    let forward = i32::from(keys.has(Keys::FORWARD)) - i32::from(keys.has(Keys::BACK));
    let strafe = i32::from(keys.has(Keys::RIGHT)) - i32::from(keys.has(Keys::LEFT));
    let (sy, cy) = dmath::sin_cos(input.yaw);
    let (fx, fz) = (-sy, cy);
    let (rx, rz) = (-cy, -sy);
    let mut wish = DVec3::new(fx * forward as f64 + rx * strafe as f64, 0.0, fz * forward as f64 + rz * strafe as f64);
    let length2 = wish.x * wish.x + wish.z * wish.z;
    if length2 > 1.0 {
        // Pressing two keys is no faster than one. The only two lengths here are 1 and sqrt 2.
        wish *= std::f64::consts::FRAC_1_SQRT_2;
    }

    let start = body.pos;
    if body.flying {
        body.vel.x = body.vel.x * FLY_GRIP + wish.x * FLY_ACCEL * if keys.has(Keys::SPRINT) { 2.2 } else { 1.0 };
        body.vel.z = body.vel.z * FLY_GRIP + wish.z * FLY_ACCEL * if keys.has(Keys::SPRINT) { 2.2 } else { 1.0 };
        let up = i32::from(keys.has(Keys::JUMP)) - i32::from(keys.has(Keys::SNEAK));
        body.vel.y = body.vel.y * FLY_GRIP + up as f64 * FLY_VERTICAL * (1.0 - FLY_GRIP);
        let delta = body.vel;
        move_body(w, reg, body, delta, PLAYER_WIDTH, PLAYER_HEIGHT, false, boxes);
        body.fall = 0.0;
        return StepResult { landed_after: None, moved: (body.pos - start).length() };
    }

    if body.in_water || body.in_lava {
        let grip = if body.in_lava { 0.5 } else { WATER_GRIP };
        body.vel.x = body.vel.x * grip + wish.x * WATER_ACCEL * 2.0;
        body.vel.z = body.vel.z * grip + wish.z * WATER_ACCEL * 2.0;
        body.vel.y = body.vel.y * grip - GRAVITY * 0.25;
        if keys.has(Keys::JUMP) {
            body.vel.y += 0.05;
        }
        if keys.has(Keys::SNEAK) {
            body.vel.y -= 0.03;
        }
        let delta = body.vel;
        move_body(w, reg, body, delta, PLAYER_WIDTH, PLAYER_HEIGHT, true, boxes);
        // Climbing out over the edge of a pool.
        if keys.has(Keys::JUMP)
            && (body.pos.x != start.x + delta.x || body.pos.z != start.z + delta.z)
            && !body.eyes_in_water
        {
            body.vel.y = 0.3;
        }
        body.fall = 0.0;
        return StepResult { landed_after: None, moved: (body.pos - start).length() };
    }

    let accel = if body.on_ground {
        if keys.has(Keys::SNEAK) {
            SNEAK_ACCEL
        } else if keys.has(Keys::SPRINT) && forward > 0 {
            SPRINT_ACCEL
        } else {
            WALK_ACCEL
        }
    } else {
        AIR_ACCEL
    };
    let grip = if body.on_ground { GROUND_GRIP } else { AIR_GRIP };
    let mut landed_after = None;
    if keys.has(Keys::JUMP) && body.on_ground && !body.climbing {
        body.vel.y = JUMP;
        body.on_ground = false;
    }
    let mut delta = DVec3::new(body.vel.x * grip + wish.x * accel, body.vel.y, body.vel.z * grip + wish.z * accel);
    body.vel.x = delta.x;
    body.vel.z = delta.z;

    if body.climbing {
        let climbing_up = keys.has(Keys::JUMP) || forward != 0;
        body.vel.y = if climbing_up {
            CLIMB_SPEED
        } else if keys.has(Keys::SNEAK) {
            0.0
        } else {
            -CLIMB_SPEED
        };
        delta.y = body.vel.y;
        delta.x = delta.x.clamp(-0.15, 0.15);
        delta.z = delta.z.clamp(-0.15, 0.15);
        body.fall = 0.0;
    }

    // Sneaking at a ledge stops you at it, as long as you are standing on it.
    if keys.has(Keys::SNEAK) && body.on_ground {
        gather(w, reg, &body.aabb(PLAYER_WIDTH, PLAYER_HEIGHT).inflate(1.5), boxes);
        let support = |dx: f64, dz: f64| -> bool {
            let shifted = body.aabb(PLAYER_WIDTH, PLAYER_HEIGHT).offset(DVec3::new(dx, -STEP_UP, dz));
            boxes.iter().any(|b| shifted.intersects(b))
        };
        while delta.x != 0.0 && !support(delta.x, 0.0) {
            delta.x = shrink(delta.x);
        }
        while delta.z != 0.0 && !support(0.0, delta.z) {
            delta.z = shrink(delta.z);
        }
        while delta.x != 0.0 && delta.z != 0.0 && !support(delta.x, delta.z) {
            delta.x = shrink(delta.x);
            delta.z = shrink(delta.z);
        }
    }

    let was_falling = body.vel.y < 0.0;
    let delta_y = delta.y;
    move_body(w, reg, body, delta, PLAYER_WIDTH, PLAYER_HEIGHT, true, boxes);
    if !body.climbing {
        body.vel.y = ((body.vel.y - GRAVITY) * AIR_DRAG).max(-TERMINAL);
    }
    if body.on_ground {
        if was_falling && body.fall > 0.0 {
            landed_after = Some(body.fall);
        }
        body.fall = 0.0;
        body.vel.y = -GRAVITY * AIR_DRAG;
    } else if delta_y < 0.0 && !body.climbing {
        body.fall += -delta_y;
    }
    StepResult { landed_after, moved: (body.pos - start).length() }
}

/// Moves a ledge-guard step towards zero, and to exactly zero at the end.
fn shrink(v: f64) -> f64 {
    const STEP: f64 = 0.05;
    if v.abs() < STEP {
        0.0
    } else if v > 0.0 {
        v - STEP
    } else {
        v + STEP
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunk, ChunkMap};
    use zenith_content::{BlockState, id};
    use zenith_core::ChunkPos;

    fn flat(y: i32) -> ChunkMap {
        let mut map = ChunkMap::default();
        for cx in -3..3 {
            for cz in -3..3 {
                let mut c = Chunk::empty(ChunkPos::new(cx, cz));
                for x in 0..16 {
                    for z in 0..16 {
                        c.set(x, y, z, BlockState::of(id::STONE));
                    }
                }
                map.chunks.insert(c.pos, c);
            }
        }
        map
    }

    fn run(body: &mut Body, input: MoveInput, w: &ChunkMap, ticks: u32) {
        let reg = Registry::builtin();
        let mut boxes = Vec::new();
        for _ in 0..ticks {
            step(body, input, w, &reg, &mut boxes);
        }
    }

    fn walk(keys: u8) -> MoveInput {
        MoveInput { keys: Keys(keys), yaw: 0, pitch: 0 }
    }

    #[test]
    fn a_falling_body_lands_on_the_ground_and_stays_there() {
        let w = flat(60);
        let mut b = Body::at(DVec3::new(0.5, 70.0, 0.5));
        run(&mut b, walk(0), &w, 60);
        assert_eq!(b.pos.y, 61.0);
        assert!(b.on_ground);
    }

    #[test]
    fn a_body_over_a_chunk_that_has_not_arrived_holds_still() {
        let w = ChunkMap::default();
        let mut b = Body::at(DVec3::new(0.5, 70.0, 0.5));
        run(&mut b, walk(Keys::FORWARD), &w, 20);
        assert_eq!(b.pos, DVec3::new(0.5, 70.0, 0.5));
    }

    #[test]
    fn walking_reaches_about_four_blocks_a_second_and_sprinting_more() {
        let w = flat(60);
        let speed = |keys: u8| {
            let mut b = Body::at(DVec3::new(0.5, 61.0, -40.0));
            run(&mut b, walk(0), &w, 2);
            let z0 = b.pos.z;
            run(&mut b, walk(keys), &w, 40);
            (b.pos.z - z0) / 2.0
        };
        let (walking, sprinting) = (speed(Keys::FORWARD), speed(Keys::FORWARD | Keys::SPRINT));
        assert!((3.8..4.6).contains(&walking), "{walking} blocks/s");
        assert!((4.9..6.0).contains(&sprinting), "{sprinting} blocks/s");
    }

    #[test]
    fn strafing_diagonally_is_no_faster_than_walking_straight() {
        let w = flat(60);
        let distance = |keys: u8| {
            let mut b = Body::at(DVec3::new(0.5, 61.0, 0.5));
            run(&mut b, walk(0), &w, 2);
            let start = b.pos;
            run(&mut b, walk(keys), &w, 40);
            (b.pos - start).length()
        };
        let (straight, diagonal) = (distance(Keys::FORWARD), distance(Keys::FORWARD | Keys::LEFT));
        assert!((straight - diagonal).abs() < 0.01, "{straight} vs {diagonal}");
    }

    #[test]
    fn yaw_decides_which_way_forward_is() {
        let w = flat(60);
        let mut b = Body::at(DVec3::new(0.5, 61.0, 0.5));
        // A quarter turn to the right of +z is -x.
        run(&mut b, MoveInput { keys: Keys(Keys::FORWARD), yaw: 16384, pitch: 0 }, &w, 20);
        assert!(b.pos.x < -2.0, "{:?}", b.pos);
        assert!((b.pos.z - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_wall_stops_a_body_flush_against_it() {
        let mut w = flat(60);
        for y in 61..64 {
            for x in -5..6 {
                w.set(BlockPos::new(x, y, 5), BlockState::of(id::STONE));
            }
        }
        let mut b = Body::at(DVec3::new(0.5, 61.0, 0.5));
        run(&mut b, walk(Keys::FORWARD), &w, 60);
        assert!((b.pos.z - (5.0 - PLAYER_WIDTH / 2.0)).abs() < 1e-9, "{:?}", b.pos);
    }

    #[test]
    fn a_body_walks_up_a_slab_but_not_a_full_block() {
        let mut w = flat(60);
        w.set(BlockPos::new(0, 61, 3), BlockState::of(id::STONE_SLAB));
        let mut b = Body::at(DVec3::new(0.5, 61.0, 0.5));
        run(&mut b, walk(Keys::FORWARD), &w, 15);
        assert!(b.pos.z > 3.0 && b.pos.y >= 61.5, "{:?}", b.pos);

        let mut w = flat(60);
        for x in -3..4 {
            w.set(BlockPos::new(x, 61, 3), BlockState::of(id::STONE));
        }
        let mut b = Body::at(DVec3::new(0.5, 61.0, 0.5));
        run(&mut b, walk(Keys::FORWARD), &w, 30);
        assert!(b.pos.z < 3.0 && b.pos.y == 61.0, "{:?}", b.pos);
    }

    #[test]
    fn a_jump_clears_one_block_and_falls_back() {
        let w = flat(60);
        let mut b = Body::at(DVec3::new(0.5, 61.0, 0.5));
        run(&mut b, walk(0), &w, 2);
        let reg = Registry::builtin();
        let mut boxes = Vec::new();
        step(&mut b, walk(Keys::JUMP), &w, &reg, &mut boxes);
        let mut highest = b.pos.y;
        for _ in 0..30 {
            step(&mut b, walk(0), &w, &reg, &mut boxes);
            highest = highest.max(b.pos.y);
        }
        assert!((62.0..62.4).contains(&highest), "peak {highest}");
        assert_eq!(b.pos.y, 61.0);
    }

    #[test]
    fn landing_reports_how_far_the_body_fell() {
        let w = flat(60);
        let mut b = Body::at(DVec3::new(0.5, 81.0, 0.5));
        let reg = Registry::builtin();
        let mut boxes = Vec::new();
        let mut landed = None;
        for _ in 0..80 {
            if let Some(d) = step(&mut b, walk(0), &w, &reg, &mut boxes).landed_after {
                landed = Some(d);
            }
        }
        let d = landed.expect("it landed");
        assert!((18.0..21.0).contains(&d), "fell {d}");
    }

    #[test]
    fn sneaking_stops_at_the_edge_of_a_ledge() {
        let mut w = ChunkMap::default();
        for cx in -2..2 {
            for cz in -2..2 {
                w.chunks.insert(ChunkPos::new(cx, cz), Chunk::empty(ChunkPos::new(cx, cz)));
            }
        }
        for x in -2..3 {
            for z in -2..3 {
                w.set(BlockPos::new(x, 60, z), BlockState::of(id::STONE));
            }
        }
        let mut b = Body::at(DVec3::new(0.5, 61.0, 0.5));
        run(&mut b, walk(0), &w, 2);
        run(&mut b, walk(Keys::FORWARD | Keys::SNEAK), &w, 200);
        assert_eq!(b.pos.y, 61.0, "fell off at {:?}", b.pos);
        assert!(b.pos.z > 2.5, "stopped too early at {:?}", b.pos);
    }

    #[test]
    fn water_slows_and_floats_a_body() {
        let mut w = flat(50);
        for y in 51..=60 {
            for x in -3..4 {
                for z in -3..4 {
                    w.set(BlockPos::new(x, y, z), BlockState::of(id::WATER));
                }
            }
        }
        let mut sinking = Body::at(DVec3::new(0.5, 60.0, 0.5));
        run(&mut sinking, walk(0), &w, 15);
        assert!(sinking.in_water && sinking.vel.y > -0.2, "{:?}", sinking);
        let mut swimming = Body::at(DVec3::new(0.5, 53.0, 0.5));
        run(&mut swimming, walk(Keys::JUMP), &w, 40);
        assert!(swimming.pos.y > 56.0, "{:?}", swimming.pos);
    }

    #[test]
    fn ladders_climb_when_pushed_and_slide_when_not() {
        let mut w = flat(60);
        for y in 61..70 {
            w.set(BlockPos::new(0, y, 1), BlockState::new(id::LADDER, 1));
        }
        let mut b = Body::at(DVec3::new(0.5, 61.0, 1.5));
        run(&mut b, walk(Keys::FORWARD), &w, 30);
        assert!(b.pos.y > 64.0, "climbed only to {:?}", b.pos);
        let top = b.pos.y;
        run(&mut b, walk(0), &w, 10);
        assert!(b.pos.y < top, "did not slide down");
        assert_eq!(b.fall, 0.0);
    }

    #[test]
    fn flying_ignores_gravity() {
        let w = flat(60);
        let mut b = Body::at(DVec3::new(0.5, 70.0, 0.5));
        b.flying = true;
        run(&mut b, walk(0), &w, 40);
        assert_eq!(b.pos.y, 70.0);
        run(&mut b, walk(Keys::JUMP), &w, 40);
        assert!(b.pos.y > 74.0, "{:?}", b.pos);
    }

    #[test]
    fn same_inputs_make_exactly_the_same_path() {
        let g = crate::Generator::new(5);
        let (sx, sy, sz) = g.find_spawn();
        let mut w = ChunkMap::default();
        for cx in (sx >> 4) - 3..=(sx >> 4) + 3 {
            for cz in (sz >> 4) - 3..=(sz >> 4) + 3 {
                w.chunks.insert(ChunkPos::new(cx, cz), g.chunk(ChunkPos::new(cx, cz)));
            }
        }
        let mut rng = zenith_core::Rng::new(1);
        let inputs: Vec<MoveInput> = (0..400)
            .map(|_| MoveInput { keys: Keys(rng.below(128) as u8), yaw: rng.next_u64() as u16, pitch: 0 })
            .collect();
        let reg = Registry::builtin();
        let simulate = || {
            let mut b = Body::at(DVec3::new(sx as f64 + 0.5, sy as f64 + 3.0, sz as f64 + 0.5));
            let mut boxes = Vec::new();
            for &i in &inputs {
                step(&mut b, i, &w, &reg, &mut boxes);
            }
            (b.pos.x.to_bits(), b.pos.y.to_bits(), b.pos.z.to_bits())
        };
        assert_eq!(simulate(), simulate());
    }
}
