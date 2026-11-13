//! A server under load: real bots over real TCP, measured the way a server measures itself.
//!
//! `cargo run --release -p zenith-server --example load -- [--bots 1,10,50,100] [--mode walk|explore|build]
//!  [--seconds 60] [--warmup 20] [--view 10]`
//!
//! Each bot does what a client does on the wire: says hello, subscribes to the chunks around it, sends
//! input every tick, and answers pings. It keeps no world of its own, so this is the server's load
//! and not the load of a hundred clients. They do share this machine with the server, which a real
//! player would not, so the numbers here err on the slow side.
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use zenith_core::{BlockPos, ChunkPos, Rng};
use zenith_protocol::*;
use zenith_server::{Server, ServerConfig, listen};
use zenith_world::physics::{Keys, MoveInput};

const TICK: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Around the spawn point, as players at a base do.
    Walk,
    /// Flying straight out, each its own way: the server has new terrain to load all the time.
    Explore,
    /// Walking, and breaking and placing a block every second, with a chat line now and then.
    Build,
}

struct Bot {
    peer: Peer,
    seq: u32,
    rng: Rng,
    yaw: u16,
    turn_in: u32,
    center: Option<ChunkPos>,
    known: HashSet<ChunkPos>,
    pos: (f64, f64, f64),
    home: Option<(f64, f64)>,
    started: bool,
}

impl Bot {
    fn connect(address: std::net::SocketAddr, n: u32, mode: Mode) -> Option<Self> {
        let stream = std::net::TcpStream::connect(address).ok()?;
        let peer = tcp_peer(stream).ok()?;
        peer.send(&C2S::Hello { protocol: PROTOCOL, name: format!("bot{n}"), password: None });
        let mut rng = Rng::new(0xB07 + n as u64);
        let yaw = (rng.next_u64() & 0xFFFF) as u16;
        if mode == Mode::Explore {
            peer.send(&C2S::SetFlying(true));
        }
        if mode == Mode::Build {
            peer.send(&C2S::Pick { item: 1 });
        }
        Some(Self {
            peer,
            seq: 0,
            rng,
            yaw,
            turn_in: 0,
            center: None,
            known: HashSet::new(),
            pos: (0.0, 0.0, 0.0),
            home: None,
            started: false,
        })
    }

    fn step(&mut self, mode: Mode, view: i32, bytes: &AtomicU64) {
        while let Some(frame) = self.peer.try_recv_frame() {
            bytes.fetch_add(frame.len() as u64, Ordering::Relaxed);
            match decode::<S2C>(&frame) {
                Ok(S2C::Ping(n)) => {
                    self.peer.send(&C2S::Pong(n));
                }
                Ok(S2C::Me { body, .. }) => {
                    self.pos = (body.pos.x, body.pos.y, body.pos.z);
                    self.home.get_or_insert((body.pos.x, body.pos.z));
                    self.started = true;
                }
                Ok(S2C::Reject(why)) | Ok(S2C::Bye(why)) => eprintln!("a bot was told: {why}"),
                _ => {}
            }
        }
        if !self.started {
            return;
        }
        self.subscribe(view);
        self.seq += 1;
        let keys = match mode {
            Mode::Explore => Keys::FORWARD | Keys::SPRINT,
            _ => {
                self.turn_in = self.turn_in.saturating_sub(1);
                if self.turn_in == 0 {
                    self.turn_in = 20 + self.rng.below(80);
                    self.yaw = (self.rng.next_u64() & 0xFFFF) as u16;
                    // Too far from home, and it heads back.
                    if let Some(home) = self.home {
                        let (dx, dz) = (home.0 - self.pos.0, home.1 - self.pos.2);
                        if dx * dx + dz * dz > 64.0 * 64.0 {
                            self.yaw = zenith_core::dmath::deg_to_turn((-dx).atan2(dz).to_degrees());
                        }
                    }
                }
                if self.rng.below(10) == 0 { Keys::FORWARD | Keys::JUMP } else { Keys::FORWARD }
            }
        };
        self.peer.send(&C2S::Input { seq: self.seq, input: MoveInput { keys: Keys(keys), yaw: self.yaw, pitch: 0 } });
        if mode == Mode::Build && self.seq.is_multiple_of(20) {
            let below =
                BlockPos::new(self.pos.0.floor() as i32, self.pos.1.floor() as i32 - 1, self.pos.2.floor() as i32);
            if self.seq.is_multiple_of(40) {
                self.peer.send(&C2S::Dig { pos: below, face: 1 });
            } else {
                self.peer.send(&C2S::UseOn { pos: below, face: 1 });
            }
        }
        if mode == Mode::Build && self.seq.is_multiple_of(200) {
            self.peer.send(&C2S::Chat("a line of chat, as players send".into()));
        }
    }

    /// As a client does: asks about the chunks around it, and stops asking about those it left.
    fn subscribe(&mut self, view: i32) {
        let c = ChunkPos::new(self.pos.0.floor() as i32 >> 4, self.pos.2.floor() as i32 >> 4);
        if self.center == Some(c) {
            return;
        }
        self.center = Some(c);
        let mut wanted = HashSet::new();
        for dz in -view..=view {
            for dx in -view..=view {
                if dx * dx + dz * dz <= view * view + view {
                    wanted.insert(ChunkPos::new(c.x + dx, c.z + dz));
                }
            }
        }
        let new: Vec<ChunkPos> = wanted.difference(&self.known).copied().collect();
        let gone: Vec<ChunkPos> = self.known.difference(&wanted).copied().collect();
        if !new.is_empty() {
            self.peer.send(&C2S::Subscribe(new));
        }
        if !gone.is_empty() {
            self.peer.send(&C2S::Unsubscribe(gone));
        }
        self.known = wanted;
    }
}

/// The memory this process holds, in megabytes, where there is a cheap way to ask.
fn resident_mb() -> Option<f64> {
    if cfg!(windows) {
        let script = format!("(Get-Process -Id {}).WorkingSet64", std::process::id());
        let out =
            std::process::Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]).output().ok()?;
        return String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().ok().map(|b| b / 1_048_576.0);
    }
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    statm.split_whitespace().nth(1)?.parse::<f64>().ok().map(|pages| pages * 4096.0 / 1_048_576.0)
}

struct Row {
    bots: u32,
    tps: f64,
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    worst: f64,
    rss: f64,
    kib_per_bot: f64,
    chunks: usize,
}

fn run(count: u32, mode: Mode, view: i32, warmup: Duration, seconds: Duration) -> Result<Row, String> {
    let dir = std::env::temp_dir().join(format!("zenith-load-{}-{count}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut server = Server::new(ServerConfig {
        world_dir: Some(dir.clone()),
        seed: 12345,
        default_mode: if mode == Mode::Walk { GameMode::Survival } else { GameMode::Creative },
        sim_distance: view,
        max_players: count as usize + 1,
        threads: (cores / 2).max(1),
        ..Default::default()
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let (address, peers) = listen("127.0.0.1:0", stop.clone()).map_err(|e| e.to_string())?;
    let bytes = Arc::new(AtomicU64::new(0));

    // A thread for every 25 bots, each joining them one at a time.
    let mut threads = Vec::new();
    let per_thread = 25;
    for first in (0..count).step_by(per_thread) {
        let (stop, bytes) = (stop.clone(), bytes.clone());
        let last = (first + per_thread as u32).min(count);
        threads.push(std::thread::spawn(move || {
            let mut bots: Vec<Bot> = Vec::new();
            let mut next = first;
            let mut next_join = Instant::now() + Duration::from_millis(first as u64 * 40);
            while !stop.load(Ordering::Acquire) {
                if next < last && Instant::now() >= next_join {
                    if let Some(bot) = Bot::connect(address, next, mode) {
                        bots.push(bot);
                    }
                    next += 1;
                    next_join = Instant::now() + Duration::from_millis(40 * per_thread as u64);
                }
                let started = Instant::now();
                for bot in &mut bots {
                    bot.step(mode, view, &bytes);
                }
                std::thread::sleep(TICK.saturating_sub(started.elapsed()));
            }
        }));
    }

    let drive = |server: &mut Server, length: Duration, mut record: Option<&mut Vec<f64>>| {
        let (begin, mut next, mut ticks) = (Instant::now(), Instant::now(), 0u64);
        while begin.elapsed() < length {
            while let Ok(peer) = peers.try_recv() {
                server.add_peer(peer);
            }
            let started = Instant::now();
            server.tick();
            if let Some(list) = record.as_deref_mut() {
                list.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            ticks += 1;
            next += TICK;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > Duration::from_secs(2) {
                next = now;
            }
        }
        ticks as f64 / begin.elapsed().as_secs_f64()
    };

    // Everyone in, and a while to settle.
    drive(&mut server, warmup + Duration::from_millis(count as u64 * 40), None);
    let bytes_before = bytes.load(Ordering::Relaxed);
    let mut durations = Vec::new();
    let tps = drive(&mut server, seconds, Some(&mut durations));
    let bytes_measured = bytes.load(Ordering::Relaxed) - bytes_before;
    let rss = resident_mb().unwrap_or(0.0);
    let chunks = server.world.map.chunks.len();

    stop.store(true, Ordering::Release);
    server.shutdown()?;
    for t in threads {
        let _ = t.join();
    }
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);

    durations.sort_by(f64::total_cmp);
    let at = |q: f64| durations[((durations.len() - 1) as f64 * q) as usize];
    Ok(Row {
        bots: count,
        tps,
        mean: durations.iter().sum::<f64>() / durations.len() as f64,
        p50: at(0.5),
        p95: at(0.95),
        p99: at(0.99),
        worst: at(1.0),
        rss,
        kib_per_bot: bytes_measured as f64 / seconds.as_secs_f64() / count as f64 / 1024.0,
        chunks,
    })
}

fn main() -> Result<(), String> {
    let (mut counts, mut mode, mut seconds, mut warmup, mut view) =
        (vec![1, 10, 25, 50, 100], Mode::Walk, 30u64, 15u64, 10i32);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
        match arg.as_str() {
            "--bots" => counts = value.split(',').filter_map(|v| v.parse().ok()).collect(),
            "--mode" => {
                mode = match value.as_str() {
                    "walk" => Mode::Walk,
                    "explore" => Mode::Explore,
                    "build" => Mode::Build,
                    other => return Err(format!("no mode called {other}")),
                }
            }
            "--seconds" => seconds = value.parse().map_err(|_| "--seconds needs a number")?,
            "--warmup" => warmup = value.parse().map_err(|_| "--warmup needs a number")?,
            "--view" => view = value.parse().map_err(|_| "--view needs a number")?,
            other => return Err(format!("unknown option {other}")),
        }
    }
    println!("| bots | TPS | mean ms | p50 | p95 | p99 | worst | process MB | KiB/s per bot | chunks held |");
    println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
    for count in counts {
        let r = run(count, mode, view, Duration::from_secs(warmup), Duration::from_secs(seconds))?;
        println!(
            "| {} | {:.1} | {:.2} | {:.2} | {:.2} | {:.2} | {:.1} | {:.0} | {:.1} | {} |",
            r.bots, r.tps, r.mean, r.p50, r.p95, r.p99, r.worst, r.rss, r.kib_per_bot, r.chunks
        );
    }
    Ok(())
}
