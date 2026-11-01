//! Deterministic primitives shared by every layer of Zenith.
//!
//! Nothing here may use a platform-dependent float operation (`sin`, `powf`, `exp`...): worlds and
//! physics must come out bit-identical on every machine, because the client regenerates untouched
//! terrain itself instead of downloading it. See `dmath` for the sine and cosine that are allowed.

pub mod aabb;
pub mod dmath;
pub mod hash;
pub mod noise;
pub mod palette;
pub mod pool;
pub mod pos;
pub mod rng;

pub use aabb::Aabb;
pub use glam;
pub use hash::{FxMap, FxSet};
pub use palette::Paletted;
pub use pos::{BlockPos, ChunkPos, Dir};
pub use rng::Rng;
