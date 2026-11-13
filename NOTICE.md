# Notices

## Horizon

Zenith was designed after studying [Horizon](https://github.com/horizon-rust/horizon), MIT License, Copyright (c) 2026
Ludovic Marie and the Horizon contributors. No Horizon code was copied. Some ideas were taken from it, most clearly the
single command table that produces a CLI, MCP tools and a reference page. [docs/REVERSE_ENGINEERING.md](docs/REVERSE_ENGINEERING.md)
says which.

## Minecraft

Zenith is not related to, based on or compatible with Minecraft. It contains no code, assets or data from Mojang or
Microsoft, and it is not endorsed by them. Its textures are painted by code in `crates/zenith-client/src/render/textures.rs`,
and its sounds are synthesised by code in `crates/zenith-client/src/audio.rs`.

## mc-modkit

`mc-modkit` installs mod loaders and mods for Minecraft: Java Edition, which you need your own copy of. Minecraft is a
trademark of Mojang Studios. Fabric, Quilt, Forge, NeoForge, Modrinth and CurseForge belong to their own projects and
companies. None of them made or endorse it. It downloads from their public services and runs nothing of theirs except
Forge's and NeoForge's own installers, when you ask for those loaders.

## windows-link

`third_party/windows-link` is a modified copy of the `windows-link` crate 0.2.1 from
[microsoft/windows-rs](https://github.com/microsoft/windows-rs), MIT or Apache-2.0, Copyright (c) Microsoft Corporation.
Its one change makes `link!` use the import libraries of `windows-targets` under the GNU toolchain, so that the project
builds on Windows with only `rustup` installed. Everywhere else it behaves as the original does.

## Other dependencies

The crates listed in `Cargo.lock` keep their own licences.
