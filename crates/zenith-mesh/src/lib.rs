//! Turns a chunk and its neighbours into triangles.
//!
//! This is a pure function of the blocks: a worker solves the light of the 3x3 chunks around one,
//! then meshes the sections that need it. Coplanar faces with identical texture, colour and corner
//! lighting merge into one quad, which on real terrain takes the vertex count down several times
//! and costs nothing visually because textures repeat across a quad.
use zenith_content::{BlockState, Fluid, Layer, Registry, Shape, Tint, id};
use zenith_core::palette::CELLS;
use zenith_core::pos::{MIN_Y, SECTIONS, cell_index};
use zenith_core::{ChunkPos, Dir};
use zenith_world::light::{Hood, LightVolume, solve};
use zenith_world::worldgen::{foliage_tint, grass_tint, water_tint};
use zenith_world::{Chunk, Generator};

/// A corner of a quad. Quads are four of these in a row, drawn with a shared index pattern.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    /// Position in the section in 1/256 of a block, biased by `POS_BIAS` so plants may lean out.
    pub pos: [u16; 3],
    /// Low byte: ambient occlusion, 0-255. High byte: `flags`.
    pub ao_flags: u16,
    /// Texture coordinates, in 1/256 of a tile. They run past one tile on merged quads.
    pub uv: [u16; 2],
    pub layer: u16,
    /// Low byte: sky light, 0-255. High byte: block light, 0-255.
    pub light: u16,
    pub tint: [u8; 4],
}

/// What `Vertex::pos` holds for a coordinate of 0.
pub const POS_BIAS: u16 = 2 * 256;

pub mod flags {
    /// Bits 0-2 hold the face: an index into `Dir`.
    pub const NORMAL_MASK: u16 = 7;
    /// Moves in the wind: a little (leaves).
    pub const SWAY_SOFT: u16 = 1 << 3;
    /// Moves in the wind: a lot, from the top (plants).
    pub const SWAY_TOP: u16 = 1 << 4;
    pub const WATER: u16 = 1 << 5;
    pub const LAVA: u16 = 1 << 6;
    /// Gives off light, so looks lit whatever is around it.
    pub const GLOW: u16 = 1 << 7;
}

/// The triangles of one section, one list per pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SectionMesh {
    pub opaque: Vec<Vertex>,
    pub cutout: Vec<Vertex>,
    pub translucent: Vec<Vertex>,
}

impl SectionMesh {
    pub fn is_empty(&self) -> bool {
        self.opaque.is_empty() && self.cutout.is_empty() && self.translucent.is_empty()
    }
    pub fn quads(&self) -> usize {
        (self.opaque.len() + self.cutout.len() + self.translucent.len()) / 4
    }
}

#[derive(Debug)]
pub struct MeshedSection {
    pub index: u8,
    pub mesh: SectionMesh,
}

#[derive(Debug)]
pub struct MeshResult {
    pub pos: ChunkPos,
    /// Only the sections that were meshed. Others keep what they had.
    pub sections: Vec<MeshedSection>,
    /// Digests of each section's light, to tell next time whether it changed.
    pub light_hashes: [u64; SECTIONS],
    /// Sky and block light of the middle chunk, if asked for: `[(y * 256) + z * 16 + x]`.
    pub light: Option<Box<[u8]>>,
}

/// One `(x, y, z)` step across the three axes of a face: the two directions along it and the one
/// out of it, with the corners listed counter-clockwise from outside.
struct FaceAxes {
    u: [i32; 3],
    v: [i32; 3],
    n: [i32; 3],
}

fn axes(d: Dir) -> FaceAxes {
    // `u` runs left to right and `v` top to bottom as seen from outside, so textures stay upright.
    match d {
        Dir::Up => FaceAxes { u: [1, 0, 0], v: [0, 0, 1], n: [0, 1, 0] },
        Dir::Down => FaceAxes { u: [1, 0, 0], v: [0, 0, -1], n: [0, -1, 0] },
        Dir::North => FaceAxes { u: [-1, 0, 0], v: [0, -1, 0], n: [0, 0, -1] },
        Dir::South => FaceAxes { u: [1, 0, 0], v: [0, -1, 0], n: [0, 0, 1] },
        Dir::West => FaceAxes { u: [0, 0, 1], v: [0, -1, 0], n: [-1, 0, 0] },
        Dir::East => FaceAxes { u: [0, 0, -1], v: [0, -1, 0], n: [1, 0, 0] },
    }
}

/// Everything about a face that must match for it to merge with its neighbour.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FaceKey {
    layer: u16,
    flags: u16,
    tint: [u8; 3],
    ao: [u8; 4],
    sky: [u8; 4],
    block: [u8; 4],
    pass: u8,
}

/// Working memory a worker keeps between jobs.
pub struct Mesher {
    light: LightVolume,
    /// The section being meshed and the shell of cells around it.
    cells: Vec<u16>,
    section_raw: Vec<u16>,
    mask: Vec<Option<FaceKey>>,
    tints: Vec<[[u8; 3]; 3]>,
}

const SHELL: usize = 18;

#[inline]
fn shell(x: i32, y: i32, z: i32) -> usize {
    (((y + 1) as usize) * SHELL + (z + 1) as usize) * SHELL + (x + 1) as usize
}

impl Default for Mesher {
    fn default() -> Self {
        Self {
            light: LightVolume::default(),
            cells: vec![0; SHELL * SHELL * SHELL],
            section_raw: vec![0; CELLS],
            mask: vec![None; 16 * 16],
            tints: vec![[[0; 3]; 3]; 16 * 16],
        }
    }
}

/// Finds the chunk and local x, z for a position relative to the middle chunk.
#[inline]
fn hood_get(hood: &Hood, x: i32, y: i32, z: i32) -> u16 {
    let (rx, rz) = (((x >> 4) + 1) as usize, ((z >> 4) + 1) as usize);
    match hood[rz][rx] {
        Some(c) => c.get(x & 15, y, z & 15).0,
        None => 0,
    }
}

impl Mesher {
    /// Meshes the sections of `hood`'s middle chunk that are marked in `dirty`, or whose light has
    /// changed since `previous` was computed.
    pub fn mesh(
        &mut self,
        hood: &Hood,
        reg: &Registry,
        generator: &Generator,
        dirty: u32,
        previous: &[u64; SECTIONS],
        keep_light: bool,
    ) -> Option<MeshResult> {
        let center = hood[1][1]?;
        solve(hood, &reg.lut, &mut self.light);
        let mut light_hashes = [0u64; SECTIONS];
        let mut wanted = dirty;
        for s in 0..SECTIONS {
            light_hashes[s] = self.section_light_hash(s);
            if light_hashes[s] != previous[s] {
                wanted |= 1 << s;
            }
        }
        self.prepare_tints(center.pos, generator);
        let mut sections = Vec::new();
        for s in 0..SECTIONS {
            if wanted & (1 << s) == 0 {
                continue;
            }
            let mesh = self.mesh_section(hood, center, s, reg);
            sections.push(MeshedSection { index: s as u8, mesh });
        }
        let light = keep_light.then(|| {
            let mut out = vec![0u8; 256 * zenith_core::pos::WORLD_HEIGHT as usize].into_boxed_slice();
            for y in 0..zenith_core::pos::WORLD_HEIGHT {
                for z in 0..16 {
                    for x in 0..16 {
                        out[(y * 256 + z * 16 + x) as usize] = self.light.packed(x, MIN_Y + y, z);
                    }
                }
            }
            out
        });
        Some(MeshResult { pos: center.pos, sections, light_hashes, light })
    }

    /// A digest of the light a section's mesh depends on: its own cells and the shell around them.
    fn section_light_hash(&self, s: usize) -> u64 {
        let y0 = MIN_Y + s as i32 * 16;
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        for y in y0 - 1..=y0 + 16 {
            if !(MIN_Y..=zenith_core::pos::MAX_Y).contains(&y) {
                continue;
            }
            for z in -1..=16 {
                let start = zenith_world::light::index(-1, y, z);
                for chunk in self.light.cells[start..start + SHELL].chunks(8) {
                    let mut word = [0u8; 8];
                    word[..chunk.len()].copy_from_slice(chunk);
                    h = (h.rotate_left(5) ^ u64::from_le_bytes(word)).wrapping_mul(0x517C_C1B7_2722_0A95);
                }
            }
        }
        h
    }

    fn prepare_tints(&mut self, pos: ChunkPos, generator: &Generator) {
        // Six bits a channel: colours drift so slowly that neighbours nearly always agree, and
        // faces with the same colour can merge.
        let q = |c: [u8; 3]| [(c[0] & 0xFC) | 2, (c[1] & 0xFC) | 2, (c[2] & 0xFC) | 2];
        for z in 0..16 {
            for x in 0..16 {
                let (t, h) = generator.climate((pos.x * 16 + x) as f64, (pos.z * 16 + z) as f64);
                self.tints[(z * 16 + x) as usize] = [q(grass_tint(t, h)), q(foliage_tint(t, h)), q(water_tint(t, h))];
            }
        }
    }

    fn tint_for(&self, tint: Tint, x: i32, z: i32) -> [u8; 3] {
        let (cx, cz) = (x.clamp(0, 15), z.clamp(0, 15));
        let t = &self.tints[(cz * 16 + cx) as usize];
        match tint {
            Tint::None => [255, 255, 255],
            Tint::Grass => t[0],
            Tint::Foliage => t[1],
            Tint::Water => t[2],
        }
    }

    /// Copies a section and the shell of cells around it into `self.cells`.
    fn flatten(&mut self, hood: &Hood, center: &Chunk, s: usize) {
        let y0 = MIN_Y + s as i32 * 16;
        center.sections[s].to_raw(&mut self.section_raw);
        for y in 0..16 {
            for z in 0..16 {
                let from = cell_index(0, y, z);
                let to = shell(0, y, z);
                self.cells[to..to + 16].copy_from_slice(&self.section_raw[from..from + 16]);
            }
        }
        for y in -1..=16i32 {
            for z in -1..=16i32 {
                for x in -1..=16i32 {
                    let inside = (0..16).contains(&x) && (0..16).contains(&y) && (0..16).contains(&z);
                    if !inside {
                        self.cells[shell(x, y, z)] = hood_get(hood, x, y0 + y, z);
                    }
                }
            }
        }
    }

    fn mesh_section(&mut self, hood: &Hood, center: &Chunk, s: usize, reg: &Registry) -> SectionMesh {
        let mut out = SectionMesh::default();
        let section = &center.sections[s];
        if section.single().is_some_and(|v| v >> 4 == 0) {
            return out;
        }
        self.flatten(hood, center, s);
        // Solid rock with solid rock all around it: the commonest section deep down, and empty.
        if section.single().is_some_and(|v| reg.lut.opaque[(v >> 4) as usize]) {
            let buried = (0..16i32).all(|a| {
                (0..16i32).all(|b| {
                    [
                        shell(a, b, -1),
                        shell(a, b, 16),
                        shell(a, -1, b),
                        shell(a, 16, b),
                        shell(-1, a, b),
                        shell(16, a, b),
                    ]
                    .iter()
                    .all(|&i| reg.lut.opaque[(self.cells[i] >> 4) as usize])
                })
            });
            if buried {
                return out;
            }
        }
        let y0 = MIN_Y + s as i32 * 16;
        for d in Dir::ALL {
            self.greedy(d, y0, reg, &mut out);
        }
        for y in 0..16i32 {
            for z in 0..16i32 {
                for x in 0..16i32 {
                    let state = BlockState(self.cells[shell(x, y, z)]);
                    if state.is_air() {
                        continue;
                    }
                    let def = reg.block(state);
                    match def.shape {
                        Shape::Cube | Shape::Empty => {}
                        Shape::Cross | Shape::Crop => self.cross(state, x, y, z, y0, reg, &mut out),
                        Shape::Rail => self.rail(state, x, y, z, y0, reg, &mut out),
                        Shape::Fluid => self.fluid(state, x, y, z, y0, reg, &mut out),
                        _ => self.boxes(state, x, y, z, y0, reg, &mut out),
                    }
                }
            }
        }
        out
    }

    /// Whether the face of `me` towards `other` can never be seen.
    #[inline]
    fn hidden(reg: &Registry, me: BlockState, other: BlockState) -> bool {
        if reg.lut.opaque[other.kind() as usize] {
            return true;
        }
        // Glass against glass, leaves against leaves, ice against ice: no seams inside a pane or
        // a crown, and a forest costs a fraction of the triangles.
        reg.block(me).layer != Layer::Opaque && other.kind() == me.kind()
    }

    /// Lights one corner of a face: (ambient occlusion 0-3, sky 0-255, block 0-255).
    #[inline]
    fn corner(&self, reg: &Registry, x: i32, y: i32, z: i32, y0: i32, a: &FaceAxes, cu: i32, cv: i32) -> (u8, u8, u8) {
        // The four cells around this corner, in the layer the face looks into.
        let (nx, ny, nz) = (x + a.n[0], y + a.n[1], z + a.n[2]);
        let s1 = (nx + a.u[0] * cu, ny + a.u[1] * cu, nz + a.u[2] * cu);
        let s2 = (nx + a.v[0] * cv, ny + a.v[1] * cv, nz + a.v[2] * cv);
        let c = (nx + a.u[0] * cu + a.v[0] * cv, ny + a.u[1] * cu + a.v[1] * cv, nz + a.u[2] * cu + a.v[2] * cv);
        let opaque = |p: (i32, i32, i32)| reg.lut.opaque[(self.cells[shell(p.0, p.1, p.2)] >> 4) as usize];
        let (o1, o2, oc) = (opaque(s1), opaque(s2), opaque(c));
        let ao = if o1 && o2 { 0 } else { 3 - u8::from(o1) - u8::from(o2) - u8::from(oc) };
        let light = |p: (i32, i32, i32)| self.light.packed(p.0, y0 + p.1, p.2);
        let here = light((nx, ny, nz));
        let (mut sky, mut block, mut n) = ((here >> 4) as u32, (here & 15) as u32, 1u32);
        let mut add = |p: (i32, i32, i32), blocked: bool| {
            if !blocked {
                let l = light(p);
                sky += (l >> 4) as u32;
                block += (l & 15) as u32;
                n += 1;
            }
        };
        add(s1, o1);
        add(s2, o2);
        add(c, oc || (o1 && o2));
        // 0-15 averaged over up to four cells, scaled to fill a byte.
        (ao, (sky * 17 / n) as u8, (block * 17 / n) as u8)
    }

    fn greedy(&mut self, d: Dir, y0: i32, reg: &Registry, out: &mut SectionMesh) {
        let a = axes(d);
        let n = a.n;
        // Which axis the slices run along, and which two span a slice.
        let (slice_axis, u_axis, v_axis) = (
            n.iter().position(|&c| c != 0).unwrap_or(0),
            a.u.iter().position(|&c| c != 0).unwrap_or(0),
            a.v.iter().position(|&c| c != 0).unwrap_or(0),
        );
        for slice in 0..16i32 {
            let mut any = false;
            for j in 0..16i32 {
                for i in 0..16i32 {
                    let mut p = [0i32; 3];
                    p[slice_axis] = slice;
                    p[u_axis] = i;
                    p[v_axis] = j;
                    let [x, y, z] = p;
                    let slot = (j * 16 + i) as usize;
                    self.mask[slot] = None;
                    let state = BlockState(self.cells[shell(x, y, z)]);
                    if state.is_air() {
                        continue;
                    }
                    let def = reg.block(state);
                    if def.shape != Shape::Cube {
                        continue;
                    }
                    let other = BlockState(self.cells[shell(x + n[0], y + n[1], z + n[2])]);
                    if Self::hidden(reg, state, other) {
                        continue;
                    }
                    // Corners in order: (-u,-v), (+u,-v), (+u,+v), (-u,+v).
                    let mut ao = [0u8; 4];
                    let mut sky = [0u8; 4];
                    let mut block = [0u8; 4];
                    for (k, (cu, cv)) in [(-1, -1), (1, -1), (1, 1), (-1, 1)].into_iter().enumerate() {
                        let (o, s, b) = self.corner(reg, x, y, z, y0, &a, cu * sign(a.u), cv * sign(a.v));
                        ao[k] = o;
                        sky[k] = s;
                        block[k] = b;
                    }
                    let layer = reg.face_tile(state.kind(), d as usize);
                    // A grass block's sides carry the tint only in their top band, which the shader
                    // reads from the texture; the colour is the same on all faces.
                    let tint = self.tint_for(def.tint, x, z);
                    let tint = if def.tint == Tint::Grass && d == Dir::Down { [255, 255, 255] } else { tint };
                    let mut face_flags = d as u16;
                    if def.tint == Tint::Foliage {
                        face_flags |= flags::SWAY_SOFT;
                    }
                    if def.emission > 0 {
                        face_flags |= flags::GLOW;
                    }
                    let pass = match def.layer {
                        Layer::Opaque => 0,
                        Layer::Cutout => 1,
                        Layer::Translucent => 2,
                    };
                    self.mask[slot] = Some(FaceKey { layer, flags: face_flags, tint, ao, sky, block, pass });
                    any = true;
                }
            }
            if !any {
                continue;
            }
            // Grow rectangles over the mask.
            for j in 0..16usize {
                let mut i = 0usize;
                while i < 16 {
                    let Some(key) = self.mask[j * 16 + i] else {
                        i += 1;
                        continue;
                    };
                    let mut w = 1;
                    while i + w < 16 && self.mask[j * 16 + i + w] == Some(key) {
                        w += 1;
                    }
                    let mut h = 1;
                    'grow: while j + h < 16 {
                        for k in 0..w {
                            if self.mask[(j + h) * 16 + i + k] != Some(key) {
                                break 'grow;
                            }
                        }
                        h += 1;
                    }
                    for row in 0..h {
                        for col in 0..w {
                            self.mask[(j + row) * 16 + i + col] = None;
                        }
                    }
                    let mut p = [0i32; 3];
                    p[slice_axis] = slice;
                    p[u_axis] = i as i32;
                    p[v_axis] = j as i32;
                    emit_merged(&a, p, u_axis, v_axis, w as i32, h as i32, &key, out);
                    i += w;
                }
            }
        }
    }

    /// Two quads crossed like an X, for plants. A crop stands as tall as it has grown, in its rows,
    /// and shows the lower part of its picture: the ripe heads at the top come in last.
    fn cross(&self, state: BlockState, x: i32, y: i32, z: i32, y0: i32, reg: &Registry, out: &mut SectionMesh) {
        let def = reg.block(state);
        let stage = (def.shape == Shape::Crop).then(|| state.meta().min(7));
        let wx = x.wrapping_mul(31) ^ z.wrapping_mul(17) ^ y.wrapping_mul(7);
        // Plants lean out of line a little, so a meadow does not look planted in rows. Crops are.
        let (ox, oz) = if stage.is_some() {
            (0.0, 0.0)
        } else {
            (((wx & 7) - 3) as f32 * 12.0, (((wx >> 3) & 7) - 3) as f32 * 12.0)
        };
        let tint = self.tint_for(def.tint, x, z);
        let layer = reg.face_tile(state.kind(), 2);
        let here = self.light.packed(x, y0 + y, z);
        let (sky, block) = (((here >> 4) as u16) * 17, ((here & 15) as u16) * 17);
        let light = sky | (block << 8);
        let base = |px: f32, py: f32, pz: f32| -> [u16; 3] {
            [
                (x as f32 * 256.0 + px + ox + POS_BIAS as f32) as u16,
                (y as f32 * 256.0 + py + POS_BIAS as f32) as u16,
                (z as f32 * 256.0 + pz + oz + POS_BIAS as f32) as u16,
            ]
        };
        let top = stage.map_or(256.0, |s| (s as f32 + 1.0) * 32.0);
        let (lo, hi) = (38.0, 218.0);
        let quads = [
            [(lo, top, lo), (hi, top, hi), (hi, 0.0, hi), (lo, 0.0, lo)],
            [(hi, top, lo), (lo, top, hi), (lo, 0.0, hi), (hi, 0.0, lo)],
        ];
        let v0 = 256 - top as u16;
        let uvs = [[0u16, v0], [256, v0], [256, 256], [0, 256]];
        for quad in quads {
            for (k, c) in quad.into_iter().enumerate() {
                let sway = if c.1 > 0.0 { flags::SWAY_TOP } else { 0 };
                out.cutout.push(Vertex {
                    pos: base(c.0, c.1, c.2),
                    ao_flags: 255 | ((Dir::Up as u16 | sway) << 8),
                    uv: uvs[k],
                    layer,
                    light,
                    tint: [tint[0], tint[1], tint[2], 255],
                });
            }
        }
    }

    /// A rail: its picture flat on the ground, turned the way the rail runs, or tilted up a step.
    /// A curve uses the picture kept on the block's bottom face.
    fn rail(&self, state: BlockState, x: i32, y: i32, z: i32, y0: i32, reg: &Registry, out: &mut SectionMesh) {
        let meta = state.meta() % 10;
        let face = if meta >= 6 { Dir::Down } else { Dir::Up };
        let layer = reg.face_tile(state.kind(), face as usize);
        let here = self.light.packed(x, y0 + y, z);
        let (sky, block) = (((here >> 4) as u16) * 17, ((here & 15) as u16) * 17);
        let light = sky | (block << 8);
        // Quarter turns of the picture: straight rails are painted running north to south, and a
        // curve joining south and east.
        let turns = match meta {
            1..=3 | 9 => 1,
            8 => 2,
            7 => 3,
            _ => 0,
        };
        // A slope lifts its high side by a block.
        let lift = |cx: f32, cz: f32| match meta {
            2 => cx,
            3 => 256.0 - cx,
            4 => 256.0 - cz,
            5 => cz,
            _ => 0.0,
        };
        for (cx, cz) in [(0.0f32, 0.0f32), (256.0, 0.0), (256.0, 256.0), (0.0, 256.0)] {
            let (mut u, mut v) = (cx, cz);
            for _ in 0..turns {
                (u, v) = (256.0 - v, u);
            }
            out.cutout.push(Vertex {
                pos: [
                    (x as f32 * 256.0 + cx + POS_BIAS as f32) as u16,
                    (y as f32 * 256.0 + 16.0 + lift(cx, cz) + POS_BIAS as f32) as u16,
                    (z as f32 * 256.0 + cz + POS_BIAS as f32) as u16,
                ],
                ao_flags: 255 | ((Dir::Up as u16) << 8),
                uv: [u as u16, v as u16],
                layer,
                light,
                tint: [255, 255, 255, 255],
            });
        }
    }

    /// Anything else made of boxes: slabs, stairs, torches, ladders, wire.
    fn boxes(&self, state: BlockState, x: i32, y: i32, z: i32, y0: i32, reg: &Registry, out: &mut SectionMesh) {
        let def = reg.block(state);
        let mut tint = self.tint_for(def.tint, x, z);
        // Wire glows brighter the stronger its signal.
        if state.kind() == id::WIRE {
            let v = 110 + state.meta() as u16 * 9;
            tint = [v as u8, v as u8, v as u8];
        }
        let list = match def.layer {
            Layer::Opaque => &mut out.opaque,
            Layer::Cutout => &mut out.cutout,
            Layer::Translucent => &mut out.translucent,
        };
        let own = self.light.packed(x, y0 + y, z);
        let glow = if def.emission > 0 { flags::GLOW } else { 0 };
        for b in def.shape.boxes(state.meta()) {
            for d in Dir::ALL {
                let a = axes(d);
                let n = a.n;
                let axis = n.iter().position(|&c| c != 0).unwrap_or(0);
                let on_boundary = if n[axis] > 0 { b[1][axis] == 16 } else { b[0][axis] == 0 };
                let light_cell = if on_boundary {
                    let other = BlockState(self.cells[shell(x + n[0], y + n[1], z + n[2])]);
                    if reg.lut.opaque[other.kind() as usize] {
                        continue;
                    }
                    self.light.packed(x + n[0], y0 + y + n[1], z + n[2])
                } else {
                    own
                };
                // Under an opaque neighbour a box's own cell is dark: use the brighter of the two.
                let sky = (light_cell >> 4).max(own >> 4) as u16 * 17;
                let block = (light_cell & 15).max(own & 15) as u16 * 17;
                let shade_ao: u16 = 255;
                let corners = box_face(b, d);
                let tile = reg.face_tile(state.kind(), d as usize);
                let uv_of = |c: [u8; 3]| -> [u16; 2] {
                    let u_axis = a.u.iter().position(|&v| v != 0).unwrap_or(0);
                    let v_axis = a.v.iter().position(|&v| v != 0).unwrap_or(0);
                    let uu = if a.u[u_axis] > 0 { c[u_axis] as u16 } else { 16 - c[u_axis] as u16 };
                    let vv = if a.v[v_axis] > 0 { c[v_axis] as u16 } else { 16 - c[v_axis] as u16 };
                    [uu * 16, vv * 16]
                };
                for c in corners {
                    list.push(Vertex {
                        pos: [
                            ((x * 16 + c[0] as i32) * 16) as u16 + POS_BIAS,
                            ((y * 16 + c[1] as i32) * 16) as u16 + POS_BIAS,
                            ((z * 16 + c[2] as i32) * 16) as u16 + POS_BIAS,
                        ],
                        ao_flags: shade_ao | ((d as u16 | glow) << 8),
                        uv: uv_of(c),
                        layer: tile,
                        light: sky | (block << 8),
                        tint: [tint[0], tint[1], tint[2], 255],
                    });
                }
            }
        }
    }

    /// Height of a fluid surface in a cell, in 1/256 of a block.
    fn fluid_level(&self, kind: u16, x: i32, y: i32, z: i32) -> Option<u32> {
        let s = BlockState(self.cells[shell(x, y, z)]);
        if s.kind() != kind {
            return None;
        }
        let above = BlockState(self.cells[shell(x, y + 1, z)]);
        if above.kind() == kind || s.meta() & 8 != 0 {
            return Some(256);
        }
        let level = (s.meta() & 7) as u32;
        Some(if level == 0 { 228 } else { 228 * (9 - level) / 9 })
    }

    fn fluid(&self, state: BlockState, x: i32, y: i32, z: i32, y0: i32, reg: &Registry, out: &mut SectionMesh) {
        let def = reg.block(state);
        let kind = state.kind();
        let tint = self.tint_for(def.tint, x, z);
        let layer = reg.face_tile(kind, 1);
        let (fluid_flag, list) = if def.fluid == Fluid::Water {
            (flags::WATER, &mut out.translucent)
        } else {
            (flags::LAVA, &mut out.opaque)
        };

        // A corner stands as high as the highest of the four cells around it, or the full block if
        // the same fluid is above any of them.
        let corner_height = |dx: i32, dz: i32| -> u32 {
            let mut best = 0;
            for (ox, oz) in [(0, 0), (dx, 0), (0, dz), (dx, dz)] {
                let (cx, cz) = (x + ox, z + oz);
                if BlockState(self.cells[shell(cx, y + 1, cz)]).kind() == kind {
                    return 256;
                }
                if let Some(h) = self.fluid_level(kind, cx, y, cz) {
                    best = best.max(h);
                }
            }
            best
        };
        // Corners by name: (x, z) = nw, ne, se, sw.
        let (hnw, hne, hse, hsw) =
            (corner_height(-1, -1), corner_height(1, -1), corner_height(1, 1), corner_height(-1, 1));
        let lit = |lx: i32, ly: i32, lz: i32| -> u16 {
            let l = self.light.packed(lx, y0 + ly, lz);
            ((l >> 4) as u16 * 17) | (((l & 15) as u16 * 17) << 8)
        };
        let px = |v: i32| (v * 256) as u16 + POS_BIAS;
        let py = |h: u32| (y as u32 * 256 + h) as u16 + POS_BIAS;
        let (x0, x1, z0, z1) = (px(x), px(x + 1), px(z), px(z + 1));
        let (wx, wz) = (((x & 15) * 256) as u16, ((z & 15) * 256) as u16);
        let mut quad = |corners: [[u16; 3]; 4], uvs: [[u16; 2]; 4], light: u16, face: Dir| {
            for k in 0..4 {
                list.push(Vertex {
                    pos: corners[k],
                    ao_flags: 255 | ((face as u16 | fluid_flag) << 8),
                    uv: uvs[k],
                    layer,
                    light,
                    tint: [tint[0], tint[1], tint[2], 255],
                });
            }
        };
        let above = BlockState(self.cells[shell(x, y + 1, z)]);
        if above.kind() != kind && !reg.lut.opaque[above.kind() as usize] {
            let uv = [[wx, wz], [wx + 256, wz], [wx + 256, wz + 256], [wx, wz + 256]];
            quad(
                [[x0, py(hnw), z0], [x0, py(hsw), z1], [x1, py(hse), z1], [x1, py(hne), z0]],
                [uv[0], uv[3], uv[2], uv[1]],
                lit(x, y + 1, z),
                Dir::Up,
            );
        }
        let below = BlockState(self.cells[shell(x, y - 1, z)]);
        if below.kind() != kind && !reg.lut.opaque[below.kind() as usize] {
            let uv = [[wx, wz], [wx + 256, wz], [wx + 256, wz + 256], [wx, wz + 256]];
            quad([[x0, py(0), z1], [x0, py(0), z0], [x1, py(0), z0], [x1, py(0), z1]], uv, lit(x, y - 1, z), Dir::Down);
        }
        let side = |dx: i32, dz: i32| -> bool {
            let s = BlockState(self.cells[shell(x + dx, y, z + dz)]);
            s.kind() != kind && !reg.lut.opaque[s.kind() as usize]
        };
        let v = |h: u32| (256 - h.min(256)) as u16;
        if side(0, -1) {
            quad(
                [[x1, py(hne), z0], [x1, py(0), z0], [x0, py(0), z0], [x0, py(hnw), z0]],
                [[0, v(hne)], [0, 256], [256, 256], [256, v(hnw)]],
                lit(x, y, z - 1),
                Dir::North,
            );
        }
        if side(0, 1) {
            quad(
                [[x0, py(hsw), z1], [x0, py(0), z1], [x1, py(0), z1], [x1, py(hse), z1]],
                [[0, v(hsw)], [0, 256], [256, 256], [256, v(hse)]],
                lit(x, y, z + 1),
                Dir::South,
            );
        }
        if side(-1, 0) {
            quad(
                [[x0, py(hnw), z0], [x0, py(0), z0], [x0, py(0), z1], [x0, py(hsw), z1]],
                [[0, v(hnw)], [0, 256], [256, 256], [256, v(hsw)]],
                lit(x - 1, y, z),
                Dir::West,
            );
        }
        if side(1, 0) {
            quad(
                [[x1, py(hse), z1], [x1, py(0), z1], [x1, py(0), z0], [x1, py(hne), z0]],
                [[0, v(hse)], [0, 256], [256, 256], [256, v(hne)]],
                lit(x + 1, y, z),
                Dir::East,
            );
        }
    }
}

#[inline]
fn sign(v: [i32; 3]) -> i32 {
    v[0] + v[1] + v[2]
}

/// The four corners of a box's face, counter-clockwise from outside, in sixteenths.
fn box_face(b: &zenith_content::B16, d: Dir) -> [[u8; 3]; 4] {
    let ([x0, y0, z0], [x1, y1, z1]) = (b[0], b[1]);
    match d {
        Dir::Up => [[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]],
        Dir::Down => [[x0, y0, z1], [x0, y0, z0], [x1, y0, z0], [x1, y0, z1]],
        Dir::North => [[x1, y1, z0], [x1, y0, z0], [x0, y0, z0], [x0, y1, z0]],
        Dir::South => [[x0, y1, z1], [x0, y0, z1], [x1, y0, z1], [x1, y1, z1]],
        Dir::West => [[x0, y1, z0], [x0, y0, z0], [x0, y0, z1], [x0, y1, z1]],
        Dir::East => [[x1, y1, z1], [x1, y0, z1], [x1, y0, z0], [x1, y1, z0]],
    }
}

/// Writes a merged face as one quad. `p` is the corner cell with the lowest slice coordinates.
fn emit_merged(
    a: &FaceAxes,
    p: [i32; 3],
    u_axis: usize,
    v_axis: usize,
    w: i32,
    h: i32,
    key: &FaceKey,
    out: &mut SectionMesh,
) {
    // In section coordinates the rectangle covers `w` cells along the u axis and `h` along v.
    // Which side of a cell the face lies on depends on the direction it looks in.
    let n = a.n;
    let slice_axis = n.iter().position(|&c| c != 0).unwrap_or(0);
    let plane = p[slice_axis] + i32::from(n[slice_axis] > 0);
    let at = |i: i32, j: i32| -> [u16; 3] {
        let mut c = [0i32; 3];
        c[slice_axis] = plane;
        c[u_axis] = p[u_axis] + i;
        c[v_axis] = p[v_axis] + j;
        [(c[0] * 256) as u16 + POS_BIAS, (c[1] * 256) as u16 + POS_BIAS, (c[2] * 256) as u16 + POS_BIAS]
    };
    // The mask's i and j count up along the axes; textures want u and v to follow the face's own
    // direction, so a negative axis flips the coordinate.
    let (flip_u, flip_v) = (sign(a.u) < 0, sign(a.v) < 0);
    let uv = |i: i32, j: i32| -> [u16; 2] {
        let uu = if flip_u { w - i } else { i };
        let vv = if flip_v { h - j } else { j };
        [(uu * 256) as u16, (vv * 256) as u16]
    };
    // The key's corners are (-u,-v), (+u,-v), (+u,+v), (-u,+v) in axis coordinates, with `cu * sign`
    // already applied, so they correspond to the mask's low and high i and j directly.
    let corners = [(0, 0), (w, 0), (w, h), (0, h)];
    // Counter-clockwise from outside depends on how the slice and span axes are handed.
    let cross =
        |a: [i32; 3], b: [i32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let mut unit_u = [0; 3];
    unit_u[u_axis] = 1;
    let mut unit_v = [0; 3];
    unit_v[v_axis] = 1;
    let facing = cross(unit_u, unit_v)[slice_axis] * n[slice_axis];
    let order: [usize; 4] = if facing > 0 { [0, 1, 2, 3] } else { [0, 3, 2, 1] };
    // Split along the diagonal with the brighter corners, or dark corners smear across the quad.
    let ao_value = |k: usize| key.ao[k] as u32;
    let flip_diagonal = ao_value(0) + ao_value(2) < ao_value(1) + ao_value(3);
    let order = if flip_diagonal { [order[1], order[2], order[3], order[0]] } else { order };
    let list = match key.pass {
        0 => &mut out.opaque,
        1 => &mut out.cutout,
        _ => &mut out.translucent,
    };
    for k in order {
        let (i, j) = corners[k];
        list.push(Vertex {
            pos: at(i, j),
            ao_flags: (key.ao[k] as u16 * 85) | (key.flags << 8),
            uv: uv(i, j),
            layer: key.layer,
            light: key.sky[k] as u16 | ((key.block[k] as u16) << 8),
            tint: [key.tint[0], key.tint[1], key.tint[2], 255],
        });
    }
}

/// What a chunk's neighbourhood looks like to a worker: owned snapshots of the 3x3.
pub type HoodOwned = [[Option<Chunk>; 3]; 3];

/// Borrows a snapshot as the form the light and the mesher take.
pub fn hood_ref(h: &HoodOwned) -> Hood<'_> {
    std::array::from_fn(|z| std::array::from_fn(|x| h[z][x].as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_core::pos::SEA_LEVEL;

    fn registry() -> Registry {
        Registry::builtin()
    }
    fn generator() -> Generator {
        Generator::new(1)
    }

    fn mesh_of(c: &Chunk) -> Vec<MeshedSection> {
        let mut hood: Hood = [[None; 3]; 3];
        hood[1][1] = Some(c);
        let (reg, g) = (registry(), generator());
        Mesher::default().mesh(&hood, &reg, &g, u32::MAX, &[0; SECTIONS], false).expect("a mesh").sections
    }

    fn total(sections: &[MeshedSection]) -> usize {
        sections.iter().map(|s| s.mesh.quads()).sum()
    }

    fn position(v: &Vertex) -> [f32; 3] {
        [
            (v.pos[0] as f32 - POS_BIAS as f32) / 256.0,
            (v.pos[1] as f32 - POS_BIAS as f32) / 256.0,
            (v.pos[2] as f32 - POS_BIAS as f32) / 256.0,
        ]
    }

    fn normal(v: &Vertex) -> [f32; 3] {
        let d = Dir::from_u8(((v.ao_flags >> 8) & flags::NORMAL_MASK) as u8).expect("a direction");
        let o = d.offset();
        [o[0] as f32, o[1] as f32, o[2] as f32]
    }

    #[test]
    fn an_empty_chunk_has_no_triangles() {
        assert_eq!(total(&mesh_of(&Chunk::empty(ChunkPos::new(0, 0)))), 0);
    }

    #[test]
    fn a_lone_cube_has_six_faces() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        c.set(5, 70, 5, BlockState::of(id::STONE));
        assert_eq!(total(&mesh_of(&c)), 6);
    }

    #[test]
    fn faces_between_touching_cubes_are_not_drawn() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        c.set(5, 70, 5, BlockState::of(id::STONE));
        c.set(6, 70, 5, BlockState::of(id::STONE));
        let quads = total(&mesh_of(&c));
        // Two cubes side by side: ten faces, of which four pairs merge when lit alike.
        assert!((6..=10).contains(&quads), "{quads}");
    }

    #[test]
    fn every_face_winds_counter_clockwise_seen_from_outside() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        c.set(5, 70, 5, BlockState::of(id::STONE));
        c.set(9, 70, 5, BlockState::of(id::STONE_SLAB));
        c.set(9, 72, 9, BlockState::new(id::STONE_STAIRS, 2));
        c.set(3, 72, 9, BlockState::of(id::LAVA));
        for s in mesh_of(&c) {
            for quad in s.mesh.opaque.chunks(4) {
                let p: Vec<[f32; 3]> = quad.iter().map(position).collect();
                let (a, b) = (
                    [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]],
                    [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]],
                );
                let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
                let want = normal(&quad[0]);
                let dot = n[0] * want[0] + n[1] * want[1] + n[2] * want[2];
                assert!(dot > 0.0, "a quad with normal {want:?} winds {n:?}: {p:?}");
            }
        }
    }

    #[test]
    fn a_flat_floor_merges_into_very_few_quads() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        for x in 0..16 {
            for z in 0..16 {
                c.set(x, 60, z, BlockState::of(id::STONE));
            }
        }
        let hood: Hood = [[Some(&c); 3]; 3];
        let (reg, g) = (registry(), generator());
        let r = Mesher::default().mesh(&hood, &reg, &g, u32::MAX, &[0; SECTIONS], false).expect("a mesh");
        // Top and bottom: 256 cells each, with chunks of the same floor all around it.
        assert_eq!(total(&r.sections), 2, "256 x 2 faces should be 2 quads");
    }

    #[test]
    fn the_merged_quad_covers_the_same_area_as_its_faces() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        for x in 2..7 {
            for z in 4..9 {
                c.set(x, 60, z, BlockState::of(id::STONE));
            }
        }
        let area_up: f32 = mesh_of(&c)
            .iter()
            .flat_map(|s| s.mesh.opaque.chunks(4))
            .filter(|q| normal(&q[0])[1] > 0.0)
            .map(|q| {
                let p: Vec<[f32; 3]> = q.iter().map(position).collect();
                (p[2][0] - p[0][0]).abs() * (p[2][2] - p[0][2]).abs()
            })
            .sum();
        assert_eq!(area_up, 25.0);
    }

    #[test]
    fn a_corner_against_a_wall_is_darker_than_one_in_the_open() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        for x in 0..16 {
            for z in 0..16 {
                c.set(x, 60, z, BlockState::of(id::STONE));
            }
        }
        c.set(8, 61, 8, BlockState::of(id::STONE));
        let meshes = mesh_of(&c);
        let mut seen = std::collections::HashSet::new();
        for s in &meshes {
            for v in &s.mesh.opaque {
                // Positions are relative to the section, which starts at y = 48 here.
                if normal(v)[1] > 0.0 && position(v)[1] == 61.0 - 48.0 {
                    seen.insert(v.ao_flags & 255);
                }
            }
        }
        assert!(seen.contains(&255), "open floor is fully bright: {seen:?}");
        assert!(seen.iter().any(|&a| a < 255), "floor next to a block is darker: {seen:?}");
    }

    #[test]
    fn neighbouring_chunks_hide_the_faces_between_them() {
        let (mut a, mut b) = (Chunk::empty(ChunkPos::new(0, 0)), Chunk::empty(ChunkPos::new(1, 0)));
        a.set(15, 70, 5, BlockState::of(id::STONE));
        b.set(0, 70, 5, BlockState::of(id::STONE));
        let mut hood: Hood = [[None; 3]; 3];
        hood[1][1] = Some(&a);
        hood[1][2] = Some(&b);
        let (reg, g) = (registry(), generator());
        let r = Mesher::default().mesh(&hood, &reg, &g, u32::MAX, &[0; SECTIONS], false).expect("a mesh");
        assert_eq!(total(&r.sections), 5, "the face towards the neighbour is hidden");
    }

    #[test]
    fn plants_go_in_the_cutout_list_and_lean_inside_their_bounds() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        c.set(5, 70, 5, BlockState::of(id::TALL_GRASS));
        let meshes = mesh_of(&c);
        let m = meshes.iter().map(|s| &s.mesh).find(|m| !m.is_empty()).expect("a plant");
        assert!(m.opaque.is_empty());
        assert_eq!(m.cutout.len(), 8);
        for v in &m.cutout {
            let p = position(v);
            assert!((4.0..7.0).contains(&p[0]) && (4.0..7.0).contains(&p[2]), "{p:?}");
        }
    }

    #[test]
    fn water_is_translucent_and_only_shows_its_surface() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        for x in 0..16 {
            for z in 0..16 {
                for y in 50..=SEA_LEVEL {
                    c.set(x, y, z, BlockState::of(id::WATER));
                }
                c.set(x, 49, z, BlockState::of(id::STONE));
            }
        }
        let hood: Hood = [[Some(&c); 3]; 3];
        let (reg, g) = (registry(), generator());
        let r = Mesher::default().mesh(&hood, &reg, &g, u32::MAX, &[0; SECTIONS], false).expect("a mesh");
        let water: usize = r.sections.iter().map(|s| s.mesh.translucent.len() / 4).sum();
        assert_eq!(water, 256, "one surface quad per column, nothing inside");
        let top = r
            .sections
            .iter()
            .flat_map(|s| s.mesh.translucent.iter().map(|v| position(v)[1] + (MIN_Y + s.index as i32 * 16) as f32))
            .fold(f32::MIN, f32::max);
        assert!(
            top > SEA_LEVEL as f32 && top < SEA_LEVEL as f32 + 1.0,
            "the surface sits below the top of the block: {top}"
        );
    }

    #[test]
    fn a_flowing_edge_slopes_down_from_the_source() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        for x in 0..16 {
            for z in 0..16 {
                c.set(x, 60, z, BlockState::of(id::STONE));
            }
        }
        c.set(5, 61, 5, BlockState::of(id::WATER));
        c.set(6, 61, 5, BlockState::new(id::WATER, 4));
        let meshes = mesh_of(&c);
        let mut heights: Vec<u16> =
            meshes.iter().flat_map(|s| &s.mesh.translucent).filter(|v| normal(v)[1] > 0.0).map(|v| v.pos[1]).collect();
        heights.sort_unstable();
        heights.dedup();
        assert!(heights.len() >= 2, "a level surface: {heights:?}");
    }

    #[test]
    fn slabs_only_hide_what_they_really_cover() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        c.set(5, 70, 5, BlockState::of(id::STONE_SLAB));
        c.set(5, 69, 5, BlockState::of(id::STONE));
        // The stone keeps all six faces, since a slab is no full block. The slab loses the one it
        // rests on, since stone is.
        assert_eq!(total(&mesh_of(&c)), 6 + 5);
    }

    #[test]
    fn an_unchanged_chunk_is_not_meshed_again() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        c.set(5, 70, 5, BlockState::of(id::STONE));
        let mut hood: Hood = [[None; 3]; 3];
        hood[1][1] = Some(&c);
        let (reg, g) = (registry(), generator());
        let mut m = Mesher::default();
        let first = m.mesh(&hood, &reg, &g, u32::MAX, &[0; SECTIONS], false).expect("a mesh");
        let again = m.mesh(&hood, &reg, &g, 0, &first.light_hashes, false).expect("a mesh");
        assert!(again.sections.is_empty(), "nothing changed, nothing to do");
    }

    #[test]
    fn new_light_remeshes_a_section_nobody_edited() {
        let mut c = Chunk::empty(ChunkPos::new(0, 0));
        for x in 0..16 {
            for z in 0..16 {
                c.set(x, 60, z, BlockState::of(id::STONE));
                c.set(x, 80, z, BlockState::of(id::STONE));
            }
        }
        let (reg, g) = (registry(), generator());
        let mut m = Mesher::default();
        let first = {
            let hood: Hood = [[Some(&c); 3]; 3];
            m.mesh(&hood, &reg, &g, u32::MAX, &[0; SECTIONS], false).expect("a mesh")
        };
        // A torch in the room: its light reaches the floor's section, which holds no edit.
        c.set(8, 62, 8, BlockState::of(id::TORCH));
        let hood: Hood = [[Some(&c); 3]; 3];
        let torch_section = ((62 - MIN_Y) / 16) as usize;
        let second = m.mesh(&hood, &reg, &g, 1 << torch_section, &first.light_hashes, false).expect("a mesh");
        let indices: Vec<u8> = second.sections.iter().map(|s| s.index).collect();
        assert!(indices.contains(&(torch_section as u8)));
        let lit = second.sections.iter().flat_map(|s| s.mesh.opaque.iter()).any(|v| v.light >> 8 > 0);
        assert!(lit, "the floor did not pick up the torch's light");
    }

    #[test]
    fn real_terrain_merges_well_and_stays_within_bounds() {
        let g = generator();
        let reg = registry();
        let (sx, _, sz) = g.find_spawn();
        let (cx, cz) = (sx >> 4, sz >> 4);
        let chunks: Vec<Vec<Chunk>> =
            (cz - 1..=cz + 1).map(|z| (cx - 1..=cx + 1).map(|x| g.chunk(ChunkPos::new(x, z))).collect()).collect();
        let hood: Hood = std::array::from_fn(|z| std::array::from_fn(|x| Some(&chunks[z][x])));
        let r = Mesher::default().mesh(&hood, &reg, &g, u32::MAX, &[0; SECTIONS], false).expect("a mesh");
        let quads = total(&r.sections);
        assert!(quads > 50, "only {quads} quads on real terrain");
        for s in &r.sections {
            for v in s.mesh.opaque.iter().chain(&s.mesh.cutout).chain(&s.mesh.translucent) {
                let p = position(v);
                assert!(p.iter().all(|c| (-1.0..=17.0).contains(c)), "{p:?}");
                assert!((v.layer as usize) < reg.tiles().len());
            }
        }
    }

    #[test]
    fn meshing_is_a_pure_function_of_the_blocks() {
        let g = generator();
        let reg = registry();
        let chunks: Vec<Vec<Chunk>> = (0..3).map(|z| (0..3).map(|x| g.chunk(ChunkPos::new(x, z))).collect()).collect();
        let hood: Hood = std::array::from_fn(|z| std::array::from_fn(|x| Some(&chunks[z][x])));
        let mut m = Mesher::default();
        let (a, b) = (
            m.mesh(&hood, &reg, &g, u32::MAX, &[0; SECTIONS], false).expect("a mesh"),
            Mesher::default().mesh(&hood, &reg, &g, u32::MAX, &[0; SECTIONS], false).expect("a mesh"),
        );
        assert_eq!(a.light_hashes, b.light_hashes);
        for (x, y) in a.sections.iter().zip(&b.sections) {
            assert_eq!(x.mesh, y.mesh);
        }
    }
}
