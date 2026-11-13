//! Block states and block definitions.
use serde::{Deserialize, Serialize};

/// What one cell of the world holds: a kind in the high 12 bits, its state in the low 4.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockState(pub u16);

impl BlockState {
    pub const AIR: Self = Self(0);
    pub const fn new(kind: u16, meta: u8) -> Self {
        Self((kind << 4) | (meta as u16 & 15))
    }
    pub const fn of(kind: u16) -> Self {
        Self::new(kind, 0)
    }
    pub const fn kind(self) -> u16 {
        self.0 >> 4
    }
    pub const fn meta(self) -> u8 {
        (self.0 & 15) as u8
    }
    pub const fn is_air(self) -> bool {
        self.0 >> 4 == 0
    }
}

/// A box inside a block, in sixteenths: `[min, max]`.
pub type B16 = [[u8; 3]; 2];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Shape {
    Empty,
    #[default]
    Cube,
    /// Two crossed quads: plants.
    Cross,
    /// Meta 0: bottom half. Meta 1: top half.
    Slab,
    /// Meta bits 0-1: which side is the high one (north, south, west, east). Bit 2: upside down.
    Stairs,
    /// Meta 0: standing. Meta 1-4: on the wall to the north, south, west, east.
    Torch,
    /// Meta 0-3: on the wall to the north, south, west, east.
    Ladder,
    /// Flat on the ground, no collision.
    Flat,
    /// A small box on the ground, no collision.
    Button,
    /// Meta 0-7: height in eighths, minus one.
    Layer,
    /// A cube inset by one sixteenth on the sides.
    Inset,
    /// A cube inset by one sixteenth on every side but the bottom.
    Crate,
    /// Water and lava. Meta is the level: 0 is a source, 1-7 thinner and thinner. Bit 3: falling.
    Fluid,
    /// A crop: crossed quads like a plant, as tall as it has grown. Meta 0-7 is how far, 7 ripe.
    Crop,
    /// Tilled soil, a sixteenth short of a full block.
    Farmland,
    /// Half a bed. Meta 0-3: the way from its foot to its head, north, south, west, east.
    Bed,
    /// Half a door. Meta bits 0-1: which side of its cell it closes, north, south, west, east.
    /// Bit 2: open, swung a quarter turn onto the next side round. Bit 3: the top half.
    Door,
    /// Bits 0-1: the side its hinge is on. Bit 2: open, standing against that side. Bit 3: in the
    /// top of its cell rather than the bottom.
    Trapdoor,
    /// A post, with rails to whichever neighbours it joins: meta bits 0-3 north, south, west, east.
    /// Too tall to jump.
    Fence,
    /// A gate in a fence. Bits 0-1: which way it faces; north and south span east to west. Bit 2: open.
    Gate,
    /// A board with words on it. Bits 0-1: the way the words face. Bit 2: on a wall, not a post.
    Sign,
    /// A rail, drawn flat on the ground or up a step, with nothing to bump into. Meta 0-9 is its
    /// shape: see `rail_slopes`, and the server's rails module for the rest.
    Rail,
}

/// Whether a rail of this meta rises a block across its cell.
pub const fn rail_slopes(meta: u8) -> bool {
    matches!(meta, 2..=5)
}

const WHOLE: B16 = [[0, 0, 0], [16, 16, 16]];
const SLAB_LOW: B16 = [[0, 0, 0], [16, 8, 16]];
const SLAB_HIGH: B16 = [[0, 8, 0], [16, 16, 16]];
const FARMLAND: B16 = [[0, 0, 0], [16, 15, 16]];
const BED: B16 = [[0, 0, 0], [16, 9, 16]];
static DOOR: [[B16; 1]; 8] = [
    // Closed, along the north, south, west or east side of its cell.
    [[[0, 0, 0], [16, 16, 3]]],
    [[[0, 0, 13], [16, 16, 16]]],
    [[[0, 0, 0], [3, 16, 16]]],
    [[[13, 0, 0], [16, 16, 16]]],
    // Open: turned about its hinge onto the next side round, north to west to south to east.
    [[[0, 0, 0], [3, 16, 16]]],
    [[[13, 0, 0], [16, 16, 16]]],
    [[[0, 0, 13], [16, 16, 16]]],
    [[[0, 0, 0], [16, 16, 3]]],
];
const LEAF_LOW: B16 = [[0, 0, 0], [16, 3, 16]];
const LEAF_HIGH: B16 = [[0, 13, 0], [16, 16, 16]];
static TRAPDOOR: [[B16; 1]; 16] = [
    [LEAF_LOW],
    [LEAF_LOW],
    [LEAF_LOW],
    [LEAF_LOW],
    [[[0, 0, 0], [16, 16, 3]]],
    [[[0, 0, 13], [16, 16, 16]]],
    [[[0, 0, 0], [3, 16, 16]]],
    [[[13, 0, 0], [16, 16, 16]]],
    [LEAF_HIGH],
    [LEAF_HIGH],
    [LEAF_HIGH],
    [LEAF_HIGH],
    [[[0, 0, 0], [16, 16, 3]]],
    [[[0, 0, 13], [16, 16, 16]]],
    [[[0, 0, 0], [3, 16, 16]]],
    [[[13, 0, 0], [16, 16, 16]]],
];

/// Boxes that depend on the meta in more ways than a fixed table holds: fences, gates and signs.
struct Built {
    fence: Vec<Vec<B16>>,
    fence_solid: Vec<Vec<B16>>,
    gate: Vec<Vec<B16>>,
    gate_solid: Vec<Vec<B16>>,
    sign: Vec<Vec<B16>>,
}

/// A box turned a quarter about the vertical: x for z.
const fn turned(b: B16) -> B16 {
    [[b[0][2], b[0][1], b[0][0]], [b[1][2], b[1][1], b[1][0]]]
}

fn built() -> &'static Built {
    static BUILT: std::sync::OnceLock<Built> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| {
        // A fence's rails, and its tall walls, toward north, south, west and east.
        let rails: [[B16; 2]; 4] = [
            [[[7, 6, 0], [9, 9, 6]], [[7, 12, 0], [9, 15, 6]]],
            [[[7, 6, 10], [9, 9, 16]], [[7, 12, 10], [9, 15, 16]]],
            [[[0, 6, 7], [6, 9, 9]], [[0, 12, 7], [6, 15, 9]]],
            [[[10, 6, 7], [16, 9, 9]], [[10, 12, 7], [16, 15, 9]]],
        ];
        let walls: [B16; 4] = [
            [[6, 0, 0], [10, 24, 6]],
            [[6, 0, 10], [10, 24, 16]],
            [[0, 0, 6], [6, 24, 10]],
            [[10, 0, 6], [16, 24, 10]],
        ];
        let mut fence = Vec::new();
        let mut fence_solid = Vec::new();
        for meta in 0..16usize {
            let mut seen = vec![[[6, 0, 6], [10, 16, 10]]];
            let mut solid = vec![[[6, 0, 6], [10, 24, 10]]];
            for side in 0..4 {
                if meta & (1 << side) != 0 {
                    seen.extend(rails[side]);
                    solid.push(walls[side]);
                }
            }
            fence.push(seen);
            fence_solid.push(solid);
        }
        // A gate across x, closed and open, then the same turned to go across z.
        let closed: Vec<B16> = vec![
            [[0, 5, 7], [2, 16, 9]],
            [[14, 5, 7], [16, 16, 9]],
            [[2, 6, 7], [14, 9, 9]],
            [[2, 12, 7], [14, 15, 9]],
            [[7, 9, 7], [9, 12, 9]],
        ];
        let open: Vec<B16> = vec![
            [[0, 5, 7], [2, 16, 9]],
            [[14, 5, 7], [16, 16, 9]],
            [[0, 6, 9], [2, 9, 15]],
            [[0, 12, 9], [2, 15, 15]],
            [[14, 6, 9], [16, 9, 15]],
            [[14, 12, 9], [16, 15, 15]],
        ];
        let across =
            |list: &[B16], turn: bool| -> Vec<B16> { list.iter().map(|&b| if turn { turned(b) } else { b }).collect() };
        let mut gate = Vec::new();
        let mut gate_solid = Vec::new();
        for meta in 0..8usize {
            let turn = meta & 2 != 0;
            let is_open = meta & 4 != 0;
            gate.push(across(if is_open { &open } else { &closed }, turn));
            gate_solid.push(if is_open { Vec::new() } else { across(&[[[0, 0, 6], [16, 24, 10]]], turn) });
        }
        // Signs: on a post, facing north, south, west, east; then on a wall behind their words.
        let post: B16 = [[7, 0, 7], [9, 7, 9]];
        let board: B16 = [[1, 7, 7], [15, 15, 9]];
        let mut sign = Vec::new();
        for facing in 0..4 {
            sign.push(vec![if facing < 2 { board } else { turned(board) }, post]);
        }
        sign.push(vec![[[1, 4, 14], [15, 12, 16]]]);
        sign.push(vec![[[1, 4, 0], [15, 12, 2]]]);
        sign.push(vec![[[14, 4, 1], [16, 12, 15]]]);
        sign.push(vec![[[0, 4, 1], [2, 12, 15]]]);
        Built { fence, fence_solid, gate, gate_solid, sign }
    })
}

static CROP_OUTLINE: [[B16; 1]; 8] = [
    [[[2, 0, 2], [14, 2, 14]]],
    [[[2, 0, 2], [14, 4, 14]]],
    [[[2, 0, 2], [14, 6, 14]]],
    [[[2, 0, 2], [14, 8, 14]]],
    [[[2, 0, 2], [14, 10, 14]]],
    [[[2, 0, 2], [14, 12, 14]]],
    [[[2, 0, 2], [14, 14, 14]]],
    [[[2, 0, 2], [14, 16, 14]]],
];

static STAIRS: [[B16; 2]; 8] = [
    [SLAB_LOW, [[0, 8, 0], [16, 16, 8]]],
    [SLAB_LOW, [[0, 8, 8], [16, 16, 16]]],
    [SLAB_LOW, [[0, 8, 0], [8, 16, 16]]],
    [SLAB_LOW, [[8, 8, 0], [16, 16, 16]]],
    [SLAB_HIGH, [[0, 0, 0], [16, 8, 8]]],
    [SLAB_HIGH, [[0, 0, 8], [16, 8, 16]]],
    [SLAB_HIGH, [[0, 0, 0], [8, 8, 16]]],
    [SLAB_HIGH, [[8, 0, 0], [16, 8, 16]]],
];
static TORCH: [[B16; 1]; 5] = [
    [[[7, 0, 7], [9, 10, 9]]],
    [[[7, 3, 0], [9, 13, 3]]],
    [[[7, 3, 13], [9, 13, 16]]],
    [[[0, 3, 7], [3, 13, 9]]],
    [[[13, 3, 7], [16, 13, 9]]],
];
static LADDER: [[B16; 1]; 4] = [
    [[[0, 0, 0], [16, 16, 2]]],
    [[[0, 0, 14], [16, 16, 16]]],
    [[[0, 0, 0], [2, 16, 16]]],
    [[[14, 0, 0], [16, 16, 16]]],
];
static LAYER: [[B16; 1]; 8] = [
    [[[0, 0, 0], [16, 2, 16]]],
    [[[0, 0, 0], [16, 4, 16]]],
    [[[0, 0, 0], [16, 6, 16]]],
    [[[0, 0, 0], [16, 8, 16]]],
    [[[0, 0, 0], [16, 10, 16]]],
    [[[0, 0, 0], [16, 12, 16]]],
    [[[0, 0, 0], [16, 14, 16]]],
    [WHOLE],
];

impl Shape {
    /// The boxes a block of this shape is drawn with. Empty for shapes the mesher handles itself.
    pub fn boxes(self, meta: u8) -> &'static [B16] {
        match self {
            Shape::Cube => std::slice::from_ref(&WHOLE),
            Shape::Slab => {
                if meta & 1 == 0 {
                    std::slice::from_ref(&SLAB_LOW)
                } else {
                    std::slice::from_ref(&SLAB_HIGH)
                }
            }
            Shape::Stairs => &STAIRS[(meta & 7) as usize],
            Shape::Torch => &TORCH[(meta as usize).min(4)],
            Shape::Ladder => &LADDER[(meta & 3) as usize],
            Shape::Flat => &[[[0, 0, 0], [16, 1, 16]]],
            Shape::Button => &[[[5, 0, 5], [11, 6, 11]]],
            Shape::Layer => &LAYER[(meta & 7) as usize],
            Shape::Inset => &[[[1, 0, 1], [15, 16, 15]]],
            Shape::Crate => &[[[1, 0, 1], [15, 15, 15]]],
            Shape::Farmland => std::slice::from_ref(&FARMLAND),
            Shape::Bed => std::slice::from_ref(&BED),
            Shape::Door => &DOOR[(meta & 7) as usize],
            Shape::Trapdoor => &TRAPDOOR[(meta & 15) as usize],
            Shape::Fence => &built().fence[(meta & 15) as usize],
            Shape::Gate => &built().gate[(meta & 7) as usize],
            Shape::Sign => &built().sign[(meta & 7) as usize],
            Shape::Empty | Shape::Cross | Shape::Fluid | Shape::Crop | Shape::Rail => &[],
        }
    }
    /// The boxes a body collides with.
    pub fn collision(self, meta: u8) -> &'static [B16] {
        match self {
            Shape::Cube
            | Shape::Slab
            | Shape::Stairs
            | Shape::Layer
            | Shape::Inset
            | Shape::Crate
            | Shape::Ladder
            | Shape::Farmland
            | Shape::Bed
            | Shape::Door
            | Shape::Trapdoor => self.boxes(meta),
            // Too tall to jump, as a fence should be.
            Shape::Fence => &built().fence_solid[(meta & 15) as usize],
            Shape::Gate => &built().gate_solid[(meta & 7) as usize],
            Shape::Sign
            | Shape::Rail
            | Shape::Empty
            | Shape::Cross
            | Shape::Torch
            | Shape::Flat
            | Shape::Button
            | Shape::Fluid
            | Shape::Crop => &[],
        }
    }
    /// The boxes the crosshair can hit: everything that can be broken.
    pub fn outline(self, meta: u8) -> &'static [B16] {
        match self {
            Shape::Cross => &[[[3, 0, 3], [13, 13, 13]]],
            Shape::Crop => &CROP_OUTLINE[(meta & 7) as usize],
            Shape::Rail if rail_slopes(meta) => &[[[0, 0, 0], [16, 8, 16]]],
            Shape::Rail => &[[[0, 0, 0], [16, 2, 16]]],
            _ => self.boxes(meta),
        }
    }
}

/// Which pass draws a block.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Layer {
    #[default]
    Opaque,
    /// Holes in the texture, no blending: leaves, plants, glass.
    Cutout,
    /// Blended and sorted: water, ice.
    Translucent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Tool {
    #[default]
    Any,
    Pick,
    Axe,
    Shovel,
    Sword,
    /// Tills grass and dirt into farmland.
    Hoe,
}

/// How a block picks up its biome's colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Tint {
    #[default]
    None,
    Grass,
    Foliage,
    Water,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Sound {
    #[default]
    Stone,
    Dirt,
    Grass,
    Sand,
    Wood,
    Glass,
    Wool,
    Snow,
    Metal,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Fluid {
    #[default]
    None,
    Water,
    Lava,
}

/// How to paint a texture. The client turns these into pixels when it starts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum Pattern {
    /// Soft mottling between two colours.
    #[default]
    Noise,
    /// One colour with sparse dark and light grains.
    Speckle,
    /// Horizontal bands.
    Strata,
    /// Rounded stones with dark seams.
    Cobble,
    Bricks,
    /// Large cut stones.
    Blocks,
    Planks,
    LogSide,
    LogTop,
    Leaves,
    Grass,
    GrassSide,
    Sand,
    Gravel,
    /// Stone (colour `a`) with blobs of colour `b`.
    Ore,
    Tuft,
    Flower,
    Mushroom,
    Reed,
    Sapling,
    DeadBush,
    Glass,
    Wool,
    Water,
    Lava,
    Ice,
    Snow,
    Torch,
    Ladder,
    BenchTop,
    BenchSide,
    FurnaceFront,
    FurnaceSide,
    ChestFront,
    ChestSide,
    ChestTop,
    Lamp,
    Wire,
    Lever,
    Inverter,
    Cactus,
    CactusTop,
    Tnt,
    /// Tilled rows of soil.
    Farmland,
    /// Stalks (colour `a`) with ripe heads (colour `b`) at the top.
    Crop,
    /// A patchwork quilt, the top of a bed's foot.
    Quilt,
    /// A pillow on the quilt, the top of a bed's head.
    Pillow,
    /// A bed's wooden frame under the edge of its quilt.
    BedSide,
    /// A panelled wooden door.
    Door,
    /// A trapdoor: boards across a frame.
    Trapdoor,
    /// Two bars (colour `a`) along wooden ties (colour `b`), straight.
    Rail,
    /// The same, round a corner.
    RailCurve,
    /// Tongues of fire, colour `a` at the foot and `b` at the tips.
    Flame,
    /// Charred logs (colour `a`) laid across glowing coals (colour `b`).
    Embers,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Tex {
    #[serde(default)]
    pub pattern: Pattern,
    #[serde(default)]
    pub a: [u8; 3],
    #[serde(default)]
    pub b: [u8; 3],
    #[serde(default)]
    pub seed: u32,
}

impl Tex {
    pub const fn new(pattern: Pattern, a: [u8; 3], b: [u8; 3], seed: u32) -> Self {
        Self { pattern, a, b, seed }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Faces {
    pub top: Tex,
    pub bottom: Tex,
    pub side: Tex,
    /// The south face, where it differs from the sides.
    #[serde(default)]
    pub front: Option<Tex>,
}

impl Faces {
    pub const fn all(t: Tex) -> Self {
        Self { top: t, bottom: t, side: t, front: None }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BlockDef {
    pub name: String,
    pub display: String,
    pub shape: Shape,
    pub layer: Layer,
    /// How much sky and block light this block takes away; 15 stops it.
    pub opacity: u8,
    pub emission: u8,
    /// Seconds to break with bare hands, before multipliers. Negative: unbreakable.
    pub hardness: f32,
    pub tool: Tool,
    /// The lowest tool tier that gets the drop; 0 means anything does.
    pub tier: u8,
    /// Item name dropped when broken; `None` drops the block itself, empty string drops nothing.
    pub drop: Option<String>,
    pub drop_min: u8,
    pub drop_max: u8,
    pub tex: Faces,
    pub tint: Tint,
    pub sound: Sound,
    /// Falls when nothing holds it up.
    pub gravity: bool,
    /// Placing a block here replaces it.
    pub replaceable: bool,
    pub fluid: Fluid,
    pub climbable: bool,
    /// Breaks when the block it rests on goes.
    pub fragile: bool,
    /// Damage per second to anything touching it.
    pub contact_damage: u8,
    /// Whether it exists as an item.
    pub has_item: bool,
    /// How readily it burns: the chances in a hundred that fire beside it takes it, each time the
    /// fire flares. 0 never burns.
    pub flammable: u8,
}

impl Default for BlockDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            display: String::new(),
            shape: Shape::Cube,
            layer: Layer::Opaque,
            opacity: 15,
            emission: 0,
            hardness: 1.0,
            tool: Tool::Any,
            tier: 0,
            drop: None,
            drop_min: 1,
            drop_max: 1,
            tex: Faces::default(),
            tint: Tint::None,
            sound: Sound::Stone,
            gravity: false,
            replaceable: false,
            fluid: Fluid::None,
            climbable: false,
            fragile: false,
            contact_damage: 0,
            has_item: true,
            flammable: 0,
        }
    }
}

impl BlockDef {
    /// Whether this block fills its cell and hides whatever is behind its faces.
    pub fn is_full_opaque(&self) -> bool {
        self.shape == Shape::Cube && self.layer == Layer::Opaque && self.opacity >= 15
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_packs_kind_and_meta_without_overlap() {
        let s = BlockState::new(4095, 15);
        assert_eq!((s.kind(), s.meta()), (4095, 15));
        assert_eq!(BlockState::new(7, 200).meta(), 8, "meta wraps into its four bits");
        assert!(BlockState::new(0, 3).is_air());
    }

    #[test]
    fn every_box_of_every_shape_and_meta_stays_inside_its_block() {
        let shapes = [
            Shape::Cube,
            Shape::Slab,
            Shape::Stairs,
            Shape::Torch,
            Shape::Ladder,
            Shape::Flat,
            Shape::Button,
            Shape::Layer,
            Shape::Inset,
            Shape::Crate,
            Shape::Cross,
            Shape::Crop,
            Shape::Farmland,
            Shape::Bed,
            Shape::Door,
            Shape::Trapdoor,
            Shape::Fence,
            Shape::Gate,
            Shape::Sign,
            Shape::Rail,
        ];
        for shape in shapes {
            for meta in 0..16 {
                for b in shape.boxes(meta).iter().chain(shape.outline(meta)) {
                    for axis in 0..3 {
                        assert!(b[0][axis] < b[1][axis] && b[1][axis] <= 16, "{shape:?} meta {meta}: {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn fences_and_closed_gates_are_too_tall_to_jump_and_open_gates_let_you_by() {
        assert!(Shape::Fence.collision(0).iter().all(|b| b[1][1] == 24));
        assert_eq!(Shape::Fence.boxes(0b1111).len(), 9, "a post and two rails each way");
        assert!(Shape::Gate.collision(0).iter().any(|b| b[1][1] == 24));
        assert!(Shape::Gate.collision(4).is_empty());
        assert_eq!(Shape::Gate.boxes(2)[0], turned(Shape::Gate.boxes(0)[0]));
        assert!(Shape::Sign.collision(0).is_empty());
    }

    #[test]
    fn an_open_door_turns_onto_the_next_side_and_a_closed_trapdoor_lies_flat() {
        for facing in 0..4u8 {
            assert_ne!(Shape::Door.boxes(facing), Shape::Door.boxes(facing | 4));
            assert_eq!(Shape::Door.boxes(facing), Shape::Door.boxes(facing | 8), "the top half is the same");
        }
        assert_eq!(Shape::Trapdoor.boxes(0)[0], LEAF_LOW);
        assert_eq!(Shape::Trapdoor.boxes(8)[0], LEAF_HIGH);
        assert_eq!(Shape::Trapdoor.boxes(4), Shape::Door.boxes(0));
    }

    #[test]
    fn plants_and_fluids_do_not_collide_but_slabs_do() {
        assert!(Shape::Cross.collision(0).is_empty());
        assert!(Shape::Fluid.collision(0).is_empty());
        assert_eq!(Shape::Slab.collision(1), &[SLAB_HIGH]);
    }

    #[test]
    fn upside_down_stairs_keep_their_step_on_the_same_side() {
        let (low, high) = (Shape::Stairs.boxes(0), Shape::Stairs.boxes(4));
        assert_eq!(low[0], SLAB_LOW);
        assert_eq!(high[0], SLAB_HIGH);
        assert_eq!(low[1][0][2], high[1][0][2]);
        assert_eq!(low[1][1][2], high[1][1][2]);
    }
}
