//! The game itself, with no window, walked through its screens by the same input a keyboard and a
//! mouse give. It needs a GPU, and says so and passes where there is none, as on most build servers.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use zenith_agent::control::ControlClient;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

/// Screen positions, in pixels, at this size. The UI's units are two pixels each.
const fn ui(x: i32, y: i32) -> (i32, i32) {
    (x * 2, y * 2)
}
const PLAY: (i32, i32) = ui(320, 172);
const SEED_FIELD: (i32, i32) = ui(320, 152);
const MODE_BUTTON: (i32, i32) = ui(320, 204);
const CREATE: (i32, i32) = ui(271, 262);
const FIRST_WORLD: (i32, i32) = ui(320, 94);
const HOTBAR_0: (i32, i32) = ui(138, 256);
const CRAFT_0: (i32, i32) = ui(156, 108);
const CRAFT_RESULT: (i32, i32) = ui(222, 118);
const BACK_TO_GAME: (i32, i32) = ui(320, 108);
const VIEW_MORE: (i32, i32) = ui(396, 130);
const LOOK: (i32, i32) = ui(320, 174);
const RAY_TRACING: (i32, i32) = ui(320, 196);
const DEBUG_OPTIONS: (i32, i32) = ui(320, 262);
const SAVE_AND_LEAVE: (i32, i32) = ui(320, 288);
const CHUNK_BORDERS: (i32, i32) = ui(239, 147);
const FULL_BRIGHT: (i32, i32) = ui(239, 235);
const NIGHT: (i32, i32) = ui(456, 103);
const COME_BACK: (i32, i32) = ui(320, 189);
const MODS: (i32, i32) = ui(320, 220);
const GET_MODS: (i32, i32) = ui(176, 313);
const INSTALL_FIRST: (i32, i32) = ui(579, 94);
const BROWSE_BACK: (i32, i32) = ui(577, 323);
const MODS_BACK: (i32, i32) = ui(462, 313);

struct Game {
    child: Child,
    client: ControlClient,
}

impl Drop for Game {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Game {
    /// Starts the game, or returns `None` if this machine has no GPU to run it on.
    fn start(home: &Path) -> Option<Self> {
        let endpoint = home.join("control.json");
        let mut child = Command::new(env!("CARGO_BIN_EXE_zenith"))
            .args([
                "--offscreen",
                "--render-distance",
                "4",
                "--width",
                &WIDTH.to_string(),
                "--height",
                &HEIGHT.to_string(),
                "--control",
            ])
            .arg(&endpoint)
            .env("ZENITH_HOME", home)
            .env("ZENITH_NO_AUDIO", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the game starts");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                eprintln!("skipped: the game exited with {status}, most likely for want of a GPU");
                return None;
            }
            if let Ok(client) = ControlClient::connect(&endpoint, Duration::from_millis(200)) {
                return Some(Self { child, client });
            }
            assert!(Instant::now() < deadline, "the game never answered");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        let answer = self.client.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(answer["ok"], json!(true), "{name}: {answer}");
        answer["result"].clone()
    }

    fn screen(&mut self) -> String {
        self.call("frame", json!({}))["screen"].as_str().expect("a screen").to_string()
    }

    /// Waits for a screen. Input takes effect on the next frame, not at once.
    fn wait_for(&mut self, want: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let now = self.screen();
            if now == want {
                return;
            }
            assert!(Instant::now() < deadline, "waited for {want}, and the screen is still {now}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn click(&mut self, at: (i32, i32)) {
        self.call("mouse", json!({"x": at.0, "y": at.1, "left": true}));
        std::thread::sleep(Duration::from_millis(120));
    }

    fn key(&mut self, name: &str) {
        self.call("key", json!({"name": name}));
    }

    fn chat(&mut self, text: &str) {
        self.call("chat", json!({"text": text}));
        std::thread::sleep(Duration::from_millis(200));
    }

    /// The count of an item, wherever in the inventory or the open screen it is.
    fn count_of(&mut self, item: &str, part: &str) -> u64 {
        let inv = self.call("inventory", json!({}));
        let list = if part == "screen" { &inv["screen"]["slots"] } else { &inv["slots"] };
        list.as_array().map_or(0, |slots| {
            slots.iter().filter(|s| s["item"] == json!(item)).map(|s| s["count"].as_u64().unwrap_or(0)).sum()
        })
    }

    fn wait_for_count(&mut self, item: &str, part: &str, want: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let now = self.count_of(item, part);
            if now == want {
                return;
            }
            assert!(Instant::now() < deadline, "waited for {want} {item} in the {part}, and there are {now}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn temp_home() -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("zenith-screens-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a folder");
    dir
}

#[test]
fn the_mods_screen_installs_a_mod_that_new_worlds_then_have() {
    let home = temp_home();
    let Some(mut g) = Game::start(&home) else { return };
    g.click(MODS);
    g.wait_for("title/mods");
    g.click(GET_MODS);
    g.wait_for("title/mods/browse");
    // The examples are inside the game, but they still arrive from a thread: click until it takes.
    let deadline = Instant::now() + Duration::from_secs(15);
    let installed = |home: &Path| {
        zenith_mods::Library::open(home).map(|l| l.installed().iter().any(|m| m.manifest.id == "neon")).unwrap_or(false)
    };
    while !installed(&home) {
        assert!(Instant::now() < deadline, "the first example never installed");
        g.click(INSTALL_FIRST);
        std::thread::sleep(Duration::from_millis(200));
    }
    g.click(BROWSE_BACK);
    g.wait_for("title/mods");
    g.click(MODS_BACK);
    g.wait_for("title");

    g.click(PLAY);
    g.wait_for("title/new_world");
    g.click(CREATE);
    g.wait_for("playing");
    g.call("wait_ready", json!({"seconds": 45}));
    g.chat("/give neon_pink 3");
    g.wait_for_count("neon_pink", "inventory", 3);
    g.call("quit", json!({}));
    drop(g);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn every_screen_works_from_the_title_to_a_saved_world_and_back() {
    let home = temp_home();
    let Some(mut g) = Game::start(&home) else { return };

    // The title. No worlds yet, so Play leads straight to making one.
    assert_eq!(g.screen(), "title");
    g.click(PLAY);
    g.wait_for("title/new_world");
    g.click(SEED_FIELD);
    g.call("type", json!({"text": "2026"}));
    g.click(MODE_BUTTON);
    let picture = home.join("shots").join("form.png");
    g.call("screenshot", json!({"path": picture.display().to_string()}));
    let png = std::fs::read(&picture).expect("a picture of the title screen");
    assert!(png.starts_with(b"\x89PNG") && png.len() > 10_000, "that is hardly a picture: {} bytes", png.len());
    g.click(CREATE);
    g.wait_for("playing");
    g.call("wait_ready", json!({"seconds": 45}));
    let state = g.call("state", json!({}));
    assert_eq!(state["mode"], json!("Creative"), "{state}");
    assert_eq!(state["seed"], json!(2026), "{state}");

    // Crafting by hand: a stack to the cursor, the cursor to the grid, the result to the inventory.
    g.chat("/give oak_log 4");
    g.wait_for_count("oak_log", "inventory", 4);
    g.key("e");
    g.wait_for("inventory");
    g.click(HOTBAR_0);
    g.click(CRAFT_0);
    g.wait_for_count("oak_log", "screen", 4);
    g.wait_for_count("oak_planks", "screen", 4);
    g.click(CRAFT_RESULT);
    g.click(HOTBAR_0);
    g.wait_for_count("oak_planks", "inventory", 4);
    g.key("escape");
    g.wait_for("playing");
    // What was left in the grid has come back.
    g.wait_for_count("oak_log", "inventory", 3);

    // The pause screen: the buttons work, and clicking on one is not also a swing at the world.
    g.key("escape");
    g.wait_for("pause");
    let view_before = g.call("frame", json!({}));
    g.click(VIEW_MORE);
    // The look starts classic, and a click paints the world again for the modern one, which is the one
    // with ray tracing.
    assert_eq!(g.call("frame", json!({}))["look"], json!("classic"));
    g.click(LOOK);
    let deadline = Instant::now() + Duration::from_secs(10);
    while g.call("frame", json!({}))["look"] != json!("modern") {
        assert!(Instant::now() < deadline, "the look button did nothing");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Ray tracing, where the machine can: the button turns it on, and it stays on from here, through
    // leaving the world and coming back. Where it cannot, the button says so and changes nothing.
    g.click(RAY_TRACING);
    let deadline = Instant::now() + Duration::from_secs(10);
    let tracing = loop {
        let now = g.call("frame", json!({}))["ray_tracing"].as_str().unwrap_or_default().to_string();
        if now != "off" {
            break now;
        }
        assert!(Instant::now() < deadline, "the ray tracing button did nothing");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(tracing == "on" || tracing.starts_with("unavailable"), "{tracing}");
    g.click(BACK_TO_GAME);
    g.wait_for("playing");
    assert_eq!(view_before["screen"], json!("pause"));

    // The debug options: F6 opens and closes them, and so does the pause screen's button. The switches
    // switch, and the time buttons are commands.
    g.key("f6");
    g.wait_for("debug");
    g.key("f6");
    g.wait_for("playing");
    g.key("escape");
    g.wait_for("pause");
    g.click(DEBUG_OPTIONS);
    g.wait_for("debug");
    assert_eq!(g.call("frame", json!({}))["debug_view"]["chunk_borders"], json!(false));
    g.click(CHUNK_BORDERS);
    g.click(FULL_BRIGHT);
    let view = g.call("frame", json!({}))["debug_view"].clone();
    assert_eq!(
        (&view["chunk_borders"], &view["full_bright"], &view["fog"]),
        (&json!(true), &json!(true), &json!(true))
    );
    g.click(NIGHT);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(18_000..19_000).contains(&g.call("state", json!({}))["time"].as_u64().unwrap_or(0)) {
        assert!(Instant::now() < deadline, "the night button did not make it night");
        std::thread::sleep(Duration::from_millis(50));
    }
    g.key("escape");
    g.wait_for("playing");

    // Chat and commands from the keyboard.
    g.key("t");
    g.wait_for("chat: ");
    g.call("type", json!({"text": "hello from the keyboard"}));
    g.wait_for("chat: hello from the keyboard");
    g.key("enter");
    g.wait_for("playing");
    std::thread::sleep(Duration::from_millis(300));
    let said = g.call("messages", json!({"n": 3}));
    assert!(said.to_string().contains("hello from the keyboard"), "{said}");
    g.key("/");
    g.wait_for("chat: /");
    g.key("escape");
    g.wait_for("playing");

    // A key held down is let go even if a screen opened in between, and does not stick.
    g.call("key", json!({"name": "w", "hold": true}));
    g.key("t");
    g.wait_for("chat: ");
    g.call("key", json!({"name": "w", "release": true}));
    g.key("escape");
    g.wait_for("playing");
    // Friction brings the player to rest; a key that stuck would keep them going for good.
    let speed = |g: &mut Game| {
        let v = g.call("state", json!({}))["velocity"].clone();
        (v[0].as_f64().unwrap_or(9.0).powi(2) + v[2].as_f64().unwrap_or(9.0).powi(2)).sqrt()
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while speed(&mut g) > 1e-4 {
        assert!(Instant::now() < deadline, "still walking with no key held");
        std::thread::sleep(Duration::from_millis(50));
    }
    let x = |g: &mut Game| g.call("state", json!({}))["position"][2].as_f64().unwrap_or(f64::NAN);
    let at = x(&mut g);
    std::thread::sleep(Duration::from_millis(500));
    assert!((x(&mut g) - at).abs() < 1e-3, "moved again with no key held");

    // Leaving saves; coming back finds it all.
    g.key("escape");
    g.wait_for("pause");
    g.click(SAVE_AND_LEAVE);
    g.wait_for("title");
    g.click(PLAY);
    g.wait_for("title/worlds");
    g.click(FIRST_WORLD);
    g.wait_for("playing");
    g.call("wait_ready", json!({"seconds": 45}));
    g.wait_for_count("oak_planks", "inventory", 4);
    assert_eq!(g.call("state", json!({}))["seed"], json!(2026));

    // Dying, and coming back. The death screen must go when the server says so.
    g.chat("/gamemode survival");
    g.chat("/kill");
    g.wait_for("dead");
    g.click(COME_BACK);
    g.wait_for("playing");
    assert_eq!(g.call("state", json!({}))["health"], json!(20));

    // And out, cleanly, from the very top.
    g.key("escape");
    g.wait_for("pause");
    g.click(SAVE_AND_LEAVE);
    g.wait_for("title");
    g.call("quit", json!({}));
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = g.child.try_wait().expect("a process to ask about") {
            break status;
        }
        assert!(Instant::now() < deadline, "the game did not exit");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "the game exited with {status}");
    drop(g);
    let _ = std::fs::remove_dir_all(&home);
}
