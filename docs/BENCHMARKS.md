# Benchmarks

Measured on 3 November 2026, and the two looks on 6 November, on one desktop:

- Intel Core i5-14400F (10 cores, 16 threads), 32 GB, NVIDIA GeForce RTX 5060, Windows 11
- Rust 1.99.1, release builds

The raw results are in [benchmarks/2026-11-03](benchmarks/2026-11-03) and [benchmarks/2026-11-06](benchmarks/2026-11-06).
Each section says how to repeat it.

## Terrain

```bash
cargo run --release -p zenith-mesh --example throughput -- 2026 16
```

1,089 chunks around the spawn point of seed 2026:

| Work | Time a chunk | Chunks a second |
| --- | --- | --- |
| Generation, one thread | 0.46 ms | 2,193 |
| Generation, 16 threads | 0.07 ms | 14,873 |
| Light and mesh, one thread | 2.88 ms | 347 |

A meshed chunk is 102.3 KiB on the GPU on average: 1,310 quads, at 20 bytes a vertex.

Generation is cheap enough for a client to make every untouched chunk itself, which is what lets the server skip
sending them. Merging matching faces saves less than hoped: about 1.4 times. Smooth lighting and corner shading make
neighbouring faces differ on rough ground and in caves, and those faces cannot merge.

## The renderer

```bash
cargo build --release
```

```bash
tools/bench-client.sh 8 16 32
```

The game itself with no window (`--offscreen --uncapped`), at 1920 by 1080, standing at the spawn point of seed 2026
and looking out to sea and the mountains. It waits for the GPU at the end of every frame, so the frame time includes the
GPU's share. "Time to a complete view" runs from starting the program, making a new world included, to every chunk in
view being on the GPU.

| View distance | Time to a complete view | fps | Frame ms, average | Frame ms, p99 | CPU ms a frame | Sections drawn | Quads | Vertex MB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 8 | 2.1 s | 432 | 2.36 | 3.59 | 0.84 | 433 | 75,287 | 22 |
| 16 | 1.7 s | 278 | 3.71 | 10.57 | 1.62 | 1,818 | 333,051 | 83 |
| 32 | 2.9 s | 158 | 6.32 | 11.27 | 3.00 | 7,032 | 1,460,236 | 360 |

A video call, a game and an animated wallpaper were drawing on the same GPU during this run, and repeat runs of the
same build varied: 387 to 432 fps at view distance 8, 239 to 282 at 16, and 153 to 177 at 32. An hour earlier, with the
GPU less busy, the same drawing code reached 480, 333 and 196 fps, with p99 frame times of 2.7, 3.8 and 6.4 ms. The
CPU's share of a frame hardly moved between runs, so most of the difference is time spent waiting for the GPU.

That earlier run also turned up a real problem: a complete view at distance 32 took 23 seconds. The client asked for
new chunks only on its 20 ticks a second, eight at a time, so it could never load more than 160 chunks a second
however fast the machine was. It now asks on every frame and keeps 32 jobs a worker queued. Its worker pools are also
sized so that, all busy at once, they leave the frame a core of its own: with more busy workers than cores, frames
waited their turn and loading stuttered. Loading a large view still makes for some uneven frames, of 10 to 25 ms,
while every worker is busy.

### The two looks

```bash
BENCH_FLAGS="--look classic" tools/bench-client.sh 16 32
```

The classic look draws the same sections with no shadow-map passes and no bloom. Measured on 6 November 2026, one look
right after the other, on the same machine as everything else here:

| View distance | Look | Time to a complete view | fps | Frame ms, average | Frame ms, p99 | CPU ms a frame |
| --- | --- | --- | --- | --- | --- | --- |
| 16 | classic | 1.8 s | 640 | 1.62 | 2.35 | 0.75 |
| 16 | modern | 1.8 s | 343 | 2.94 | 3.80 | 1.27 |
| 32 | classic | 3.5 s | 342 | 2.93 | 3.62 | 1.48 |
| 32 | modern | 2.9 s | 195 | 5.15 | 6.02 | 2.57 |

### With ray tracing

```bash
BENCH_FLAGS=--ray-tracing tools/bench-client.sh 16 32
```

The same runs with shadows and water reflections traced, beside runs without, taken one after the other on the same busy
machine. Every section within 16 chunks is traced, and the times wait for all of them.

| View distance | Ray tracing | Time to a complete view | fps | Frame ms, average | Frame ms, p99 | CPU ms a frame |
| --- | --- | --- | --- | --- | --- | --- |
| 16 | off | 2.3 s | 242 | 4.17 | 14.85 | 1.70 |
| 16 | on | 2.5 s | 236 | 4.25 | 13.70 | 1.30 |
| 32 | off | 4.3 s | 176 | 5.72 | 8.43 | 3.02 |
| 32 | on | 3.6 s | 232 | 4.34 | 6.43 | 2.23 |

Tracing costs less than what it replaces: the two shadow-map passes, which draw every section in the sun's view again.
The first version cost about 2 ms of CPU time a frame, because it rebuilt the top-level structure around the camera every
frame. Now the sections sit around an anchor that moves only every kilometre, and the top level is rebuilt only when
sections come, go or change.

## The server under load

```bash
cargo run --release -p zenith-server --example load -- --mode walk --bots 1,10,25,50,100,200
```

`--mode build` and `--mode explore` give the other two tables. Real bots over TCP on 127.0.0.1. Each does what a client does on the wire: says hello, subscribes to the chunks within
10 of it, sends its keys every tick and answers pings. It keeps no world of its own. The server has a view and
simulation distance of 10 chunks, mobs on, and saves to disk as usual. After the bots join and 15 seconds pass, 30
seconds of ticks are timed. TPS is ticks a second, 20 being full speed. "Process MB" is the whole process, bots
included.

- **walk**: survival, wandering within 64 blocks of the spawn point, as players at a base do.
- **build**: creative, wandering, and breaking or placing a block every second, with a chat line every 10 seconds.
- **explore**: creative, flying straight out at full speed, each its own way, so the server keeps loading new terrain.

### walk

| Bots | TPS | Mean ms | p50 | p95 | p99 | Worst | Process MB | KiB/s per bot | Chunks held |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 20.0 | 0.44 | 0.36 | 0.80 | 1.47 | 4.9 | 22 | 2.4 | 526 |
| 10 | 20.0 | 1.83 | 1.62 | 2.72 | 7.60 | 21.2 | 26 | 7.3 | 617 |
| 25 | 20.0 | 1.47 | 1.44 | 2.00 | 3.77 | 4.9 | 30 | 14.8 | 618 |
| 50 | 20.0 | 2.77 | 2.70 | 4.09 | 7.72 | 9.7 | 41 | 26.9 | 667 |
| 100 | 20.0 | 6.15 | 6.29 | 8.44 | 12.37 | 21.7 | 56 | 50.9 | 740 |
| 200 | 20.0 | 17.69 | 18.32 | 23.65 | 31.61 | 39.6 | 89 | 88.4 | 765 |

### build

| Bots | TPS | Mean ms | p50 | p95 | p99 | Worst | Process MB | KiB/s per bot | Chunks held |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 20.0 | 0.39 | 0.35 | 0.46 | 0.77 | 13.6 | 20 | 2.4 | 526 |
| 10 | 20.0 | 0.85 | 0.82 | 1.08 | 1.45 | 3.2 | 25 | 7.1 | 592 |
| 25 | 20.0 | 1.75 | 1.38 | 4.49 | 8.32 | 10.8 | 31 | 14.5 | 616 |
| 50 | 20.0 | 4.38 | 3.14 | 9.56 | 19.67 | 38.4 | 38 | 27.1 | 617 |
| 100 | 20.0 | 7.07 | 7.15 | 11.43 | 17.82 | 52.0 | 59 | 50.7 | 689 |
| 200 | 20.0 | 18.25 | 17.97 | 30.80 | 38.60 | 46.8 | 89 | 86.7 | 716 |

### explore

| Bots | TPS | Mean ms | p50 | p95 | p99 | Worst | Process MB | KiB/s per bot | Chunks held |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 20.0 | 0.47 | 0.34 | 0.58 | 1.49 | 28.7 | 20 | 1.8 | 441 |
| 10 | 20.0 | 2.15 | 1.65 | 4.93 | 10.96 | 26.7 | 42 | 4.9 | 1,551 |
| 25 | 20.0 | 3.02 | 2.21 | 5.47 | 22.17 | 83.3 | 54 | 10.1 | 2,006 |
| 50 | 20.0 | 10.08 | 8.85 | 26.14 | 46.79 | 79.5 | 65 | 14.7 | 2,224 |
| 100 | 20.0 | 29.01 | 25.02 | 60.30 | 82.82 | 165.8 | 79 | 30.8 | 2,553 |
| 200 | 12.4 | 80.71 | 73.66 | 156.47 | 191.27 | 228.1 | 100 | 33.2 | 2,301 |

The server keeps full speed with 200 bots walking or building, at under 20 ms a tick on average. Exploring is the hard
case, because every bot keeps asking for terrain nobody has loaded: 100 explorers still get 20 TPS, though some ticks
run long, and 200 bring it down to 12.4.

The bots run in the same process as the server and take some of its CPU, which a real player's client would not. The
numbers here err on the slow side for that reason.

## Against Horizon: why these do not compare

Horizon publishes its own server measurements. On its equal-work run, both Horizon and vanilla keep 20 TPS up to 50
bots, and at 100 bots Horizon runs at 9 to 11 TPS while vanilla's watchdog stops it (Horizon's `docs/server.md`, line
99 and section 9). Zenith's tables above show more bots at full speed, but **that does not mean Zenith's server is
faster at the same job**. Too much differs:

- **The work is different.** Horizon simulates all of Minecraft for each player: villagers, redstone, every mob's AI
  and more. Zenith simulates far less.
- **The protocol is different.** Horizon has to send every chunk a player sees, and its own profiling found chunk
  sending to be its largest cost. Zenith sends only changed chunks, by design, so most of that work does not exist.
  This is a real advantage of the design, but only for a game that is free to have it.
- **The bots are different.** Horizon's bots behave like vanilla clients, which acknowledge chunk batches and wait for
  the chunk they stand in. Zenith's are its own.
- **The machines are different.** Horizon ran its server on a ThinkPad, with the bots on a separate Mac mini.

Running both on equal work would need Minecraft, which this project does not use, so it was not done.
