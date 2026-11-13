// Terrain too far away for chunks: a height field drawn from the same function chunks come from.
@group(0) @binding(0) var<uniform> g: Globals;
// One texel per chunk, wrapping around: set where real chunks stand.
@group(1) @binding(0) var near_chunks: texture_2d<f32>;

struct VertexIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) normal: vec4<f32>,
    @location(3) origin: vec3<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rel: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) normal: vec3<f32>,
};

fn real_chunks_at(world: vec2<f32>) -> bool {
    let chunk = vec2<i32>(floor(world / 16.0)) & vec2<i32>(127);
    return textureLoad(near_chunks, chunk, 0).r > 0.5;
}

@vertex
fn vs_main(v: VertexIn) -> VertexOut {
    var rel = v.origin + v.pos;
    // Under real chunks the surface drops out of the way. Along their edge that makes a steep curtain,
    // closing the view where a chunk has no wall against a neighbour it has not meshed.
    let world = rel.xz + g.camera.xz;
    let inside = real_chunks_at(world + vec2<f32>(1.0, 1.0))
        && real_chunks_at(world + vec2<f32>(-1.0, 1.0))
        && real_chunks_at(world + vec2<f32>(1.0, -1.0))
        && real_chunks_at(world + vec2<f32>(-1.0, -1.0));
    if (inside) {
        rel.y -= 96.0;
    }
    var out: VertexOut;
    out.clip = g.view_proj * vec4<f32>(rel, 1.0);
    out.rel = rel;
    // Colours are authored in sRGB, and light is added up in linear.
    out.color = vec4<f32>(pow(v.color.rgb, vec3<f32>(2.2)), v.color.a);
    if (inside) {
        // What that curtain would be if it were real: the rock under the ground.
        out.color = vec4<f32>(0.1, 0.095, 0.09, v.color.a);
    }
    out.normal = v.normal.xyz;
    return out;
}

// A tile's colours are coarse. What its blocks would show up close comes from here instead.
fn land_color(in: VertexOut) -> vec3<f32> {
    let world = in.rel.xz + g.camera.xz;
    let up_close = exp(-length(in.rel) * 0.0011);
    let grain = (value_noise(world * 0.37) - 0.5) * 0.24 + (value_noise(world * 0.071) - 0.5) * 0.34;
    var base = in.color.rgb;
    let top = max(base.r, max(base.g, base.b));
    let colorless = 1.0 - (top - min(base.r, min(base.g, base.b))) / max(top, 0.001);
    if (colorless > 0.92) {
        // Only rock and snow have no colour. Between two vertices they would fade into a smear;
        // here they meet at a broken line, and the rock shows its layers.
        let height = in.rel.y + g.camera.w;
        let edge = top + (value_noise(world * 0.09) - 0.5) * 0.28 + (value_noise(world * 0.31) - 0.5) * 0.12;
        let layers = 0.82 + 0.36 * value_noise(vec2<f32>(height * 0.42 + value_noise(world * 0.006) * 9.0, 0.5));
        base = mix(vec3<f32>(0.135, 0.135, 0.145) * layers, vec3<f32>(0.86, 0.9, 0.95), smoothstep(0.48, 0.56, edge));
    }
    return base * (1.0 + grain * up_close);
}

struct FragmentOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

@fragment
fn fs_main(in: VertexOut) -> FragmentOut {
    var out: FragmentOut;
    // Where real chunks have arrived, they are what is drawn: this is pushed behind anything they
    // draw, and so only shows through a gap between them. It need not look like much.
    if (real_chunks_at(in.rel.xz + g.camera.xz)) {
        out.color = vec4<f32>(apply_haze(g, in.color.rgb * (g.ambient.rgb + g.light_color.rgb * g.light_dir.w * 0.5), in.rel), 1.0);
        out.depth = in.clip.z * 0.001;
        return out;
    }
    out.depth = in.clip.z;
    let n = normalize(in.normal);
    if (g.fog.y > 0.5) {
        // The classic look: open sky, shaded by how steep the ground is. The sea is flat colour.
        let level = classic_level(g, 1.0, 0.0);
        var plain = land_color(in) * level * mix(0.66, 1.0, clamp(n.y, 0.0, 1.0));
        if (in.color.a < 0.5) {
            plain = in.color.rgb * level * 0.9;
        }
        out.color = vec4<f32>(apply_haze(g, plain, in.rel), 1.0);
        return out;
    }
    let ndl = max(dot(n, g.light_dir.xyz), 0.0);
    let ambient = g.ambient.rgb * (0.72 + 0.28 * n.y);
    var color = land_color(in) * (ambient + g.light_color.rgb * g.light_dir.w * ndl);
    if (in.color.a < 0.5) {
        // The sea: a mirror of the sky at this distance, with a little glitter.
        let view = normalize(in.rel);
        let cos_view = clamp(-view.y, 0.0, 1.0);
        let fresnel = 0.05 + 0.95 * pow(1.0 - cos_view, 4.0);
        let mirror = sky_gradient(g, normalize(vec3<f32>(view.x, abs(view.y), view.z)));
        let body = in.color.rgb * (g.ambient.rgb * 0.5 + g.light_color.rgb * g.light_dir.w * max(g.light_dir.y, 0.0) * 0.35);
        color = mix(body, mirror, fresnel);
    }
    out.color = vec4<f32>(apply_haze(g, color, in.rel), 1.0);
    return out;
}
