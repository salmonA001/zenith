# Mods

There are two different things here, because there are two different games:

- **Zenith's mods** are content packs: new blocks, items and recipes, written in JSON. The game's Mods screen finds,
  installs and updates them.
- **`mc-modkit`** is a separate tool for your own copy of Minecraft: Java Edition. It installs Fabric, Quilt, Forge or
  NeoForge, and mods from Modrinth or CurseForge. It does nothing for Zenith, which cannot run Minecraft mods: those are
  Java code written against Minecraft's own.

## Zenith's mods

### Playing with them

Choose **Mods** on the title screen. It lists what is installed, with a switch to turn each one on or off. **Get mods**
shows what each source offers, with **Install** or **Update** beside each mod.

- Mods that are on are in every single-player world you open.
- Turning a mod off, or removing it, is safe for your worlds. A world remembers the number each block was given, and
  any block from a mod that is gone shows as a magenta *Missing* block until the mod is back.
- To add a pack by hand, put its `.json` file in the mods folder (**Open the folder** on the Mods screen). The game
  makes it an installed mod the next time it looks.

Three example mods come with the game, so there is something to try without a network: Neon Lights, Masonry and Garden.

### Sources

A source is somewhere to look for mods. **Add a source** takes any of these. Ctrl+V pastes.

| Source | What it is |
| --- | --- |
| A catalogue | An `https://` address of a `zenith-catalogue.json` file |
| A Git repository | An `https://` address of a repository with a `zenith-catalogue.json` at its top. It is cloned, then updated with each look. Needs `git` |
| A folder | A folder on this computer laid out like a repository |

Downloads go through the system's own `curl`, which Windows 10 and later, macOS and nearly every Linux have.

### Publishing mods

Write a catalogue, and put it on the web or at the top of a Git repository:

```json
{
  "zenith_catalogue": 1,
  "name": "My packs",
  "description": "Things I made.",
  "mods": [
    {
      "id": "lanterns",
      "name": "Lanterns",
      "version": "1.0.0",
      "author": "me",
      "description": "Paper lanterns in six colours.",
      "file": "packs/lanterns.json",
      "sha256": "the SHA-256 of packs/lanterns.json, in hex"
    }
  ]
}
```

- `id` is lower-case letters, digits, `-` and `_`. It is the mod's folder name on players' computers.
- `file` is a path from where the catalogue is, or an `https://` address.
- `sha256` is required. A file that no longer matches it is refused, so a changed file needs a changed catalogue. On
  Windows, `certutil -hashfile packs\lanterns.json SHA256` prints it. On Linux and macOS, use `sha256sum` or `shasum -a 256`.
- A new `version` is offered to everyone who has the mod as an update.

Every pack is checked before it is installed: it has to parse, and to load into the game on its own. `crates/zenith-mods/builtin`
holds the examples, which show most of what a pack can say.

### Servers

```bash
cargo run --release -p zenith-server -- --world my-world --mods
```

`--mods` sends the mods that are on in this computer's library to everyone who joins, along with any packs in the world's
own `packs` folder. Players need nothing installed: the server sends what they need.

### From a script

`zenith-ctl mods` does what the Mods screen does, and prints JSON. `zenith-ctl mods help` lists its commands:

```bash
cargo run --release -p zenith-ctl -- mods browse
```

```bash
cargo run --release -p zenith-ctl -- mods install neon
```

## mc-modkit, for Minecraft: Java Edition

You need your own copy of the game, installed with the official launcher.

The simplest way is step by step: it asks which loader and game version you want and a name, then lets you search for
mods and pick them by number:

```bash
cargo run --release -p mc-modkit
```

Or one command at a time:

```bash
cargo run --release -p mc-modkit -- new fabric-fun --loader fabric
```

```bash
cargo run --release -p mc-modkit -- add fabric-fun sodium lithium
```

Then open the Minecraft Launcher and choose the installation **fabric-fun (Fabric …)**. `mc-modkit --help` lists the
rest: `search`, `mods`, `remove`, `update`, `delete`, and `versions` to see which game versions a loader supports.

What it does, and does not do:

- **Your game is left alone.** Each installation gets its own folder at `.minecraft/modkit/NAME`, with its own `mods`
  folder, and its own entry in the launcher. Before it first changes the launcher's `launcher_profiles.json`, it keeps
  a copy beside it as `launcher_profiles.json.modkit-backup`.
- **Fabric and Quilt** are installed as their own installers do: their launcher profile is fetched from
  `meta.fabricmc.net` or `meta.quiltmc.org` and written to the `versions` folder.
- **Forge and NeoForge** have to patch the game, which only their own installers do. Their official installers are
  fetched from `maven.minecraftforge.net` and `maven.neoforged.net` and run with Java, which you need installed.
- **Mods** come from Modrinth by default. Required dependencies come too, and every file is checked against the hash
  its source gives, or not kept. `remove` also takes away dependencies nothing else needs.
- **CurseForge** needs an API key from [console.curseforge.com](https://console.curseforge.com), given with
  `--curseforge-key` or `CURSEFORGE_API_KEY`, and `--source curseforge`. Some authors allow downloads only through
  CurseForge's own site and app; for those mods it says so.

**How far it has been tested.** With canned answers, every path has a test: loader versions, Fabric and Quilt profiles,
launcher entries, searches, dependencies, hashes, updates and removal. Against the real services, Fabric and Quilt
installations were made in a scratch folder standing in for `.minecraft`, and Modrinth was searched. Three things have
not been tried for real: downloading a mod's file, Forge's and NeoForge's installers, and CurseForge, for want of a key.
It has also not been launched from the real Minecraft Launcher.

Minecraft is a trademark of Mojang Studios. Fabric, Quilt, Forge, NeoForge, Modrinth and CurseForge belong to their
own projects and companies. None of them made or endorse this tool.
