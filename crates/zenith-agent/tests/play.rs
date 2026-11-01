//! Agents with no window, playing a real server: over memory and over TCP.
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use zenith_agent::{Agent, Session};
use zenith_content::{BlockState, ItemStack, id};
use zenith_core::BlockPos;
use zenith_core::glam::DVec3;
use zenith_protocol::*;
use zenith_server::{Server, ServerConfig, listen};
use zenith_world::BlockAccess;

/// A server and an agent, ticked together on one thread: everything is as fast as the CPU allows.
struct Game {
    server: Server,
    agent: Agent,
}

impl Game {
    fn start(cfg: ServerConfig) -> Self {
        let mut server = Server::new(ServerConfig { threads: 2, spawn_mobs: false, ..cfg }).expect("server");
        let (near, far) = memory_pair();
        server.add_peer(far);
        // The hello has to be answered by a tick, while the session waits for it.
        let (session_tx, session_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let s = Session::connect(near, "ann", None, 4, 2, Duration::from_secs(20));
            session_tx.send(s).ok();
        });
        let session = loop {
            server.tick();
            if let Ok(s) = session_rx.try_recv() {
                break s.expect("connected");
            }
        };
        let mut game = Self { server, agent: Agent::new(session) };
        game.call("wait_ready", json!({ "seconds": 60 })).expect("the world arrives");
        game
    }

    fn tick(&mut self) {
        self.server.tick();
        self.agent.step();
    }

    /// Sends a command and plays until it answers.
    fn call(&mut self, cmd: &str, args: Value) -> Result<Value, String> {
        let (tx, rx) = mpsc::channel();
        self.agent.command(cmd, &args, tx);
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Ok(answer) = rx.try_recv() {
                return if answer["ok"] == json!(true) {
                    Ok(answer["result"].clone())
                } else {
                    Err(answer["error"].as_str().unwrap_or("error").to_string())
                };
            }
            assert!(Instant::now() < deadline, "{cmd} never answered");
            self.tick();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn settle(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.tick();
        }
    }

    fn give(&mut self, name: &str, count: u8) {
        let reg = self.server.world.reg.clone();
        let item = reg.item_by_name(name).expect("item");
        self.server.players[0].inv.add(&reg, ItemStack::new(item, count));
        self.settle(2);
    }

    fn pos(&self) -> DVec3 {
        self.agent.session.body.pos
    }

    /// A level, walkable floor around the player, whatever the terrain there is like.
    fn pave(&mut self, radius: i32) -> BlockPos {
        let at = BlockPos::from_vec(self.pos());
        let floor = at.y - 1;
        for x in -radius..=radius {
            for z in -radius..=radius {
                self.server.world.set(BlockPos::new(at.x + x, floor, at.z + z), BlockState::of(id::STONE));
                for y in 0..5 {
                    self.server.world.set(BlockPos::new(at.x + x, at.y + y, at.z + z), BlockState::AIR);
                }
            }
        }
        self.settle(10);
        at
    }
}

fn quiet() -> ServerConfig {
    ServerConfig { seed: 21, ..Default::default() }
}

#[test]
fn a_client_builds_exactly_the_world_the_server_has_from_the_seed_alone() {
    let mut g = Game::start(quiet());
    g.settle(60);
    let mut compared = 0;
    for (pos, chunk) in &g.agent.session.world.chunks {
        if let Some(theirs) = g.server.world.map.get(*pos) {
            assert!(chunk == theirs, "chunk {pos:?} differs between client and server");
            compared += 1;
        }
    }
    assert!(compared > 20, "only compared {compared} chunks");
}

#[test]
fn prediction_stays_in_step_with_the_server_on_real_terrain() {
    let mut g = Game::start(quiet());
    g.settle(30);
    let spot = g.pos();
    let before = g.agent.session.corrections;
    for (dx, dz) in [(6.0, 0.0), (6.0, 6.0), (0.0, 6.0), (0.0, 0.0)] {
        let _ = g.call("goto", json!({ "x": spot.x + dx, "y": spot.y, "z": spot.z + dz, "near": 1.5, "timeout": 12 }));
    }
    g.settle(5);
    let server = g.server.players[0].body.pos;
    assert!(g.pos().distance(server) < 1.0, "client {} vs server {server}", g.pos());
    let corrections = g.agent.session.corrections - before;
    assert!(corrections < 5, "{corrections} corrections on a walk across generated terrain");
}

#[test]
fn walking_goes_where_it_was_told() {
    let mut g = Game::start(quiet());
    let at = g.pave(12);
    let goal = DVec3::new(at.x as f64 + 7.5, at.y as f64, at.z as f64 - 6.5);
    let state = g.call("goto", json!({ "x": goal.x, "y": goal.y, "z": goal.z, "near": 0.6 })).expect("arrives");
    let p = state["position"].as_array().expect("position");
    let reached = DVec3::new(p[0].as_f64().expect("x"), p[1].as_f64().expect("y"), p[2].as_f64().expect("z"));
    assert!(reached.distance(goal) < 0.8, "{reached} vs {goal}");
}

#[test]
fn walking_finds_a_way_around_walls() {
    let mut g = Game::start(quiet());
    let at = g.pave(12);
    for z in -6..=6 {
        for y in 0..4 {
            g.server.world.set(at.add(3, y, z), BlockState::of(id::STONE));
        }
    }
    g.settle(5);
    let goal = at.add(7, 0, 0).center();
    g.call("goto", json!({ "x": goal.x, "y": at.y as f64, "z": goal.z, "near": 1.0, "timeout": 30 })).expect("gets around");
    assert!(g.pos().x > at.x as f64 + 4.0);
}

#[test]
fn mining_breaks_the_block_and_the_drop_lands_in_the_inventory() {
    let mut g = Game::start(quiet());
    let at = g.pave(6);
    g.give("stone_pick", 1);
    let target = at.add(2, 0, 0);
    g.server.world.set(target, BlockState::of(id::STONE));
    g.settle(5);
    let result = g.call("break", json!({ "x": target.x, "y": target.y, "z": target.z })).expect("mines");
    assert_eq!(result["broken"], "stone");
    assert!(g.agent.session.block(target).is_air());
    assert!(g.server.world.block(target).is_air());
    g.settle(60);
    let inventory = g.call("inventory", json!({})).expect("inventory");
    let has_cobble = inventory["slots"].as_array().expect("slots").iter().any(|s| s["item"] == "cobble");
    assert!(has_cobble, "the drop was not picked up: {inventory}");
}

#[test]
fn mining_something_far_away_walks_there_first() {
    let mut g = Game::start(quiet());
    let at = g.pave(14);
    g.give("stone_pick", 1);
    let target = at.add(11, -1, 3);
    g.call("break", json!({ "x": target.x, "y": target.y, "z": target.z })).expect("mines");
    assert!(g.server.world.block(target).is_air());
}

#[test]
fn placing_builds_what_was_asked_and_reports_mistakes() {
    let mut g = Game::start(quiet());
    let at = g.pave(5);
    g.give("cobble", 3);
    let p = at.add(2, 0, 0);
    g.call("place", json!({ "x": p.x, "y": p.y, "z": p.z, "item": "cobble" })).expect("placed");
    g.settle(5);
    assert_eq!(g.server.world.block(p).kind(), id::COBBLE);
    assert_eq!(g.agent.session.block(p).kind(), id::COBBLE, "the client hears about it");

    let e = g.call("place", json!({ "x": p.x, "y": p.y + 1, "z": p.z, "item": "glass" })).expect_err("none to place");
    assert!(e.contains("no glass"), "{e}");
    let e = g.call("place", json!({ "x": p.x + 40, "y": p.y, "z": p.z, "item": "cobble" })).expect_err("too far");
    assert!(e.contains("reach"), "{e}");
    let e = g.call("place", json!({ "x": p.x, "y": 310, "z": p.z, "item": "cobble" })).expect_err("too far up");
    assert!(e.contains("reach"), "{e}");
}

#[test]
fn crafting_from_logs_to_a_pickaxe_through_a_placed_bench() {
    let mut g = Game::start(quiet());
    let at = g.pave(5);
    g.give("oak_log", 8);
    assert_eq!(g.call("craft", json!({ "item": "oak_planks", "count": 3 })).expect("planks")["made"], 12);
    g.settle(4);
    assert_eq!(g.call("craft", json!({ "item": "stick" })).expect("sticks")["made"], 4);
    g.settle(4);
    assert_eq!(g.call("craft", json!({ "item": "bench" })).expect("a bench")["made"], 1);
    g.settle(4);
    // A pick needs three across: not possible in the 2x2 grid.
    let e = g.call("craft", json!({ "item": "wood_pick" })).expect_err("needs the bench");
    assert!(e.contains("workbench"), "{e}");
    let b = at.add(2, 0, 0);
    g.call("place", json!({ "x": b.x, "y": b.y, "z": b.z, "item": "bench" })).expect("placed");
    g.settle(5);
    assert_eq!(g.call("craft", json!({ "item": "wood_pick" })).expect("a pick")["made"], 1);
    g.settle(6);
    let reg = g.server.world.reg.clone();
    let pick = reg.item_by_name("wood_pick").expect("pick");
    let p = &g.server.players[0];
    assert!(p.inv.slots.iter().flatten().any(|s| s.item == pick), "{:?}", p.inv.slots);
    assert!(p.craft.iter().all(Option::is_none), "nothing is left on the grid");
    assert!(p.inv.cursor.is_none());
}

#[test]
fn asking_about_the_world_answers_from_what_the_client_knows() {
    let mut g = Game::start(quiet());
    let at = g.pave(5);
    let log = at.add(3, 0, 0);
    g.server.world.set(log, BlockState::of(id::OAK_LOG));
    g.settle(4);
    let found = g.call("find_block", json!({ "block": "oak_log", "radius": 6 })).expect("found");
    assert_eq!(found["nearest"][0], json!([log.x, log.y, log.z]));
    let one = g.call("block", json!({ "x": log.x, "y": log.y, "z": log.z })).expect("block");
    assert_eq!(one["name"], "oak_log");
    let counts = g.call("blocks", json!({ "x1": log.x, "y1": log.y, "z1": log.z, "x2": log.x, "y2": log.y, "z2": log.z })).expect("blocks");
    assert_eq!(counts["counts"]["oak_log"], 1);
    let recipes = g.call("recipes", json!({ "item": "bench" })).expect("recipes");
    assert_eq!(recipes["recipes"][0]["key"]["#"], "#planks");
    let state = g.call("state", json!({})).expect("state");
    assert_eq!(state["health"], 20);
    assert_eq!(state["connected"], true);
    assert_eq!(state["seed"], 21);
    assert!(state["biome"].is_string());
}

#[test]
fn unknown_commands_and_bad_arguments_are_explained() {
    let mut g = Game::start(quiet());
    assert!(g.call("fly_to_the_moon", json!({})).expect_err("unknown").contains("unknown"));
    assert!(g.call("block", json!({ "x": 0 })).expect_err("missing").contains("needs y"));
    assert!(g.call("walk", json!({ "direction": "up", "seconds": 1 })).expect_err("bad").contains("not a direction"));
    assert!(g.call("screenshot", json!({ "path": "x.png" })).expect_err("no gpu").contains("windowed"));
}

#[test]
fn chat_commands_work_and_show_up_in_the_event_log() {
    let mut g = Game::start(ServerConfig { everyone_op: true, ..quiet() });
    g.call("chat", json!({ "text": "/give torch 3" })).expect("said");
    g.settle(6);
    let inventory = g.call("inventory", json!({})).expect("inventory");
    assert!(inventory["slots"].as_array().expect("slots").iter().any(|s| s["item"] == "torch"));
    g.call("chat", json!({ "text": "hello" })).expect("said");
    g.settle(4);
    let events = g.call("events", json!({})).expect("events");
    let list = events["events"].as_array().expect("events");
    assert!(list.iter().any(|e| e["type"] == "chat" && e["text"] == "hello" && e["from"] == "ann"));
    assert!(list.iter().any(|e| e["type"] == "item_gained" && e["item"] == "torch"));
    let latest = events["latest"].as_u64().expect("latest");
    let after = g.call("events", json!({ "since": latest })).expect("events");
    assert!(after["events"].as_array().expect("events").is_empty(), "nothing new");
}

#[test]
fn dying_and_coming_back() {
    let mut g = Game::start(ServerConfig { everyone_op: true, ..quiet() });
    g.call("chat", json!({ "text": "/kill" })).expect("said");
    g.settle(5);
    assert_eq!(g.agent.session.health, 0);
    let state = g.call("respawn", json!({})).expect("back");
    assert_eq!(state["health"], 20);
}

#[test]
fn modified_terrain_reaches_a_client_who_joins_later() {
    let mut server = Server::new(ServerConfig { threads: 2, seed: 9, spawn_mobs: false, ..Default::default() }).expect("server");
    let pos = BlockPos::new(server.spawn.x, 200, server.spawn.z);
    server.world.request(pos.chunk(), 0);
    while !server.world.is_loaded(pos) {
        server.world.pump();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(server.world.set(pos, BlockState::of(id::GLOW_BLOCK)));
    let (near, far) = memory_pair();
    server.add_peer(far);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tx.send(Session::connect(near, "late", None, 3, 2, Duration::from_secs(20))).ok();
    });
    let session = loop {
        server.tick();
        if let Ok(s) = rx.try_recv() {
            break s.expect("connected");
        }
    };
    let mut agent = Agent::new(session);
    let deadline = Instant::now() + Duration::from_secs(60);
    while agent.session.block(pos).kind() != id::GLOW_BLOCK {
        assert!(Instant::now() < deadline, "the change never arrived");
        server.tick();
        agent.step();
    }
}

#[test]
fn many_agents_can_play_over_real_sockets_at_full_speed() {
    let stop = Arc::new(AtomicBool::new(false));
    let (addr, peers) = listen("127.0.0.1:0", stop.clone()).expect("listens");
    let server = Server::new(ServerConfig { threads: 2, seed: 4, spawn_mobs: false, ..Default::default() }).expect("server");
    let thread = {
        let stop = stop.clone();
        std::thread::spawn(move || server.run(stop, peers))
    };
    let mut agents: Vec<Agent> = (0..8)
        .map(|i| {
            let peer = tcp_peer(std::net::TcpStream::connect(addr).expect("connects")).expect("peer");
            Agent::new(Session::connect(peer, &format!("bot{i}"), None, 3, 1, Duration::from_secs(20)).expect("joined"))
        })
        .collect();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(4) {
        for a in &mut agents {
            a.step();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    for a in &agents {
        assert!(a.session.is_connected(), "{:?}", a.session.disconnected);
        assert_eq!(a.session.players.len(), 8);
        assert!(a.session.entities.len() >= 7, "a bot sees {} others", a.session.entities.len());
        assert!(a.session.loaded_chunks() > 10);
    }
    stop.store(true, Ordering::Release);
    thread.join().expect("thread").expect("stops cleanly");
}
