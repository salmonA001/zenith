//! The game with a window. Everything that is not about the screen lives in `zenith-agent` and
//! `zenith-server`; this crate turns a session into pictures, and a keyboard into input.
pub mod app;
pub mod audio;
pub mod game;
pub mod hud;
pub mod icon;
pub mod models;
pub mod mods_screen;
pub mod options;
pub mod render;
pub mod streamer;
pub mod title;
pub mod touch;
