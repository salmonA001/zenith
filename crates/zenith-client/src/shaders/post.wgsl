// From the bright, linear scene to what goes on the screen: bloom, then tone mapping.
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var linear_clamp: sampler;
@group(0) @binding(2) var bloom: texture_2d<f32>;
@group(0) @binding(3) var<uniform> post: PostParams;

struct PostParams {
    // x: 1 / source width. y: 1 / source height. z: under water. w: exposure.
    texel: vec4<f32>,
    // x: damage flash, 0 to 1. y: darkness when dead, 0 to 1. z: seconds. w: 1 for the classic look.
    effects: vec4<f32>,
};

struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> Out {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var out: Out;
    out.clip = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

// One step down the bloom chain, keeping only what is brighter than a surface can be.
fn shrink(uv: vec2<f32>) -> vec3<f32> {
    let t = post.texel.xy;
    var c = textureSampleLevel(source, linear_clamp, uv, 0.0).rgb * 4.0;
    c += textureSampleLevel(source, linear_clamp, uv + vec2<f32>(-t.x, -t.y), 0.0).rgb;
    c += textureSampleLevel(source, linear_clamp, uv + vec2<f32>(t.x, -t.y), 0.0).rgb;
    c += textureSampleLevel(source, linear_clamp, uv + vec2<f32>(-t.x, t.y), 0.0).rgb;
    c += textureSampleLevel(source, linear_clamp, uv + vec2<f32>(t.x, t.y), 0.0).rgb;
    return c / 8.0;
}

@fragment
fn fs_down(in: Out) -> @location(0) vec4<f32> {
    return vec4<f32>(shrink(in.uv), 1.0);
}

@fragment
fn fs_threshold(in: Out) -> @location(0) vec4<f32> {
    let c = shrink(in.uv);
    let brightness = max(c.r, max(c.g, c.b));
    let keep = smoothstep(0.85, 1.6, brightness);
    // A blazing sun must not turn the whole screen white.
    return vec4<f32>(min(c * keep, vec3<f32>(12.0)), 1.0);
}

// One step back up: a soft tent, added onto the level above.
@fragment
fn fs_up(in: Out) -> @location(0) vec4<f32> {
    let t = post.texel.xy;
    var c = textureSampleLevel(source, linear_clamp, in.uv, 0.0).rgb * 4.0;
    c += textureSampleLevel(source, linear_clamp, in.uv + vec2<f32>(-t.x, 0.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(source, linear_clamp, in.uv + vec2<f32>(t.x, 0.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(source, linear_clamp, in.uv + vec2<f32>(0.0, -t.y), 0.0).rgb * 2.0;
    c += textureSampleLevel(source, linear_clamp, in.uv + vec2<f32>(0.0, t.y), 0.0).rgb * 2.0;
    c += textureSampleLevel(source, linear_clamp, in.uv + vec2<f32>(-t.x, -t.y), 0.0).rgb;
    c += textureSampleLevel(source, linear_clamp, in.uv + vec2<f32>(t.x, -t.y), 0.0).rgb;
    c += textureSampleLevel(source, linear_clamp, in.uv + vec2<f32>(-t.x, t.y), 0.0).rgb;
    c += textureSampleLevel(source, linear_clamp, in.uv + vec2<f32>(t.x, t.y), 0.0).rgb;
    return vec4<f32>(c / 16.0, 1.0);
}

// A filmic curve: highlights roll off gently rather than clipping.
fn tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_final(in: Out) -> @location(0) vec4<f32> {
    var uv = in.uv;
    if (post.texel.z > 0.5) {
        // The world wavers through water.
        let t = post.effects.z;
        uv += vec2<f32>(sin(uv.y * 26.0 + t * 1.8), cos(uv.x * 22.0 + t * 1.4)) * 0.0018;
    }
    let classic = post.effects.w > 0.5;
    var c = textureSampleLevel(source, linear_clamp, uv, 0.0).rgb;
    let d = in.uv - 0.5;
    if (!classic) {
        c += textureSampleLevel(bloom, linear_clamp, uv, 0.0).rgb * 0.1;
        c *= 1.0 - dot(d, d) * 0.28;
    }
    c *= post.texel.w;

    // A hit flashes the edges of the screen red.
    let edge = smoothstep(0.1, 0.75, length(d) * 1.35);
    c = mix(c, vec3<f32>(0.7, 0.03, 0.02), post.effects.x * edge * 0.75);
    c = mix(c, c * vec3<f32>(0.5, 0.12, 0.1), post.effects.y);

    // The classic look shows its colours as they are, where the modern one rolls bright ones off.
    if (classic) {
        return vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
    }
    return vec4<f32>(tonemap(c), 1.0);
}

// Copies the finished picture to whatever format the window wants.
@fragment
fn fs_blit(in: Out) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, linear_clamp, in.uv, 0.0);
}
