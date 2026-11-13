//! Mods for Zenith: content packs, from wherever they are published, kept in one library.
//!
//! A mod is a content pack (blocks, items and recipes, in JSON) and a note of where it came from.
//! Catalogues list mods: one comes with the game, others are files on the web or at the top of a
//! Git repository. The library is a folder with one folder per mod, and a `library.json` saying
//! where to look for more and which mods are turned off.
//!
//! Packs are data. Nothing here runs anything that came with a mod.
mod catalogue;
mod library;

pub use catalogue::{CATALOGUE_FILE, Catalogue, CatalogueEntry, Listing, Source};
pub use library::{EntryState, Installed, Library, Manifest, default_home};
