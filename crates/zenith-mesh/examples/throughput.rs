//! How fast terrain is made and turned into triangles, and how much merging faces saves.
//!
//! `cargo run --release -p zenith-mesh --example throughput -- [SEED] [RADIUS]`
use std::sync::Arc;
use std::time::Instant;
use zenith_content::{Layer, Registry, Shape};
use zenith_core::pos::{MAX_Y, MIN_Y, SECTIONS};
use zenith_core::{BlockPos, ChunkPos, Dir};
use zenith_mesh::{HoodOwned, Mesher, hood_ref};
use zenith_world::chunk::BlockAccess;
use zenith_world::{Chunk, ChunkMap, Generator};

const ALL_SECTIONS: u32 = (1 << SECTIONS) - 1;

fn main() {
    let mut args = std::env::args().skip(1);
    let seed: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(2026);
    let radius: i32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(16);
    let generator = Arc::new(Generator::new(seed));
    let reg = Arc::new(Registry::builtin());
    let (sx, _, sz) = generator.find_spawn();
    let center = ChunkPos::new(sx >> 4, sz >> 4);
    let mut positions = Vec::new();
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            positions.push(ChunkPos::new(center.x + dx, center.z + dz));
        }
    }
    println!("seed {seed}, {} chunks around the spawn point", positions.len());

    // Generation, on one thread and then on them all.
    let started = Instant::now();
    let chunks: Vec<(ChunkPos, Chunk)> = positions.iter().map(|&p| (p, generator.chunk(p))).collect();
    let one = started.elapsed().as_secs_f64();
    println!(
        "generation, one thread:   {:>8.2} ms a chunk, {:>7.0} chunks a second",
        one * 1000.0 / chunks.len() as f64,
        chunks.len() as f64 / one
    );
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let started = Instant::now();
    std::thread::scope(|s| {
        for slice in positions.chunks(positions.len().div_ceil(threads)) {
            let generator = &generator;
            s.spawn(move || {
                for &p in slice {
                    std::hint::black_box(generator.chunk(p));
                }
            });
        }
    });
    let all = started.elapsed().as_secs_f64();
    println!(
        "generation, {threads} threads:  {:>8.2} ms a chunk, {:>7.0} chunks a second",
        all * 1000.0 / chunks.len() as f64,
        chunks.len() as f64 / all
    );

    let mut map = ChunkMap::default();
    for (p, c) in chunks {
        map.chunks.insert(p, c);
    }

    // Light and mesh together: what a client does for each chunk. Only the inner ones have all
    // their neighbours.
    let mut mesher = Mesher::default();
    let inner: Vec<ChunkPos> = positions
        .iter()
        .copied()
        .filter(|p| (p.x - center.x).abs() < radius && (p.z - center.z).abs() < radius)
        .collect();
    let started = Instant::now();
    let (mut quads, mut bytes, mut cube_quads) = (0usize, 0usize, 0usize);
    for &pos in &inner {
        let hood: HoodOwned = std::array::from_fn(|rz| {
            std::array::from_fn(|rx| map.get(ChunkPos::new(pos.x + rx as i32 - 1, pos.z + rz as i32 - 1)).cloned())
        });
        let result = mesher
            .mesh(&hood_ref(&hood), &reg, &generator, ALL_SECTIONS, &[0; SECTIONS], false)
            .expect("all neighbours are here");
        for s in &result.sections {
            quads += s.mesh.quads();
            bytes += (s.mesh.opaque.len() + s.mesh.cutout.len() + s.mesh.translucent.len())
                * std::mem::size_of::<zenith_mesh::Vertex>();
            cube_quads += s.mesh.opaque.len() / 4;
        }
    }
    let meshing = started.elapsed().as_secs_f64();
    println!(
        "light and mesh, one thread: {:>6.2} ms a chunk, {:>7.0} chunks a second",
        meshing * 1000.0 / inner.len() as f64,
        inner.len() as f64 / meshing
    );
    println!(
        "mesh size:                {:>8.1} KiB a chunk, {:.0} quads a chunk, {} bytes a vertex",
        bytes as f64 / 1024.0 / inner.len() as f64,
        quads as f64 / inner.len() as f64,
        std::mem::size_of::<zenith_mesh::Vertex>()
    );

    // What drawing every exposed face on its own would have taken.
    let opaque = |x: i32, y: i32, z: i32| reg.lut.opaque[map.block(BlockPos::new(x, y, z)).kind() as usize];
    let started = Instant::now();
    let mut exposed = 0usize;
    for &pos in &inner {
        for z in pos.z * 16..pos.z * 16 + 16 {
            for x in pos.x * 16..pos.x * 16 + 16 {
                for y in MIN_Y..=MAX_Y {
                    let def = reg.block(map.block(BlockPos::new(x, y, z)));
                    if def.layer != Layer::Opaque || def.shape != Shape::Cube {
                        continue;
                    }
                    for d in Dir::ALL {
                        let n = BlockPos::new(x, y, z).offset(d);
                        if n.y >= MIN_Y && n.y <= MAX_Y && !opaque(n.x, n.y, n.z) {
                            exposed += 1;
                        }
                    }
                }
            }
        }
    }
    println!(
        "merging faces:            {:.0} exposed faces a chunk become {:.0} opaque quads: {:.1} times fewer (counted in {:.1} s)",
        exposed as f64 / inner.len() as f64,
        cube_quads as f64 / inner.len() as f64,
        exposed as f64 / cube_quads.max(1) as f64,
        started.elapsed().as_secs_f64()
    );
}
