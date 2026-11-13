//! Turns entities, particles and the like into triangles, once a frame, on the CPU.
use crate::render::textures::{ICON, item_icon};
use crate::render::{EntityVertex, NO_TEXTURE};
use glam::{DVec3, Mat4, Vec3};
use zenith_content::{BlockState, Icon, ItemKind, ItemStack, Registry, Shape};
use zenith_core::{BlockPos, FxMap};
use zenith_protocol::EntityKind;

#[derive(Clone, Copy)]
pub struct Light {
    pub sky: u8,
    pub block: u8,
}

fn vertex(p: Vec3, uv: [f32; 2], layer: u32, color: [u8; 4], n: Vec3, glow: bool, l: Light) -> EntityVertex {
    EntityVertex {
        pos: p.to_array(),
        uv,
        layer,
        color,
        normal: [(n.x * 127.0) as i8, (n.y * 127.0) as i8, (n.z * 127.0) as i8, if glow { 127 } else { 0 }],
        light: [l.sky, l.block],
        _pad: [0, 0],
    }
}

const FACES: [([f32; 3], [[f32; 3]; 4]); 6] = [
    ([0.0, -1.0, 0.0], [[0.0, 0.0, 1.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0]]),
    ([0.0, 1.0, 0.0], [[0.0, 1.0, 0.0], [0.0, 1.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, 0.0]]),
    ([0.0, 0.0, -1.0], [[1.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
    ([0.0, 0.0, 1.0], [[0.0, 1.0, 1.0], [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0]]),
    ([-1.0, 0.0, 0.0], [[0.0, 1.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 1.0]]),
    ([1.0, 0.0, 0.0], [[1.0, 1.0, 1.0], [1.0, 0.0, 1.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]]),
];
const UVS: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];

fn push_quad(out: &mut Vec<EntityVertex>, q: [EntityVertex; 4]) {
    out.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
}

/// A box with one colour, through a transform.
pub fn colored_box(out: &mut Vec<EntityVertex>, m: Mat4, min: Vec3, max: Vec3, color: [u8; 4], light: Light) {
    let size = max - min;
    for (n, corners) in FACES {
        let normal = m.transform_vector3(Vec3::from_array(n)).normalize_or_zero();
        // A little shading between the faces gives flat colours their shape back.
        let q = corners.map(|c| {
            let p = m.transform_point3(min + Vec3::from_array(c) * size);
            vertex(p, [0.0, 0.0], NO_TEXTURE, color, normal, false, light)
        });
        push_quad(out, q);
    }
}

/// A block as a small cube with its real textures.
pub fn block_cube(
    out: &mut Vec<EntityVertex>,
    reg: &Registry,
    m: Mat4,
    state: BlockState,
    tint: [u8; 3],
    light: Light,
) {
    let kind = state.kind();
    // A block made of boxes shows them, as a fence or a slab looks; the rest is a whole cube. A fence
    // shows a run of itself.
    const WHOLE: [zenith_content::B16; 1] = [[[0, 0, 0], [16, 16, 16]]];
    let shape = reg.block(state).shape;
    let meta = if shape == Shape::Fence { 0b1100 } else { state.meta() };
    let boxes = match shape.boxes(meta) {
        [] => &WHOLE[..],
        list => list,
    };
    for b in boxes {
        let lo = Vec3::new(b[0][0] as f32, b[0][1] as f32, b[0][2] as f32) / 16.0;
        let size = Vec3::new(b[1][0] as f32, b[1][1] as f32, b[1][2] as f32) / 16.0 - lo;
        for (face, (n, corners)) in FACES.iter().enumerate() {
            let normal = m.transform_vector3(Vec3::from_array(*n)).normalize_or_zero();
            let layer = reg.face_tile(kind, face) as u32;
            let mut i = 0;
            let q = corners.map(|c| {
                let p = m.transform_point3(lo + Vec3::from_array(c) * size - Vec3::splat(0.5));
                let v = vertex(p, UVS[i], layer, [tint[0], tint[1], tint[2], 255], normal, false, light);
                i += 1;
                v
            });
            push_quad(out, q);
        }
    }
}

/// A block's picture on a thin sheet, for things that are not cubes: torches, plants, ladders.
fn block_sheet(out: &mut Vec<EntityVertex>, reg: &Registry, m: Mat4, state: BlockState, tint: [u8; 3], light: Light) {
    let layer = reg.face_tile(state.kind(), 2) as u32;
    let color = [tint[0], tint[1], tint[2], 255];
    let corners =
        [Vec3::new(-0.5, 0.5, 0.0), Vec3::new(-0.5, -0.5, 0.0), Vec3::new(0.5, -0.5, 0.0), Vec3::new(0.5, 0.5, 0.0)];
    let normal = m.transform_vector3(Vec3::Z).normalize_or_zero();
    let front: [EntityVertex; 4] =
        std::array::from_fn(|i| vertex(m.transform_point3(corners[i]), UVS[i], layer, color, normal, false, light));
    push_quad(out, front);
    // The same sheet seen from behind.
    let back: [EntityVertex; 4] = std::array::from_fn(|i| {
        vertex(m.transform_point3(corners[3 - i]), UVS[3 - i], layer, color, -normal, false, light)
    });
    push_quad(out, back);
}

/// Whether a block's item is shown as its picture, not as a cube of itself.
fn shown_flat(shape: Shape) -> bool {
    matches!(
        shape,
        Shape::Cross
            | Shape::Torch
            | Shape::Ladder
            | Shape::Flat
            | Shape::Button
            | Shape::Door
            | Shape::Trapdoor
            | Shape::Sign
            | Shape::Rail
    )
}

/// Sets a block's colour, for items shown in the world: biome tints have no biome to use there.
fn neutral_tint(reg: &Registry, state: BlockState) -> [u8; 3] {
    match reg.block(state).tint {
        zenith_content::Tint::None => [255, 255, 255],
        _ => [120, 178, 90],
    }
}

fn icon_color(icon: Icon) -> [u8; 3] {
    match icon {
        Icon::Tool(_, c)
        | Icon::Lump(c)
        | Icon::Ingot(c)
        | Icon::Gem(c)
        | Icon::Dust(c)
        | Icon::Meat(c)
        | Icon::Seeds(c)
        | Icon::Sheaf(c)
        | Icon::Armor(_, c)
        | Icon::Shield(c) => c,
        Icon::Bread => [196, 132, 62],
        Icon::Arrow => [200, 190, 170],
        Icon::Bow => [132, 96, 58],
        Icon::Feather => [236, 236, 230],
        Icon::Sinew => [214, 196, 160],
        Icon::Boat(c) => c,
        Icon::Cart => [132, 134, 140],
        Icon::Bucket(_) => [206, 138, 96],
        Icon::Rod => [132, 96, 58],
        Icon::Fish(c) => c,
        Icon::Tinderbox => [150, 152, 158],
        Icon::Stick => [132, 96, 58],
        Icon::Apple => [206, 44, 40],
        Icon::Brick => [166, 88, 70],
        Icon::Block => [200, 200, 200],
    }
}

/// How many pixels across an item's solid picture is: its icon, at the size of a block's texture
/// in the classic look.
const SOLID: usize = 16;

/// A pixel of an item's picture made solid: where it is, its colour, and which of its sides have
/// nothing next to them (1 left, 2 right, 4 up, 8 down), so only those are drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Voxel {
    x: u8,
    y: u8,
    color: [u8; 4],
    open: u8,
}

/// Items that are not blocks, each as its icon made solid, the way things are held and dropped:
/// every pixel of the picture a little block, one pixel deep. Worked out the first time each item
/// is drawn, from the same icon the inventory shows.
#[derive(Default)]
pub struct ItemShapes(FxMap<u16, Vec<Voxel>>);

impl ItemShapes {
    fn get(&mut self, reg: &Registry, item: u16) -> &[Voxel] {
        self.0.entry(item).or_insert_with(|| match reg.item(item) {
            Some(def) => solid_picture(&item_icon(def.icon, u64::from(item) + 1)),
            None => Vec::new(),
        })
    }
}

/// An icon, at `SOLID` pixels across, as little blocks: a pixel is there where most of the icon's
/// pixels under it are, in their average colour.
fn solid_picture(icon: &[[u8; 4]]) -> Vec<Voxel> {
    let scale = ICON / SOLID;
    let cell = |x: usize, y: usize| -> Option<[u8; 4]> {
        let (mut sum, mut n) = ([0u32; 3], 0u32);
        for dy in 0..scale {
            for dx in 0..scale {
                let p = icon[(y * scale + dy) * ICON + x * scale + dx];
                if p[3] >= 128 {
                    n += 1;
                    for c in 0..3 {
                        sum[c] += u32::from(p[c]);
                    }
                }
            }
        }
        (n * 2 >= (scale * scale) as u32).then(|| [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, 255])
    };
    let grid: Vec<Option<[u8; 4]>> = (0..SOLID * SOLID).map(|i| cell(i % SOLID, i / SOLID)).collect();
    let filled = |x: i32, y: i32| {
        (0..SOLID as i32).contains(&x)
            && (0..SOLID as i32).contains(&y)
            && grid[y as usize * SOLID + x as usize].is_some()
    };
    grid.iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let (x, y) = ((i % SOLID) as i32, (i / SOLID) as i32);
            let open = u8::from(!filled(x - 1, y))
                | (u8::from(!filled(x + 1, y)) << 1)
                | (u8::from(!filled(x, y - 1)) << 2)
                | (u8::from(!filled(x, y + 1)) << 3);
            c.map(|color| Voxel { x: x as u8, y: y as u8, color, open })
        })
        .collect()
}

/// Draws an item's solid picture through `m`: it fills the square from -0.5 to 0.5 across and up,
/// one pixel deep, facing +z.
fn solid_item(out: &mut Vec<EntityVertex>, m: Mat4, voxels: &[Voxel], light: Light) {
    let px = 1.0 / SOLID as f32;
    let normals = FACES.map(|(n, _)| m.transform_vector3(Vec3::from_array(n)).normalize_or_zero());
    for v in voxels {
        let min = Vec3::new(-0.5 + f32::from(v.x) * px, 0.5 - (f32::from(v.y) + 1.0) * px, -0.5 * px);
        // Down, up, back, front, left, right: back and front always, the rest where open.
        let shown = [v.open & 8 != 0, v.open & 4 != 0, true, true, v.open & 1 != 0, v.open & 2 != 0];
        for (face, (_, corners)) in FACES.iter().enumerate() {
            if shown[face] {
                let q = corners.map(|c| {
                    let p = m.transform_point3(min + Vec3::from_array(c) * px);
                    vertex(p, [0.0, 0.0], NO_TEXTURE, v.color, normals[face], false, light)
                });
                push_quad(out, q);
            }
        }
    }
}

/// An item lying on the ground, turning slowly.
pub fn item_entity(
    out: &mut Vec<EntityVertex>,
    reg: &Registry,
    shapes: &mut ItemShapes,
    rel: Vec3,
    stack: ItemStack,
    seconds: f32,
    id: u32,
    light: Light,
) {
    let spin = seconds * 1.4 + id as f32;
    let bob = (seconds * 2.2 + id as f32).sin() * 0.05 + 0.2;
    let m = Mat4::from_translation(rel + Vec3::Y * bob) * Mat4::from_rotation_y(spin);
    item_in_space(out, reg, shapes, m, stack, light);
}

fn item_in_space(
    out: &mut Vec<EntityVertex>,
    reg: &Registry,
    shapes: &mut ItemShapes,
    m: Mat4,
    stack: ItemStack,
    light: Light,
) {
    let Some(def) = reg.item(stack.item) else { return };
    if let ItemKind::Block(kind) = def.kind {
        let state = BlockState::of(kind);
        if shown_flat(reg.block(state).shape) {
            block_sheet(out, reg, m * Mat4::from_scale(Vec3::splat(0.42)), state, neutral_tint(reg, state), light);
        } else {
            block_cube(out, reg, m * Mat4::from_scale(Vec3::splat(0.28)), state, neutral_tint(reg, state), light);
        }
        return;
    }
    // Everything else is its picture, made solid.
    solid_item(
        out,
        m * Mat4::from_translation(Vec3::Y * 0.1) * Mat4::from_scale(Vec3::splat(0.4)),
        shapes.get(reg, stack.item),
        light,
    );
}

struct Part {
    pivot: [f32; 3],
    min: [f32; 3],
    max: [f32; 3],
    color: [u8; 3],
    motion: Motion,
    /// Lit up of itself, as eyes that shine in the dark.
    glow: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Motion {
    Still,
    Head,
    SwingA,
    SwingB,
    /// Held out in front, as zombies do.
    Reach,
    /// A tail, flicking from side to side.
    Wag,
}

const fn p(pivot: [f32; 3], min: [f32; 3], max: [f32; 3], color: [u8; 3], motion: Motion) -> Part {
    Part { pivot, min, max, color, motion, glow: false }
}

const fn glowing(part: Part) -> Part {
    Part { glow: true, ..part }
}

/// What sets one two-legged creature apart from another: its hair if it has any, its eyes, and
/// whether its bones show.
#[derive(Clone, Copy)]
struct Features {
    hair: Option<[u8; 3]>,
    /// The white of the eye, if it has one, and the eye itself.
    white: Option<[u8; 3]>,
    eye: [u8; 3],
    mouth: [u8; 3],
    shoes: [u8; 3],
    /// Ribs drawn on the chest.
    ribs: Option<[u8; 3]>,
}

fn darker(c: [u8; 3], by: f32) -> [u8; 3] {
    c.map(|v| (f32::from(v) * by) as u8)
}

/// Something that walks on two legs: a head with a face, a neck, a body with a belt, legs ending in
/// shoes, and arms in sleeves ending in hands.
fn humanoid(skin: [u8; 3], shirt: [u8; 3], pants: [u8; 3], arms: Motion, slim: f32, f: Features) -> Vec<Part> {
    let w = slim;
    let head = [0.0, 1.45, 0.0];
    let mut parts = vec![
        p(head, [-0.25, 0.0, -0.25], [0.25, 0.5, 0.25], skin, Motion::Head),
        p([0.0, 1.38, 0.0], [-0.09 * w, 0.0, -0.08 * w], [0.09 * w, 0.1, 0.08 * w], darker(skin, 0.9), Motion::Still),
        p([0.0, 0.7, 0.0], [-0.25 * w, 0.0, -0.13 * w], [0.25 * w, 0.75, 0.13 * w], shirt, Motion::Still),
        p(
            [0.0, 0.7, 0.0],
            [-0.255 * w, 0.0, -0.135 * w],
            [0.255 * w, 0.07, 0.135 * w],
            darker(pants, 0.6),
            Motion::Still,
        ),
    ];
    // The face: eyes under a brow, a nose, a mouth.
    for side in [-1.0f32, 1.0] {
        let x = |a: f32, b: f32| if side < 0.0 { (-b, -a) } else { (a, b) };
        if let Some(white) = f.white {
            let (x0, x1) = x(0.06, 0.17);
            parts.push(p(head, [x0, 0.23, 0.25], [x1, 0.31, 0.258], white, Motion::Head));
        }
        let (x0, x1) = x(0.08, 0.13);
        parts.push(p(head, [x0, 0.23, 0.252], [x1, 0.31, 0.264], f.eye, Motion::Head));
        let (x0, x1) = x(0.05, 0.18);
        parts.push(p(head, [x0, 0.32, 0.25], [x1, 0.345, 0.268], darker(f.hair.unwrap_or(skin), 0.7), Motion::Head));
    }
    parts.push(p(head, [-0.035, 0.15, 0.25], [0.035, 0.25, 0.29], darker(skin, 0.92), Motion::Head));
    parts.push(p(head, [-0.08, 0.075, 0.25], [0.08, 0.105, 0.26], f.mouth, Motion::Head));
    if let Some(hair) = f.hair {
        parts.push(p(head, [-0.265, 0.38, -0.265], [0.265, 0.53, 0.265], hair, Motion::Head));
        parts.push(p(head, [-0.265, 0.08, -0.265], [0.265, 0.4, -0.17], hair, Motion::Head));
        for x in [-0.265f32, 0.215] {
            parts.push(p(head, [x, 0.2, -0.2], [x + 0.05, 0.4, 0.06], hair, Motion::Head));
        }
    }
    if let Some(bone) = f.ribs {
        for k in 0..4 {
            let y = 0.28 + k as f32 * 0.11;
            parts.push(p(
                [0.0, 0.7, 0.0],
                [-0.2 * w, y, 0.13 * w],
                [0.2 * w, y + 0.045, 0.14 * w],
                bone,
                Motion::Still,
            ));
        }
    }
    // Legs, and what is on their feet.
    for (x, motion) in [(-0.125f32, Motion::SwingA), (0.125, Motion::SwingB)] {
        let hip = [x * w, 0.72, 0.0];
        parts.push(p(hip, [-0.11 * w, -0.72, -0.12 * w], [0.11 * w, 0.0, 0.12 * w], pants, motion));
        parts.push(p(hip, [-0.12 * w, -0.72, -0.13 * w], [0.12 * w, -0.6, 0.2 * w], f.shoes, motion));
    }
    // Arms: a sleeve to the elbow, bare below it, and a hand.
    let (left, right) =
        if arms == Motion::Reach { (Motion::Reach, Motion::Reach) } else { (Motion::SwingB, Motion::SwingA) };
    for (x, motion) in [(-0.36f32, left), (0.36, right)] {
        let shoulder = [x * w, 1.38, 0.0];
        parts.push(p(shoulder, [-0.1 * w, -0.7, -0.12 * w], [0.1 * w, 0.05, 0.12 * w], skin, motion));
        parts.push(p(shoulder, [-0.112 * w, -0.34, -0.132 * w], [0.112 * w, 0.07, 0.132 * w], shirt, motion));
        parts.push(p(
            shoulder,
            [-0.105 * w, -0.72, -0.125 * w],
            [0.105 * w, -0.58, 0.125 * w],
            darker(skin, 0.9),
            motion,
        ));
    }
    parts
}

/// Something that walks on four legs: a body, a head with eyes and ears, legs ending in hooves,
/// and a tail.
fn quadruped(body: [u8; 3], head: [u8; 3], legs: [u8; 3], width: f32) -> Vec<Part> {
    let w = width;
    let neck = [0.0, 0.62, 0.42];
    let mut v = vec![
        p([0.0, 0.45, 0.0], [-0.3 * w, 0.0, -0.45], [0.3 * w, 0.42, 0.45], body, Motion::Still),
        p(neck, [-0.2, -0.16, 0.0], [0.2, 0.2, 0.34], head, Motion::Head),
    ];
    for (x, z, motion) in [
        (-0.2f32, 0.32f32, Motion::SwingA),
        (0.2, 0.32, Motion::SwingB),
        (-0.2, -0.32, Motion::SwingB),
        (0.2, -0.32, Motion::SwingA),
    ] {
        let hip = [x * w, 0.45, z];
        v.push(p(hip, [-0.08, -0.45, -0.08], [0.08, 0.0, 0.08], legs, motion));
        v.push(p(hip, [-0.088, -0.45, -0.088], [0.088, -0.36, 0.088], darker(legs, 0.45), motion));
    }
    for side in [-1.0f32, 1.0] {
        // An eye on each side of the face, and an ear above it.
        let (x0, x1) = if side < 0.0 { (-0.2, -0.13) } else { (0.13, 0.2) };
        v.push(p(neck, [x0, 0.04, 0.3], [x1, 0.11, 0.345], [24, 22, 22], Motion::Head));
        let (x0, x1) = if side < 0.0 { (-0.27, -0.19) } else { (0.19, 0.27) };
        v.push(p(neck, [x0, 0.12, 0.04], [x1, 0.22, 0.14], darker(head, 0.85), Motion::Head));
    }
    v.push(p([0.0, 0.8, -0.45], [-0.04, -0.2, -0.05], [0.04, 0.0, 0.0], darker(body, 0.85), Motion::Wag));
    v
}

fn parts_of(kind: EntityKind) -> Vec<Part> {
    let living = Features {
        hair: Some([74, 50, 32]),
        white: Some([244, 244, 240]),
        eye: [52, 84, 150],
        mouth: [150, 84, 80],
        shoes: [58, 44, 34],
        ribs: None,
    };
    match kind {
        EntityKind::Player => humanoid([214, 170, 138], [58, 112, 176], [46, 52, 92], Motion::SwingA, 1.0, living),
        // The dead have no shine in the eye, and what hair they had is going.
        EntityKind::Zombie => {
            let dead = Features {
                hair: None,
                white: None,
                eye: [18, 26, 18],
                mouth: [44, 66, 44],
                shoes: [44, 48, 62],
                ribs: None,
            };
            let mut v = humanoid([98, 148, 92], [48, 118, 124], [60, 62, 112], Motion::Reach, 1.0, dead);
            // Rags: a tear in the shirt, with the skin showing through.
            v.push(p([0.0, 0.7, 0.0], [0.04, 0.12, 0.13], [0.2, 0.3, 0.138], [86, 132, 82], Motion::Still));
            v
        }
        EntityKind::Skeleton => {
            let bone = Features {
                hair: None,
                white: None,
                eye: [20, 20, 22],
                mouth: [60, 60, 58],
                shoes: [176, 176, 168],
                ribs: Some([92, 92, 88]),
            };
            humanoid([204, 204, 196], [186, 186, 178], [176, 176, 168], Motion::SwingA, 0.62, bone)
        }
        EntityKind::Sheep => {
            let mut v = quadruped([234, 232, 226], [96, 82, 72], [84, 72, 64], 1.05);
            // Its fleece stands proud of its legs, and sits in a tuft on its head.
            v[0].min = [-0.36, -0.04, -0.5];
            v[0].max = [0.36, 0.48, 0.5];
            v.push(p([0.0, 0.62, 0.42], [-0.17, 0.16, 0.0], [0.17, 0.27, 0.22], [234, 232, 226], Motion::Head));
            v
        }
        EntityKind::Pig => {
            let mut v = quadruped([236, 164, 164], [240, 172, 172], [214, 142, 142], 1.0);
            let snout = [0.0, 0.62, 0.42];
            v.push(p(snout, [-0.09, -0.1, 0.33], [0.09, 0.03, 0.4], [214, 130, 136], Motion::Head));
            for x in [-0.06f32, 0.025] {
                v.push(p(snout, [x, -0.06, 0.4], [x + 0.035, -0.02, 0.405], [150, 84, 92], Motion::Head));
            }
            v
        }
        EntityKind::Chicken => {
            let (white, yellow, head) = ([240, 238, 232], [226, 170, 52], [0.0, 0.5, 0.16]);
            let mut v = vec![
                p([0.0, 0.28, 0.0], [-0.14, 0.0, -0.2], [0.14, 0.28, 0.2], white, Motion::Still),
                p(head, [-0.09, 0.0, -0.08], [0.09, 0.2, 0.08], [244, 242, 236], Motion::Head),
                p(head, [-0.05, 0.05, 0.08], [0.05, 0.11, 0.17], [236, 178, 54], Motion::Head),
                p(head, [-0.03, 0.2, -0.04], [0.03, 0.27, 0.05], [204, 42, 40], Motion::Head),
                p(head, [-0.03, -0.03, 0.08], [0.03, 0.05, 0.12], [204, 42, 40], Motion::Head),
                // Tail feathers, standing up behind.
                p([0.0, 0.5, -0.2], [-0.08, -0.06, -0.08], [0.08, 0.14, 0.0], [226, 224, 216], Motion::Wag),
                p([-0.08, 0.28, 0.0], [-0.03, -0.28, -0.03], [0.03, 0.0, 0.03], yellow, Motion::SwingA),
                p([0.08, 0.28, 0.0], [-0.03, -0.28, -0.03], [0.03, 0.0, 0.03], yellow, Motion::SwingB),
                p([-0.08, 0.28, 0.0], [-0.06, -0.28, -0.03], [0.06, -0.25, 0.09], yellow, Motion::SwingA),
                p([0.08, 0.28, 0.0], [-0.06, -0.28, -0.03], [0.06, -0.25, 0.09], yellow, Motion::SwingB),
            ];
            for side in [-1.0f32, 1.0] {
                // A wing folded along each side, and an eye.
                let (x0, x1) = if side < 0.0 { (-0.17, -0.14) } else { (0.14, 0.17) };
                v.push(p([0.0, 0.28, 0.0], [x0, 0.06, -0.14], [x1, 0.24, 0.12], [222, 220, 212], Motion::Still));
                let (x0, x1) = if side < 0.0 { (-0.095, -0.085) } else { (0.085, 0.095) };
                v.push(p(head, [x0, 0.11, 0.02], [x1, 0.15, 0.06], [20, 20, 22], Motion::Head));
            }
            v
        }
        EntityKind::Gloomcrawler => {
            let (shell, dark, eye) = ([38, 46, 66], [22, 24, 32], [255, 178, 52]);
            let mut v = vec![
                // A broad shell low over the ground, ridged along its back, its head before it, and
                // two eyes that shine.
                p([0.0, 0.2, 0.0], [-0.42, 0.0, -0.42], [0.42, 0.32, 0.4], shell, Motion::Still),
                p([0.0, 0.3, 0.0], [-0.3, 0.0, -0.3], [0.3, 0.08, 0.28], [52, 64, 92], Motion::Still),
                p([0.0, 0.26, 0.4], [-0.2, -0.1, 0.0], [0.2, 0.12, 0.22], dark, Motion::Head),
                glowing(p([0.0, 0.26, 0.4], [-0.15, 0.0, 0.2], [-0.06, 0.06, 0.24], eye, Motion::Head)),
                glowing(p([0.0, 0.26, 0.4], [0.06, 0.0, 0.2], [0.15, 0.06, 0.24], eye, Motion::Head)),
            ];
            for z in [-0.28f32, -0.1, 0.08] {
                v.push(p([0.0, 0.38, 0.0], [-0.22, 0.0, z], [0.22, 0.035, z + 0.07], [70, 84, 116], Motion::Still));
            }
            // Jaws that close from the sides.
            for x in [-0.14f32, 0.08] {
                v.push(p([0.0, 0.26, 0.4], [x, -0.1, 0.2], [x + 0.06, -0.04, 0.32], [60, 62, 74], Motion::Head));
            }
            // Three legs a side, reaching out and down, each pair stepping against the next.
            for (i, z) in [-0.26f32, 0.0, 0.26].into_iter().enumerate() {
                let (a, b) =
                    if i % 2 == 0 { (Motion::SwingA, Motion::SwingB) } else { (Motion::SwingB, Motion::SwingA) };
                v.push(p([-0.4, 0.22, z], [-0.3, -0.22, -0.04], [0.0, 0.03, 0.04], dark, a));
                v.push(p([0.4, 0.22, z], [0.0, -0.22, -0.04], [0.3, 0.03, 0.04], dark, b));
            }
            v
        }
        EntityKind::Lurker => {
            let (skin, dim) = ([200, 196, 188], [150, 146, 140]);
            let head = [0.0, 1.62, 0.1];
            let mut v = vec![
                // A long head with no eyes, hung forward, and a mouth too wide for it.
                p(head, [-0.19, -0.04, -0.18], [0.19, 0.38, 0.24], skin, Motion::Head),
                p(head, [-0.15, 0.02, 0.24], [0.15, 0.07, 0.25], [48, 30, 34], Motion::Head),
                p([0.0, 0.88, 0.0], [-0.19, 0.0, -0.1], [0.19, 0.78, 0.1], dim, Motion::Still),
                p([-0.1, 0.88, 0.0], [-0.07, -0.88, -0.07], [0.07, 0.0, 0.07], dim, Motion::SwingA),
                p([0.1, 0.88, 0.0], [-0.07, -0.88, -0.07], [0.07, 0.0, 0.07], dim, Motion::SwingB),
                // Arms that reach past its knees, ending in long hands.
                p([-0.27, 1.6, 0.0], [-0.06, -1.05, -0.06], [0.06, 0.04, 0.06], skin, Motion::SwingB),
                p([0.27, 1.6, 0.0], [-0.06, -1.05, -0.06], [0.06, 0.04, 0.06], skin, Motion::SwingA),
                p([-0.27, 1.6, 0.0], [-0.075, -1.2, -0.03], [0.075, -1.03, 0.05], [176, 172, 164], Motion::SwingB),
                p([0.27, 1.6, 0.0], [-0.075, -1.2, -0.03], [0.075, -1.03, 0.05], [176, 172, 164], Motion::SwingA),
            ];
            // Ribs showing through its sides, and the knobs of its spine.
            for k in 0..4 {
                let y = 0.3 + k as f32 * 0.11;
                v.push(p([0.0, 0.88, 0.0], [-0.16, y, 0.1], [0.16, y + 0.04, 0.108], [120, 116, 112], Motion::Still));
                v.push(p([0.0, 0.88, 0.0], [-0.03, y, -0.13], [0.03, y + 0.06, -0.1], skin, Motion::Still));
            }
            v
        }
        _ => Vec::new(),
    }
}

/// A box with its edges and corners taken off: six faces drawn in from their edges, a strip along
/// each edge between them, and a triangle at each corner, each lit by the way it faces. Bodies are
/// not made of planks.
fn soft_box(out: &mut Vec<EntityVertex>, m: Mat4, min: Vec3, max: Vec3, color: [u8; 4], light: Light) {
    let size = max - min;
    let r = (size.min_element() * 0.28).min(0.07);
    if r < 0.01 {
        colored_box(out, m, min, max, color, light);
        return;
    }
    let (centre, half) = ((min + max) * 0.5, size * 0.5);
    // The corner at `s` (each part 1 or -1), pulled back from every side but the one facing `keep`.
    let at = |s: Vec3, keep: Vec3| centre + s * (half - (Vec3::ONE - keep) * r);
    let mut face = |points: &[Vec3], outward: Vec3| {
        let normal = m.transform_vector3(outward).normalize_or_zero();
        let v = |p: Vec3| vertex(m.transform_point3(p), [0.0, 0.0], NO_TEXTURE, color, normal, false, light);
        // Wound to face outward, whichever way round the points came.
        let flip = (points[1] - points[0]).cross(points[2] - points[0]).dot(outward) < 0.0;
        for tri in [[0, 1, 2], [0, 2, 3]] {
            if tri.iter().any(|&k| k >= points.len()) {
                continue;
            }
            let [a, b, c] = if flip { [tri[0], tri[2], tri[1]] } else { tri };
            out.extend_from_slice(&[v(points[a]), v(points[b]), v(points[c])]);
        }
    };
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    for (i, &a) in axes.iter().enumerate() {
        let (b, c) = (axes[(i + 1) % 3], axes[(i + 2) % 3]);
        for sa in [-1.0f32, 1.0] {
            // A face, across the two other ways.
            face(&[at(a * sa - b - c, a), at(a * sa + b - c, a), at(a * sa + b + c, a), at(a * sa - b + c, a)], a * sa);
            // The strip between this face and the next way round, along the third.
            for sb in [-1.0f32, 1.0] {
                let (lo, hi) = (a * sa + b * sb - c, a * sa + b * sb + c);
                face(&[at(lo, a), at(hi, a), at(hi, b), at(lo, b)], a * sa + b * sb);
            }
        }
    }
    for sx in [-1.0f32, 1.0] {
        for sy in [-1.0f32, 1.0] {
            for sz in [-1.0f32, 1.0] {
                let s = Vec3::new(sx, sy, sz);
                face(&[at(s, Vec3::X), at(s, Vec3::Y), at(s, Vec3::Z)], s);
            }
        }
    }
}

/// A mob or player, walking by `phase` with its feet at `rel`, at `scale` of its grown size.
pub fn creature(
    out: &mut Vec<EntityVertex>,
    kind: EntityKind,
    rel: Vec3,
    yaw_turns: f32,
    pitch_turns: f32,
    phase: f32,
    speed: f32,
    scale: f32,
    time: f32,
    light: Light,
) {
    // Models are built facing +z, which is where a yaw of zero looks.
    let base = Mat4::from_translation(rel)
        * Mat4::from_rotation_y(-yaw_turns * std::f32::consts::TAU)
        * Mat4::from_scale(Vec3::splat(scale));
    let swing = phase.sin() * speed.clamp(0.0, 1.0) * 0.75;
    // Standing still is not standing stock still: it breathes, looks about, and shifts its weight.
    let rest = 1.0 - speed.clamp(0.0, 1.0);
    let base = base * Mat4::from_scale(Vec3::new(1.0, 1.0 + (time * 1.7).sin() * 0.012, 1.0));
    let glance = ((time * 0.43).sin() + (time * 0.17).sin()) * 0.11 * rest;
    let sway = (time * 1.1).sin() * 0.035 * rest;
    for part in parts_of(kind) {
        let rot = match part.motion {
            Motion::Still => Mat4::IDENTITY,
            Motion::Head => {
                Mat4::from_rotation_y(glance) * Mat4::from_rotation_x(pitch_turns * std::f32::consts::TAU + sway * 0.5)
            }
            Motion::SwingA => Mat4::from_rotation_x(swing + sway),
            Motion::SwingB => Mat4::from_rotation_x(-swing - sway),
            Motion::Reach => Mat4::from_rotation_x(-1.45 + swing * 0.15 + sway),
            Motion::Wag => Mat4::from_rotation_z((time * 5.3).sin() * 0.35) * Mat4::from_rotation_x(0.25),
        };
        let m = base * Mat4::from_translation(Vec3::from_array(part.pivot)) * rot;
        let c = part.color;
        let lit = if part.glow { Light { sky: 255, block: 255 } } else { light };
        soft_box(out, m, Vec3::from_array(part.min), Vec3::from_array(part.max), [c[0], c[1], c[2], 255], lit);
    }
}

/// A rowing boat, its bow along `yaw`: a plank hull, its gunwales, and a seat across it.
pub fn boat(out: &mut Vec<EntityVertex>, rel: Vec3, yaw_turns: f32, light: Light) {
    let m = Mat4::from_translation(rel) * Mat4::from_rotation_y(-yaw_turns * std::f32::consts::TAU);
    let (hull, rim, seat) = ([150, 112, 68, 255], [118, 86, 50, 255], [104, 76, 44, 255]);
    colored_box(out, m, Vec3::new(-0.5, 0.0, -0.8), Vec3::new(0.5, 0.08, 0.8), hull, light);
    for side in [-1.0f32, 1.0] {
        let x = side * 0.46;
        colored_box(out, m, Vec3::new(x - 0.05, 0.08, -0.8), Vec3::new(x + 0.05, 0.42, 0.8), hull, light);
        colored_box(out, m, Vec3::new(x - 0.06, 0.42, -0.8), Vec3::new(x + 0.06, 0.47, 0.8), rim, light);
        let z = side * 0.76;
        colored_box(out, m, Vec3::new(-0.41, 0.08, z - 0.04), Vec3::new(0.41, 0.42, z + 0.04), hull, light);
    }
    colored_box(out, m, Vec3::new(-0.41, 0.22, -0.12), Vec3::new(0.41, 0.28, 0.12), seat, light);
}

/// A cart: an iron tub on four wheels, tipped up or down a slope.
pub fn cart(out: &mut Vec<EntityVertex>, rel: Vec3, yaw_turns: f32, pitch_turns: f32, light: Light) {
    let m = Mat4::from_translation(rel)
        * Mat4::from_rotation_y(-yaw_turns * std::f32::consts::TAU)
        * Mat4::from_rotation_x(pitch_turns * std::f32::consts::TAU);
    let (iron, dark, wheel) = ([132, 134, 140, 255], [86, 88, 94, 255], [40, 40, 44, 255]);
    colored_box(out, m, Vec3::new(-0.42, 0.14, -0.48), Vec3::new(0.42, 0.2, 0.48), dark, light);
    for side in [-1.0f32, 1.0] {
        colored_box(
            out,
            m,
            Vec3::new(side * 0.42 - 0.04, 0.14, -0.48),
            Vec3::new(side * 0.42 + 0.04, 0.62, 0.48),
            iron,
            light,
        );
        colored_box(
            out,
            m,
            Vec3::new(-0.42, 0.14, side * 0.48 - 0.04),
            Vec3::new(0.42, 0.62, side * 0.48 + 0.04),
            iron,
            light,
        );
        for end in [-1.0f32, 1.0] {
            let (x, z) = (side * 0.44, end * 0.3);
            colored_box(out, m, Vec3::new(x - 0.04, 0.0, z - 0.1), Vec3::new(x + 0.04, 0.2, z + 0.1), wheel, light);
        }
    }
}

/// A name floating over a head is drawn by the UI; this is where it should go.
pub const NAME_HEIGHT: f32 = 2.1;

pub fn arrow(out: &mut Vec<EntityVertex>, rel: Vec3, yaw_turns: f32, pitch_turns: f32, light: Light) {
    let m = Mat4::from_translation(rel)
        * Mat4::from_rotation_y(-yaw_turns * std::f32::consts::TAU)
        * Mat4::from_rotation_x(pitch_turns * std::f32::consts::TAU);
    colored_box(out, m, Vec3::new(-0.02, -0.02, -0.35), Vec3::new(0.02, 0.02, 0.3), [150, 118, 78, 255], light);
    colored_box(out, m, Vec3::new(-0.05, -0.05, 0.27), Vec3::new(0.05, 0.05, 0.36), [196, 200, 206, 255], light);
    colored_box(out, m, Vec3::new(-0.06, -0.015, -0.36), Vec3::new(0.06, 0.015, -0.22), [232, 228, 216, 255], light);
}

/// A fishing float: white below the water line, red above, on a little stem.
pub fn float(out: &mut Vec<EntityVertex>, rel: Vec3, light: Light) {
    let m = Mat4::from_translation(rel);
    colored_box(out, m, Vec3::new(-0.06, 0.0, -0.06), Vec3::new(0.06, 0.08, 0.06), [238, 236, 228, 255], light);
    colored_box(out, m, Vec3::new(-0.06, 0.08, -0.06), Vec3::new(0.06, 0.16, 0.06), [208, 44, 40, 255], light);
    colored_box(out, m, Vec3::new(-0.012, 0.16, -0.012), Vec3::new(0.012, 0.26, 0.012), [60, 60, 64, 255], light);
}

/// A thin line between two points, both given from the eye: a ribbon turned to face it. It hangs
/// a little in the middle, as a slack line does.
pub fn line(out: &mut Vec<EntityVertex>, a: Vec3, b: Vec3, color: [u8; 4], light: Light) {
    const PIECES: usize = 8;
    let sag = (a.distance(b) * 0.06).min(0.6);
    let at = |i: usize| {
        let t = i as f32 / PIECES as f32;
        a.lerp(b, t) - Vec3::Y * sag * 4.0 * t * (1.0 - t)
    };
    for i in 0..PIECES {
        let (p, q) = (at(i), at(i + 1));
        // As wide as a thread looks at each end's distance: about a pixel and a half.
        let across = (q - p).cross((p + q) * 0.5).normalize_or_zero();
        let (sp, sq) = (across * (0.002 + p.length() * 0.0011), across * (0.002 + q.length() * 0.0011));
        let v = |p: Vec3| vertex(p, [0.0, 0.0], NO_TEXTURE, color, Vec3::Y, false, light);
        // Both ways round, to be seen from either side.
        push_quad(out, [v(p - sp), v(p + sp), v(q + sq), v(q - sq)]);
        push_quad(out, [v(q - sq), v(q + sq), v(p + sp), v(p - sp)]);
    }
}

/// A falling block or a lit charge: a whole block moving through the world.
pub fn whole_block(
    out: &mut Vec<EntityVertex>,
    reg: &Registry,
    rel: Vec3,
    state: BlockState,
    scale: f32,
    light: Light,
) {
    let m = Mat4::from_translation(rel + Vec3::Y * 0.5) * Mat4::from_scale(Vec3::splat(scale));
    block_cube(out, reg, m, state, neutral_tint(reg, state), light);
}

/// The dark line around the block under the crosshair.
pub fn outline(out: &mut Vec<EntityVertex>, reg: &Registry, rel_origin: Vec3, state: BlockState) {
    let def = reg.block(state);
    for b in def.shape.outline(state.meta()) {
        let e = 0.004;
        let (lo, hi) = (
            rel_origin + Vec3::new(b[0][0] as f32, b[0][1] as f32, b[0][2] as f32) / 16.0 - Vec3::splat(e),
            rel_origin + Vec3::new(b[1][0] as f32, b[1][1] as f32, b[1][2] as f32) / 16.0 + Vec3::splat(e),
        );
        wire_box(out, lo, hi, 0.008, [8, 8, 10, 170]);
    }
}

/// A line in the world: a thin unlit bar from `a` to `b`, reaching `t` out from it each way.
pub fn bar(out: &mut Vec<EntityVertex>, a: Vec3, b: Vec3, t: f32, color: [u8; 4]) {
    let glow = Light { sky: 255, block: 255 };
    let (min, max) = (a.min(b) - Vec3::splat(t), a.max(b) + Vec3::splat(t));
    let size = max - min;
    for (n, corners) in FACES {
        let q = corners.map(|c| {
            vertex(min + Vec3::from_array(c) * size, [0.0, 0.0], NO_TEXTURE, color, Vec3::from_array(n), true, glow)
        });
        push_quad(out, q);
    }
}

/// The twelve edges of a box, as lines.
pub fn wire_box(out: &mut Vec<EntityVertex>, lo: Vec3, hi: Vec3, t: f32, color: [u8; 4]) {
    let corner = |x: bool, y: bool, z: bool| {
        Vec3::new(if x { hi.x } else { lo.x }, if y { hi.y } else { lo.y }, if z { hi.z } else { lo.z })
    };
    for (a, b) in [false, true].into_iter().flat_map(|p| [false, true].map(|q| (p, q))) {
        bar(out, corner(false, a, b), corner(true, a, b), t, color);
        bar(out, corner(a, false, b), corner(a, true, b), t, color);
        bar(out, corner(a, b, false), corner(a, b, true), t, color);
    }
}

/// Cracks on a block being broken.
pub fn cracks(out: &mut Vec<EntityVertex>, rel_origin: Vec3, layer: u32) {
    let glow = Light { sky: 255, block: 255 };
    let e = 0.003;
    let size = Vec3::splat(1.0 + e * 2.0);
    let min = rel_origin - Vec3::splat(e);
    for (n, corners) in FACES {
        let mut i = 0;
        let q = corners.map(|c| {
            let v = vertex(
                min + Vec3::from_array(c) * size,
                UVS[i],
                layer,
                [255, 255, 255, 255],
                Vec3::from_array(n),
                true,
                glow,
            );
            i += 1;
            v
        });
        push_quad(out, q);
    }
}

/// A little square of something, always facing the camera.
#[derive(Clone, Copy, Debug)]
pub struct Particle {
    pub pos: DVec3,
    pub vel: DVec3,
    pub age: f32,
    pub life: f32,
    pub size: f32,
    pub color: [u8; 4],
    pub layer: u32,
    pub uv: [f32; 2],
    pub gravity: f32,
    pub glow: bool,
}

impl Particle {
    /// Moves it along. Returns whether it is still alive.
    pub fn update(&mut self, dt: f32, solid: impl Fn(BlockPos) -> bool) -> bool {
        self.age += dt;
        self.vel.y -= (self.gravity * dt) as f64;
        self.vel *= (1.0 - dt as f64 * 0.8).max(0.0);
        let next = self.pos + self.vel * dt as f64;
        if solid(BlockPos::from_vec(next)) {
            self.vel = DVec3::ZERO;
        } else {
            self.pos = next;
        }
        self.age < self.life
    }
}

pub fn draw_particle(out: &mut Vec<EntityVertex>, p: &Particle, cam: DVec3, right: Vec3, up: Vec3, light: Light) {
    let rel = (p.pos - cam).as_vec3();
    let fade = (1.0 - p.age / p.life).clamp(0.0, 1.0);
    let s = p.size * if p.glow { fade } else { fade.sqrt() };
    let (r, u) = (right * s, up * s);
    let mut c = p.color;
    c[3] = (c[3] as f32 * fade.min(1.0)) as u8;
    let n = (r.cross(u)).normalize_or_zero();
    let span = if p.layer == NO_TEXTURE { 0.0 } else { 0.18 };
    let [u0, v0] = p.uv;
    let q = [
        vertex(rel - r + u, [u0, v0], p.layer, c, n, true, light),
        vertex(rel - r - u, [u0, v0 + span], p.layer, c, n, true, light),
        vertex(rel + r - u, [u0 + span, v0 + span], p.layer, c, n, true, light),
        vertex(rel + r + u, [u0 + span, v0], p.layer, c, n, true, light),
    ];
    push_quad(out, q);
}

/// A streak of rain or a flake of snow, from a vertical quad turned to the camera.
pub fn streak(out: &mut Vec<EntityVertex>, rel: Vec3, right: Vec3, height: f32, width: f32, color: [u8; 4]) {
    let glow = Light { sky: 255, block: 0 };
    let side = Vec3::new(right.x, 0.0, right.z).normalize_or_zero() * width;
    let n = Vec3::Y;
    let q = [
        vertex(rel - side + Vec3::Y * height, [0.0, 0.0], NO_TEXTURE, color, n, true, glow),
        vertex(rel - side, [0.0, 0.0], NO_TEXTURE, color, n, true, glow),
        vertex(rel + side, [0.0, 0.0], NO_TEXTURE, color, n, true, glow),
        vertex(rel + side + Vec3::Y * height, [0.0, 0.0], NO_TEXTURE, color, n, true, glow),
    ];
    push_quad(out, q);
}

/// The first-person arm, and what it holds, in view space.
pub fn view_model(
    out: &mut Vec<EntityVertex>,
    reg: &Registry,
    shapes: &mut ItemShapes,
    held: Option<ItemStack>,
    swing: f32,
    bob: f32,
    drawn: f32,
    guarding: bool,
    light: Light,
) {
    let lift = (swing * std::f32::consts::PI).sin();
    let turn = (swing.sqrt() * std::f32::consts::PI).sin();
    let base = Mat4::from_translation(Vec3::new(
        0.66 - turn * 0.25,
        -0.58 + bob.sin().abs() * -0.018 - lift * 0.1,
        -0.92 + bob.cos() * 0.01,
    )) * Mat4::from_rotation_x(-0.25 - lift * 1.0)
        * Mat4::from_rotation_y(-0.3 - turn * 0.5)
        * Mat4::from_scale(Vec3::splat(0.82));
    let arm = |out: &mut Vec<EntityVertex>, m: Mat4, end: f32| {
        // A hand, and a sleeve running off the edge of the screen.
        colored_box(
            out,
            m,
            Vec3::new(-0.075, end - 0.2, -0.075),
            Vec3::new(0.075, end, 0.075),
            [214, 170, 138, 255],
            light,
        );
        colored_box(
            out,
            m,
            Vec3::new(-0.088, -0.72, -0.088),
            Vec3::new(0.088, end - 0.2, 0.088),
            [58, 74, 104, 255],
            light,
        );
    };
    match held {
        None => arm(out, base * Mat4::from_rotation_x(-1.0), 0.2),
        Some(stack) => {
            let Some(def) = reg.item(stack.item) else { return };
            arm(out, base * Mat4::from_translation(Vec3::new(0.1, -0.25, 0.25)) * Mat4::from_rotation_x(-1.0), 0.15);
            let hand = base * Mat4::from_translation(Vec3::new(0.0, 0.0, -0.1));
            match def.kind {
                ItemKind::Block(kind) => {
                    let state = BlockState::of(kind);
                    if shown_flat(reg.block(state).shape) {
                        let sheet = hand
                            * Mat4::from_translation(Vec3::Y * 0.1)
                            * Mat4::from_rotation_y(0.5)
                            * Mat4::from_scale(Vec3::splat(0.5));
                        block_sheet(out, reg, sheet, state, neutral_tint(reg, state), light);
                    } else {
                        block_cube(
                            out,
                            reg,
                            hand * Mat4::from_rotation_y(0.5) * Mat4::from_scale(Vec3::splat(0.28)),
                            state,
                            neutral_tint(reg, state),
                            light,
                        );
                    }
                }
                // A bow upright, its string pulled back the further it is drawn, an arrow on it.
                ItemKind::Bow { .. } => {
                    let m = hand * Mat4::from_translation(Vec3::new(-0.08, 0.05, 0.1 - drawn * 0.12));
                    let wood = [120, 86, 50, 255];
                    colored_box(out, m, Vec3::new(-0.03, -0.08, -0.03), Vec3::new(0.03, 0.08, 0.03), wood, light);
                    for sign in [1.0f32, -1.0] {
                        let a = Vec3::new(-0.025, 0.08 * sign, -0.02);
                        let tip = Vec3::new(0.025, 0.36 * sign, 0.09);
                        colored_box(out, m, a.min(tip), a.max(tip) - Vec3::new(0.0, 0.0, 0.05), wood, light);
                        let b = Vec3::new(-0.02, 0.2 * sign, 0.04);
                        colored_box(out, m, b.min(tip), b.max(tip), wood, light);
                    }
                    let pulled = 0.1 + drawn * 0.16;
                    let string = [230, 226, 214, 255];
                    colored_box(
                        out,
                        m,
                        Vec3::new(-0.006, -0.36, pulled - 0.006),
                        Vec3::new(0.006, 0.36, pulled + 0.006),
                        string,
                        light,
                    );
                    if drawn > 0.0 {
                        let shaft = [168, 132, 86, 255];
                        colored_box(
                            out,
                            m,
                            Vec3::new(-0.012, -0.012, -0.32),
                            Vec3::new(0.012, 0.012, pulled),
                            shaft,
                            light,
                        );
                        colored_box(
                            out,
                            m,
                            Vec3::new(-0.03, -0.03, -0.38),
                            Vec3::new(0.03, 0.03, -0.31),
                            [196, 200, 206, 255],
                            light,
                        );
                    }
                }
                // A shield at the side, or up in front when raised.
                ItemKind::Shield { .. } => {
                    let at = if guarding { Vec3::new(-0.42, 0.2, -0.05) } else { Vec3::new(0.0, -0.05, 0.05) };
                    let m =
                        hand * Mat4::from_translation(at) * Mat4::from_rotation_y(if guarding { 0.35 } else { 0.9 });
                    let c = icon_color(def.icon);
                    colored_box(
                        out,
                        m,
                        Vec3::new(-0.24, -0.3, -0.03),
                        Vec3::new(0.24, 0.3, 0.03),
                        [c[0], c[1], c[2], 255],
                        light,
                    );
                    colored_box(
                        out,
                        m,
                        Vec3::new(-0.26, -0.32, -0.02),
                        Vec3::new(0.26, 0.32, 0.02),
                        [92, 70, 44, 255],
                        light,
                    );
                    colored_box(
                        out,
                        m,
                        Vec3::new(-0.06, -0.06, -0.06),
                        Vec3::new(0.06, 0.06, -0.02),
                        [196, 198, 204, 255],
                        light,
                    );
                }
                // A tool or weapon is gripped by its handle, its head up and away, edge on to the eye.
                ItemKind::Tool { .. } | ItemKind::Rod { .. } | ItemKind::Lighter { .. } => {
                    let m = hand
                        * Mat4::from_translation(Vec3::new(0.0, 0.2, -0.08))
                        * Mat4::from_rotation_y(2.2)
                        * Mat4::from_rotation_z(0.2)
                        * Mat4::from_scale(Vec3::splat(0.62));
                    solid_item(out, m, shapes.get(reg, stack.item), light);
                }
                // Anything else is held up flat, facing the eye a little.
                _ => {
                    let m = hand
                        * Mat4::from_translation(Vec3::new(-0.02, 0.1, 0.0))
                        * Mat4::from_rotation_y(0.7)
                        * Mat4::from_scale(Vec3::splat(0.42));
                    solid_item(out, m, shapes.get(reg, stack.item), light);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIT: Light = Light { sky: 255, block: 0 };

    #[test]
    fn every_creature_has_a_body_that_stands_on_its_feet() {
        for kind in [
            EntityKind::Player,
            EntityKind::Zombie,
            EntityKind::Skeleton,
            EntityKind::Sheep,
            EntityKind::Pig,
            EntityKind::Chicken,
            EntityKind::Gloomcrawler,
            EntityKind::Lurker,
        ] {
            let mut out = Vec::new();
            creature(&mut out, kind, Vec3::ZERO, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, LIT);
            assert!(!out.is_empty(), "{kind:?}");
            let lowest = out.iter().map(|v| v.pos[1]).fold(f32::MAX, f32::min);
            let highest = out.iter().map(|v| v.pos[1]).fold(f32::MIN, f32::max);
            let (_, h) = kind.size();
            assert!(lowest.abs() < 0.06, "{kind:?} floats or sinks: lowest at {lowest}");
            assert!(highest < h as f32 + 0.25, "{kind:?} is {highest} tall against a box of {h}");
            // Young, it is smaller, and still stands on the ground.
            let mut young = Vec::new();
            creature(&mut young, kind, Vec3::ZERO, 0.0, 0.0, 0.0, 0.0, 0.55, 0.0, LIT);
            let top = young.iter().map(|v| v.pos[1]).fold(f32::MIN, f32::max);
            assert!(top < highest * 0.6, "{kind:?}");
        }
    }

    #[test]
    fn faces_of_a_box_wind_outwards() {
        let mut out = Vec::new();
        colored_box(&mut out, Mat4::IDENTITY, Vec3::ZERO, Vec3::ONE, [255; 4], LIT);
        assert_eq!(out.len(), 36);
        for tri in out.chunks(3) {
            let a = Vec3::from_array(tri[0].pos);
            let b = Vec3::from_array(tri[1].pos);
            let c = Vec3::from_array(tri[2].pos);
            let n = (b - a).cross(c - a);
            let center = (a + b + c) / 3.0 - Vec3::splat(0.5);
            assert!(n.dot(center) > 0.0, "a triangle faces inwards");
        }
    }

    #[test]
    fn yaw_turns_a_model_the_way_physics_turns_a_body() {
        // A quarter turn of yaw faces -x in physics; the model's +z must end up pointing there.
        let m = Mat4::from_rotation_y(-0.25 * std::f32::consts::TAU);
        let facing = m.transform_vector3(Vec3::Z);
        assert!(facing.x < -0.99, "{facing}");
    }

    #[test]
    fn particles_stop_at_solid_ground_and_die_of_age() {
        let mut p = Particle {
            pos: DVec3::new(0.5, 10.5, 0.5),
            vel: DVec3::new(0.0, -5.0, 0.0),
            age: 0.0,
            life: 1.0,
            size: 0.1,
            color: [255; 4],
            layer: NO_TEXTURE,
            uv: [0.0, 0.0],
            gravity: 9.0,
            glow: false,
        };
        let solid = |b: BlockPos| b.y < 10;
        let mut alive = true;
        for _ in 0..40 {
            alive = p.update(0.05, solid);
        }
        assert!(p.pos.y >= 10.0, "fell through at {}", p.pos.y);
        assert!(!alive);
    }

    #[test]
    fn items_of_every_kind_can_be_drawn_in_the_world_and_in_the_hand() {
        let reg = Registry::builtin();
        let mut shapes = ItemShapes::default();
        for i in 0..reg.items().len() as u16 {
            let stack = ItemStack::new(i, 1);
            let mut world = Vec::new();
            item_entity(&mut world, &reg, &mut shapes, Vec3::ZERO, stack, 0.0, 1, LIT);
            assert!(!world.is_empty(), "{}", reg.items()[i as usize].name);
            let mut hand = Vec::new();
            view_model(&mut hand, &reg, &mut shapes, Some(stack), 0.3, 0.0, 0.0, false, LIT);
            assert!(!hand.is_empty());
            let mut busy = Vec::new();
            view_model(&mut busy, &reg, &mut shapes, Some(stack), 0.0, 0.0, 1.0, true, LIT);
            assert!(!busy.is_empty());
        }
    }

    #[test]
    fn an_item_is_its_icon_made_solid_with_sides_only_at_its_edges() {
        // A plus sign, three pixels of the solid picture wide.
        let mut icon = vec![[0u8; 4]; ICON * ICON];
        let scale = ICON / SOLID;
        for (cx, cy) in [(5, 4), (4, 5), (5, 5), (6, 5), (5, 6)] {
            for dy in 0..scale {
                for dx in 0..scale {
                    icon[(cy * scale + dy) * ICON + cx * scale + dx] = [200, 40, 40, 255];
                }
            }
        }
        let voxels = solid_picture(&icon);
        assert_eq!(voxels.len(), 5);
        let middle = voxels.iter().find(|v| (v.x, v.y) == (5, 5)).expect("the middle");
        assert_eq!(middle.open, 0, "surrounded, it shows no sides");
        let top = voxels.iter().find(|v| (v.x, v.y) == (5, 4)).expect("the top");
        assert_eq!(top.open, 1 | 2 | 4, "the top arm shows left, right and up");
        let mut out = Vec::new();
        solid_item(&mut out, Mat4::IDENTITY, &voxels, LIT);
        // Each pixel shows back and front, and 12 sides go round the outline: 22 faces.
        assert_eq!(out.len(), 22 * 6);
        // One pixel thick, and within the square it fills.
        assert!(out.iter().all(|v| v.pos[2].abs() <= 0.5 / SOLID as f32 + 1e-6));
        assert!(out.iter().all(|v| v.pos[0].abs() <= 0.5 && v.pos[1].abs() <= 0.5));
        // Faces wind outwards, as the boxes' do.
        for tri in out.chunks(3) {
            let (a, b, c) = (Vec3::from_array(tri[0].pos), Vec3::from_array(tri[1].pos), Vec3::from_array(tri[2].pos));
            let n = Vec3::new(f32::from(tri[0].normal[0]), f32::from(tri[0].normal[1]), f32::from(tri[0].normal[2]));
            assert!((b - a).cross(c - a).dot(n) > 0.0);
        }
    }

    #[test]
    fn a_sword_is_held_as_a_sword() {
        let reg = Registry::builtin();
        let sword = reg.item_by_name("iron_sword").expect("a sword");
        let mut shapes = ItemShapes::default();
        let voxels = shapes.get(&reg, sword).to_vec();
        // Long and thin, corner to corner, not a plate: far fewer pixels than the square holds.
        assert!(voxels.len() > 12 && voxels.len() < SOLID * SOLID / 3, "{} pixels", voxels.len());
        let spread =
            |f: fn(&Voxel) -> u8| voxels.iter().map(f).max().unwrap_or(0) - voxels.iter().map(f).min().unwrap_or(0);
        assert!(spread(|v| v.x) >= 10 && spread(|v| v.y) >= 10, "it runs corner to corner");
    }
}
