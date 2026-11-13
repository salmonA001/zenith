//! World data and the pure functions over it.
//!
//! Three things here are pure functions on purpose, and much of the design hangs off that:
//!
//! - **Generation** ([`worldgen::Generator::chunk`]): a chunk depends on the seed and its position only,
//!   never on which chunks were generated before it. Features that cross chunk borders are stamped
//!   from their origin in whichever chunk asks, so there are no proto-chunks or generation stages.
//!   A client can make any untouched chunk itself, for any view distance, at no cost to the server.
//! - **Light** ([`light::solve`]): a chunk's light depends on the blocks around it only, so it can
//!   never drift out of step with them and chunk arrival order cannot change the result.
//! - **Physics** ([`physics::step`]): the server and the client's prediction run this same code on
//!   the same inputs, so correcting a client is rare and cheating at movement is not possible.

pub mod chunk;
pub mod light;
pub mod path;
pub mod physics;
pub mod pick;
pub mod store;
pub mod worldgen;

pub use chunk::{BlockAccess, Chunk, ChunkMap};
pub use physics::{Body, Keys, MoveInput};
pub use store::RegionStore;
pub use worldgen::{Biome, GENERATOR_VERSION, Generator};
