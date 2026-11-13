//! Everything drawn over the world while playing: the HUD and the screens.
use crate::game::{Game, Overlay};
use crate::options::Look;
use crate::render::rt::TraceStats;
use crate::render::ui::*;
use crate::render::{Camera, Tracing};
use glam::Vec3;
use zenith_agent::Session;
use zenith_content::ItemStack;
use zenith_core::BlockPos;
use zenith_protocol::*;
use zenith_world::Generator;

/// The mouse, in UI units, as of this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mouse {
    pub x: f32,
    pub y: f32,
    pub left: bool,
    pub right: bool,
    pub shift: bool,
}

/// A clickable label. Returns whether it was clicked.
pub fn button(ui: &mut UiBatch, m: Mouse, x: f32, y: f32, w: f32, h: f32, label: &str) -> bool {
    let hover = m.x >= x && m.x < x + w && m.y >= y && m.y < y + h;
    ui.rect(x, y, w, h, if hover { [74, 80, 104, 245] } else { PANEL_LIGHT });
    ui.rect(x, y, w, 1.0, [255, 255, 255, if hover { 70 } else { 30 }]);
    ui.rect(x, y + h - 1.0, w, 1.0, [0, 0, 0, 110]);
    ui.text_centered(x + w / 2.0, y + (h - 7.0) / 2.0, label, 1.0, if hover { ACCENT } else { WHITE });
    hover && m.left
}

const SLOT_SIZE: f32 = 20.0;
/// The gap under the hotbar: on a phone, enough to clear the bar Android draws for its home gesture.
pub const BELOW_HOTBAR: f32 = if cfg!(target_os = "android") { 14.0 } else { 3.0 };

fn draw_stack(ui: &mut UiBatch, reg: &zenith_content::Registry, x: f32, y: f32, stack: ItemStack) {
    ui.icon(x + 2.0, y + 2.0, 16.0, stack.item);
    if stack.count > 1 {
        let text = stack.count.to_string();
        let w = ui.text_width(&text, 1.0);
        ui.text(x + 19.0 - w, y + 12.0, &text, 1.0, WHITE);
    }
    let durability = reg.item(stack.item).and_then(|i| i.kind.durability());
    if let Some(durability) = durability
        && stack.damage > 0
    {
        let left = 1.0 - stack.damage as f32 / durability as f32;
        ui.rect(x + 3.0, y + 16.0, 14.0, 2.0, [0, 0, 0, 255]);
        let color = [((1.0 - left) * 255.0) as u8, (left * 255.0) as u8, 40, 255];
        ui.rect(x + 3.0, y + 16.0, 14.0 * left, 1.0, color);
    }
}

/// A loaf in pixels, for the food bar. `half` draws only its left half.
fn loaf(ui: &mut UiBatch, x: f32, y: f32, color: Rgba, half: bool) {
    const ROWS: [(f32, f32); 6] = [(2.0, 3.0), (1.0, 5.0), (0.0, 7.0), (0.0, 7.0), (0.0, 7.0), (1.0, 5.0)];
    for (i, (ox, w)) in ROWS.iter().enumerate() {
        let w = if half { (3.5 - ox).max(0.0) } else { *w };
        ui.rect(x + ox, y + 1.0 + i as f32, w, 1.0, color);
    }
}

/// A shield in pixels, for the armour bar.
fn shield(ui: &mut UiBatch, x: f32, y: f32, color: Rgba, half: bool) {
    const ROWS: [(f32, f32); 7] = [(0.0, 7.0), (0.0, 7.0), (0.0, 7.0), (0.0, 7.0), (1.0, 5.0), (2.0, 3.0), (3.0, 1.0)];
    for (i, (ox, w)) in ROWS.iter().enumerate() {
        let w = if half { (3.5 - ox).max(0.0) } else { *w };
        ui.rect(x + ox, y + i as f32, w, 1.0, color);
    }
}

/// A heart in pixels.
fn heart(ui: &mut UiBatch, x: f32, y: f32, color: Rgba) {
    const ROWS: [(f32, f32); 7] = [(1.0, 2.0), (0.0, 7.0), (0.0, 7.0), (0.0, 7.0), (1.0, 5.0), (2.0, 3.0), (3.0, 1.0)];
    for (i, (ox, w)) in ROWS.iter().enumerate() {
        ui.rect(x + ox, y + i as f32, *w, 1.0, color);
    }
    ui.rect(x + 4.0, y, 2.0, 1.0, color);
    // Clear the notch between the lobes.
    ui.rect(x + 3.0, y, 1.0, 1.0, [0, 0, 0, 0]);
}

/// The touch controls, once a touch has been seen: the buttons, and the stick while a thumb holds it.
fn touch_controls(ui: &mut UiBatch, view: crate::touch::TouchView) {
    use crate::touch::Button;
    if !view.active {
        return;
    }
    let s = ui.scale;
    for b in Button::ALL {
        let ((x, y), half) = b.place(view.size);
        let (x, y, half) = (x / s, y / s, half / s);
        ui.rect(x - half, y - half, half * 2.0, half * 2.0, [20, 22, 30, 110]);
        ui.text_centered(x, y - 3.0, b.label(), 1.0, [230, 230, 236, 200]);
    }
    if let Some((anchor, thumb)) = view.stick {
        let reach = view.size.1 * 0.12 / s;
        let (ax, ay) = (anchor.0 / s, anchor.1 / s);
        ui.rect(ax - reach, ay - reach, reach * 2.0, reach * 2.0, [20, 22, 30, 80]);
        // The knob stays inside the rim, however far the thumb goes.
        let (dx, dy) = (thumb.0 / s - ax, thumb.1 / s - ay);
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let k = len.min(reach) / len;
        let (kx, ky) = (ax + dx * k, ay + dy * k);
        let knob = reach * 0.45;
        ui.rect(kx - knob, ky - knob, knob * 2.0, knob * 2.0, [200, 204, 220, 150]);
    }
}

fn hotbar(ui: &mut UiBatch, s: &Session) {
    let (w, h) = (ui.width, ui.height);
    let total = SLOT_SIZE * 9.0;
    let x0 = ((w - total) / 2.0).floor();
    let y0 = h - SLOT_SIZE - BELOW_HOTBAR;
    ui.rect(x0 - 2.0, y0 - 2.0, total + 4.0, SLOT_SIZE + 4.0, [10, 11, 16, 170]);
    for i in 0..9usize {
        let x = x0 + i as f32 * SLOT_SIZE;
        let selected = s.selected as usize == i;
        ui.rect(
            x + 1.0,
            y0 + 1.0,
            SLOT_SIZE - 2.0,
            SLOT_SIZE - 2.0,
            if selected { [90, 96, 118, 220] } else { [38, 42, 54, 200] },
        );
        if selected {
            ui.rect(x, y0, SLOT_SIZE, 1.0, ACCENT);
            ui.rect(x, y0 + SLOT_SIZE - 1.0, SLOT_SIZE, 1.0, ACCENT);
            ui.rect(x, y0, 1.0, SLOT_SIZE, ACCENT);
            ui.rect(x + SLOT_SIZE - 1.0, y0, 1.0, SLOT_SIZE, ACCENT);
        }
        if let Some(stack) = s.inventory.get(i).copied().flatten() {
            draw_stack(ui, &s.reg, x, y0, stack);
        }
    }

    if s.mode == GameMode::Survival {
        // Experience: a thin bar right over the hotbar, and the level between hearts and food.
        ui.rect(x0, y0 - 5.0, total, 3.0, [16, 18, 12, 200]);
        ui.rect(x0, y0 - 5.0, total * s.xp as f32 / 255.0, 3.0, [126, 220, 74, 255]);
        if s.level > 0 {
            ui.text_centered(w / 2.0, y0 - 15.0, &s.level.to_string(), 1.0, [150, 240, 96, 255]);
        }
        // Health over the left half of the bar, ten hearts of two points each.
        let y = y0 - 15.0;
        for i in 0..10 {
            let x = x0 + i as f32 * 8.0;
            heart(ui, x, y, [30, 10, 12, 210]);
            let points = s.health as i32 - i * 2;
            if points >= 2 {
                heart(ui, x, y, [226, 44, 52, 255]);
            } else if points == 1 {
                heart(ui, x, y, [150, 40, 46, 255]);
            }
        }
        // Food over the right half, ten loaves of two points each, emptying from the left.
        for i in 0..10 {
            let x = x0 + total - 8.0 - i as f32 * 8.0;
            loaf(ui, x, y, [34, 22, 12, 210], false);
            let points = s.food as i32 - i * 2;
            if points >= 2 {
                loaf(ui, x, y, [214, 150, 70, 255], false);
            } else if points == 1 {
                loaf(ui, x, y, [214, 150, 70, 255], true);
            }
        }
        // Armour over the hearts, when any is worn.
        if s.armor_points > 0 {
            let y = y - 9.0;
            for i in 0..10 {
                let x = x0 + i as f32 * 8.0;
                let points = s.armor_points as i32 - i * 2;
                shield(ui, x, y, [30, 32, 40, 200], false);
                if points >= 2 {
                    shield(ui, x, y, [200, 206, 220, 255], false);
                } else if points == 1 {
                    shield(ui, x, y, [200, 206, 220, 255], true);
                }
            }
        }
        // Air only shows when it is being used.
        if s.air < 20 {
            let y = y - 9.0;
            for i in 0..10i32 {
                let x = x0 + total - 8.0 - i as f32 * 8.0;
                if (s.air as i32) > i * 2 {
                    ui.rect(x + 1.0, y, 5.0, 5.0, [120, 186, 240, 255]);
                    ui.rect(x + 2.0, y + 1.0, 1.0, 1.0, WHITE);
                }
            }
        }
    }
    // The held item's name, for a moment's reading.
    if let Some(stack) = s.held()
        && let Some(def) = s.reg.item(stack.item)
    {
        let above = if s.mode == GameMode::Survival { 36.0 } else { 24.0 };
        ui.text_centered(w / 2.0, y0 - above, &def.display, 1.0, [255, 255, 255, 190]);
    }
}

fn crosshair(ui: &mut UiBatch) {
    let (cx, cy) = ((ui.width / 2.0).floor(), (ui.height / 2.0).floor());
    let c = [255, 255, 255, 210];
    ui.rect(cx - 5.0, cy, 11.0, 1.0, c);
    ui.rect(cx, cy - 5.0, 1.0, 11.0, c);
}

fn chat_lines(ui: &mut UiBatch, s: &Session, open: bool) {
    let h = ui.height;
    let max = if open { 18 } else { 6 };
    let start = s.chat.len().saturating_sub(max);
    let shown: Vec<_> = s.chat.iter().skip(start).collect();
    let mut y = h - 45.0 - BELOW_HOTBAR - shown.len() as f32 * 9.0;
    for line in shown {
        let text = match &line.from {
            Some(from) => format!("<{from}> {}", line.text),
            None => line.text.clone(),
        };
        let w = ui.text_width(&text, 1.0).min(ui.width - 8.0);
        ui.rect(2.0, y - 1.0, w + 4.0, 9.0, [0, 0, 0, 96]);
        ui.text(4.0, y, &text, 1.0, if line.from.is_some() { WHITE } else { [216, 224, 150, 255] });
        y += 9.0;
    }
}

fn debug_text(ui: &mut UiBatch, g: &Game, stats: DebugStats) {
    let s = g.session();
    let b = s.body;
    let col = s.generator.column(b.pos.x, b.pos.z);
    let cell = BlockPos::from_vec(b.pos);
    let light = g.streamer.light_at(cell.x, cell.y, cell.z);
    let lines = [
        format!("Zenith  {:.0} fps  {:.1} ms", g.fps, g.frame_ms),
        format!("{}  {}", stats.adapter, stats.backend),
        format!("XYZ {:.2} {:.2} {:.2}", b.pos.x, b.pos.y, b.pos.z),
        format!("Chunk {} {}   facing {:.0} {:.0}", cell.x >> 4, cell.z >> 4, g.yaw, g.pitch),
        format!("Biome {}   height {:.0}", Generator::biome(&col).name(), col.height),
        format!("Time {}   rain {}   light {:?}", s.clock.time, s.clock.rain, light),
        format!(
            "Chunks {} loaded, {} pending; meshes {} pending",
            s.loaded_chunks(),
            s.pending_chunks(),
            g.streamer.pending()
        ),
        format!(
            "Sections {} on GPU, {} drawn, {} in shadow; {} quads",
            stats.sections, stats.drawn, stats.shadow, stats.quads
        ),
        match g.streamer.reach() {
            Some(reach) => format!(
                "Meshed chunks {}, out to {} (GPU full); far tiles {} ({} pending)",
                g.streamer.meshed_chunks, reach, stats.far, stats.far_pending
            ),
            None => format!(
                "Meshed chunks {}; far tiles {} ({} pending)",
                g.streamer.meshed_chunks, stats.far, stats.far_pending
            ),
        },
        format!("Vertex memory {:.0} / {:.0} MB", stats.used, stats.capacity),
        match &stats.tracing {
            Tracing::On => format!(
                "Ray tracing on: {} sections, {} waiting, {:.1}M triangles",
                stats.traced.sections,
                stats.traced.waiting,
                stats.traced.triangles as f64 / 1e6
            ),
            other => format!("Ray tracing {}", other.describe()),
        },
        format!("Entities {}   corrections {}   mode {:?}", s.entities.len(), s.corrections, s.mode),
    ];
    for (i, l) in lines.iter().enumerate() {
        let y = 3.0 + i as f32 * 9.0;
        let w = ui.text_width(l, 1.0);
        ui.rect(2.0, y - 1.0, w + 3.0, 9.0, [0, 0, 0, 120]);
        ui.text_raw(3.0, y, l, 1.0, WHITE);
    }
}

#[derive(Clone, Default)]
pub struct DebugStats {
    pub adapter: String,
    pub backend: String,
    pub sections: usize,
    pub drawn: usize,
    pub shadow: usize,
    pub quads: u64,
    pub far: usize,
    pub far_pending: usize,
    pub used: f32,
    pub capacity: f32,
    pub tracing: Tracing,
    pub traced: TraceStats,
}

/// What a screen wants clicked this frame.
struct SlotGrid {
    hovered: Option<u16>,
}

fn slot(
    ui: &mut UiBatch,
    s: &Session,
    m: Mouse,
    grid: &mut SlotGrid,
    x: f32,
    y: f32,
    index: u16,
    content: Option<ItemStack>,
) {
    let hover = m.x >= x && m.x < x + SLOT_SIZE && m.y >= y && m.y < y + SLOT_SIZE;
    ui.rect(x, y, SLOT_SIZE, SLOT_SIZE, [12, 13, 18, 255]);
    ui.rect(x + 1.0, y + 1.0, SLOT_SIZE - 2.0, SLOT_SIZE - 2.0, if hover { SLOT_HOVER } else { SLOT });
    if let Some(stack) = content {
        draw_stack(ui, &s.reg, x, y, stack);
    }
    if hover {
        grid.hovered = Some(index);
    }
}

/// What a click on the screen's own slots means is the server's business: this only reports them.
fn inventory_screen(ui: &mut UiBatch, g: &mut Game, m: Mouse) {
    let (w, h) = (ui.width, ui.height);
    ui.rect(0.0, 0.0, w, h, [4, 5, 10, 130]);
    let creative = g.session().mode == GameMode::Creative;
    let panel_w = SLOT_SIZE * 9.0 + 16.0;
    let top_h = 92.0;
    let panel_h = top_h + SLOT_SIZE * 4.0 + 24.0;
    let total_w = if creative { panel_w * 2.0 + 8.0 } else { panel_w };
    let x0 = ((w - total_w) / 2.0).floor();
    let y0 = ((h - panel_h) / 2.0).floor();
    ui.panel(x0, y0, panel_w, panel_h);

    let session = &g.agent.session;
    let mut grid = SlotGrid { hovered: None };
    let screen = session.screen.clone();
    let kind = screen.as_ref().map_or(ScreenKind::Inventory, |s| s.kind);
    let get = |i: usize| screen.as_ref().and_then(|s| s.slots.get(i).copied().flatten());
    let title = match kind {
        ScreenKind::Inventory => "Inventory",
        ScreenKind::Bench => "Workbench",
        ScreenKind::Furnace => "Furnace",
        ScreenKind::Chest => "Chest",
    };
    ui.text(x0 + 8.0, y0 + 6.0, title, 1.0, WHITE);

    let (tx, ty) = (x0 + 8.0, y0 + 16.0);
    match kind {
        ScreenKind::Inventory | ScreenKind::Bench => {
            let size = if kind == ScreenKind::Bench { 3 } else { 2 };
            let gx = tx + 18.0;
            for r in 0..size {
                for c in 0..size {
                    let i = r * size + c;
                    slot(
                        ui,
                        session,
                        m,
                        &mut grid,
                        gx + c as f32 * SLOT_SIZE,
                        ty + r as f32 * SLOT_SIZE,
                        slots::SCREEN + i as u16,
                        get(i),
                    );
                }
            }
            let ox = gx + size as f32 * SLOT_SIZE + 26.0;
            let oy = ty + (size as f32 * SLOT_SIZE - SLOT_SIZE) / 2.0;
            arrow_right(ui, ox - 20.0, oy + 6.0, 1.0);
            slot(ui, session, m, &mut grid, ox, oy, slots::SCREEN + (size * size) as u16, get(size * size));
        }
        ScreenKind::Furnace => {
            let progress = screen.as_ref().map_or([0, 0], |s| s.progress);
            let cx = tx + 40.0;
            slot(ui, session, m, &mut grid, cx, ty, slots::SCREEN, get(0));
            slot(ui, session, m, &mut grid, cx, ty + SLOT_SIZE * 2.0, slots::SCREEN + 1, get(1));
            // The flame between them shrinks as fuel burns.
            let fuel = progress[1] as f32 / 255.0;
            ui.rect(cx + 6.0, ty + SLOT_SIZE + 4.0, 8.0, 12.0, [40, 34, 34, 255]);
            ui.rect(cx + 6.0, ty + SLOT_SIZE + 4.0 + 12.0 * (1.0 - fuel), 8.0, 12.0 * fuel, [250, 150, 50, 255]);
            arrow_right(ui, cx + 30.0, ty + SLOT_SIZE + 6.0, progress[0] as f32 / 255.0);
            slot(ui, session, m, &mut grid, cx + 58.0, ty + SLOT_SIZE, slots::SCREEN + 2, get(2));
        }
        ScreenKind::Chest => {
            for r in 0..3 {
                for c in 0..9 {
                    let i = r * 9 + c;
                    slot(
                        ui,
                        session,
                        m,
                        &mut grid,
                        tx + c as f32 * SLOT_SIZE,
                        ty + r as f32 * SLOT_SIZE,
                        slots::SCREEN + i as u16,
                        get(i),
                    );
                }
            }
        }
    }

    // What is worn, in the player's own screen: head and body, then legs and feet.
    if kind == ScreenKind::Inventory {
        let ax = x0 + panel_w - 8.0 - SLOT_SIZE * 2.0;
        ui.text(ax, y0 + 6.0, "Worn", 1.0, TEXT_DIM);
        for (k, part) in zenith_content::builtin::ARMOR_SLOTS.iter().enumerate() {
            let (x, y) = (ax + (k % 2) as f32 * SLOT_SIZE, ty + (k / 2) as f32 * SLOT_SIZE);
            let worn = session.armor[k];
            slot(ui, session, m, &mut grid, x, y, slots::ARMOR + k as u16, worn);
            // An empty slot shows a ghost of what goes in it.
            if worn.is_none()
                && let Some(item) = session.reg.item_by_name(&format!("iron_{part}"))
            {
                ui.icon(x + 2.0, y + 2.0, 16.0, item);
                ui.rect(x + 1.0, y + 1.0, SLOT_SIZE - 2.0, SLOT_SIZE - 2.0, [30, 33, 44, 205]);
            }
        }
    }

    let (ix, iy) = (x0 + 8.0, y0 + top_h + 8.0);
    for r in 0..3usize {
        for c in 0..9usize {
            let i = 9 + r * 9 + c;
            slot(
                ui,
                session,
                m,
                &mut grid,
                ix + c as f32 * SLOT_SIZE,
                iy + r as f32 * SLOT_SIZE,
                i as u16,
                session.inventory.get(i).copied().flatten(),
            );
        }
    }
    let hy = iy + 3.0 * SLOT_SIZE + 4.0;
    for c in 0..9usize {
        slot(
            ui,
            session,
            m,
            &mut grid,
            ix + c as f32 * SLOT_SIZE,
            hy,
            c as u16,
            session.inventory.get(c).copied().flatten(),
        );
    }

    // Creative: everything there is, a click away.
    let mut picked = None;
    let mut hover_name: Option<String> = None;
    if creative {
        let per_page = 9 * 8;
        let items = session.reg.items().len();
        let pages = items.div_ceil(per_page).max(1);
        let page = g.creative_page.min(pages - 1);
        let (cx, cy) = (x0 + panel_w + 8.0, y0);
        ui.panel(cx, cy, panel_w, panel_h);
        ui.text(cx + 8.0, cy + 6.0, &format!("All items  {}/{}", page + 1, pages), 1.0, WHITE);
        for n in 0..per_page {
            let i = page * per_page + n;
            if i >= items {
                break;
            }
            let (x, y) = (cx + 8.0 + (n % 9) as f32 * SLOT_SIZE, cy + 16.0 + (n / 9) as f32 * SLOT_SIZE);
            let hover = m.x >= x && m.x < x + SLOT_SIZE && m.y >= y && m.y < y + SLOT_SIZE;
            ui.rect(x, y, SLOT_SIZE, SLOT_SIZE, if hover { SLOT_HOVER } else { SLOT });
            ui.icon(x + 2.0, y + 2.0, 16.0, i as u16);
            if hover {
                hover_name = session.reg.item(i as u16).map(|d| d.display.clone());
                if m.left {
                    picked = Some(i as u16);
                }
            }
        }
        let by = cy + panel_h - 18.0;
        if page > 0 && button(ui, m, cx + 8.0, by, 40.0, 14.0, "<") {
            g.creative_page = page - 1;
        }
        if page + 1 < pages && button(ui, m, cx + panel_w - 48.0, by, 40.0, 14.0, ">") {
            g.creative_page = page + 1;
        }
    }

    let session = &g.agent.session;
    if let Some(i) = grid.hovered {
        let content = if i < slots::INVENTORY {
            session.inventory.get(i as usize).copied().flatten()
        } else if i < slots::SCREEN {
            session.armor.get((i - slots::ARMOR) as usize).copied().flatten()
        } else {
            get(i as usize - slots::SCREEN as usize)
        };
        if let Some(stack) = content {
            hover_name = session.reg.item(stack.item).map(|d| d.display.clone());
        }
    }
    if let Some(stack) = session.cursor {
        let reg = session.reg.clone();
        draw_stack(ui, &reg, m.x - 10.0, m.y - 10.0, stack);
    } else if let Some(name) = hover_name {
        let tw = ui.text_width(&name, 1.0);
        ui.rect(m.x + 8.0, m.y - 12.0, tw + 6.0, 11.0, [16, 10, 26, 240]);
        ui.text(m.x + 11.0, m.y - 10.0, &name, 1.0, WHITE);
    }

    if let Some(item) = picked {
        session.send(&C2S::Pick { item });
    }
    if let Some(i) = grid.hovered {
        if m.left {
            let button = if m.shift { ClickButton::Quick } else { ClickButton::Left };
            session.send(&C2S::Click { slot: i, button });
        } else if m.right {
            session.send(&C2S::Click { slot: i, button: ClickButton::Right });
        }
    }
}

fn arrow_right(ui: &mut UiBatch, x: f32, y: f32, filled: f32) {
    ui.rect(x, y + 2.0, 12.0, 3.0, [60, 64, 78, 255]);
    ui.rect(x, y + 2.0, 12.0 * filled, 3.0, [230, 230, 236, 255]);
    for i in 0..4 {
        let c = if filled >= 0.98 { [230, 230, 236, 255] } else { [60, 64, 78, 255] };
        ui.rect(x + 12.0 + i as f32, y + i as f32 - 1.0, 1.0, 9.0 - i as f32 * 2.0, c);
    }
}

fn pause_screen(ui: &mut UiBatch, g: &mut Game, m: Mouse) {
    let (w, h) = (ui.width, ui.height);
    ui.rect(0.0, 0.0, w, h, [4, 5, 10, 120]);
    let (bw, x) = (170.0, ((w - 170.0) / 2.0).floor());
    let mut y = (h / 2.0 - 70.0).floor();
    ui.text_centered(w / 2.0, y - 18.0, "Game paused", 2.0, WHITE);
    if button(ui, m, x, y, bw, 18.0, "Back to the game") {
        g.overlay = Overlay::None;
    }
    y += 22.0;
    let label = format!("View distance: {} chunks", g.options.render_distance);
    ui.rect(x, y, bw, 18.0, PANEL_LIGHT);
    ui.text_centered(x + bw / 2.0, y + 5.0, &label, 1.0, WHITE);
    if button(ui, m, x, y, 18.0, 18.0, "-") {
        g.options.render_distance = (g.options.render_distance - 2).max(4);
        g.agent.session.view_radius = g.options.render_distance + 1;
    }
    if button(ui, m, x + bw - 18.0, y, 18.0, 18.0, "+") {
        g.options.render_distance = (g.options.render_distance + 2).min(48);
        g.agent.session.view_radius = g.options.render_distance + 1;
    }
    y += 22.0;
    let far = format!("Far terrain: {}", if g.options.far_terrain { "on" } else { "off" });
    if button(ui, m, x, y, bw, 18.0, &far) {
        g.options.far_terrain = !g.options.far_terrain;
    }
    y += 22.0;
    if button(ui, m, x, y, bw, 18.0, &format!("Look: {}", g.options.look.name())) {
        g.options.look = match g.options.look {
            Look::Classic => Look::Modern,
            Look::Modern => Look::Classic,
        };
    }
    y += 22.0;
    // Where tracing is unavailable a click can still turn the option off, in case it was on when the
    // reason turned up, but never on.
    let unavailable = matches!(g.debug.tracing, Tracing::Unavailable(_));
    let tracing = if unavailable {
        "Ray tracing: not available".to_string()
    } else {
        format!("Ray tracing: {}", if g.options.ray_tracing { "on" } else { "off" })
    };
    if button(ui, m, x, y, bw, 18.0, &tracing) && (g.options.ray_tracing || !unavailable) {
        g.options.ray_tracing = !g.options.ray_tracing;
    }
    y += 22.0;
    let fov = format!("Field of view: {:.0}", g.options.fov);
    ui.rect(x, y, bw, 18.0, PANEL_LIGHT);
    ui.text_centered(x + bw / 2.0, y + 5.0, &fov, 1.0, WHITE);
    if button(ui, m, x, y, 18.0, 18.0, "-") {
        g.options.fov = (g.options.fov - 5.0).max(40.0);
    }
    if button(ui, m, x + bw - 18.0, y, 18.0, 18.0, "+") {
        g.options.fov = (g.options.fov + 5.0).min(120.0);
    }
    y += 22.0;
    if button(ui, m, x, y, bw, 18.0, "Debug options") {
        g.overlay = Overlay::Debug;
    }
    y += 26.0;
    if button(ui, m, x, y, bw, 18.0, "Save and leave") {
        g.leave_to_title = true;
    }
}

/// The debug options screen, F6: what to show over the world, and a few commands for trying things.
fn debug_screen(ui: &mut UiBatch, g: &mut Game, m: Mouse) {
    let (w, h) = (ui.width, ui.height);
    ui.rect(0.0, 0.0, w, h, [4, 5, 10, 90]);
    let (cw, gap) = (150.0, 12.0);
    let left = ((w - cw * 2.0 - gap) / 2.0).floor();
    let right = left + cw + gap;
    let top = (h / 2.0 - 86.0).floor().max(26.0);
    ui.text_centered(w / 2.0, top - 18.0, "Debug options", 2.0, WHITE);

    let mut y = top;
    let view = &mut g.debug_view;
    for (label, value) in [
        ("Info text (F3)", &mut g.show_debug),
        ("Frame graph", &mut view.graph),
        ("Chunk borders", &mut view.chunk_borders),
        ("Hitboxes", &mut view.hitboxes),
        ("Fog", &mut view.fog),
        ("Clouds", &mut view.clouds),
        ("Full bright", &mut view.full_bright),
    ] {
        if button(ui, m, left, y, cw, 18.0, &format!("{label}: {}", if *value { "on" } else { "off" })) {
            *value = !*value;
        }
        y += 22.0;
    }

    // Commands, as an operator would type them. Single player makes everyone one.
    let mut y = top;
    let commands: [(&str, &[(&str, &str)]); 3] = [
        ("Time", &[("Day", "/time set day"), ("Noon", "/time set noon"), ("Night", "/time set midnight")]),
        ("Weather", &[("Clear", "/weather clear"), ("Rain", "/weather rain")]),
        ("Mode", &[("Survival", "/gamemode survival"), ("Creative", "/gamemode creative")]),
    ];
    for (label, choices) in commands {
        ui.text(right, y + 5.0, label, 1.0, WHITE);
        let lw = 52.0;
        let bw = ((cw - lw) / choices.len() as f32).floor();
        for (i, (name, line)) in choices.iter().enumerate() {
            if button(ui, m, right + lw + i as f32 * bw, y, bw - 2.0, 18.0, name) {
                g.agent.session.send(&C2S::Chat((*line).to_string()));
            }
        }
        y += 22.0;
    }
    if button(ui, m, right, y, cw, 18.0, "Go to spawn") {
        g.agent.session.send(&C2S::Chat("/spawn".to_string()));
    }
    y += 22.0;
    if button(ui, m, right, y, cw, 18.0, "Back to the game") {
        g.overlay = Overlay::None;
    }
}

/// The last frames' times as bars, against lines at 60 and 30 frames a second.
fn frame_graph(ui: &mut UiBatch, times: &std::collections::VecDeque<f32>) {
    let (gw, gh) = (crate::game::FRAME_GRAPH as f32, 44.0);
    let (x0, y0) = (((ui.width - gw) / 2.0).floor(), 4.0);
    // 50 ms fills the graph.
    let scale = gh / 50.0;
    ui.rect(x0 - 2.0, y0 - 2.0, gw + 4.0, gh + 15.0, [0, 0, 0, 130]);
    for (i, &ms) in times.iter().enumerate() {
        let bar = (ms * scale).clamp(1.0, gh);
        let color = match ms {
            t if t <= 17.0 => [90, 220, 110, 220],
            t if t <= 34.0 => [240, 200, 60, 220],
            _ => [240, 70, 60, 230],
        };
        ui.rect(x0 + i as f32, y0 + gh - bar, 1.0, bar, color);
    }
    for ms in [1000.0 / 60.0, 1000.0 / 30.0] {
        ui.rect(x0, (y0 + gh - ms * scale).round(), gw, 1.0, [255, 255, 255, 80]);
    }
    let average = times.iter().sum::<f32>() / times.len().max(1) as f32;
    let worst = times.iter().copied().fold(0.0, f32::max);
    ui.text(x0, y0 + gh + 3.0, &format!("{average:.1} ms average, {worst:.1} worst"), 1.0, WHITE);
}

/// Writing on a sign: four lines on a board, the one being written marked.
fn sign_screen(ui: &mut UiBatch, g: &mut Game, m: Mouse) {
    let (w, h) = (ui.width, ui.height);
    ui.rect(0.0, 0.0, w, h, [4, 5, 10, 120]);
    let (bw, bh) = (150.0, 56.0);
    let (x0, y0) = (((w - bw) / 2.0).floor(), (h / 2.0 - 70.0).floor().max(4.0));
    ui.text_centered(w / 2.0, y0 - 14.0, "Write on the sign", 1.0, WHITE);
    ui.rect(x0 - 2.0, y0 - 2.0, bw + 4.0, bh + 4.0, [96, 70, 40, 255]);
    ui.rect(x0, y0, bw, bh, [196, 162, 108, 255]);
    let caret = ((g.blink() * 2.0) as u32).is_multiple_of(2);
    let Overlay::Sign { lines, line, .. } = &mut g.overlay else { return };
    for (i, text) in lines.iter().enumerate() {
        let y = y0 + 6.0 + i as f32 * 12.0;
        if m.left && m.x >= x0 && m.x < x0 + bw && m.y >= y - 2.0 && m.y < y + 10.0 {
            *line = i;
        }
        let shown = if i == *line && caret { format!("> {text} <") } else { text.clone() };
        ui.text_centered(w / 2.0, y, &shown, 1.0, [36, 24, 12, 255]);
    }
    if button(ui, m, ((w - 80.0) / 2.0).floor(), y0 + bh + 10.0, 80.0, 18.0, "Done") {
        g.close_overlay();
    }
}

/// Asleep: the world dims, and there is a way out of bed.
fn sleep_screen(ui: &mut UiBatch, g: &mut Game, m: Mouse) {
    let (w, h) = (ui.width, ui.height);
    ui.rect(0.0, 0.0, w, h, [4, 6, 18, 175]);
    ui.text_centered(w / 2.0, h / 2.0 - 30.0, "Sleeping", 2.0, WHITE);
    ui.text_centered(w / 2.0, h / 2.0 - 10.0, "The night passes when everyone is in bed.", 1.0, TEXT_DIM);
    let (bw, x) = (120.0, ((w - 120.0) / 2.0).floor());
    if button(ui, m, x, (h / 2.0).floor() + 8.0, bw, 18.0, "Leave bed") {
        g.agent.session.send(&C2S::WakeUp);
    }
}

fn dead_screen(ui: &mut UiBatch, g: &mut Game, m: Mouse) {
    let (w, h) = (ui.width, ui.height);
    ui.rect(0.0, 0.0, w, h, [70, 6, 8, 110]);
    ui.text_centered(w / 2.0, h / 2.0 - 44.0, "You died", 3.0, WHITE);
    let (bw, x) = (150.0, ((w - 150.0) / 2.0).floor());
    if button(ui, m, x, (h / 2.0).floor(), bw, 18.0, "Come back") {
        g.respawn();
    }
    if button(ui, m, x, (h / 2.0).floor() + 22.0, bw, 18.0, "Leave") {
        g.leave_to_title = true;
    }
}

/// Where a point in the world lands on screen, in UI units.
fn on_screen(camera: Camera, ui: &UiBatch, size: (u32, u32), rel: Vec3) -> Option<(f32, f32)> {
    let view = crate::render::look_to(Vec3::ZERO, camera.forward(), Vec3::Y);
    let proj = crate::render::perspective((camera.fov_deg as f32).to_radians(), size.0 as f32 / size.1 as f32);
    let p = proj * view * rel.extend(1.0);
    if p.w <= 0.0 {
        return None;
    }
    Some(((p.x / p.w * 0.5 + 0.5) * ui.width, (0.5 - p.y / p.w * 0.5) * ui.height))
}

/// Draws the HUD and whatever screen is open.
pub fn draw(
    g: &mut Game,
    ui: &mut UiBatch,
    mouse: Mouse,
    size: (u32, u32),
    names: &[(Vec3, String)],
    signs: &[(Vec3, Vec<String>)],
    camera: Camera,
) {
    ui.begin(size.0 as f32, size.1 as f32);
    let m = Mouse { x: mouse.x / ui.scale, y: mouse.y / ui.scale, ..mouse };
    // What signs say, on boards of their own over the real ones: part of the world, so shown with
    // the HUD hidden too.
    for (rel, lines) in signs {
        if let Some((x, y)) = on_screen(camera, ui, size, *rel) {
            let shown: Vec<&String> = lines.iter().collect();
            let w = shown.iter().map(|l| ui.text_width(l, 1.0)).fold(24.0, f32::max) + 8.0;
            let h = shown.len() as f32 * 9.0 + 4.0;
            ui.rect(x - w / 2.0, y - h / 2.0, w, h, [150, 116, 70, 200]);
            for (i, line) in shown.iter().enumerate() {
                ui.text_centered(x, y - h / 2.0 + 3.0 + i as f32 * 9.0, line, 1.0, [36, 24, 12, 255]);
            }
        }
    }
    if g.hide_hud && g.overlay == Overlay::None {
        return;
    }
    for (rel, name) in names {
        if let Some((x, y)) = on_screen(camera, ui, size, *rel) {
            let w = ui.text_width(name, 1.0);
            ui.rect(x - w / 2.0 - 2.0, y - 1.0, w + 3.0, 9.0, [0, 0, 0, 90]);
            ui.text_raw(x - w / 2.0, y, name, 1.0, WHITE);
        }
    }
    let overlay = g.overlay.clone();
    let sleeping = g.session().sleeping;
    if overlay == Overlay::None && !sleeping {
        crosshair(ui);
    }
    hotbar(ui, g.session());
    if overlay == Overlay::None && !sleeping {
        touch_controls(ui, g.touch_view);
    }
    if sleeping && overlay == Overlay::None {
        sleep_screen(ui, g, m);
    }
    chat_lines(ui, g.session(), matches!(overlay, Overlay::Chat(_)));
    if g.show_debug {
        debug_text(ui, g, g.debug.clone());
    }
    if g.debug_view.graph {
        frame_graph(ui, &g.frame_times);
    }
    match overlay {
        Overlay::None => {}
        Overlay::Inventory => inventory_screen(ui, g, m),
        Overlay::Pause => pause_screen(ui, g, m),
        Overlay::Debug => debug_screen(ui, g, m),
        Overlay::Sign { .. } => sign_screen(ui, g, m),
        Overlay::Dead => dead_screen(ui, g, m),
        Overlay::Chat(text) => {
            let y = ui.height - 42.0;
            ui.rect(2.0, y - 2.0, ui.width - 4.0, 12.0, [0, 0, 0, 170]);
            let caret = if ((g.blink() * 2.0) as u32).is_multiple_of(2) { "_" } else { "" };
            ui.text(4.0, y, &format!("{text}{caret}"), 1.0, WHITE);
        }
    }
}
