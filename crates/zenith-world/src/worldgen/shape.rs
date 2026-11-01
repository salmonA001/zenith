//! The large shape of the world: continents, mountains, rivers, climate and biomes.
//!
//! Everything is a continuous function of position, so biomes need no blending pass: heights and
//! colours flow across borders by themselves.
use super::Generator;
use zenith_core::pos::SEA_LEVEL;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Biome {
    DeepOcean,
    Ocean,
    River,
    Beach,
    #[default]
    Plains,
    Forest,
    BirchForest,
    Taiga,
    Tundra,
    Desert,
    Savanna,
    Rainforest,
    Swamp,
    Mountains,
    SnowyPeaks,
}

impl Biome {
    pub const fn name(self) -> &'static str {
        match self {
            Biome::DeepOcean => "Deep Ocean",
            Biome::Ocean => "Ocean",
            Biome::River => "River",
            Biome::Beach => "Beach",
            Biome::Plains => "Plains",
            Biome::Forest => "Forest",
            Biome::BirchForest => "Birch Forest",
            Biome::Taiga => "Taiga",
            Biome::Tundra => "Tundra",
            Biome::Desert => "Desert",
            Biome::Savanna => "Savanna",
            Biome::Rainforest => "Rainforest",
            Biome::Swamp => "Swamp",
            Biome::Mountains => "Mountains",
            Biome::SnowyPeaks => "Snowy Peaks",
        }
    }
}

/// What the world looks like from far above one column, before caves and decoration.
#[derive(Clone, Copy, Debug, Default)]
pub struct Column {
    pub height: f64,
    pub temperature: f64,
    pub humidity: f64,
    /// 0 on gentle land, towards 1 where cliffs and overhangs belong.
    pub rugged: f64,
    /// 1 at the middle of a river.
    pub river: f64,
}

#[inline]
pub(super) fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// 0 below `lo`, 1 above `hi`, smooth between. `lo` may exceed `hi` for a falling edge.
#[inline]
pub(super) fn smoothstep(lo: f64, hi: f64, x: f64) -> f64 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Land height from how far inland a place is.
const BASE_HEIGHT: [(f64, f64); 9] = [
    (-1.00, 18.0),
    (-0.50, 28.0),
    (-0.26, 44.0),
    (-0.14, 56.0),
    (-0.06, 61.5),
    (0.02, 65.0),
    (0.22, 71.0),
    (0.50, 80.0),
    (1.00, 96.0),
];

fn base_height(c: f64) -> f64 {
    let mut prev = BASE_HEIGHT[0];
    if c <= prev.0 {
        return prev.1;
    }
    for &next in &BASE_HEIGHT[1..] {
        if c <= next.0 {
            return lerp(prev.1, next.1, (c - prev.0) / (next.0 - prev.0));
        }
        prev = next;
    }
    prev.1
}

impl Generator {
    /// The shape of the world at one point. A pure function of the seed and the position.
    pub fn column(&self, x: f64, z: f64) -> Column {
        let wx = x + self.warp_x.get2(x / 310.0, z / 310.0) * 70.0;
        let wz = z + self.warp_z.get2(x / 310.0, z / 310.0) * 70.0;

        let c = (self.continent.get2(wx / 2600.0, wz / 2600.0) * 2.1 + 0.16).clamp(-1.0, 1.0);
        let e = (self.erosion.get2(wx / 980.0, wz / 980.0) * 2.0).clamp(-1.0, 1.0);
        let land = smoothstep(-0.08, 0.14, c);

        let ridge_noise = self.ridge.get2(wx / 390.0, wz / 390.0);
        let ridge = 1.0 - (ridge_noise.abs() * 3.0).min(1.0);

        // Low erosion raises ranges; the further inland, the higher they can go.
        let range = land * smoothstep(0.08, -0.42, e) * smoothstep(0.02, 0.32, c);
        let peaks = ridge * ridge * (0.55 + 0.45 * ridge);
        let mountain = peaks * range * 168.0 + range * 16.0;

        let hills_amp = 3.0 + 15.0 * land * (1.0 - e.abs() * 0.6) + 20.0 * range;
        let hills = self.hills.get2(wx / 150.0, wz / 150.0) * hills_amp;
        let detail = self.detail.get2(wx / 37.0, wz / 37.0) * (1.4 + 3.2 * land);

        let mut height = base_height(c) + hills * (0.35 + 0.65 * land) + mountain + detail;

        // Rivers cut through everything but the high ranges.
        let river_noise = self.river.get2(wx / 1250.0, wz / 1250.0).abs();
        let river = smoothstep(0.052, 0.012, river_noise) * smoothstep(-0.02, 0.1, c) * (1.0 - range * 0.92);
        let bed = SEA_LEVEL as f64 - 3.5;
        if height > bed {
            height = lerp(height, bed, river);
        }

        let temperature = (self.temp.get2(x / 1700.0, z / 1700.0) * 2.2).clamp(-1.0, 1.0);
        let humidity = (self.humid.get2(x / 1300.0, z / 1300.0) * 2.2).clamp(-1.0, 1.0);

        Column { height, temperature, humidity, rugged: smoothstep(0.1, 0.6, range), river }
    }

    /// Air gets colder the higher you go: this is the temperature at the surface of a column.
    pub fn surface_temperature(col: &Column) -> f64 {
        col.temperature - (col.height - SEA_LEVEL as f64).max(0.0) * 0.0105
    }

    pub fn biome(col: &Column) -> Biome {
        let h = col.height;
        let sea = SEA_LEVEL as f64;
        let cold = Self::surface_temperature(col);
        if h < sea - 16.0 {
            return Biome::DeepOcean;
        }
        if col.river > 0.45 {
            return Biome::River;
        }
        if h < sea - 0.5 {
            return Biome::Ocean;
        }
        if cold < -0.72 && (h > 118.0 || col.rugged > 0.3) {
            return Biome::SnowyPeaks;
        }
        if col.rugged > 0.5 || h > 138.0 {
            return Biome::Mountains;
        }
        if h < sea + 2.5 {
            if col.humidity > 0.3 && col.temperature > -0.12 {
                return Biome::Swamp;
            }
            if col.temperature > -0.35 {
                return Biome::Beach;
            }
        }
        match (col.temperature, col.humidity) {
            (t, w) if t < -0.28 => {
                if w > -0.05 {
                    Biome::Taiga
                } else {
                    Biome::Tundra
                }
            }
            (t, w) if t < 0.26 => {
                if w < -0.16 {
                    Biome::Plains
                } else if w < 0.22 {
                    Biome::Forest
                } else {
                    Biome::BirchForest
                }
            }
            (_, w) => {
                if w < -0.22 {
                    Biome::Desert
                } else if w < 0.14 {
                    Biome::Savanna
                } else {
                    Biome::Rainforest
                }
            }
        }
    }
}

fn mix3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)]
}

fn to_rgb8(c: [f64; 3]) -> [u8; 3] {
    [c[0].clamp(0.0, 255.0) as u8, c[1].clamp(0.0, 255.0) as u8, c[2].clamp(0.0, 255.0) as u8]
}

fn climate_ramp(t: f64, w: f64, cold_dry: [f64; 3], cold_wet: [f64; 3], hot_dry: [f64; 3], hot_wet: [f64; 3]) -> [f64; 3] {
    let tt = (t * 0.5 + 0.5).clamp(0.0, 1.0);
    let ww = (w * 0.5 + 0.5).clamp(0.0, 1.0);
    mix3(mix3(cold_dry, cold_wet, ww), mix3(hot_dry, hot_wet, ww), tt)
}

/// The colour grass takes on here.
pub fn grass_tint(temperature: f64, humidity: f64) -> [u8; 3] {
    to_rgb8(climate_ramp(
        temperature,
        humidity,
        [126.0, 162.0, 118.0],
        [86.0, 148.0, 104.0],
        [186.0, 178.0, 84.0],
        [72.0, 172.0, 62.0],
    ))
}

/// The colour leaves take on here.
pub fn foliage_tint(temperature: f64, humidity: f64) -> [u8; 3] {
    to_rgb8(climate_ramp(
        temperature,
        humidity,
        [92.0, 132.0, 96.0],
        [62.0, 118.0, 84.0],
        [150.0, 150.0, 62.0],
        [48.0, 142.0, 42.0],
    ))
}

/// The colour water takes on here.
pub fn water_tint(temperature: f64, humidity: f64) -> [u8; 3] {
    to_rgb8(climate_ramp(
        temperature,
        humidity,
        [52.0, 96.0, 168.0],
        [40.0, 88.0, 150.0],
        [46.0, 156.0, 196.0],
        [52.0, 130.0, 128.0],
    ))
}

/// How a distant column looks, for terrain too far away to have chunks.
#[derive(Clone, Copy, Debug)]
pub struct FarSample {
    /// Height of the land, or of the sea where it is under water.
    pub height: f32,
    pub color: [u8; 3],
    pub water: bool,
}

impl Generator {
    /// A far-away column, using the same shape the real terrain follows. `slope` is the steepest
    /// height change per block around it, which the caller knows from its neighbours.
    pub fn far_sample(&self, x: f64, z: f64, slope: f64) -> FarSample {
        let col = self.column(x, z);
        let biome = Self::biome(&col);
        let sea = SEA_LEVEL as f64;
        if col.height < sea {
            let t = water_tint(col.temperature, col.humidity);
            let depth = ((sea - col.height) / 28.0).clamp(0.0, 1.0);
            let shade = 1.0 - depth * 0.45;
            return FarSample {
                height: SEA_LEVEL as f32,
                color: [(t[0] as f64 * shade) as u8, (t[1] as f64 * shade) as u8, (t[2] as f64 * shade) as u8],
                water: true,
            };
        }
        let g = grass_tint(col.temperature, col.humidity);
        let f = foliage_tint(col.temperature, col.humidity);
        let wooded = |amount: f64| -> [u8; 3] {
            to_rgb8(mix3([g[0] as f64, g[1] as f64, g[2] as f64], [f[0] as f64 * 0.72, f[1] as f64 * 0.72, f[2] as f64 * 0.72], amount))
        };
        let (mut color, canopy) = match biome {
            Biome::Beach => ([222, 208, 164], 0.0),
            Biome::Desert => ([224, 206, 158], 0.0),
            Biome::Plains => (wooded(0.08), 0.0),
            Biome::Savanna => (wooded(0.14), 1.0),
            Biome::Forest | Biome::BirchForest => (wooded(0.82), 6.0),
            Biome::Rainforest => (wooded(0.95), 9.0),
            Biome::Swamp => (wooded(0.6), 3.0),
            Biome::Taiga => ([72, 104, 88], 7.0),
            Biome::Tundra => ([232, 238, 242], 0.0),
            Biome::Mountains => ([122, 122, 124], 0.0),
            Biome::SnowyPeaks => ([240, 244, 248], 0.0),
            Biome::River | Biome::Ocean | Biome::DeepOcean => ([150, 142, 116], 0.0),
        };
        if matches!(biome, Biome::Mountains | Biome::SnowyPeaks) && slope < 0.6 && Self::surface_temperature(&col) < -0.72 {
            color = [240, 244, 248];
        } else if slope > 1.1 && !matches!(biome, Biome::Beach | Biome::Desert) {
            color = [116, 116, 118];
        } else if biome == Biome::Mountains && slope < 0.55 {
            color = wooded(0.35);
        }
        FarSample { height: (col.height + canopy) as f32, color, water: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothstep_clamps_and_can_fall() {
        assert_eq!(smoothstep(0.0, 1.0, -5.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 5.0), 1.0);
        assert_eq!(smoothstep(0.0, 1.0, 0.5), 0.5);
        assert_eq!(smoothstep(1.0, 0.0, 1.5), 0.0);
        assert_eq!(smoothstep(1.0, 0.0, -0.5), 1.0);
    }

    #[test]
    fn base_height_rises_inland() {
        let mut last = f64::MIN;
        for i in -100..=100 {
            let h = base_height(i as f64 / 100.0);
            assert!(h >= last);
            last = h;
        }
        assert!(base_height(-1.0) < SEA_LEVEL as f64 && base_height(1.0) > SEA_LEVEL as f64);
    }

    #[test]
    fn the_world_has_every_kind_of_place_in_it() {
        let g = Generator::new(1);
        let mut seen = std::collections::HashSet::new();
        for x in -400..400 {
            for z in -400..400 {
                let col = g.column(x as f64 * 64.0, z as f64 * 64.0);
                seen.insert(Generator::biome(&col));
            }
        }
        for b in [
            Biome::DeepOcean,
            Biome::Ocean,
            Biome::River,
            Biome::Beach,
            Biome::Plains,
            Biome::Forest,
            Biome::BirchForest,
            Biome::Taiga,
            Biome::Tundra,
            Biome::Desert,
            Biome::Savanna,
            Biome::Rainforest,
            Biome::Swamp,
            Biome::Mountains,
            Biome::SnowyPeaks,
        ] {
            assert!(seen.contains(&b), "{b:?} never appears in 51 km x 51 km");
        }
    }

    #[test]
    fn heights_stay_inside_the_buildable_world() {
        let g = Generator::new(9);
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for x in -300..300 {
            for z in -300..300 {
                let h = g.column(x as f64 * 60.0, z as f64 * 60.0).height;
                lo = lo.min(h);
                hi = hi.max(h);
            }
        }
        assert!(lo > 0.0, "ocean floor at {lo}");
        assert!(hi < 300.0, "peak at {hi}");
        assert!(hi > 150.0, "no real mountains: the highest point is {hi}");
    }

    #[test]
    fn the_shape_of_the_land_changes_gradually() {
        let g = Generator::new(5);
        for i in 0..4000 {
            let (x, z) = (i as f64 * 23.0 - 40000.0, i as f64 * 17.0 - 30000.0);
            let d = (g.column(x, z).height - g.column(x + 1.0, z).height).abs();
            assert!(d < 12.0, "a {d} block cliff in one step at {x},{z}");
        }
    }

    #[test]
    fn tints_are_warmer_where_it_is_hot() {
        let (cold, hot) = (grass_tint(-1.0, 0.0), grass_tint(1.0, -1.0));
        assert!(hot[0] > cold[0]);
    }
}
