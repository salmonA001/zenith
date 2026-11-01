//! The whole server, driven through the protocol the way a client drives it.
use super::*;
use std::fs;
use zenith_content::{BlockState, id};
use zenith_world::physics::{Keys, MoveInput};
use zenith_world::BlockAccess;

struct Client {
    peer: Peer,
    welcome: Option<Welcome>,
    seq: u32,
    log: Vec<S2C>,
}

impl Client {
    fn join(server: &mut Server, name: &str) -> Self {
        let (near, far) = memory_pair();
        server.add_peer(far);
        near.send(&C2S::Hello { protocol: PROTOCOL, name: name.into(), password: None });
        let mut c = Self { peer: near, welcome: None, seq: 0, log: Vec::new() };
        server.tick();
        c.drain();
        c
    }
    fn drain(&mut self) {
        while let Some(frame) = self.peer.try_recv_frame() {
            let msg: S2C = decode(&frame).expect("server messages decode");
            if let S2C::Welcome(w) = &msg {
                self.welcome = Some((**w).clone());
            }
            self.log.push(msg);
        }
    }
    fn send(&self, msg: &C2S) {
        self.peer.send(msg);
    }
    fn input(&mut self, keys: u8) {
        self.seq += 1;
        self.send(&C2S::Input { seq: self.seq, input: MoveInput { keys: Keys(keys), yaw: 0, pitch: 0 } });
    }
    fn last_body(&self) -> Option<Body> {
        self.log.iter().rev().find_map(|m| if let S2C::Me { body, .. } = m { Some(*body) } else { None })
    }
    fn saw(&self, test: impl Fn(&S2C) -> bool) -> bool {
        self.log.iter().any(test)
    }
}

fn quiet_config() -> ServerConfig {
    ServerConfig { threads: 0, seed: 77, spawn_mobs: false, ..Default::default() }
}

fn run(server: &mut Server, clients: &mut [&mut Client], ticks: u32) {
    for _ in 0..ticks {
        server.tick();
        for c in clients.iter_mut() {
            c.drain();
        }
    }
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("zenith-server-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        Self(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn joining_gives_the_seed_and_a_registry_the_client_can_read() {
    let mut s = Server::new(quiet_config()).expect("server");
    let c = Client::join(&mut s, "ann");
    let w = c.welcome.expect("welcomed");
    assert_eq!(w.seed, 77);
    assert_eq!(w.generator_version, GENERATOR_VERSION);
    let raw = decompress(&w.registry).expect("decompresses");
    let reg = Registry::from_bytes(&raw).expect("parses");
    assert_eq!(reg.block_by_name("stone"), Some(id::STONE));
}

#[test]
fn bad_hellos_are_refused_with_a_reason() {
    let mut s = Server::new(ServerConfig { password: Some("secret".into()), ..quiet_config() }).expect("server");
    let try_hello = |s: &mut Server, msg: C2S| -> String {
        let (near, far) = memory_pair();
        s.add_peer(far);
        near.send(&msg);
        s.tick();
        let frame = near.recv_frame_timeout(Duration::from_secs(5)).expect("a reply");
        match decode::<S2C>(&frame).expect("decodes") {
            S2C::Reject(reason) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    };
    let hello = |protocol, name: &str, password: Option<&str>| C2S::Hello { protocol, name: name.into(), password: password.map(String::from) };
    assert!(try_hello(&mut s, hello(PROTOCOL + 1, "ann", Some("secret"))).contains("protocol"));
    assert!(try_hello(&mut s, hello(PROTOCOL, "../etc", Some("secret"))).contains("name"));
    assert!(try_hello(&mut s, hello(PROTOCOL, "ann", Some("wrong"))).contains("password"));
    assert!(try_hello(&mut s, C2S::Respawn).contains("hello"));
}

#[test]
fn the_same_name_cannot_join_twice() {
    let mut s = Server::new(quiet_config()).expect("server");
    let _first = Client::join(&mut s, "ann");
    let second = Client::join(&mut s, "ann");
    assert!(second.saw(|m| matches!(m, S2C::Reject(_))));
    assert_eq!(s.players.len(), 1);
}

#[test]
fn nonsense_from_a_client_cuts_the_connection_not_the_server() {
    let mut s = Server::new(quiet_config()).expect("server");
    let mut c = Client::join(&mut s, "ann");
    c.peer.send_frame(vec![255u8, 255, 255, 255].into());
    run(&mut s, &mut [&mut c], 2);
    assert!(c.saw(|m| matches!(m, S2C::Bye(_))));
    assert!(s.players.is_empty());
}

#[test]
fn inputs_are_the_only_way_to_move_and_the_server_acknowledges_each_one() {
    let mut s = Server::new(quiet_config()).expect("server");
    let mut c = Client::join(&mut s, "ann");
    // Let the ground arrive and the player land.
    for _ in 0..60 {
        c.input(0);
        run(&mut s, &mut [&mut c], 1);
    }
    let start = c.last_body().expect("a body").pos;
    for _ in 0..20 {
        c.input(Keys::FORWARD);
        run(&mut s, &mut [&mut c], 1);
    }
    let end = c.last_body().expect("a body").pos;
    assert!(end.z - start.z > 2.0, "walked from {start} to {end}");
    assert!(c.saw(|m| matches!(m, S2C::Me { ack, .. } if *ack == c.seq)));
}

#[test]
fn untouched_chunks_cost_the_server_nothing_to_describe() {
    let mut s = Server::new(quiet_config()).expect("server");
    let mut c = Client::join(&mut s, "ann");
    // Further than anything the server has loaded: it should not load or generate anything.
    let far: Vec<ChunkPos> = (0..500).map(|i| ChunkPos::new(10_000 + i, 10_000)).collect();
    c.send(&C2S::Subscribe(far.clone()));
    run(&mut s, &mut [&mut c], 2);
    let deltas: Vec<&ChunkDelta> = c.log.iter().filter_map(|m| if let S2C::Deltas(d) = m { Some(d) } else { None }).flatten().collect();
    assert_eq!(deltas.len(), 500);
    assert!(deltas.iter().all(|d| d.data.is_empty()));
    assert!(s.world.map.chunks.keys().all(|p| p.x < 1000), "the server generated a chunk it did not need");
}

#[test]
fn block_changes_reach_those_who_subscribed_and_only_them() {
    let mut s = Server::new(quiet_config()).expect("server");
    let (mut a, mut b) = (Client::join(&mut s, "ann"), Client::join(&mut s, "bob"));
    let pos = BlockPos::new(s.spawn.x, 300, s.spawn.z);
    a.send(&C2S::Subscribe(vec![pos.chunk()]));
    run(&mut s, &mut [&mut a, &mut b], 2);
    assert!(s.world.set(pos, BlockState::of(id::STONE)));
    run(&mut s, &mut [&mut a, &mut b], 1);
    let change = |m: &S2C| matches!(m, S2C::Blocks(list) if list.iter().any(|c| c.pos == pos));
    assert!(a.saw(change));
    assert!(!b.saw(change));
}

#[test]
fn a_late_subscriber_gets_what_changed_earlier() {
    let mut s = Server::new(quiet_config()).expect("server");
    let mut c = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut c], 2);
    let pos = BlockPos::new(s.spawn.x, 100, s.spawn.z);
    assert!(s.world.set(pos, BlockState::of(id::GLASS)));
    c.send(&C2S::Subscribe(vec![pos.chunk()]));
    run(&mut s, &mut [&mut c], 2);
    let delta = c.log.iter().filter_map(|m| if let S2C::Deltas(d) = m { Some(d) } else { None }).flatten().find(|d| d.pos == pos.chunk()).expect("a delta");
    let mut copy = zenith_world::Chunk::empty(pos.chunk());
    copy.apply_encoded(&delta.unpack().expect("unpacks")).expect("applies");
    assert_eq!(copy.get(pos.x & 15, pos.y, pos.z & 15).kind(), id::GLASS);
}

#[test]
fn players_see_each_other_come_move_and_go() {
    let mut s = Server::new(quiet_config()).expect("server");
    let (mut a, mut b) = (Client::join(&mut s, "ann"), Client::join(&mut s, "bob"));
    run(&mut s, &mut [&mut a, &mut b], 3);
    let bob_id = b.welcome.as_ref().expect("welcomed").entity_id;
    assert!(a.saw(|m| matches!(m, S2C::Spawn(list) if list.iter().any(|e| e.id == bob_id && e.name.as_deref() == Some("bob")))));
    for _ in 0..80 {
        b.input(Keys::FORWARD);
        a.input(0);
        run(&mut s, &mut [&mut a, &mut b], 1);
    }
    assert!(a.saw(|m| matches!(m, S2C::Moves(list) if list.iter().any(|e| e.id == bob_id))));
    drop(b);
    run(&mut s, &mut [&mut a], 2);
    assert!(a.saw(|m| matches!(m, S2C::Despawn(list) if list.contains(&bob_id))));
    assert!(a.saw(|m| matches!(m, S2C::Chat(l) if l.text.contains("bob left"))));
}

#[test]
fn entity_moves_are_small_and_sum_to_where_it_went() {
    let mut s = Server::new(quiet_config()).expect("server");
    let (mut a, mut b) = (Client::join(&mut s, "ann"), Client::join(&mut s, "bob"));
    run(&mut s, &mut [&mut a, &mut b], 60);
    let bob_id = b.welcome.as_ref().expect("welcomed").entity_id;
    a.log.clear();
    let start = s.players.iter().find(|p| p.id == bob_id).expect("bob").sent.pos;
    for _ in 0..20 {
        b.input(Keys::FORWARD);
        run(&mut s, &mut [&mut a, &mut b], 1);
    }
    let end = s.players.iter().find(|p| p.id == bob_id).expect("bob").sent.pos;
    let mut sum = [0i32; 3];
    for m in &a.log {
        if let S2C::Moves(list) = m {
            for e in list.iter().filter(|e| e.id == bob_id) {
                for axis in 0..3 {
                    sum[axis] += e.d[axis] as i32;
                }
            }
        }
    }
    assert_eq!([start[0] + sum[0], start[1] + sum[1], start[2] + sum[2]], end);
}

#[test]
fn commands_need_an_operator_and_work_for_one() {
    let mut s = Server::new(quiet_config()).expect("server");
    let mut c = Client::join(&mut s, "ann");
    c.send(&C2S::Chat("/gamemode creative".into()));
    run(&mut s, &mut [&mut c], 5);
    assert!(c.saw(|m| matches!(m, S2C::Chat(l) if l.text.contains("operator"))));
    assert_eq!(s.players[0].mode, GameMode::Survival);

    s.players[0].op = true;
    c.send(&C2S::Chat("/gamemode creative".into()));
    run(&mut s, &mut [&mut c], 2);
    assert_eq!(s.players[0].mode, GameMode::Creative);
}

#[test]
fn chat_commands_cover_the_world_and_bad_input_only_gets_an_explanation() {
    let mut s = Server::new(ServerConfig { everyone_op: true, ..quiet_config() }).expect("server");
    let mut c = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut c], 2);
    for (line, expect) in [
        ("seed", "77"),
        ("time set noon", "6000"),
        ("time set soon", "not a time"),
        ("give nonsense", "no item"),
        ("tp a b c", "not a number"),
        ("fill 0 0 0 9000 300 9000 stone", "limit"),
        ("nope", "Unknown"),
        ("summon dragon", "no mob"),
    ] {
        let replies = command::run(&mut s, Some(0), line);
        assert!(replies.iter().any(|r| r.contains(expect)), "/{line} said {replies:?}");
    }
    assert_eq!(s.clock.time, 6000);
    let replies = command::run(&mut s, None, "gamemode creative");
    assert!(replies.iter().any(|r| r.contains("console")));
}

#[test]
fn a_world_survives_being_closed_and_reopened() {
    let dir = TempDir::new("reopen");
    let cfg = ServerConfig { world_dir: Some(dir.0.clone()), everyone_op: true, ..quiet_config() };
    let pos;
    {
        let mut s = Server::new(cfg.clone()).expect("server");
        let mut c = Client::join(&mut s, "ann");
        run(&mut s, &mut [&mut c], 2);
        pos = BlockPos::new(s.spawn.x, 150, s.spawn.z);
        assert!(s.world.set(pos, BlockState::of(id::GLOW_BLOCK)));
        let chest = pos.add(1, 0, 0);
        assert!(s.world.set(chest, BlockState::of(id::CHEST)));
        let dirt = s.world.reg.item_by_name("dirt").expect("dirt");
        *s.world.chest(chest) = vec![Some(ItemStack::new(dirt, 9)); 27];
        s.players[0].inv.add(&s.world.reg.clone(), ItemStack::new(dirt, 3));
        s.players[0].health = 13;
        s.clock.time = 1234;
        s.shutdown().expect("saved");
        drop(c);
        run(&mut s, &mut [], 2);
    }
    let mut s = Server::new(ServerConfig { seed: 999, ..cfg }).expect("reopened");
    assert_eq!(s.world.generator.seed, 77, "the saved seed wins over the config");
    assert_eq!(s.clock.time, 1234);
    assert!(s.world.saved.contains(&pos.chunk()), "the chunk is indexed");
    let mut c = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut c], 2);
    assert_eq!(s.players[0].health, 13);
    let dirt = s.world.reg.item_by_name("dirt").expect("dirt");
    assert_eq!(s.players[0].inv.slots[0], Some(ItemStack::new(dirt, 3)));
    assert_eq!(s.world.block(pos).kind(), id::GLOW_BLOCK);
    assert_eq!(s.world.chest(pos.add(1, 0, 0))[0], Some(ItemStack::new(dirt, 9)));
}

#[test]
fn a_world_from_another_generator_version_will_not_open() {
    let dir = TempDir::new("version");
    let cfg = ServerConfig { world_dir: Some(dir.0.clone()), ..quiet_config() };
    drop(Server::new(cfg.clone()).expect("created"));
    let mut level = persist::read_level(&dir.0).expect("readable").expect("written");
    level.generator_version += 1;
    persist::write_level(&dir.0, &level).expect("written");
    let error = Server::new(cfg).err().expect("refused");
    assert!(error.contains("generator"), "{error}");
}

#[test]
fn only_modified_chunks_take_up_disk() {
    let dir = TempDir::new("disk");
    let cfg = ServerConfig { world_dir: Some(dir.0.clone()), sim_distance: 5, ..quiet_config() };
    let mut s = Server::new(cfg).expect("server");
    let mut c = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut c], 3);
    let loaded = s.world.map.chunks.len();
    assert!(loaded >= 100, "{loaded} chunks loaded");
    let pos = BlockPos::new(s.spawn.x, 200, s.spawn.z);
    s.world.set(pos, BlockState::of(id::STONE));
    s.save_and_wait().expect("saved");
    assert_eq!(s.world.saved.len(), 1, "one chunk changed out of {loaded}");
    let size: u64 = fs::read_dir(dir.0.join("regions")).expect("regions").map(|e| e.expect("entry").metadata().expect("metadata").len()).sum();
    assert!(size < 80_000, "the whole save is {size} bytes");
}

/// The foundation of replays, tests and fair netcode: a run depends only on its seed and inputs.
#[test]
fn the_same_inputs_make_the_same_world_every_time() {
    let simulate = || -> u64 {
        let mut s = Server::new(ServerConfig { spawn_mobs: true, sim_distance: 4, ..quiet_config() }).expect("server");
        let mut a = Client::join(&mut s, "ann");
        let mut b = Client::join(&mut s, "bob");
        let mut rng = zenith_core::Rng::new(5);
        s.players[0].mode = GameMode::Survival;
        let at = s.players[0].body.pos;
        s.entities.spawn_mob(EntityKind::Zombie, at + DVec3::new(6.0, 2.0, 0.0));
        s.entities.spawn_mob(EntityKind::Sheep, at + DVec3::new(-6.0, 2.0, 2.0));
        for _ in 0..600 {
            a.input(rng.below(128) as u8);
            b.input(rng.below(128) as u8);
            if rng.below(20) == 0 {
                let body = a.last_body().unwrap_or_default();
                let pos = BlockPos::from_vec(body.pos).add(rng.range(-3, 3), rng.range(-2, 0), rng.range(-3, 3));
                a.send(&C2S::Dig { pos, face: 1 });
            }
            run(&mut s, &mut [&mut a, &mut b], 1);
            a.log.clear();
            b.log.clear();
        }
        s.state_hash()
    };
    assert_eq!(simulate(), simulate());
}

#[test]
fn content_packs_reach_clients_and_their_blocks_can_be_given_and_saved() {
    let dir = TempDir::new("packs");
    let pack = Registry::parse_pack(
        r#"{"name":"neon","blocks":[{"name":"neon","display":"Neon","emission":12,
            "tex":{"top":{"a":[255,0,128]},"bottom":{},"side":{}}}]}"#,
    )
    .expect("pack parses");
    let cfg = ServerConfig { world_dir: Some(dir.0.clone()), everyone_op: true, packs: vec![pack.clone()], ..quiet_config() };
    let neon;
    {
        let mut s = Server::new(cfg.clone()).expect("server");
        let c = Client::join(&mut s, "ann");
        let w = c.welcome.as_ref().expect("welcomed");
        let reg = Registry::from_bytes(&decompress(&w.registry).expect("decompresses")).expect("parses");
        neon = reg.block_by_name("neon").expect("the client knows the pack's block");
        assert!(reg.item_by_name("neon").is_some());
        assert_eq!(command::run(&mut s, Some(0), "give neon 2"), vec!["Gave 2 neon.".to_string()]);
        let pos = BlockPos::new(s.spawn.x, 200, s.spawn.z);
        s.world.set(pos, BlockState::of(neon));
        s.shutdown().expect("saved");
    }
    // Reopened with another pack added before it: the block keeps its number.
    let other = Registry::parse_pack(r#"{"name":"aaa","blocks":[{"name":"aaa","display":"A"}]}"#).expect("parses");
    let s = Server::new(ServerConfig { packs: vec![other, pack], ..cfg }).expect("reopened");
    assert_eq!(s.world.reg.block_by_name("neon"), Some(neon));
}

#[test]
fn over_tcp_a_client_can_join_and_is_told_when_the_server_stops() {
    let stop = Arc::new(AtomicBool::new(false));
    let (addr, peers) = listen("127.0.0.1:0", stop.clone()).expect("listens");
    let server = Server::new(ServerConfig { threads: 2, ..quiet_config() }).expect("server");
    let thread = {
        let stop = stop.clone();
        std::thread::spawn(move || server.run(stop, peers))
    };
    let peer = tcp_peer(std::net::TcpStream::connect(addr).expect("connects")).expect("peer");
    peer.send(&C2S::Hello { protocol: PROTOCOL, name: "ann".into(), password: None });
    let frame = peer.recv_frame_timeout(Duration::from_secs(10)).expect("an answer");
    assert!(matches!(decode::<S2C>(&frame).expect("decodes"), S2C::Welcome(_)));
    stop.store(true, Ordering::Release);
    thread.join().expect("thread").expect("clean stop");
    let mut said_bye = false;
    while let Some(frame) = peer.recv_frame_timeout(Duration::from_secs(2)) {
        said_bye |= matches!(decode::<S2C>(&frame), Ok(S2C::Bye(_)));
    }
    assert!(said_bye);
}

#[test]
fn background_loading_never_blocks_a_tick_for_long() {
    let mut s = Server::new(ServerConfig { threads: 2, sim_distance: 8, ..quiet_config() }).expect("server");
    let mut c = Client::join(&mut s, "ann");
    let mut worst = Duration::ZERO;
    for _ in 0..20 {
        let t = Instant::now();
        s.tick();
        worst = worst.max(t.elapsed());
        c.drain();
    }
    assert!(worst < Duration::from_millis(200), "a tick took {worst:?} while chunks loaded");
}
