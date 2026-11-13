# What Horizon does, and what Zenith does differently

This is the study of [horizon-rust/horizon](https://github.com/horizon-rust/horizon) that Zenith was built from,
at commit `bfc2a27` (1 November 2026). Paths below are in that repository unless they start with `crates/zenith`.

Every statement is marked as one of these:

- **Observed**: read in Horizon's source or documentation, with the place to check it.
- **Inferred**: a conclusion drawn from what was observed. It could be wrong.
- **Unknown**: could not be found out here.

## How this was studied

Horizon is a complete source repository, so this was ordinary code reading. No decompiler was involved, and no
binary was analysed.

**Horizon was never run.** It needs a copy of Minecraft: Java Edition 26.3 and a Microsoft account that owns it
(`README.md:29`, `README.md:35`). Everything below comes from reading, so none of it says how Horizon behaves on a
screen, and every performance figure for Horizon is Horizon's own published number.

## Why Zenith is not a port

**Observed.** Horizon is "Minecraft: Java Edition 26.3, rewritten in Rust", and its purpose is to be
indistinguishable from the original: same worlds, same rules, same terrain, checked against the real game by 326
differential test harnesses (`README.md:27`, `docs/how-horizon-is-built.md:17`). It "contains no Minecraft code,
assets or data" (`README.md:29`), and `horizon-prepare` extracts what it needs from the player's own copy
(`docs/server.md:97`).

**The decision that follows.** A faithful remake would have to be Minecraft: it would need Mojang's assets to show
anything and the real game to test against, neither of which belongs in a new project. So Zenith is an original
sandbox game that takes Horizon's engineering ideas and none of its compatibility goals. It contains no Mojang code,
assets or names, and it cannot open a Minecraft world or join a Minecraft server.

That choice also removes the constraint behind most of what is below. Horizon has to speak Mojang's protocol, lay out
data as Mojang does, and match Mojang's behaviour bug for bug. Much of what Zenith does differently is only possible
because it does not have to, so **this is not a judgement of Horizon's engineering**. It is a different problem.

## The size of each

| | Horizon | Zenith |
| --- | --- | --- |
| Rust | 1,118,412 lines in the checkout, with 33 crates under `crates/` (observed) | about 25,000 lines in 9 crates |
| Largest crate | `horizon-client`, 425,288 lines | `zenith-client`, about 7,400 lines |
| Content | The full game: villagers, raids, every dimension, every command | See the [README](../README.md#whats-in-it) |
| Worlds | Reads and upgrades real Minecraft saves | Its own format only |
| Tested against | The real game, 326 harnesses | Itself: 300+ tests of its own rules |

Horizon has about forty times as much code. Zenith being small is not an achievement in itself: **it has far less
game in it**, and nothing here should be read as saying otherwise.

## Findings, and what Zenith did with each

### 1. Everything hangs off one object

**Observed.** `crates/horizon-client/src/game.rs:1924` declares `struct Game<'a>`. The struct is 740 lines long with
about 400 fields, and the file is 19,807 lines, with 258 more files and folders under `game/`.

**Observed.** Multiplayer guests are simulated by swapping their state into that object:
`crates/horizon-client/src/game/lan_actions.rs:290`, "Run `f` as the guest `conn`: the host's player state is
swapped for theirs."

**Inferred.** The game began as a single-player client and grew servers around the code that existed. Swapping is
what lets all of that player-handling code serve many players without being rewritten. The cost is that a bug in
the swap, or a field it misses, applies one player's state to another.

**Zenith.** Players are plain values in a list (`crates/zenith-server/src/player.rs`), and the server never pretends
that one player is another.

### 2. The dedicated server is the whole game client

**Observed.** "The server is the full game client" (`docs/server.md:97`). The architecture diagram shows the engine as
`horizon-client --dedicated (the game, headless)` (`docs/server.md:121`), and `horizon-client` depends on `wgpu` and
`winit` (`crates/horizon-client/Cargo.toml:39-40`). A separate `horizon-server` crate supervises it from outside.

**Observed, to be fair to it.** `docs/server-next.md` is a design for a faster server in which single player, LAN,
`--dedicated` and the supervisor all run the same server code. Horizon knows about this and is working on it.

**Zenith.** `zenith-server` has no GPU or window code, and neither do `zenith-agent`, `zenith-mesh` or `zenith-ctl`. A
test enforces it (`crates/zenith-ctl/tests/tree.rs`), so it cannot quietly stop being true. Single player is that same
server on a thread, with messages passing through a memory queue instead of a socket.

### 3. Chunks are sent, in full, to everyone

**Observed.** Mojang's protocol sends clients whole chunks, with their light (`crates/horizon-protocol/src/chunk.rs:3`,
`:320`). Horizon's own benchmark found that this dominated: "every chunk section was encoded once per player (#623),
and every packet woke its connection's writer thread (#649). Together they took the 50-bot walking tick from 281 ms
to 31 ms" (`docs/server.md:570`).

**Zenith.** Generation is a pure function of the seed and the position (`crates/zenith-world/src/worldgen/mod.rs`), so
a client makes untouched chunks itself. The server sends only what differs from the generator, which for most chunks
is nothing. View distance then costs the server almost nothing, and the
[benchmarks](BENCHMARKS.md) show what that buys. A test checks that a client's chunks match the server's byte for byte.

Messages that go to everyone are encoded once into a shared buffer (`Frame = Arc<[u8]>`,
`crates/zenith-protocol/src/net.rs`). Each subscriber's chunk deltas are still encoded for that subscriber, but there are
few of them, because only changed chunks have one.

**The price.** A client must be able to generate terrain, and it must run the same generator version as the server. A
change to terrain would change old worlds, so `GENERATOR_VERSION` and a golden digest test guard it.

### 4. World generation is a staged pipeline

**Observed.** Vanilla generation advances a chunk through ten statuses, from `Empty` through `Biomes`, `Terrain`,
`Features` and `Light` to `Full` (`crates/horizon-world/src/chunk_status.rs:8`), because a feature in one chunk can
reach into its neighbours. Horizon must reproduce this exactly.

**Zenith.** There are no stages. A chunk is worked out from the seed alone: features that could reach it are found by
looking at the nearby chunks they could start from, each stamping only what falls inside the chunk being made. Reach
is capped at 15 blocks. Any chunk can be made at any time on any thread, in any order, with the same result.

**The price.** Features cannot depend on each other or on neighbouring terrain's final state, and they are limited in
size. Generation is simpler and less varied than Minecraft's as a result.

### 5. Light

**Observed.** Horizon's client already works out "Sky and block light for one chunk, flood-filled over its 3x3
neighbourhood" (`crates/horizon-client-render/src/light.rs:1`), and the protocol still carries light from the server
because vanilla's does.

**Zenith.** This idea was taken further. Light is not stored or sent at all: it is solved from the blocks each time a
chunk is meshed (`crates/zenith-world/src/light.rs`). There is nothing to keep in sync, and nothing to save.

**The price.** Light costs a flood fill per mesh, and the server cannot ask how bright a block is without solving it.

### 6. Movement

**Observed.** Vanilla's protocol has clients report where they are. Horizon checks moves the way vanilla does ("moved
too quickly", "moved wrongly", `benchmarks/server-equal-work-2026-10-17/README.md`), and its anticheat has a
`prediction` check that compares a move with what the keys allow (`docs/anticheat.md:77`).

**Zenith.** Clients send keys, not positions. The server runs the same physics function the client predicts with
(`crates/zenith-world/src/physics.rs`), and clients correct themselves when they disagree. A client that sends inputs
faster than real time runs out of credit, so speed and fly hacks cannot be expressed in the protocol.

**The price.** A client's moves are always predictions, and the physics has to be identical on both sides, which is why
it uses its own deterministic maths (`crates/zenith-core/src/dmath.rs`) rather than the platform's. Prediction has
only been exercised over a local connection here, never over a real long-distance link.

### 7. A generated command table for AI agents

**Observed.** `horizon-ctl` has "one table of commands (`commands.rs`) [that] gives the help, the argument checks, the
MCP tools and the reference" (`docs/agent-control.md:23`), and a test fails if the reference goes stale
(`docs/agent-control.md:16`).

**Zenith.** Taken directly, because it is simply a good idea:
`crates/zenith-agent/src/commands.rs` does the same, and `crates/zenith-ctl/tests/tree.rs` has the same test. Zenith
adds a headless player (`zenith-ctl play`), which needs no GPU because the agent crate does not contain one, and
simulated keyboard and mouse commands that reach the same code a real keyboard does.

### 8. Terrain vertices

**Observed.** Horizon's vertex is 24 bytes (`crates/horizon-client-render/src/mesher.rs:49`, asserted at
`gfx.rs:7519`), already smaller than vanilla's 28. It keeps float positions so that offset plants land exactly where
vanilla puts them.

**Zenith.** 20 bytes, with fixed-point positions (`crates/zenith-mesh/src/lib.rs`), because no one else's pixel
positions have to be matched. Faces are also merged where they match exactly. That saves less than hoped, about 1.4
times on real terrain, because smooth lighting makes neighbouring faces differ.

### 9. Far terrain: Horizon's is better

**Observed.** Horizon draws "progressively coarser voxel columns" that keep "separate material layers and air gaps",
up to the whole world, refined from real generation (`docs/graphics.md:39-50`, `README.md:47`).

**Zenith.** A plain height field from the same terrain function, in tiles (`crates/zenith-client/src/render/far.rs`). It
has no overhangs and no caves, and it cannot show what players have built.
**Horizon's is the stronger system here.** Zenith's is simple, and it does join up with real chunks exactly, one chunk at
a time.

### 10. Saves

**Observed.** Horizon reads and writes Mojang's region format, as it must to open real saves
(`docs/world-data.md`). Its server settings include vanilla's `sync-chunk-writes`
(`benchmarks/server-equal-work-2026-10-17/README.md`).

**Unknown.** How Horizon behaves if the power fails during a save was not looked into, so nothing here compares the
two on this point.

**Zenith.** Its own format was free to choose (`crates/zenith-world/src/store.rs`). It stores only modified chunks,
writes new data to free space only, keeps two checksummed tables written alternately, and checksums every record, so
that a crash can lose recent work but not the world. Tests simulate a crash before a save finishes, a half-written table,
flipped bits and a file of garbage. A worker thread does the writing, so a tick does not wait for the disk.

## What Zenith did not do

Things Horizon has and Zenith lacks, as a reminder that this is a study and a smaller game, not a replacement:

- Compatibility with anything: Minecraft worlds, servers, clients, resource packs, mods.
- Most of the content: other dimensions, villagers, raids, enchanting, brewing, scoreboards, and hundreds of blocks.
- A launcher, automatic updates, signed releases, crash telemetry.
- Encrypted connections and online accounts. **Zenith's passwords travel unencrypted, so use it on networks you trust.**
- TAA, FXAA, upscaling, HDR output, controllers and accessibility features.
- A native mod system. Zenith's content packs are JSON: blocks, items and recipes only.

## Unknowns

- **How Horizon behaves when running.** It was never run here.
- **Whether Horizon's documents match its code everywhere.** At least one does not: `docs/world-data.md:5-6` says
  lighting, world generation and networking "remain unimplemented" in `horizon-world`, while `README.md` lists them as
  working. The document may only be describing that one crate, but this shows why the source was preferred.
- **How the two compare on equal work.** This cannot be measured without running both. [BENCHMARKS.md](BENCHMARKS.md)
  explains why its numbers do not compare directly.
- **Why Horizon's `Game` grew as it did.** The history above is an inference from the shape of the code. The git history
  was not studied for it.
