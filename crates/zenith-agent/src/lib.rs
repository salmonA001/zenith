//! Everything a client is, except the window.
//!
//! [`Session`] is the connection: the world as this client knows it, its predicted body, what it
//! can see. The windowed game draws one, and [`Agent`] drives one, so an AI or a script plays with
//! the same code a person does, on any machine, with no GPU needed. That is the difference from
//! driving a game through its window: a thousand of these fit on one small server.

pub mod agent;
pub mod commands;
pub mod control;
pub mod session;

pub use agent::{Agent, Reply};
pub use commands::{ArgKind, ArgSpec, COMMANDS, CommandSpec};
pub use session::{Event, RemoteEntity, Session};
