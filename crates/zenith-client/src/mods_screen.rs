//! The Mods pages of the title screen: what is installed, where to find more, and adding somewhere
//! to look. Fetching runs on a thread of its own, so the screen keeps drawing while the network works.
use crate::hud::{Mouse, button};
use crate::render::ui::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use zenith_mods::{CatalogueEntry, EntryState, Installed, Library, Listing, Source};

const ERROR: Rgba = [255, 128, 116, 255];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModsPage {
    Installed,
    Browse,
    AddSource,
}

enum Done {
    Listed(Source, Box<Result<Listing, String>>),
    Installed(Result<String, String>),
}

pub struct ModsScreen {
    home: PathBuf,
    library: Option<Library>,
    installed: Vec<Installed>,
    pub page: ModsPage,
    /// The source the Browse page shows.
    selected: usize,
    listings: Vec<(Source, Result<Listing, String>)>,
    busy: Option<(String, Receiver<Done>)>,
    /// The last thing worth saying: what was done, or what went wrong.
    note: Option<(String, bool)>,
    pub url: String,
    scroll: usize,
    /// The address field was tapped and is being typed in; see `TitleScreen::typing`.
    pub typing: bool,
    pub tapped: bool,
}

/// Text cut to fit a width, with an ellipsis if anything was cut.
fn fit(ui: &UiBatch, text: &str, width: f32) -> String {
    if ui.text_width(text, 1.0) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    for c in text.chars() {
        if ui.text_width(&format!("{out}{c}..."), 1.0) > width {
            break;
        }
        out.push(c);
    }
    format!("{out}...")
}

/// The end of a text that fits a width: the part being typed is the part that shows.
fn tail(ui: &UiBatch, text: &str, width: f32) -> String {
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        let rest: String = chars[start..].iter().collect();
        if ui.text_width(&rest, 1.0) <= width {
            return rest;
        }
    }
    String::new()
}

/// Opens a folder in the system's file browser.
fn show_folder(path: &Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(path).spawn();
}

impl ModsScreen {
    pub fn new(home: PathBuf) -> Self {
        let mut screen = Self {
            home,
            library: None,
            installed: Vec::new(),
            page: ModsPage::Installed,
            selected: 0,
            listings: Vec::new(),
            busy: None,
            note: None,
            url: String::new(),
            scroll: 0,
            typing: false,
            tapped: false,
        };
        screen.reload();
        screen
    }

    fn reload(&mut self) {
        match Library::open(&self.home) {
            Ok(library) => {
                self.installed = library.installed();
                self.library = Some(library);
            }
            Err(e) => {
                self.installed.clear();
                self.library = None;
                self.note = Some((e, true));
            }
        }
    }

    /// Comes to the first page, with the library as it is now on disk.
    pub fn open(&mut self) {
        self.page = ModsPage::Installed;
        self.note = None;
        self.scroll = 0;
        self.reload();
    }

    pub fn screen_name(&self) -> &'static str {
        match self.page {
            ModsPage::Installed => "title/mods",
            ModsPage::Browse => "title/mods/browse",
            ModsPage::AddSource => "title/mods/add_source",
        }
    }

    pub fn type_char(&mut self, c: char) {
        if self.page == ModsPage::AddSource && !c.is_control() && self.url.len() < 1024 {
            self.url.push(c);
        }
    }

    pub fn backspace(&mut self) {
        if self.page == ModsPage::AddSource {
            self.url.pop();
        }
    }

    /// Up a level, as Escape does. True when that leaves the Mods pages altogether.
    pub fn back(&mut self) -> bool {
        match self.page {
            ModsPage::Installed => true,
            ModsPage::Browse => {
                self.open();
                false
            }
            ModsPage::AddSource => {
                self.page = ModsPage::Browse;
                false
            }
        }
    }

    fn poll(&mut self) {
        let Some((_, rx)) = &self.busy else { return };
        match rx.try_recv() {
            Ok(Done::Listed(source, result)) => {
                if let Err(e) = result.as_ref() {
                    self.note = Some((e.clone(), true));
                }
                self.listings.retain(|(s, _)| *s != source);
                self.listings.push((source, *result));
                self.busy = None;
            }
            Ok(Done::Installed(result)) => {
                self.note = Some(match result {
                    Ok(name) => (format!("Installed {name}. It is on, in every world you open."), false),
                    Err(e) => (e, true),
                });
                self.busy = None;
                self.reload();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.busy = None;
                self.note = Some(("That stopped before it finished.".into(), true));
            }
        }
    }

    fn look_in(&mut self, index: usize) {
        let Some(library) = &self.library else { return };
        let Some(source) = library.sources().get(index).cloned() else { return };
        self.selected = index;
        self.scroll = 0;
        if self.busy.is_some() {
            return;
        }
        self.note = None;
        let cache = library.cache();
        let label = source.label();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = Listing::fetch(&source, &cache, &zenith_fetch::Curl);
            let _ = tx.send(Done::Listed(source, Box::new(result)));
        });
        self.busy = Some((format!("Looking in {label}..."), rx));
    }

    fn install(&mut self, listing: Listing, entry: CatalogueEntry) {
        if self.busy.is_some() {
            return;
        }
        let home = self.home.clone();
        let label = format!("Installing {}...", entry.name);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result =
                Library::open(&home).and_then(|l| l.install(&listing, &entry, &zenith_fetch::Curl)).map(|m| m.name);
            let _ = tx.send(Done::Installed(result));
        });
        self.busy = Some((label, rx));
    }

    pub fn draw(&mut self, ui: &mut UiBatch, m: Mouse, blink: bool) -> bool {
        self.poll();
        match self.page {
            ModsPage::Installed => self.draw_installed(ui, m),
            ModsPage::Browse => {
                self.draw_browse(ui, m);
                false
            }
            ModsPage::AddSource => {
                self.draw_add(ui, m, blink);
                false
            }
        }
    }

    /// The note, or what is happening, on one line.
    fn draw_status(&self, ui: &mut UiBatch, x: f32, y: f32, width: f32) {
        if let Some((what, _)) = &self.busy {
            ui.text(x, y, &fit(ui, what, width), 1.0, ACCENT);
        } else if let Some((text, is_error)) = &self.note {
            ui.text(x, y, &fit(ui, text, width), 1.0, if *is_error { ERROR } else { ACCENT });
        }
    }

    /// Returns true when the player asked to leave.
    fn draw_installed(&mut self, ui: &mut UiBatch, m: Mouse) -> bool {
        let (w, h) = (ui.width, ui.height);
        let (pw, ph) = ((w - 20.0).min(440.0), (h - 30.0).min(300.0));
        let (x0, y0) = (((w - pw) / 2.0).floor(), ((h - ph) / 2.0).floor());
        ui.panel(x0, y0, pw, ph);
        ui.text_centered(w / 2.0, y0 + 7.0, "Mods", 1.0, WHITE);
        ui.text(
            x0 + 10.0,
            y0 + 20.0,
            &fit(ui, "Mods that are on are in every world you open.", pw - 20.0),
            1.0,
            TEXT_DIM,
        );

        let top = y0 + 34.0;
        let row = 20.0;
        let rows = (((ph - 34.0 - 52.0) / row).floor() as usize).max(1);
        self.scroll = self.scroll.min(self.installed.len().saturating_sub(rows));
        if self.installed.is_empty() {
            ui.text(x0 + 10.0, top + 5.0, "None yet: Get mods finds some.", 1.0, TEXT_DIM);
        }
        let mut toggle = None;
        let mut remove = None;
        for (i, item) in self.installed.iter().enumerate().skip(self.scroll).take(rows) {
            let y = top + (i - self.scroll) as f32 * row;
            let mf = &item.manifest;
            let origin = mf.source.as_ref().map_or("the mods folder".to_string(), Source::label);
            let label = fit(ui, &format!("{}  {}", mf.name, mf.version), pw - 260.0);
            ui.text(x0 + 10.0, y + 6.0, &label, 1.0, if item.enabled { WHITE } else { TEXT_DIM });
            let from = fit(ui, &format!("from {origin}"), 110.0);
            ui.text(x0 + pw - 240.0, y + 6.0, &from, 1.0, TEXT_DIM);
            if button(ui, m, x0 + pw - 124.0, y + 1.0, 48.0, 18.0, if item.enabled { "On" } else { "Off" }) {
                toggle = Some((mf.id.clone(), !item.enabled));
            }
            if button(ui, m, x0 + pw - 72.0, y + 1.0, 62.0, 18.0, "Remove") {
                remove = Some((mf.id.clone(), mf.name.clone()));
            }
        }
        if self.installed.len() > rows {
            let x = x0 + pw - 8.0 - 10.0;
            if self.scroll > 0 && button(ui, m, x, top - 14.0, 10.0, 12.0, "^") {
                self.scroll -= 1;
            }
            if self.scroll + rows < self.installed.len() && button(ui, m, x, top + rows as f32 * row, 10.0, 12.0, "v") {
                self.scroll += 1;
            }
        }
        if let (Some((id, on)), Some(library)) = (toggle, self.library.as_mut()) {
            if let Err(e) = library.set_enabled(&id, on) {
                self.note = Some((e, true));
            }
            self.reload();
        }
        if let (Some((id, name)), Some(library)) = (remove, self.library.as_mut()) {
            self.note = Some(match library.remove(&id) {
                Ok(()) => (format!("Removed {name}. Its blocks show as Missing in worlds that had them."), false),
                Err(e) => (e, true),
            });
            self.reload();
        }

        self.draw_status(ui, x0 + 10.0, y0 + ph - 42.0, pw - 20.0);
        let bw = ((pw - 28.0) / 3.0).floor();
        let by = y0 + ph - 26.0;
        if button(ui, m, x0 + 8.0, by, bw, 18.0, "Get mods") {
            self.page = ModsPage::Browse;
            self.note = None;
            let index = self.selected;
            if !self.listings.iter().any(|(s, _)| self.library.as_ref().and_then(|l| l.sources().get(index)) == Some(s))
            {
                self.look_in(index);
            }
        }
        if button(ui, m, x0 + 14.0 + bw, by, bw, 18.0, "Open the folder")
            && let Some(library) = &self.library
        {
            show_folder(library.dir());
        }
        button(ui, m, x0 + 20.0 + bw * 2.0, by, bw, 18.0, "Back")
    }

    fn draw_browse(&mut self, ui: &mut UiBatch, m: Mouse) {
        let (w, h) = (ui.width, ui.height);
        let (pw, ph) = ((w - 20.0).min(620.0), (h - 24.0).min(320.0));
        let (x0, y0) = (((w - pw) / 2.0).floor(), ((h - ph) / 2.0).floor());
        ui.panel(x0, y0, pw, ph);
        ui.text_centered(w / 2.0, y0 + 7.0, "Get mods", 1.0, WHITE);

        // Where to look.
        let sources: Vec<Source> = self.library.as_ref().map(|l| l.sources().to_vec()).unwrap_or_default();
        self.selected = self.selected.min(sources.len().saturating_sub(1));
        let lw = 150.0;
        ui.text(x0 + 10.0, y0 + 22.0, "Look in", 1.0, TEXT_DIM);
        let mut y = y0 + 34.0;
        let mut pick = None;
        for (i, source) in sources.iter().enumerate() {
            if y > y0 + ph - 74.0 {
                break;
            }
            if i == self.selected {
                ui.rect(x0 + 6.0, y, 2.0, 18.0, ACCENT);
            }
            if button(ui, m, x0 + 10.0, y, lw, 18.0, &fit(ui, &source.label(), lw - 8.0)) {
                pick = Some(i);
            }
            y += 21.0;
        }
        if let Some(i) = pick {
            self.look_in(i);
        }
        if button(ui, m, x0 + 10.0, y0 + ph - 72.0, lw, 18.0, "Add a source") {
            self.page = ModsPage::AddSource;
            self.url.clear();
            self.note = None;
        }
        let forgettable = sources.get(self.selected).is_some_and(|s| *s != Source::Examples);
        if forgettable
            && button(ui, m, x0 + 10.0, y0 + ph - 50.0, lw, 18.0, "Forget this source")
            && let Some(library) = self.library.as_mut()
        {
            let gone = sources[self.selected].clone();
            if let Err(e) = library.remove_source(self.selected) {
                self.note = Some((e, true));
            }
            self.listings.retain(|(s, _)| *s != gone);
            self.selected = 0;
        }

        // What is there.
        let (x1, rw) = (x0 + lw + 22.0, pw - lw - 32.0);
        let selected = sources.get(self.selected).cloned();
        let listing =
            selected.as_ref().and_then(|s| self.listings.iter().find(|(l, _)| l == s)).map(|(_, r)| r.clone());
        if let (None, Some(_), None) = (&listing, &selected, &self.busy) {
            self.look_in(self.selected);
        }
        self.draw_status(ui, x1, y0 + 22.0, rw);
        let mut install = None;
        if let Some(Ok(listing)) = &listing {
            let top = y0 + 36.0;
            ui.text(x1, top, &fit(ui, &listing.catalogue.name, rw), 1.0, WHITE);
            ui.text(x1, top + 10.0, &fit(ui, &listing.catalogue.description, rw), 1.0, TEXT_DIM);
            let row = 32.0;
            let list_top = top + 24.0;
            let rows = (((y0 + ph - 32.0 - list_top) / row).floor() as usize).max(1);
            let entries = &listing.catalogue.mods;
            self.scroll = self.scroll.min(entries.len().saturating_sub(rows));
            for (i, entry) in entries.iter().enumerate().skip(self.scroll).take(rows) {
                let y = list_top + (i - self.scroll) as f32 * row;
                ui.rect(x1, y, rw, row - 3.0, [24, 26, 36, 200]);
                let by_who = if entry.author.is_empty() { String::new() } else { format!("  by {}", entry.author) };
                ui.text(
                    x1 + 6.0,
                    y + 5.0,
                    &fit(ui, &format!("{}  {}{by_who}", entry.name, entry.version), rw - 90.0),
                    1.0,
                    WHITE,
                );
                ui.text(x1 + 6.0, y + 16.0, &fit(ui, &entry.description, rw - 90.0), 1.0, TEXT_DIM);
                let state = self.library.as_ref().map_or(EntryState::NotInstalled, |l| l.state_of(entry));
                let bx = x1 + rw - 76.0;
                match state {
                    EntryState::Installed => ui.text(bx + 8.0, y + 10.0, "Installed", 1.0, TEXT_DIM),
                    EntryState::NotInstalled => {
                        if button(ui, m, bx, y + 5.0, 70.0, 18.0, "Install") {
                            install = Some(entry.clone());
                        }
                    }
                    EntryState::Update(_) => {
                        if button(ui, m, bx, y + 5.0, 70.0, 18.0, "Update") {
                            install = Some(entry.clone());
                        }
                    }
                }
            }
            if entries.len() > rows {
                if self.scroll > 0 && button(ui, m, x1 + rw - 12.0, list_top - 13.0, 12.0, 12.0, "^") {
                    self.scroll -= 1;
                }
                if self.scroll + rows < entries.len()
                    && button(ui, m, x1 + rw - 12.0, list_top + rows as f32 * row, 12.0, 12.0, "v")
                {
                    self.scroll += 1;
                }
            }
            if let Some(entry) = install {
                self.install(listing.clone(), entry);
            }
        }
        if button(ui, m, x0 + pw - 98.0, y0 + ph - 26.0, 90.0, 18.0, "Back") {
            self.open();
        }
    }

    fn draw_add(&mut self, ui: &mut UiBatch, m: Mouse, blink: bool) {
        let (w, h) = (ui.width, ui.height);
        let (pw, ph) = ((w - 20.0).min(440.0), 156.0);
        let (x0, y0) = (((w - pw) / 2.0).floor(), crate::title::panel_top(h, ph, self.typing.then_some(22.0)));
        ui.panel(x0, y0, pw, ph);
        ui.text_centered(w / 2.0, y0 + 7.0, "Add a source", 1.0, WHITE);
        let (fx, fw) = (x0 + 10.0, pw - 20.0);
        if m.left {
            self.typing = m.x >= fx && m.x < fx + fw && m.y >= y0 + 32.0 && m.y < y0 + 48.0;
            self.tapped |= self.typing;
        }
        ui.text(fx, y0 + 22.0, "A catalogue's address, or a Git repository", 1.0, TEXT_DIM);
        ui.rect(fx, y0 + 32.0, fw, 16.0, [14, 16, 24, 250]);
        ui.rect(fx, y0 + 47.0, fw, 1.0, ACCENT);
        let shown = format!("{}{}", self.url, if blink { "_" } else { "" });
        ui.text(fx + 4.0, y0 + 37.0, &tail(ui, &shown, fw - 8.0), 1.0, WHITE);
        ui.text(fx, y0 + 56.0, &fit(ui, "A catalogue is a zenith-catalogue.json file on the web.", fw), 1.0, TEXT_DIM);
        ui.text(
            fx,
            y0 + 66.0,
            &fit(ui, "A Git repository needs one at its top. A folder works too.", fw),
            1.0,
            TEXT_DIM,
        );
        ui.text(fx, y0 + 76.0, &fit(ui, "Ctrl+V pastes.", fw), 1.0, TEXT_DIM);
        self.draw_status(ui, fx, y0 + 96.0, fw);
        let bw = ((pw - 26.0) / 2.0).floor();
        let by = y0 + ph - 26.0;
        if button(ui, m, x0 + 8.0, by, bw, 18.0, "Add") {
            match Source::from_text(&self.url) {
                Ok(source) => {
                    if let Some(library) = self.library.as_mut() {
                        match library.add_source(source.clone()) {
                            Ok(()) => {
                                let index = library.sources().iter().position(|s| *s == source).unwrap_or(0);
                                self.page = ModsPage::Browse;
                                self.listings.retain(|(s, _)| *s != source);
                                self.look_in(index);
                            }
                            Err(e) => self.note = Some((e, true)),
                        }
                    }
                }
                Err(e) => self.note = Some((e, true)),
            }
        }
        if button(ui, m, x0 + 18.0 + bw, by, bw, 18.0, "Back") {
            self.page = ModsPage::Browse;
            self.note = None;
        }
    }
}
