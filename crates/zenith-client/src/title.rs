//! The screens before a world opens: over a live view of a world's far terrain, not a picture of one.
use crate::hud::{Mouse, button};
use crate::mods_screen::ModsScreen;
use crate::render::ui::*;
use std::path::{Path, PathBuf};
use zenith_protocol::GameMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    Main,
    Worlds,
    NewWorld,
    Join,
    Mods,
    Message(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    WorldName,
    Seed,
    Address,
    PlayerName,
}

/// What the title screen wants done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Open(PathBuf),
    Create { dir: PathBuf, seed: Option<String>, mode: GameMode },
    Join(String),
    Quit,
}

#[derive(Clone, Debug)]
pub struct WorldEntry {
    pub name: String,
    pub dir: PathBuf,
    pub modified: u64,
}

pub struct TitleScreen {
    pub page: Page,
    pub worlds: Vec<WorldEntry>,
    pub world_name: String,
    pub seed: String,
    pub creative: bool,
    pub address: String,
    pub player_name: String,
    pub focus: Field,
    /// A field was tapped and is being typed in, so a phone's keyboard is up. It covers the lower
    /// part of the screen, so the panel moves up to keep that field in sight.
    pub typing: bool,
    /// A field was tapped since the app last looked: the keyboard goes up again even if Android put
    /// it away by itself.
    pub tapped: bool,
    pub mods: ModsScreen,
    home: PathBuf,
}

fn list_worlds(home: &Path) -> Vec<WorldEntry> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(home.join("saves")) {
        for e in entries.flatten() {
            let dir = e.path();
            if !dir.join("level.json").exists() {
                continue;
            }
            let modified = std::fs::metadata(dir.join("level.json"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            out.push(WorldEntry { name: e.file_name().to_string_lossy().into_owned(), dir, modified });
        }
    }
    out.sort_by_key(|w| std::cmp::Reverse(w.modified));
    out
}

/// Where a panel's top goes: the middle of the screen, or, while a phone's keyboard covers the lower
/// part of it, high enough that the field being typed in (`field` down from the top of the panel)
/// shows near the top of the screen.
pub fn panel_top(height: f32, panel_height: f32, typing_at: Option<f32>) -> f32 {
    match typing_at {
        Some(field) if cfg!(target_os = "android") => 10.0 - field,
        _ => ((height - panel_height) / 2.0).floor(),
    }
}

/// A folder name that is safe on any file system, made from whatever a person typed.
pub fn safe_folder(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() { "World".to_string() } else { trimmed }
}

impl TitleScreen {
    pub fn new(home: PathBuf, player_name: String, address: String) -> Self {
        let worlds = list_worlds(&home);
        Self {
            page: Page::Main,
            worlds,
            world_name: "New World".into(),
            seed: String::new(),
            creative: false,
            address,
            player_name,
            focus: Field::WorldName,
            typing: false,
            tapped: false,
            mods: ModsScreen::new(home.clone()),
            home,
        }
    }

    pub fn refresh(&mut self) {
        self.worlds = list_worlds(&self.home);
    }

    /// Takes typed text into whichever field has focus.
    pub fn type_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        if self.page == Page::Mods {
            self.mods.type_char(c);
            return;
        }
        match self.focus {
            Field::WorldName if self.world_name.len() < 32 => self.world_name.push(c),
            Field::Seed if self.seed.len() < 32 => self.seed.push(c),
            Field::Address if self.address.len() < 64 => self.address.push(c),
            Field::PlayerName if self.player_name.len() < 24 && (c.is_ascii_alphanumeric() || c == '_' || c == '-') => {
                self.player_name.push(c)
            }
            _ => {}
        }
    }

    /// Up a level, as Escape does.
    pub fn back(&mut self) {
        if self.page == Page::Mods && !self.mods.back() {
            return;
        }
        self.page = match self.page {
            Page::NewWorld => {
                self.refresh();
                if self.worlds.is_empty() { Page::Main } else { Page::Worlds }
            }
            _ => Page::Main,
        };
    }

    pub fn backspace(&mut self) {
        if self.page == Page::Mods {
            self.mods.backspace();
            return;
        }
        match self.focus {
            Field::WorldName => self.world_name.pop(),
            Field::Seed => self.seed.pop(),
            Field::Address => self.address.pop(),
            Field::PlayerName => self.player_name.pop(),
        };
    }

    fn field(
        &mut self,
        ui: &mut UiBatch,
        m: Mouse,
        x: f32,
        y: f32,
        w: f32,
        label: &str,
        value: String,
        which: Field,
        blink: bool,
    ) {
        ui.text(x, y, label, 1.0, TEXT_DIM);
        let (fy, fh) = (y + 9.0, 16.0);
        let hover = m.x >= x && m.x < x + w && m.y >= fy && m.y < fy + fh;
        if hover && m.left {
            self.focus = which;
            self.typing = true;
            self.tapped = true;
        }
        let focused = self.focus == which;
        ui.rect(x, fy, w, fh, if focused { [14, 16, 24, 250] } else { [24, 26, 36, 240] });
        ui.rect(x, fy + fh - 1.0, w, 1.0, if focused { ACCENT } else { [90, 94, 110, 255] });
        let shown = if focused && blink { format!("{value}_") } else { value };
        ui.text(x + 4.0, fy + 5.0, &shown, 1.0, WHITE);
    }

    /// Draws the current page and returns what it asks for.
    pub fn draw(&mut self, ui: &mut UiBatch, mouse: Mouse, size: (u32, u32), seconds: f32) -> Action {
        ui.begin(size.0 as f32, size.1 as f32);
        let m = Mouse { x: mouse.x / ui.scale, y: mouse.y / ui.scale, ..mouse };
        let (w, h) = (ui.width, ui.height);
        let blink = ((seconds * 2.0) as u32).is_multiple_of(2);
        ui.rect(0.0, 0.0, w, h, [6, 8, 16, 40]);
        let mut action = Action::None;
        let (bw, x) = (190.0, ((w - 190.0) / 2.0).floor());
        // A tap anywhere but a field ends typing; one on a field starts it again below.
        if m.left {
            self.typing = false;
        }
        let page = self.page.clone();
        match self.page.clone() {
            Page::Main => {
                let title = "ZENITH";
                ui.text_centered(w / 2.0, (h * 0.17).floor(), title, 6.0, WHITE);
                ui.text_centered(
                    w / 2.0,
                    (h * 0.17).floor() + 48.0,
                    "A world to the horizon, from nothing but a seed.",
                    1.0,
                    ACCENT,
                );
                let mut y = (h * 0.45).floor();
                if button(ui, m, x, y, bw, 20.0, "Play") {
                    self.refresh();
                    self.page = if self.worlds.is_empty() { Page::NewWorld } else { Page::Worlds };
                    self.focus = Field::WorldName;
                }
                y += 24.0;
                if button(ui, m, x, y, bw, 20.0, "Join a server") {
                    self.page = Page::Join;
                    self.focus = Field::Address;
                }
                y += 24.0;
                if button(ui, m, x, y, bw, 20.0, "Mods") {
                    self.page = Page::Mods;
                    self.mods.open();
                }
                y += 24.0;
                if button(ui, m, x, y, bw, 20.0, "Quit") {
                    action = Action::Quit;
                }
                let note = "Open source. Every texture and sound in it is made by code.";
                ui.text_centered(w / 2.0, h - 12.0, note, 1.0, [255, 255, 255, 130]);
            }
            Page::Worlds => {
                let panel_h = (h - 80.0).min(230.0);
                let y0 = ((h - panel_h) / 2.0).floor();
                ui.panel(x - 20.0, y0, bw + 40.0, panel_h);
                ui.text_centered(w / 2.0, y0 + 7.0, "Choose a world", 1.0, WHITE);
                let mut y = y0 + 20.0;
                let list = self.worlds.clone();
                for world in list.iter().take(((panel_h - 62.0) / 20.0) as usize) {
                    if button(ui, m, x, y, bw, 18.0, &world.name) {
                        action = Action::Open(world.dir.clone());
                    }
                    y += 20.0;
                }
                let by = y0 + panel_h - 22.0;
                if button(ui, m, x, by, bw / 2.0 - 2.0, 18.0, "New world") {
                    self.page = Page::NewWorld;
                    self.focus = Field::WorldName;
                }
                if button(ui, m, x + bw / 2.0 + 2.0, by, bw / 2.0 - 2.0, 18.0, "Back") {
                    self.page = Page::Main;
                }
            }
            Page::NewWorld => {
                let panel_h = 190.0;
                let row = match self.focus {
                    Field::Seed => 1.0,
                    Field::PlayerName => 2.0,
                    _ => 0.0,
                };
                let y0 = panel_top(h, panel_h, self.typing.then_some(20.0 + 30.0 * row));
                ui.panel(x - 20.0, y0, bw + 40.0, panel_h);
                ui.text_centered(w / 2.0, y0 + 7.0, "A new world", 1.0, WHITE);
                let mut y = y0 + 20.0;
                self.field(ui, m, x, y, bw, "Name", self.world_name.clone(), Field::WorldName, blink);
                y += 30.0;
                self.field(ui, m, x, y, bw, "Seed (leave empty for a surprise)", self.seed.clone(), Field::Seed, blink);
                y += 30.0;
                self.field(ui, m, x, y, bw, "Your name", self.player_name.clone(), Field::PlayerName, blink);
                y += 30.0;
                let label = if self.creative { "Mode: Creative" } else { "Mode: Survival" };
                if button(ui, m, x, y, bw, 18.0, label) {
                    self.creative = !self.creative;
                }
                let by = y0 + panel_h - 22.0;
                if button(ui, m, x, by, bw / 2.0 - 2.0, 18.0, "Create") {
                    let dir = self.home.join("saves").join(safe_folder(&self.world_name));
                    let seed = (!self.seed.trim().is_empty()).then(|| self.seed.trim().to_string());
                    action = Action::Create {
                        dir,
                        seed,
                        mode: if self.creative { GameMode::Creative } else { GameMode::Survival },
                    };
                }
                if button(ui, m, x + bw / 2.0 + 2.0, by, bw / 2.0 - 2.0, 18.0, "Back") {
                    self.refresh();
                    self.page = if self.worlds.is_empty() { Page::Main } else { Page::Worlds };
                }
            }
            Page::Join => {
                let panel_h = 130.0;
                let row = if self.focus == Field::PlayerName { 1.0 } else { 0.0 };
                let y0 = panel_top(h, panel_h, self.typing.then_some(20.0 + 30.0 * row));
                ui.panel(x - 20.0, y0, bw + 40.0, panel_h);
                ui.text_centered(w / 2.0, y0 + 7.0, "Join a server", 1.0, WHITE);
                self.field(ui, m, x, y0 + 20.0, bw, "Address", self.address.clone(), Field::Address, blink);
                self.field(ui, m, x, y0 + 50.0, bw, "Your name", self.player_name.clone(), Field::PlayerName, blink);
                let by = y0 + panel_h - 22.0;
                if button(ui, m, x, by, bw / 2.0 - 2.0, 18.0, "Join") {
                    action = Action::Join(self.address.trim().to_string());
                }
                if button(ui, m, x + bw / 2.0 + 2.0, by, bw / 2.0 - 2.0, 18.0, "Back") {
                    self.page = Page::Main;
                }
            }
            Page::Mods => {
                if self.mods.draw(ui, m, blink) {
                    self.page = Page::Main;
                }
            }
            Page::Message(text) => {
                let tw = ui.text_width(&text, 1.0).clamp(160.0, w - 40.0);
                let y0 = (h / 2.0 - 30.0).floor();
                ui.panel((w - tw) / 2.0 - 12.0, y0, tw + 24.0, 54.0);
                ui.text_centered(w / 2.0, y0 + 10.0, &text, 1.0, WHITE);
                if button(ui, m, ((w - 100.0) / 2.0).floor(), y0 + 28.0, 100.0, 18.0, "OK") {
                    self.page = Page::Main;
                }
            }
        }
        if self.page != page {
            self.typing = false;
        }
        action
    }
}

/// A seed from what someone typed: a number stays that number, anything else is hashed.
pub fn parse_seed(text: Option<&str>) -> u64 {
    match text {
        Some(t) => t.parse::<u64>().unwrap_or_else(|_| zenith_core::hash::fnv64(t.as_bytes())),
        None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_safe_on_any_file_system() {
        assert_eq!(safe_folder("My World"), "My World");
        assert_eq!(safe_folder("../../etc"), "_.._etc".replace("_.._etc", "______etc"));
        assert_eq!(safe_folder("a:b*c?"), "a_b_c_");
        assert_eq!(safe_folder("   "), "World");
        assert_eq!(safe_folder("..."), "___");
    }

    #[test]
    fn seeds_can_be_numbers_or_words_and_words_are_repeatable() {
        assert_eq!(parse_seed(Some("1234")), 1234);
        assert_eq!(parse_seed(Some("zenith")), parse_seed(Some("zenith")));
        assert_ne!(parse_seed(Some("zenith")), parse_seed(Some("horizon")));
    }
}
