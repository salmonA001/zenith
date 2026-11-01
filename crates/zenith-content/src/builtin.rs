//! The game's own blocks, items and recipes.
use crate::block::*;
use crate::item::*;
use std::collections::BTreeMap;

/// Block kinds the game's code refers to. These numbers are in saved worlds: never reuse one.
pub mod id {
    pub const AIR: u16 = 0;
    pub const STONE: u16 = 1;
    pub const SLATE: u16 = 2;
    pub const GRANITE: u16 = 3;
    pub const SANDSTONE: u16 = 4;
    pub const COBBLE: u16 = 5;
    pub const BEDROCK: u16 = 6;
    pub const DIRT: u16 = 7;
    pub const GRASS: u16 = 8;
    pub const SAND: u16 = 9;
    pub const GRAVEL: u16 = 10;
    pub const CLAY: u16 = 11;
    pub const SNOW_BLOCK: u16 = 12;
    pub const ICE: u16 = 13;
    pub const WATER: u16 = 14;
    pub const LAVA: u16 = 15;
    pub const OBSIDIAN: u16 = 16;
    pub const COAL_ORE: u16 = 17;
    pub const COPPER_ORE: u16 = 18;
    pub const IRON_ORE: u16 = 19;
    pub const GOLD_ORE: u16 = 20;
    pub const CRYSTAL_ORE: u16 = 21;
    pub const EMBER_ORE: u16 = 22;
    pub const OAK_LOG: u16 = 23;
    pub const OAK_LEAVES: u16 = 24;
    pub const OAK_PLANKS: u16 = 25;
    pub const BIRCH_LOG: u16 = 26;
    pub const BIRCH_LEAVES: u16 = 27;
    pub const BIRCH_PLANKS: u16 = 28;
    pub const PINE_LOG: u16 = 29;
    pub const PINE_LEAVES: u16 = 30;
    pub const PINE_PLANKS: u16 = 31;
    pub const ACACIA_LOG: u16 = 32;
    pub const ACACIA_LEAVES: u16 = 33;
    pub const CACTUS: u16 = 34;
    pub const TALL_GRASS: u16 = 35;
    pub const FERN: u16 = 36;
    pub const DEAD_BUSH: u16 = 37;
    pub const POPPY: u16 = 38;
    pub const DAISY: u16 = 39;
    pub const BLUEBELL: u16 = 40;
    pub const RED_MUSHROOM: u16 = 41;
    pub const BROWN_MUSHROOM: u16 = 42;
    pub const REEDS: u16 = 43;
    pub const SAPLING: u16 = 44;
    pub const STONE_BRICK: u16 = 45;
    pub const BRICK: u16 = 46;
    pub const GLASS: u16 = 47;
    pub const WOOL_WHITE: u16 = 48;
    pub const WOOL_RED: u16 = 49;
    pub const WOOL_BLUE: u16 = 50;
    pub const WOOL_GREEN: u16 = 51;
    pub const WOOL_YELLOW: u16 = 52;
    pub const WOOL_BLACK: u16 = 53;
    pub const STONE_SLAB: u16 = 54;
    pub const PLANK_SLAB: u16 = 55;
    pub const STONE_STAIRS: u16 = 56;
    pub const PLANK_STAIRS: u16 = 57;
    pub const LADDER: u16 = 58;
    pub const TORCH: u16 = 59;
    pub const GLOW_BLOCK: u16 = 60;
    pub const BENCH: u16 = 61;
    pub const FURNACE: u16 = 62;
    pub const CHEST: u16 = 63;
    pub const WIRE: u16 = 64;
    pub const LEVER: u16 = 65;
    pub const LAMP: u16 = 66;
    pub const LAMP_LIT: u16 = 67;
    pub const INVERTER: u16 = 68;
    pub const INVERTER_OFF: u16 = 69;
    pub const SNOW_LAYER: u16 = 70;
    pub const TNT: u16 = 71;
    /// Every kind below this is the game's own; content packs start here.
    pub const FIRST_PACK: u16 = 256;
}

/// Items the game's code refers to, by name.
pub mod it {
    pub const STICK: &str = "stick";
    pub const COAL: &str = "coal";
    pub const APPLE: &str = "apple";
    pub const RAW_MEAT: &str = "raw_meat";
    pub const COOKED_MEAT: &str = "cooked_meat";
    pub const COPPER_INGOT: &str = "copper_ingot";
    pub const IRON_INGOT: &str = "iron_ingot";
    pub const CRYSTAL: &str = "crystal";
    pub const EMBER_DUST: &str = "ember_dust";
}

const STONE_RGB: [u8; 3] = [126, 128, 131];
const DEEP_RGB: [u8; 3] = [72, 74, 82];

const fn tx(pattern: Pattern, a: [u8; 3], b: [u8; 3], seed: u32) -> Tex {
    Tex::new(pattern, a, b, seed)
}

fn def(name: &str, display: &str, tex: Tex) -> BlockDef {
    BlockDef { name: name.into(), display: display.into(), tex: Faces::all(tex), ..Default::default() }
}

impl BlockDef {
    fn hard(mut self, hardness: f32, tool: Tool, tier: u8) -> Self {
        self.hardness = hardness;
        self.tool = tool;
        self.tier = tier;
        self
    }
    fn drops(mut self, item: &str, min: u8, max: u8) -> Self {
        self.drop = Some(item.into());
        self.drop_min = min;
        self.drop_max = max;
        self
    }
    fn sounds(mut self, s: Sound) -> Self {
        self.sound = s;
        self
    }
    /// A plant: crossed quads you walk through and knock off by hand.
    fn plant(mut self) -> Self {
        self.shape = Shape::Cross;
        self.layer = Layer::Cutout;
        self.opacity = 0;
        self.hardness = 0.0;
        self.fragile = true;
        self.sound = Sound::Grass;
        self
    }
    fn see_through(mut self, opacity: u8) -> Self {
        self.layer = Layer::Cutout;
        self.opacity = opacity;
        self
    }
    fn shaped(mut self, shape: Shape, opacity: u8) -> Self {
        self.shape = shape;
        self.opacity = opacity;
        self
    }
}

/// Block kinds, each at its fixed number.
pub fn blocks() -> Vec<BlockDef> {
    use Pattern as P;
    use Sound as S;
    use Tool as T;
    use id::*;
    let mut list: Vec<Option<BlockDef>> = vec![None; INVERTER_OFF.max(TNT) as usize + 1];
    let mut put = |kind: u16, b: BlockDef| {
        let slot = &mut list[kind as usize];
        assert!(slot.is_none(), "kind {kind} defined twice");
        *slot = Some(b);
    };

    put(AIR, BlockDef {
        name: "air".into(),
        display: "Air".into(),
        shape: Shape::Empty,
        opacity: 0,
        hardness: 0.0,
        drop: Some(String::new()),
        replaceable: true,
        has_item: false,
        ..Default::default()
    });

    // Rock.
    put(STONE, def("stone", "Stone", tx(P::Speckle, STONE_RGB, [106, 108, 112], 1)).hard(1.5, T::Pick, 1).drops("cobble", 1, 1));
    put(SLATE, def("slate", "Slate", tx(P::Strata, DEEP_RGB, [58, 60, 68], 2)).hard(3.0, T::Pick, 1).drops("cobble", 1, 1));
    put(GRANITE, def("granite", "Granite", tx(P::Speckle, [152, 110, 94], [116, 78, 68], 3)).hard(1.5, T::Pick, 1));
    put(SANDSTONE, BlockDef {
        tex: Faces {
            top: tx(P::Sand, [221, 203, 157], [204, 184, 136], 4),
            bottom: tx(P::Sand, [212, 192, 144], [190, 170, 124], 5),
            side: tx(P::Strata, [217, 198, 150], [194, 174, 128], 6),
            front: None,
        },
        ..def("sandstone", "Sandstone", Tex::default()).hard(0.8, T::Pick, 1)
    });
    put(COBBLE, def("cobble", "Cobblestone", tx(P::Cobble, [122, 122, 124], [84, 84, 88], 7)).hard(2.0, T::Pick, 1));
    put(BEDROCK, BlockDef { has_item: false, ..def("bedrock", "Bedrock", tx(P::Cobble, [58, 56, 58], [28, 28, 30], 8)).hard(-1.0, T::Pick, 9) });
    put(OBSIDIAN, def("obsidian", "Obsidian", tx(P::Noise, [28, 20, 44], [52, 36, 78], 9)).hard(30.0, T::Pick, 5));

    // Soil.
    put(DIRT, def("dirt", "Dirt", tx(P::Speckle, [125, 88, 60], [100, 68, 46], 10)).hard(0.5, T::Shovel, 0).sounds(S::Dirt));
    put(GRASS, BlockDef {
        tex: Faces {
            top: tx(P::Grass, [255, 255, 255], [205, 205, 205], 11),
            bottom: tx(P::Speckle, [125, 88, 60], [100, 68, 46], 10),
            side: tx(P::GrassSide, [125, 88, 60], [100, 68, 46], 12),
            front: None,
        },
        tint: Tint::Grass,
        ..def("grass", "Grass", Tex::default()).hard(0.6, T::Shovel, 0).sounds(S::Grass).drops("dirt", 1, 1)
    });
    put(SAND, BlockDef { gravity: true, ..def("sand", "Sand", tx(P::Sand, [226, 212, 168], [207, 191, 146], 13)).hard(0.5, T::Shovel, 0).sounds(S::Sand) });
    put(GRAVEL, BlockDef { gravity: true, ..def("gravel", "Gravel", tx(P::Gravel, [136, 130, 126], [96, 92, 90], 14)).hard(0.6, T::Shovel, 0).sounds(S::Sand) });
    put(CLAY, def("clay", "Clay", tx(P::Noise, [160, 165, 176], [142, 148, 160], 15)).hard(0.6, T::Shovel, 0).sounds(S::Dirt));
    put(SNOW_BLOCK, def("snow_block", "Snow", tx(P::Snow, [244, 249, 252], [226, 236, 244], 16)).hard(0.2, T::Shovel, 0).sounds(S::Snow));
    put(ICE, BlockDef { layer: Layer::Translucent, opacity: 2, ..def("ice", "Ice", tx(P::Ice, [158, 206, 244], [204, 232, 252], 17)).hard(0.5, T::Pick, 0).drops("", 0, 0).sounds(S::Glass) });
    put(SNOW_LAYER, BlockDef {
        fragile: true,
        replaceable: true,
        has_item: false,
        ..def("snow_layer", "Snow Layer", tx(P::Snow, [244, 249, 252], [226, 236, 244], 16)).shaped(Shape::Layer, 0).hard(0.1, T::Shovel, 0).drops("", 0, 0).sounds(S::Snow)
    });

    // Fluids.
    put(WATER, BlockDef {
        shape: Shape::Fluid,
        layer: Layer::Translucent,
        opacity: 1,
        tint: Tint::Water,
        fluid: Fluid::Water,
        replaceable: true,
        has_item: false,
        hardness: -1.0,
        drop: Some(String::new()),
        ..def("water", "Water", tx(P::Water, [255, 255, 255], [225, 225, 225], 18))
    });
    put(LAVA, BlockDef {
        shape: Shape::Fluid,
        opacity: 15,
        emission: 15,
        fluid: Fluid::Lava,
        replaceable: true,
        has_item: false,
        hardness: -1.0,
        drop: Some(String::new()),
        contact_damage: 4,
        ..def("lava", "Lava", tx(P::Lava, [255, 120, 22], [255, 214, 72], 19))
    });

    // Ore.
    let ore = |seed: u32, base: [u8; 3], colour: [u8; 3]| tx(P::Ore, base, colour, seed);
    put(COAL_ORE, def("coal_ore", "Coal Ore", ore(20, STONE_RGB, [38, 38, 42])).hard(3.0, T::Pick, 1).drops(it::COAL, 1, 1));
    put(COPPER_ORE, def("copper_ore", "Copper Ore", ore(21, STONE_RGB, [200, 122, 76])).hard(3.0, T::Pick, 2));
    put(IRON_ORE, def("iron_ore", "Iron Ore", ore(22, STONE_RGB, [216, 180, 150])).hard(3.0, T::Pick, 2));
    put(GOLD_ORE, def("gold_ore", "Gold Ore", ore(23, STONE_RGB, [244, 208, 70])).hard(3.0, T::Pick, 4));
    put(CRYSTAL_ORE, def("crystal_ore", "Crystal Ore", ore(24, DEEP_RGB, [96, 232, 228])).hard(4.5, T::Pick, 4).drops(it::CRYSTAL, 1, 1));
    put(EMBER_ORE, BlockDef { emission: 5, ..def("ember_ore", "Ember Ore", ore(25, DEEP_RGB, [255, 86, 44])).hard(3.0, T::Pick, 4).drops(it::EMBER_DUST, 2, 4) });

    // Wood.
    let log = |name: &str, display: &str, bark: [u8; 3], bark2: [u8; 3], core: [u8; 3], seed: u32| BlockDef {
        tex: Faces {
            top: tx(P::LogTop, core, bark, seed),
            bottom: tx(P::LogTop, core, bark, seed),
            side: tx(P::LogSide, bark, bark2, seed + 1),
            front: None,
        },
        ..def(name, display, Tex::default()).hard(2.0, T::Axe, 0).sounds(S::Wood)
    };
    let leaves = |name: &str, display: &str, seed: u32| BlockDef {
        tint: Tint::Foliage,
        ..def(name, display, tx(P::Leaves, [255, 255, 255], [190, 190, 190], seed)).see_through(1).hard(0.2, T::Any, 0).drops("", 0, 0).sounds(S::Grass)
    };
    let planks = |name: &str, display: &str, a: [u8; 3], b: [u8; 3], seed: u32| def(name, display, tx(P::Planks, a, b, seed)).hard(2.0, T::Axe, 0).sounds(S::Wood);
    put(OAK_LOG, log("oak_log", "Oak Log", [104, 80, 52], [78, 60, 38], [176, 146, 96], 30));
    put(OAK_LEAVES, leaves("oak_leaves", "Oak Leaves", 32));
    put(OAK_PLANKS, planks("oak_planks", "Oak Planks", [176, 142, 92], [146, 116, 72], 33));
    put(BIRCH_LOG, log("birch_log", "Birch Log", [224, 222, 212], [62, 58, 52], [208, 192, 142], 34));
    put(BIRCH_LEAVES, leaves("birch_leaves", "Birch Leaves", 36));
    put(BIRCH_PLANKS, planks("birch_planks", "Birch Planks", [212, 194, 142], [186, 168, 118], 37));
    put(PINE_LOG, log("pine_log", "Pine Log", [66, 48, 34], [46, 34, 24], [132, 100, 66], 38));
    put(PINE_LEAVES, leaves("pine_leaves", "Pine Needles", 40));
    put(PINE_PLANKS, planks("pine_planks", "Pine Planks", [124, 90, 58], [98, 70, 44], 41));
    put(ACACIA_LOG, log("acacia_log", "Acacia Log", [116, 104, 92], [86, 78, 70], [186, 104, 62], 42));
    put(ACACIA_LEAVES, leaves("acacia_leaves", "Acacia Leaves", 44));
    put(CACTUS, BlockDef {
        tex: Faces {
            top: tx(P::CactusTop, [86, 142, 62], [58, 108, 44], 45),
            bottom: tx(P::CactusTop, [86, 142, 62], [58, 108, 44], 45),
            side: tx(P::Cactus, [76, 132, 56], [44, 92, 36], 46),
            front: None,
        },
        fragile: true,
        contact_damage: 1,
        ..def("cactus", "Cactus", Tex::default()).shaped(Shape::Inset, 2).hard(0.4, T::Any, 0).sounds(S::Wool)
    });

    // Plants.
    let plant = |name: &str, display: &str, tex: Tex, tint: Tint| BlockDef { tint, ..def(name, display, tex).plant() };
    put(TALL_GRASS, BlockDef { replaceable: true, ..plant("tall_grass", "Tall Grass", tx(P::Tuft, [255, 255, 255], [196, 196, 196], 50), Tint::Grass).drops("", 0, 0) });
    put(FERN, BlockDef { replaceable: true, ..plant("fern", "Fern", tx(P::Tuft, [232, 240, 232], [170, 190, 170], 51), Tint::Foliage).drops("", 0, 0) });
    put(DEAD_BUSH, plant("dead_bush", "Dead Bush", tx(P::DeadBush, [132, 96, 56], [96, 68, 40], 52), Tint::None).drops(it::STICK, 0, 2));
    put(POPPY, plant("poppy", "Poppy", tx(P::Flower, [226, 44, 40], [62, 132, 50], 53), Tint::None));
    put(DAISY, plant("daisy", "Daisy", tx(P::Flower, [246, 244, 226], [66, 138, 54], 54), Tint::None));
    put(BLUEBELL, plant("bluebell", "Bluebell", tx(P::Flower, [76, 108, 232], [58, 128, 58], 55), Tint::None));
    put(RED_MUSHROOM, BlockDef { emission: 1, ..plant("red_mushroom", "Red Mushroom", tx(P::Mushroom, [206, 42, 40], [236, 226, 208], 56), Tint::None) });
    put(BROWN_MUSHROOM, plant("brown_mushroom", "Brown Mushroom", tx(P::Mushroom, [142, 104, 74], [226, 212, 188], 57), Tint::None));
    put(REEDS, plant("reeds", "Reeds", tx(P::Reed, [136, 182, 86], [96, 142, 62], 58), Tint::None));
    put(SAPLING, plant("sapling", "Sapling", tx(P::Sapling, [70, 142, 52], [98, 72, 44], 59), Tint::None));

    // Building.
    put(STONE_BRICK, def("stone_brick", "Stone Bricks", tx(P::Blocks, [132, 132, 134], [88, 88, 92], 60)).hard(2.0, T::Pick, 1));
    put(BRICK, def("brick", "Bricks", tx(P::Bricks, [156, 82, 66], [190, 180, 170], 61)).hard(2.0, T::Pick, 1));
    put(GLASS, def("glass", "Glass", tx(P::Glass, [222, 240, 246], [168, 206, 222], 62)).see_through(0).hard(0.3, T::Any, 0).drops("", 0, 0).sounds(S::Glass));
    let wool = |name: &str, display: &str, a: [u8; 3], b: [u8; 3], seed: u32| def(name, display, tx(P::Wool, a, b, seed)).hard(0.8, T::Any, 0).sounds(S::Wool);
    put(WOOL_WHITE, wool("wool_white", "White Wool", [236, 236, 232], [214, 214, 210], 63));
    put(WOOL_RED, wool("wool_red", "Red Wool", [186, 48, 44], [160, 38, 36], 64));
    put(WOOL_BLUE, wool("wool_blue", "Blue Wool", [52, 74, 170], [42, 60, 146], 65));
    put(WOOL_GREEN, wool("wool_green", "Green Wool", [72, 126, 52], [58, 104, 42], 66));
    put(WOOL_YELLOW, wool("wool_yellow", "Yellow Wool", [228, 196, 54], [204, 172, 42], 67));
    put(WOOL_BLACK, wool("wool_black", "Black Wool", [38, 38, 44], [26, 26, 32], 68));
    put(STONE_SLAB, def("stone_slab", "Stone Slab", tx(P::Blocks, [132, 132, 134], [88, 88, 92], 60)).shaped(Shape::Slab, 0).hard(2.0, T::Pick, 1));
    put(PLANK_SLAB, def("plank_slab", "Plank Slab", tx(P::Planks, [176, 142, 92], [146, 116, 72], 33)).shaped(Shape::Slab, 0).hard(2.0, T::Axe, 0).sounds(S::Wood));
    put(STONE_STAIRS, def("stone_stairs", "Stone Stairs", tx(P::Blocks, [132, 132, 134], [88, 88, 92], 60)).shaped(Shape::Stairs, 0).hard(2.0, T::Pick, 1));
    put(PLANK_STAIRS, def("plank_stairs", "Plank Stairs", tx(P::Planks, [176, 142, 92], [146, 116, 72], 33)).shaped(Shape::Stairs, 0).hard(2.0, T::Axe, 0).sounds(S::Wood));
    put(LADDER, BlockDef { climbable: true, layer: Layer::Cutout, fragile: true, ..def("ladder", "Ladder", tx(P::Ladder, [156, 118, 72], [116, 86, 50], 70)).shaped(Shape::Ladder, 0).hard(0.4, T::Axe, 0).sounds(S::Wood) });
    put(TORCH, BlockDef { emission: 14, fragile: true, ..def("torch", "Torch", tx(P::Torch, [255, 214, 92], [112, 82, 50], 71)).shaped(Shape::Torch, 0).hard(0.0, T::Any, 0).sounds(S::Wood) });
    put(GLOW_BLOCK, BlockDef { emission: 15, ..def("glow_block", "Glow Block", tx(P::Lamp, [255, 226, 148], [228, 164, 74], 72)).hard(0.5, T::Any, 0).sounds(S::Glass) });

    // Stations.
    put(BENCH, BlockDef {
        tex: Faces {
            top: tx(P::BenchTop, [176, 142, 92], [104, 76, 46], 73),
            bottom: tx(P::Planks, [176, 142, 92], [146, 116, 72], 33),
            side: tx(P::BenchSide, [156, 122, 76], [86, 62, 38], 74),
            front: None,
        },
        ..def("bench", "Workbench", Tex::default()).hard(2.5, T::Axe, 0).sounds(S::Wood)
    });
    put(FURNACE, BlockDef {
        tex: Faces {
            top: tx(P::FurnaceSide, [128, 128, 130], [92, 92, 96], 75),
            bottom: tx(P::FurnaceSide, [128, 128, 130], [92, 92, 96], 75),
            side: tx(P::FurnaceSide, [128, 128, 130], [92, 92, 96], 76),
            front: Some(tx(P::FurnaceFront, [128, 128, 130], [40, 34, 32], 77)),
        },
        ..def("furnace", "Furnace", Tex::default()).hard(3.5, T::Pick, 1)
    });
    put(CHEST, BlockDef {
        tex: Faces {
            top: tx(P::ChestTop, [166, 116, 60], [108, 72, 36], 78),
            bottom: tx(P::ChestTop, [166, 116, 60], [108, 72, 36], 78),
            side: tx(P::ChestSide, [160, 110, 56], [104, 68, 34], 79),
            front: Some(tx(P::ChestFront, [160, 110, 56], [230, 196, 92], 80)),
        },
        ..def("chest", "Chest", Tex::default()).shaped(Shape::Crate, 0).hard(2.5, T::Axe, 0).sounds(S::Wood)
    });

    // Signal parts.
    put(WIRE, BlockDef { layer: Layer::Cutout, fragile: true, has_item: true, ..def("wire", "Ember Wire", tx(P::Wire, [240, 70, 38], [120, 24, 18], 81)).shaped(Shape::Flat, 0).hard(0.0, T::Any, 0).sounds(S::Sand) });
    put(LEVER, BlockDef { fragile: true, ..def("lever", "Lever", tx(P::Lever, [128, 128, 130], [120, 88, 54], 82)).shaped(Shape::Button, 0).hard(0.3, T::Any, 0).sounds(S::Wood) });
    put(LAMP, def("lamp", "Lamp", tx(P::Lamp, [116, 84, 60], [74, 54, 40], 83)).hard(0.5, T::Any, 0).sounds(S::Glass));
    put(LAMP_LIT, BlockDef { emission: 15, has_item: false, ..def("lamp_lit", "Lit Lamp", tx(P::Lamp, [255, 226, 148], [228, 164, 74], 72)).hard(0.5, T::Any, 0).drops("lamp", 1, 1).sounds(S::Glass) });
    put(INVERTER, BlockDef { emission: 7, fragile: true, ..def("inverter", "Inverter", tx(P::Inverter, [240, 70, 38], [112, 82, 50], 84)).shaped(Shape::Torch, 0).hard(0.0, T::Any, 0).sounds(S::Wood) });
    put(INVERTER_OFF, BlockDef { fragile: true, has_item: false, ..def("inverter_off", "Inverter", tx(P::Inverter, [86, 40, 32], [112, 82, 50], 85)).shaped(Shape::Torch, 0).hard(0.0, T::Any, 0).drops("inverter", 1, 1).sounds(S::Wood) });
    put(TNT, def("tnt", "Blast Charge", tx(P::Tnt, [198, 56, 42], [232, 226, 212], 86)).hard(0.0, T::Any, 0).sounds(S::Grass));

    list.into_iter()
        .enumerate()
        .map(|(kind, b)| b.unwrap_or_else(|| panic!("block kind {kind} has no definition")))
        .collect()
}

/// Items that are not blocks. Block items come first in the registry, one per block with an item.
pub fn items() -> Vec<ItemDef> {
    let mut out = Vec::new();
    let mut material = |name: &str, display: &str, icon: Icon, burn: u16| {
        out.push(ItemDef { name: name.into(), display: display.into(), icon, burn_ticks: burn, ..Default::default() });
    };
    material(it::STICK, "Stick", Icon::Stick, 100);
    material(it::COAL, "Coal", Icon::Lump([46, 46, 50]), 1600);
    material(it::COPPER_INGOT, "Copper Ingot", Icon::Ingot([212, 130, 84]), 0);
    material(it::IRON_INGOT, "Iron Ingot", Icon::Ingot([224, 224, 226]), 0);
    material("gold_ingot", "Gold Ingot", Icon::Ingot([250, 214, 76]), 0);
    material(it::CRYSTAL, "Crystal", Icon::Gem([104, 236, 232]), 0);
    material(it::EMBER_DUST, "Ember Dust", Icon::Dust([250, 84, 44]), 0);
    material("clay_ball", "Clay Ball", Icon::Lump([168, 172, 184]), 0);
    material("brick_item", "Brick", Icon::Brick, 0);
    let mut food = |name: &str, display: &str, icon: Icon, heal: u8| {
        out.push(ItemDef { name: name.into(), display: display.into(), icon, kind: ItemKind::Food { heal }, ..Default::default() });
    };
    food(it::APPLE, "Apple", Icon::Apple, 4);
    food(it::RAW_MEAT, "Raw Meat", Icon::Meat([214, 108, 104]), 2);
    food(it::COOKED_MEAT, "Cooked Meat", Icon::Meat([146, 88, 52]), 8);

    // (name, display, tier, durability, base damage, speed, head colour)
    let tiers: [(&str, &str, u8, u16, u8, u8, [u8; 3]); 5] = [
        ("wood", "Wooden", 1, 60, 2, 2, [150, 116, 72]),
        ("stone", "Stone", 2, 130, 3, 4, [136, 136, 140]),
        ("copper", "Copper", 3, 200, 4, 5, [212, 130, 84]),
        ("iron", "Iron", 4, 400, 5, 6, [226, 226, 230]),
        ("crystal", "Crystal", 5, 1500, 7, 9, [104, 236, 232]),
    ];
    for (material, label, tier, durability, damage, speed, colour) in tiers {
        let kinds: [(Tool, &str, &str, u8); 4] = [
            (Tool::Pick, "pick", "Pick", 1),
            (Tool::Axe, "axe", "Axe", 2),
            (Tool::Shovel, "shovel", "Shovel", 0),
            (Tool::Sword, "sword", "Sword", 3),
        ];
        for (tool, suffix, name, bonus) in kinds {
            out.push(ItemDef {
                name: format!("{material}_{suffix}"),
                display: format!("{label} {name}"),
                kind: ItemKind::Tool { tool, tier, durability, damage: damage + bonus, speed },
                max_stack: 1,
                icon: Icon::Tool(tool, colour),
                burn_ticks: if tier == 1 { 200 } else { 0 },
                ..Default::default()
            });
        }
    }
    out
}

fn shaped(rows: &[&str], key: &[(char, &str)], out: &str, count: u8) -> RecipeDef {
    RecipeDef {
        pattern: rows.iter().map(|r| r.to_string()).collect(),
        key: key.iter().map(|&(c, n)| (c, n.to_string())).collect::<BTreeMap<_, _>>(),
        inputs: Vec::new(),
        out: out.into(),
        count,
    }
}

fn loose(inputs: &[&str], out: &str, count: u8) -> RecipeDef {
    RecipeDef {
        pattern: Vec::new(),
        key: BTreeMap::new(),
        inputs: inputs.iter().map(|s| s.to_string()).collect(),
        out: out.into(),
        count,
    }
}

pub fn recipes() -> Vec<RecipeDef> {
    let mut r = vec![
        loose(&["oak_log"], "oak_planks", 4),
        loose(&["birch_log"], "birch_planks", 4),
        loose(&["pine_log"], "pine_planks", 4),
        loose(&["acacia_log"], "oak_planks", 4),
        shaped(&["#", "#"], &[('#', "#planks")], it::STICK, 4),
        shaped(&["##", "##"], &[('#', "#planks")], "bench", 1),
        shaped(&["c", "s"], &[('c', it::COAL), ('s', it::STICK)], "torch", 4),
        shaped(&["###", "# #", "###"], &[('#', "cobble")], "furnace", 1),
        shaped(&["###", "# #", "###"], &[('#', "#planks")], "chest", 1),
        shaped(&["s s", "sss", "s s"], &[('s', it::STICK)], "ladder", 3),
        shaped(&["##", "##"], &[('#', "stone")], "stone_brick", 4),
        shaped(&["##", "##"], &[('#', "brick_item")], "brick", 1),
        shaped(&["###"], &[('#', "stone_brick")], "stone_slab", 6),
        shaped(&["###"], &[('#', "#planks")], "plank_slab", 6),
        shaped(&["#  ", "## ", "###"], &[('#', "stone_brick")], "stone_stairs", 4),
        shaped(&["#  ", "## ", "###"], &[('#', "#planks")], "plank_stairs", 4),
        shaped(&["##", "##"], &[('#', "sand")], "sandstone", 1),
        shaped(&["##", "##"], &[('#', "clay_ball")], "clay", 1),
        shaped(&["e e", " g ", "e e"], &[('e', it::EMBER_DUST), ('g', "glass")], "glow_block", 1),
        loose(&[it::EMBER_DUST], "wire", 4),
        shaped(&["s", "#"], &[('s', it::STICK), ('#', "cobble")], "lever", 1),
        shaped(&[" e ", "e#e", " e "], &[('e', it::EMBER_DUST), ('#', "glass")], "lamp", 1),
        shaped(&["e", "s"], &[('e', it::EMBER_DUST), ('s', it::STICK)], "inverter", 1),
        shaped(&["e#e", "#e#", "e#e"], &[('e', it::EMBER_DUST), ('#', "sand")], "tnt", 1),
        loose(&["wool_white", "poppy"], "wool_red", 1),
        loose(&["wool_white", "bluebell"], "wool_blue", 1),
        loose(&["wool_white", "daisy"], "wool_yellow", 1),
        loose(&["wool_white", "fern"], "wool_green", 1),
        loose(&["wool_white", it::COAL], "wool_black", 1),
    ];
    let heads: [(&str, &str); 5] = [
        ("wood", "#planks"),
        ("stone", "cobble"),
        ("copper", it::COPPER_INGOT),
        ("iron", it::IRON_INGOT),
        ("crystal", it::CRYSTAL),
    ];
    for (material, head) in heads {
        let key: [(char, &str); 2] = [('#', head), ('s', it::STICK)];
        r.push(shaped(&["###", " s ", " s "], &key, &format!("{material}_pick"), 1));
        r.push(shaped(&["##", "#s", " s"], &key, &format!("{material}_axe"), 1));
        r.push(shaped(&["#", "s", "s"], &key, &format!("{material}_shovel"), 1));
        r.push(shaped(&["#", "#", "s"], &key, &format!("{material}_sword"), 1));
    }
    r
}

pub fn smelts() -> Vec<SmeltDef> {
    [
        ("copper_ore", it::COPPER_INGOT),
        ("iron_ore", it::IRON_INGOT),
        ("gold_ore", "gold_ingot"),
        ("sand", "glass"),
        ("cobble", "stone"),
        ("clay_ball", "brick_item"),
        ("clay", "brick"),
        (it::RAW_MEAT, it::COOKED_MEAT),
        ("oak_log", it::COAL),
        ("birch_log", it::COAL),
        ("pine_log", it::COAL),
    ]
    .into_iter()
    .map(|(input, output)| SmeltDef { input: input.into(), output: output.into() })
    .collect()
}

/// Tags given to block items, by name.
pub fn block_item_tags(name: &str) -> Vec<String> {
    let mut tags = Vec::new();
    if name.ends_with("_planks") {
        tags.push("planks".to_string());
    }
    if name.ends_with("_log") {
        tags.push("log".to_string());
    }
    tags
}

/// How long a block's item burns in a furnace.
pub fn block_item_burn(name: &str) -> u16 {
    if name.ends_with("_log") || name.ends_with("_planks") || name == "bench" || name == "chest" {
        300
    } else if name == "sapling" || name == "ladder" {
        100
    } else {
        0
    }
}
