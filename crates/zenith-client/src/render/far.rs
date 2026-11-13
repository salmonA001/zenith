//! Terrain to the horizon. Tiles of a height field, finer near the camera, sampled on worker threads
//! from the very function real chunks follow, so the far hills are the hills you will walk to.
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use wgpu::util::DeviceExt;
use zenith_core::pool::Pool;
use zenith_core::{FxMap, FxSet};
use zenith_world::Generator;

/// Cells along a tile's edge.
const CELLS: usize = 64;
/// The smallest cell, in blocks.
const BASE_CELL: i64 = 4;
/// The coarsest level. A tile there is 64 x 4 x 128 = 32768 blocks across.
const MAX_LEVEL: u8 = 7;
/// A tile splits when the camera is closer than this many of its own widths.
const SPLIT: f64 = 1.8;
const MAX_JOBS: usize = 32;
/// Tiles not drawn for this many frames give their memory back.
const KEEP_FRAMES: u64 = 600;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FarVertex {
    pub pos: [f32; 3],
    pub color: [u8; 4],
    pub normal: [i8; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Tile {
    level: u8,
    x: i32,
    z: i32,
}

impl Tile {
    fn cell(self) -> i64 {
        BASE_CELL << self.level
    }
    fn width(self) -> i64 {
        self.cell() * CELLS as i64
    }
    fn origin(self) -> (i64, i64) {
        (self.x as i64 * self.width(), self.z as i64 * self.width())
    }
    fn children(self) -> [Tile; 4] {
        let (level, x, z) = (self.level - 1, self.x * 2, self.z * 2);
        [
            Tile { level, x, z },
            Tile { level, x: x + 1, z },
            Tile { level, x, z: z + 1 },
            Tile { level, x: x + 1, z: z + 1 },
        ]
    }
}

struct Ready {
    buffer: wgpu::Buffer,
    origin: (i64, i64),
    width: f32,
    heights: (f32, f32),
    last_used: u64,
}

/// A tile to draw this frame.
pub struct FarDraw<'a> {
    pub buffer: &'a wgpu::Buffer,
    pub origin: (i64, i64),
    pub width: f32,
    /// The lowest and highest points of the tile.
    pub heights: (f32, f32),
}

pub struct FarTerrain {
    generator: Arc<Generator>,
    pool: Pool,
    tx: Sender<(Tile, Vec<FarVertex>)>,
    rx: Receiver<(Tile, Vec<FarVertex>)>,
    ready: FxMap<Tile, Ready>,
    pending: FxSet<Tile>,
    pub index_buffer: wgpu::Buffer,
    pub index_count: u32,
    frame: u64,
}

fn build_indices() -> Vec<u32> {
    let n = CELLS as u32 + 1;
    let mut out = Vec::with_capacity(CELLS * CELLS * 6 + CELLS * 4 * 6);
    for z in 0..CELLS as u32 {
        for x in 0..CELLS as u32 {
            let (a, b, c, d) = (z * n + x, z * n + x + 1, (z + 1) * n + x, (z + 1) * n + x + 1);
            out.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    // A skirt hangs from each edge, hiding the cracks between tiles of different detail. Each skirt
    // vertex follows the grid's own, in the order `skirt_edges` lists them.
    let grid = n * n;
    for (edge, cells) in skirt_edges().iter().enumerate() {
        let offset = grid + edge as u32 * n;
        for i in 0..CELLS as u32 {
            let (top_a, top_b) = (cells[i as usize], cells[i as usize + 1]);
            let (low_a, low_b) = (offset + i, offset + i + 1);
            out.extend_from_slice(&[top_a, low_a, top_b, top_b, low_a, low_b]);
        }
    }
    out
}

/// The grid vertices along each edge of a tile.
fn skirt_edges() -> [Vec<u32>; 4] {
    let n = CELLS as u32 + 1;
    [
        (0..n).collect(),
        (0..n).map(|x| (n - 1) * n + x).collect(),
        (0..n).map(|z| z * n).collect(),
        (0..n).map(|z| z * n + n - 1).collect(),
    ]
}

/// Samples a tile: pure, and safe on any thread.
fn sample_tile(generator: &Generator, tile: Tile) -> Vec<FarVertex> {
    let (ox, oz) = tile.origin();
    let cell = tile.cell() as f64;
    let n = CELLS + 1;
    // One extra row all round, for slopes and normals across the edges.
    let m = n + 2;
    let mut columns = Vec::with_capacity(m * m);
    for z in 0..m {
        for x in 0..m {
            let (wx, wz) = (ox as f64 + (x as f64 - 1.0) * cell, oz as f64 + (z as f64 - 1.0) * cell);
            columns.push(generator.column(wx, wz));
        }
    }
    let at = |x: usize, z: usize| columns[z * m + x].height.max(zenith_core::pos::SEA_LEVEL as f64);
    let mut out = Vec::with_capacity(n * n + 4 * n);
    for z in 1..=n {
        for x in 1..=n {
            let (dx, dz) = (at(x + 1, z) - at(x - 1, z), at(x, z + 1) - at(x, z - 1));
            let slope = (dx * dx + dz * dz).sqrt() / (2.0 * cell);
            let look = Generator::far_look(&columns[z * m + x], slope);
            let normal = {
                let (nx, ny, nz) = (-dx / (2.0 * cell), 1.0f64, -dz / (2.0 * cell));
                let len = (nx * nx + ny * ny + nz * nz).sqrt();
                [(nx / len * 127.0) as i8, (ny / len * 127.0) as i8, (nz / len * 127.0) as i8, 0]
            };
            let water = look.water;
            out.push(FarVertex {
                pos: [(x - 1) as f32 * cell as f32, look.height - 1.5, (z - 1) as f32 * cell as f32],
                color: [look.color[0], look.color[1], look.color[2], if water { 0 } else { 255 }],
                normal: if water { [0, 127, 0, 0] } else { normal },
            });
        }
    }
    let drop = cell as f32 * 1.5 + 4.0;
    for edge in skirt_edges() {
        for i in edge {
            let mut v = out[i as usize];
            v.pos[1] -= drop;
            out.push(v);
        }
    }
    out
}

impl FarTerrain {
    pub fn new(device: &wgpu::Device, generator: Arc<Generator>, threads: usize) -> Self {
        let indices = build_indices();
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("far indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let (tx, rx) = mpsc::channel();
        Self {
            generator,
            pool: Pool::new("far", threads.max(1)),
            tx,
            rx,
            ready: FxMap::default(),
            pending: FxSet::default(),
            index_buffer,
            index_count: indices.len() as u32,
            frame: 0,
        }
    }

    pub fn tile_count(&self) -> usize {
        self.ready.len()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Takes in finished tiles, asks for what the view needs, and returns what to draw.
    pub fn update(&mut self, device: &wgpu::Device, cam: (f64, f64, f64)) -> Vec<FarDraw<'_>> {
        self.frame += 1;
        let frame = self.frame;
        // Uploading a few per frame is plenty, and never a hitch.
        for _ in 0..8 {
            let Ok((tile, verts)) = self.rx.try_recv() else { break };
            self.pending.remove(&tile);
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("far tile"),
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let heights = verts.iter().fold((f32::MAX, f32::MIN), |h, v| (h.0.min(v.pos[1]), h.1.max(v.pos[1])));
            self.ready.insert(
                tile,
                Ready { buffer, origin: tile.origin(), width: tile.width() as f32, heights, last_used: frame },
            );
        }

        let root_width = (BASE_CELL << MAX_LEVEL) * CELLS as i64;
        let (rx, rz) = ((cam.0 as i64).div_euclid(root_width) as i32, (cam.2 as i64).div_euclid(root_width) as i32);
        let mut want: Vec<(f64, Tile)> = Vec::new();
        let mut draw: Vec<Tile> = Vec::new();
        for z in rz - 1..=rz + 1 {
            for x in rx - 1..=rx + 1 {
                self.visit(Tile { level: MAX_LEVEL, x, z }, cam, &mut want, &mut draw);
            }
        }
        // Coarse first: the view fills in at once, then sharpens from the camera outwards.
        want.sort_by(|a, b| b.1.level.cmp(&a.1.level).then(a.0.total_cmp(&b.0)));
        for (d, tile) in want {
            if self.pending.len() >= MAX_JOBS {
                break;
            }
            if !self.pending.insert(tile) {
                continue;
            }
            let (generator, tx) = (self.generator.clone(), self.tx.clone());
            self.pool.submit(d as i64, move || {
                let _ = tx.send((tile, sample_tile(&generator, tile)));
            });
        }
        for tile in &draw {
            if let Some(r) = self.ready.get_mut(tile) {
                r.last_used = frame;
            }
        }
        self.ready.retain(|_, r| frame - r.last_used < KEEP_FRAMES);
        draw.iter()
            .filter_map(|t| self.ready.get(t))
            .map(|r| FarDraw { buffer: &r.buffer, origin: r.origin, width: r.width, heights: r.heights })
            .collect()
    }

    /// Whether this tile, or finer ones covering all of it, can be drawn right now.
    fn covered(&self, tile: Tile, cam: (f64, f64, f64)) -> bool {
        if Self::wants_split(tile, cam) {
            let children = tile.children();
            if children.iter().all(|&c| self.covered(c, cam)) {
                return true;
            }
        }
        self.ready.contains_key(&tile)
    }

    fn wants_split(tile: Tile, cam: (f64, f64, f64)) -> bool {
        if tile.level == 0 {
            return false;
        }
        let w = tile.width() as f64;
        let (ox, oz) = tile.origin();
        let (cx, cz) = (ox as f64 + w / 2.0, oz as f64 + w / 2.0);
        let (dx, dy, dz) = (cam.0 - cx, (cam.1 - 80.0).max(0.0), cam.2 - cz);
        (dx * dx + dy * dy + dz * dz).sqrt() < w * SPLIT
    }

    fn visit(&self, tile: Tile, cam: (f64, f64, f64), want: &mut Vec<(f64, Tile)>, draw: &mut Vec<Tile>) {
        let distance = {
            let w = tile.width() as f64;
            let (ox, oz) = tile.origin();
            ((cam.0 - (ox as f64 + w / 2.0)).powi(2) + (cam.2 - (oz as f64 + w / 2.0)).powi(2)).sqrt()
        };
        if Self::wants_split(tile, cam) {
            let children = tile.children();
            // A parent stays on screen until all of its children can take over. Once they have, it
            // is not wanted at all: asking for it then would only make it again to throw away.
            if children.iter().all(|&c| self.covered(c, cam)) {
                for c in children {
                    self.visit(c, cam, want, draw);
                }
                return;
            }
            for c in children {
                if !self.ready.contains_key(&c) {
                    want.push((distance, c));
                }
                // Children ready, grandchildren wanted: keep the refinement going underneath.
                if self.ready.contains_key(&c) && Self::wants_split(c, cam) {
                    for g in c.children() {
                        if !self.ready.contains_key(&g) {
                            want.push((distance, g));
                        }
                    }
                }
            }
        }
        if self.ready.contains_key(&tile) {
            draw.push(tile);
        } else {
            want.push((distance, tile));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_has_a_vertex_for_every_grid_point_and_skirt_point() {
        let n = CELLS + 1;
        let verts = sample_tile(&Generator::new(1), Tile { level: 2, x: 3, z: -2 });
        assert_eq!(verts.len(), n * n + 4 * n);
    }

    #[test]
    fn every_index_points_at_a_real_vertex() {
        let n = CELLS + 1;
        let count = (n * n + 4 * n) as u32;
        let indices = build_indices();
        assert!(indices.iter().all(|&i| i < count));
        assert_eq!(indices.len() % 3, 0);
    }

    #[test]
    fn neighbouring_tiles_of_one_level_share_their_edge_heights() {
        let g = Generator::new(4);
        let (a, b) = (Tile { level: 1, x: 0, z: 0 }, Tile { level: 1, x: 1, z: 0 });
        let (va, vb) = (sample_tile(&g, a), sample_tile(&g, b));
        let n = CELLS + 1;
        for z in 0..n {
            let (right, left) = (va[z * n + n - 1].pos[1], vb[z * n].pos[1]);
            assert_eq!(right, left, "row {z}");
        }
    }

    #[test]
    fn far_terrain_stands_where_chunks_will() {
        // The far surface and the real one come from one function: they must agree up close.
        let g = Generator::new(9);
        let tile = Tile { level: 0, x: 3, z: 3 };
        let verts = sample_tile(&g, tile);
        let (ox, oz) = tile.origin();
        let chunk = g.chunk(zenith_core::ChunkPos::new((ox >> 4) as i32, (oz >> 4) as i32));
        let top =
            chunk.top((ox & 15) as i32, (oz & 15) as i32, |s| s.kind() != zenith_content::id::AIR).expect("ground");
        // The tile sits a little low on purpose, so chunks win where they overlap, and forests
        // raise it a little; a few blocks either way is agreement.
        assert!((verts[0].pos[1] - top as f32).abs() < 18.0, "far {} vs real {top}", verts[0].pos[1]);
    }

    #[test]
    fn a_camera_splits_the_tiles_around_it_and_only_those() {
        let cam = (100.0, 80.0, 100.0);
        let near = Tile { level: 3, x: 0, z: 0 };
        let far = Tile { level: 3, x: 200, z: 200 };
        assert!(FarTerrain::wants_split(near, cam));
        assert!(!FarTerrain::wants_split(far, cam));
        assert!(!FarTerrain::wants_split(Tile { level: 0, x: 0, z: 0 }, cam), "level 0 is as fine as it gets");
    }

    #[test]
    fn children_cover_exactly_their_parent() {
        let t = Tile { level: 3, x: -2, z: 5 };
        let (ox, oz) = t.origin();
        let w = t.width();
        let kids = t.children();
        assert_eq!(kids[0].origin(), (ox, oz));
        assert_eq!(kids[3].origin(), (ox + w / 2, oz + w / 2));
        assert_eq!(kids[3].width(), w / 2);
    }
}
