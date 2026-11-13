// The sky: a gradient, the sun and moon, stars and a layer of clouds.
@group(0) @binding(0) var<uniform> g: Globals;

struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> Out {
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    var out: Out;
    out.clip = vec4<f32>(x, y, 0.0, 1.0);
    out.ndc = vec2<f32>(x, y);
    return out;
}

// Each octave is turned against the last, so no two grids line up into a visible straight edge.
fn fbm(p_in: vec2<f32>) -> f32 {
    let turn = mat2x2<f32>(0.8, 0.6, -0.6, 0.8);
    var p = p_in;
    var a = 0.5;
    var sum = 0.0;
    for (var i = 0; i < 5; i++) {
        sum += gradient_noise(p) * a;
        p = turn * p * 2.03 + vec2<f32>(17.1, 9.2);
        a *= 0.5;
    }
    return 0.5 + sum * 0.62;
}

// Mostly faint points, and a few bright ones, turning with the sun around the same axis.
fn stars(d: vec3<f32>, sun: vec3<f32>) -> vec3<f32> {
    let spin = normalize(sun.xy);
    let dir = vec3<f32>(spin.x * d.x + spin.y * d.y, spin.x * d.y - spin.y * d.x, d.z);
    // The faces of a cube keep the cells about one size all over the sky.
    let a = abs(dir);
    var uv = dir.xy / max(a.z, 0.0001);
    var face = sign(dir.z) * 2.0;
    if (a.x >= a.y && a.x >= a.z) {
        uv = dir.yz / a.x;
        face = sign(dir.x);
    } else if (a.y >= a.z) {
        uv = dir.zx / a.y;
        face = sign(dir.y) * 3.0;
    }
    let p = uv * 170.0 + vec2<f32>(face * 1000.0, face * 2000.0);
    let h = pcg2d(vec2<i32>(floor(p)));
    let odds = f32(h.x) / 4294967296.0;
    if (odds > 0.035) {
        return vec3<f32>(0.0);
    }
    let bright = odds / 0.035;
    let spot = vec2<f32>(f32(h.y & 65535u), f32(h.y >> 16u)) / 65536.0;
    if (g.fog.y > 0.5) {
        // The classic look: hard little squares, no twinkle.
        let q = abs(fract(p) - 0.2 - spot * 0.6);
        let size = 0.1 + 0.12 * bright * bright;
        return vec3<f32>(0.9, 0.92, 1.0) * select(0.0, 0.3 + 2.0 * pow(bright, 6.0), max(q.x, q.y) < size);
    }
    let d_spot = length(fract(p) - 0.2 - spot * 0.6);
    let radius = 0.16 + 0.18 * bright * bright * bright;
    let twinkle = 0.8 + 0.2 * sin(g.ambient.w * (1.5 + bright * 7.0) + f32(h.y & 255u));
    let tint = mix(vec3<f32>(1.0, 0.82, 0.68), vec3<f32>(0.72, 0.84, 1.0), spot.x);
    return tint * smoothstep(radius, radius * 0.25, d_spot) * (0.28 + 3.2 * pow(bright, 6.0)) * twinkle;
}

// The classic look's sun and moon: discs of big square pixels, eight across. Gives whether `dir` is
// on the disc around `centre`, `size` wide as a sine, and the pixel it is on, counted from the middle.
fn pixel_disc(dir: vec3<f32>, centre: vec3<f32>, size: f32) -> vec3<f32> {
    if (dot(dir, centre) <= 0.0) {
        return vec3<f32>(0.0);
    }
    // The sun and moon travel around the z axis, so this keeps their pixels upright all day.
    let right = normalize(cross(centre, vec3<f32>(0.0, 0.0, 1.0)));
    let up = cross(right, centre);
    let cell = floor(vec2<f32>(dot(dir, right), dot(dir, up)) / size * 4.0);
    let mid = cell + 0.5;
    return vec3<f32>(select(0.0, 1.0, dot(mid, mid) < 16.0), cell);
}

fn classic_sun_and_moon(dir: vec3<f32>, sun: vec3<f32>, rain: f32) -> vec3<f32> {
    var color = vec3<f32>(0.0);
    let s = pixel_disc(dir, sun, 0.06);
    if (s.x > 0.5) {
        let low = 1.0 - clamp(sun.y * 3.0, 0.0, 1.0);
        let core = dot(s.yz + 0.5, s.yz + 0.5) < 6.0;
        let rim = mix(vec3<f32>(1.0, 0.78, 0.3), vec3<f32>(1.0, 0.45, 0.15), low);
        let tint = select(rim, vec3<f32>(1.0, 0.95, 0.68), core);
        color = mix(color, tint, smoothstep(-0.06, 0.02, sun.y) * (1.0 - rain * 0.9));
    }
    let m = pixel_disc(dir, -sun, 0.05);
    if (m.x > 0.5) {
        // A few darker pixels for its seas.
        let sea = hash21(m.yz + vec2<f32>(40.0, 70.0)) < 0.28;
        let tint = select(vec3<f32>(0.86, 0.88, 0.94), vec3<f32>(0.6, 0.63, 0.72), sea);
        color += tint * 0.9 * smoothstep(-0.06, 0.02, -sun.y) * (1.0 - rain);
    }
    return color;
}

// One cell of the classic clouds: more than about half means cloud. Cells come in lumps, with some
// scatter of their own, and repeat every 256 cells: that is 4096 blocks, where the camera's position
// wraps, so that crossing the line moves no cloud.
fn cloud_cell(cell: vec2<f32>) -> f32 {
    let c = cell - floor(cell / 256.0) * 256.0;
    let p = c / 8.0;
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let wrap = 32.0;
    let i10 = i + vec2<f32>(1.0, 0.0);
    let i01 = i + vec2<f32>(0.0, 1.0);
    let i11 = i + vec2<f32>(1.0, 1.0);
    let a = hash21(i - floor(i / wrap) * wrap);
    let b = hash21(i10 - floor(i10 / wrap) * wrap);
    let d = hash21(i01 - floor(i01 / wrap) * wrap);
    let e = hash21(i11 - floor(i11 / wrap) * wrap);
    let lumps = mix(mix(a, b, u.x), mix(d, e, u.x), u.y);
    return lumps * 0.8 + hash21(c + vec2<f32>(1000.0, 3000.0)) * 0.2;
}

// The classic look's clouds: one flat layer of square cells, sixteen blocks across, drifting east.
fn classic_clouds(dir: vec3<f32>, color: vec3<f32>) -> vec3<f32> {
    if (abs(dir.y) < 0.004) {
        return color;
    }
    let t = (192.0 - g.camera.w) / dir.y;
    if (t <= 0.0 || t > 2400.0) {
        return color;
    }
    let at = g.camera.xz + dir.xz * t + vec2<f32>(g.ambient.w * 1.5, 0.0);
    if (cloud_cell(floor(at / 16.0)) < mix(0.6, 0.46, g.sky_horizon.w)) {
        return color;
    }
    let warm = mix(vec3<f32>(1.0, 0.72, 0.55), vec3<f32>(1.0), clamp(g.sun_dir.y * 3.5, 0.0, 1.0));
    var cloud = mix(vec3<f32>(0.16, 0.18, 0.26), warm, g.light_color.w);
    // From below, the underside is a little darker.
    cloud *= select(1.0, 0.9, dir.y > 0.0);
    cloud = mix(cloud, cloud * 0.55, g.sky_horizon.w);
    return mix(color, cloud, 0.85 * smoothstep(2400.0, 700.0, t));
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let far = g.inv_view_proj * vec4<f32>(in.ndc, 0.5, 1.0);
    let dir = normalize(far.xyz / far.w);
    var color = sky_gradient(g, dir);
    let rain = g.sky_horizon.w;

    if (dir.y > -0.05) {
        let night = g.sky_zenith.w;
        if (night > 0.01) {
            color += stars(dir, g.sun_dir.xyz) * night * clamp(dir.y * 8.0 + 0.4, 0.0, 1.0) * (1.0 - rain);
        }
    }

    if (g.fog.y > 0.5) {
        color += classic_sun_and_moon(dir, g.sun_dir.xyz, rain);
        if (!hide_clouds(g)) {
            color = classic_clouds(dir, color);
        }
        if (g.fog.z > 0.5) {
            color = vec3<f32>(0.03, 0.14, 0.24) * (0.3 + g.light_color.w * 0.7);
        }
        return vec4<f32>(color, 1.0);
    }

    // The sun, bright enough to bloom. The moon, across from it.
    let sun = g.sun_dir.xyz;
    let cos_sun = dot(dir, sun);
    let disc = smoothstep(0.99965, 0.99985, cos_sun);
    let above = smoothstep(-0.06, 0.02, sun.y);
    let sun_tint = mix(vec3<f32>(1.0, 0.45, 0.15), vec3<f32>(1.0, 0.95, 0.82), clamp(sun.y * 3.0, 0.0, 1.0));
    color += sun_tint * disc * 26.0 * above * (1.0 - rain * 0.9);

    let moon_dir = -sun;
    let cos_moon = dot(dir, moon_dir);
    if (cos_moon > 0.999) {
        let edge = smoothstep(0.99925, 0.9995, cos_moon);
        let spots = 0.75 + 0.25 * value_noise(dir.xz * 90.0);
        color += vec3<f32>(0.82, 0.86, 0.94) * edge * spots * 1.4 * smoothstep(-0.06, 0.02, moon_dir.y) * (1.0 - rain);
    }
    color += vec3<f32>(0.5, 0.56, 0.7) * pow(max(cos_moon, 0.0), 500.0) * 0.1 * g.sky_zenith.w;

    // Clouds: a flat layer high up, seen through the ray that reaches it.
    if (dir.y > 0.012 && !hide_clouds(g)) {
        let height = 340.0 - min(g.camera.w, 300.0);
        let dist = height / dir.y;
        let at = dir.xz * dist + g.camera.xz + vec2<f32>(g.ambient.w * 1.6, g.ambient.w * 0.5);
        let q = at * 0.0016;
        let warped = q + vec2<f32>(gradient_noise(q * 0.7 + 3.1), gradient_noise(q * 0.7 + 11.7)) * 0.55;
        let n = fbm(warped);
        let coverage = mix(0.53, 0.36, rain);
        let density = smoothstep(coverage, coverage + 0.2, n);
        if (density > 0.001) {
            // Thick parts are darker underneath, and the sun lights the rest from its side.
            let toward = fbm(warped + sun.xz * 0.14);
            let shade = clamp(1.0 - (toward - n) * 3.2 - density * 0.25, 0.0, 1.0);
            let day = g.light_color.w;
            let lit = g.ambient.rgb * 1.05 + g.light_color.rgb * g.light_dir.w * (0.55 + shade * 0.5);
            let tinted_sun = mix(vec3<f32>(1.0, 0.56, 0.3), vec3<f32>(1.0), clamp(sun.y * 3.5, 0.0, 1.0));
            var cloud = lit * mix(vec3<f32>(1.0), tinted_sun, day) * mix(0.55, 1.0, shade);
            cloud = mix(cloud, cloud * 0.5, rain);
            let fade = smoothstep(0.012, 0.11, dir.y);
            color = mix(color, cloud, density * fade * 0.94);
        }
    }

    if (g.fog.z > 0.5) {
        color = vec3<f32>(0.03, 0.14, 0.24) * (0.3 + g.light_color.w * 0.7);
    }
    return vec4<f32>(color, 1.0);
}
