// The game's icon, painted by code like everything else in it: a sun going down behind snowy
// mountains, on a rounded square. It uses nothing but `std`, because the build script includes this
// file to paint the same picture into the Windows program's own icon.

/// The icon, `size` pixels square, as RGBA rows from the top.
pub fn paint_icon(size: u32) -> Vec<u8> {
    // Each pixel is the average of a grid of samples, so edges stay smooth even at 16 pixels.
    const GRID: u32 = 4;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut sum = [0f32; 4];
            for sy in 0..GRID {
                for sx in 0..GRID {
                    let u = (x as f32 + (sx as f32 + 0.5) / GRID as f32) / size as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / GRID as f32) / size as f32;
                    if let Some(c) = icon_at(u, v) {
                        sum = [sum[0] + c[0], sum[1] + c[1], sum[2] + c[2], sum[3] + 1.0];
                    }
                }
            }
            let covered = sum[3];
            let channel = |c: f32| if covered > 0.0 { byte(c / covered) } else { 0 };
            out.extend([channel(sum[0]), channel(sum[1]), channel(sum[2]), byte(covered / (GRID * GRID) as f32)]);
        }
    }
    out
}

fn byte(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// How far down a ridge is at `u`, from straight slopes between its peaks and passes.
fn ridge(u: f32, points: &[(f32, f32)]) -> f32 {
    for pair in points.windows(2) {
        let ((u0, v0), (u1, v1)) = (pair[0], pair[1]);
        if u <= u1 {
            return v0 + (v1 - v0) * ((u - u0) / (u1 - u0)).clamp(0.0, 1.0);
        }
    }
    points.last().map_or(1.0, |p| p.1)
}

/// The colour at a point, `u` across and `v` down, both 0 to 1; nothing outside the rounded square.
fn icon_at(u: f32, v: f32) -> Option<[f32; 3]> {
    let corner = 0.19;
    let (dx, dy) = ((u - 0.5).abs() - (0.5 - corner), (v - 0.5).abs() - (0.5 - corner));
    if dx > 0.0 && dy > 0.0 && dx * dx + dy * dy > corner * corner {
        return None;
    }
    // The sky: night overhead, burning toward the horizon.
    let t = v / 0.68;
    let mut c = if t < 0.55 {
        mix([0.11, 0.09, 0.30], [0.62, 0.24, 0.42], t / 0.55)
    } else {
        mix([0.62, 0.24, 0.42], [1.0, 0.62, 0.30], (t - 0.55) / 0.45)
    };
    // The sun, sinking into a pass, with a glow around it.
    let (sun_u, sun_v, sun_r) = (0.69, 0.55, 0.15);
    let d = ((u - sun_u).powi(2) + (v - sun_v).powi(2)).sqrt();
    c = mix(c, [1.0, 0.82, 0.55], (1.0 - (d - sun_r) / 0.22).clamp(0.0, 1.0).powi(2) * 0.45);
    if d < sun_r {
        c = [1.0, 0.93, 0.72];
    }
    // The far range, with snow on its high peak.
    let far =
        [(0.0, 0.6), (0.13, 0.47), (0.24, 0.55), (0.40, 0.27), (0.55, 0.53), (0.69, 0.63), (0.86, 0.48), (1.0, 0.58)];
    let top = ridge(u, &far);
    if v > top {
        let snowy = top < 0.42 && v < top + 0.07 + (u * 37.0).sin().abs() * 0.025;
        c = if snowy { [0.93, 0.91, 1.0] } else { mix([0.25, 0.17, 0.32], [0.13, 0.10, 0.20], (v - top) * 6.0) };
    }
    // And a nearer, darker one in front.
    let near = [(0.0, 0.74), (0.22, 0.66), (0.47, 0.78), (0.74, 0.69), (1.0, 0.76)];
    if v > ridge(u, &near) {
        c = [0.06, 0.05, 0.10];
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_square_has_clear_corners_and_a_solid_middle() {
        let size = 32;
        let icon = paint_icon(size);
        assert_eq!(icon.len(), (size * size * 4) as usize);
        let alpha = |x: u32, y: u32| icon[((y * size + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "a rounded corner");
        assert_eq!(alpha(16, 16), 255);
        assert_eq!(alpha(16, 0), 255, "the edge midway along is inside");
    }
}
