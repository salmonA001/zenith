//! A real MCP conversation, over real pipes, with a real player. Nothing here needs a GPU.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const CTL: &str = env!("CARGO_BIN_EXE_zenith-ctl");

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Mcp {
    _child: Killed,
    to: ChildStdin,
    from: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(endpoint: &Path) -> Self {
        let mut child = Command::new(CTL)
            .arg("--endpoint")
            .arg(endpoint)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the server starts");
        let to = child.stdin.take().expect("its stdin");
        let from = BufReader::new(child.stdout.take().expect("its stdout"));
        Self { _child: Killed(child), to, from, next_id: 1 }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.to, "{message}").expect("it is listening");
        self.to.flush().expect("it is listening");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut line = String::new();
        self.from.read_line(&mut line).expect("an answer");
        let answer: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
        assert_eq!(answer["id"], json!(id), "{answer}");
        assert_eq!(answer["jsonrpc"], json!("2.0"));
        answer
    }

    /// Calls a tool and returns what it says, parsed; the error text if it says no.
    fn tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let answer = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &answer["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or_else(|| panic!("no text in {answer}")).to_string();
        if result["isError"] == json!(true) {
            return Err(text);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
}

fn temp_dir() -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("zenith-mcp-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a folder");
    dir
}

#[test]
fn an_ai_client_can_find_the_tools_and_play_with_them() {
    let dir = temp_dir();
    let endpoint = dir.join("control.json");
    let world = dir.join("world");
    let player = Command::new(CTL)
        .arg("--endpoint")
        .arg(&endpoint)
        .args(["play", "--seed", "2026", "--creative", "--name", "Claude", "--view", "3", "--world"])
        .arg(&world)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the player starts");
    let mut player = Killed(player);

    let mut mcp = Mcp::start(&endpoint);

    // The handshake every MCP client begins with.
    let hello = mcp.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(hello["result"]["serverInfo"]["name"], json!("zenith"));
    assert!(hello["result"]["capabilities"]["tools"].is_object());
    mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}));

    let tools = mcp.request("tools/list", json!({}));
    let names: Vec<&str> =
        tools["result"]["tools"].as_array().expect("tools").iter().filter_map(|t| t["name"].as_str()).collect();
    for name in ["state", "goto", "break", "place", "craft", "wait_ready", "screenshot", "key", "mouse"] {
        assert!(names.contains(&name), "no {name} in {names:?}");
    }

    // The player takes a moment to start: the tools say so rather than hanging.
    let deadline = Instant::now() + Duration::from_secs(30);
    let state = loop {
        match mcp.tool("state", json!({})) {
            Ok(state) => break state,
            Err(e) => {
                assert!(e.contains("Is the game running"), "{e}");
                assert!(Instant::now() < deadline, "the player never came up: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    assert_eq!(state["mode"], json!("Creative"), "{state}");
    mcp.tool("wait_ready", json!({"seconds": 30})).expect("the world arrives");

    // Build something, look at it, and take it down again.
    mcp.tool("chat", json!({"text": "/give oak_planks 8"})).expect("a command");
    let position = mcp.tool("state", json!({})).expect("a state")["position"].clone();
    let (x, y, z) = (
        position[0].as_f64().expect("x").floor() as i64 + 2,
        position[1].as_f64().expect("y").floor() as i64,
        position[2].as_f64().expect("z").floor() as i64,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while mcp.tool("inventory", json!({})).expect("an inventory").to_string().find("oak_planks").is_none() {
        assert!(Instant::now() < deadline, "the planks never came");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Whatever the ground does here, this leaves stone to build on, and room above it.
    mcp.tool("chat", json!({"text": format!("/setblock {x} {} {z} stone", y - 1)})).expect("a command");
    mcp.tool("chat", json!({"text": format!("/setblock {x} {y} {z} air")})).expect("a command");
    let deadline = Instant::now() + Duration::from_secs(5);
    while mcp.tool("block", json!({"x": x, "y": y - 1, "z": z})).expect("a block")["name"] != json!("stone")
        || mcp.tool("block", json!({"x": x, "y": y, "z": z})).expect("a block")["name"] != json!("air")
    {
        assert!(Instant::now() < deadline, "the ground never changed");
        std::thread::sleep(Duration::from_millis(50));
    }
    mcp.tool("place", json!({"x": x, "y": y, "z": z, "item": "oak_planks"})).expect("it is placed");
    assert_eq!(mcp.tool("block", json!({"x": x, "y": y, "z": z})).expect("a block")["name"], json!("oak_planks"));
    mcp.tool("break", json!({"x": x, "y": y, "z": z})).expect("it is broken");
    assert_eq!(mcp.tool("block", json!({"x": x, "y": y, "z": z})).expect("a block")["name"], json!("air"));

    // Mistakes come back as tool errors an AI can read, and do not end the conversation.
    assert!(mcp.tool("block", json!({"x": 0})).expect_err("incomplete").contains("needs y"));
    assert!(mcp.tool("nope", json!({})).expect_err("unknown").contains("no tool"));
    assert!(mcp.tool("screenshot", json!({"path": "x.png"})).expect_err("no screen").contains("the game itself"));
    assert_eq!(mcp.request("nope", json!({}))["error"]["code"], json!(-32601));
    assert!(mcp.tool("state", json!({})).is_ok(), "the conversation goes on");

    // Quitting saves the world and ends the player.
    mcp.tool("quit", json!({})).expect("a goodbye");
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = player.0.try_wait().expect("a process to ask about") {
            break status;
        }
        assert!(Instant::now() < deadline, "the player did not stop");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "the player exited with {status}");
    assert!(world.join("level.json").exists(), "the world was not saved");
    assert!(mcp.tool("state", json!({})).expect_err("gone").contains("Is the game running"));
    drop(mcp);
    let _ = std::fs::remove_dir_all(&dir);
}
