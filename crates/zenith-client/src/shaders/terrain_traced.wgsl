// Light for chunks with ray tracing: rays toward the sun for shadows, and off water for reflections.
//
// Every section near the player is in `scene`, built from the very vertices the chunks are drawn
// with. A ray that hits something can therefore look up what it hit: its texture, its light, its tint.

@group(2) @binding(0) var scene: acceleration_structure;
// The chunk vertices, five words each, laid out as `zenith_mesh::Vertex`.
@group(2) @binding(1) var<storage, read> arena: array<u32>;
// For each traced section, where each of its geometries starts in `arena`. The top bit marks a cutout.
@group(2) @binding(2) var<storage, read> sections: array<vec2<u32>>;
// The camera's place in `scene`, which is laid out around an anchor that seldom moves: everything else
// here is relative to the camera, so a ray starts at its point plus this.
@group(2) @binding(3) var<uniform> scene_camera: vec4<f32>;

const CUTOUT_GEOMETRY: u32 = 0x80000000u;
// Further than anything traced can be.
const TRACE_FAR: f32 = 2048.0;

// The triangle a ray met: the first word of each corner, and how much of each corner is where it struck.
struct Triangle {
    words: vec3<u32>,
    weights: vec3<f32>,
    cutout: bool,
};

fn triangle(i: RayIntersection) -> Triangle {
    let start = sections[i.instance_custom_data][i.geometry_index];
    // Quads are four corners in a row, cut into the triangles 0-1-2 and 0-2-3.
    let quad = (start & ~CUTOUT_GEOMETRY) + (i.primitive_index >> 1u) * 4u;
    let second = i.primitive_index & 1u;
    var t: Triangle;
    t.words = vec3<u32>(quad, quad + 1u + second, quad + 2u + second) * 5u;
    t.weights = vec3<f32>(1.0 - i.barycentrics.x - i.barycentrics.y, i.barycentrics.x, i.barycentrics.y);
    t.cutout = (start & CUTOUT_GEOMETRY) != 0u;
    return t;
}

fn halves(w: u32) -> vec2<f32> {
    return vec2<f32>(f32(w & 0xFFFFu), f32(w >> 16u));
}

fn tile_uv(t: Triangle) -> vec2<f32> {
    let w = t.weights;
    return (halves(arena[t.words.x + 2u]) * w.x + halves(arena[t.words.y + 2u]) * w.y + halves(arena[t.words.z + 2u]) * w.z) / 256.0;
}

fn tile_layer(t: Triangle) -> u32 {
    return arena[t.words.x + 3u] & 0xFFFFu;
}

// Leaves and plants are see-through between their pixels: a ray only stops where the picture is solid.
fn stops_ray(i: RayIntersection) -> bool {
    let t = triangle(i);
    return textureSampleLevel(tiles, tile_sampler, tile_uv(t), tile_layer(t), 0.0).a >= 0.5;
}

// 1 if the sun (or moon) can be seen from `origin`, 0 if anything solid is in the way.
fn sunlight(origin: vec3<f32>, start: f32) -> f32 {
    var rq: ray_query;
    rayQueryInitialize(&rq, scene, RayDesc(RAY_FLAG_TERMINATE_ON_FIRST_HIT, 0xFFu, start, TRACE_FAR, origin + scene_camera.xyz, g.light_dir.xyz));
    while (rayQueryProceed(&rq)) {
        if (stops_ray(rayQueryGetCandidateIntersection(&rq))) {
            rayQueryConfirmIntersection(&rq);
        }
    }
    return select(1.0, 0.0, rayQueryGetCommittedIntersection(&rq).kind != RAY_QUERY_INTERSECTION_NONE);
}

fn shadow_at(rel: vec3<f32>, normal: vec3<f32>, flags: u32) -> f32 {
    // Plants sway in the wind and their traced copies stand still: a ray from one starts clear of it.
    let start = select(0.01, 0.35, (flags & (SWAY_SOFT | SWAY_TOP)) != 0u);
    return sunlight(rel + normal * 0.002, start);
}

fn water_shadow(rel: vec3<f32>) -> f32 {
    return sunlight(rel, 0.05);
}

// What water mirrors in direction `dir`: whatever a ray meets first, lit as the world is, or `sky`.
fn reflection(rel: vec3<f32>, dir: vec3<f32>, sky: vec3<f32>) -> vec3<f32> {
    var rq: ray_query;
    rayQueryInitialize(&rq, scene, RayDesc(RAY_FLAG_NONE, 0xFFu, 0.05, TRACE_FAR, rel + scene_camera.xyz, dir));
    while (rayQueryProceed(&rq)) {
        if (stops_ray(rayQueryGetCandidateIntersection(&rq))) {
            rayQueryConfirmIntersection(&rq);
        }
    }
    let hit = rayQueryGetCommittedIntersection(&rq);
    if (hit.kind == RAY_QUERY_INTERSECTION_NONE) {
        return sky;
    }
    let t = triangle(hit);
    let w = t.weights;
    let first = arena[t.words.x + 1u];
    let flags = first >> 24u;
    let ao = dot(vec3<f32>(f32((first >> 16u) & 255u), f32((arena[t.words.y + 1u] >> 16u) & 255u), f32((arena[t.words.z + 1u] >> 16u) & 255u)), w);
    let l0 = arena[t.words.x + 3u] >> 16u;
    let l1 = arena[t.words.y + 3u] >> 16u;
    let l2 = arena[t.words.z + 3u] >> 16u;
    let sky_light = dot(vec3<f32>(f32(l0 & 255u), f32(l1 & 255u), f32(l2 & 255u)), w);
    let block_light = dot(vec3<f32>(f32(l0 >> 8u), f32(l1 >> 8u), f32(l2 >> 8u)), w);
    let tint = unpack4x8unorm(arena[t.words.x + 4u]).rgb * w.x + unpack4x8unorm(arena[t.words.y + 4u]).rgb * w.y + unpack4x8unorm(arena[t.words.z + 4u]).rgb * w.z;

    let at = rel + dir * hit.t;
    // A ray has no neighbours to compare with: how blurred the texture should be comes from how far it went.
    let lod = log2(1.0 + (length(rel) + hit.t) * 0.06);
    let tex = textureSampleLevel(tiles, tile_sampler, tile_uv(t), tile_layer(t), lod);
    let linear_tint = pow(tint, vec3<f32>(2.2));
    var albedo = tex.rgb * mix(vec3<f32>(1.0), linear_tint, tex.a);
    if (t.cutout) {
        albedo = tex.rgb * linear_tint;
    }
    let face = face_normal(flags & 7u);
    var color = light_up(albedo, at, face, vec3<f32>(ao, sky_light, block_light) / 255.0, flags, plain_surface(face, 0.8));
    if ((flags & LAVA) != 0u) {
        color = vec3<f32>(2.4, 1.0, 0.2);
    }
    return apply_haze(g, color, at);
}
