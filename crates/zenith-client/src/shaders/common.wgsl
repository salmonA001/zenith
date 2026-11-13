diagnostic(off, derivative_uniformity);
// What every shader that sees the world shares: the frame's constants, and how the sky looks.
struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    shadow: array<mat4x4<f32>, 2>,
    // xyz: where the light comes from. w: how strong direct light is, 0 to 1.
    light_dir: vec4<f32>,
    // rgb: the sun's or moon's colour. w: 1 by day, 0 at night.
    light_color: vec4<f32>,
    // rgb: light from the sky in general. w: seconds since start.
    ambient: vec4<f32>,
    // rgb: sky straight up. w: stars, 0 to 1.
    sky_zenith: vec4<f32>,
    // rgb: sky at the horizon. w: rain, 0 to 1.
    sky_horizon: vec4<f32>,
    // xyz: camera position, wrapped to stay precise. w: real camera height.
    camera: vec4<f32>,
    // x: haze. y: unused. z: under water, 0 or 1. w: lava, 0 or 1.
    fog: vec4<f32>,
    // width, height, 1/width, 1/height.
    screen: vec4<f32>,
    // xyz: the sun itself, whichever light is casting shadows. w: unused.
    sun_dir: vec4<f32>,
};

// Noise from integer maths: the same on every GPU, however far from the origin.
fn pcg2d(p: vec2<i32>) -> vec2<u32> {
    var v = vec2<u32>(p) * 1664525u + 1013904223u;
    v.x += v.y * 1664525u;
    v.y += v.x * 1664525u;
    v = v ^ (v >> vec2<u32>(16u));
    v.x += v.y * 1664525u;
    v.y += v.x * 1664525u;
    v = v ^ (v >> vec2<u32>(16u));
    return v;
}

fn hash21(p: vec2<f32>) -> f32 {
    return f32(pcg2d(vec2<i32>(floor(p))).x) / 4294967296.0;
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash21(i), hash21(i + vec2<f32>(1.0, 0.0)), u.x),
        mix(hash21(i + vec2<f32>(0.0, 1.0)), hash21(i + vec2<f32>(1.0, 1.0)), u.x),
        u.y
    );
}

fn corner_slope(cell: vec2<f32>, to: vec2<f32>) -> f32 {
    let angle = hash21(cell) * 6.2831853;
    return dot(vec2<f32>(cos(angle), sin(angle)), to);
}

// Gradient noise, about -1 to 1. Unlike value noise it has no flat bands along its grid.
fn gradient_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    return mix(
        mix(corner_slope(i, f), corner_slope(i + vec2<f32>(1.0, 0.0), f - vec2<f32>(1.0, 0.0)), u.x),
        mix(corner_slope(i + vec2<f32>(0.0, 1.0), f - vec2<f32>(0.0, 1.0)), corner_slope(i + vec2<f32>(1.0, 1.0), f - vec2<f32>(1.0, 1.0)), u.x),
        u.y
    ) * 1.4;
}

// The sky in a direction, without sun, moon, stars or clouds: what fog fades into.
fn sky_gradient(g: Globals, dir: vec3<f32>) -> vec3<f32> {
    let up = clamp(dir.y, -0.25, 1.0);
    let height = pow(max(up, 0.0), 0.48);
    var color = mix(g.sky_horizon.rgb, g.sky_zenith.rgb, height);
    // Below the horizon the haze thickens into a darker version of itself.
    if (up < 0.0) {
        color = mix(color, g.sky_horizon.rgb * 0.62, clamp(-up * 4.0, 0.0, 1.0));
    }
    // Scattering around the sun: broad at sunset, when the light crosses the most air.
    let toward_sun = max(dot(dir, g.sun_dir.xyz), 0.0);
    let low_sun = 1.0 - clamp(g.sun_dir.y * 2.6, 0.0, 1.0);
    let visible = clamp(g.sun_dir.y * 8.0 + 0.8, 0.0, 1.0);
    let band = exp(-abs(up) * mix(9.0, 3.0, low_sun));
    let sunset = vec3<f32>(1.0, 0.42, 0.16) * pow(toward_sun, 3.0) * band * low_sun * 1.25 * visible;
    // The classic look's sun is a disc of pixels, and a soft blaze around it would hide them.
    let blaze = mix(1.0, 0.15, g.fog.y);
    let glow = vec3<f32>(1.0, 0.86, 0.66) * (pow(toward_sun, 18.0) * 0.26 + pow(toward_sun, 160.0) * 0.5) * visible * blaze;
    let rain = 1.0 - g.sky_horizon.w * 0.8;
    return color + (sunset + glow) * rain;
}

// The classic look lights a face by the way it points, as blocky games long have: the top in full,
// the sides less, the bottom least. No sun direction comes into it.
fn classic_face(n: vec3<f32>) -> f32 {
    let a = abs(n);
    if (a.y >= a.x && a.y >= a.z) {
        return select(0.5, 1.0, n.y > 0.0);
    }
    return select(0.6, 0.8, a.z > a.x);
}

// How bright the classic look makes a sky level and a block level, each 0 to 1. Skylight dims at
// night and turns a little blue; torchlight is warm.
fn classic_level(g: Globals, sky: f32, block: f32) -> vec3<f32> {
    let from_sky = sky * mix(0.12, 1.0, g.light_color.w) * (1.0 - g.sky_horizon.w * 0.25);
    let sky_color = mix(vec3<f32>(0.6, 0.66, 1.0), vec3<f32>(1.0), g.light_color.w) * from_sky;
    let level = max(sky_color, vec3<f32>(1.0, 0.85, 0.66) * block);
    // Even steps to the eye: each light level looks about as much brighter as the last.
    return pow(level, vec3<f32>(1.5)) * 0.96 + vec3<f32>(0.035);
}

// The debug options screen's switches, in sun_dir.w: 1 hides the clouds, 2 lights everything.
fn hide_clouds(g: Globals) -> bool {
    return (u32(g.sun_dir.w + 0.5) & 1u) != 0u;
}

fn full_bright(g: Globals) -> f32 {
    return f32((u32(g.sun_dir.w + 0.5) >> 1u) & 1u);
}

// Set when the classic look is ray traced: its pictures and sky stay, and it is lit as the modern
// look is, sun, shadows, materials and all.
fn full_light(g: Globals) -> bool {
    return (u32(g.sun_dir.w + 0.5) & 4u) != 0u;
}

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// How much of what is behind it a distance of air leaves showing.
fn haze_amount(g: Globals, rel: vec3<f32>) -> f32 {
    let d = length(rel);
    if (g.fog.z > 0.5) {
        return 1.0 - exp(-d * 0.042);
    }
    if (g.fog.w > 0.5) {
        return 1.0 - exp(-d * 0.9);
    }
    // Air thins with height: looking up through it is clearer than looking along the ground.
    let thin = exp(-max(g.camera.w + rel.y * 0.5 - 62.0, 0.0) * 0.004);
    let amount = 1.0 - exp(-d * g.fog.x * (0.45 + thin * 0.55));
    return clamp(amount * amount * (3.0 - 2.0 * amount), 0.0, 1.0);
}

fn apply_haze(g: Globals, color: vec3<f32>, rel: vec3<f32>) -> vec3<f32> {
    let dir = normalize(rel);
    var fog_color = sky_gradient(g, dir);
    if (g.fog.z > 0.5) {
        fog_color = vec3<f32>(0.03, 0.14, 0.24) * (0.3 + g.light_color.w * 0.7);
    } else if (g.fog.w > 0.5) {
        fog_color = vec3<f32>(1.0, 0.32, 0.05);
    }
    return mix(color, fog_color, haze_amount(g, rel));
}
