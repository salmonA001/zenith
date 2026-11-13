//! A world being played: the session, the camera, input, and what goes on screen.
use crate::audio::{Audio, Sfx};
use crate::hud;
use crate::models::{self, Light, Particle};
use crate::options::Options;
use crate::render::ui::UiBatch;
use crate::render::{Camera, DebugView, EntityVertex, Frame, NO_TEXTURE, Renderer};
use crate::streamer::Streamer;
use glam::{DVec3, Vec3};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;
use zenith_agent::{Agent, Event, Session};
use zenith_content::{BlockState, ItemKind, id};
use zenith_core::{Aabb, BlockPos, FxMap, Rng, dmath};
use zenith_protocol::*;
pub use zenith_protocol::{SIGN_LINE_LENGTH, SIGN_LINES};
use zenith_server::player::TOO_HUNGRY_TO_SPRINT;
use zenith_server::{Server, ServerConfig};
use zenith_world::Generator;
use zenith_world::physics::{EYE_HEIGHT, Keys, MoveInput};

pub const TICK: f64 = 0.05;
const REACH: f64 = 5.0;
const ATTACK_REACH: f64 = 4.5;

/// A world running on a thread of this process. It is a real server: only the wire is short.
pub struct LocalServer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
    pub peers: Sender<Peer>,
}

impl LocalServer {
    pub fn start(cfg: ServerConfig) -> Result<Self, String> {
        let server = Server::new(cfg)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (peers, rx) = std::sync::mpsc::channel();
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("server".into())
                .spawn(move || server.run(stop, rx))
                .map_err(|e| e.to_string())?
        };
        Ok(Self { stop, thread: Some(thread), peers })
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            match t.join() {
                Ok(Err(e)) => eprintln!("zenith: the world did not save cleanly: {e}"),
                Err(_) => eprintln!("zenith: the server thread crashed"),
                Ok(Ok(())) => {}
            }
        }
    }
}

/// What the keyboard and mouse are holding.
#[derive(Clone, Copy, Debug, Default)]
pub struct Held {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
    pub attack: bool,
    pub use_item: bool,
}

/// Screens that can cover the world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    None,
    Inventory,
    Pause,
    Chat(String),
    Dead,
    /// The debug options screen, F6.
    Debug,
    /// Writing on a sign: its lines so far, and the one being written.
    Sign {
        pos: BlockPos,
        lines: Vec<String>,
        line: usize,
    },
}

/// What the player is looking at, past whatever else.
enum Target {
    None,
    Block(zenith_world::pick::Hit),
    Entity(u32),
}

pub struct Game {
    pub agent: Agent,
    _server: Option<LocalServer>,
    pub streamer: Streamer,
    pub yaw: f64,
    pub pitch: f64,
    pub held: Held,
    pub overlay: Overlay,
    pub show_debug: bool,
    /// The switches on the debug options screen.
    pub debug_view: DebugView,
    /// How long the last frames took, in milliseconds, oldest first: the debug screen's graph.
    pub frame_times: VecDeque<f32>,
    /// The touch controls, as they stand this frame, for the HUD to draw.
    pub touch_view: crate::touch::TouchView,
    pub hide_hud: bool,
    pub options: Options,
    pub creative_page: usize,
    pub quit: bool,
    pub leave_to_title: bool,
    pub frame_ms: f32,
    pub fps: f32,
    pub debug: hud::DebugStats,

    accumulator: f64,
    seconds: f64,
    particles: Vec<Particle>,
    rng: Rng,
    entities: Vec<EntityVertex>,
    alpha: Vec<EntityVertex>,
    hand: Vec<EntityVertex>,
    /// Items as they are held and dropped, made the first time each is drawn.
    item_shapes: models::ItemShapes,
    /// Which block is being dug, and how far along it seems to be.
    digging: Option<(BlockPos, f32)>,
    swing: f32,
    bob: f32,
    attack_cool: u32,
    /// How many ticks the bow in hand has been drawn.
    pub bow_drawn: u32,
    /// Whether the server has been told the shield is up.
    guarding: bool,
    use_cool: u32,
    /// Whether the use button was down a tick ago: a fishing rod answers the press, not the hold.
    use_was_held: bool,
    jump_taps: u32,
    jump_was_held: bool,
    hurt_flash: f32,
    walk_phase: FxMap<u32, f32>,
    target_cache: Option<(BlockPos, BlockState)>,
    pub saved_message: Option<String>,
    audio: Audio,
    step_timer: f32,
    /// The view last handed to the agent. If the agent's differs, a command has turned the player.
    view_given: (u16, i16),
    last_fps_check: f64,
    frames_since_check: u32,
}

impl Game {
    fn from_session(session: Session, server: Option<LocalServer>, options: Options, renderer: &mut Renderer) -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        // While a view loads, every pool is busy at once. Together they leave a core for this thread,
        // which draws the frames, and one for the server: with more busy workers than cores, a frame
        // waits its turn behind them and loading stutters.
        let workers = cores.saturating_sub(2 + session.generator_threads()).max(2);
        let far = (workers / 4).max(1);
        renderer.use_registry(&session.reg);
        renderer.set_world(session.generator.clone(), far);
        let streamer = Streamer::new(session.reg.clone(), session.generator.clone(), (workers - far).clamp(1, 12));
        let mut agent = Agent::new(session);
        agent.set_view(0, 0);
        Self {
            agent,
            _server: server,
            streamer,
            yaw: 0.0,
            pitch: 10.0,
            held: Held::default(),
            overlay: Overlay::None,
            show_debug: false,
            debug_view: DebugView::default(),
            frame_times: VecDeque::with_capacity(FRAME_GRAPH),
            touch_view: Default::default(),
            hide_hud: false,
            options,
            creative_page: 0,
            quit: false,
            leave_to_title: false,
            frame_ms: 0.0,
            fps: 0.0,
            debug: hud::DebugStats::default(),
            accumulator: 0.0,
            seconds: 0.0,
            particles: Vec::new(),
            rng: Rng::new(0x5EED),
            entities: Vec::new(),
            alpha: Vec::new(),
            hand: Vec::new(),
            item_shapes: models::ItemShapes::default(),
            digging: None,
            swing: 0.0,
            bob: 0.0,
            attack_cool: 0,
            bow_drawn: 0,
            guarding: false,
            use_cool: 0,
            use_was_held: false,
            jump_taps: 0,
            jump_was_held: false,
            hurt_flash: 0.0,
            walk_phase: FxMap::default(),
            target_cache: None,
            saved_message: None,
            audio: Audio::start(),
            step_timer: 0.0,
            view_given: (0, 0),
            last_fps_check: 0.0,
            frames_since_check: 0,
        }
    }

    /// Opens or creates a world on a thread of this process, and joins it.
    /// `mods` are packs from the mod library, on top of any the world keeps in its own folder.
    pub fn single_player(
        dir: PathBuf,
        seed: u64,
        mode: GameMode,
        options: Options,
        renderer: &mut Renderer,
        mods: Vec<zenith_content::Pack>,
    ) -> Result<Self, String> {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let mut packs = zenith_server::load_packs(&dir.join("packs"))?;
        packs.extend(mods);
        let server = LocalServer::start(ServerConfig {
            name: dir.file_name().and_then(|n| n.to_str()).unwrap_or("World").to_string(),
            world_dir: Some(dir),
            seed,
            default_mode: mode,
            everyone_op: true,
            threads: (cores / 4).max(1),
            packs,
            ..Default::default()
        })?;
        let (near, far) = memory_pair();
        server.peers.send(far).map_err(|_| "The world stopped before it started.".to_string())?;
        let session =
            Session::connect(near, &options.name, None, options.render_distance + 1, 2, Duration::from_secs(30))?;
        Ok(Self::from_session(session, Some(server), options, renderer))
    }

    /// Joins a server over the network.
    pub fn multiplayer(
        address: &str,
        password: Option<String>,
        options: Options,
        renderer: &mut Renderer,
    ) -> Result<Self, String> {
        let address = if address.contains(':') { address.to_string() } else { format!("{address}:25575") };
        let stream = std::net::TcpStream::connect(&address).map_err(|e| format!("Could not reach {address}: {e}"))?;
        let peer = tcp_peer(stream).map_err(|e| e.to_string())?;
        let session =
            Session::connect(peer, &options.name, password, options.render_distance + 1, 4, Duration::from_secs(20))?;
        Ok(Self::from_session(session, None, options, renderer))
    }

    pub fn session(&self) -> &Session {
        &self.agent.session
    }

    /// Seconds of play, for things that blink.
    pub fn blink(&self) -> f32 {
        self.seconds as f32
    }

    fn view_input(&self, keys: u8) -> MoveInput {
        MoveInput {
            keys: Keys(keys),
            yaw: dmath::deg_to_turn(self.yaw),
            pitch: (self.pitch.clamp(-90.0, 90.0) * 65536.0 / 360.0) as i16,
        }
    }

    fn look(&self) -> DVec3 {
        self.view_input(0).look()
    }

    fn keys_from_held(&self) -> u8 {
        // Asleep, the body lies still whatever is held.
        if self.overlay != Overlay::None || self.session().sleeping {
            return 0;
        }
        let h = &self.held;
        let mut k = 0;
        for (on, bit) in [
            (h.forward, Keys::FORWARD),
            (h.back, Keys::BACK),
            (h.left, Keys::LEFT),
            (h.right, Keys::RIGHT),
            (h.jump, Keys::JUMP),
            (h.sneak, Keys::SNEAK),
            (h.sprint, Keys::SPRINT),
        ] {
            if on {
                k |= bit;
            }
        }
        // Too hungry to sprint: the server would refuse it, so do not pretend.
        if self.session().mode == GameMode::Survival && self.session().food <= TOO_HUNGRY_TO_SPRINT {
            k &= !Keys::SPRINT;
        }
        // Behind a shield, a body crouches and cannot run, as the server has it.
        if self.guarding {
            k = (k | Keys::SNEAK) & !Keys::SPRINT;
        }
        k
    }

    /// Turns the camera. Degrees of yaw and pitch, as a mouse moves them.
    pub fn turn(&mut self, dx: f64, dy: f64) {
        if self.overlay != Overlay::None {
            return;
        }
        let s = self.options.sensitivity;
        self.yaw = (self.yaw + dx * s).rem_euclid(360.0);
        self.pitch = (self.pitch + dy * s).clamp(-89.9, 89.9);
    }

    /// Advances time: network, ticks, and everything that follows from them.
    pub fn update(&mut self, dt: f64, renderer: &mut Renderer) {
        self.seconds += dt;
        self.accumulator = (self.accumulator + dt).min(0.5);
        self.frames_since_check += 1;
        if self.seconds - self.last_fps_check >= 0.5 {
            self.fps = self.frames_since_check as f32 / (self.seconds - self.last_fps_check) as f32;
            self.last_fps_check = self.seconds;
            self.frames_since_check = 0;
        }
        self.agent.session.poll();
        self.take_events(renderer);

        while self.accumulator >= TICK {
            self.accumulator -= TICK;
            self.fixed_tick();
        }

        let center = BlockPos::from_vec(self.session().body.pos).chunk();
        self.streamer.upload(renderer, center);
        self.streamer.schedule(&self.agent.session.world, center);

        self.swing = (self.swing - dt as f32 * 3.4).max(0.0);
        self.hurt_flash = (self.hurt_flash - dt as f32 * 2.2).max(0.0);
        let solid = &self.agent.session;
        let reg = solid.reg.clone();
        self.particles.retain_mut(|p| p.update(dt as f32, |b| reg.lut.collides[solid.block(b).kind() as usize]));
        if self.particles.len() > 4000 {
            let extra = self.particles.len() - 4000;
            self.particles.drain(..extra);
        }

        // The server says who is alive, so that is what decides when this screen comes and goes.
        let dead = self.session().health == 0;
        if dead != (self.overlay == Overlay::Dead) {
            self.overlay = if dead { Overlay::Dead } else { Overlay::None };
        }
        if let Some(why) = self.session().disconnected.clone() {
            self.saved_message = Some(why);
            self.leave_to_title = true;
        }
        if self.agent.quit_requested {
            self.quit = true;
        }
    }

    fn take_events(&mut self, renderer: &mut Renderer) {
        let events = std::mem::take(&mut self.agent.session.events);
        self.agent.digest_events(&events);
        for e in events {
            match e {
                Event::ChunkReady(pos) => self.streamer.chunk_arrived(pos),
                Event::ChunkGone(pos) => self.streamer.chunk_left(pos, renderer),
                Event::BlockChanged { pos, old, new } => {
                    self.streamer.block_changed(pos, old, new);
                    if self.digging.is_some_and(|(p, _)| p == pos) {
                        self.digging = None;
                    }
                }
                Event::Effect(fx) => self.effect(fx),
                Event::EditSign(pos) => {
                    let mut lines = self.session().signs.get(&pos).cloned().unwrap_or_default();
                    lines.resize(SIGN_LINES, String::new());
                    self.overlay = Overlay::Sign { pos, lines, line: 0 };
                }
                Event::Hurt => {
                    self.hurt_flash = 1.0;
                    self.audio.play(Sfx::Hurt, 1.0);
                }
                Event::Died => self.audio.play(Sfx::Death, 1.0),
                Event::ScreenOpened(_) => {
                    if !self.agent.is_driving() {
                        self.overlay = Overlay::Inventory;
                    }
                }
                Event::ScreenClosed if self.overlay == Overlay::Inventory => {}
                _ => {}
            }
        }
    }

    fn effect(&mut self, fx: Effect) {
        let at = DVec3::new(fx.pos[0] as f64, fx.pos[1] as f64, fx.pos[2] as f64);
        let d = at.distance(self.session().body.pos) as f32;
        let volume = (1.0 - d / 40.0).clamp(0.0, 1.0);
        match fx.kind {
            EffectKind::BlockBroken(state) => {
                let reg = self.session().reg.clone();
                let def = reg.block(state);
                self.audio.play(Sfx::Break(def.sound), volume);
                let layer = reg.face_tile(state.kind(), 2) as u32;
                let tint = match def.tint {
                    zenith_content::Tint::None => [255, 255, 255, 255],
                    _ => [130, 180, 90, 255],
                };
                for _ in 0..28 {
                    let r = |rng: &mut Rng| rng.f64() - 0.5;
                    let (ox, oy, oz) = (r(&mut self.rng), r(&mut self.rng), r(&mut self.rng));
                    let u = [self.rng.f64() as f32 * 0.8, self.rng.f64() as f32 * 0.8];
                    self.particles.push(Particle {
                        pos: at + DVec3::new(ox, oy, oz) * 0.8,
                        vel: DVec3::new(ox * 3.5, oy * 2.0 + 1.5, oz * 3.5),
                        age: 0.0,
                        life: 0.5 + self.rng.f64() as f32 * 0.5,
                        size: 0.05 + self.rng.f64() as f32 * 0.05,
                        color: tint,
                        layer,
                        uv: u,
                        gravity: 11.0,
                        glow: false,
                    });
                }
            }
            EffectKind::BlockPlaced(state) => {
                let sound = self.session().reg.block(state).sound;
                self.audio.play(Sfx::Place(sound), volume);
            }
            EffectKind::Explosion => {
                self.audio.play(Sfx::Explosion, volume);
                for _ in 0..160 {
                    let dir = DVec3::new(self.rng.f64() - 0.5, self.rng.f64() - 0.3, self.rng.f64() - 0.5)
                        .normalize_or_zero();
                    let v = 2.0 + self.rng.f64() * 9.0;
                    let hot = self.rng.chance(0.4);
                    self.particles.push(Particle {
                        pos: at,
                        vel: dir * v,
                        age: 0.0,
                        life: 0.4 + self.rng.f64() as f32 * 1.0,
                        size: 0.25 + self.rng.f64() as f32 * 0.5,
                        color: if hot { [255, 170, 60, 220] } else { [86, 84, 82, 170] },
                        layer: NO_TEXTURE,
                        uv: [0.0, 0.0],
                        gravity: if hot { 2.0 } else { -1.0 },
                        glow: true,
                    });
                }
            }
            EffectKind::Fizz => {
                self.audio.play(Sfx::Fizz, volume);
                for _ in 0..12 {
                    let v = DVec3::new(self.rng.f64() - 0.5, 1.0 + self.rng.f64(), self.rng.f64() - 0.5);
                    self.particles.push(Particle {
                        pos: at,
                        vel: v,
                        age: 0.0,
                        life: 0.8,
                        size: 0.15,
                        color: [210, 210, 210, 120],
                        layer: NO_TEXTURE,
                        uv: [0.0, 0.0],
                        gravity: -1.0,
                        glow: true,
                    });
                }
            }
            EffectKind::Hurt => self.audio.play(Sfx::Hit, volume),
            EffectKind::Death => self.audio.play(Sfx::Death, volume),
            EffectKind::Splash => self.audio.play(Sfx::Splash, volume),
            EffectKind::Pickup => self.audio.play(Sfx::Pickup, volume),
            EffectKind::Love => self.sparkle(at, [236, 86, 120, 230], 7, -0.6),
            EffectKind::Blocked => self.audio.play(Sfx::Place(zenith_content::Sound::Wood), volume),
            EffectKind::LevelUp => {
                self.audio.play(Sfx::Pickup, volume);
                self.sparkle(at + DVec3::Y, [255, 214, 92, 240], 16, -0.9);
            }
            EffectKind::Eat => self.audio.play(Sfx::Eat, volume),
            EffectKind::Lever => self.audio.play(Sfx::Click, volume),
            EffectKind::Shoot => self.audio.play(Sfx::Shoot, volume),
            EffectKind::Open => self.audio.play(Sfx::Open, volume),
        }
    }

    /// A little burst of glowing specks drifting up: hearts between animals, a level reached.
    fn sparkle(&mut self, at: DVec3, color: [u8; 4], count: usize, gravity: f32) {
        for _ in 0..count {
            let v = DVec3::new(self.rng.f64() - 0.5, 0.6 + self.rng.f64() * 0.6, self.rng.f64() - 0.5);
            self.particles.push(Particle {
                pos: at + DVec3::new(self.rng.f64() - 0.5, self.rng.f64() * 0.4, self.rng.f64() - 0.5) * 0.6,
                vel: v,
                age: 0.0,
                life: 0.7 + self.rng.f64() as f32 * 0.5,
                size: 0.08 + self.rng.f64() as f32 * 0.05,
                color,
                layer: NO_TEXTURE,
                uv: [0.0, 0.0],
                gravity,
                glow: true,
            });
        }
    }

    /// One game tick: 20 of these a second, whatever the frame rate.
    fn fixed_tick(&mut self) {
        // A command driving the player takes the controls; otherwise the keyboard has them.
        let driven = self.agent.is_driving();
        if self.agent.view() != self.view_given {
            let (y, p) = self.agent.view();
            self.yaw = dmath::turn_to_deg(y);
            self.pitch = p as f64 * 360.0 / 65536.0;
        }
        if !driven {
            let v = self.view_input(0);
            self.agent.set_view(v.yaw, v.pitch);
            self.view_given = (v.yaw, v.pitch);
        }
        let agent_input = self.agent.tick_input();
        // The shield first: the crouch it brings is in this very step, as the server will see it.
        self.raise_guard();
        let input = if driven {
            let (y, p) = self.agent.view();
            self.yaw = dmath::turn_to_deg(y);
            self.pitch = p as f64 * 360.0 / 65536.0;
            self.view_given = (y, p);
            agent_input
        } else {
            self.view_input(self.keys_from_held())
        };
        self.creative_flight();
        let was_ground = self.session().body.on_ground;
        self.agent.session.tick(input);
        self.footsteps(input, was_ground);
        self.act_on_mouse();
    }

    fn creative_flight(&mut self) {
        if self.session().mode != GameMode::Creative || self.session().riding.is_some() {
            self.jump_taps = 0;
            return;
        }
        // Tap jump twice to take off, twice more to land.
        let tapped = self.held.jump && !self.jump_was_held;
        self.jump_was_held = self.held.jump;
        self.jump_taps = self.jump_taps.saturating_sub(1);
        if tapped && self.overlay == Overlay::None {
            if self.jump_taps > 0 {
                let on = !self.session().body.flying;
                self.agent.session.body.flying = on;
                self.agent.session.send(&C2S::SetFlying(on));
                self.jump_taps = 0;
            } else {
                self.jump_taps = 6;
            }
        }
    }

    fn footsteps(&mut self, input: MoveInput, was_ground: bool) {
        let b = self.session().body;
        let speed = (b.vel.x * b.vel.x + b.vel.z * b.vel.z).sqrt() as f32;
        if b.on_ground {
            self.bob += speed * 3.3;
        }
        self.step_timer -= speed;
        if b.on_ground && (self.step_timer <= 0.0 || !was_ground) && (input.keys.0 & 15 != 0 || !was_ground) {
            self.step_timer = 1.9;
            let below = self.session().block(BlockPos::from_vec(b.pos - DVec3::Y * 0.2));
            let sound = self.session().reg.block(below).sound;
            self.audio.play(Sfx::Step(sound), 0.35);
        }
    }

    /// What the crosshair is on.
    fn target(&self) -> Target {
        let s = self.session();
        let eye = s.body.pos + DVec3::Y * EYE_HEIGHT;
        let look = self.look();
        let block = s.target(look, REACH);
        let block_d = block.map_or(f64::MAX, |h| h.distance);
        let mut best: Option<(f64, u32)> = None;
        for e in s.entities.values() {
            if !e.kind.is_mob() && !e.kind.is_vehicle() && e.kind != EntityKind::Player {
                continue;
            }
            let (w, h) = body_size(e.kind, e.baby);
            if let Some((t, _)) = Aabb::standing(e.pos, w, h).inflate(0.1).ray(eye, look)
                && t <= ATTACK_REACH
                && t < block_d
                && best.is_none_or(|b| t < b.0)
            {
                best = Some((t, e.id));
            }
        }
        match (best, block) {
            (Some((_, id)), _) => Target::Entity(id),
            (None, Some(h)) => Target::Block(h),
            _ => Target::None,
        }
    }

    /// Whether the mouse acts on the world: nothing over it, alive and awake, and not left to the
    /// autopilot.
    fn hands_free(&self) -> bool {
        self.overlay == Overlay::None
            && self.session().health > 0
            && !self.session().sleeping
            && !self.agent.is_driving()
    }

    /// A shield is up for as long as the button is held.
    fn raise_guard(&mut self) {
        let reg = self.session().reg.clone();
        let shield = self
            .session()
            .held()
            .and_then(|s| reg.item(s.item))
            .is_some_and(|i| matches!(i.kind, ItemKind::Shield { .. }));
        let guard = shield && self.hands_free() && self.held.use_item;
        if guard != self.guarding {
            self.guarding = guard;
            self.agent.session.send(&C2S::Guard(guard));
        }
    }

    fn act_on_mouse(&mut self) {
        self.attack_cool = self.attack_cool.saturating_sub(1);
        self.use_cool = self.use_cool.saturating_sub(1);
        let active = self.hands_free();
        let target = if active { self.target() } else { Target::None };
        self.target_cache = match &target {
            Target::Block(h) => Some((h.pos, h.state)),
            _ => None,
        };
        if (!active || !self.held.attack) && self.digging.take().is_some() {
            self.agent.session.send(&C2S::CancelDig);
        }
        if active && self.held.attack {
            match target {
                Target::Entity(id) => {
                    if self.attack_cool == 0 {
                        self.attack_cool = 8;
                        self.agent.session.send(&C2S::Attack { entity: id });
                        self.swing = 1.0;
                    }
                }
                Target::Block(hit) => self.dig_tick(hit),
                Target::None => {
                    if self.digging.take().is_some() {
                        self.agent.session.send(&C2S::CancelDig);
                    }
                }
            }
        }
        let reg = self.session().reg.clone();
        let in_hand = self.session().held().and_then(|s| reg.item(s.item)).map(|i| i.kind);
        let bow = matches!(in_hand, Some(ItemKind::Bow { .. }));
        let shield = matches!(in_hand, Some(ItemKind::Shield { .. }));
        // A bow draws while the button is held, and lets go when it is released.
        if bow && active && self.held.use_item {
            self.bow_drawn += 1;
        } else if self.bow_drawn > 0 {
            if bow && active {
                self.agent.session.send(&C2S::Shoot { drawn: self.bow_drawn.min(255) as u8 });
                self.swing = self.swing.max(0.3);
            }
            self.bow_drawn = 0;
        }
        let rod = matches!(in_hand, Some(ItemKind::Rod { .. }));
        let pressed = self.held.use_item && !self.use_was_held;
        self.use_was_held = self.held.use_item;
        if active && self.held.use_item && (self.use_cool == 0 || (rod && pressed)) && !bow && !shield {
            self.use_cool = 4;
            let is_food = self
                .session()
                .held()
                .is_some_and(|s| matches!(reg.item(s.item).map(|i| i.kind), Some(ItemKind::Food { .. })));
            // What is used on whatever is looked at from where one stands, with no block needed: a
            // boat goes down on water or ground, and an empty bucket scoops up a spring.
            let from_afar = rod
                || self
                    .session()
                    .held()
                    .and_then(|s| reg.item(s.item))
                    .is_some_and(|i| i.name == "boat" || i.kind == ItemKind::Bucket { holds: 0 });
            // Food is eaten wherever it points, unless at something that opens or works; so with
            // the things used from afar, and a ride to get into comes first of all.
            let target = self.target();
            let opening = matches!(target, Target::Block(h) if opens(h.state.kind()));
            match target {
                // A creature: feed it, if it eats what is in hand. A boat or cart: get in.
                Target::Entity(entity) => {
                    self.agent.session.send(&C2S::UseEntity { entity });
                    self.swing = self.swing.max(0.5);
                }
                // A rod casts, or pulls in, once for each press.
                _ if from_afar && !opening => {
                    if !rod || pressed {
                        self.agent.session.send(&C2S::UseItem);
                        self.swing = self.swing.max(0.5);
                    }
                }
                Target::Block(h) if !is_food || opens(h.state.kind()) => {
                    self.agent.session.send(&C2S::UseOn { pos: h.pos, face: h.face as u8 });
                    self.swing = self.swing.max(0.7);
                }
                _ if is_food && self.session().food < 20 => {
                    self.agent.session.send(&C2S::UseItem);
                    // Bringing it to the mouth, bite after bite.
                    self.swing = self.swing.max(0.35);
                }
                _ => {}
            }
        }
    }

    fn dig_tick(&mut self, hit: zenith_world::pick::Hit) {
        let s = &self.agent.session;
        let creative = s.mode == GameMode::Creative;
        if creative {
            if self.attack_cool == 0 {
                self.attack_cool = 5;
                s.dig(hit.pos, hit.face as u8);
                self.swing = 1.0;
            }
            return;
        }
        let ticks = s.reg.dig_ticks(hit.state, s.held().map(|h| h.item)).unwrap_or(f32::MAX).max(1.0);
        match &mut self.digging {
            Some((pos, progress)) if *pos == hit.pos => {
                *progress = (*progress + 1.0 / ticks).min(0.99);
            }
            _ => {
                if self.digging.is_some() {
                    s.send(&C2S::CancelDig);
                }
                s.dig(hit.pos, hit.face as u8);
                self.digging = Some((hit.pos, 1.0 / ticks));
            }
        }
        if self.swing < 0.3 {
            self.swing = 1.0;
        }
    }

    pub fn select_slot(&mut self, slot: u8) {
        self.agent.session.send(&C2S::SelectSlot(slot));
        self.agent.session.selected = slot;
    }

    pub fn scroll_slot(&mut self, by: i32) {
        let next = (self.session().selected as i32 - by).rem_euclid(9) as u8;
        self.select_slot(next);
    }

    pub fn toggle_inventory(&mut self) {
        match self.overlay {
            Overlay::Inventory => self.close_overlay(),
            Overlay::None => self.overlay = Overlay::Inventory,
            _ => {}
        }
    }

    pub fn close_overlay(&mut self) {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Inventory => self.agent.session.send(&C2S::CloseScreen),
            // A sign keeps what was written on it, however the writing ends.
            Overlay::Sign { pos, lines, .. } => {
                self.agent.session.signs.insert(pos, lines.clone());
                self.agent.session.send(&C2S::SignText { pos, lines });
            }
            _ => {}
        }
    }

    /// Signs near enough to read, facing this way and in plain sight: where their words go, and
    /// the words.
    fn readable_signs(&self, cam_pos: DVec3) -> Vec<(Vec3, Vec<String>)> {
        let s = self.session();
        let mut out = Vec::new();
        for (pos, lines) in &s.signs {
            let state = s.block(*pos);
            if state.kind() != id::SIGN || lines.iter().all(|l| l.trim().is_empty()) {
                continue;
            }
            let Some(board) = s.reg.block(state).shape.boxes(state.meta()).first().copied() else { continue };
            let center = DVec3::new(
                pos.x as f64 + (board[0][0] as f64 + board[1][0] as f64) / 32.0,
                pos.y as f64 + (board[0][1] as f64 + board[1][1] as f64) / 32.0,
                pos.z as f64 + (board[0][2] as f64 + board[1][2] as f64) / 32.0,
            );
            let facing = match state.meta() & 3 {
                0 => DVec3::NEG_Z,
                1 => DVec3::Z,
                2 => DVec3::NEG_X,
                _ => DVec3::X,
            };
            let at = center + facing * 0.08;
            let to_eye = cam_pos - at;
            let distance = to_eye.length();
            if distance > 8.0 || to_eye.dot(facing) <= 0.0 {
                continue;
            }
            // Hidden behind something else.
            let dir = (at - cam_pos) / distance;
            let blocked = zenith_world::pick::pick(&s.world, &s.reg, cam_pos, dir, distance - 0.05)
                .is_some_and(|hit| hit.pos != *pos);
            if !blocked {
                out.push(((at - cam_pos).as_vec3(), lines.clone()));
            }
        }
        out
    }

    /// Notes a frame's time for the graph.
    pub fn record_frame(&mut self, ms: f32) {
        self.frame_ms = ms;
        if self.frame_times.len() == FRAME_GRAPH {
            self.frame_times.pop_front();
        }
        self.frame_times.push_back(ms);
    }

    pub fn respawn(&mut self) {
        self.agent.session.send(&C2S::Respawn);
    }

    fn light_near(&self, p: DVec3) -> Light {
        // A body brushing a wall has its middle in a solid cell, which is dark: take the lighter of
        // that one and the one above.
        let b = BlockPos::from_vec(p + DVec3::Y * 0.5);
        let mut lit = None;
        for y in [b.y, b.y + 1] {
            if let Some((sky, block)) = self.streamer.light_at(b.x, y, b.z) {
                let (s, k) = lit.unwrap_or((0, 0));
                lit = Some((sky.max(s), block.max(k)));
            }
        }
        match lit {
            Some((sky, block)) => Light { sky: sky * 17, block: block * 17 },
            None => Light { sky: 255, block: 0 },
        }
    }

    /// Builds the frame and draws it.
    pub fn draw(&mut self, renderer: &mut Renderer, ui: &mut UiBatch, mouse: hud::Mouse) {
        let alpha_t = (self.accumulator / TICK).clamp(0.0, 1.0);
        let session = &self.agent.session;
        let feet = session.interpolated_pos(alpha_t);
        let bob = if session.body.on_ground && self.options.view_bob {
            let speed = (session.body.vel.x.powi(2) + session.body.vel.z.powi(2)).sqrt();
            (self.bob as f64 * 2.0).sin() * 0.03 * (speed * 3.0).min(1.0)
        } else {
            0.0
        };
        // Asleep, the eye is down on the pillow. Riding, it is where the server seats the body, as
        // low as sitting puts it, and no bob.
        let eye = if session.sleeping {
            0.3
        } else if session.riding.is_some() {
            EYE_HEIGHT
        } else {
            EYE_HEIGHT + bob
        };
        let cam_pos = feet + DVec3::Y * eye;
        // A drawn bow narrows the view a little, as taking aim does.
        let aim = f64::from(self.bow_drawn.min(20)) / 20.0;
        let fov = self.options.fov * (1.0 - 0.12 * aim);
        let camera = Camera { pos: cam_pos, yaw_deg: self.yaw, pitch_deg: self.pitch, fov_deg: fov };
        let forward = camera.forward();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();

        let reg = session.reg.clone();
        let cam_cell = BlockPos::from_vec(cam_pos);
        let here = reg.block(session.block(cam_cell));
        let under_water = here.fluid == zenith_content::Fluid::Water;
        let under_lava = here.fluid == zenith_content::Fluid::Lava;

        self.entities.clear();
        self.alpha.clear();
        self.hand.clear();
        let seconds = self.seconds as f32;

        // Things in the world.
        let list: Vec<_> = session.entities.values().cloned().collect();
        let mut names = Vec::new();
        for e in &list {
            let p = e.prev.lerp(e.pos, alpha_t);
            let rel = (p - cam_pos).as_vec3();
            if rel.length_squared() > 128.0 * 128.0 {
                continue;
            }
            let light = self.light_near(p);
            match e.kind {
                EntityKind::Item => {
                    if let Some(stack) = e.stack {
                        models::item_entity(
                            &mut self.entities,
                            &reg,
                            &mut self.item_shapes,
                            rel,
                            stack,
                            seconds,
                            e.id,
                            light,
                        );
                    }
                }
                EntityKind::FallingBlock => models::whole_block(&mut self.entities, &reg, rel, e.block, 1.0, light),
                EntityKind::Tnt => {
                    let pulse = (seconds * 16.0).sin() > 0.0;
                    let shown = if pulse { BlockState::of(id::SNOW_BLOCK) } else { BlockState::of(id::TNT) };
                    models::whole_block(&mut self.entities, &reg, rel, shown, 1.02, Light { sky: 255, block: 255 });
                }
                EntityKind::Arrow => {
                    models::arrow(&mut self.entities, rel, e.yaw as f32 / 256.0, e.pitch as f32 / 256.0, light)
                }
                // A float, and the line to it from its angler's rod: this player's own hand, or about
                // where another's would be.
                EntityKind::Float => {
                    models::float(&mut self.entities, rel, light);
                    let hand = if e.owner == session.entity_id {
                        Some(forward * 0.8 + right * 0.46 - up * 0.2)
                    } else {
                        list.iter()
                            .find(|o| o.id == e.owner)
                            .map(|o| (o.prev.lerp(o.pos, alpha_t) - cam_pos).as_vec3() + Vec3::Y * 1.45)
                    };
                    if let Some(hand) = hand {
                        models::line(&mut self.entities, hand, rel + Vec3::Y * 0.26, [236, 236, 230, 255], light);
                    }
                }
                EntityKind::Boat => {
                    let yaw = lerp_turn(e.prev_yaw, e.yaw, alpha_t as f32);
                    models::boat(&mut self.entities, rel, yaw, light);
                }
                EntityKind::Cart => {
                    let yaw = lerp_turn(e.prev_yaw, e.yaw, alpha_t as f32);
                    models::cart(&mut self.entities, rel, yaw, e.pitch as f32 / 256.0, light);
                }
                kind => {
                    let moved = (e.pos - e.prev).length() as f32;
                    let phase = self.walk_phase.entry(e.id).or_insert(0.0);
                    *phase += moved * 2.2 * (renderer_dt(self.frame_ms));
                    let speed = (moved * 8.0).min(1.0);
                    let yaw = lerp_turn(e.prev_yaw, e.yaw, alpha_t as f32);
                    models::creature(
                        &mut self.entities,
                        kind,
                        rel,
                        yaw,
                        e.pitch as f32 / 256.0,
                        *phase * 6.0,
                        speed,
                        if e.baby { 0.55 } else { 1.0 },
                        light,
                    );
                    if let Some(name) = &e.name {
                        names.push((rel + Vec3::Y * models::NAME_HEIGHT, name.clone()));
                    }
                }
            }
        }

        if self.debug_view.hitboxes {
            for e in &list {
                let rel = (e.prev.lerp(e.pos, alpha_t) - cam_pos).as_vec3();
                let (width, height) = body_size(e.kind, e.baby);
                let half = Vec3::new(width as f32 / 2.0, 0.0, width as f32 / 2.0);
                let (lo, hi) = (rel - half, rel + half + Vec3::Y * height as f32);
                let thick = 0.008 + rel.length() * 0.0012;
                models::wire_box(&mut self.alpha, lo, hi, thick, [255, 255, 255, 200]);
            }
        }
        if self.debug_view.chunk_borders {
            chunk_borders(&mut self.alpha, cam_pos);
        }

        // The selected block, and cracks.
        if !self.hide_hud
            && self.overlay == Overlay::None
            && let Some((pos, state)) = self.target_cache
        {
            let rel = DVec3::new(pos.x as f64, pos.y as f64, pos.z as f64) - cam_pos;
            models::outline(&mut self.alpha, &reg, rel.as_vec3(), state);
        }
        if let Some((pos, progress)) = self.digging {
            let rel = (DVec3::new(pos.x as f64, pos.y as f64, pos.z as f64) - cam_pos).as_vec3();
            let stage = ((progress * 10.0) as u32).min(9);
            models::cracks(&mut self.alpha, rel, renderer.crack_layer + stage);
        }
        for (&pos, &stage) in &session.cracks {
            let rel = (DVec3::new(pos.x as f64, pos.y as f64, pos.z as f64) - cam_pos).as_vec3();
            models::cracks(&mut self.alpha, rel, renderer.crack_layer + stage.min(9) as u32);
        }

        for p in &self.particles {
            let light = self.light_near(p.pos);
            models::draw_particle(&mut self.alpha, p, cam_pos, right, up, light);
        }

        // Weather: a column of it around the camera, only where the sky is open.
        let rain = session.clock.rain as f32 / 255.0;
        if rain > 0.02 {
            weather(&mut self.alpha, session, cam_pos, right, seconds, rain);
        }

        // The arm.
        if !self.hide_hud && session.health > 0 {
            let light = self.light_near(cam_pos);
            let drawn = self.bow_drawn.min(20) as f32 / 20.0;
            models::view_model(
                &mut self.hand,
                &reg,
                &mut self.item_shapes,
                session.held(),
                self.swing,
                self.bob,
                drawn,
                self.guarding,
                light,
            );
        }

        let signs = self.readable_signs(cam_pos);
        hud::draw(self, ui, mouse, renderer.size(), &names, &signs, camera);

        let session = &self.agent.session;
        let frame = Frame {
            camera,
            time_of_day: session.clock.time as f32,
            rain,
            seconds,
            under_water,
            under_lava,
            hurt_flash: self.hurt_flash,
            dead: if session.health == 0 { 1.0 } else { 0.0 },
            entities: &self.entities,
            alpha: &self.alpha,
            view_model: &self.hand,
            ui,
            draw_far: self.options.far_terrain,
            ray_tracing: self.options.ray_tracing,
            classic: self.options.look == crate::options::Look::Classic,
            debug: self.debug_view,
        };
        renderer.render(&frame);
    }
}

/// How many frames the debug screen's graph shows.
pub const FRAME_GRAPH: usize = 150;

/// Whether using a block does something of its own: opening it, working it, lying in it.
/// How big a creature's body is: half size while young, as the server has it.
fn body_size(kind: EntityKind, baby: bool) -> (f64, f64) {
    let (w, h) = kind.size();
    if baby { (w * 0.5, h * 0.5) } else { (w, h) }
}

fn opens(kind: u16) -> bool {
    matches!(kind, id::BENCH | id::FURNACE | id::CHEST | id::LEVER | id::TNT | id::BED | id::BED_HEAD | id::CAMPFIRE)
}

fn renderer_dt(frame_ms: f32) -> f32 {
    (frame_ms / 50.0).clamp(0.0, 1.0)
}

/// The edges of the chunk the camera is in, and the corners of those around it: posts up and down,
/// and a ring at each section's floor, sixteen blocks apart.
fn chunk_borders(out: &mut Vec<EntityVertex>, cam: DVec3) {
    let (cx, cz) = ((cam.x / 16.0).floor() * 16.0, (cam.z / 16.0).floor() * 16.0);
    let floor = (cam.y / 16.0).floor() * 16.0;
    let (low, high) = (floor - 32.0, floor + 48.0);
    let rel = |x: f64, y: f64, z: f64| Vec3::new((x - cam.x) as f32, (y - cam.y) as f32, (z - cam.z) as f32);
    let thick = |at: Vec3| 0.01 + Vec3::new(at.x, 0.0, at.z).length() * 0.0015;
    for i in -1..=2 {
        for j in -1..=2 {
            let (x, z) = (cx + f64::from(i) * 16.0, cz + f64::from(j) * 16.0);
            let own = (0..=1).contains(&i) && (0..=1).contains(&j);
            let color = if own { [255, 220, 60, 230] } else { [230, 70, 60, 170] };
            let base = rel(x, low, z);
            models::bar(out, base, rel(x, high, z), thick(base), color);
        }
    }
    let mut y = low;
    while y <= high {
        let corners = [rel(cx, y, cz), rel(cx + 16.0, y, cz), rel(cx + 16.0, y, cz + 16.0), rel(cx, y, cz + 16.0)];
        for k in 0..4 {
            let (a, b) = (corners[k], corners[(k + 1) % 4]);
            models::bar(out, a, b, thick((a + b) / 2.0), [90, 190, 255, 200]);
        }
        y += 16.0;
    }
}

fn lerp_turn(from: u8, to: u8, t: f32) -> f32 {
    let d = to.wrapping_sub(from) as i8 as f32;
    (from as f32 + d * t) / 256.0
}

fn weather(out: &mut Vec<EntityVertex>, s: &Session, cam: DVec3, right: Vec3, seconds: f32, amount: f32) {
    let (cx, cz) = (cam.x.floor() as i32, cam.z.floor() as i32);
    let radius = 14;
    let reg = &s.reg;
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            let (x, z) = (cx + dx, cz + dz);
            let seed = zenith_core::rng::mix(x as u32 as u64, z as u32 as u64);
            if (seed & 255) as f32 / 255.0 > amount * 0.5 {
                continue;
            }
            let Some(chunk) = s.world.get(zenith_core::ChunkPos::new(x >> 4, z >> 4)) else { continue };
            let top = chunk.top(x & 15, z & 15, |st| reg.lut.opacity[st.kind() as usize] > 0).unwrap_or(0);
            if (top as f64) > cam.y + 24.0 {
                continue;
            }
            let col = s.generator.climate(x as f64, z as f64);
            let cold = col.0 - ((top - 62).max(0) as f64) * 0.0105 < -0.42;
            let phase = ((seed >> 8) & 1023) as f32 / 1023.0;
            let fall = if cold { 2.5 } else { 22.0 };
            let span = 22.0;
            let y = cam.y as f32 + 12.0 - ((seconds * fall / span + phase).fract()) * span;
            if (y as f64) < top as f64 {
                continue;
            }
            let (ox, oz) = (((seed >> 20) & 255) as f32 / 255.0, ((seed >> 28) & 255) as f32 / 255.0);
            let drift = if cold { (seconds * 0.9 + phase * 6.0).sin() * 0.3 } else { 0.0 };
            let rel =
                Vec3::new((x as f64 - cam.x) as f32 + ox + drift, y - cam.y as f32, (z as f64 - cam.z) as f32 + oz);
            if cold {
                models::streak(out, rel, right, 0.07, 0.035, [245, 248, 255, 230]);
            } else {
                models::streak(out, rel, right, 1.15, 0.012, [170, 190, 225, 110]);
            }
        }
    }
}

/// The far terrain's source when no world is open: the title screen's scenery.
pub fn title_generator() -> Arc<Generator> {
    Arc::new(Generator::new(0x5A45_4E49_5448))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turning_through_zero_takes_the_short_way() {
        assert!((lerp_turn(250, 6, 0.5) - 0.0).abs() < 0.01 || (lerp_turn(250, 6, 0.5) - 1.0).abs() < 0.01);
    }
}
