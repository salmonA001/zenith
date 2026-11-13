//! Rails, and the way a cart runs along them: which way a rail goes, how rails join up as they are
//! laid, and the path through each one.
use zenith_content::id;
use zenith_core::glam::DVec3;
use zenith_core::{BlockPos, Dir};
use zenith_world::BlockAccess;

/// How many shapes a rail has, by meta: straight north-south and east-west; rising to the east,
/// west, north and south; then curves joining south and east, south and west, north and west,
/// north and east.
pub const SHAPES: u8 = 10;
/// How far above the bottom of its cell a cart's wheels run.
pub const HEIGHT: f64 = 1.0 / 16.0;
const SIDES: [Dir; 4] = [Dir::North, Dir::South, Dir::West, Dir::East];

/// An end of a rail: the side of its cell it leads out of, and whether that end is a block up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct End {
    pub side: Dir,
    pub up: bool,
}

const fn end(side: Dir, up: bool) -> End {
    End { side, up }
}

/// A rail's two ends, by its shape.
pub const fn ends(meta: u8) -> [End; 2] {
    match meta {
        0 => [end(Dir::North, false), end(Dir::South, false)],
        1 => [end(Dir::West, false), end(Dir::East, false)],
        2 => [end(Dir::West, false), end(Dir::East, true)],
        3 => [end(Dir::East, false), end(Dir::West, true)],
        4 => [end(Dir::South, false), end(Dir::North, true)],
        5 => [end(Dir::North, false), end(Dir::South, true)],
        6 => [end(Dir::South, false), end(Dir::East, false)],
        7 => [end(Dir::South, false), end(Dir::West, false)],
        8 => [end(Dir::North, false), end(Dir::West, false)],
        _ => [end(Dir::North, false), end(Dir::East, false)],
    }
}

pub const fn is_rail(kind: u16) -> bool {
    matches!(kind, id::RAIL | id::BOOST_RAIL)
}

/// Where an end is in its cell: the middle of its side, at the height the wheels run.
fn end_point(e: End) -> DVec3 {
    let h = if e.up { 1.0 } else { 0.0 } + HEIGHT;
    match e.side {
        Dir::North => DVec3::new(0.5, h, 0.0),
        Dir::South => DVec3::new(0.5, h, 1.0),
        Dir::West => DVec3::new(0.0, h, 0.5),
        _ => DVec3::new(1.0, h, 0.5),
    }
}

fn is_curve(from: End, to: End) -> bool {
    from.side != to.side.opposite()
}

/// The point `t` of the way along a rail, from one end to the other, within its cell. A curve is
/// a quarter circle about the corner its two sides share.
pub fn point(from: End, to: End, t: f64) -> DVec3 {
    let (a, b) = (end_point(from), end_point(to));
    if !is_curve(from, to) {
        return a.lerp(b, t);
    }
    let corner = DVec3::new(if a.x == 0.5 { b.x } else { a.x }, a.y, if a.z == 0.5 { b.z } else { a.z });
    let angle = t * std::f64::consts::FRAC_PI_2;
    corner + (a - corner) * angle.cos() + (b - corner) * angle.sin()
}

/// How long the way through a rail is: a curve is shorter, a slope longer.
pub fn length(from: End, to: End) -> f64 {
    if is_curve(from, to) {
        std::f64::consts::FRAC_PI_4
    } else if from.up != to.up {
        std::f64::consts::SQRT_2
    } else {
        1.0
    }
}

/// Which way a cart is heading at `t` along a rail, as a flat direction.
pub fn heading(from: End, to: End, t: f64) -> DVec3 {
    let ahead = point(from, to, (t + 0.05).min(1.0)) - point(from, to, (t - 0.05).max(0.0));
    DVec3::new(ahead.x, 0.0, ahead.z).normalize_or_zero()
}

/// Where a cart leaving `cell` by `out` goes on: the rail beyond that leads back to it, and which of
/// its ends the cart comes in by. A rail a block lower that rises to meet it counts.
pub fn next(w: &impl BlockAccess, cell: BlockPos, out: End) -> Option<(BlockPos, u8, usize)> {
    let height = cell.y + i32::from(out.up);
    let beyond = cell.offset(out.side);
    for y in [height, height - 1] {
        let c = BlockPos::new(beyond.x, y, beyond.z);
        let state = w.block(c);
        if !is_rail(state.kind()) {
            continue;
        }
        let meta = state.meta() % SHAPES;
        for (i, e) in ends(meta).into_iter().enumerate() {
            if e.side == out.side.opposite() && c.y + i32::from(e.up) == height {
                return Some((c, meta, i));
            }
        }
    }
    None
}

/// The shape for a rail at `p`, from the rails around it: up a step toward a rail a block higher,
/// else straight or round a corner to whichever sides have rails. With nothing around, it keeps
/// the shape it has.
pub fn shape_for(w: &impl BlockAccess, p: BlockPos, current: u8) -> u8 {
    let rail_at = |q: BlockPos| is_rail(w.block(q).kind());
    let mut joins = [false; 4];
    let mut rises = [false; 4];
    for (i, side) in SIDES.into_iter().enumerate() {
        let beside = p.offset(side);
        rises[i] = rail_at(beside.offset(Dir::Up));
        joins[i] = rises[i] || rail_at(beside) || rail_at(beside.offset(Dir::Down));
    }
    match rises {
        [_, _, _, true] => return 2,
        [_, _, true, _] => return 3,
        [true, _, _, _] => return 4,
        [_, true, _, _] => return 5,
        _ => {}
    }
    let [north, south, west, east] = joins;
    match (north || south, west || east) {
        (false, false) => current % SHAPES,
        (true, false) => 0,
        (false, true) => 1,
        _ if north && south => 0,
        _ if west && east => 1,
        _ => match (north, east) {
            (false, true) => 6,
            (false, false) => 7,
            (true, false) => 8,
            (true, true) => 9,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::tests::flat_world;
    use zenith_content::BlockState;

    #[test]
    fn every_rail_runs_from_end_to_end_without_a_gap() {
        for meta in 0..SHAPES {
            let [a, b] = ends(meta);
            assert_eq!(point(a, b, 0.0), end_point(a), "{meta}");
            assert!((point(a, b, 1.0) - end_point(b)).length() < 1e-9, "{meta}");
            // A quarter of the way round a curve is a quarter of a circle, half a block from its corner.
            let mid = point(a, b, 0.5);
            assert!((0.0..=1.0).contains(&mid.x) && (0.0..=1.0).contains(&mid.z), "{meta}: {mid}");
            assert!(heading(a, b, 0.5).length() > 0.99);
        }
        let [a, b] = ends(6);
        let corner = DVec3::new(1.0, HEIGHT, 1.0);
        assert!(((point(a, b, 0.37) - corner).length() - 0.5).abs() < 1e-9, "a curve keeps its radius");
    }

    #[test]
    fn a_cart_leaving_one_rail_finds_the_next_level_up_and_down() {
        let mut w = flat_world();
        let rail = |w: &mut crate::world::ServerWorld, x: i32, y: i32, meta: u8| {
            w.set(BlockPos::new(x, y, 0), BlockState::new(id::RAIL, meta));
        };
        // East along the floor, up a step, and on along the top.
        rail(&mut w, 0, 61, 1);
        rail(&mut w, 1, 61, 2);
        rail(&mut w, 2, 62, 1);
        let east = ends(1)[1];
        let (c, meta, i) = next(&w, BlockPos::new(0, 61, 0), east).expect("the slope");
        assert_eq!((c, meta, ends(meta)[i]), (BlockPos::new(1, 61, 0), 2, end(Dir::West, false)));
        let up = ends(2)[1];
        let (c, _, i) = next(&w, BlockPos::new(1, 61, 0), up).expect("the top");
        assert_eq!((c, i), (BlockPos::new(2, 62, 0), 0));
        // And back west, down the slope to the floor.
        let (c, meta, i) = next(&w, BlockPos::new(2, 62, 0), ends(1)[0]).expect("down onto the slope");
        assert_eq!((c, meta, ends(meta)[i]), (BlockPos::new(1, 61, 0), 2, end(Dir::East, true)));
        assert!(next(&w, BlockPos::new(2, 62, 0), east).is_none(), "the end of the line");
    }

    #[test]
    fn rails_take_the_shape_of_those_around_them() {
        let mut w = flat_world();
        let at = BlockPos::new(0, 61, 0);
        let lay = |w: &mut crate::world::ServerWorld, x: i32, y: i32, z: i32| {
            w.set(BlockPos::new(x, y, z), BlockState::of(id::RAIL));
        };
        assert_eq!(shape_for(&w, at, 1), 1, "alone, as it was");
        lay(&mut w, 0, 61, 1);
        assert_eq!(shape_for(&w, at, 1), 0, "a rail to the south: north-south");
        lay(&mut w, 1, 61, 0);
        assert_eq!(shape_for(&w, at, 0), 6, "south and east: round the corner");
        lay(&mut w, 0, 61, -1);
        assert_eq!(shape_for(&w, at, 6), 0, "straight on wins at a junction");
        lay(&mut w, -1, 62, 0);
        assert_eq!(shape_for(&w, at, 0), 3, "up a step to the west");
    }
}
