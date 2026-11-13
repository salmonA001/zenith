// The shadow passes write depth only. Holes in leaves still let the sun through.
struct ShadowGlobals {
    light: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> sg: ShadowGlobals;
@group(0) @binding(1) var tiles: texture_2d_array<f32>;
@group(0) @binding(2) var tile_sampler: sampler;

struct VertexIn {
    @location(0) pos_flags: vec4<u32>,
    @location(1) uv: vec2<u32>,
    @location(2) layer_light: vec2<u32>,
    @location(4) origin: vec3<f32>,
};

struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
};

@vertex
fn vs_shadow(v: VertexIn) -> ShadowOut {
    let local = (vec3<f32>(v.pos_flags.xyz) - 512.0) / 256.0;
    var out: ShadowOut;
    out.clip = sg.light * vec4<f32>(v.origin + local, 1.0);
    out.uv = vec2<f32>(v.uv) / 256.0;
    out.layer = v.layer_light.x;
    return out;
}

@fragment
fn fs_shadow_cutout(in: ShadowOut) {
    if (textureSampleLevel(tiles, tile_sampler, in.uv, in.layer, 1.0).a < 0.5) {
        discard;
    }
}
