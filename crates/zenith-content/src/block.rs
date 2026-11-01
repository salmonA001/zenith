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
}

const WHOLE: B16 = [[0, 0, 0], [16, 16, 16]];
const SLAB_LOW: B16 = [[0, 0, 0], [16, 8, 16]];
const SLAB_HIGH: B16 = [[0, 8, 0], [16, 16, 16]];

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
static LADDER: [[B16; 1]; 4] =
    [[[[0, 0, 0], [16, 16, 2]]], [[[0, 0, 14], [16, 16, 16]]], [[[0, 0, 0], [2, 16, 16]]], [[[14, 0, 0], [16, 16, 16]]]];
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
            Shape::Empty | Shape::Cross | Shape::Fluid => &[],
        }
    }
    /// The boxes a body collides with.
    pub fn collision(self, meta: u8) -> &'static [B16] {
        match self {
            Shape::Cube | Shape::Slab | Shape::Stairs | Shape::Layer | Shape::Inset | Shape::Crate | Shape::Ladder => {
                self.boxes(meta)
            }
            Shape::Empty | Shape::Cross | Shape::Torch | Shape::Flat | Shape::Button | Shape::Fluid => &[],
        }
    }
    /// The boxes the crosshair can hit: everything that can be broken.
    pub fn outline(self, meta: u8) -> &'static [B16] {
        match self {
            Shape::Cross => &[[[3, 0, 3], [13, 13, 13]]],
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
