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
    Food {
        heal: u8,
    },
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
