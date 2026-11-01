//! Axis-aligned boxes in world space.
use crate::pos::Dir;
use glam::DVec3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: DVec3,
    pub max: DVec3,
}

impl Aabb {
    pub const fn new(min: DVec3, max: DVec3) -> Self {
        Self { min, max }
    }
    /// A box standing on `feet`, as wide as `width` and as tall as `height`.
    pub fn standing(feet: DVec3, width: f64, height: f64) -> Self {
        let r = width * 0.5;
        Self::new(DVec3::new(feet.x - r, feet.y, feet.z - r), DVec3::new(feet.x + r, feet.y + height, feet.z + r))
    }
    pub fn offset(self, d: DVec3) -> Self {
        Self::new(self.min + d, self.max + d)
    }
    /// Grown by `d` on the side `d` points to, so it covers the sweep of a move by `d`.
    pub fn swept(self, d: DVec3) -> Self {
        Self::new(self.min + d.min(DVec3::ZERO), self.max + d.max(DVec3::ZERO))
    }
    pub fn inflate(self, by: f64) -> Self {
        Self::new(self.min - DVec3::splat(by), self.max + DVec3::splat(by))
    }
    pub fn intersects(&self, o: &Aabb) -> bool {
        self.min.x < o.max.x
            && self.max.x > o.min.x
            && self.min.y < o.max.y
            && self.max.y > o.min.y
            && self.min.z < o.max.z
            && self.max.z > o.min.z
    }
    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }
    /// How far `dy` can go before `self` hits `o`, moving along y.
    pub fn clip_y(&self, o: &Aabb, dy: f64) -> f64 {
        if self.max.x <= o.min.x || self.min.x >= o.max.x || self.max.z <= o.min.z || self.min.z >= o.max.z {
            return dy;
        }
        if dy > 0.0 && self.max.y <= o.min.y {
            dy.min(o.min.y - self.max.y)
        } else if dy < 0.0 && self.min.y >= o.max.y {
            dy.max(o.max.y - self.min.y)
        } else {
            dy
        }
    }
    pub fn clip_x(&self, o: &Aabb, dx: f64) -> f64 {
        if self.max.y <= o.min.y || self.min.y >= o.max.y || self.max.z <= o.min.z || self.min.z >= o.max.z {
            return dx;
        }
        if dx > 0.0 && self.max.x <= o.min.x {
            dx.min(o.min.x - self.max.x)
        } else if dx < 0.0 && self.min.x >= o.max.x {
            dx.max(o.max.x - self.min.x)
        } else {
            dx
        }
    }
    pub fn clip_z(&self, o: &Aabb, dz: f64) -> f64 {
        if self.max.y <= o.min.y || self.min.y >= o.max.y || self.max.x <= o.min.x || self.min.x >= o.max.x {
            return dz;
        }
        if dz > 0.0 && self.max.z <= o.min.z {
            dz.min(o.min.z - self.max.z)
        } else if dz < 0.0 && self.min.z >= o.max.z {
            dz.max(o.max.z - self.min.z)
        } else {
            dz
        }
    }
    /// Where a ray enters this box, as a fraction of `dir`, and through which face.
    pub fn ray(&self, origin: DVec3, dir: DVec3) -> Option<(f64, Dir)> {
        let mut t_near = f64::NEG_INFINITY;
        let mut t_far = f64::INFINITY;
        let mut face = Dir::Up;
        let axes = [
            (origin.x, dir.x, self.min.x, self.max.x, Dir::West, Dir::East),
            (origin.y, dir.y, self.min.y, self.max.y, Dir::Down, Dir::Up),
            (origin.z, dir.z, self.min.z, self.max.z, Dir::North, Dir::South),
        ];
        for (o, d, lo, hi, lo_face, hi_face) in axes {
            if d == 0.0 {
                if o < lo || o > hi {
                    return None;
                }
                continue;
            }
            let (t0, t1, entered) =
                if d > 0.0 { ((lo - o) / d, (hi - o) / d, lo_face) } else { ((hi - o) / d, (lo - o) / d, hi_face) };
            if t0 > t_near {
                t_near = t0;
                face = entered;
            }
            t_far = t_far.min(t1);
            if t_near > t_far {
                return None;
            }
        }
        (t_far >= 0.0).then_some((t_near.max(0.0), face))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> Aabb {
        Aabb::new(DVec3::ZERO, DVec3::ONE)
    }

    #[test]
    fn falling_box_stops_on_the_one_below() {
        let falling = unit().offset(DVec3::new(0.0, 2.0, 0.0));
        assert_eq!(falling.clip_y(&unit(), -5.0), -1.0);
    }

    #[test]
    fn a_box_beside_the_path_does_not_clip() {
        let falling = unit().offset(DVec3::new(2.0, 2.0, 0.0));
        assert_eq!(falling.clip_y(&unit(), -5.0), -5.0);
    }

    #[test]
    fn touching_boxes_do_not_intersect() {
        assert!(!unit().intersects(&unit().offset(DVec3::X)));
        assert!(unit().intersects(&unit().offset(DVec3::X * 0.99)));
    }

    #[test]
    fn ray_reports_the_face_it_enters() {
        let (t, face) = unit().ray(DVec3::new(-1.0, 0.5, 0.5), DVec3::X).expect("hits");
        assert_eq!((t, face), (1.0, Dir::West));
        let (t, face) = unit().ray(DVec3::new(0.5, 3.0, 0.5), DVec3::NEG_Y).expect("hits");
        assert_eq!((t, face), (2.0, Dir::Up));
        assert!(unit().ray(DVec3::new(-1.0, 2.0, 0.5), DVec3::X).is_none());
        assert!(unit().ray(DVec3::new(2.0, 0.5, 0.5), DVec3::X).is_none(), "behind the origin");
    }
}
