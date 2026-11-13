//! `zenith-ctl`: everything a script or an AI needs to play.
//!
//! One table of commands (in `zenith-agent`) gives the help text, the argument checks, the MCP tools
//! and the reference page, so none of them can disagree. `play` runs a whole player with no window
//! and no GPU, so an agent needs nothing but a CPU and a network.
mod args;
mod mcp;
mod mods;

use serde_json::json;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use zenith_agent::control::{self, ControlClient};
use zenith_agent::{Agent, COMMANDS, Session};
use zenith_protocol::{GameMode, memory_pair, tcp_peer};
use zenith_server::{Server, ServerConfig};

const USAGE: &str = "\
zenith-ctl: drive Zenith from a script or an AI

USAGE:
  zenith-ctl [--endpoint FILE] <command> [arguments...]   run one command, print its JSON reply
  zenith-ctl [--endpoint FILE] mcp                        serve every command over MCP on stdio
  zenith-ctl play [options]                               a player with no window (see below)
  zenith-ctl mods [command]                               the mod library: zenith-ctl mods help
  zenith-ctl launch [game options]                        start the game with control on, and wait
                                                          (its output goes to the endpoint file's .log)
  zenith-ctl help [--markdown]                            every command and its arguments

The endpoint file says how to reach the game; it defaults to ./zenith-control.json.

PLAY OPTIONS:
  --connect ADDRESS   join a server, or else:
  --world DIR         open or create a world in this process (no server needed)
  --seed N            seed for a new world
  --creative          a new world starts in creative mode
  --name NAME         player name (default Agent)
  --view N            chunks around the player to keep (default 6)
";

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("zenith-ctl: {e}");
            2
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32, String> {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    let mut endpoint = PathBuf::from("zenith-control.json");
    if argv.first().is_some_and(|a| a == "--endpoint") {
        if argv.len() < 2 {
            return Err("--endpoint needs a file".into());
        }
        endpoint = PathBuf::from(argv.remove(1));
        argv.remove(0);
    }
    let Some(first) = argv.first().cloned() else {
        print!("{USAGE}");
        return Ok(0);
    };
    let rest = &argv[1..];
    match first.as_str() {
        "--help" | "-h" => {
            print!("{USAGE}");
            Ok(0)
        }
        "help" => {
            if rest.iter().any(|a| a == "--markdown") {
                print!("{}", zenith_agent::commands::reference_markdown());
            } else {
                print!("{USAGE}\nCOMMANDS:\n");
                for c in COMMANDS {
                    let usage: Vec<String> = c
                        .args
                        .iter()
                        .map(|a| if a.required { a.name.to_string() } else { format!("[{}]", a.name) })
                        .collect();
                    println!("  {:<14} {:<34} {}", c.name.replace('_', "-"), usage.join(" "), c.help);
                }
            }
            Ok(0)
        }
        "mcp" => mcp::serve(&endpoint).map(|()| 0),
        "mods" => {
            let answer = mods::run(rest)?;
            match answer.as_str() {
                Some(text) => print!("{text}"),
                None => println!("{}", serde_json::to_string_pretty(&answer).unwrap_or_default()),
            }
            Ok(0)
        }
        "play" => play(rest, &endpoint).map(|()| 0),
        "launch" => launch(rest, &endpoint).map(|()| 0),
        name => {
            let spec = zenith_agent::commands::find(name)
                .ok_or_else(|| format!("no command called {name}: try zenith-ctl help"))?;
            let call_args = args::parse(spec, rest)?;
            let mut client = ControlClient::connect(&endpoint, Duration::from_secs(2))?;
            let answer = client.call(spec.name, &call_args)?;
            println!("{}", serde_json::to_string_pretty(&answer).unwrap_or_default());
            Ok(if answer["ok"] == json!(true) { 0 } else { 1 })
        }
    }
}

/// On Windows a child inherits every inheritable handle of its parent, not only the ones it is given
/// as its streams. The ones this program was started with are inheritable, so without this the game
/// would quietly hold them open for as long as it runs.
#[cfg(windows)]
#[allow(unsafe_code)]
fn keep_standard_streams_from_children() {
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    let streams =
        [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()];
    for handle in streams {
        // SAFETY: these are this process's own standard streams, or null if it has none, in which
        // case the call fails harmlessly. It changes one flag on the handle and nothing else.
        unsafe {
            SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

#[cfg(not(windows))]
fn keep_standard_streams_from_children() {}

/// Starts the game beside this program with the control socket on, and waits until it answers.
fn launch(game_args: &[String], endpoint: &std::path::Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let game = exe.with_file_name(if cfg!(windows) { "zenith.exe" } else { "zenith" });
    let _ = std::fs::remove_file(endpoint);
    // The game outlives this program, so it must not hold on to the pipes of whoever ran it: a
    // `zenith-ctl launch | tail` would never end.
    let log_path = endpoint.with_extension("log");
    let log = std::fs::File::create(&log_path).map_err(|e| format!("could not write {}: {e}", log_path.display()))?;
    let log_too = log.try_clone().map_err(|e| e.to_string())?;
    keep_standard_streams_from_children();
    let mut child = std::process::Command::new(&game)
        .args(game_args)
        .arg("--control")
        .arg(endpoint)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_too)
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", game.display()))?;
    // Reports a game that fell over, rather than waiting for one that never will answer.
    let mut still_running = move || -> Result<(), String> {
        match child.try_wait() {
            Ok(Some(status)) => Err(format!("the game exited on its own: {status}")),
            _ => Ok(()),
        }
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut client = loop {
        still_running()?;
        match ControlClient::connect(endpoint, Duration::from_millis(200)) {
            Ok(c) => break c,
            Err(e) if Instant::now() > deadline => return Err(e),
            Err(_) => {}
        }
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        still_running()?;
        if let Ok(answer) = client.call("wait_ready", &json!({ "seconds": 5 }))
            && answer["ok"] == json!(true)
        {
            println!("{}", serde_json::to_string_pretty(&answer["result"]).unwrap_or_default());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("the game started, but no world became ready in two minutes".into())
}

/// A whole player with no window: joins or opens a world and answers commands until told to stop.
fn play(rest: &[String], endpoint: &std::path::Path) -> Result<(), String> {
    let (mut connect, mut world, mut seed, mut creative, mut name, mut view) =
        (None, None, 0u64, false, "Agent".to_string(), 6i32);
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let mut value = |what: &str| it.next().cloned().ok_or_else(|| format!("{arg} needs {what}"));
        match arg.as_str() {
            "--connect" => connect = Some(value("an address")?),
            "--world" => world = Some(PathBuf::from(value("a folder")?)),
            "--seed" => seed = value("a number")?.parse().map_err(|_| "--seed needs a number")?,
            "--creative" => creative = true,
            "--name" => name = value("a name")?,
            "--view" => view = value("a number")?.parse().map_err(|_| "--view needs a number")?,
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        }
    }
    // A world opened here runs on its own thread, a real server with a very short wire.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut server_thread = None;
    let peer = match (connect, world) {
        (Some(address), _) => {
            let address = if address.contains(':') { address } else { format!("{address}:25575") };
            tcp_peer(std::net::TcpStream::connect(&address).map_err(|e| format!("could not reach {address}: {e}"))?)
                .map_err(|e| e.to_string())?
        }
        (None, world_dir) => {
            let cfg = ServerConfig {
                world_dir,
                seed,
                default_mode: if creative { GameMode::Creative } else { GameMode::Survival },
                everyone_op: true,
                ..Default::default()
            };
            let server = Server::new(cfg)?;
            let (near, far) = memory_pair();
            let (tx, rx) = mpsc::channel();
            tx.send(far).map_err(|e| e.to_string())?;
            let stop_for_server = stop.clone();
            server_thread = Some(std::thread::spawn(move || server.run(stop_for_server, rx)));
            near
        }
    };
    let session = Session::connect(peer, &name, None, view, 2, Duration::from_secs(20))?;
    let mut agent = Agent::new(session);
    let control = control::start(endpoint).map_err(|e| format!("could not start the control socket: {e}"))?;
    eprintln!(
        "zenith-ctl: playing as {name}; control on 127.0.0.1:{} (details in {})",
        control.port,
        endpoint.display()
    );

    let tick = Duration::from_millis(50);
    let mut next = Instant::now();
    while !agent.quit_requested && agent.session.is_connected() {
        while let Ok(request) = control.requests.try_recv() {
            agent.command(&request.cmd, &request.args, request.reply);
        }
        agent.step();
        next += tick;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    if let Some(why) = &agent.session.disconnected {
        eprintln!("zenith-ctl: disconnected: {why}");
    }
    control.settle(Duration::from_millis(500));
    drop(agent);
    stop.store(true, std::sync::atomic::Ordering::Release);
    if let Some(t) = server_thread {
        let done: Result<Result<(), String>, _> = t.join();
        if let Ok(Err(e)) = done {
            return Err(format!("the world did not save cleanly: {e}"));
        }
    }
    let _ = std::fs::remove_file(endpoint);
    Ok(())
}
