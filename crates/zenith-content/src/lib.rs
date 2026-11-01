//! What exists in the game, as plain data.
//!
//! The server owns the [`Registry`] and sends it to every client when they join, so a content pack
//! installed on a server reaches its players with nothing to install. Textures are descriptions
//! ([`Tex`]) the client paints when it starts, not image files: the game ships without assets.

pub mod block;
pub mod builtin;
pub mod item;
pub mod registry;

pub use block::{B16, BlockDef, BlockState, Faces, Fluid, Layer, Pattern, Shape, Sound, Tex, Tint, Tool};
pub use builtin::{id, it};
pub use item::{Icon, ItemDef, ItemKind, ItemStack, RecipeDef, SmeltDef};
pub use registry::{Crafted, Lut, Pack, Registry, Stable};
