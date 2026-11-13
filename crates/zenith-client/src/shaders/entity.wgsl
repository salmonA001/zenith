// Mobs, items, particles, the outline of the selected block, and the first-person hand.
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var tiles: texture_2d_array<f32>;
@group(0) @binding(2) var tile_sampler: sampler;

struct VertexIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) layer: u32,
    @location(3) color: vec4<f32>,
    // xyz: normal. w: 1 for things that glow, unaffected by light.
    @location(4) normal: vec4<f32>,
    // x: sky light. y: block light.
    @location(5) light: vec2<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rel: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) layer: u32,
    @location(3) color: vec4<f32>,
    @location(4) normal: vec4<f32>,
    @location(5) light: vec2<f32>,
};

@vertex
fn vs_main(v: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    out.rel = v.pos;
    out.uv = v.uv;
    out.layer = v.layer;
    // Colours are authored in sRGB, and light is added up in linear.
    out.color = vec4<f32>(pow(v.color.rgb, vec3<f32>(2.2)), v.color.a);
    out.normal = v.normal;
    out.light = vec2<f32>(v.light.x, max(v.light.y, full_bright(g)));
    return out;
}

const NO_TEXTURE: u32 = 0xFFFFFFFFu;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    var base = in.color;
    if (in.layer != NO_TEXTURE) {
        let tex = textureSample(tiles, tile_sampler, in.uv, in.layer);
        base = vec4<f32>(tex.rgb * in.color.rgb, in.color.a);
        if (tex.a < 0.5 && in.normal.w < 0.5) {
            discard;
        }
        if (in.normal.w > 0.5) {
            base.a = base.a * tex.a;
        }
    }
    if (base.a <= 0.003) {
        discard;
    }
    var color = base.rgb;
    if (in.normal.w < 0.5 && g.fog.y > 0.5 && !full_light(g)) {
        // The classic look, as the blocks around it have it.
        color = base.rgb * classic_level(g, in.light.x, in.light.y) * classic_face(normalize(in.normal.xyz));
    } else if (in.normal.w < 0.5) {
        let n = normalize(in.normal.xyz);
        let sky = in.light.x;
        let open = sky * sky * (3.0 - 2.0 * sky);
        let ambient = g.ambient.rgb * open * (0.75 + 0.25 * n.y) + vec3<f32>(0.03, 0.033, 0.04);
        let torch = vec3<f32>(1.0, 0.68, 0.36) * pow(in.light.y, 2.0) * 1.5;
        let direct = g.light_color.rgb * g.light_dir.w * max(dot(n, g.light_dir.xyz), 0.0) * smoothstep(0.25, 0.75, sky);
        color = base.rgb * (ambient + torch + direct);
    }
    return vec4<f32>(apply_haze(g, color, in.rel), base.a);
}
