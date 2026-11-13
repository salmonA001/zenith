//! Where to find things in a world: the nearest place of each biome, and how much of each there is.
//!
//! `cargo run -p zenith-world --example atlas -- SEED [RADIUS]`
use std::collections::BTreeMap;
use zenith_world::Generator;

fn main() {
    let mut args = std::env::args().skip(1);
    let seed: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(2026);
    let radius: i32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(6000);
    let generator = Generator::new(seed);
    let (sx, sy, sz) = generator.find_spawn();
    println!("seed {seed}: spawn at {sx} {sy} {sz}");

    let step = 32;
    let mut nearest: BTreeMap<String, (i64, i32, i32, i32)> = BTreeMap::new();
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut total = 0;
    let mut z = -radius;
    while z <= radius {
        let mut x = -radius;
        while x <= radius {
            let col = generator.column(x as f64, z as f64);
            let name = format!("{:?}", Generator::biome(&col));
            let (dx, dz) = ((x - sx) as i64, (z - sz) as i64);
            let d = dx * dx + dz * dz;
            let height = col.height.round() as i32;
            let e = nearest.entry(name.clone()).or_insert((i64::MAX, 0, 0, 0));
            if d < e.0 {
                *e = (d, x, height, z);
            }
            *counts.entry(name).or_default() += 1;
            total += 1;
            x += step;
        }
        z += step;
    }
    println!("{:<14} {:>6}  nearest (x height z)", "biome", "share");
    for (name, (_, x, h, z)) in &nearest {
        println!("{name:<14} {:>5.1}%  {x} {h} {z}", counts[name] as f64 * 100.0 / total as f64);
    }
}
