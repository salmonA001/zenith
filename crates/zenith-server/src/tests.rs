//! The whole server, driven through the protocol the way a client drives it.
use super::*;
use std::fs;
use zenith_content::{BlockState, id};
use zenith_world::BlockAccess;
use zenith_world::physics::{Keys, MoveInput};

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
    let hello = |protocol, name: &str, password: Option<&str>| C2S::Hello {
        protocol,
        name: name.into(),
        password: password.map(String::from),
    };
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
    let deltas: Vec<&ChunkDelta> =
        c.log.iter().filter_map(|m| if let S2C::Deltas(d) = m { Some(d) } else { None }).flatten().collect();
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
    let delta = c
        .log
        .iter()
        .filter_map(|m| if let S2C::Deltas(d) = m { Some(d) } else { None })
        .flatten()
        .find(|d| d.pos == pos.chunk())
        .expect("a delta");
    let mut copy = zenith_world::Chunk::empty(pos.chunk());
    copy.apply_encoded(&delta.unpack().expect("unpacks")).expect("applies");
    assert_eq!(copy.get(pos.x & 15, pos.y, pos.z & 15).kind(), id::GLASS);
}

#[test]
fn falling_into_water_makes_a_splash_everyone_near_hears() {
    let mut s = Server::new(quiet_config()).expect("server");
    let mut a = Client::join(&mut s, "ann");
    let mut b = Client::join(&mut s, "bob");
    let pool = BlockPos::new(s.spawn.x, 250, s.spawn.z);
    for dz in -2..=2 {
        for dx in -2..=2 {
            s.world.set(BlockPos::new(pool.x + dx, pool.y - 1, pool.z + dz), BlockState::of(id::STONE));
            for dy in 0..3 {
                s.world.set(BlockPos::new(pool.x + dx, pool.y + dy, pool.z + dz), BlockState::of(id::WATER));
            }
        }
    }
    s.teleport(0, DVec3::new(pool.x as f64 + 0.5, pool.y as f64 + 8.0, pool.z as f64 + 0.5));
    s.teleport(1, DVec3::new(pool.x as f64 + 4.5, pool.y as f64 + 8.0, pool.z as f64 + 0.5));
    let splash = |c: &Client| c.saw(|m| matches!(m, S2C::Effect(e) if e.kind == EffectKind::Splash));
    for _ in 0..60 {
        a.input(0);
        b.input(0);
        run(&mut s, &mut [&mut a, &mut b], 1);
        if splash(&a) && splash(&b) {
            break;
        }
    }
    assert!(splash(&a), "the one who fell heard nothing");
    assert!(splash(&b), "the one beside the pool heard nothing");
}

#[test]
fn players_see_each_other_come_move_and_go() {
    let mut s = Server::new(quiet_config()).expect("server");
    let (mut a, mut b) = (Client::join(&mut s, "ann"), Client::join(&mut s, "bob"));
    run(&mut s, &mut [&mut a, &mut b], 3);
    let bob_id = b.welcome.as_ref().expect("welcomed").entity_id;
    assert!(a.saw(
        |m| matches!(m, S2C::Spawn(list) if list.iter().any(|e| e.id == bob_id && e.name.as_deref() == Some("bob")))
    ));
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
fn a_bed_is_where_you_come_back_to_and_sleeping_in_one_passes_the_night() {
    let mut s = Server::new(ServerConfig { everyone_op: true, ..quiet_config() }).expect("server");
    let mut c = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut c], 40);
    let stand = BlockPos::from_vec(s.players[0].body.pos);
    let foot = stand.offset(zenith_core::Dir::East);
    s.world.set(foot, BlockState::new(id::BED, 3));
    s.world.set(foot.offset(zenith_core::Dir::East), BlockState::new(id::BED_HEAD, 3));
    // By day: no sleep, but the bed counts.
    s.clock.time = 1000;
    c.send(&C2S::UseOn { pos: foot, face: 1 });
    run(&mut s, &mut [&mut c], 2);
    assert_eq!((s.players[0].sleeping, s.players[0].bed), (None, Some(foot)));
    // By night: asleep, and after a while it is morning.
    s.clock.time = 14_000;
    c.send(&C2S::UseOn { pos: foot.offset(zenith_core::Dir::East), face: 1 });
    run(&mut s, &mut [&mut c], 2);
    assert_eq!(s.players[0].sleeping, Some(foot));
    assert!(c.saw(|m| matches!(m, S2C::Vitals(v) if v.sleeping)));
    run(&mut s, &mut [&mut c], 110);
    assert!(s.players[0].sleeping.is_none());
    assert!(s.clock.time < 200, "still night at {}", s.clock.time);
    // Dying sends you back to the bed.
    c.send(&C2S::Chat("/kill".into()));
    run(&mut s, &mut [&mut c], 2);
    assert!(s.players[0].dead);
    c.send(&C2S::Respawn);
    run(&mut s, &mut [&mut c], 2);
    let at = s.players[0].body.pos;
    assert!((at.x - (foot.x as f64 + 0.5)).abs() < 0.01 && (at.z - (foot.z as f64 + 0.5)).abs() < 0.01, "{at}");
    // With the bed gone, back to the world's spawn, and told so.
    s.world.set(foot, BlockState::AIR);
    c.send(&C2S::Chat("/kill".into()));
    run(&mut s, &mut [&mut c], 2);
    c.send(&C2S::Respawn);
    run(&mut s, &mut [&mut c], 2);
    assert!(s.players[0].bed.is_none());
    assert!(c.saw(|m| matches!(m, S2C::Chat(l) if l.text.contains("bed is gone"))));
}

#[test]
fn what_is_written_on_a_sign_reaches_everyone_watching_and_is_kept() {
    let dir = TempDir::new("signs");
    let cfg = || ServerConfig { world_dir: Some(dir.0.clone()), everyone_op: true, ..quiet_config() };
    let mut s = Server::new(cfg()).expect("server");
    let mut ann = Client::join(&mut s, "ann");
    let mut bob = Client::join(&mut s, "bob");
    run(&mut s, &mut [&mut ann, &mut bob], 40);
    let stand = BlockPos::from_vec(s.players[0].body.pos);
    let at = stand.offset(zenith_core::Dir::East);
    let chunk = at.chunk();
    for c in [&ann, &bob] {
        c.send(&C2S::Subscribe(vec![chunk]));
    }
    // Placed from the hotbar: blank, and its writer is asked for words.
    let sign = s.world.reg.item_by_name("sign").expect("signs");
    s.players[0].inv.slots[0] = Some(ItemStack::new(sign, 1));
    s.players[0].inv.selected = 0;
    let floor = at.offset(zenith_core::Dir::Down);
    s.world.set(floor, BlockState::of(id::STONE));
    s.world.set(at, BlockState::AIR);
    ann.send(&C2S::UseOn { pos: floor, face: 1 });
    run(&mut s, &mut [&mut ann, &mut bob], 2);
    assert_eq!(s.world.block(at).kind(), id::SIGN);
    assert!(ann.saw(|m| matches!(m, S2C::EditSign(p) if *p == at)));
    // Too long, too many lines, odd characters: trimmed to what a sign holds.
    let long = "x".repeat(40);
    ann.send(&C2S::SignText { pos: at, lines: vec!["Welcome\u{7}".into(), long, "c".into(), "d".into(), "e".into()] });
    run(&mut s, &mut [&mut ann, &mut bob], 2);
    let said = |c: &Client| {
        c.log.iter().rev().find_map(|m| match m {
            S2C::Signs(list) => list.iter().find(|(p, _)| *p == at).map(|(_, l)| l.clone()),
            _ => None,
        })
    };
    let words = said(&bob).expect("bob heard what the sign says");
    assert_eq!(words.len(), 4);
    assert_eq!(words[0], "Welcome");
    assert_eq!(words[1].len(), SIGN_LINE_LENGTH);
    // Out of reach, nobody can rewrite it.
    bob.send(&C2S::SignText { pos: BlockPos::new(at.x + 50, at.y, at.z), lines: vec!["nope".into()] });
    run(&mut s, &mut [&mut ann, &mut bob], 2);
    // Kept with the world.
    s.save().expect("saved");
    drop(s);
    let mut s = Server::new(cfg()).expect("server again");
    let mut cid = Client::join(&mut s, "cid");
    run(&mut s, &mut [&mut cid], 40);
    cid.send(&C2S::Subscribe(vec![chunk]));
    run(&mut s, &mut [&mut cid], 5);
    assert_eq!(said(&cid).map(|l| l[0].clone()), Some("Welcome".to_string()));
}

#[test]
fn doors_open_both_halves_and_fences_join_what_is_beside_them() {
    let mut s = Server::new(ServerConfig { everyone_op: true, ..quiet_config() }).expect("server");
    let mut ann = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut ann], 40);
    let stand = BlockPos::from_vec(s.players[0].body.pos);
    let floor = stand.offset(zenith_core::Dir::East).offset(zenith_core::Dir::Down);
    for p in [floor, floor.offset(zenith_core::Dir::East)] {
        s.world.set(p, BlockState::of(id::STONE));
        s.world.set(p.offset(zenith_core::Dir::Up), BlockState::AIR);
        s.world.set(p.offset(zenith_core::Dir::Up).offset(zenith_core::Dir::Up), BlockState::AIR);
    }
    let door = s.world.reg.item_by_name("door").expect("doors");
    s.players[0].inv.slots[0] = Some(ItemStack::new(door, 1));
    s.players[0].inv.selected = 0;
    ann.send(&C2S::UseOn { pos: floor, face: 1 });
    run(&mut s, &mut [&mut ann], 2);
    let (low, high) =
        (floor.offset(zenith_core::Dir::Up), floor.offset(zenith_core::Dir::Up).offset(zenith_core::Dir::Up));
    assert_eq!((s.world.block(low).kind(), s.world.block(high).kind()), (id::DOOR, id::DOOR));
    assert_eq!(s.world.block(high).meta() & 8, 8, "the upper half knows it is");
    ann.send(&C2S::UseOn { pos: high, face: 2 });
    run(&mut s, &mut [&mut ann], 2);
    assert!(s.world.block(low).meta() & 4 != 0 && s.world.block(high).meta() & 4 != 0, "both halves open");
    // Breaking the top takes the bottom too.
    s.world.set(high, BlockState::AIR);
    rules::drain_updates(&mut s.world);
    assert!(s.world.block(low).is_air());
    // Two fences side by side join, and a third beside a wall joins it as well.
    let a = low;
    let b = a.offset(zenith_core::Dir::East);
    let fence = s.world.reg.item_by_name("fence").expect("fences");
    s.players[0].inv.slots[0] = Some(ItemStack::new(fence, 2));
    ann.send(&C2S::UseOn { pos: floor, face: 1 });
    ann.send(&C2S::UseOn { pos: floor.offset(zenith_core::Dir::East), face: 1 });
    run(&mut s, &mut [&mut ann], 2);
    assert_eq!(s.world.block(a).kind(), id::FENCE);
    assert_eq!(s.world.block(b).kind(), id::FENCE);
    assert!(s.world.block(a).meta() & 8 != 0, "joins east");
    assert!(s.world.block(b).meta() & 4 != 0, "joins west");
}

#[test]
fn a_rider_goes_where_the_cart_goes_gets_off_with_sneak_and_carts_are_kept_with_the_world() {
    let dir = TempDir::new("vehicles");
    let cfg = || ServerConfig { world_dir: Some(dir.0.clone()), everyone_op: true, ..quiet_config() };
    let mut s = Server::new(cfg()).expect("server");
    let mut ann = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut ann], 40);
    let stand = BlockPos::from_vec(s.players[0].body.pos);
    let start = stand.offset(zenith_core::Dir::East);
    for i in 0..12 {
        let p = start.add(i, 0, 0);
        s.world.set(p.offset(zenith_core::Dir::Down), BlockState::of(id::STONE));
        s.world.set(p, BlockState::new(id::RAIL, 1));
        s.world.set(p.offset(zenith_core::Dir::Up), BlockState::AIR);
        s.world.set(p.offset(zenith_core::Dir::Up).offset(zenith_core::Dir::Up), BlockState::AIR);
    }
    let cart = s.entities.spawn_vehicle(&s.world, EntityKind::Cart, start.center());
    run(&mut s, &mut [&mut ann], 2);
    ann.send(&C2S::UseEntity { entity: cart });
    run(&mut s, &mut [&mut ann], 2);
    assert_eq!(s.players[0].riding, Some(cart));
    assert!(ann.saw(|m| matches!(m, S2C::Vitals(v) if v.riding == Some(cart))));
    // Facing east, Forward pushes it off east, and the rider goes with it.
    let before = s.players[0].body.pos.x;
    for _ in 0..30 {
        ann.seq += 1;
        let input = MoveInput { keys: Keys(Keys::FORWARD), yaw: 49152, pitch: 0 };
        ann.send(&C2S::Input { seq: ann.seq, input });
        run(&mut s, &mut [&mut ann], 1);
    }
    let (rider, ridden) = (s.players[0].body.pos, s.entities.list[&cart].body.pos);
    assert!(rider.x > before + 2.0, "carried along: {before} to {}", rider.x);
    assert!((rider.x - ridden.x).abs() < 0.01 && (rider.z - ridden.z).abs() < 0.01, "sitting in it");
    // Sneak to get off; the cart stays.
    ann.input(Keys::SNEAK);
    run(&mut s, &mut [&mut ann], 2);
    assert!(s.players[0].riding.is_none());
    // Taken somewhere else while riding, a rider leaves the cart behind and stays where they went.
    ann.input(0);
    run(&mut s, &mut [&mut ann], 2);
    ann.send(&C2S::UseEntity { entity: cart });
    run(&mut s, &mut [&mut ann], 2);
    assert_eq!(s.players[0].riding, Some(cart));
    let away = s.players[0].body.pos + DVec3::new(0.0, 0.0, 6.0);
    command::run(&mut s, Some(0), &format!("tp {} {} {}", away.x, away.y + 1.0, away.z));
    run(&mut s, &mut [&mut ann], 2);
    assert!(s.players[0].riding.is_none());
    assert!(s.players[0].body.pos.distance(s.entities.list[&cart].body.pos) > 4.0, "not back in the cart");
    // Kept with the world, it is there when the world is opened again.
    let at = s.entities.list[&cart].body.pos;
    s.save().expect("saved");
    drop(s);
    let s = Server::new(cfg()).expect("server again");
    let back: Vec<_> = s.entities.list.values().filter(|e| e.kind == EntityKind::Cart).collect();
    assert_eq!(back.len(), 1);
    assert!(back[0].body.pos.distance(at) < 0.01);
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
    let size: u64 = fs::read_dir(dir.0.join("regions"))
        .expect("regions")
        .map(|e| e.expect("entry").metadata().expect("metadata").len())
        .sum();
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
    let cfg =
        ServerConfig { world_dir: Some(dir.0.clone()), everyone_op: true, packs: vec![pack.clone()], ..quiet_config() };
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

/// Packs come and go between runs, and item numbers with them. A chest and a pocket saved under one
/// numbering keep what they hold after the level file has moved on to another.
#[test]
fn chests_and_pockets_keep_their_items_when_packs_change_between_runs() {
    let dir = TempDir::new("renumber");
    let neon = Registry::parse_pack(r#"{"name":"neon","blocks":[{"name":"neon","display":"Neon"}]}"#).expect("parses");
    let aaa = Registry::parse_pack(r#"{"name":"aaa","blocks":[{"name":"aaa","display":"A"}]}"#).expect("parses");
    let cfg = |packs: Vec<zenith_content::Pack>| ServerConfig {
        world_dir: Some(dir.0.clone()),
        everyone_op: true,
        packs,
        ..quiet_config()
    };
    let chest_at;
    {
        let mut s = Server::new(cfg(vec![neon.clone()])).expect("server");
        let _ann = Client::join(&mut s, "ann");
        assert_eq!(command::run(&mut s, Some(0), "give neon 2"), vec!["Gave 2 neon.".to_string()]);
        chest_at = BlockPos::new(s.spawn.x, 200, s.spawn.z);
        let item = s.world.reg.item_by_name("neon").expect("an item");
        s.world.set(chest_at, BlockState::of(id::CHEST));
        let mut cells = vec![None; inventory::CHEST_SIZE];
        cells[0] = Some(ItemStack { item, count: 3, damage: 0 });
        s.world.block_entities.insert(chest_at, persist::BlockEntity::Chest(cells));
        s.shutdown().expect("saved");
    }
    // A run with a pack ahead of it moves every later number up one, and the level file with them,
    // while nobody comes and the chest's chunk stays on disk.
    {
        let mut s = Server::new(cfg(vec![aaa.clone(), neon.clone()])).expect("second run");
        assert_ne!(s.world.reg.item_by_name("neon"), None);
        s.shutdown().expect("saved");
    }
    let mut s = Server::new(cfg(vec![aaa, neon])).expect("third run");
    let mut ann = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut ann], 5);
    let item = s.world.reg.item_by_name("neon").expect("an item");
    let held: u32 =
        s.players[0].inv.slots.iter().flatten().filter(|st| st.item == item).map(|st| u32::from(st.count)).sum();
    assert_eq!(held, 2, "ann still has her neon");
    match s.world.block_entities.get(&chest_at) {
        Some(persist::BlockEntity::Chest(cells)) => {
            assert_eq!(cells[0].map(|st| (st.item, st.count)), Some((item, 3)), "and the chest still has its own");
        }
        other => panic!("no chest: {other:?}"),
    }
}

/// Files from before each kept its own list of item names go by the numbering the world had then,
/// and still do after the level file has moved on, run after run.
#[test]
fn a_player_file_with_no_names_of_its_own_is_read_by_the_old_numbering_for_good() {
    let dir = TempDir::new("legacy");
    let neon = Registry::parse_pack(r#"{"name":"neon","blocks":[{"name":"neon","display":"Neon"}]}"#).expect("parses");
    let aaa = Registry::parse_pack(r#"{"name":"aaa","blocks":[{"name":"aaa","display":"A"}]}"#).expect("parses");
    let cfg = |packs: Vec<zenith_content::Pack>| ServerConfig {
        world_dir: Some(dir.0.clone()),
        everyone_op: true,
        packs,
        ..quiet_config()
    };
    let edit = |file: &str, drop: &str| {
        let path = dir.0.join(file);
        let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&path).expect("reads")).expect("json");
        json.as_object_mut().expect("an object").remove(drop).expect("it was there");
        fs::write(&path, serde_json::to_vec(&json).expect("encodes")).expect("writes");
    };
    let old_names: Vec<String>;
    {
        let mut s = Server::new(cfg(vec![neon.clone()])).expect("server");
        let _bob = Client::join(&mut s, "bob");
        assert_eq!(command::run(&mut s, Some(0), "give neon 2"), vec!["Gave 2 neon.".to_string()]);
        old_names = s.world.reg.items().iter().map(|i| i.name.clone()).collect();
        s.shutdown().expect("saved");
    }
    // As a build from before would have left them: no list in the player file, none set aside.
    edit("players/bob.json", "items");
    edit("level.json", "legacy_items");
    // Two runs with the numbering moved on, bob away for the first of them.
    {
        let mut s = Server::new(cfg(vec![aaa.clone(), neon.clone()])).expect("second run");
        s.shutdown().expect("saved");
    }
    let level = persist::read_level(&dir.0).expect("reads").expect("a level");
    assert_eq!(level.legacy_items, old_names, "the old numbering is set aside");
    assert_ne!(level.items, old_names, "while the level moves on");
    let mut s = Server::new(cfg(vec![aaa, neon])).expect("third run");
    let mut bob = Client::join(&mut s, "bob");
    run(&mut s, &mut [&mut bob], 2);
    let item = s.world.reg.item_by_name("neon").expect("an item");
    let held: u32 =
        s.players[0].inv.slots.iter().flatten().filter(|st| st.item == item).map(|st| u32::from(st.count)).sum();
    assert_eq!(held, 2, "bob still has his neon");
}

#[test]
fn a_cast_float_waits_for_a_bite_and_pulled_then_brings_something_in() {
    let mut s = Server::new(ServerConfig { everyone_op: true, ..quiet_config() }).expect("server");
    let mut ann = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut ann], 40);
    s.players[0].mode = GameMode::Survival;
    let at = BlockPos::from_vec(s.players[0].body.pos);
    // A pond in front of her, and open air over it.
    for x in -3..=3 {
        for z in 0..=14 {
            let water = z >= 2;
            s.world.set(at.add(x, -2, z), BlockState::of(id::STONE));
            s.world.set(at.add(x, -1, z), BlockState::of(if water { id::WATER } else { id::STONE }));
            for y in 0..5 {
                s.world.set(at.add(x, y, z), BlockState::AIR);
            }
        }
    }
    command::run(&mut s, Some(0), "give fishing_rod 1");
    let rod = s.world.reg.item_by_name("fishing_rod");
    let slot = s.players[0].inv.slots.iter().position(|st| st.map(|st| st.item) == rod).expect("a rod");
    ann.send(&C2S::SelectSlot(slot as u8));
    ann.seq += 1;
    ann.send(&C2S::Input { seq: ann.seq, input: MoveInput { keys: Keys(0), yaw: 0, pitch: 0 } });
    run(&mut s, &mut [&mut ann], 3);
    ann.send(&C2S::UseItem);
    run(&mut s, &mut [&mut ann], 2);
    let biting = |s: &Server| {
        s.entities.list.values().find_map(|e| match e.data {
            Data::Float { bite, .. } => Some(bite > 0),
            _ => None,
        })
    };
    assert_eq!(biting(&s), Some(false), "the float is out");
    assert!(ann.saw(|m| matches!(m, S2C::Spawn(list) if list.iter().any(|e| e.kind == EntityKind::Float && e.owner == s.players[0].id))));
    let mut bitten = false;
    for _ in 0..700 {
        run(&mut s, &mut [&mut ann], 1);
        if biting(&s) == Some(true) {
            bitten = true;
            break;
        }
    }
    let float = s
        .entities
        .list
        .values()
        .find(|e| e.kind == EntityKind::Float)
        .map(|e| (e.body.pos - at.center(), e.body.in_water));
    assert!(bitten, "nothing bit: the float is at {float:?} from her");
    ann.send(&C2S::UseItem);
    run(&mut s, &mut [&mut ann], 80);
    assert_eq!(biting(&s), None, "the float is in");
    // One thing came in on the line, in her pockets by now or still on its way: clearing the pond
    // left other things lying about, which do not count.
    let catches: Vec<u16> =
        ["perch", "trout", "stick", "crystal"].iter().filter_map(|n| s.world.reg.item_by_name(n)).collect();
    let pocketed = s.players[0].inv.slots.iter().flatten().filter(|st| catches.contains(&st.item)).count();
    let flying = s
        .entities
        .list
        .values()
        .filter(|e| matches!(e.data, Data::Item { stack, .. } if catches.contains(&stack.item)))
        .count();
    assert_eq!(pocketed + flying, 1, "one thing on the line");
    assert!(s.players[0].xp > 0);
    assert_eq!(s.players[0].inv.held().map(|st| st.damage), Some(1), "the rod wears");
    // A float is taken in when the rod is put away.
    ann.send(&C2S::UseItem);
    run(&mut s, &mut [&mut ann], 3);
    assert_eq!(biting(&s), Some(false));
    ann.send(&C2S::SelectSlot((slot as u8 + 1) % 9));
    run(&mut s, &mut [&mut ann], 3);
    assert_eq!(biting(&s), None);
}

#[test]
fn food_laid_on_a_campfire_comes_off_cooked() {
    let mut s = Server::new(ServerConfig { everyone_op: true, ..quiet_config() }).expect("server");
    let mut ann = Client::join(&mut s, "ann");
    run(&mut s, &mut [&mut ann], 40);
    s.players[0].mode = GameMode::Survival;
    let at = BlockPos::from_vec(s.players[0].body.pos);
    let fire = at.add(1, 0, 0);
    s.world.set(fire.offset(zenith_core::Dir::Down), BlockState::of(id::STONE));
    s.world.set(fire, BlockState::of(id::CAMPFIRE));
    s.world.set(fire.offset(zenith_core::Dir::Up), BlockState::AIR);
    command::run(&mut s, Some(0), "give perch 2");
    let perch = s.world.reg.item_by_name("perch");
    let slot = s.players[0].inv.slots.iter().position(|st| st.map(|st| st.item) == perch).expect("perch");
    ann.send(&C2S::SelectSlot(slot as u8));
    run(&mut s, &mut [&mut ann], 2);
    ann.send(&C2S::UseOn { pos: fire, face: 1 });
    run(&mut s, &mut [&mut ann], 2);
    assert_eq!(s.players[0].inv.held().map(|st| st.count), Some(1), "one fish is on the fire");
    assert!(matches!(s.world.block_entities.get(&fire), Some(persist::BlockEntity::Campfire(c)) if c.len() == 1));
    run(&mut s, &mut [&mut ann], u32::from(player::CAMPFIRE_TICKS) + 60);
    let cooked = s.world.reg.item_by_name("cooked_perch").expect("an item");
    let got = s.players[0].inv.slots.iter().flatten().filter(|st| st.item == cooked).count()
        + s.entities
            .list
            .values()
            .filter(|e| matches!(e.data, Data::Item { stack, .. } if stack.item == cooked))
            .count();
    assert_eq!(got, 1, "and comes off cooked");
    assert!(matches!(s.world.block_entities.get(&fire), Some(persist::BlockEntity::Campfire(c)) if c.is_empty()));
    // Broken with something still on it, a campfire gives it back raw.
    ann.send(&C2S::UseOn { pos: fire, face: 1 });
    run(&mut s, &mut [&mut ann], 2);
    s.world.set(fire, BlockState::AIR);
    run(&mut s, &mut [&mut ann], 2);
    assert!(s.entities.list.values().any(|e| matches!(e.data, Data::Item { stack, .. } if Some(stack.item) == perch)));
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
