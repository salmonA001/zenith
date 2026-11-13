//! Draws the HUD and screens: a batch of textured quads over one atlas of font and icons.
use super::font::{self, FIRST, GLYPH_H, GLYPHS};
use super::textures::{ICON, Pixels};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct UiVertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

pub type Rgba = [u8; 4];

pub const WHITE: Rgba = [255, 255, 255, 255];
pub const TEXT_DIM: Rgba = [180, 186, 196, 255];
pub const ACCENT: Rgba = [255, 196, 92, 255];
pub const PANEL: Rgba = [18, 20, 28, 226];
pub const PANEL_LIGHT: Rgba = [44, 48, 62, 240];
pub const SLOT: Rgba = [30, 33, 44, 255];
pub const SLOT_HOVER: Rgba = [78, 84, 106, 255];

/// Where things are in the atlas, in pixels.
pub struct Atlas {
    pub pixels: Vec<[u8; 4]>,
    pub width: usize,
    pub height: usize,
    glyphs: Vec<(usize, usize)>,
    icons_start_y: usize,
    icons_per_row: usize,
}

const CELL: usize = 8;
/// As many icons as fit a texture every GPU can make. A registry can have more, shown blank.
const MAX_ICONS: usize = (4096 - 64) / ICON * (1024 / ICON);

impl Atlas {
    /// Builds the atlas: a white pixel, the font, then one icon per item.
    pub fn new(icons: &[Pixels]) -> Self {
        let width = 1024usize;
        let icons_per_row = width / ICON;
        let icons = &icons[..icons.len().min(MAX_ICONS)];
        let rows = icons.len().div_ceil(icons_per_row).max(1);
        let icons_start_y = 64usize;
        let height = (icons_start_y + rows * ICON).next_power_of_two();
        let mut pixels = vec![[0u8, 0, 0, 0]; width * height];
        // The white block every plain rectangle samples.
        for y in 0..4 {
            for x in 0..4 {
                pixels[y * width + x] = [255, 255, 255, 255];
            }
        }
        let mut glyphs = Vec::with_capacity(GLYPHS.len());
        for (i, g) in GLYPHS.iter().enumerate() {
            let (cx, cy) = (8 + (i % 120) * CELL, (i / 120) * CELL);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) != 0 {
                        pixels[(cy + row) * width + cx + col] = [255, 255, 255, 255];
                    }
                }
            }
            glyphs.push((cx, cy));
        }
        for (i, icon) in icons.iter().enumerate() {
            let (ox, oy) = ((i % icons_per_row) * ICON, icons_start_y + (i / icons_per_row) * ICON);
            for y in 0..ICON {
                for x in 0..ICON {
                    pixels[(oy + y) * width + ox + x] = icon[y * ICON + x];
                }
            }
        }
        Self { pixels, width, height, glyphs, icons_start_y, icons_per_row }
    }

    fn icon_origin(&self, index: usize) -> (usize, usize) {
        ((index % self.icons_per_row) * ICON, self.icons_start_y + (index / self.icons_per_row) * ICON)
    }
}

/// One frame's worth of UI.
#[derive(Clone)]
pub struct UiBatch {
    pub verts: Vec<UiVertex>,
    pub indices: Vec<u32>,
    atlas_w: f32,
    atlas_h: f32,
    glyphs: Vec<(usize, usize)>,
    icon_origins: Vec<(usize, usize)>,
    /// How many screen pixels one UI unit is.
    pub scale: f32,
    pub width: f32,
    pub height: f32,
}

impl UiBatch {
    pub fn new(atlas: &Atlas, item_count: usize) -> Self {
        Self {
            verts: Vec::new(),
            indices: Vec::new(),
            atlas_w: atlas.width as f32,
            atlas_h: atlas.height as f32,
            glyphs: atlas.glyphs.clone(),
            icon_origins: (0..item_count.min(MAX_ICONS)).map(|i| atlas.icon_origin(i)).collect(),
            scale: 2.0,
            width: 1280.0,
            height: 720.0,
        }
    }

    /// Starts a frame, with the UI scale that suits the window.
    pub fn begin(&mut self, width: f32, height: f32) {
        self.verts.clear();
        self.indices.clear();
        self.scale = ((height / 360.0).floor()).clamp(1.0, 6.0);
        self.width = width / self.scale;
        self.height = height / self.scale;
    }

    /// A quad in UI units. Everything is scaled to pixels here.
    fn quad(&mut self, x: f32, y: f32, w: f32, h: f32, u0: f32, v0: f32, u1: f32, v1: f32, color: Rgba) {
        let s = self.scale;
        let base = self.verts.len() as u32;
        let (x0, y0, x1, y1) = (x * s, y * s, (x + w) * s, (y + h) * s);
        let (u0, v0, u1, v1) = (u0 / self.atlas_w, v0 / self.atlas_h, u1 / self.atlas_w, v1 / self.atlas_h);
        self.verts.extend_from_slice(&[
            UiVertex { pos: [x0, y0], uv: [u0, v0], color },
            UiVertex { pos: [x1, y0], uv: [u1, v0], color },
            UiVertex { pos: [x1, y1], uv: [u1, v1], color },
            UiVertex { pos: [x0, y1], uv: [u0, v1], color },
        ]);
        self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        self.quad(x, y, w, h, 1.0, 1.0, 3.0, 3.0, color);
    }

    /// A panel with a soft lighter edge.
    pub fn panel(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.rect(x, y, w, h, PANEL);
        self.rect(x, y, w, 1.0, [255, 255, 255, 26]);
        self.rect(x, y + h - 1.0, w, 1.0, [0, 0, 0, 90]);
        self.rect(x, y, 1.0, h, [255, 255, 255, 14]);
        self.rect(x + w - 1.0, y, 1.0, h, [0, 0, 0, 60]);
    }

    /// How wide a piece of text is, in UI units, at `size` (1 is 8 units tall).
    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        let mut w = 0.0;
        for c in text.chars() {
            let i = (c as u32).wrapping_sub(FIRST as u32) as usize;
            let glyph = GLYPHS.get(i).unwrap_or(&GLYPHS[(b'?' - FIRST) as usize]);
            w += (font::extent(glyph).1 + 1) as f32 * size;
        }
        w
    }

    pub fn text_raw(&mut self, x: f32, y: f32, text: &str, size: f32, color: Rgba) {
        let mut pen = x;
        for c in text.chars() {
            let i = (c as u32).wrapping_sub(FIRST as u32) as usize;
            let i = if i < GLYPHS.len() { i } else { (b'?' - FIRST) as usize };
            let (first, width) = font::extent(&GLYPHS[i]);
            let (cx, cy) = self.glyphs[i];
            if GLYPHS[i].iter().any(|&r| r != 0) {
                let (cx, cy) = ((cx + first) as f32, cy as f32);
                self.quad(
                    pen,
                    y,
                    width as f32 * size,
                    GLYPH_H as f32 * size,
                    cx,
                    cy,
                    cx + width as f32,
                    cy + GLYPH_H as f32,
                    color,
                );
            }
            pen += (width + 1) as f32 * size;
        }
    }

    /// Text with a drop shadow, readable over anything.
    pub fn text(&mut self, x: f32, y: f32, text: &str, size: f32, color: Rgba) {
        let shadow = [0, 0, 0, (color[3] as u32 * 150 / 255) as u8];
        // A step down and to the right. Big letters would be lost in a shadow as big as they are.
        let step = size.min(2.0);
        self.text_raw(x + step, y + step, text, size, shadow);
        self.text_raw(x, y, text, size, color);
    }

    pub fn text_centered(&mut self, cx: f32, y: f32, text: &str, size: f32, color: Rgba) {
        let w = self.text_width(text, size);
        self.text(cx - w / 2.0, y, text, size, color);
    }

    /// An item's icon, `size` units across.
    pub fn icon(&mut self, x: f32, y: f32, size: f32, item: u16) {
        let Some(&(ox, oy)) = self.icon_origins.get(item as usize) else { return };
        let (ox, oy) = (ox as f32, oy as f32);
        self.quad(x, y, size, size, ox, oy, ox + ICON as f32, oy + ICON as f32, WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch() -> UiBatch {
        let icons = vec![vec![[9u8, 9, 9, 255]; ICON * ICON]; 100];
        let atlas = Atlas::new(&icons);
        let mut b = UiBatch::new(&atlas, icons.len());
        b.begin(1280.0, 720.0);
        b
    }

    #[test]
    fn the_scale_keeps_ui_about_the_same_size_on_any_screen() {
        let mut b = batch();
        b.begin(1280.0, 720.0);
        assert_eq!(b.scale, 2.0);
        b.begin(3840.0, 2160.0);
        assert_eq!(b.scale, 6.0);
        b.begin(640.0, 300.0);
        assert_eq!(b.scale, 1.0);
    }

    #[test]
    fn atlas_keeps_every_icon_where_it_says() {
        let mut icons = vec![vec![[0u8, 0, 0, 0]; ICON * ICON]; 70];
        icons[69][0] = [1, 2, 3, 4];
        let atlas = Atlas::new(&icons);
        let (ox, oy) = atlas.icon_origin(69);
        assert_eq!(atlas.pixels[oy * atlas.width + ox], [1, 2, 3, 4]);
    }

    #[test]
    fn text_width_matches_what_is_drawn() {
        let mut b = batch();
        let w = b.text_width("Hello", 1.0);
        assert!(w > 0.0);
        b.text_raw(0.0, 0.0, "Hello", 1.0, WHITE);
        let right = b.verts.iter().map(|v| v.pos[0]).fold(0.0, f32::max) / b.scale;
        assert!((right - (w - 1.0)).abs() < 1.0, "{right} vs {w}");
    }

    #[test]
    fn characters_outside_the_font_show_a_question_mark_and_do_not_panic() {
        let mut b = batch();
        b.text(10.0, 1.0, "\u{1F600} tilde ~ and \t", 1.0, WHITE);
        assert!(!b.verts.is_empty());
    }

    #[test]
    fn asking_for_an_item_that_does_not_exist_draws_nothing() {
        let mut b = batch();
        b.icon(0.0, 0.0, 16.0, 9999);
        assert!(b.verts.is_empty());
    }
}
