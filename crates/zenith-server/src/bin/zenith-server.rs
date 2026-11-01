//! The dedicated server. Links no GPU or window code: `cargo tree -p zenith-server` proves it.
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use zenith_protocol::GameMode;
use zenith_server::{Server, ServerConfig, command, listen, load_packs};

const USAGE: &str = "\
zenith-server: a Zenith world on the network

USAGE: zenith-server [OPTIONS]

  --world DIR        where the world lives (default: world)
  --port N           TCP port (default: 25575)
  --bind ADDR        address to listen on (default: 0.0.0.0)
  --seed N           seed for a new world (default: from the clock)
  --name TEXT        server name
  --password TEXT    players must give this to join
  --op NAME          make NAME an operator (repeatable)
  --creative         new players start in creative mode
  --sim-distance N   simulated radius around players, in chunks (default: 6)
  --max-players N    (default: 200)
  --threads N        chunk worker threads (default: half the cores)
  --packs DIR        content packs, sent to clients as they join (default: <world>/packs)
  --no-mobs          no natural mob spawning
  --help

Commands typed here run as the console: /help lists them. Ctrl-C saves and stops.";

fn main() {
    if let Err(e) = run() {
        eprintln!("zenith-server: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut cfg = ServerConfig {
        world_dir: Some(PathBuf::from("world")),
        threads: std::thread::available_parallelism().map_or(2, |n| n.get() / 2).max(1),
        seed: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64),
        ..Default::default()
    };
    let (mut port, mut bind) = (25575u16, "0.0.0.0".to_string());
    let mut packs_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or_else(|| format!("{arg} needs {what}"));
        match arg.as_str() {
            "--world" => cfg.world_dir = Some(PathBuf::from(value("a folder")?)),
            "--port" => port = value("a number")?.parse().map_err(|_| "--port needs a number")?,
            "--bind" => bind = value("an address")?,
            "--seed" => cfg.seed = value("a number")?.parse().map_err(|_| "--seed needs a number")?,
            "--name" => cfg.name = value("a name")?,
            "--password" => cfg.password = Some(value("a password")?),
            "--op" => cfg.ops.push(value("a name")?),
            "--creative" => cfg.default_mode = GameMode::Creative,
            "--sim-distance" => cfg.sim_distance = value("a number")?.parse().map_err(|_| "--sim-distance needs a number")?,
            "--max-players" => cfg.max_players = value("a number")?.parse().map_err(|_| "--max-players needs a number")?,
            "--threads" => cfg.threads = value("a number")?.parse().map_err(|_| "--threads needs a number")?,
            "--packs" => packs_dir = Some(PathBuf::from(value("a folder")?)),
            "--no-mobs" => cfg.spawn_mobs = false,
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        }
    }
    let world = cfg.world_dir.clone().unwrap_or_default();
    cfg.packs = load_packs(&packs_dir.unwrap_or_else(|| world.join("packs")))?;
    let packs = cfg.packs.len();

    let stop = Arc::new(AtomicBool::new(false));
    let (addr, peers) = listen((bind.as_str(), port), stop.clone()).map_err(|e| format!("cannot listen on {bind}:{port}: {e}"))?;
    let mut server = Server::new(cfg)?;
    println!(
        "zenith-server: world {} (seed {}, {packs} content packs), listening on {addr}",
        world.display(),
        server.world.generator.seed
    );

    // Console lines come in on their own thread and run between ticks.
    let (lines_tx, lines_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });

    let tick = std::time::Duration::from_millis(50);
    let mut next = std::time::Instant::now();
    while !stop.load(Ordering::Acquire) {
        while let Ok(peer) = peers.try_recv() {
            server.add_peer(peer);
        }
        while let Ok(line) = lines_rx.try_recv() {
            let line = line.trim();
            if line == "stop" || line == "/stop" {
                stop.store(true, Ordering::Release);
                break;
            }
            for reply in command::run(&mut server, None, line.trim_start_matches('/')) {
                println!("{reply}");
            }
        }
        server.tick();
        next += tick;
        let now = std::time::Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > std::time::Duration::from_secs(2) {
            next = now;
        }
    }
    server.shutdown()?;
    println!("zenith-server: saved. Bye.");
    Ok(())
}
