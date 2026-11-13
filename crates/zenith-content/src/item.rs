//! Items and how they are made.
use crate::block::Tool;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ItemKind {
    #[default]
    Material,
    /// Places this block kind.
    Block(u16),
    Tool {
        tool: Tool,
        /// Wood 1, stone 2, copper 3, iron 4, crystal 5.
        tier: u8,
        durability: u16,
        /// Hit points per swing.
        damage: u8,
        /// How much faster than bare hands on a block it suits.
        speed: u8,
    },
    /// Eaten, it fills this many points of the food bar.
    Food { heal: u8 },
    /// Worn in an armour slot: 0 head, 1 body, 2 legs, 3 feet.
    Armor {
        slot: u8,
        /// How much it takes off a hit: each point is four per cent.
        points: u8,
        durability: u16,
    },
    /// Draws and shoots arrows.
    Bow { durability: u16 },
    /// Held up, it takes blows from the front.
    Shield { durability: u16 },
    /// Carries a fluid: the block kind it holds, or 0 while empty. Empty, it scoops up the spring
    /// it is pointed at; full, it pours one out.
    Bucket { holds: u16 },
    /// A fishing rod: casts a float onto water, and pulls in what bites.
    Rod { durability: u16 },
    /// Strikes a spark: lights a fire where it is used.
    Lighter { durability: u16 },
}

impl ItemKind {
    /// How many uses it has in it, for things that wear out.
    pub const fn durability(self) -> Option<u16> {
        match self {
            Self::Tool { durability, .. }
            | Self::Armor { durability, .. }
            | Self::Bow { durability }
            | Self::Shield { durability }
            | Self::Rod { durability }
            | Self::Lighter { durability } => Some(durability),
            _ => None,
        }
    }
}

/// How to draw an item that is not a block.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Icon {
    /// The block's own faces, drawn as a little cube.
    #[default]
    Block,
    Tool(Tool, [u8; 3]),
    Stick,
    Lump([u8; 3]),
    Ingot([u8; 3]),
    Gem([u8; 3]),
    Dust([u8; 3]),
    Apple,
    Meat([u8; 3]),
    Brick,
    Seeds([u8; 3]),
    /// A bundle of cut grain.
    Sheaf([u8; 3]),
    Bread,
    /// A piece of armour for a slot (0 head to 3 feet), in a colour.
    Armor(u8, [u8; 3]),
    Arrow,
    Bow,
    Shield([u8; 3]),
    Feather,
    /// A coil of tough fibre.
    Sinew,
    Boat([u8; 3]),
    Cart,
    /// A pail, and the colour of what is in it, if anything.
    Bucket(Option<[u8; 3]>),
    /// A fishing rod and its line.
    Rod,
    /// A fish, in its colour.
    Fish([u8; 3]),
    /// A tinderbox: a small tin, and a spark over it.
    Tinderbox,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ItemDef {
    pub name: String,
    pub display: String,
    pub kind: ItemKind,
    pub max_stack: u8,
    pub icon: Icon,
    /// Groups a recipe can ask for, such as `planks`.
    pub tags: Vec<String>,
    /// How many ticks this burns in a furnace; 0 means it is no fuel.
    pub burn_ticks: u16,
}

impl Default for ItemDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            display: String::new(),
            kind: ItemKind::Material,
            max_stack: 64,
            icon: Icon::Block,
            tags: Vec::new(),
            burn_ticks: 0,
        }
    }
}

/// A pile of one item.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ItemStack {
    pub item: u16,
    pub count: u8,
    /// Durability used up, for tools.
    pub damage: u16,
}

impl ItemStack {
    pub const fn new(item: u16, count: u8) -> Self {
        Self { item, count, damage: 0 }
    }
    pub fn stacks_with(&self, o: &Self) -> bool {
        self.item == o.item && self.damage == o.damage
    }
}

/// A crafting recipe. Names starting with `#` mean any item with that tag.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RecipeDef {
    /// Rows of the shape; a space is an empty cell. Empty for a recipe with no shape.
    pub pattern: Vec<String>,
    pub key: BTreeMap<char, String>,
    /// The ingredients of a recipe with no shape.
    pub inputs: Vec<String>,
    pub out: String,
    pub count: u8,
}

/// What a furnace makes of an item.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SmeltDef {
    pub input: String,
    pub output: String,
}
