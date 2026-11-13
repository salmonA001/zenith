//! The window, and the loop that runs everything.
use crate::game::{Game, Held, Overlay, title_generator};
use crate::hud::Mouse;
use crate::options::{Look, Options, home};
use crate::render::ui::UiBatch;
use crate::render::{Camera, Frame, Renderer, new_instance, open_device};
use crate::title::{Action, Page, TitleScreen, parse_seed};
use glam::DVec3;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};
use zenith_agent::control::{self, ControlServer};
use zenith_protocol::{C2S, GameMode};

/// What the command line asked for.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub world: Option<String>,
    pub seed: Option<String>,
    pub creative: bool,
    pub connect: Option<String>,
    pub name: Option<String>,
    pub control: Option<PathBuf>,
    pub render_distance: Option<i32>,
    pub offscreen: bool,
    pub width: u32,
    pub height: u32,
    /// Draw as fast as the GPU can: no waiting for the display, for measuring.
    pub uncapped: bool,
    /// Turn ray tracing on, as the option in the game does.
    pub ray_tracing: bool,
    /// Draw the world this way, as the option in the game does.
    pub look: Option<Look>,
}

/// A key going down or up, from a keyboard or from a command.
#[derive(Clone, Debug, Default)]
pub struct KeyInput {
    pub code: Option<KeyCode>,
    pub named: Option<NamedKey>,
    pub text: Option<String>,
    pub pressed: bool,
    pub repeat: bool,
}

impl KeyInput {
    fn from_event(event: &KeyEvent) -> Self {
        let code = match event.physical_key {
            PhysicalKey::Code(c) => Some(c),
            PhysicalKey::Unidentified(_) => None,
        };
        let (named, text) = match &event.logical_key {
            Key::Named(n) => (Some(*n), None),
            // Android names these keys only by the character they type.
            Key::Character(s) if s == "\n" || s == "\r" => (Some(NamedKey::Enter), None),
            Key::Character(s) if s == "\t" => (Some(NamedKey::Tab), None),
            Key::Character(s) => (None, Some(s.to_string())),
            _ => (None, None),
        };
        Self { code, named, text, pressed: event.state == ElementState::Pressed, repeat: event.repeat }
    }

    /// A key by the name a person would give it.
    pub fn by_name(name: &str, pressed: bool) -> Option<Self> {
        const LETTERS: [KeyCode; 26] = [
            KeyCode::KeyA,
            KeyCode::KeyB,
            KeyCode::KeyC,
            KeyCode::KeyD,
            KeyCode::KeyE,
            KeyCode::KeyF,
            KeyCode::KeyG,
            KeyCode::KeyH,
            KeyCode::KeyI,
            KeyCode::KeyJ,
            KeyCode::KeyK,
            KeyCode::KeyL,
            KeyCode::KeyM,
            KeyCode::KeyN,
            KeyCode::KeyO,
            KeyCode::KeyP,
            KeyCode::KeyQ,
            KeyCode::KeyR,
            KeyCode::KeyS,
            KeyCode::KeyT,
            KeyCode::KeyU,
            KeyCode::KeyV,
            KeyCode::KeyW,
            KeyCode::KeyX,
            KeyCode::KeyY,
            KeyCode::KeyZ,
        ];
        const DIGITS: [KeyCode; 10] = [
            KeyCode::Digit0,
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
        ];
        const FUNCTION: [(KeyCode, NamedKey); 12] = [
            (KeyCode::F1, NamedKey::F1),
            (KeyCode::F2, NamedKey::F2),
            (KeyCode::F3, NamedKey::F3),
            (KeyCode::F4, NamedKey::F4),
            (KeyCode::F5, NamedKey::F5),
            (KeyCode::F6, NamedKey::F6),
            (KeyCode::F7, NamedKey::F7),
            (KeyCode::F8, NamedKey::F8),
            (KeyCode::F9, NamedKey::F9),
            (KeyCode::F10, NamedKey::F10),
            (KeyCode::F11, NamedKey::F11),
            (KeyCode::F12, NamedKey::F12),
        ];
        let name = name.to_ascii_lowercase();
        let printable = |code: KeyCode, c: &str| Self {
            code: Some(code),
            text: Some(c.to_string()),
            pressed,
            ..Default::default()
        };
        let named =
            |code: KeyCode, n: NamedKey| Self { code: Some(code), named: Some(n), pressed, ..Default::default() };
        let mut chars = name.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_lowercase() {
                return Some(printable(LETTERS[(c as u8 - b'a') as usize], &name));
            }
            if c.is_ascii_digit() {
                return Some(printable(DIGITS[(c as u8 - b'0') as usize], &name));
            }
            if c == '/' {
                return Some(printable(KeyCode::Slash, "/"));
            }
        }
        if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<usize>().ok())
            && (1..=12).contains(&n)
        {
            let (code, key) = FUNCTION[n - 1];
            return Some(named(code, key));
        }
        Some(match name.as_str() {
            "space" => named(KeyCode::Space, NamedKey::Space),
            "enter" | "return" => named(KeyCode::Enter, NamedKey::Enter),
            "escape" | "esc" => named(KeyCode::Escape, NamedKey::Escape),
            "backspace" => named(KeyCode::Backspace, NamedKey::Backspace),
            "shift" => named(KeyCode::ShiftLeft, NamedKey::Shift),
            "ctrl" | "control" => named(KeyCode::ControlLeft, NamedKey::Control),
            "slash" => printable(KeyCode::Slash, "/"),
            _ => return None,
        })
    }
}

/// Which of the keys that are held down this one is.
fn set_held(held: &mut Held, code: KeyCode, down: bool) {
    match code {
        KeyCode::KeyW => held.forward = down,
        KeyCode::KeyS => held.back = down,
        KeyCode::KeyA => held.left = down,
        KeyCode::KeyD => held.right = down,
        KeyCode::Space => held.jump = down,
        KeyCode::ShiftLeft => held.sneak = down,
        KeyCode::ControlLeft => held.sprint = down,
        _ => {}
    }
}

/// How long the last few frames took.
#[derive(Default)]
struct FrameTimes {
    /// Between frames, in milliseconds, including any wait for the display.
    gaps: std::collections::VecDeque<f32>,
    /// Spent on the CPU making each frame.
    work: std::collections::VecDeque<f32>,
}

impl FrameTimes {
    const KEEP: usize = 240;

    fn add(&mut self, gap: f32, work: f32) {
        for (list, value) in [(&mut self.gaps, gap), (&mut self.work, work)] {
            list.push_back(value);
            if list.len() > Self::KEEP {
                list.pop_front();
            }
        }
    }

    /// The average, the 99th percentile and the worst.
    fn summary(list: &std::collections::VecDeque<f32>) -> (f32, f32, f32) {
        if list.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let mut sorted: Vec<f32> = list.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        let high = sorted[((sorted.len() - 1) * 99) / 100];
        (sorted.iter().sum::<f32>() / sorted.len() as f32, high, sorted[sorted.len() - 1])
    }
}

struct Screen {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
}

enum Mode {
    Title(Box<TitleScreen>),
    Playing(Box<Game>),
}

/// Everything that exists once a GPU is open.
struct Running {
    renderer: Renderer,
    ui: UiBatch,
    mode: Mode,
    home: PathBuf,
    options: Options,
    title_seconds: f32,
    last_frame: Instant,
    mouse_pos: (f32, f32),
    clicked_left: bool,
    clicked_right: bool,
    clicked_shift: bool,
    shift: bool,
    ctrl: bool,
    control: Option<ControlServer>,
    control_path: Option<PathBuf>,
    screenshots: Vec<(PathBuf, Sender<Value>)>,
    times: FrameTimes,
    quit: bool,
    touch: crate::touch::Touch,
}

impl Running {
    /// Whether someone is typing: in a field they tapped (a world's name or seed, an address, a mod
    /// source), or in chat.
    #[cfg(target_os = "android")]
    fn wants_text(&self) -> bool {
        match &self.mode {
            Mode::Title(t) => match t.page {
                Page::NewWorld | Page::Join => t.typing,
                Page::Mods => t.mods.typing && t.mods.screen_name() == "title/mods/add_source",
                _ => false,
            },
            Mode::Playing(g) => matches!(g.overlay, Overlay::Chat(_) | Overlay::Sign { .. }),
        }
    }

    /// Whether a text field was tapped since the last call.
    #[cfg(target_os = "android")]
    fn take_text_tap(&mut self) -> bool {
        match &mut self.mode {
            Mode::Title(t) => std::mem::take(&mut t.tapped) | std::mem::take(&mut t.mods.tapped),
            Mode::Playing(_) => false,
        }
    }

    /// A finger on the screen: the touch controls while the world is showing, a click on any menu.
    fn on_touch(&mut self, id: u64, phase: winit::event::TouchPhase, at: (f32, f32)) {
        let (w, h) = self.renderer.size();
        match &mut self.mode {
            Mode::Playing(game) if game.overlay == Overlay::None => {
                self.touch.on_world(game, id, phase, at, (w as f32, h as f32), self.ui.scale);
            }
            _ => {
                self.mouse_pos = at;
                if phase == winit::event::TouchPhase::Started {
                    self.clicked_left = true;
                }
            }
        }
    }

    fn new(renderer: Renderer, launch: &Launch) -> Self {
        let home = home();
        let mut options = Options::load(&home);
        if let Some(name) = &launch.name {
            options.name.clone_from(name);
        }
        if let Some(rd) = launch.render_distance {
            options.render_distance = rd.clamp(2, 64);
        }
        if launch.ray_tracing {
            options.ray_tracing = true;
        }
        if let Some(look) = launch.look {
            options.look = look;
        }
        let ui = renderer.new_ui_batch();
        let title = TitleScreen::new(home.clone(), options.name.clone(), options.last_server.clone());
        let mut running = Self {
            renderer,
            ui,
            mode: Mode::Title(Box::new(title)),
            home,
            options,
            title_seconds: 0.0,
            last_frame: Instant::now(),
            mouse_pos: (0.0, 0.0),
            clicked_left: false,
            clicked_right: false,
            clicked_shift: false,
            shift: false,
            ctrl: false,
            control: None,
            control_path: launch.control.clone(),
            screenshots: Vec::new(),
            times: FrameTimes::default(),
            quit: false,
            touch: Default::default(),
        };
        running.renderer.set_world(title_generator(), 2);
        if let Some(path) = &launch.control {
            match control::start(path) {
                Ok(c) => {
                    eprintln!("zenith: control socket on 127.0.0.1:{} (details in {})", c.port, path.display());
                    running.control = Some(c);
                }
                Err(e) => eprintln!("zenith: could not start the control socket: {e}"),
            }
        }
        if let Some(address) = &launch.connect {
            running.open_join(address.clone());
        } else if let Some(world) = &launch.world {
            let dir =
                if world.contains(['/', '\\']) { PathBuf::from(world) } else { running.home.join("saves").join(world) };
            let mode = if launch.creative { GameMode::Creative } else { GameMode::Survival };
            running.open_world(dir, parse_seed(launch.seed.as_deref()), mode);
        }
        running
    }

    fn open_world(&mut self, dir: PathBuf, seed: u64, mode: GameMode) {
        if let Mode::Title(t) = &self.mode {
            self.options.name.clone_from(&t.player_name);
        }
        self.options.save(&self.home);
        let mods = match zenith_mods::Library::open(&self.home).and_then(|l| l.enabled_packs()) {
            Ok(packs) => packs,
            Err(e) => return self.show_message(format!("A mod could not be read: {e}")),
        };
        match Game::single_player(dir, seed, mode, self.options.clone(), &mut self.renderer, mods) {
            Ok(game) => self.start_playing(game),
            Err(e) => self.show_message(e),
        }
    }

    fn open_join(&mut self, address: String) {
        if let Mode::Title(t) = &self.mode {
            self.options.name.clone_from(&t.player_name);
        }
        self.options.last_server.clone_from(&address);
        self.options.save(&self.home);
        match Game::multiplayer(&address, None, self.options.clone(), &mut self.renderer) {
            Ok(game) => self.start_playing(game),
            Err(e) => self.show_message(e),
        }
    }

    fn start_playing(&mut self, game: Game) {
        // Joining may have brought new textures, and so new places for the icons.
        self.ui = self.renderer.new_ui_batch();
        self.mode = Mode::Playing(Box::new(game));
    }

    fn show_message(&mut self, message: String) {
        eprintln!("zenith: {message}");
        match &mut self.mode {
            Mode::Title(t) => t.page = Page::Message(message),
            Mode::Playing(_) => {
                let mut t =
                    TitleScreen::new(self.home.clone(), self.options.name.clone(), self.options.last_server.clone());
                t.page = Page::Message(message);
                self.mode = Mode::Title(Box::new(t));
                self.renderer.set_world(title_generator(), 2);
            }
        }
    }

    fn leave_game(&mut self, message: Option<String>) {
        if let Mode::Playing(g) = &self.mode {
            self.options = g.options.clone();
            self.options.save(&self.home);
        }
        // Dropping the game stops its server, which saves the world.
        let mut t = TitleScreen::new(self.home.clone(), self.options.name.clone(), self.options.last_server.clone());
        if let Some(m) = message {
            t.page = Page::Message(m);
        }
        self.mode = Mode::Title(Box::new(t));
        self.renderer.set_world(title_generator(), 2);
    }

    fn handle_control(&mut self) {
        let Some(control) = &self.control else { return };
        let requests: Vec<_> = control.requests.try_iter().collect();
        for req in requests {
            let flag = |name: &str| req.args.get(name).and_then(Value::as_bool).unwrap_or(false);
            let text = |name: &str| req.args.get(name).and_then(Value::as_str).map(str::to_string);
            let number = |name: &str| req.args.get(name).and_then(Value::as_f64);
            let answer = |reply: Value| json!({"ok": true, "result": reply});
            let problem = |message: String| json!({"ok": false, "error": message});
            match req.cmd.as_str() {
                "screenshot" => match text("path") {
                    Some(path) => self.screenshots.push((PathBuf::from(path), req.reply)),
                    None => {
                        let _ = req.reply.send(problem("screenshot needs path".into()));
                    }
                },
                "frame" => {
                    let _ = req.reply.send(answer(self.frame_report()));
                }
                "key" => {
                    let name = text("name").unwrap_or_default();
                    let reply = match KeyInput::by_name(&name, !flag("release")) {
                        Some(key) => {
                            self.on_key(&key, None);
                            if !flag("hold") && !flag("release") {
                                self.on_key(&KeyInput { pressed: false, ..key }, None);
                            }
                            answer(self.frame_report())
                        }
                        None => problem(format!("no key called {name}")),
                    };
                    let _ = req.reply.send(reply);
                }
                "type" => {
                    for c in text("text").unwrap_or_default().chars() {
                        let key = KeyInput { text: Some(c.to_string()), pressed: true, ..Default::default() };
                        self.on_key(&key, None);
                    }
                    let _ = req.reply.send(answer(self.frame_report()));
                }
                "mouse" => {
                    self.mouse_pos = (number("x").unwrap_or(0.0) as f32, number("y").unwrap_or(0.0) as f32);
                    self.clicked_left |= flag("left");
                    self.clicked_right |= flag("right");
                    self.clicked_shift |= flag("shift");
                    let _ = req.reply.send(answer(self.frame_report()));
                }
                _ => match &mut self.mode {
                    Mode::Playing(game) => game.agent.command(&req.cmd, &req.args, req.reply),
                    Mode::Title(_) if req.cmd == "quit" => {
                        self.quit = true;
                        let _ = req.reply.send(answer(json!({"quit": true})));
                    }
                    Mode::Title(_) => {
                        let _ = req.reply.send(problem("no world is open yet".into()));
                    }
                },
            }
        }
    }

    /// Which screen is up, and how fast it is being drawn.
    fn frame_report(&self) -> Value {
        let screen = match &self.mode {
            Mode::Title(t) => match &t.page {
                Page::Main => "title".to_string(),
                Page::Worlds => "title/worlds".to_string(),
                Page::NewWorld => "title/new_world".to_string(),
                Page::Join => "title/join".to_string(),
                Page::Mods => t.mods.screen_name().to_string(),
                Page::Message(m) => format!("title/message: {m}"),
            },
            Mode::Playing(g) => match &g.overlay {
                Overlay::None => "playing".to_string(),
                Overlay::Inventory => "inventory".to_string(),
                Overlay::Pause => "pause".to_string(),
                Overlay::Debug => "debug".to_string(),
                Overlay::Sign { lines, .. } => format!("sign: {}", lines.join("|")),
                Overlay::Dead => "dead".to_string(),
                Overlay::Chat(text) => format!("chat: {text}"),
            },
        };
        let (gap_avg, gap_p99, gap_max) = FrameTimes::summary(&self.times.gaps);
        let (work_avg, work_p99, work_max) = FrameTimes::summary(&self.times.work);
        let (width, height) = self.renderer.size();
        let st = self.renderer.stats;
        json!({
            "screen": screen,
            "width": width,
            "height": height,
            "adapter": self.renderer.adapter_name,
            "backend": self.renderer.backend,
            "fps": if gap_avg > 0.0 { 1000.0 / gap_avg } else { 0.0 },
            "frame_ms": {"average": gap_avg, "p99": gap_p99, "worst": gap_max},
            "cpu_ms": {"average": work_avg, "p99": work_p99, "worst": work_max},
            "frames_measured": self.times.gaps.len(),
            "chunks_pending": match &self.mode {
                Mode::Playing(g) => g.session().pending_chunks(),
                Mode::Title(_) => 0,
            },
            "chunks_loaded": match &self.mode {
                Mode::Playing(g) => g.session().loaded_chunks(),
                Mode::Title(_) => 0,
            },
            "chunks_wanted": match &self.mode {
                Mode::Playing(g) => g.session().wanted_chunks(),
                Mode::Title(_) => 0,
            },
            "meshes_pending": match &self.mode {
                Mode::Playing(g) => g.streamer.pending(),
                Mode::Title(_) => 0,
            },
            "chunks_meshed": match &self.mode {
                Mode::Playing(g) => g.streamer.meshed_chunks,
                Mode::Title(_) => 0,
            },
            "sections_on_gpu": st.sections,
            "sections_drawn": st.drawn,
            "sections_in_shadow": st.shadow_drawn,
            "quads": st.quads,
            "far_tiles": st.far_tiles,
            "far_pending": st.far_pending,
            "vertex_mb_used": st.arena_used_mb,
            "vertex_mb_capacity": st.arena_capacity_mb,
            "look": match &self.mode {
                Mode::Playing(g) => g.options.look.name(),
                Mode::Title(_) => self.options.look.name(),
            },
            "ray_tracing": self.renderer.tracing().describe(),
            "traced_sections": st.traced.sections,
            "traced_waiting": st.traced.waiting,
            "traced_triangles": st.traced.triangles,
            "debug_view": match &self.mode {
                Mode::Playing(g) => json!({
                    "graph": g.debug_view.graph,
                    "chunk_borders": g.debug_view.chunk_borders,
                    "hitboxes": g.debug_view.hitboxes,
                    "fog": g.debug_view.fog,
                    "clouds": g.debug_view.clouds,
                    "full_bright": g.debug_view.full_bright,
                }),
                Mode::Title(_) => Value::Null,
            },
        })
    }

    /// Runs a frame. Returns false when it is time to quit.
    fn frame(&mut self) -> bool {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f64().min(0.5);
        let gap = (now - self.last_frame).as_secs_f32() * 1000.0;
        self.last_frame = now;
        self.handle_control();
        let mouse = Mouse {
            x: self.mouse_pos.0,
            y: self.mouse_pos.1,
            left: self.clicked_left,
            right: self.clicked_right,
            shift: self.shift || self.clicked_shift,
        };
        self.clicked_left = false;
        self.clicked_right = false;
        self.clicked_shift = false;
        let mut keep_going = !self.quit;
        let mut action = Action::None;
        let mut leave = None;
        match &mut self.mode {
            Mode::Title(title) => {
                self.title_seconds += dt as f32;
                let t = self.title_seconds as f64;
                // A slow flight along a mountain range, late in the afternoon.
                let camera = Camera {
                    pos: DVec3::new(
                        t * 9.0 + 2000.0,
                        215.0 + (t * 0.05).sin() * 25.0,
                        1700.0 + (t * 0.04).sin() * 300.0,
                    ),
                    yaw_deg: 248.0 + (t * 0.03).sin() * 18.0,
                    pitch_deg: 5.0 + (t * 0.07).sin() * 2.0,
                    fov_deg: 72.0,
                };
                action = title.draw(&mut self.ui, mouse, self.renderer.size(), self.title_seconds);
                let frame = Frame {
                    camera,
                    time_of_day: 10400.0 + (t * 3.0) as f32,
                    rain: 0.0,
                    seconds: self.title_seconds,
                    under_water: false,
                    under_lava: false,
                    hurt_flash: 0.0,
                    dead: 0.0,
                    entities: &[],
                    alpha: &[],
                    view_model: &[],
                    ui: &self.ui,
                    draw_far: true,
                    ray_tracing: false,
                    classic: self.options.look == Look::Classic,
                    debug: Default::default(),
                };
                self.renderer.render(&frame);
            }
            Mode::Playing(game) => {
                game.record_frame(dt as f32 * 1000.0);
                // Fingers only steer the world: a screen over it lets go of all of them.
                if game.overlay == Overlay::None {
                    self.touch.update(game);
                } else {
                    self.touch.release(game);
                }
                let (w, h) = self.renderer.size();
                game.touch_view = self.touch.view((w as f32, h as f32));
                game.update(dt, &mut self.renderer);
                let r = &self.renderer;
                let st = r.stats;
                game.debug = crate::hud::DebugStats {
                    adapter: r.adapter_name.clone(),
                    backend: r.backend.clone(),
                    sections: st.sections,
                    drawn: st.drawn,
                    shadow: st.shadow_drawn,
                    quads: st.quads,
                    far: st.far_tiles,
                    far_pending: st.far_pending,
                    used: st.arena_used_mb,
                    capacity: st.arena_capacity_mb,
                    tracing: r.tracing(),
                    traced: st.traced,
                };
                game.draw(&mut self.renderer, &mut self.ui, mouse);
                if game.leave_to_title {
                    leave = Some(game.saved_message.take());
                }
                if game.quit {
                    keep_going = false;
                }
            }
        }
        for (path, reply) in self.screenshots.drain(..) {
            let answer = match self.renderer.screenshot(&path) {
                Ok(()) => json!({"ok": true, "result": {"path": path.display().to_string()}}),
                Err(e) => json!({"ok": false, "error": e}),
            };
            let _ = reply.send(answer);
        }
        if let Some(message) = leave {
            self.leave_game(message);
        }
        match action {
            Action::None => {}
            Action::Quit => keep_going = false,
            Action::Open(dir) => self.open_world(dir, 0, GameMode::Survival),
            Action::Create { dir, seed, mode } => self.open_world(dir, parse_seed(seed.as_deref()), mode),
            Action::Join(address) => self.open_join(address),
        }
        self.times.add(gap, now.elapsed().as_secs_f32() * 1000.0);
        keep_going
    }

    fn on_key(&mut self, key: &KeyInput, window: Option<&Window>) {
        // Android's back button backs out of whatever is open, as Escape does.
        let back = key.named == Some(NamedKey::BrowserBack) || key.code == Some(KeyCode::BrowserBack);
        let escape = KeyInput { code: Some(KeyCode::Escape), named: Some(NamedKey::Escape), ..key.clone() };
        let key = if back { &escape } else { key };
        let pressed = key.pressed;
        if matches!(key.code, Some(KeyCode::ShiftLeft | KeyCode::ShiftRight)) {
            self.shift = pressed;
        }
        if matches!(key.code, Some(KeyCode::ControlLeft | KeyCode::ControlRight)) {
            self.ctrl = pressed;
        }
        // With Ctrl down, letters are shortcuts, not text: Ctrl+V pastes, the rest do nothing. What
        // a paste types comes from no key, and goes through.
        if self.ctrl && key.text.is_some() && key.code.is_some() {
            if pressed && key.code == Some(KeyCode::KeyV) {
                let pasted = clipboard_text();
                let text: String = pasted.chars().filter(|c| !c.is_control()).take(1024).collect();
                self.on_key(&KeyInput { text: Some(text), pressed: true, ..Default::default() }, window);
            }
            return;
        }
        match &mut self.mode {
            Mode::Title(title) => {
                if pressed {
                    match key.named {
                        Some(NamedKey::Backspace) => title.backspace(),
                        Some(NamedKey::Space) => title.type_char(' '),
                        // A phone keyboard's Enter: done typing, which puts the keyboard away.
                        Some(NamedKey::Enter) => {
                            title.typing = false;
                            title.mods.typing = false;
                        }
                        Some(NamedKey::Escape) => title.back(),
                        _ => {}
                    }
                    for c in key.text.iter().flat_map(|s| s.chars()) {
                        title.type_char(c);
                    }
                }
            }
            Mode::Playing(game) => {
                // Letting go always counts, whatever has opened since the key went down, or it
                // would stay held for good.
                if let (false, Some(code)) = (pressed, key.code) {
                    set_held(&mut game.held, code, false);
                }
                // Writing on a sign: Enter goes down a line, and past the last one, or Escape, is done.
                if let Overlay::Sign { lines, line, .. } = &mut game.overlay {
                    let mut done = false;
                    if pressed {
                        let count = lines.len();
                        match key.named {
                            Some(NamedKey::Enter) if *line + 1 < count => *line += 1,
                            Some(NamedKey::Enter | NamedKey::Escape) => done = true,
                            Some(NamedKey::ArrowUp) => *line = line.saturating_sub(1),
                            Some(NamedKey::ArrowDown) => *line = (*line + 1).min(count - 1),
                            Some(NamedKey::Backspace) => {
                                lines[*line].pop();
                            }
                            named => {
                                let text = &mut lines[*line];
                                let typed = if named == Some(NamedKey::Space) {
                                    Some(" ".to_string())
                                } else {
                                    key.text.clone()
                                };
                                for c in typed.iter().flat_map(|s| s.chars()).filter(|c| !c.is_control()) {
                                    if text.chars().count() < crate::game::SIGN_LINE_LENGTH {
                                        text.push(c);
                                    }
                                }
                            }
                        }
                    }
                    if done {
                        game.close_overlay();
                    }
                    return;
                }
                // While typing, keys are text.
                if let Overlay::Chat(text) = &mut game.overlay {
                    if pressed {
                        match key.named {
                            Some(NamedKey::Enter) => {
                                let line = std::mem::take(text);
                                game.overlay = Overlay::None;
                                if !line.trim().is_empty() {
                                    game.agent.session.send(&C2S::Chat(line));
                                }
                            }
                            Some(NamedKey::Escape) => game.overlay = Overlay::None,
                            Some(NamedKey::Backspace) => {
                                text.pop();
                            }
                            Some(NamedKey::Space) => text.push(' '),
                            _ => {
                                if let Some(s) = key.text.as_deref().filter(|_| text.len() < 256) {
                                    text.push_str(s);
                                }
                            }
                        }
                    }
                    return;
                }
                let Some(code) = key.code else { return };
                if pressed {
                    set_held(&mut game.held, code, true);
                }
                if !pressed || key.repeat {
                    return;
                }
                match code {
                    KeyCode::Escape => match game.overlay {
                        // Out of bed first.
                        Overlay::None if game.session().sleeping => game.agent.session.send(&C2S::WakeUp),
                        Overlay::None => game.overlay = Overlay::Pause,
                        Overlay::Dead => {}
                        _ => game.close_overlay(),
                    },
                    KeyCode::KeyE => game.toggle_inventory(),
                    KeyCode::KeyT if game.overlay == Overlay::None => game.overlay = Overlay::Chat(String::new()),
                    KeyCode::Slash if game.overlay == Overlay::None => game.overlay = Overlay::Chat("/".to_string()),
                    KeyCode::KeyQ if game.overlay == Overlay::None => {
                        game.agent.session.send(&C2S::DropItem { all: false })
                    }
                    KeyCode::F1 => game.hide_hud = !game.hide_hud,
                    KeyCode::F2 => {
                        let stamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_secs());
                        let path = self.home.join("screenshots").join(format!("{stamp}.png"));
                        let (tx, _rx) = std::sync::mpsc::channel();
                        self.screenshots.push((path, tx));
                    }
                    KeyCode::F3 => game.show_debug = !game.show_debug,
                    KeyCode::F6 => match game.overlay {
                        Overlay::Debug => game.overlay = Overlay::None,
                        Overlay::None | Overlay::Pause => game.overlay = Overlay::Debug,
                        _ => {}
                    },
                    KeyCode::F11 => {
                        if let Some(w) = window {
                            let next = if w.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) };
                            w.set_fullscreen(next);
                        }
                    }
                    KeyCode::Digit1 => game.select_slot(0),
                    KeyCode::Digit2 => game.select_slot(1),
                    KeyCode::Digit3 => game.select_slot(2),
                    KeyCode::Digit4 => game.select_slot(3),
                    KeyCode::Digit5 => game.select_slot(4),
                    KeyCode::Digit6 => game.select_slot(5),
                    KeyCode::Digit7 => game.select_slot(6),
                    KeyCode::Digit8 => game.select_slot(7),
                    KeyCode::Digit9 => game.select_slot(8),
                    _ => {}
                }
            }
        }
    }

    /// Whether the pointer belongs to the game right now.
    fn wants_cursor_grabbed(&self) -> bool {
        matches!(&self.mode, Mode::Playing(g) if g.overlay == Overlay::None && !g.session().sleeping)
    }
}

/// What the system clipboard holds, where the game can read one.
fn clipboard_text() -> String {
    #[cfg(not(target_os = "android"))]
    return arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
    #[cfg(target_os = "android")]
    return String::new();
}

/// Sets a surface up to be drawn to, in `keep` when the renderer already draws in a format, else in
/// the first sRGB one the surface offers.
fn configure_surface(
    surface: &wgpu::Surface<'static>,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    size: PhysicalSize<u32>,
    keep: Option<wgpu::TextureFormat>,
    uncapped: bool,
) -> Result<wgpu::SurfaceConfiguration, String> {
    let caps = surface.get_capabilities(adapter);
    let format = match keep {
        Some(f) if caps.formats.contains(&f) => f,
        Some(_) => return Err("The window came back in a format the game cannot draw in.".into()),
        None => caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or(caps.formats.first().copied())
            .ok_or("The window offers no format to draw in.")?,
    };
    let mut config = surface
        .get_default_config(adapter, size.width.max(1), size.height.max(1))
        .ok_or("The window cannot be drawn to by this GPU.")?;
    config.format = format;
    config.present_mode = if uncapped { wgpu::PresentMode::AutoNoVsync } else { wgpu::PresentMode::AutoVsync };
    config.desired_maximum_frame_latency = 2;
    surface.configure(device, &config);
    Ok(config)
}

/// Runs the game in a window until it quits.
struct App {
    launch: Launch,
    screen: Option<Screen>,
    running: Option<Running>,
    /// Kept to make a new surface when the system takes the window away and gives another.
    gpu: Option<(wgpu::Instance, wgpu::Adapter)>,
    /// Android's on-screen keyboard is showing.
    #[cfg(target_os = "android")]
    keyboard: bool,
    grabbed: bool,
    focused: bool,
    /// Someone has clicked in the window since it last had to give up focus. Until then the pointer
    /// stays theirs: a game started by a script must never take it from a person doing something else.
    clicked_here: bool,
    failed: Option<String>,
}

impl App {
    fn sync_cursor(&mut self) {
        let (Some(screen), Some(running)) = (&self.screen, &self.running) else { return };
        let want = self.focused && self.clicked_here && running.wants_cursor_grabbed();
        if want == self.grabbed {
            return;
        }
        self.grabbed = want;
        if want {
            let _ = screen
                .window
                .set_cursor_grab(CursorGrabMode::Confined)
                .or_else(|_| screen.window.set_cursor_grab(CursorGrabMode::Locked));
            screen.window.set_cursor_visible(false);
        } else {
            let _ = screen.window.set_cursor_grab(CursorGrabMode::None);
            screen.window.set_cursor_visible(true);
        }
    }

    fn open_window(&self, event_loop: &ActiveEventLoop) -> Result<Arc<Window>, String> {
        const ICON: u32 = 64;
        let icon = winit::window::Icon::from_rgba(crate::icon::paint_icon(ICON), ICON, ICON).ok();
        let attrs = Window::default_attributes()
            .with_title("Zenith")
            .with_window_icon(icon)
            .with_inner_size(PhysicalSize::new(self.launch.width.max(320), self.launch.height.max(240)));
        Ok(Arc::new(event_loop.create_window(attrs).map_err(|e| format!("Could not open a window: {e}"))?))
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let window = self.open_window(event_loop)?;
        let instance = new_instance();
        let surface =
            instance.create_surface(window.clone()).map_err(|e| format!("Could not draw to the window: {e}"))?;
        let (adapter, device, queue) = open_device(&instance, Some(&surface))?;
        let config = configure_surface(&surface, &adapter, &device, window.inner_size(), None, self.launch.uncapped)?;
        let registry = zenith_content::Registry::builtin();
        let renderer =
            Renderer::new(&adapter, device, queue, &registry, Some(config.format), config.width, config.height);
        let running = Running::new(renderer, &self.launch);
        self.running = Some(running);
        self.gpu = Some((instance, adapter));
        self.screen = Some(Screen { window, surface, config });
        Ok(())
    }

    /// A window again, after the system took the last one away, as Android does whenever the game
    /// goes into the background. The device, the world and everything in it carry on as they were.
    fn reopen(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let window = self.open_window(event_loop)?;
        let (Some((instance, adapter)), Some(running)) = (&self.gpu, &mut self.running) else { return Ok(()) };
        let surface =
            instance.create_surface(window.clone()).map_err(|e| format!("Could not draw to the window: {e}"))?;
        let renderer = &mut running.renderer;
        let config = configure_surface(
            &surface,
            adapter,
            &renderer.device,
            window.inner_size(),
            renderer.surface_format(),
            self.launch.uncapped,
        )?;
        renderer.resize(config.width, config.height);
        self.screen = Some(Screen { window, surface, config });
        Ok(())
    }

    /// Whether frames should not wait for the display: asked for at launch, or chosen in the menu.
    fn wants_uncapped(&self) -> bool {
        self.launch.uncapped
            || self.running.as_ref().is_some_and(|r| match &r.mode {
                Mode::Playing(g) => !g.options.frame_cap,
                Mode::Title(_) => !r.options.frame_cap,
            })
    }

    /// Sets the window to wait for the display or not, when that choice has changed.
    fn sync_frame_cap(&mut self) {
        let mode = if self.wants_uncapped() { wgpu::PresentMode::AutoNoVsync } else { wgpu::PresentMode::AutoVsync };
        if let (Some(screen), Some(running)) = (&mut self.screen, &self.running)
            && screen.config.present_mode != mode
        {
            screen.config.present_mode = mode;
            screen.surface.configure(&running.renderer.device, &screen.config);
        }
    }

    /// Shows Android's on-screen keyboard while someone is typing, and only then. A tap on a field
    /// shows it again, in case Android put it away by itself.
    #[cfg(target_os = "android")]
    fn sync_keyboard(&mut self) {
        let (Some(screen), Some(running)) = (&self.screen, &mut self.running) else { return };
        let want = running.wants_text();
        let tapped = running.take_text_tap();
        if want != self.keyboard || (want && tapped) {
            screen.window.set_ime_allowed(want);
            self.keyboard = want;
        }
    }

    fn present(&mut self) -> bool {
        let (Some(screen), Some(running)) = (&mut self.screen, &mut self.running) else { return true };
        let keep = running.frame();
        match screen.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
                running.renderer.present_to(&view);
                running.renderer.queue.present(frame);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                screen.surface.configure(&running.renderer.device, &screen.config);
            }
            _ => {}
        }
        keep
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let opened = match (&self.running, &self.screen) {
            (None, _) => self.start(event_loop),
            (Some(_), None) => self.reopen(event_loop),
            (Some(_), Some(_)) => Ok(()),
        };
        if let Err(e) = opened {
            self.failed = Some(e);
            event_loop.exit();
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // The window is going away: on Android, the game is off the screen. What draws to it goes too,
        // and the game pauses rather than playing on unseen.
        self.screen = None;
        self.grabbed = false;
        #[cfg(target_os = "android")]
        {
            self.keyboard = false;
        }
        if let Some(running) = &mut self.running
            && let Mode::Playing(game) = &mut running.mode
        {
            game.held = Default::default();
            if game.overlay == Overlay::None {
                game.overlay = Overlay::Pause;
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let (Some(screen), Some(running)) = (&mut self.screen, &mut self.running) else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    screen.config.width = size.width;
                    screen.config.height = size.height;
                    screen.surface.configure(&running.renderer.device, &screen.config);
                    running.renderer.resize(size.width, size.height);
                }
            }
            WindowEvent::CursorMoved { position, .. } => running.mouse_pos = (position.x as f32, position.y as f32),
            WindowEvent::Touch(t) => {
                self.clicked_here = true;
                running.on_touch(t.id, t.phase, (t.location.x as f32, t.location.y as f32));
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if pressed {
                    match button {
                        MouseButton::Left => running.clicked_left = true,
                        MouseButton::Right => running.clicked_right = true,
                        _ => {}
                    }
                }
                let had_pointer = self.grabbed;
                self.clicked_here |= pressed;
                if let Mode::Playing(game) = &mut running.mode {
                    // A click on a menu, or one that takes the pointer back, is not also a swing.
                    let down = pressed && had_pointer && game.overlay == Overlay::None;
                    match button {
                        MouseButton::Left => game.held.attack = down,
                        MouseButton::Right => game.held.use_item = down,
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Mode::Playing(game) = &mut running.mode {
                    let by = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                    };
                    if by.abs() > 0.01 && game.overlay == Overlay::None {
                        game.scroll_slot(by.signum() as i32);
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let window = screen.window.clone();
                running.on_key(&KeyInput::from_event(&event), Some(&window));
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.clicked_here &= focused;
                // Let go of everything held, and pause rather than playing on unattended.
                if let (false, Mode::Playing(game)) = (focused, &mut running.mode) {
                    game.held = Default::default();
                    if game.overlay == Overlay::None {
                        game.overlay = Overlay::Pause;
                    }
                }
                self.sync_cursor();
            }
            WindowEvent::RedrawRequested => {
                self.sync_frame_cap();
                if !self.present() {
                    event_loop.exit();
                }
                self.sync_cursor();
                #[cfg(target_os = "android")]
                self.sync_keyboard();
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        // These arrive wherever the pointer is, so only count while the game has it.
        if let (DeviceEvent::MouseMotion { delta }, Some(running), true) = (event, &mut self.running, self.grabbed)
            && let Mode::Playing(game) = &mut running.mode
        {
            game.turn(delta.0, delta.1);
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(screen) = &self.screen {
            screen.window.request_redraw();
        }
    }
}

pub fn run_window(launch: Launch) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| format!("Could not start the window system: {e}"))?;
    run_in(event_loop, launch)
}

#[cfg(target_os = "android")]
pub use winit::platform::android::activity::AndroidApp;

/// Runs the game as an Android app, in the activity Android started.
#[cfg(target_os = "android")]
pub fn run_android(app: AndroidApp, launch: Launch) -> Result<(), String> {
    use winit::platform::android::EventLoopBuilderExtAndroid;
    let event_loop = EventLoop::builder()
        .with_android_app(app)
        .build()
        .map_err(|e| format!("Could not start the window system: {e}"))?;
    run_in(event_loop, launch)
}

fn run_in(event_loop: EventLoop<()>, launch: Launch) -> Result<(), String> {
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        launch,
        screen: None,
        running: None,
        gpu: None,
        #[cfg(target_os = "android")]
        keyboard: false,
        grabbed: false,
        focused: false,
        clicked_here: false,
        failed: None,
    };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    if let Some(control) = app.running.as_ref().and_then(|r| r.control.as_ref()) {
        control.settle(std::time::Duration::from_millis(500));
    }
    if let Some(e) = app.failed {
        return Err(e);
    }
    Ok(())
}

/// Runs with no window: pictures only exist when asked for. For agents, tests and benchmarks.
pub fn run_offscreen(launch: Launch) -> Result<(), String> {
    let instance = new_instance();
    let (adapter, device, queue) = open_device(&instance, None)?;
    let registry = zenith_content::Registry::builtin();
    let renderer = Renderer::new(&adapter, device, queue, &registry, None, launch.width.max(16), launch.height.max(16));
    let mut running = Running::new(renderer, &launch);
    eprintln!("zenith: offscreen on {} ({})", running.renderer.adapter_name, running.renderer.backend);
    let path = running.control_path.clone();
    loop {
        if !running.frame() {
            break;
        }
        if launch.uncapped {
            // The GPU's share of the frame counts too, so that the rate measured is the real one.
            let _ = running.renderer.device.poll(wgpu::PollType::wait_indefinitely());
        } else {
            // No display to wait for: hold to about 60 frames, or the fans will notice.
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
    }
    if let Some(control) = &running.control {
        control.settle(std::time::Duration::from_millis(500));
    }
    if let Some(p) = path {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_can_be_named_the_way_a_person_would() {
        let w = KeyInput::by_name("W", true).expect("a letter");
        assert_eq!(w.code, Some(KeyCode::KeyW));
        assert_eq!(w.text.as_deref(), Some("w"));
        assert!(w.pressed);
        assert_eq!(KeyInput::by_name("7", false).and_then(|k| k.code), Some(KeyCode::Digit7));
        assert_eq!(KeyInput::by_name("/", true).and_then(|k| k.text), Some("/".to_string()));
        assert_eq!(KeyInput::by_name("f11", true).and_then(|k| k.named), Some(NamedKey::F11));
        assert_eq!(KeyInput::by_name("esc", true).and_then(|k| k.named), Some(NamedKey::Escape));
        assert!(KeyInput::by_name("f13", true).is_none());
        assert!(KeyInput::by_name("nonsense", true).is_none());
    }

    #[test]
    fn frame_times_report_the_slow_ones_as_well_as_the_average() {
        let mut t = FrameTimes::default();
        for _ in 0..99 {
            t.add(10.0, 2.0);
        }
        t.add(110.0, 50.0);
        let (avg, _, worst) = FrameTimes::summary(&t.gaps);
        assert!((avg - 11.0).abs() < 0.01, "{avg}");
        assert_eq!(worst, 110.0);
        for _ in 0..FrameTimes::KEEP {
            t.add(5.0, 1.0);
        }
        assert_eq!(FrameTimes::summary(&t.gaps), (5.0, 5.0, 5.0), "old frames age out");
    }
}
