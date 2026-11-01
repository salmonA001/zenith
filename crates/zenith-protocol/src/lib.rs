//! What the client and the server say to each other.
//!
//! There is one protocol and one path through it. Single player encodes the same messages and hands
//! the same frames over a channel, so everything a window does on its own world, a stranger across
//! the internet can do too, and the bytes of the two are exactly alike.
pub mod msg;
pub mod net;

pub use msg::*;
pub use net::{Frame, Peer, decode, encode, memory_pair, tcp_peer};
