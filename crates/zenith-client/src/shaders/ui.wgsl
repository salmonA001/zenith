// Screens and the HUD: textured quads in pixels, over everything.
@group(0) @binding(0) var<uniform> screen: vec4<f32>;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(v: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = vec4<f32>(v.pos.x * screen.x * 2.0 - 1.0, 1.0 - v.pos.y * screen.y * 2.0, 0.0, 1.0);
    out.uv = v.uv;
    // Colours are authored in sRGB, and the target wants them linear.
    out.color = vec4<f32>(pow(v.color.rgb, vec3<f32>(2.2)), v.color.a);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(atlas, atlas_sampler, in.uv) * in.color;
}
