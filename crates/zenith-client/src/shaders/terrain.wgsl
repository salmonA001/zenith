// Chunks: opaque blocks, cutouts, water, and the depth-only pass for shadows.
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var tiles: texture_2d_array<f32>;
@group(0) @binding(2) var tile_sampler: sampler;
@group(0) @binding(3) var shadow_map: texture_depth_2d_array;
@group(0) @binding(4) var shadow_sampler: sampler_comparison;
// Each tile's material: which way its texels lean, then roughness and metalness.
@group(0) @binding(5) var materials: texture_2d_array<f32>;

const POS_BIAS: f32 = 512.0;
const TILE_SIZE: f32 = 32.0;
const SWAY_SOFT: u32 = 8u;
const SWAY_TOP: u32 = 16u;
const WATER: u32 = 32u;
const LAVA: u32 = 64u;
const GLOW: u32 = 128u;

fn face_normal(face: u32) -> vec3<f32> {
    switch face {
        case 0u: { return vec3<f32>(0.0, -1.0, 0.0); }
        case 1u: { return vec3<f32>(0.0, 1.0, 0.0); }
        case 2u: { return vec3<f32>(0.0, 0.0, -1.0); }
        case 3u: { return vec3<f32>(0.0, 0.0, 1.0); }
        case 4u: { return vec3<f32>(-1.0, 0.0, 0.0); }
        default: { return vec3<f32>(1.0, 0.0, 0.0); }
    }
}

struct VertexIn {
    @location(0) pos_flags: vec4<u32>,
    @location(1) uv: vec2<u32>,
    @location(2) layer_light: vec2<u32>,
    @location(3) tint: vec4<f32>,
    @location(4) origin: vec3<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) rel: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) layer: u32,
    @location(3) @interpolate(flat) flags: u32,
    @location(4) light: vec3<f32>, // ao, sky, block
    @location(5) tint: vec3<f32>,
    @location(6) @interpolate(flat) normal: vec3<f32>,
};

fn wind(world: vec2<f32>, t: f32) -> vec2<f32> {
    let gust = sin(t * 0.7 + world.x * 0.08 + world.y * 0.05) * 0.5 + 0.5;
    let a = sin(t * 2.1 + world.x * 0.9 + world.y * 0.7);
    let b = sin(t * 1.7 + world.x * 0.6 - world.y * 1.1);
    return vec2<f32>(a, b) * (0.35 + gust * 0.65);
}

fn place(v: VertexIn, flags: u32) -> vec3<f32> {
    var local = (vec3<f32>(v.pos_flags.xyz) - POS_BIAS) / 256.0;
    var rel = v.origin + local;
    let world_xz = rel.xz + g.camera.xz;
    let strength = 1.0 + g.sky_horizon.w * 1.4;
    if ((flags & SWAY_TOP) != 0u) {
        rel = vec3<f32>(rel.x, rel.y, rel.z) + vec3<f32>(wind(world_xz, g.ambient.w).x, 0.0, wind(world_xz, g.ambient.w).y) * 0.1 * strength;
    } else if ((flags & SWAY_SOFT) != 0u) {
        rel = rel + vec3<f32>(wind(world_xz, g.ambient.w * 0.7).x, 0.0, wind(world_xz, g.ambient.w * 0.7).y) * 0.022 * strength;
    }
    return rel;
}

@vertex
fn vs_main(v: VertexIn) -> VertexOut {
    let ao = v.pos_flags.w & 255u;
    let flags = v.pos_flags.w >> 8u;
    let rel = place(v, flags);
    var out: VertexOut;
    out.clip = g.view_proj * vec4<f32>(rel, 1.0);
    out.rel = rel;
    out.uv = vec2<f32>(v.uv) / 256.0;
    out.layer = v.layer_light.x;
    out.flags = flags;
    let sky = f32(v.layer_light.y & 255u) / 255.0;
    let block = max(f32(v.layer_light.y >> 8u) / 255.0, full_bright(g));
    out.light = vec3<f32>(f32(ao) / 255.0, sky, block);
    // Colours are authored in sRGB, and light is added up in linear.
    out.tint = pow(v.tint.rgb, vec3<f32>(2.2));
    out.normal = face_normal(flags & 7u);
    return out;
}

// Sharp pixels up close, and proper anisotropic filtering at a distance, from one sampler.
fn sample_tile(uv: vec2<f32>, layer: u32) -> vec4<f32> {
    let p = uv * TILE_SIZE;
    let dx = dpdx(uv);
    let dy = dpdy(uv);
    let width = max(abs(dpdx(p)) + abs(dpdy(p)), vec2<f32>(0.0001));
    // Stays on a texel's centre until the last pixel before the seam, and blends across only that.
    let seam = floor(p + 0.5);
    let snapped = seam + clamp((p - seam) / width, vec2<f32>(-0.5), vec2<f32>(0.5));
    return textureSampleGrad(tiles, tile_sampler, snapped / TILE_SIZE, layer, dx, dy);
}

// What a surface is made of, texel by texel: which way it faces, how rough it is, how metal.
struct Surface {
    normal: vec3<f32>,
    rough: f32,
    metal: f32,
};

// The way a face's texture runs across it, its across and down, then the face's own normal: the
// same as the mesher lays textures out.
fn face_frame(face: u32) -> mat3x3<f32> {
    switch face {
        case 0u: { return mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, -1.0), vec3<f32>(0.0, -1.0, 0.0)); }
        case 1u: { return mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0)); }
        case 2u: { return mat3x3<f32>(vec3<f32>(-1.0, 0.0, 0.0), vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(0.0, 0.0, -1.0)); }
        case 3u: { return mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0)); }
        case 4u: { return mat3x3<f32>(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0)); }
        default: { return mat3x3<f32>(vec3<f32>(0.0, 0.0, -1.0), vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0)); }
    }
}

// A face's surface from its material, its texels leaning as their picture does.
fn surface_at(uv: vec2<f32>, layer: u32, flags: u32) -> Surface {
    let m = textureSampleGrad(materials, tile_sampler, uv, layer, dpdx(uv), dpdy(uv));
    let lean = m.rg * 2.0 - 1.0;
    let up = sqrt(max(1.0 - dot(lean, lean), 0.0));
    let n = normalize(face_frame(flags & 7u) * vec3<f32>(lean.x, lean.y, up));
    return Surface(n, max(m.b, 0.04), m.a);
}

// A plain surface: the face as it is, roughness as given, no metal.
fn plain_surface(normal: vec3<f32>, rough: f32) -> Surface {
    return Surface(normal, rough, 0.0);
}

// How much light a surface sends toward the eye from a light in direction `l`: a glint, tight and
// bright on smooth things, broad and faint on rough ones (GGX, with Schlick's Fresnel).
fn glint(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, rough: f32, f0: vec3<f32>) -> vec3<f32> {
    let h = normalize(v + l);
    let a = max(rough * rough, 0.003);
    let a2 = a * a;
    let ndh = max(dot(n, h), 0.0);
    let ndv = max(dot(n, v), 0.001);
    let ndl = max(dot(n, l), 0.0);
    let d = ndh * ndh * (a2 - 1.0) + 1.0;
    let distribution = a2 / (3.14159 * d * d);
    let k = (rough + 1.0) * (rough + 1.0) / 8.0;
    let visibility = 1.0 / ((ndv * (1.0 - k) + k) * (ndl * (1.0 - k) + k));
    let fresnel = f0 + (1.0 - f0) * pow(1.0 - max(dot(h, v), 0.0), 5.0);
    return distribution * visibility * fresnel * ndl / 4.0;
}

// `shadow_at`, `water_shadow` and `reflection` come from terrain_raster.wgsl or terrain_traced.wgsl:
// the shadow map and the sky, or rays, whichever this pipeline was built with.

// The classic look: light levels, and a fixed shade for each way a face points. No sun shadows.
fn classic_light(albedo: vec3<f32>, normal: vec3<f32>, light: vec3<f32>, flags: u32) -> vec3<f32> {
    if ((flags & GLOW) != 0u) {
        return albedo * 1.4;
    }
    return albedo * classic_level(g, light.y, light.z) * classic_face(normal) * mix(0.5, 1.0, light.x);
}

// Light on a face, from the sky, the sun or moon, and lamps: `normal` is the face's own, for its
// shadow, and `s` what it is made of, for how each texel takes the light.
fn light_up(albedo: vec3<f32>, rel: vec3<f32>, normal: vec3<f32>, light: vec3<f32>, flags: u32, s: Surface) -> vec3<f32> {
    if (g.fog.y > 0.5 && !full_light(g)) {
        return classic_light(albedo, normal, light, flags);
    }
    let ao = mix(0.28, 1.0, light.x);
    let sky = light.y;
    let block = light.z;
    let n = s.normal;
    // Up-facing surfaces see more of the sky than ones facing sideways or down.
    let facing = 0.72 + 0.28 * n.y;
    let open = sky * sky * (3.0 - 2.0 * sky);
    let ambient = g.ambient.rgb * open * facing + vec3<f32>(0.018, 0.02, 0.028);
    let torch = vec3<f32>(1.0, 0.68, 0.36) * pow(block, 2.0) * 1.5;
    var direct = vec3<f32>(0.0);
    var shine = vec3<f32>(0.0);
    let f0 = mix(vec3<f32>(0.04), albedo, s.metal);
    if (dot(normal, g.light_dir.xyz) > 0.0 && g.light_dir.w > 0.001 && sky > 0.2) {
        let shadow = shadow_at(rel, normal, flags);
        let reach = smoothstep(0.25, 0.75, sky);
        let sun = g.light_color.rgb * g.light_dir.w * shadow * reach;
        direct = sun * max(dot(n, g.light_dir.xyz), 0.0);
        shine = sun * glint(n, -normalize(rel), g.light_dir.xyz, s.rough, f0);
    }
    // Metal has no colour of its own underneath, only what it reflects. Rough metal reflects its
    // surroundings in a blur, which comes to the light falling on it tinted by the metal; smooth metal
    // mirrors them, which `mirrored` adds.
    let light_in = (ambient + torch) * ao + direct * mix(0.6, 1.0, light.x);
    let blur = clamp(s.rough / 0.4, 0.0, 1.0);
    let lit = albedo * (1.0 - s.metal) * light_in + f0 * s.metal * blur * light_in + shine;
    if ((flags & GLOW) != 0u) {
        // The bright parts of a glowing block give off light; its darker parts are lit as anything is.
        let glow = smoothstep(0.18, 0.6, luminance(albedo));
        return mix(lit, albedo * 2.8, glow);
    }
    return lit;
}

// What a smooth surface mirrors: the sky, or with rays whatever is there, more of it at a grazing
// look and the smoother the surface. Rough ones mirror nothing worth the cost.
fn mirrored(albedo: vec3<f32>, rel: vec3<f32>, light: vec3<f32>, s: Surface) -> vec3<f32> {
    if (s.rough > 0.4 || (g.fog.y > 0.5 && !full_light(g))) {
        return vec3<f32>(0.0);
    }
    let v = -normalize(rel);
    let r = reflect(-v, s.normal);
    let open = light.y * light.y;
    let sky = sky_gradient(g, normalize(vec3<f32>(r.x, max(r.y, 0.02), r.z))) * (0.15 + open * 0.85);
    let f0 = mix(vec3<f32>(0.04), albedo, s.metal);
    let fresnel = f0 + (1.0 - f0) * pow(1.0 - max(dot(s.normal, v), 0.0), 5.0);
    let gloss = 1.0 - s.rough / 0.4;
    return reflection(rel, r, sky) * fresnel * gloss * gloss;
}

@fragment
fn fs_opaque(in: VertexOut) -> @location(0) vec4<f32> {
    let tex = sample_tile(in.uv, in.layer);
    // In an opaque block's texture, alpha says how much of it takes the biome's colour.
    let albedo = tex.rgb * mix(vec3<f32>(1.0), in.tint, tex.a);
    let s = surface_at(in.uv, in.layer, in.flags);
    var color = light_up(albedo, in.rel, in.normal, in.light, in.flags, s) + mirrored(albedo, in.rel, in.light, s);
    if ((in.flags & LAVA) != 0u) {
        let world = in.uv;
        let flow = value_noise(world * 3.0 + vec2<f32>(g.ambient.w * 0.09, -g.ambient.w * 0.06));
        let pulse = 0.75 + 0.25 * sin(g.ambient.w * 0.8 + world.x * 2.0);
        color = mix(vec3<f32>(1.6, 0.34, 0.03), vec3<f32>(3.2, 1.7, 0.35), flow * flow) * pulse;
    }
    return vec4<f32>(apply_haze(g, color, in.rel), 1.0);
}

@fragment
fn fs_cutout(in: VertexOut) -> @location(0) vec4<f32> {
    let tex = sample_tile(in.uv, in.layer);
    if (tex.a < 0.5) {
        discard;
    }
    let albedo = tex.rgb * in.tint;
    // Plants stand at angles of their own: their texels light as the face does, rough as the
    // material says.
    let m = textureSampleGrad(materials, tile_sampler, in.uv, in.layer, dpdx(in.uv), dpdy(in.uv));
    let color = light_up(albedo, in.rel, in.normal, in.light, in.flags, Surface(in.normal, max(m.b, 0.04), m.a));
    return vec4<f32>(apply_haze(g, color, in.rel), 1.0);
}

// Water and ice: blended, and aware of what is under them.
@group(1) @binding(0) var scene_depth: texture_depth_2d;

fn water_normal(world: vec2<f32>, t: f32) -> vec3<f32> {
    // A few layers of drifting ripples.
    let a = value_noise(world * 0.9 + vec2<f32>(t * 0.33, t * 0.21));
    let b = value_noise(world * 2.3 - vec2<f32>(t * 0.47, t * 0.13));
    let c = value_noise(world * 5.1 + vec2<f32>(-t * 0.61, t * 0.57));
    let e = 0.08;
    let ax = value_noise((world + vec2<f32>(e, 0.0)) * 0.9 + vec2<f32>(t * 0.33, t * 0.21));
    let az = value_noise((world + vec2<f32>(0.0, e)) * 0.9 + vec2<f32>(t * 0.33, t * 0.21));
    let bx = value_noise((world + vec2<f32>(e, 0.0)) * 2.3 - vec2<f32>(t * 0.47, t * 0.13));
    let bz = value_noise((world + vec2<f32>(0.0, e)) * 2.3 - vec2<f32>(t * 0.47, t * 0.13));
    let cx = value_noise((world + vec2<f32>(e, 0.0)) * 5.1 + vec2<f32>(-t * 0.61, t * 0.57));
    let cz = value_noise((world + vec2<f32>(0.0, e)) * 5.1 + vec2<f32>(-t * 0.61, t * 0.57));
    let dx = (ax - a) * 0.5 + (bx - b) * 0.3 + (cx - c) * 0.16;
    let dz = (az - a) * 0.5 + (bz - b) * 0.3 + (cz - c) * 0.16;
    return normalize(vec3<f32>(-dx / e * 0.5, 1.0, -dz / e * 0.5));
}

// The broadest of those ripples on its own: smooth from one pixel to the next, even far away.
fn swell_normal(world: vec2<f32>, t: f32) -> vec3<f32> {
    let e = 0.08;
    let drift = vec2<f32>(t * 0.33, t * 0.21);
    let a = value_noise(world * 0.9 + drift);
    let ax = value_noise((world + vec2<f32>(e, 0.0)) * 0.9 + drift);
    let az = value_noise((world + vec2<f32>(0.0, e)) * 0.9 + drift);
    return normalize(vec3<f32>(-(ax - a) / e * 0.25, 1.0, -(az - a) / e * 0.25));
}

@fragment
fn fs_translucent(in: VertexOut) -> @location(0) vec4<f32> {
    if ((in.flags & WATER) == 0u) {
        // Ice, and anything else see-through and solid.
        let tex = sample_tile(in.uv, in.layer);
        let s = plain_surface(in.normal, 0.06);
        let albedo = tex.rgb * in.tint;
        let color = light_up(albedo, in.rel, in.normal, in.light, in.flags, s) + mirrored(albedo, in.rel, in.light, s);
        return vec4<f32>(apply_haze(g, color, in.rel), tex.a);
    }
    let view = normalize(in.rel);
    let world = in.rel.xz + g.camera.xz;
    let sky = in.light.y;
    let t = g.ambient.w;
    let rough = 1.0 + g.sky_horizon.w * 1.5;
    // Ripples smooth out with distance: too fine to resolve, they would only be noise.
    let calm = exp(-length(in.rel) * 0.022);
    var n = vec3<f32>(0.0, 1.0, 0.0);
    if (in.normal.y > 0.5) {
        n = water_normal(world, t);
        n = normalize(vec3<f32>(n.x * rough * calm, n.y, n.z * rough * calm));
    } else {
        n = in.normal;
    }

    // How deep the water is along the line of sight, from the depth of what is behind it.
    let px = vec2<i32>(in.clip.xy);
    let behind = textureLoad(scene_depth, px, 0);
    // With an infinite reversed depth, depth = near / distance.
    let near = 0.05;
    let surface_w = near / max(in.clip.z, 0.0000001);
    let behind_w = near / max(behind, 0.0000001);
    let thickness = clamp(behind_w - surface_w, 0.0, 256.0);

    let absorb = 1.0 - exp(-thickness * 0.22);

    if (g.fog.y > 0.5 && g.fog.z < 0.5 && !full_light(g)) {
        // The classic look: water is its own texture, drifting slowly, and see-through. No mirror.
        let drift = vec2<f32>(t * 0.05, t * 0.02);
        let tex = sample_tile(in.uv + drift, in.layer);
        let plain = classic_light(tex.rgb * in.tint, in.normal, in.light, in.flags);
        return vec4<f32>(apply_haze(g, plain, in.rel), mix(0.62, 0.86, absorb));
    }
    let deep = in.tint * vec3<f32>(0.42, 0.62, 0.72);
    let shallow = in.tint * vec3<f32>(1.0, 1.25, 1.2);
    let body = mix(shallow, deep, absorb);
    let open = sky * sky;
    var sun = g.light_dir.w;
    if (sun > 0.001) {
        sun *= water_shadow(in.rel);
    }
    let lit_body = body * (g.ambient.rgb * (0.25 + open * 0.75) + g.light_color.rgb * sun * max(g.light_dir.y, 0.0) * 0.5 * open + vec3<f32>(1.0, 0.68, 0.36) * pow(in.light.z, 2.4));

    // Water mirrors what is above it at a grazing angle and shows its own depths looking down.
    let cos_view = clamp(dot(-view, n), 0.0, 1.0);
    let fresnel = 0.03 + 0.97 * pow(1.0 - cos_view, 5.0);
    let reflected = reflect(view, n);
    let mirror_dir = normalize(vec3<f32>(reflected.x, abs(reflected.y), reflected.z));
    var mirror = sky_gradient(g, mirror_dir) * (0.18 + open * 0.82);
    if (in.normal.y > 0.5 && fresnel > 0.035) {
        // What is mirrored follows only the broad swell. The finer ripples would scatter neighbouring
        // rays between shore and sky, and the surface would sparkle with noise.
        let swell = swell_normal(world, t);
        let still = normalize(vec3<f32>(swell.x * rough * calm, swell.y, swell.z * rough * calm));
        let bounced = reflect(view, still);
        mirror = reflection(in.rel, normalize(vec3<f32>(bounced.x, abs(bounced.y), bounced.z)), mirror);
    }
    var color = mix(lit_body, mirror, fresnel * step(0.5, in.normal.y));
    var alpha = mix(0.55 + absorb * 0.4, 1.0, fresnel);

    if (in.normal.y > 0.5) {
        // The sun or moon, glittering on the ripples.
        let h = normalize(g.light_dir.xyz - view);
        let spec = pow(max(dot(n, h), 0.0), 420.0) * 7.0 * sun * open;
        color += g.light_color.rgb * spec;
        // A bright line where water meets the shore.
        let foam = smoothstep(0.9, 0.0, thickness) * (0.45 + 0.55 * value_noise(world * 3.0 + vec2<f32>(t * 0.4, 0.0)));
        color += (g.ambient.rgb + g.light_color.rgb * g.light_dir.w) * foam * 0.35;
        alpha = max(alpha, foam * 0.75);
    }
    // From inside the water the surface is just a bright ceiling.
    if (g.fog.z > 0.5) {
        color = lit_body * 1.4;
        alpha = 0.7;
    }
    return vec4<f32>(apply_haze(g, color, in.rel), alpha);
}
