// Light for chunks without ray tracing: shadows from the shadow map, and water that mirrors the sky.

fn shadow_at(rel: vec3<f32>, normal: vec3<f32>, flags: u32) -> f32 {
    let texel = 0.09;
    for (var i = 0u; i < 2u; i++) {
        let offset = normal * (texel * (1.0 + f32(i) * 2.5));
        let p = g.shadow[i] * vec4<f32>(rel + offset, 1.0);
        let uv = vec2<f32>(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
        if (all(uv > vec2<f32>(0.01)) && all(uv < vec2<f32>(0.99)) && p.z > 0.0 && p.z < 1.0) {
            var lit = 0.0;
            let step = 1.0 / 2048.0;
            for (var y = -1; y <= 1; y++) {
                for (var x = -1; x <= 1; x++) {
                    lit += textureSampleCompareLevel(shadow_map, shadow_sampler, uv + vec2<f32>(f32(x), f32(y)) * step, i, p.z);
                }
            }
            lit /= 9.0;
            // The furthest edge of the shadowed area fades out rather than stopping at a line.
            if (i == 1u) {
                let edge = max(abs(p.x), abs(p.y));
                return mix(lit, 1.0, smoothstep(0.8, 0.98, edge));
            }
            return lit;
        }
    }
    return 1.0;
}

// Water is left out of the shadow map, so nothing shadows it.
fn water_shadow(rel: vec3<f32>) -> f32 {
    return 1.0;
}

// Only the sky is reflected: `sky` is what the water would mirror.
fn reflection(rel: vec3<f32>, dir: vec3<f32>, sky: vec3<f32>) -> vec3<f32> {
    return sky;
}
