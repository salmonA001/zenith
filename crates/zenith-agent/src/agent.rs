//! Drives a session from commands: what an AI or a script does instead of a mouse and keyboard.
//!
//! Commands that take time (walking, mining) block their caller, not the agent: each becomes a
//! task, ticked along with the game, and its reply goes out when it finishes.
use crate::commands;
use crate::session::{Event, Session};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use zenith_content::{BlockState, ItemKind, ItemStack, Shape};
use zenith_core::glam::DVec3;
use zenith_core::{BlockPos, Dir, dmath};
use zenith_protocol::*;
use zenith_world::physics::{EYE_HEIGHT, Keys, MoveInput};
use zenith_world::{BlockAccess, path};

/// Where a command's answer goes.
pub type Reply = Sender<Value>;

const REACH: f64 = 5.0;
const EVENT_LOG: usize = 500;

enum Task {
    WaitReady {
        until: Instant,
    },
    Goto {
        goal: DVec3,
        near: f64,
        until: Instant,
        sprint: bool,
        route: Vec<BlockPos>,
        at: usize,
        replan_in: u32,
        stuck: u32,
        last: DVec3,
        then: Option<Box<Task>>,
    },
    Walk {
        keys: u8,
        until: Instant,
    },
    Jump {
        left_ground: bool,
        until: Instant,
    },
    Break {
        pos: BlockPos,
        before: BlockState,
        until: Instant,
        started: bool,
        gained_from: u64,
    },
    Place {
        pos: BlockPos,
        kind: u16,
        until: Instant,
    },
    Attack {
        entity: u32,
        until: Instant,
    },
    /// Holding food to the mouth until one is gone.
    Eat {
        item: u16,
        had: u32,
        until: Instant,
    },
    Craft(Box<CraftJob>),
    Respawn {
        until: Instant,
    },
}

struct CraftJob {
    /// Clicks left to send, one per tick so that each sees the result of the one before.
    steps: VecDeque<(u16, ClickButton)>,
    item: u16,
    times_left: u32,
    made: u32,
    size: usize,
    until: Instant,
    close_after: bool,
    settle: u32,
}

/// Events are numbered so a client can ask for what it missed.
struct LoggedEvent {
    seq: u64,
    value: Value,
}

pub struct Agent {
    pub session: Session,
    yaw: u16,
    pitch: i16,
    tasks: VecDeque<(Task, Reply)>,
    log: VecDeque<LoggedEvent>,
    next_event: u64,
    pub quit_requested: bool,
}

fn err(message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": message.into() })
}
fn ok(result: Value) -> Value {
    json!({ "ok": true, "result": result })
}

fn yaw_pitch_towards(from: DVec3, to: DVec3) -> (u16, i16) {
    let d = to - from;
    let horizontal = (d.x * d.x + d.z * d.z).sqrt();
    // This is aiming, not simulation: the platform's arctangent is fine, the turn it gives is sent
    // to the server like any other input.
    let yaw_deg = (-d.x).atan2(d.z).to_degrees();
    let pitch_deg = (-d.y).atan2(horizontal).to_degrees();
    (dmath::deg_to_turn(yaw_deg), (pitch_deg.clamp(-90.0, 90.0) * 65536.0 / 360.0) as i16)
}

impl Agent {
    pub fn new(session: Session) -> Self {
        Self {
            yaw: 0,
            pitch: 0,
            tasks: VecDeque::new(),
            log: VecDeque::new(),
            next_event: 1,
            quit_requested: false,
            session,
        }
    }

    /// Whether commands are moving the player right now. A window lets go of the keyboard then.
    pub fn is_driving(&self) -> bool {
        !self.tasks.is_empty()
    }

    pub fn view(&self) -> (u16, i16) {
        (self.yaw, self.pitch)
    }

    pub fn set_view(&mut self, yaw: u16, pitch: i16) {
        self.yaw = yaw;
        self.pitch = pitch;
    }

    fn eye(&self) -> DVec3 {
        self.session.body.pos + DVec3::Y * EYE_HEIGHT
    }

    fn log_event(&mut self, value: Value) {
        let seq = self.next_event;
        self.next_event += 1;
        self.log.push_back(LoggedEvent { seq, value });
        if self.log.len() > EVENT_LOG {
            self.log.pop_front();
        }
    }

    /// Notes what the session reports, keeping the events for whoever owns the session.
    pub fn digest_events(&mut self, events: &[Event]) {
        for e in events {
            let logged = match e {
                Event::Chat(line) => Some(json!({"type": "chat", "from": line.from, "text": line.text})),
                Event::Hurt => Some(json!({"type": "hurt", "health": self.session.health})),
                Event::Died => Some(json!({"type": "died"})),
                Event::Disconnected(why) => Some(json!({"type": "disconnected", "why": why})),
                Event::ScreenOpened(kind) => Some(json!({"type": "screen_opened", "screen": format!("{kind:?}")})),
                Event::ScreenClosed => Some(json!({"type": "screen_closed"})),
                Event::ItemGained { item, count } => {
                    let name = self.session.reg.item(*item).map_or("?", |i| i.name.as_str()).to_string();
                    Some(json!({"type": "item_gained", "item": name, "count": count}))
                }
                _ => None,
            };
            if let Some(value) = logged {
                self.log_event(value);
            }
        }
    }

    /// One tick of the agent: runs its tasks and returns what to send as this tick's input.
    pub fn tick_input(&mut self) -> MoveInput {
        let mut keys = 0u8;
        if let Some((mut task, reply)) = self.tasks.pop_front() {
            match self.run_task(&mut task, &mut keys) {
                Some(answer) => {
                    let _ = reply.send(answer);
                }
                None => self.tasks.push_front((task, reply)),
            }
        }
        MoveInput { keys: Keys(keys), yaw: self.yaw, pitch: self.pitch }
    }

    /// Plays one tick with no window: network in, task, physics, network out.
    pub fn step(&mut self) {
        self.session.poll();
        let events = std::mem::take(&mut self.session.events);
        self.digest_events(&events);
        let input = self.tick_input();
        self.session.tick(input);
    }

    fn face_towards(&mut self, point: DVec3) {
        let (y, p) = yaw_pitch_towards(self.eye(), point);
        self.yaw = y;
        self.pitch = p;
    }

    fn run_task(&mut self, task: &mut Task, keys: &mut u8) -> Option<Value> {
        match task {
            Task::WaitReady { until } => {
                let radius = (self.session.view_radius).min(3);
                if self.session.ready_within(radius) && self.session.pending_chunks() < 8 {
                    return Some(ok(self.state()));
                }
                (Instant::now() > *until).then(|| err("still waiting for chunks"))
            }
            Task::Walk { keys: held, until } => {
                *keys = *held;
                (Instant::now() > *until).then(|| ok(self.state()))
            }
            Task::Jump { left_ground, until } => {
                if self.session.body.on_ground && !*left_ground {
                    *keys = Keys::JUMP;
                }
                if !self.session.body.on_ground {
                    *left_ground = true;
                }
                if (*left_ground && self.session.body.on_ground) || Instant::now() > *until {
                    return Some(ok(self.state()));
                }
                None
            }
            Task::Goto { .. } => self.run_goto(task, keys),
            Task::Break { pos, before, until, started, gained_from } => {
                let (pos, before, gained_from) = (*pos, *before, *gained_from);
                if self.session.block(pos) != before {
                    let gained: Vec<Value> = self
                        .log
                        .iter()
                        .filter(|l| l.seq >= gained_from && l.value["type"] == "item_gained")
                        .map(|l| l.value.clone())
                        .collect();
                    return Some(ok(json!({ "broken": self.session.reg.block(before).name, "gained": gained })));
                }
                if Instant::now() > *until {
                    self.session.send(&C2S::CancelDig);
                    return Some(err("timed out mining it"));
                }
                self.face_towards(pos.center());
                if !*started {
                    *started = true;
                    self.session.dig(pos, Dir::Up as u8);
                }
                None
            }
            // Replying only once the server has said so: whoever asked may look straight away.
            Task::Place { pos, kind, until } => {
                if self.session.block(*pos).kind() == *kind {
                    return Some(ok(
                        json!({ "placed": self.session.reg.block(BlockState::of(*kind)).name, "at": [pos.x, pos.y, pos.z] }),
                    ));
                }
                (Instant::now() > *until).then(|| err("it did not go there: something may be in the way"))
            }
            Task::Attack { entity, until } => {
                let id = *entity;
                let Some(e) = self.session.entities.get(&id) else {
                    return Some(ok(json!({ "attacked": id, "gone": true })));
                };
                let to = e.pos;
                self.face_towards(to + DVec3::Y * e.kind.size().1 * 0.5);
                if Instant::now() > *until {
                    return Some(err("could not reach it in time"));
                }
                let d = to.distance(self.session.body.pos);
                if d > 3.5 {
                    *keys = Keys::FORWARD | Keys::SPRINT;
                    return None;
                }
                self.session.send(&C2S::Attack { entity: id });
                Some(ok(json!({ "attacked": id })))
            }
            Task::Eat { item, had, until } => {
                let left: u32 =
                    self.session.inventory.iter().flatten().filter(|s| s.item == *item).map(|s| s.count as u32).sum();
                if left < *had {
                    let name = self.session.reg.item(*item).map(|d| d.name.clone());
                    return Some(ok(json!({ "ate": name, "food": self.session.food })));
                }
                if Instant::now() > *until {
                    return Some(err("could not eat it: a full stomach takes nothing"));
                }
                self.session.send(&C2S::UseItem);
                None
            }
            Task::Craft(job) => self.run_craft(job),
            Task::Respawn { until } => {
                if self.session.health > 0 {
                    return Some(ok(self.state()));
                }
                (Instant::now() > *until).then(|| err("the server did not bring us back"))
            }
        }
    }

    fn run_goto(&mut self, task: &mut Task, keys: &mut u8) -> Option<Value> {
        let Task::Goto { goal, near, until, sprint, route, at, replan_in, stuck, last, then } = task else {
            return None;
        };
        let pos = self.session.body.pos;
        let arrived = pos.distance(*goal) <= *near;
        if arrived {
            if let Some(next) = then.take() {
                *task = *next;
                return None;
            }
            return Some(ok(self.state()));
        }
        if Instant::now() > *until {
            return Some(err(format!("did not get there in time; stopped at {:.1} {:.1} {:.1}", pos.x, pos.y, pos.z)));
        }
        *replan_in = replan_in.saturating_sub(1);
        if pos.distance_squared(*last) < 0.0009 {
            *stuck += 1;
        } else {
            *stuck = 0;
        }
        *last = pos;
        let feet = BlockPos::from_vec(pos + DVec3::Y * 0.1);
        if *at >= route.len() || *replan_in == 0 || *stuck > 10 {
            let target = BlockPos::from_vec(*goal);
            route.clear();
            route.extend(path::find(&self.session.world, &self.session.reg, feet, target, 4000).unwrap_or_default());
            *at = 0;
            *replan_in = 60;
            *stuck = 0;
            if route.is_empty() {
                // Nothing known leads there. Head straight for it and see what arrives.
                let (yaw, _) = yaw_pitch_towards(pos, *goal);
                self.yaw = yaw;
                self.pitch = 0;
                *keys = Keys::FORWARD | Keys::JUMP;
                return None;
            }
        }
        // Skip ahead over waypoints already reached.
        while *at < route.len() {
            let w = route[*at];
            let c = DVec3::new(w.x as f64 + 0.5, w.y as f64, w.z as f64 + 0.5);
            let flat = ((c.x - pos.x).powi(2) + (c.z - pos.z).powi(2)).sqrt();
            if flat < 0.35 && (c.y - pos.y).abs() < 1.1 {
                *at += 1;
            } else {
                break;
            }
        }
        let &w = route.get(*at)?;
        let c = DVec3::new(w.x as f64 + 0.5, w.y as f64, w.z as f64 + 0.5);
        let (yaw, _) = yaw_pitch_towards(pos, c);
        self.yaw = yaw;
        self.pitch = 0;
        let mut held = Keys::FORWARD;
        if *sprint {
            held |= Keys::SPRINT;
        }
        let in_water = self.session.body.in_water;
        if c.y > pos.y + 0.5 || in_water || *stuck > 4 {
            held |= Keys::JUMP;
        }
        *keys = held;
        None
    }

    fn run_craft(&mut self, job: &mut CraftJob) -> Option<Value> {
        if Instant::now() > job.until {
            return Some(err("crafting took too long"));
        }
        if job.settle > 0 {
            job.settle -= 1;
            return None;
        }
        if let Some((slot, button)) = job.steps.pop_front() {
            self.session.send(&C2S::Click { slot, button });
            // Let the server's answer arrive before planning from the inventory again.
            if job.steps.is_empty() {
                job.settle = 2;
            }
            return None;
        }
        // The grid is set: is the result there?
        let result = self.session.screen.as_ref().and_then(|s| s.slots.last().copied().flatten());
        match result {
            Some(r) if r.item == job.item => {
                let result_slot = slots::SCREEN + (job.size * job.size) as u16;
                self.session.send(&C2S::Click { slot: result_slot, button: ClickButton::Quick });
                job.made += r.count as u32;
                job.times_left = job.times_left.saturating_sub(1);
                job.settle = 2;
                if job.times_left == 0 {
                    if job.close_after {
                        self.session.send(&C2S::CloseScreen);
                    }
                    return Some(ok(json!({ "made": job.made })));
                }
                // Shift-clicking crafts as much as the grid holds; refill it if more were asked for.
                match self.plan_grid(job.item, job.size) {
                    Ok(steps) => job.steps = steps.into(),
                    Err(_) => {
                        if job.close_after {
                            self.session.send(&C2S::CloseScreen);
                        }
                        return Some(ok(json!({ "made": job.made, "stopped": "ran out of ingredients" })));
                    }
                }
                None
            }
            _ => {
                job.settle += 1;
                if job.settle > 40 {
                    return Some(err("the ingredients are in the grid, but it makes nothing"));
                }
                None
            }
        }
    }

    /// The clicks that lay out a recipe in the crafting grid from what is in the inventory.
    fn plan_grid(&self, item: u16, size: usize) -> Result<Vec<(u16, ClickButton)>, String> {
        let reg = self.session.reg.clone();
        let recipes = reg.recipes_for(item);
        if recipes.is_empty() {
            return Err("nothing crafts that".to_string());
        }
        let mut last_problem = String::from("no recipe fits");
        'recipes: for recipe in recipes {
            // Each cell of the grid that needs an ingredient, by name.
            let mut cells: Vec<(usize, String)> = Vec::new();
            if recipe.pattern.is_empty() {
                if recipe.inputs.len() > size * size {
                    continue;
                }
                cells.extend(recipe.inputs.iter().cloned().enumerate());
            } else {
                let height = recipe.pattern.len();
                let width = recipe.pattern.iter().map(|r| r.chars().count()).max().unwrap_or(0);
                if height > size || width > size {
                    last_problem = "that recipe needs a workbench: stand next to one".to_string();
                    continue;
                }
                for (y, row) in recipe.pattern.iter().enumerate() {
                    for (x, c) in row.chars().enumerate() {
                        if c != ' ' {
                            let Some(name) = recipe.key.get(&c) else { continue 'recipes };
                            cells.push((y * size + x, name.clone()));
                        }
                    }
                }
            }
            // Pick a stack for every cell, without counting the same item twice.
            let mut left: Vec<Option<ItemStack>> = self.session.inventory.clone();
            let mut steps = Vec::new();
            for (cell, ingredient) in &cells {
                let found = left.iter().position(|s| s.is_some_and(|s| reg.item_matches(s.item, ingredient)));
                let Some(from) = found else {
                    last_problem = format!("missing {ingredient}");
                    continue 'recipes;
                };
                if let Some(s) = &mut left[from] {
                    s.count -= 1;
                    if s.count == 0 {
                        left[from] = None;
                    }
                }
                // Pick the stack up, put one in the cell, put the rest back.
                steps.push((from as u16, ClickButton::Left));
                steps.push((slots::SCREEN + *cell as u16, ClickButton::Right));
                steps.push((from as u16, ClickButton::Left));
            }
            return Ok(steps);
        }
        Err(last_problem)
    }

    /// Takes a command. The reply comes through `reply`, now or when it is done.
    pub fn command(&mut self, name: &str, args: &Value, reply: Reply) {
        let Some(spec) = commands::find(name) else {
            let _ = reply.send(err(format!("unknown command {name}")));
            return;
        };
        for a in spec.args {
            if a.required && args.get(a.name).is_none_or(Value::is_null) {
                let _ = reply.send(err(format!("{name} needs {}", a.name)));
                return;
            }
        }
        if spec.needs_gpu {
            let _ = reply.send(err("this needs the game itself; a headless player has no screen"));
            return;
        }
        match self.dispatch(spec.name, args, &reply) {
            Ok(Some(task)) => self.tasks.push_back((task, reply)),
            Ok(None) => {}
            Err(message) => {
                let _ = reply.send(err(message));
            }
        }
    }

    fn dispatch(&mut self, name: &str, a: &Value, reply: &Reply) -> Result<Option<Task>, String> {
        let int = |key: &str| {
            a.get(key).and_then(Value::as_i64).map(|v| v as i32).ok_or_else(|| format!("{key} must be a whole number"))
        };
        let num = |key: &str| a.get(key).and_then(Value::as_f64).ok_or_else(|| format!("{key} must be a number"));
        let text = |key: &str| {
            a.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("{key} must be text"))
        };
        let flag = |key: &str| a.get(key).and_then(Value::as_bool).unwrap_or(false);
        let seconds = |key: &str, default: f64| {
            Duration::from_secs_f64(a.get(key).and_then(Value::as_f64).unwrap_or(default).clamp(0.0, 3600.0))
        };
        let answer = |value: Value| {
            let _ = reply.send(ok(value));
        };
        let block_pos = || -> Result<BlockPos, String> { Ok(BlockPos::new(int("x")?, int("y")?, int("z")?)) };

        match name {
            "state" => answer(self.state()),
            "block" => {
                let p = block_pos()?;
                if !self.session.world.is_loaded(p) {
                    return Err("that chunk is not loaded".into());
                }
                let s = self.session.block(p);
                answer(json!({ "name": self.session.reg.block(s).name, "state": s.meta() }));
            }
            "blocks" => {
                answer(self.blocks_in(int("x1")?, int("y1")?, int("z1")?, int("x2")?, int("y2")?, int("z2")?)?)
            }
            "find_block" => {
                let radius = a.get("radius").and_then(Value::as_i64).unwrap_or(32).clamp(1, 96) as i32;
                answer(self.find_block(&text("block")?, radius)?);
            }
            "entities" => {
                let radius = a.get("radius").and_then(Value::as_f64).unwrap_or(32.0);
                answer(self.entities_near(radius, a.get("kind").and_then(Value::as_str)));
            }
            "inventory" => answer(self.inventory()),
            "recipes" => answer(self.recipes(&text("item")?)?),
            "events" => {
                let since = a.get("since").and_then(Value::as_u64).unwrap_or(0);
                let list: Vec<Value> = self
                    .log
                    .iter()
                    .filter(|l| l.seq > since)
                    .map(|l| {
                        let mut v = l.value.clone();
                        v["seq"] = json!(l.seq);
                        v
                    })
                    .collect();
                answer(json!({ "events": list, "latest": self.next_event - 1 }));
            }
            "messages" => {
                let n = a.get("n").and_then(Value::as_u64).unwrap_or(20) as usize;
                let lines: Vec<Value> = self
                    .session
                    .chat
                    .iter()
                    .rev()
                    .take(n)
                    .rev()
                    .map(|l| json!({"from": l.from, "text": l.text}))
                    .collect();
                answer(json!({ "messages": lines }));
            }
            "wait_ready" => return Ok(Some(Task::WaitReady { until: Instant::now() + seconds("seconds", 60.0) })),
            "goto" => {
                let goal = DVec3::new(num("x")?, num("y")?, num("z")?);
                return Ok(Some(self.goto_task(
                    goal,
                    a.get("near").and_then(Value::as_f64).unwrap_or(1.0),
                    seconds("timeout", 60.0),
                    flag("sprint"),
                    None,
                )));
            }
            "walk" => {
                let mut keys = match text("direction")?.as_str() {
                    "forward" => Keys::FORWARD,
                    "back" => Keys::BACK,
                    "left" => Keys::LEFT,
                    "right" => Keys::RIGHT,
                    other => return Err(format!("{other} is not a direction")),
                };
                if flag("sprint") {
                    keys |= Keys::SPRINT;
                }
                if flag("jump") {
                    keys |= Keys::JUMP;
                }
                return Ok(Some(Task::Walk { keys, until: Instant::now() + seconds("seconds", 1.0) }));
            }
            "jump" => {
                return Ok(Some(Task::Jump { left_ground: false, until: Instant::now() + Duration::from_secs(5) }));
            }
            "look" => {
                self.yaw = dmath::deg_to_turn(num("yaw")?);
                self.pitch = (num("pitch")?.clamp(-90.0, 90.0) * 65536.0 / 360.0) as i16;
                answer(self.state());
            }
            "look_at" => {
                self.face_towards(DVec3::new(num("x")?, num("y")?, num("z")?));
                answer(self.state());
            }
            "fly" => {
                if self.session.mode != GameMode::Creative {
                    return Err("flying is for creative mode".into());
                }
                let on = !flag("off");
                self.session.body.flying = on;
                self.session.send(&C2S::SetFlying(on));
                answer(self.state());
            }
            "break" => {
                let pos = block_pos()?;
                let before = self.session.block(pos);
                if before.is_air() {
                    return Err("there is nothing there".into());
                }
                let reg = self.session.reg.clone();
                let ticks =
                    reg.dig_ticks(before, self.session.held().map(|s| s.item)).ok_or("that cannot be broken")?;
                let limit = Duration::from_secs_f64((ticks as f64 / 20.0 + 5.0).max(5.0));
                let dig = Task::Break {
                    pos,
                    before,
                    until: Instant::now() + limit,
                    started: false,
                    gained_from: self.next_event,
                };
                if self.eye().distance(pos.center()) <= REACH {
                    return Ok(Some(dig));
                }
                return Ok(Some(self.goto_task(pos.center(), REACH - 1.0, Duration::from_secs(60), false, Some(dig))));
            }
            "place" => return self.place(block_pos()?, &text("item")?).map(Some),
            "interact" => {
                let pos = block_pos()?;
                self.face_towards(pos.center());
                self.session.send(&C2S::UseOn { pos, face: Dir::Up as u8 });
                answer(json!({ "used": self.session.reg.block(self.session.block(pos)).name }));
            }
            "eat" => {
                let reg = self.session.reg.clone();
                let food = (0..9).find(|&i| {
                    self.session.inventory[i]
                        .is_some_and(|s| matches!(reg.item(s.item).map(|d| d.kind), Some(ItemKind::Food { .. })))
                });
                let Some(slot) = food else { return Err("no food in the hotbar".into()) };
                self.session.send(&C2S::SelectSlot(slot as u8));
                self.session.selected = slot as u8;
                let item = self.session.inventory[slot].map_or(0, |s| s.item);
                let had: u32 =
                    self.session.inventory.iter().flatten().filter(|s| s.item == item).map(|s| s.count as u32).sum();
                return Ok(Some(Task::Eat { item, had, until: Instant::now() + Duration::from_secs(4) }));
            }
            "attack" => {
                let entity = int("entity")? as u32;
                if !self.session.entities.contains_key(&entity) {
                    return Err("no such entity nearby".into());
                }
                return Ok(Some(Task::Attack { entity, until: Instant::now() + Duration::from_secs(20) }));
            }
            "use" => {
                self.session.send(&C2S::UseItem);
                answer(json!({ "used": true }));
            }
            "select" => {
                let slot = int("slot")?;
                if !(0..9).contains(&slot) {
                    return Err("a hotbar slot is 0 to 8".into());
                }
                self.session.send(&C2S::SelectSlot(slot as u8));
                answer(json!({ "selected": slot }));
            }
            "drop" => {
                self.session.send(&C2S::DropItem { all: flag("all") });
                answer(json!({ "dropped": true }));
            }
            "craft" => {
                let item_name = text("item")?;
                let item = self
                    .session
                    .reg
                    .item_by_name(&item_name)
                    .ok_or_else(|| format!("there is no item called {item_name}"))?;
                let times = a.get("count").and_then(Value::as_u64).unwrap_or(1).clamp(1, 64) as u32;
                return self.start_craft(item, times).map(Some);
            }
            "click_slot" => {
                let button = match a.get("button").and_then(Value::as_str).unwrap_or("left") {
                    "left" => ClickButton::Left,
                    "right" => ClickButton::Right,
                    "quick" => ClickButton::Quick,
                    other => return Err(format!("{other} is not a button")),
                };
                self.session.send(&C2S::Click { slot: int("slot")? as u16, button });
                answer(json!({ "clicked": true }));
            }
            "close_screen" => {
                self.session.send(&C2S::CloseScreen);
                answer(json!({ "closed": true }));
            }
            "chat" => {
                self.session.send(&C2S::Chat(text("text")?));
                answer(json!({ "said": true }));
            }
            "respawn" => {
                self.session.send(&C2S::Respawn);
                return Ok(Some(Task::Respawn { until: Instant::now() + Duration::from_secs(10) }));
            }
            "quit" => {
                self.quit_requested = true;
                answer(json!({ "bye": true }));
            }
            other => return Err(format!("{other} is not implemented")),
        }
        Ok(None)
    }

    fn goto_task(&self, goal: DVec3, near: f64, timeout: Duration, sprint: bool, then: Option<Task>) -> Task {
        Task::Goto {
            goal,
            near,
            until: Instant::now() + timeout,
            sprint,
            route: Vec::new(),
            at: 0,
            replan_in: 0,
            stuck: 0,
            last: self.session.body.pos,
            then: then.map(Box::new),
        }
    }

    fn start_craft(&mut self, item: u16, times: u32) -> Result<Task, String> {
        // A 3x3 recipe needs a bench: use one in reach if there is one, opening it if needed.
        let open_bench = self.session.screen.as_ref().is_some_and(|s| s.kind == ScreenKind::Bench);
        let bench = self.nearby_bench();
        let (size, close_after) = if open_bench {
            (3, false)
        } else if self.plan_grid(item, 2).is_ok() {
            (2, false)
        } else if let Some(pos) = bench {
            self.session.send(&C2S::UseOn { pos, face: Dir::Up as u8 });
            (3, true)
        } else {
            self.plan_grid(item, 2)?;
            (2, false)
        };
        // Planned against what is known now; a bench that just opened changes nothing in the pockets.
        let steps = self.plan_grid(item, size)?;
        Ok(Task::Craft(Box::new(CraftJob {
            steps: steps.into(),
            item,
            times_left: times,
            made: 0,
            size,
            until: Instant::now() + Duration::from_secs(20),
            close_after,
            settle: 3,
        })))
    }

    fn nearby_bench(&self) -> Option<BlockPos> {
        let at = BlockPos::from_vec(self.eye());
        for dy in -4..=4 {
            for dz in -4..=4 {
                for dx in -4..=4 {
                    let p = at.add(dx, dy, dz);
                    if self.session.block(p).kind() == zenith_content::id::BENCH
                        && self.eye().distance(p.center()) <= REACH
                    {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    fn place(&mut self, pos: BlockPos, item_name: &str) -> Result<Task, String> {
        let reg = self.session.reg.clone();
        let item = reg.item_by_name(item_name).ok_or_else(|| format!("there is no item called {item_name}"))?;
        let ItemKind::Block(kind) = reg.item(item).map_or(ItemKind::Material, |d| d.kind) else {
            return Err(format!("{item_name} is not a block"));
        };
        if self.eye().distance(pos.center()) > REACH {
            return Err("that is out of reach: goto somewhere closer first".into());
        }
        let creative = self.session.mode == GameMode::Creative;
        let slot = self.session.inventory.iter().position(|s| s.is_some_and(|s| s.item == item));
        match slot {
            Some(s) if s < 9 => self.session.send(&C2S::SelectSlot(s as u8)),
            Some(_) => return Err(format!("{item_name} is in the inventory but not the hotbar")),
            None if creative => return Err(format!("{item_name} is not in the hotbar: pick it first")),
            None => return Err(format!("no {item_name} in the inventory")),
        }
        // Against the first neighbour that is there: below first, as that suits nearly everything.
        let order = [Dir::Down, Dir::North, Dir::South, Dir::West, Dir::East, Dir::Up];
        let support =
            order.iter().copied().find(|&d| reg.lut.collides[self.session.block(pos.offset(d)).kind() as usize]);
        let Some(d) = support else { return Err("nothing next to that position to build against".into()) };
        // A shape that hangs from something needs that side, not just any.
        let shape = reg.block(BlockState::of(kind)).shape;
        if matches!(shape, Shape::Ladder) && d == Dir::Down {
            return Err("a ladder goes on a wall".into());
        }
        self.face_towards(pos.center());
        self.session.send(&C2S::UseOn { pos: pos.offset(d), face: d.opposite() as u8 });
        Ok(Task::Place { pos, kind, until: Instant::now() + Duration::from_secs(2) })
    }

    /// The player, the world and the crosshair in one value.
    pub fn state(&self) -> Value {
        let s = &self.session;
        let b = &s.body;
        let look = MoveInput { keys: Keys(0), yaw: self.yaw, pitch: self.pitch }.look();
        let target = s.target(look, 6.0).map(|h| {
            json!({ "block": s.reg.block(h.state).name, "at": [h.pos.x, h.pos.y, h.pos.z], "face": format!("{:?}", h.face) })
        });
        let col = s.generator.column(b.pos.x, b.pos.z);
        json!({
            "position": [b.pos.x, b.pos.y, b.pos.z],
            "velocity": [b.vel.x, b.vel.y, b.vel.z],
            "on_ground": b.on_ground,
            "flying": b.flying,
            "in_water": b.in_water,
            "yaw": dmath::turn_to_deg(self.yaw),
            "pitch": self.pitch as f64 * 360.0 / 65536.0,
            "health": s.health,
            "air": s.air,
            "food": s.food,
            "armor": s.armor_points,
            "sleeping": s.sleeping,
            "mode": format!("{:?}", s.mode),
            "selected": s.selected,
            "held": s.held().map(|h| s.reg.item(h.item).map_or("?", |d| d.name.as_str()).to_string()),
            "time": s.clock.time,
            "tick": s.clock.tick,
            "raining": s.clock.rain > 128,
            "biome": zenith_world::Generator::biome(&col).name(),
            "seed": s.generator.seed,
            "target": target,
            "chunks_loaded": s.loaded_chunks(),
            "chunks_pending": s.pending_chunks(),
            "entities_tracked": s.entities.len(),
            "players": s.players.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            "corrections": s.corrections,
            "connected": s.is_connected(),
        })
    }

    fn blocks_in(&self, x1: i32, y1: i32, z1: i32, x2: i32, y2: i32, z2: i32) -> Result<Value, String> {
        let (lx, hx, ly, hy, lz, hz) = (x1.min(x2), x1.max(x2), y1.min(y2), y1.max(y2), z1.min(z2), z1.max(z2));
        if (hx - lx + 1) as i64 * (hy - ly + 1) as i64 * (hz - lz + 1) as i64 > 1 << 20 {
            return Err("a box that size would take too long; ask for less".into());
        }
        let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
        for y in ly..=hy {
            for z in lz..=hz {
                for x in lx..=hx {
                    let s = self.session.block(BlockPos::new(x, y, z));
                    if !s.is_air() {
                        *counts.entry(self.session.reg.block(s).name.clone()).or_default() += 1;
                    }
                }
            }
        }
        Ok(json!({ "counts": counts }))
    }

    fn find_block(&self, name: &str, radius: i32) -> Result<Value, String> {
        let kind = self.session.reg.block_by_name(name).ok_or_else(|| format!("there is no block called {name}"))?;
        let at = BlockPos::from_vec(self.session.body.pos);
        let mut found: Vec<(i32, BlockPos)> = Vec::new();
        for dy in -radius..=radius {
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    let p = at.add(dx, dy, dz);
                    if self.session.block(p).kind() == kind {
                        found.push((dx * dx + dy * dy + dz * dz, p));
                    }
                }
            }
        }
        found.sort_by_key(|f| f.0);
        let list: Vec<Value> = found.iter().take(16).map(|(_, p)| json!([p.x, p.y, p.z])).collect();
        Ok(json!({ "found": found.len(), "nearest": list }))
    }

    fn entities_near(&self, radius: f64, kind: Option<&str>) -> Value {
        let at = self.session.body.pos;
        let mut list: Vec<(f64, Value)> = self
            .session
            .entities
            .values()
            .filter(|e| kind.is_none_or(|k| e.kind.name() == k))
            .map(|e| (e.pos.distance(at), e))
            .filter(|(d, _)| *d <= radius)
            .map(|(d, e)| {
                let item = e.stack.and_then(|s| self.session.reg.item(s.item)).map(|d| d.name.clone());
                (d, json!({ "id": e.id, "kind": e.kind.name(), "distance": d, "at": [e.pos.x, e.pos.y, e.pos.z], "name": e.name, "item": item }))
            })
            .collect();
        list.sort_by(|a, b| a.0.total_cmp(&b.0));
        json!({ "entities": list.into_iter().map(|e| e.1).collect::<Vec<_>>() })
    }

    fn inventory(&self) -> Value {
        let s = &self.session;
        let describe = |stack: &ItemStack| json!({ "item": s.reg.item(stack.item).map_or("?", |d| d.name.as_str()), "count": stack.count, "wear": stack.damage });
        let slots: Vec<Value> = s
            .inventory
            .iter()
            .enumerate()
            .filter_map(|(i, x)| {
                x.as_ref().map(|stack| {
                    let mut v = describe(stack);
                    v["slot"] = json!(i);
                    v
                })
            })
            .collect();
        let screen = s.screen.as_ref().map(|sc| {
            json!({
                "kind": format!("{:?}", sc.kind),
                "slots": sc.slots.iter().enumerate().filter_map(|(i, x)| x.as_ref().map(|stack| {
                    let mut v = describe(stack);
                    v["slot"] = json!(i as u16 + slots::SCREEN);
                    v
                })).collect::<Vec<_>>(),
            })
        });
        json!({ "slots": slots, "selected": s.selected, "cursor": s.cursor.as_ref().map(describe), "screen": screen })
    }

    fn recipes(&self, name: &str) -> Result<Value, String> {
        let item = self.session.reg.item_by_name(name).ok_or_else(|| format!("there is no item called {name}"))?;
        let list: Vec<Value> = self
            .session
            .reg
            .recipes_for(item)
            .iter()
            .map(|r| json!({ "pattern": r.pattern, "key": r.key, "inputs": r.inputs, "makes": r.count }))
            .collect();
        Ok(json!({ "recipes": list }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aiming_agrees_with_where_physics_looks() {
        for target in [
            DVec3::new(0.0, 0.0, 5.0),
            DVec3::new(5.0, 2.0, 0.0),
            DVec3::new(-3.0, -4.0, -3.0),
            DVec3::new(0.0, 5.0, 0.01),
        ] {
            let (yaw, pitch) = yaw_pitch_towards(DVec3::ZERO, target);
            let look = MoveInput { keys: Keys(0), yaw, pitch }.look();
            assert!(look.dot(target.normalize()) > 0.999, "{target}: looked along {look}");
        }
    }
}
