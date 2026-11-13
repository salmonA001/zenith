//! The renderer: everything that touches the GPU.
//!
//! All positions on the GPU are relative to the camera, so a world can be as big as it likes without
//! the shaking floats cause far from the origin. Depth is reversed and has no far plane, so the
//! horizon is as sharp as the ground at your feet.
pub mod arena;
pub mod far;
pub mod font;
pub mod rt;
pub mod textures;
pub mod ui;

use arena::{Arena, Span};
use far::{FarTerrain, FarVertex};
use glam::{DVec3, Mat4, Vec3, Vec4};
use std::sync::Arc;
use textures::{
    FINE, all_icons, crack_tiles, enlarge, material, material_chain, mip_chain, paint, pixelate, plain_material, refine,
};
use ui::{Atlas, UiBatch, UiVertex};
use wgpu::util::DeviceExt;
use zenith_content::Registry;
use zenith_core::{ChunkPos, FxMap};
use zenith_mesh::{SectionMesh, Vertex};
use zenith_world::Generator;

pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const LDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const SHADOW_SIZE: u32 = 2048;
/// Half the width of each shadow cascade, in blocks.
const CASCADES: [f32; 2] = [36.0, 170.0];
const VERTEX_SIZE: u64 = std::mem::size_of::<Vertex>() as u64;
/// Vertices in the shared arena: about 800 MB at 20 bytes each, if the GPU allows it. A phone shares
/// a few gigabytes between everything it runs, and gets 120 MB.
const ARENA_VERTICES: u64 = if cfg!(target_os = "android") { 6_000_000 } else { 40_000_000 };
/// Enough for the worst section there can be.
const MAX_QUADS: u32 = 200_000;
const MAX_INSTANCES: u64 = 64 * 1024;
const NEAR: f32 = 0.05;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EntityVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub layer: u32,
    pub color: [u8; 4],
    pub normal: [i8; 4],
    pub light: [u8; 2],
    pub _pad: [u8; 2],
}

pub const NO_TEXTURE: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    inv_view_proj: [[f32; 4]; 4],
    shadow: [[[f32; 4]; 4]; 2],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    ambient: [f32; 4],
    sky_zenith: [f32; 4],
    sky_horizon: [f32; 4],
    camera: [f32; 4],
    fog: [f32; 4],
    screen: [f32; 4],
    sun_dir: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct PostParams {
    texel: [f32; 4],
    effects: [f32; 4],
}

/// What the camera sees this frame.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub pos: DVec3,
    pub yaw_deg: f64,
    pub pitch_deg: f64,
    pub fov_deg: f64,
}

impl Camera {
    pub fn forward(&self) -> Vec3 {
        let (y, p) = (self.yaw_deg.to_radians(), self.pitch_deg.to_radians());
        Vec3::new((-y.sin() * p.cos()) as f32, (-p.sin()) as f32, (y.cos() * p.cos()) as f32)
    }
}

/// How the sky and the light look at a given time of day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sky {
    pub sun_dir: Vec3,
    pub light_dir: Vec3,
    pub light_strength: f32,
    pub light_color: Vec3,
    pub ambient: Vec3,
    pub zenith: Vec3,
    pub horizon: Vec3,
    pub stars: f32,
    pub day: f32,
}

fn smooth(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `time` is 0-24000: 0 sunrise, 6000 noon, 12000 sunset, 18000 midnight.
pub fn sky_at(time: f32, rain: f32) -> Sky {
    let angle = time / 24000.0 * std::f32::consts::TAU;
    let sun_dir = Vec3::new(angle.cos(), angle.sin(), 0.32).normalize();
    let h = sun_dir.y;
    let day = smooth(-0.14, 0.12, h);
    let golden = 1.0 - smooth(0.02, 0.5, h.abs());
    let high = smooth(0.0, 0.55, h);

    let sun_light = Vec3::new(1.0, 0.56, 0.28).lerp(Vec3::new(1.0, 0.95, 0.86), high) * (0.95 + high * 0.55);
    let moon_light = Vec3::new(0.34, 0.42, 0.62) * 0.22;
    let (light_dir, light_color, strength) =
        if h > 0.0 { (sun_dir, sun_light, smooth(0.0, 0.1, h)) } else { (-sun_dir, moon_light, smooth(0.0, 0.1, -h)) };

    let night_ambient = Vec3::new(0.018, 0.026, 0.05);
    let dusk_ambient = Vec3::new(0.22, 0.17, 0.2);
    let noon_ambient = Vec3::new(0.27, 0.33, 0.44);
    let ambient = night_ambient.lerp(dusk_ambient.lerp(noon_ambient, high), day);

    let night_zenith = Vec3::new(0.004, 0.007, 0.018);
    let dusk_zenith = Vec3::new(0.14, 0.2, 0.42);
    let noon_zenith = Vec3::new(0.13, 0.34, 0.74);
    let zenith = night_zenith.lerp(dusk_zenith.lerp(noon_zenith, high), day);

    let night_horizon = Vec3::new(0.018, 0.028, 0.055);
    let dusk_horizon = Vec3::new(0.86, 0.5, 0.34);
    let noon_horizon = Vec3::new(0.62, 0.78, 0.96);
    let horizon = night_horizon.lerp(dusk_horizon.lerp(noon_horizon, 1.0 - golden), day);

    let wet = rain.clamp(0.0, 1.0);
    let grey = |c: Vec3, by: f32| -> Vec3 {
        let v = c.dot(Vec3::new(0.3, 0.6, 0.1));
        c.lerp(Vec3::splat(v), wet * 0.85) * (1.0 - wet * by)
    };
    Sky {
        sun_dir,
        light_dir,
        light_strength: strength * (1.0 - wet * 0.82),
        light_color,
        ambient: grey(ambient, 0.3),
        zenith: grey(zenith, 0.45),
        horizon: grey(horizon, 0.3),
        stars: 1.0 - smooth(-0.3, 0.02, h),
        day,
    }
}

/// A right-handed view looking along `dir` from `eye`.
pub fn look_to(eye: Vec3, dir: Vec3, up: Vec3) -> Mat4 {
    let f = dir.normalize();
    let s = f.cross(up).normalize();
    let u = s.cross(f);
    Mat4::from_cols(
        Vec4::new(s.x, u.x, -f.x, 0.0),
        Vec4::new(s.y, u.y, -f.y, 0.0),
        Vec4::new(s.z, u.z, -f.z, 0.0),
        Vec4::new(-eye.dot(s), -eye.dot(u), eye.dot(f), 1.0),
    )
}

/// A perspective with reversed depth and no far plane: depth is `NEAR / distance`.
pub fn perspective(fov_y: f32, aspect: f32) -> Mat4 {
    let f = 1.0 / (fov_y * 0.5).tan();
    Mat4::from_cols(
        Vec4::new(f / aspect, 0.0, 0.0, 0.0),
        Vec4::new(0.0, f, 0.0, 0.0),
        Vec4::new(0.0, 0.0, 0.0, -1.0),
        Vec4::new(0.0, 0.0, NEAR, 0.0),
    )
}

/// A square orthographic volume, `half` either way, with depth running 0 to 1 over `depth`.
fn orthographic(half: f32, depth: f32) -> Mat4 {
    Mat4::from_cols(
        Vec4::new(1.0 / half, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 1.0 / half, 0.0, 0.0),
        Vec4::new(0.0, 0.0, -1.0 / depth, 0.0),
        Vec4::new(0.0, 0.0, 0.0, 1.0),
    )
}

/// The six sides of a view volume, as planes pointing inwards.
fn frustum_planes(m: Mat4) -> [Vec4; 6] {
    let (r0, r1, r2, r3) = (m.row(0), m.row(1), m.row(2), m.row(3));
    [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2, r3 - r2]
}

fn box_in_frustum(planes: &[Vec4; 6], min: Vec3, max: Vec3) -> bool {
    planes.iter().all(|p| {
        let far = Vec3::new(
            if p.x >= 0.0 { max.x } else { min.x },
            if p.y >= 0.0 { max.y } else { min.y },
            if p.z >= 0.0 { max.z } else { min.z },
        );
        p.x * far.x + p.y * far.y + p.z * far.z + p.w >= 0.0
    })
}

#[derive(Clone, Copy, Default)]
struct GpuSection {
    opaque: Span,
    cutout: Span,
    water: Span,
}

#[derive(Clone, Copy)]
struct Draw {
    span: Span,
    instance: u32,
    distance: f32,
}

/// Numbers worth showing on the debug screen.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    pub sections: usize,
    pub drawn: usize,
    pub shadow_drawn: usize,
    pub quads: u64,
    pub arena_used_mb: f32,
    pub arena_capacity_mb: f32,
    pub far_tiles: usize,
    pub far_pending: usize,
    pub out_of_memory: u64,
    pub traced: rt::TraceStats,
}

/// Whether the world is lit by traced rays.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Tracing {
    #[default]
    Off,
    On,
    /// This machine cannot, and why.
    Unavailable(String),
}

impl Tracing {
    pub fn describe(&self) -> String {
        match self {
            Tracing::Off => "off".into(),
            Tracing::On => "on".into(),
            Tracing::Unavailable(why) => format!("unavailable: {why}"),
        }
    }
}

/// The chunk pipelines that trace rays instead of reading the shadow map.
struct TracedPipelines {
    opaque: wgpu::RenderPipeline,
    cutout: wgpu::RenderPipeline,
    water: wgpu::RenderPipeline,
}

/// One frame's inputs.
pub struct Frame<'a> {
    pub camera: Camera,
    pub time_of_day: f32,
    pub rain: f32,
    pub seconds: f32,
    pub under_water: bool,
    pub under_lava: bool,
    pub hurt_flash: f32,
    pub dead: f32,
    pub entities: &'a [EntityVertex],
    pub alpha: &'a [EntityVertex],
    pub view_model: &'a [EntityVertex],
    pub ui: &'a UiBatch,
    pub draw_far: bool,
    /// Trace rays for shadows and reflections, if this machine can. Only the modern look has them.
    pub ray_tracing: bool,
    /// The classic look: big-pixel textures, flat shading by face, blocky clouds, no bloom.
    pub classic: bool,
    pub debug: DebugView,
}

/// The switches on the debug options screen (F6). The renderer reads fog, clouds and full bright; the
/// game draws the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugView {
    pub graph: bool,
    pub chunk_borders: bool,
    pub hitboxes: bool,
    pub fog: bool,
    pub clouds: bool,
    /// Everything lit as if by a lamp: for looking around caves and the night.
    pub full_bright: bool,
}

impl Default for DebugView {
    fn default() -> Self {
        Self { graph: false, chunk_borders: false, hitboxes: false, fog: true, clouds: true, full_bright: false }
    }
}

impl DebugView {
    /// The renderer's switches as the shaders read them, in `sun_dir.w`: 1 hides clouds, 2 lights
    /// everything, and 4, set when the classic look is ray traced, lights it fully: sun, shadows,
    /// materials, glints, though its pictures and sky stay classic.
    fn shader_flags(self, full_light: bool) -> f32 {
        (u32::from(!self.clouds) | (u32::from(self.full_bright) << 1) | (u32::from(full_light) << 2)) as f32
    }
}

struct Targets {
    width: u32,
    height: u32,
    hdr_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    ldr: wgpu::Texture,
    ldr_view: wgpu::TextureView,
    bloom_views: Vec<wgpu::TextureView>,
    down_bind: Vec<wgpu::BindGroup>,
    up_bind: Vec<wgpu::BindGroup>,
    final_bind: wgpu::BindGroup,
    final_params: wgpu::Buffer,
    water_bind: wgpu::BindGroup,
    blit_bind: wgpu::BindGroup,
}

/// How many chunks across the map of where real chunks stand is. It wraps around, so it only has to
/// be wider than the view, and the shaders' wrapped camera position must be a multiple of this.
const NEAR_MASK: usize = 128;

/// What stays put when a new set of textures arrives.
struct SkinParts {
    world_layout: wgpu::BindGroupLayout,
    entity_layout: wgpu::BindGroupLayout,
    ui_layout: wgpu::BindGroupLayout,
    tile_sampler: wgpu::Sampler,
    entity_sampler: wgpu::Sampler,
    ui_sampler: wgpu::Sampler,
    shadow_compare: wgpu::Sampler,
    shadow_array_view: wgpu::TextureView,
    globals: wgpu::Buffer,
    view_model_globals: wgpu::Buffer,
    shadow_uniforms: [wgpu::Buffer; 2],
    ui_uniform: wgpu::Buffer,
}

/// Everything that depends on which blocks and items exist. A server's content packs replace it.
struct Skin {
    world_bind: wgpu::BindGroup,
    entity_bind: wgpu::BindGroup,
    view_model_bind: wgpu::BindGroup,
    shadow_binds: [wgpu::BindGroup; 2],
    ui_bind: wgpu::BindGroup,
    ui_template: UiBatch,
    crack_layer: u32,
    tile_count: u32,
    /// The registry this was painted from, to tell whether another one looks any different.
    registry: Vec<u8>,
    /// Painted for the classic look: big pixels, few colours.
    classic: bool,
}

/// An array of tiles, one layer each, with their mips: the pictures, or (in another format) their
/// materials.
fn tile_texture_descriptor(layers: u32, mips: u32) -> wgpu::TextureDescriptor<'static> {
    wgpu::TextureDescriptor {
        label: Some("tiles"),
        size: wgpu::Extent3d { width: FINE as u32, height: FINE as u32, depth_or_array_layers: layers },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    }
}

/// Paints every texture a registry describes, and points the pipelines' inputs at them.
fn paint_skin(device: &wgpu::Device, queue: &wgpu::Queue, parts: &SkinParts, reg: &Registry, classic: bool) -> Skin {
    let look = |px: textures::Pixels, pattern, seed| if classic { pixelate(&px, pattern, seed) } else { px };
    let painted: Vec<textures::Pixels> = reg.tiles().iter().map(|t| look(paint(t), t.pattern, t.seed)).collect();
    let cracks: Vec<textures::Pixels> =
        crack_tiles().into_iter().map(|px| look(px, zenith_content::Pattern::Glass, 0)).collect();
    // A registry can ask for more layers than a GPU has. Then its last tiles show the last layer,
    // which beats refusing to play; cracks always get a place.
    let room = (device.limits().max_texture_array_layers as usize).saturating_sub(cracks.len());
    let crack_layer = painted.len().min(room) as u32;
    let tile_count = crack_layer + cracks.len() as u32;
    let mips = (FINE as f32).log2() as u32 + 1;
    let tile_texture = device.create_texture(&tile_texture_descriptor(tile_count, mips));
    let write_tile = |layer: u32, chain: &[Vec<[u8; 4]>]| {
        for (level, px) in chain.iter().enumerate() {
            let size = (FINE >> level) as u32;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tile_texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(px),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size * 4), rows_per_image: Some(size) },
                wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            );
        }
    };
    // The paintings at the size the graphics card holds: the classic look's pixels as they are,
    // the modern look's drawn out fine.
    let fine: Vec<textures::Pixels> = painted
        .iter()
        .zip(reg.tiles())
        .take(crack_layer as usize)
        .map(|(px, tex)| if classic { enlarge(px) } else { refine(px, tex.pattern, tex.seed) })
        .collect();
    for (i, (px, tex)) in fine.iter().zip(reg.tiles()).enumerate() {
        write_tile(i as u32, &mip_chain(px, tex.pattern));
    }
    for (i, px) in cracks.iter().enumerate() {
        write_tile(crack_layer + i as u32, &mip_chain(&enlarge(px), zenith_content::Pattern::Glass));
    }
    let tile_view = tile_texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });

    // Each tile's material, layer for layer: its bumps, roughness and metal, made from the very
    // pictures above, classic or modern.
    let material_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("materials"),
        format: wgpu::TextureFormat::Rgba8Unorm,
        ..tile_texture_descriptor(tile_count, mips)
    });
    let write_material = |layer: u32, chain: &[Vec<[u8; 4]>]| {
        for (level, px) in chain.iter().enumerate() {
            let size = (FINE >> level) as u32;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &material_texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(px),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size * 4), rows_per_image: Some(size) },
                wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            );
        }
    };
    for (i, (px, tex)) in fine.iter().zip(reg.tiles()).enumerate() {
        write_material(i as u32, &material_chain(&material(px, tex)));
    }
    let plain = material_chain(&plain_material());
    for i in 0..cracks.len() as u32 {
        write_material(crack_layer + i, &plain);
    }
    let material_view = material_texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let world_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("world"),
        layout: &parts.world_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: parts.globals.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tile_view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&parts.tile_sampler) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&parts.shadow_array_view) },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&parts.shadow_compare) },
            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&material_view) },
        ],
    });
    let entity_bind_for = |buffer: &wgpu::Buffer, label: &str| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &parts.entity_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tile_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&parts.entity_sampler) },
            ],
        })
    };

    // The UI's atlas: the font, and an icon for every item.
    let icons = all_icons(reg, &painted);
    let atlas = Atlas::new(&icons);
    let atlas_texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("ui atlas"),
            size: wgpu::Extent3d { width: atlas.width as u32, height: atlas.height as u32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&atlas.pixels),
    );
    let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let ui_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ui"),
        layout: &parts.ui_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: parts.ui_uniform.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas_view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&parts.ui_sampler) },
        ],
    });
    Skin {
        world_bind,
        entity_bind: entity_bind_for(&parts.globals, "entity"),
        view_model_bind: entity_bind_for(&parts.view_model_globals, "view model"),
        shadow_binds: [
            entity_bind_for(&parts.shadow_uniforms[0], "shadow"),
            entity_bind_for(&parts.shadow_uniforms[1], "shadow"),
        ],
        ui_bind,
        ui_template: UiBatch::new(&atlas, reg.items().len()),
        crack_layer,
        tile_count,
        registry: reg.to_bytes(),
        classic,
    }
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_name: String,
    pub backend: String,
    surface_format: Option<wgpu::TextureFormat>,

    globals: wgpu::Buffer,
    view_model_globals: wgpu::Buffer,
    shadow_uniforms: [wgpu::Buffer; 2],
    sky_bind: wgpu::BindGroup,
    shadow_views: [wgpu::TextureView; 2],
    near_mask: wgpu::Texture,
    near_bind: wgpu::BindGroup,
    near_cells: Vec<u8>,
    near_cells_changed: bool,
    skin_parts: SkinParts,
    skin: Skin,

    terrain_opaque: wgpu::RenderPipeline,
    terrain_cutout: wgpu::RenderPipeline,
    terrain_water: wgpu::RenderPipeline,
    shadow_opaque: wgpu::RenderPipeline,
    shadow_cutout: wgpu::RenderPipeline,
    sky: wgpu::RenderPipeline,
    far_pipeline: wgpu::RenderPipeline,
    entity_opaque: wgpu::RenderPipeline,
    entity_alpha: wgpu::RenderPipeline,
    view_model: wgpu::RenderPipeline,
    bloom_threshold: wgpu::RenderPipeline,
    bloom_down: wgpu::RenderPipeline,
    bloom_up: wgpu::RenderPipeline,
    final_pass: wgpu::RenderPipeline,
    blit: Option<wgpu::RenderPipeline>,
    ui_pipeline: wgpu::RenderPipeline,

    post_layout: wgpu::BindGroupLayout,
    water_layout: wgpu::BindGroupLayout,
    linear_sampler: wgpu::Sampler,
    black_view: wgpu::TextureView,

    arena: Arena,
    arena_buffer: wgpu::Buffer,
    quad_indices: wgpu::Buffer,
    instances: wgpu::Buffer,
    instance_data: Vec<[f32; 3]>,
    sections: FxMap<(ChunkPos, u8), GpuSection>,
    entity_buffer: wgpu::Buffer,
    entity_capacity: u64,
    ui_vertices: wgpu::Buffer,
    ui_indices: wgpu::Buffer,
    ui_uniform: wgpu::Buffer,
    far_instances: wgpu::Buffer,
    far: Option<FarTerrain>,

    /// Ray tracing, on a device that can: the structures, and the pipelines that trace them, which
    /// are made the first time they are wanted.
    tracer: Option<rt::Tracer>,
    traced: Option<Result<TracedPipelines, String>>,
    /// Why this device cannot trace, if it cannot.
    no_tracing: Option<String>,
    tracing_now: bool,

    targets: Targets,
    pub stats: RenderStats,
    pub crack_layer: u32,
    pub tile_count: u32,
}

pub fn new_instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
}

/// Picks the best GPU there is, preferring one that can draw to `surface`.
pub fn open_device(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), String> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: surface,
        apply_limit_buckets: false,
    }))
    .map_err(|e| format!("No graphics adapter fits: {e}"))?;
    let limits = adapter.limits();
    let required = wgpu::Limits {
        max_buffer_size: limits.max_buffer_size,
        max_texture_dimension_2d: limits.max_texture_dimension_2d,
        max_texture_array_layers: limits.max_texture_array_layers,
        ..wgpu::Limits::default()
    };
    let open = |features: wgpu::Features, limits: wgpu::Limits, experimental: wgpu::ExperimentalFeatures| {
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("zenith"),
            required_features: features,
            required_limits: limits,
            experimental_features: experimental,
            memory_hints: wgpu::MemoryHints::Performance,
            ..Default::default()
        }))
    };
    let opened = if tracing_blocker(&adapter).is_none() {
        let traced = wgpu::Limits {
            // The shaders read the whole vertex arena, to see what a ray hit.
            max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
            ..required.clone().using_acceleration_structure_values(limits.clone())
        };
        open(rt::features(), traced, experimental_features()).or_else(|e| {
            // A driver that offers ray tracing and then fails to deliver it should not cost the game.
            eprintln!("zenith: the graphics device would not open with ray tracing, so it is off: {e}");
            open(wgpu::Features::empty(), required.clone(), wgpu::ExperimentalFeatures::disabled())
        })
    } else {
        open(wgpu::Features::empty(), required, wgpu::ExperimentalFeatures::disabled())
    };
    let (device, queue) = opened.map_err(|e| format!("Could not open the graphics device: {e}"))?;
    Ok((adapter, device, queue))
}

/// Why this adapter cannot trace rays, or nothing if it can.
fn tracing_blocker(adapter: &wgpu::Adapter) -> Option<String> {
    if std::env::var_os("ZENITH_NO_RAY_TRACING").is_some() {
        return Some("turned off by ZENITH_NO_RAY_TRACING".into());
    }
    if adapter.features().contains(rt::features()) {
        return None;
    }
    let backend = adapter.get_info().backend;
    Some(if backend == wgpu::Backend::Vulkan {
        "this graphics card or its driver cannot trace rays".into()
    } else {
        format!("ray tracing needs Vulkan, and this graphics card is being driven through {backend:?}")
    })
}

/// wgpu counts its ray tracing as experimental, and taking it up as unsafe.
#[allow(unsafe_code)]
fn experimental_features() -> wgpu::ExperimentalFeatures {
    // SAFETY: wgpu asks for this as an acknowledgement that its ray tracing is new and may still have
    // bugs, which could misbehave even through its safe API. The game asks for it only from devices
    // that report the features, tries its traced pipelines in an error scope before using them, and
    // ZENITH_NO_RAY_TRACING keeps it from being asked for at all.
    unsafe { wgpu::ExperimentalFeatures::enabled() }
}

fn shader(device: &wgpu::Device, label: &str, parts: &[&str]) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(parts.concat().into()),
    })
}

const COMMON: &str = include_str!("../shaders/common.wgsl");
const TERRAIN: &str = include_str!("../shaders/terrain.wgsl");
const TERRAIN_RASTER: &str = include_str!("../shaders/terrain_raster.wgsl");
const TERRAIN_TRACED: &str = include_str!("../shaders/terrain_traced.wgsl");
/// WGSL wants the extensions a shader uses named before anything else in it.
const ENABLE_RAY_QUERY: &str = "enable wgpu_ray_query;\n";

const CHUNK_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Uint16x4, 1 => Uint16x2, 2 => Uint16x2, 3 => Unorm8x4];
const ORIGIN_ATTR: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![4 => Float32x3];
const ENTITY_ATTRS: [wgpu::VertexAttribute; 6] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Uint32, 3 => Unorm8x4, 4 => Snorm8x4, 5 => Unorm8x2];
const FAR_ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Unorm8x4, 2 => Snorm8x4];
const FAR_ORIGIN_ATTR: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![3 => Float32x3];
const UI_ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Unorm8x4];

fn chunk_layouts() -> [Option<wgpu::VertexBufferLayout<'static>>; 2] {
    [
        Some(wgpu::VertexBufferLayout {
            array_stride: VERTEX_SIZE,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &CHUNK_ATTRS,
        }),
        Some(wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ORIGIN_ATTR,
        }),
    ]
}

#[derive(Clone, Copy)]
enum ChunkPass {
    Opaque,
    Cutout,
    Water,
}

/// A pipeline drawing chunk meshes from `module`, terrain.wgsl with one of the ways of lighting it.
fn terrain_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    label: &str,
    pass: ChunkPass,
    layout: &wgpu::PipelineLayout,
) -> wgpu::RenderPipeline {
    let (entry, cull_mode, blend) = match pass {
        ChunkPass::Opaque => ("fs_opaque", Some(wgpu::Face::Back), None),
        ChunkPass::Cutout => ("fs_cutout", None, None),
        ChunkPass::Water => ("fs_translucent", None, Some(wgpu::BlendState::ALPHA_BLENDING)),
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &chunk_layouts(),
        },
        primitive: wgpu::PrimitiveState { cull_mode, front_face: wgpu::FrontFace::Ccw, ..Default::default() },
        // What shows through water is drawn before it, so water itself leaves the depth alone.
        depth_stencil: Some(depth_state(blend.is_none())),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format: HDR_FORMAT, blend, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Everything the reversed depth buffer needs: nearer means a greater value.
fn depth_state(write: bool) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(write),
        depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

fn entry_uniform(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn entry_texture(
    binding: u32,
    dimension: wgpu::TextureViewDimension,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type, view_dimension: dimension, multisampled: false },
        count: None,
    }
}

fn entry_sampler(binding: u32, kind: wgpu::SamplerBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(kind),
        count: None,
    }
}

fn post_params(device: &wgpu::Device, texel: [f32; 4]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("post params"),
        contents: bytemuck::bytes_of(&PostParams { texel, effects: [0.0; 4] }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

impl Renderer {
    pub fn new(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        reg: &Registry,
        surface_format: Option<wgpu::TextureFormat>,
        width: u32,
        height: u32,
    ) -> Self {
        let info = adapter.get_info();
        let adapter_name = info.name.clone();
        let backend = format!("{:?}", info.backend);

        let tile_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tiles"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 16,
            ..Default::default()
        });
        let entity_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("entity tiles"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let shadow_compare = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let shadow_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow map"),
            size: wgpu::Extent3d { width: SHADOW_SIZE, height: SHADOW_SIZE, depth_or_array_layers: 2 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_array_view = shadow_texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let shadow_views = [0u32, 1].map(|layer| {
            shadow_texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });

        let uniform = |label: &str, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let globals = uniform("globals", std::mem::size_of::<Globals>() as u64);
        let view_model_globals = uniform("view model globals", std::mem::size_of::<Globals>() as u64);
        let shadow_uniforms = [uniform("shadow 0", 64), uniform("shadow 1", 64)];

        let visible_all = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let float_tex = wgpu::TextureSampleType::Float { filterable: true };
        let world_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world"),
            entries: &[
                entry_uniform(0, visible_all),
                entry_texture(1, wgpu::TextureViewDimension::D2Array, float_tex),
                entry_sampler(2, wgpu::SamplerBindingType::Filtering),
                entry_texture(3, wgpu::TextureViewDimension::D2Array, wgpu::TextureSampleType::Depth),
                entry_sampler(4, wgpu::SamplerBindingType::Comparison),
                entry_texture(5, wgpu::TextureViewDimension::D2Array, float_tex),
            ],
        });
        let entity_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("entity"),
            entries: &[
                entry_uniform(0, visible_all),
                entry_texture(1, wgpu::TextureViewDimension::D2Array, float_tex),
                entry_sampler(2, wgpu::SamplerBindingType::Filtering),
            ],
        });
        let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sky"),
            entries: &[entry_uniform(0, visible_all)],
        });
        let sky_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sky"),
            layout: &sky_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });
        let near_mask = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("near chunks"),
            size: wgpu::Extent3d { width: NEAR_MASK as u32, height: NEAR_MASK as u32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let near_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("near chunks"),
            entries: &[wgpu::BindGroupLayoutEntry {
                visibility: visible_all,
                ..entry_texture(0, wgpu::TextureViewDimension::D2, wgpu::TextureSampleType::Float { filterable: false })
            }],
        });
        let near_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("near chunks"),
            layout: &near_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(
                    &near_mask.create_view(&wgpu::TextureViewDescriptor::default()),
                ),
            }],
        });
        let water_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("water"),
            entries: &[entry_texture(0, wgpu::TextureViewDimension::D2, wgpu::TextureSampleType::Depth)],
        });
        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[
                entry_texture(0, wgpu::TextureViewDimension::D2, float_tex),
                entry_sampler(1, wgpu::SamplerBindingType::Filtering),
                entry_texture(2, wgpu::TextureViewDimension::D2, float_tex),
                entry_uniform(3, wgpu::ShaderStages::FRAGMENT),
            ],
        });

        let layout = |label: &str, groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: groups,
                immediate_size: 0,
            })
        };
        let world_pl = layout("world", &[Some(&world_layout)]);
        let water_pl = layout("water", &[Some(&world_layout), Some(&water_layout)]);
        let entity_pl = layout("entity", &[Some(&entity_layout)]);
        let sky_pl = layout("sky", &[Some(&sky_layout)]);
        let far_pl = layout("far", &[Some(&sky_layout), Some(&near_layout)]);
        let post_pl = layout("post", &[Some(&post_layout)]);

        let terrain_src = shader(&device, "terrain", &[COMMON, TERRAIN, TERRAIN_RASTER]);
        let shadow_src = shader(&device, "shadow", &[include_str!("../shaders/shadow.wgsl")]);
        let sky_src = shader(&device, "sky", &[COMMON, include_str!("../shaders/sky.wgsl")]);
        let far_src = shader(&device, "far", &[COMMON, include_str!("../shaders/far.wgsl")]);
        let entity_src = shader(&device, "entity", &[COMMON, include_str!("../shaders/entity.wgsl")]);
        let post_src = shader(&device, "post", &[include_str!("../shaders/post.wgsl")]);
        let ui_src = shader(&device, "ui", &[include_str!("../shaders/ui.wgsl")]);

        let hdr_opaque =
            [Some(wgpu::ColorTargetState { format: HDR_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })];
        let hdr_blend = [Some(wgpu::ColorTargetState {
            format: HDR_FORMAT,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let cull_back = wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Back),
            front_face: wgpu::FrontFace::Ccw,
            ..Default::default()
        };
        let no_cull = wgpu::PrimitiveState::default();

        let chunk_buffers = chunk_layouts();
        let terrain_opaque = terrain_pipeline(&device, &terrain_src, "terrain opaque", ChunkPass::Opaque, &world_pl);
        let terrain_cutout = terrain_pipeline(&device, &terrain_src, "terrain cutout", ChunkPass::Cutout, &world_pl);
        let terrain_water = terrain_pipeline(&device, &terrain_src, "terrain water", ChunkPass::Water, &water_pl);

        let shadow_pipeline = |label: &str, fs: Option<&str>, prim: wgpu::PrimitiveState| {
            let frag = fs.map(|entry| wgpu::FragmentState {
                module: &shadow_src,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[],
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&entity_pl),
                vertex: wgpu::VertexState {
                    module: &shadow_src,
                    entry_point: Some("vs_shadow"),
                    compilation_options: Default::default(),
                    buffers: &chunk_buffers,
                },
                primitive: prim,
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.5, clamp: 0.0 },
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: frag,
                multiview_mask: None,
                cache: None,
            })
        };
        let shadow_opaque =
            shadow_pipeline("shadow opaque", None, wgpu::PrimitiveState { cull_mode: None, ..Default::default() });
        let shadow_cutout = shadow_pipeline("shadow cutout", Some("fs_shadow_cutout"), no_cull);

        let sky = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky"),
            layout: Some(&sky_pl),
            vertex: wgpu::VertexState {
                module: &sky_src,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: no_cull,
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &sky_src,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &hdr_opaque,
            }),
            multiview_mask: None,
            cache: None,
        });
        let far_buffers = [
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<FarVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &FAR_ATTRS,
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: 12,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &FAR_ORIGIN_ATTR,
            }),
        ];
        let far_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("far"),
            layout: Some(&far_pl),
            vertex: wgpu::VertexState {
                module: &far_src,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &far_buffers,
            },
            primitive: no_cull,
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &far_src,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &hdr_opaque,
            }),
            multiview_mask: None,
            cache: None,
        });
        let entity_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<EntityVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ENTITY_ATTRS,
        })];
        let entity_pipeline =
            |label: &str, targets: &[Option<wgpu::ColorTargetState>], write: bool, prim: wgpu::PrimitiveState| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&entity_pl),
                    vertex: wgpu::VertexState {
                        module: &entity_src,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &entity_buffers,
                    },
                    primitive: prim,
                    depth_stencil: Some(depth_state(write)),
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &entity_src,
                        entry_point: Some("fs_main"),
                        compilation_options: Default::default(),
                        targets,
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
        let entity_opaque = entity_pipeline("entity", &hdr_opaque, true, cull_back);
        let entity_alpha = entity_pipeline("entity alpha", &hdr_blend, false, no_cull);
        let view_model = entity_pipeline("view model", &hdr_opaque, true, cull_back);

        let post_pipeline = |label: &str, fs: &str, format: wgpu::TextureFormat, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&post_pl),
                vertex: wgpu::VertexState {
                    module: &post_src,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: no_cull,
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &post_src,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let bloom_threshold = post_pipeline("bloom threshold", "fs_threshold", HDR_FORMAT, None);
        let bloom_down = post_pipeline("bloom down", "fs_down", HDR_FORMAT, None);
        let add = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::REPLACE,
        };
        let bloom_up = post_pipeline("bloom up", "fs_up", HDR_FORMAT, Some(add));
        let final_pass = post_pipeline("final", "fs_final", LDR_FORMAT, None);
        let blit = surface_format.map(|f| post_pipeline("blit", "fs_blit", f, None));

        let nearest = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("ui"), ..Default::default() });
        let ui_uniform = uniform("ui screen", 16);
        let ui_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui"),
            entries: &[
                entry_uniform(0, visible_all),
                entry_texture(1, wgpu::TextureViewDimension::D2, float_tex),
                entry_sampler(2, wgpu::SamplerBindingType::Filtering),
            ],
        });
        let ui_pl = layout("ui", &[Some(&ui_layout)]);
        let ui_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ui"),
            layout: Some(&ui_pl),
            vertex: wgpu::VertexState {
                module: &ui_src,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &UI_ATTRS,
                })],
            },
            primitive: no_cull,
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &ui_src,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: LDR_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let black = device.create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: Some("black"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &[0, 0, 0, 255],
        );
        let black_view = black.create_view(&wgpu::TextureViewDescriptor::default());

        // Where the device can trace rays, the chunk vertices are also what its structures are built
        // from, and what the shaders read to see what a ray hit.
        let traces = device.features().contains(rt::features());
        let for_tracing = |usage: wgpu::BufferUsages| if traces { usage } else { wgpu::BufferUsages::empty() };
        let mut vertices = ARENA_VERTICES.min(device.limits().max_buffer_size / VERTEX_SIZE);
        if traces {
            vertices = vertices.min(device.limits().max_storage_buffer_binding_size / VERTEX_SIZE);
        }
        let arena_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chunk vertices"),
            size: vertices * VERTEX_SIZE,
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_DST
                | for_tracing(wgpu::BufferUsages::BLAS_INPUT | wgpu::BufferUsages::STORAGE),
            mapped_at_creation: false,
        });
        let indices: Vec<u32> =
            (0..MAX_QUADS).flat_map(|q| [q * 4, q * 4 + 1, q * 4 + 2, q * 4, q * 4 + 2, q * 4 + 3]).collect();
        let quad_indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX | for_tracing(wgpu::BufferUsages::BLAS_INPUT),
        });
        let tracer = traces.then(|| rt::Tracer::new(&device, &arena_buffer));
        let no_tracing = (!traces).then(|| {
            tracing_blocker(adapter).unwrap_or_else(|| "the graphics device would not open with ray tracing".into())
        });
        let dynamic = |label: &str, size: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let instances = dynamic("section origins", MAX_INSTANCES * 12, wgpu::BufferUsages::VERTEX);
        let far_instances = dynamic("far origins", 4096 * 12, wgpu::BufferUsages::VERTEX);
        let entity_capacity = 1 << 21;
        let entity_buffer = dynamic("entities", entity_capacity, wgpu::BufferUsages::VERTEX);
        let ui_vertices = dynamic("ui vertices", 1 << 21, wgpu::BufferUsages::VERTEX);
        let ui_indices = dynamic("ui indices", 1 << 20, wgpu::BufferUsages::INDEX);

        let targets =
            Self::make_targets(&device, width, height, &post_layout, &water_layout, &linear_sampler, &black_view);

        // Every texture in the game, painted now.
        let skin_parts = SkinParts {
            world_layout,
            entity_layout,
            ui_layout,
            tile_sampler,
            entity_sampler,
            ui_sampler: nearest,
            shadow_compare,
            shadow_array_view,
            globals: globals.clone(),
            view_model_globals: view_model_globals.clone(),
            shadow_uniforms: shadow_uniforms.clone(),
            ui_uniform: ui_uniform.clone(),
        };
        let skin = paint_skin(&device, &queue, &skin_parts, reg, false);
        let (crack_layer, tile_count) = (skin.crack_layer, skin.tile_count);

        Self {
            adapter_name,
            backend,
            surface_format,
            globals,
            view_model_globals,
            shadow_uniforms,
            sky_bind,
            shadow_views,
            near_mask,
            near_bind,
            near_cells: vec![0; NEAR_MASK * NEAR_MASK],
            near_cells_changed: true,
            skin_parts,
            skin,
            terrain_opaque,
            terrain_cutout,
            terrain_water,
            shadow_opaque,
            shadow_cutout,
            sky,
            far_pipeline,
            entity_opaque,
            entity_alpha,
            view_model,
            bloom_threshold,
            bloom_down,
            bloom_up,
            final_pass,
            blit,
            ui_pipeline,
            post_layout,
            water_layout,
            linear_sampler,
            black_view,
            arena: Arena::new(vertices as u32),
            arena_buffer,
            quad_indices,
            instances,
            instance_data: Vec::new(),
            sections: FxMap::default(),
            entity_buffer,
            entity_capacity,
            ui_vertices,
            ui_indices,
            ui_uniform,
            far_instances,
            far: None,
            tracer,
            traced: None,
            no_tracing,
            tracing_now: false,
            targets,
            stats: RenderStats::default(),
            crack_layer,
            tile_count,
            device,
            queue,
        }
    }

    /// A batch that knows where things are in this renderer's atlas.
    pub fn new_ui_batch(&self) -> UiBatch {
        self.skin.ui_template.clone()
    }

    /// Makes the textures match a world's content, which a server's packs can add to. A batch from
    /// `new_ui_batch` made before this no longer knows where the icons are.
    pub fn use_registry(&mut self, reg: &Registry) {
        if self.skin.registry == reg.to_bytes() {
            return;
        }
        self.skin = paint_skin(&self.device, &self.queue, &self.skin_parts, reg, self.skin.classic);
        self.crack_layer = self.skin.crack_layer;
        self.tile_count = self.skin.tile_count;
    }

    /// Paints everything again for the other look, if it is not the one painted already.
    fn use_look(&mut self, classic: bool) {
        if self.skin.classic == classic {
            return;
        }
        if let Ok(reg) = Registry::from_bytes(&self.skin.registry) {
            self.skin = paint_skin(&self.device, &self.queue, &self.skin_parts, &reg, classic);
        }
    }

    fn make_targets(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        post_layout: &wgpu::BindGroupLayout,
        water_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        black: &wgpu::TextureView,
    ) -> Targets {
        let (width, height) = (width.max(1), height.max(1));
        let color = |label: &str, w: u32, h: u32, format: wgpu::TextureFormat, extra: wgpu::TextureUsages| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | extra,
                view_formats: &[],
            })
        };
        let hdr = color("hdr", width, height, HDR_FORMAT, wgpu::TextureUsages::empty());
        let hdr_view = hdr.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = color("depth", width, height, DEPTH_FORMAT, wgpu::TextureUsages::empty());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let ldr = color("ldr", width, height, LDR_FORMAT, wgpu::TextureUsages::COPY_SRC);
        let ldr_view = ldr.create_view(&wgpu::TextureViewDescriptor::default());

        let mut sizes = Vec::new();
        let (mut w, mut h) = (width, height);
        for _ in 0..5 {
            w = (w / 2).max(1);
            h = (h / 2).max(1);
            sizes.push((w, h));
        }
        let bloom_textures: Vec<wgpu::Texture> =
            sizes.iter().map(|&(w, h)| color("bloom", w, h, HDR_FORMAT, wgpu::TextureUsages::empty())).collect();
        let bloom_views: Vec<wgpu::TextureView> =
            bloom_textures.iter().map(|t| t.create_view(&wgpu::TextureViewDescriptor::default())).collect();

        let post_bind = |source: &wgpu::TextureView, bloom: &wgpu::TextureView, params: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post"),
                layout: post_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(source) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(bloom) },
                    wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
                ],
            })
        };
        let mut down_bind = Vec::new();
        let (mut sw, mut sh) = (width, height);
        for i in 0..bloom_views.len() {
            let params = post_params(device, [0.5 / sw as f32, 0.5 / sh as f32, 0.0, 0.0]);
            let source = if i == 0 { &hdr_view } else { &bloom_views[i - 1] };
            down_bind.push(post_bind(source, black, &params));
            (sw, sh) = sizes[i];
        }
        let mut up_bind = Vec::new();
        for i in 1..bloom_views.len() {
            let (sw, sh) = sizes[i];
            let params = post_params(device, [1.0 / sw as f32, 1.0 / sh as f32, 0.0, 0.0]);
            up_bind.push(post_bind(&bloom_views[i], black, &params));
        }
        let final_params = post_params(device, [1.0 / width as f32, 1.0 / height as f32, 0.0, 1.0]);
        let final_bind = post_bind(&hdr_view, &bloom_views[0], &final_params);
        let blit_params = post_params(device, [0.0; 4]);
        let blit_bind = post_bind(&ldr_view, black, &blit_params);
        let water_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water"),
            layout: water_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&depth_view) }],
        });
        Targets {
            width,
            height,
            hdr_view,
            depth_view,
            ldr,
            ldr_view,
            bloom_views,
            down_bind,
            up_bind,
            final_bind,
            final_params,
            water_bind,
            blit_bind,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == self.targets.width && height == self.targets.height {
            return;
        }
        self.targets = Self::make_targets(
            &self.device,
            width,
            height,
            &self.post_layout,
            &self.water_layout,
            &self.linear_sampler,
            &self.black_view,
        );
    }

    pub fn size(&self) -> (u32, u32) {
        (self.targets.width, self.targets.height)
    }

    /// Starts a world: far terrain comes from its generator.
    pub fn set_world(&mut self, generator: Arc<Generator>, threads: usize) {
        self.clear_world();
        self.far = Some(FarTerrain::new(&self.device, generator, threads));
    }

    pub fn clear_world(&mut self) {
        let spans: Vec<GpuSection> = self.sections.drain().map(|(_, s)| s).collect();
        for s in spans {
            self.arena.release(s.opaque);
            self.arena.release(s.cutout);
            self.arena.release(s.water);
        }
        if let Some(tracer) = &mut self.tracer {
            tracer.clear();
        }
        self.far = None;
        self.near_cells.fill(0);
        self.near_cells_changed = true;
    }

    /// Says whether real chunks stand at a position. Far terrain stays out of those that have them
    /// and fills in everywhere else, whatever shape that leaves.
    pub fn set_chunk_covered(&mut self, pos: ChunkPos, covered: bool) {
        let cell =
            pos.z.rem_euclid(NEAR_MASK as i32) as usize * NEAR_MASK + pos.x.rem_euclid(NEAR_MASK as i32) as usize;
        let value = if covered { 255 } else { 0 };
        if self.near_cells[cell] != value {
            self.near_cells[cell] = value;
            self.near_cells_changed = true;
        }
    }

    /// Puts a section's mesh on the GPU, replacing what it had. Without room for all of it, the
    /// section is left with nothing at all, and the answer is false.
    pub fn upload_section(&mut self, pos: ChunkPos, y: u8, mesh: &SectionMesh) -> bool {
        self.release_section(pos, y);
        if mesh.is_empty() {
            if let Some(tracer) = &mut self.tracer {
                tracer.gone((pos, y));
            }
            return true;
        }
        let mut spans = [Span::default(); 3];
        for (i, verts) in [&mesh.opaque, &mesh.cutout, &mesh.translucent].into_iter().enumerate() {
            if verts.is_empty() {
                continue;
            }
            let Some(span) = self.arena.alloc(verts.len() as u32) else {
                for s in spans {
                    self.arena.release(s);
                }
                self.stats.out_of_memory += 1;
                if let Some(tracer) = &mut self.tracer {
                    tracer.gone((pos, y));
                }
                return false;
            };
            self.queue.write_buffer(&self.arena_buffer, span.start as u64 * VERTEX_SIZE, bytemuck::cast_slice(verts));
            spans[i] = span;
        }
        self.sections.insert((pos, y), GpuSection { opaque: spans[0], cutout: spans[1], water: spans[2] });
        if let Some(tracer) = &mut self.tracer {
            tracer.changed((pos, y));
        }
        true
    }

    /// How much of the room for chunk vertices is taken, 0 to 1.
    pub fn vertex_use(&self) -> f32 {
        self.arena.used as f32 / self.arena.capacity.max(1) as f32
    }

    pub fn remove_section(&mut self, pos: ChunkPos, y: u8) {
        self.release_section(pos, y);
        if let Some(tracer) = &mut self.tracer {
            tracer.gone((pos, y));
        }
    }

    /// Gives a section's room in the arena back. Its traced copy, if any, stays until told otherwise:
    /// a section being replaced is rebuilt in the same frame, and never goes missing for one.
    fn release_section(&mut self, pos: ChunkPos, y: u8) {
        if let Some(s) = self.sections.remove(&(pos, y)) {
            self.arena.release(s.opaque);
            self.arena.release(s.cutout);
            self.arena.release(s.water);
        }
    }

    /// Whether rays can be traced, as far as anyone has found out yet.
    pub fn tracing(&self) -> Tracing {
        if let Some(why) = &self.no_tracing {
            return Tracing::Unavailable(why.clone());
        }
        if let Some(Err(why)) = &self.traced {
            return Tracing::Unavailable(why.clone());
        }
        if self.tracing_now { Tracing::On } else { Tracing::Off }
    }

    /// Whether this frame can trace, making the pipelines that do it the first time they are wanted.
    fn ready_to_trace(&mut self) -> bool {
        let Some(tracer) = &self.tracer else { return false };
        if self.traced.is_none() {
            self.traced = Some(Self::traced_pipelines(
                &self.device,
                &self.skin_parts.world_layout,
                &self.water_layout,
                &tracer.layout,
            ));
            if let Some(Err(why)) = &self.traced {
                eprintln!("zenith: ray tracing is unavailable: {why}");
            }
        }
        matches!(self.traced, Some(Ok(_)))
    }

    /// The chunk pipelines, lit by rays. Anything wrong with them, on a driver that offered more than
    /// it could give, comes back as a reason rather than a crash.
    fn traced_pipelines(
        device: &wgpu::Device,
        world: &wgpu::BindGroupLayout,
        water: &wgpu::BindGroupLayout,
        traced: &wgpu::BindGroupLayout,
    ) -> Result<TracedPipelines, String> {
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let module = shader(device, "terrain traced", &[ENABLE_RAY_QUERY, COMMON, TERRAIN, TERRAIN_TRACED]);
        let layout = |label: &str, groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: groups,
                immediate_size: 0,
            })
        };
        let solid = layout("traced", &[Some(world), None, Some(traced)]);
        let wet = layout("traced water", &[Some(world), Some(water), Some(traced)]);
        let pipelines = TracedPipelines {
            opaque: terrain_pipeline(device, &module, "traced opaque", ChunkPass::Opaque, &solid),
            cutout: terrain_pipeline(device, &module, "traced cutout", ChunkPass::Cutout, &solid),
            water: terrain_pipeline(device, &module, "traced water", ChunkPass::Water, &wet),
        };
        // Scopes come off in the reverse of the order they went on.
        let failed = pollster::block_on(internal.pop()).or(pollster::block_on(validation.pop()));
        match failed {
            Some(e) => Err(format!("the traced shaders would not build: {e}")),
            None => Ok(pipelines),
        }
    }

    pub fn remove_chunk(&mut self, pos: ChunkPos) {
        self.set_chunk_covered(pos, false);
        for y in 0..zenith_core::pos::SECTIONS as u8 {
            self.remove_section(pos, y);
        }
    }

    /// Draws a whole frame into the renderer's picture, ready to show or save.
    pub fn render(&mut self, frame: &Frame<'_>) {
        self.use_look(frame.classic);
        // Ray traced, the classic look keeps its pictures and its sky, and is lit as the modern one is.
        let traced = frame.ray_tracing && self.ready_to_trace();
        self.tracing_now = traced;
        if let Some(tracer) = &mut self.tracer {
            tracer.set_active(traced);
        }
        let cam = frame.camera;
        let (width, height) = (self.targets.width as f32, self.targets.height as f32);
        let aspect = width / height;
        let forward = cam.forward();
        let view = look_to(Vec3::ZERO, forward, Vec3::Y);
        let proj = perspective((cam.fov_deg as f32).to_radians(), aspect);
        let view_proj = proj * view;
        let sky = sky_at(frame.time_of_day, frame.rain);

        // Shadow volumes: squares of world space around the camera, snapped to whole texels so that
        // edges do not crawl as you walk.
        let light_fwd = -sky.light_dir;
        let up_hint = if light_fwd.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
        let right = light_fwd.cross(up_hint).normalize();
        let up = right.cross(light_fwd).normalize();
        let shadow_matrices: [Mat4; 2] = std::array::from_fn(|i| {
            let r = CASCADES[i];
            let texel = 2.0 * r as f64 / SHADOW_SIZE as f64;
            let (px, py) = (cam.pos.dot(right.as_dvec3()), cam.pos.dot(up.as_dvec3()));
            let (fx, fy) = (px - (px / texel).floor() * texel, py - (py / texel).floor() * texel);
            let center = -right * fx as f32 - up * fy as f32;
            let depth = 280.0 + r * 2.0;
            let eye = center - light_fwd * (depth * 0.5);
            orthographic(r, depth) * look_to(eye, light_fwd, up)
        });
        for (i, m) in shadow_matrices.iter().enumerate() {
            self.queue.write_buffer(&self.shadow_uniforms[i], 0, bytemuck::cast_slice(&m.to_cols_array_2d()));
        }

        let wrap = |v: f64| (v.rem_euclid(4096.0)) as f32;
        let globals = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            inv_view_proj: view_proj.inverse().to_cols_array_2d(),
            shadow: [shadow_matrices[0].to_cols_array_2d(), shadow_matrices[1].to_cols_array_2d()],
            light_dir: [sky.light_dir.x, sky.light_dir.y, sky.light_dir.z, sky.light_strength],
            light_color: [sky.light_color.x, sky.light_color.y, sky.light_color.z, sky.day],
            ambient: [sky.ambient.x, sky.ambient.y, sky.ambient.z, frame.seconds],
            sky_zenith: [sky.zenith.x, sky.zenith.y, sky.zenith.z, sky.stars],
            sky_horizon: [sky.horizon.x, sky.horizon.y, sky.horizon.z, frame.rain],
            camera: [wrap(cam.pos.x), cam.pos.y as f32, wrap(cam.pos.z), cam.pos.y as f32],
            fog: [
                if frame.debug.fog { 0.000085 + frame.rain * 0.0009 } else { 0.0 },
                f32::from(u8::from(frame.classic)),
                f32::from(u8::from(frame.under_water)),
                f32::from(u8::from(frame.under_lava)),
            ],
            screen: [width, height, 1.0 / width, 1.0 / height],
            sun_dir: [sky.sun_dir.x, sky.sun_dir.y, sky.sun_dir.z, frame.debug.shader_flags(frame.classic && traced)],
        };
        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        if std::mem::take(&mut self.near_cells_changed) {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.near_mask,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.near_cells,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(NEAR_MASK as u32),
                    rows_per_image: Some(NEAR_MASK as u32),
                },
                wgpu::Extent3d { width: NEAR_MASK as u32, height: NEAR_MASK as u32, depth_or_array_layers: 1 },
            );
        }

        // The hand has its own narrower view, and no idea where in the world it is.
        let vm_proj = perspective(62f32.to_radians(), aspect);
        let vm = Globals {
            view_proj: vm_proj.to_cols_array_2d(),
            inv_view_proj: vm_proj.inverse().to_cols_array_2d(),
            // No haze between the eye and the hand, but the same look.
            fog: [0.0, globals.fog[1], 0.0, 0.0],
            ..globals
        };
        self.queue.write_buffer(&self.view_model_globals, 0, bytemuck::bytes_of(&vm));

        // Which sections each pass needs.
        let main_planes = frustum_planes(view_proj);
        let shadow_planes = [frustum_planes(shadow_matrices[0]), frustum_planes(shadow_matrices[1])];
        self.instance_data.clear();
        let (mut opaque, mut cutout, mut water) = (Vec::new(), Vec::new(), Vec::new());
        let mut shadow_lists: [Vec<(Span, u32, bool)>; 2] = [Vec::new(), Vec::new()];
        // Traced shadows need no map, and the classic look has no shadows.
        let want_shadows = !traced && !frame.classic && sky.light_strength > 0.001;
        let mut quads = 0u64;
        for (&(pos, y), s) in &self.sections {
            let min = Vec3::new(
                ((pos.x as f64 * 16.0) - cam.pos.x) as f32,
                ((zenith_core::pos::MIN_Y as f64 + y as f64 * 16.0) - cam.pos.y) as f32,
                ((pos.z as f64 * 16.0) - cam.pos.z) as f32,
            );
            let max = min + Vec3::splat(16.0);
            let in_main = box_in_frustum(&main_planes, min, max);
            let in_shadow = [
                want_shadows && box_in_frustum(&shadow_planes[0], min, max),
                want_shadows && box_in_frustum(&shadow_planes[1], min, max),
            ];
            if !in_main && !in_shadow[0] && !in_shadow[1] {
                continue;
            }
            if self.instance_data.len() as u64 >= MAX_INSTANCES {
                break;
            }
            let instance = self.instance_data.len() as u32;
            self.instance_data.push(min.to_array());
            let center = min + Vec3::splat(8.0);
            let distance = center.length_squared();
            if in_main {
                if s.opaque.len > 0 {
                    opaque.push(Draw { span: s.opaque, instance, distance });
                    quads += s.opaque.len as u64 / 4;
                }
                if s.cutout.len > 0 {
                    cutout.push(Draw { span: s.cutout, instance, distance });
                    quads += s.cutout.len as u64 / 4;
                }
                if s.water.len > 0 {
                    water.push(Draw { span: s.water, instance, distance });
                    quads += s.water.len as u64 / 4;
                }
            }
            for c in 0..2 {
                if in_shadow[c] {
                    if s.opaque.len > 0 {
                        shadow_lists[c].push((s.opaque, instance, false));
                    }
                    if s.cutout.len > 0 {
                        shadow_lists[c].push((s.cutout, instance, true));
                    }
                }
            }
        }
        // Nearest first: what is hidden then costs no shading. Water the other way round, to blend.
        opaque.sort_unstable_by(|a, b| a.distance.total_cmp(&b.distance));
        cutout.sort_unstable_by(|a, b| a.distance.total_cmp(&b.distance));
        water.sort_unstable_by(|a, b| b.distance.total_cmp(&a.distance));
        self.queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.instance_data));

        // Far terrain.
        let mut far_draws = Vec::new();
        if frame.draw_far
            && let Some(far) = &mut self.far
        {
            let mut tiles = far.update(&self.device, (cam.pos.x, cam.pos.y, cam.pos.z));
            let origin_of = |t: &far::FarDraw| {
                Vec3::new(
                    (t.origin.0 as f64 - cam.pos.x) as f32,
                    -cam.pos.y as f32,
                    (t.origin.1 as f64 - cam.pos.z) as f32,
                )
            };
            tiles.retain(|t| {
                let at = origin_of(t);
                let min = Vec3::new(at.x, at.y + t.heights.0, at.z);
                let max = Vec3::new(at.x + t.width, at.y + t.heights.1, at.z + t.width);
                box_in_frustum(&main_planes, min, max)
            });
            let origins: Vec<[f32; 3]> = tiles.iter().map(|t| origin_of(t).to_array()).collect();
            self.queue.write_buffer(&self.far_instances, 0, bytemuck::cast_slice(&origins));
            far_draws = tiles.iter().map(|t| t.buffer.clone()).collect::<Vec<_>>();
        }

        // Entities, the see-through pass and the hand share one buffer.
        let all = [frame.entities, frame.alpha, frame.view_model];
        let total: u64 = all.iter().map(|l| l.len() as u64).sum::<u64>() * std::mem::size_of::<EntityVertex>() as u64;
        if total > self.entity_capacity {
            self.entity_capacity = (total * 2).next_power_of_two();
            self.entity_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("entities"),
                size: self.entity_capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let mut written = 0u64;
        let mut ranges = [0u32..0u32, 0..0, 0..0];
        let mut first = 0u32;
        for (i, list) in all.iter().enumerate() {
            if !list.is_empty() {
                self.queue.write_buffer(&self.entity_buffer, written, bytemuck::cast_slice(list));
                written += list.len() as u64 * std::mem::size_of::<EntityVertex>() as u64;
            }
            ranges[i] = first..first + list.len() as u32;
            first += list.len() as u32;
        }

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });

        // What rays are traced against, rebuilt around the camera.
        self.stats.traced = rt::TraceStats::default();
        if traced && let Some(tracer) = &mut self.tracer {
            tracer.prepare(
                &self.device,
                &self.queue,
                &mut encoder,
                &self.sections,
                cam.pos,
                &self.arena_buffer,
                &self.quad_indices,
            );
            self.stats.traced = tracer.stats;
        }
        let traced_with = match (&self.traced, &self.tracer) {
            (Some(Ok(pipelines)), Some(tracer)) if traced => Some((pipelines, &tracer.bind)),
            _ => None,
        };

        // Shadows, unless rays are doing it or the look has none.
        let shadow_passes = if traced || frame.classic { 0 } else { 2 };
        for c in 0..shadow_passes {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_views[c],
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            if !want_shadows {
                continue;
            }
            pass.set_bind_group(0, &self.skin.shadow_binds[c], &[]);
            pass.set_vertex_buffer(0, self.arena_buffer.slice(..));
            pass.set_vertex_buffer(1, self.instances.slice(..));
            pass.set_index_buffer(self.quad_indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.set_pipeline(&self.shadow_opaque);
            for &(span, instance, is_cutout) in &shadow_lists[c] {
                if !is_cutout {
                    pass.draw_indexed(0..span.len / 4 * 6, span.start as i32, instance..instance + 1);
                }
            }
            pass.set_pipeline(&self.shadow_cutout);
            for &(span, instance, is_cutout) in &shadow_lists[c] {
                if is_cutout {
                    pass.draw_indexed(0..span.len / 4 * 6, span.start as i32, instance..instance + 1);
                }
            }
        }

        // The world.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.hdr_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.targets.depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_pipeline(&self.sky);
            pass.set_bind_group(0, &self.sky_bind, &[]);
            pass.draw(0..3, 0..1);

            if !far_draws.is_empty()
                && let Some(far) = &self.far
            {
                pass.set_pipeline(&self.far_pipeline);
                pass.set_bind_group(0, &self.sky_bind, &[]);
                pass.set_bind_group(1, &self.near_bind, &[]);
                pass.set_vertex_buffer(1, self.far_instances.slice(..));
                pass.set_index_buffer(far.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                for (i, buffer) in far_draws.iter().enumerate() {
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw_indexed(0..far.index_count, 0, i as u32..i as u32 + 1);
                }
            }

            pass.set_vertex_buffer(0, self.arena_buffer.slice(..));
            pass.set_vertex_buffer(1, self.instances.slice(..));
            pass.set_index_buffer(self.quad_indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.set_bind_group(0, &self.skin.world_bind, &[]);
            if let Some((_, bind)) = traced_with {
                pass.set_bind_group(2, bind, &[]);
            }
            pass.set_pipeline(traced_with.map_or(&self.terrain_opaque, |(p, _)| &p.opaque));
            for d in &opaque {
                pass.draw_indexed(0..d.span.len / 4 * 6, d.span.start as i32, d.instance..d.instance + 1);
            }
            pass.set_pipeline(traced_with.map_or(&self.terrain_cutout, |(p, _)| &p.cutout));
            for d in &cutout {
                pass.draw_indexed(0..d.span.len / 4 * 6, d.span.start as i32, d.instance..d.instance + 1);
            }
            if !ranges[0].is_empty() {
                pass.set_pipeline(&self.entity_opaque);
                pass.set_bind_group(0, &self.skin.entity_bind, &[]);
                pass.set_vertex_buffer(0, self.entity_buffer.slice(..));
                pass.draw(ranges[0].clone(), 0..1);
            }
        }

        // Things you can see through: they read the depth of what is behind them.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("translucent"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.hdr_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.targets.depth_view,
                    depth_ops: None,
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            if !water.is_empty() {
                pass.set_pipeline(traced_with.map_or(&self.terrain_water, |(p, _)| &p.water));
                pass.set_bind_group(0, &self.skin.world_bind, &[]);
                pass.set_bind_group(1, &self.targets.water_bind, &[]);
                if let Some((_, bind)) = traced_with {
                    pass.set_bind_group(2, bind, &[]);
                }
                pass.set_vertex_buffer(0, self.arena_buffer.slice(..));
                pass.set_vertex_buffer(1, self.instances.slice(..));
                pass.set_index_buffer(self.quad_indices.slice(..), wgpu::IndexFormat::Uint32);
                for d in &water {
                    pass.draw_indexed(0..d.span.len / 4 * 6, d.span.start as i32, d.instance..d.instance + 1);
                }
            }
            if !ranges[1].is_empty() {
                pass.set_pipeline(&self.entity_alpha);
                pass.set_bind_group(0, &self.skin.entity_bind, &[]);
                pass.set_vertex_buffer(0, self.entity_buffer.slice(..));
                pass.draw(ranges[1].clone(), 0..1);
            }
        }

        // The hand, on top of everything, so it never pokes through a wall.
        if !ranges[2].is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("view model"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.hdr_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.targets.depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_pipeline(&self.view_model);
            pass.set_bind_group(0, &self.skin.view_model_bind, &[]);
            pass.set_vertex_buffer(0, self.entity_buffer.slice(..));
            pass.draw(ranges[2].clone(), 0..1);
        }

        // Bloom: down the chain, then back up it. The classic look has none, unless it is ray traced.
        let bloom_steps = if frame.classic && !traced { 0 } else { self.targets.bloom_views.len() };
        for i in 0..bloom_steps {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bloom down"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.bloom_views[i],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(if i == 0 { &self.bloom_threshold } else { &self.bloom_down });
            pass.set_bind_group(0, &self.targets.down_bind[i], &[]);
            pass.draw(0..3, 0..1);
        }
        for i in (0..self.targets.up_bind.len().min(bloom_steps)).rev() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bloom up"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.bloom_views[i],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.bloom_up);
            pass.set_bind_group(0, &self.targets.up_bind[i], &[]);
            pass.draw(0..3, 0..1);
        }

        // The picture, then the UI on it.
        let classic = f32::from(u8::from(frame.classic && !traced));
        let params = PostParams {
            texel: [1.0 / width, 1.0 / height, f32::from(u8::from(frame.under_water)), 0.92 + classic * 0.08],
            effects: [frame.hurt_flash, frame.dead, frame.seconds, classic],
        };
        self.queue.write_buffer(&self.targets.final_params, 0, bytemuck::bytes_of(&params));
        self.queue.write_buffer(&self.ui_uniform, 0, bytemuck::cast_slice(&[1.0 / width, 1.0 / height, 0.0, 0.0]));
        let ui = frame.ui;
        let ui_fits = (ui.verts.len() * std::mem::size_of::<UiVertex>()) as u64 <= self.ui_vertices.size()
            && (ui.indices.len() * 4) as u64 <= self.ui_indices.size();
        if ui_fits && !ui.verts.is_empty() {
            self.queue.write_buffer(&self.ui_vertices, 0, bytemuck::cast_slice(&ui.verts));
            self.queue.write_buffer(&self.ui_indices, 0, bytemuck::cast_slice(&ui.indices));
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("final"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.ldr_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.final_pass);
            pass.set_bind_group(0, &self.targets.final_bind, &[]);
            pass.draw(0..3, 0..1);
            if ui_fits && !ui.verts.is_empty() {
                pass.set_pipeline(&self.ui_pipeline);
                pass.set_bind_group(0, &self.skin.ui_bind, &[]);
                pass.set_vertex_buffer(0, self.ui_vertices.slice(..));
                pass.set_index_buffer(self.ui_indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..ui.indices.len() as u32, 0, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));

        self.stats = RenderStats {
            sections: self.sections.len(),
            drawn: opaque.len() + cutout.len() + water.len(),
            shadow_drawn: shadow_lists[0].len() + shadow_lists[1].len(),
            quads,
            arena_used_mb: (self.arena.used as u64 * VERTEX_SIZE) as f32 / 1_048_576.0,
            arena_capacity_mb: (self.arena.capacity as u64 * VERTEX_SIZE) as f32 / 1_048_576.0,
            far_tiles: self.far.as_ref().map_or(0, FarTerrain::tile_count),
            far_pending: self.far.as_ref().map_or(0, FarTerrain::pending_count),
            out_of_memory: self.stats.out_of_memory,
            traced: self.stats.traced,
        };
    }

    /// Copies the finished picture to the window.
    pub fn present_to(&self, view: &wgpu::TextureView) {
        let Some(blit) = &self.blit else { return };
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("present") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(blit);
            pass.set_bind_group(0, &self.targets.blit_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
    }

    pub fn surface_format(&self) -> Option<wgpu::TextureFormat> {
        self.surface_format
    }

    /// Saves the last picture drawn as a PNG. Blocks until the GPU has handed it over.
    pub fn screenshot(&self, path: &std::path::Path) -> Result<(), String> {
        let (w, h) = (self.targets.width, self.targets.height);
        let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot"),
            size: row as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("screenshot") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.targets.ldr,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(encoder.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        {
            let data = buffer.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
            for y in 0..h as usize {
                let start = y * row as usize;
                pixels.extend_from_slice(&data[start..start + w as usize * 4]);
            }
        }
        buffer.unmap();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = std::fs::File::create(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&pixels).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shader mistake is otherwise a crash on the first frame, on the player's machine.
    #[test]
    fn every_shader_compiles() {
        let sources: [(&str, String); 7] = [
            ("terrain", [COMMON, TERRAIN, TERRAIN_RASTER].concat()),
            ("shadow", include_str!("../shaders/shadow.wgsl").to_string()),
            ("sky", [COMMON, include_str!("../shaders/sky.wgsl")].concat()),
            ("far", [COMMON, include_str!("../shaders/far.wgsl")].concat()),
            ("entity", [COMMON, include_str!("../shaders/entity.wgsl")].concat()),
            ("post", include_str!("../shaders/post.wgsl").to_string()),
            ("ui", include_str!("../shaders/ui.wgsl").to_string()),
        ];
        for (name, source) in sources {
            let module = naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&source)));
            let mut validator =
                naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty());
            validator.validate(&module).unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&source)));
        }
    }

    /// The traced shader only ever compiles on a machine that can trace, which may not be this one.
    #[test]
    fn the_traced_terrain_shader_compiles() {
        let source = [ENABLE_RAY_QUERY, COMMON, TERRAIN, TERRAIN_TRACED].concat();
        let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        let mut validator =
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::RAY_QUERY);
        validator.validate(&module).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        for entry in ["fs_opaque", "fs_cutout", "fs_translucent"] {
            assert!(module.entry_points.iter().any(|e| e.name == entry), "{entry} is missing");
        }
    }

    #[test]
    fn the_sun_rises_in_the_east_and_is_highest_at_noon() {
        let (dawn, noon, dusk, midnight) =
            (sky_at(0.0, 0.0), sky_at(6000.0, 0.0), sky_at(12000.0, 0.0), sky_at(18000.0, 0.0));
        assert!(dawn.sun_dir.x > 0.9 && dawn.sun_dir.y.abs() < 0.1);
        assert!(noon.sun_dir.y > 0.9);
        assert!(dusk.sun_dir.x < -0.9);
        assert!(midnight.sun_dir.y < -0.9);
    }

    #[test]
    fn night_is_darker_than_day_and_rain_dims_the_sun() {
        let (noon, midnight) = (sky_at(6000.0, 0.0), sky_at(18000.0, 0.0));
        assert!(noon.ambient.length() > midnight.ambient.length() * 3.0);
        assert!(midnight.stars > 0.9 && noon.stars < 0.01);
        assert!(sky_at(6000.0, 1.0).light_strength < noon.light_strength * 0.4);
    }

    #[test]
    fn light_never_jumps_as_sun_gives_way_to_moon() {
        // Direct light fades to nothing at the horizon, where the light source swaps.
        let before = sky_at(11990.0, 0.0);
        let after = sky_at(12010.0, 0.0);
        assert!(before.light_strength < 0.1 && after.light_strength < 0.1, "{before:?} {after:?}");
    }

    #[test]
    fn depth_is_near_over_distance_as_the_water_shader_assumes() {
        let m = perspective(70f32.to_radians(), 1.0) * look_to(Vec3::ZERO, Vec3::Z, Vec3::Y);
        let p = m * Vec4::new(0.0, 0.0, 40.0, 1.0);
        assert!((p.w - 40.0).abs() < 1e-3);
        assert!((p.z / p.w - NEAR / 40.0).abs() < 1e-6);
    }

    #[test]
    fn a_view_puts_what_it_looks_at_in_front_and_keeps_up_up() {
        let m = look_to(Vec3::new(1.0, 2.0, 3.0), Vec3::Z, Vec3::Y);
        let ahead = m * Vec4::new(1.0, 2.0, 13.0, 1.0);
        assert!((ahead.z + 10.0).abs() < 1e-4, "{ahead}");
        let above = m * Vec4::new(1.0, 5.0, 3.0, 1.0);
        assert!((above.y - 3.0).abs() < 1e-4, "{above}");
        // Facing +z with y up, +x is on the left: this world is right-handed.
        let left = m * Vec4::new(2.0, 2.0, 3.0, 1.0);
        assert!((left.x + 1.0).abs() < 1e-4, "{left}");
    }

    #[test]
    fn a_shadow_volume_maps_its_depth_from_zero_to_one() {
        let m = orthographic(50.0, 200.0) * look_to(Vec3::ZERO, Vec3::NEG_Y, Vec3::Z);
        let near = m * Vec4::new(0.0, 0.0, 0.0, 1.0);
        let far = m * Vec4::new(0.0, -200.0, 0.0, 1.0);
        assert!(near.z.abs() < 1e-5, "{near}");
        assert!((far.z - 1.0).abs() < 1e-5, "{far}");
    }

    #[test]
    fn a_box_in_front_of_the_camera_is_visible_and_one_behind_it_is_not() {
        let planes =
            frustum_planes(perspective(70f32.to_radians(), 16.0 / 9.0) * look_to(Vec3::ZERO, Vec3::Z, Vec3::Y));
        let ahead = (Vec3::new(-1.0, -1.0, 20.0), Vec3::new(1.0, 1.0, 22.0));
        let behind = (Vec3::new(-1.0, -1.0, -22.0), Vec3::new(1.0, 1.0, -20.0));
        let aside = (Vec3::new(500.0, -1.0, 20.0), Vec3::new(502.0, 1.0, 22.0));
        let very_far = (Vec3::new(-1.0, -1.0, 90_000.0), Vec3::new(1.0, 1.0, 90_002.0));
        assert!(box_in_frustum(&planes, ahead.0, ahead.1));
        assert!(!box_in_frustum(&planes, behind.0, behind.1));
        assert!(!box_in_frustum(&planes, aside.0, aside.1));
        assert!(box_in_frustum(&planes, very_far.0, very_far.1), "no far plane: nothing is too far to draw");
    }
}
