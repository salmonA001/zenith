//! Ray tracing, on GPUs that can: the chunks near the player as acceleration structures, which the
//! terrain shaders trace for sun shadows and water reflections (see terrain_traced.wgsl).
//!
//! Each section becomes a bottom-level structure built straight from its vertices in the arena. The
//! builder reads a vertex's position as signed normalised 16-bit numbers, and the instance's transform
//! scales those back to blocks, so nothing is copied. One top-level structure holds them all, each in a
//! slot of its own, placed relative to an anchor that moves only when the camera has gone a long way.
//! So the top level is rebuilt when sections come, go or change, and not every time the camera moves;
//! the shaders add the camera's offset from the anchor.
use super::GpuSection;
use super::arena::Span;
use zenith_core::glam::{DVec3, Vec3};
use zenith_core::pos::MIN_Y;
use zenith_core::{ChunkPos, FxMap};

/// Chunks around the camera, in every direction, whose sections are traced. Beyond them nothing casts
/// a shadow, as the shadow map reaches less far still, and water reflects only the sky.
pub const RADIUS: i32 = 16;
/// Slots in the top level. The radius takes in about 1,100 chunks, and a chunk rarely has more than
/// ten sections with anything in them.
const MAX_INSTANCES: u32 = 32 * 1024;
/// Triangles a frame may build into new structures, so a view arriving all at once does not stall the
/// frames that show it. A section that changes under the player is rebuilt at once, whatever this says.
const BUILD_BUDGET: u32 = 250_000;
/// How far the camera may get from the anchor before every instance is placed again around a new one.
/// Out to here a float still places a ray within a ten-thousandth of a block.
const ANCHOR_REACH: f64 = 1024.0;
/// `zenith_mesh::Vertex` keeps positions in 1/256ths of a block, biased and well under 32,768, which
/// the builder reads divided by 32,767.
const UNITS: f32 = 32767.0 / 256.0;
const BIAS: f32 = zenith_mesh::POS_BIAS as f32 / 256.0;
// A plant may lean a little out of its block, but no position comes near the sign bit.
const _: () = assert!(16 * 256 + 2 * zenith_mesh::POS_BIAS < i16::MAX as u16);
/// Marks a cutout geometry in the table of starts the shaders read.
const CUTOUT: u32 = 1 << 31;

/// What a device needs for any of this.
pub fn features() -> wgpu::Features {
    wgpu::Features::EXPERIMENTAL_RAY_QUERY | wgpu::Features::EXTENDED_ACCELERATION_STRUCTURE_VERTEX_FORMATS
}

type Key = (ChunkPos, u8);

struct Built {
    blas: wgpu::Blas,
    /// Where each geometry starts in the arena, as the shaders look them up.
    starts: [u32; 2],
    triangles: u32,
    slot: u32,
}

/// A structure made this frame, with what its build reads.
struct Shape {
    key: Key,
    sizes: Vec<wgpu::BlasTriangleGeometrySizeDescriptor>,
    spans: Vec<Span>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TraceStats {
    /// Sections in the top level.
    pub sections: usize,
    /// Sections in range still waiting for their structure.
    pub waiting: usize,
    pub triangles: u64,
}

pub struct Tracer {
    tlas: wgpu::Tlas,
    pub layout: wgpu::BindGroupLayout,
    pub bind: wgpu::BindGroup,
    table: wgpu::Buffer,
    origin: wgpu::Buffer,
    built: FxMap<Key, Built>,
    /// Sections in range whose structure is missing or out of date.
    waiting: FxMap<Key, ()>,
    /// The table the shaders read, slot by slot, as the GPU should have it.
    starts: Vec<[u32; 2]>,
    free: Vec<u32>,
    anchor: Option<DVec3>,
    /// The chunk the camera was in, the last time anyone looked.
    center: Option<ChunkPos>,
    /// Whether the top level needs building again.
    changed: bool,
    active: bool,
    pub stats: TraceStats,
}

impl Tracer {
    pub fn new(device: &wgpu::Device, arena: &wgpu::Buffer) -> Self {
        let tlas = device.create_tlas(&wgpu::CreateTlasDescriptor {
            label: Some("traced sections"),
            max_instances: MAX_INSTANCES,
            flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
            update_mode: wgpu::AccelerationStructureUpdateMode::Build,
        });
        let table = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("traced section starts"),
            size: MAX_INSTANCES as u64 * 8,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let origin = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("traced origin"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let entry = |binding: u32, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let buffer = |ty: wgpu::BufferBindingType| wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("traced"),
            entries: &[
                entry(0, wgpu::BindingType::AccelerationStructure { vertex_return: false }),
                entry(1, buffer(wgpu::BufferBindingType::Storage { read_only: true })),
                entry(2, buffer(wgpu::BufferBindingType::Storage { read_only: true })),
                entry(3, buffer(wgpu::BufferBindingType::Uniform)),
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("traced"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: tlas.as_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: arena.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: table.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: origin.as_entire_binding() },
            ],
        });
        Self {
            tlas,
            layout,
            bind,
            table,
            origin,
            built: FxMap::default(),
            waiting: FxMap::default(),
            starts: Vec::new(),
            free: Vec::new(),
            anchor: None,
            center: None,
            changed: true,
            active: false,
            stats: TraceStats::default(),
        }
    }

    /// Starts or stops tracing. Stopping lets every structure go; starting builds them all again.
    pub fn set_active(&mut self, on: bool) {
        if on != self.active {
            self.clear();
            self.active = on;
        }
    }

    pub fn clear(&mut self) {
        let keys: Vec<Key> = self.built.keys().copied().collect();
        for key in keys {
            self.release(key);
        }
        self.waiting.clear();
        self.starts.clear();
        self.free.clear();
        self.anchor = None;
        self.center = None;
        self.changed = true;
        self.stats = TraceStats::default();
    }

    /// A section's mesh is new or has changed.
    pub fn changed(&mut self, key: Key) {
        if self.active && self.center.is_none_or(|c| key.0.distance(c) <= RADIUS) {
            self.waiting.insert(key, ());
        }
    }

    /// A section is gone, or has nothing left in it.
    pub fn gone(&mut self, key: Key) {
        self.release(key);
        self.waiting.remove(&key);
    }

    fn release(&mut self, key: Key) {
        if let Some(b) = self.built.remove(&key) {
            self.tlas[b.slot as usize] = None;
            self.free.push(b.slot);
            self.changed = true;
        }
    }

    fn place(&mut self, key: Key, slot: u32) {
        let (Some(b), Some(anchor)) = (self.built.get(&key), self.anchor) else { return };
        self.tlas[slot as usize] = Some(wgpu::TlasInstance::new(&b.blas, transform(key, anchor), slot, 0xff));
        self.starts[slot as usize] = b.starts;
        self.changed = true;
    }

    /// Builds what changed, as much as this frame allows, and the top level if anything in it moved,
    /// recording the work in `encoder` ahead of the passes that trace.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        sections: &FxMap<Key, GpuSection>,
        camera: DVec3,
        arena: &wgpu::Buffer,
        indices: &wgpu::Buffer,
    ) {
        // Far from the anchor, everything is placed again around a new one.
        if self.anchor.is_none_or(|a| (camera.x - a.x).abs().max((camera.z - a.z).abs()) > ANCHOR_REACH) {
            self.anchor = Some(DVec3::new((camera.x / 512.0).round() * 512.0, 0.0, (camera.z / 512.0).round() * 512.0));
            let placed: Vec<(Key, u32)> = self.built.iter().map(|(&k, b)| (k, b.slot)).collect();
            for (key, slot) in placed {
                self.place(key, slot);
            }
        }
        let anchor = self.anchor.unwrap_or_default();
        let offset = (camera - anchor).as_vec3();
        queue.write_buffer(&self.origin, 0, bytemuck::cast_slice(&[offset.x, offset.y, offset.z, 0.0]));

        // In a new chunk, what has fallen out of range goes, and what has come into it is wanted.
        let center = ChunkPos::new((camera.x / 16.0).floor() as i32, (camera.z / 16.0).floor() as i32);
        let in_range = |k: &Key| k.0.distance(center) <= RADIUS;
        if self.center != Some(center) {
            self.center = Some(center);
            let gone: Vec<Key> = self.built.keys().copied().filter(|k| !in_range(k)).collect();
            for key in gone {
                self.release(key);
            }
            self.waiting.retain(|k, _| in_range(k));
            for key in sections.keys().filter(|k| in_range(k) && !self.built.contains_key(*k)) {
                self.waiting.insert(*key, ());
            }
        }

        // Changes to what is already traced first, all of them, so an edit never leaves a hole for a
        // frame; then new sections, nearest first, as far as the budget goes.
        let mut edits = Vec::new();
        let mut fresh = Vec::new();
        for k in self.waiting.keys() {
            if self.built.contains_key(k) { edits.push(*k) } else { fresh.push(*k) }
        }
        fresh.sort_unstable_by_key(|k| k.0.distance_sq(center));
        let mut budget = BUILD_BUDGET;
        let mut shapes = Vec::new();
        let queued = edits.into_iter().map(|k| (k, true)).chain(fresh.into_iter().map(|k| (k, false)));
        for (key, is_edit) in queued {
            if !is_edit && budget == 0 {
                break;
            }
            let Some(section) = sections.get(&key) else {
                self.gone(key);
                continue;
            };
            let mut sizes = Vec::new();
            let mut spans = Vec::new();
            let mut starts = [0u32; 2];
            for (span, cutout) in [(section.opaque, false), (section.cutout, true)] {
                if span.len == 0 {
                    continue;
                }
                starts[sizes.len()] = span.start | if cutout { CUTOUT } else { 0 };
                sizes.push(wgpu::BlasTriangleGeometrySizeDescriptor {
                    vertex_format: wgpu::VertexFormat::Snorm16x4,
                    vertex_count: span.len,
                    index_format: Some(wgpu::IndexFormat::Uint32),
                    index_count: Some(span.len / 4 * 6),
                    // A cutout is tested against its texture in the shader. Anything else stops a ray.
                    flags: if cutout {
                        wgpu::AccelerationStructureGeometryFlags::empty()
                    } else {
                        wgpu::AccelerationStructureGeometryFlags::OPAQUE
                    },
                });
                spans.push(span);
            }
            if sizes.is_empty() {
                // Only water: nothing in it stops a ray.
                self.gone(key);
                continue;
            }
            let slot = match self.built.get(&key) {
                Some(b) => b.slot,
                None => match self.free.pop() {
                    Some(slot) => slot,
                    None if self.starts.len() < MAX_INSTANCES as usize => {
                        self.starts.push([0, 0]);
                        self.starts.len() as u32 - 1
                    }
                    // Full: the rest wait for room.
                    None => break,
                },
            };
            self.waiting.remove(&key);
            let triangles = (section.opaque.len + section.cutout.len) / 2;
            budget = budget.saturating_sub(triangles);
            let blas = device.create_blas(
                &wgpu::CreateBlasDescriptor {
                    label: None,
                    flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                    update_mode: wgpu::AccelerationStructureUpdateMode::Build,
                },
                wgpu::BlasGeometrySizeDescriptors::Triangles { descriptors: sizes.clone() },
            );
            self.built.insert(key, Built { blas, starts, triangles, slot });
            self.place(key, slot);
            shapes.push(Shape { key, sizes, spans });
        }

        if std::mem::take(&mut self.changed) {
            if !self.starts.is_empty() {
                queue.write_buffer(&self.table, 0, bytemuck::cast_slice(&self.starts));
            }
            let entries: Vec<wgpu::BlasBuildEntry<'_>> = shapes
                .iter()
                .filter_map(|shape| {
                    let blas = &self.built.get(&shape.key)?.blas;
                    let geometries = shape
                        .sizes
                        .iter()
                        .zip(&shape.spans)
                        .map(|(size, span)| wgpu::BlasTriangleGeometry {
                            size,
                            vertex_buffer: arena,
                            first_vertex: span.start,
                            vertex_stride: super::VERTEX_SIZE,
                            index_buffer: Some(indices),
                            first_index: Some(0),
                            transform_buffer: None,
                            transform_buffer_offset: None,
                        })
                        .collect();
                    Some(wgpu::BlasBuildEntry { blas, geometry: wgpu::BlasGeometries::TriangleGeometries(geometries) })
                })
                .collect();
            encoder.build_acceleration_structures(entries.iter(), std::iter::once(&self.tlas));
            self.stats.triangles = self.built.values().map(|b| b.triangles as u64).sum();
        }
        self.stats.sections = self.built.len();
        self.stats.waiting = self.waiting.len();
    }
}

/// Where a section's traced copy goes: its vertices, read as fractions, scaled back to blocks and
/// placed relative to the anchor. Rows of a 3x4 matrix.
fn transform(key: Key, anchor: DVec3) -> [f32; 12] {
    let (pos, y) = key;
    let min = DVec3::new(pos.x as f64 * 16.0, MIN_Y as f64 + y as f64 * 16.0, pos.z as f64 * 16.0) - anchor;
    let at = min.as_vec3() - Vec3::splat(BIAS);
    [UNITS, 0.0, 0.0, at.x, 0.0, UNITS, 0.0, at.y, 0.0, 0.0, UNITS, at.z]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies a transform to a position the way the builder reads a vertex: as a fraction of 32,767.
    fn place(m: [f32; 12], raw: [u16; 3]) -> Vec3 {
        let p = Vec3::new(raw[0] as f32, raw[1] as f32, raw[2] as f32) / 32767.0;
        Vec3::new(
            m[0] * p.x + m[1] * p.y + m[2] * p.z + m[3],
            m[4] * p.x + m[5] * p.y + m[6] * p.z + m[7],
            m[8] * p.x + m[9] * p.y + m[10] * p.z + m[11],
        )
    }

    #[test]
    fn a_traced_vertex_lands_where_the_chunk_pass_draws_it() {
        // The shaders work relative to the camera and add the camera's offset from the anchor.
        let camera = DVec3::new(1000.25, 70.5, -333.75);
        let anchor = DVec3::new(1024.0, 0.0, -512.0);
        let key = (ChunkPos::new(62, -21), 8u8);
        // A corner of the block at 3, 5, 7 in the section: what the terrain shader computes.
        let raw = [3 * 256 + zenith_mesh::POS_BIAS, 5 * 256 + zenith_mesh::POS_BIAS, 7 * 256 + zenith_mesh::POS_BIAS];
        let drawn = DVec3::new(62.0 * 16.0 + 3.0, MIN_Y as f64 + 8.0 * 16.0 + 5.0, -21.0 * 16.0 + 7.0) - camera;
        let traced = place(transform(key, anchor), raw) - (camera - anchor).as_vec3();
        assert!((traced - drawn.as_vec3()).abs().max_element() < 1e-3, "{traced} against {drawn}");
    }
}
