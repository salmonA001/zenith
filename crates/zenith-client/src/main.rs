//! `zenith`: the game.
// Double-clicked, the game opens its window and no console beside it. Started from a terminal, it
// still writes to that terminal: see `console`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
use std::path::PathBuf;
use zenith_client::app::{Launch, run_offscreen, run_window};
use zenith_client::options::Look;

const USAGE: &str = "\
zenith: a voxel world, to the horizon

USAGE: zenith [OPTIONS]

  --world NAME         open or create this world (a name in the saves folder, or a path)
  --seed TEXT          seed for a new world
  --creative           a new world starts in creative mode
  --connect ADDRESS    join a server
  --name NAME          your name
  --render-distance N  chunks around the player that are real
  --look classic|modern  how the world is drawn: big pixels and flat shading, or shadows and bloom
  --ray-tracing        shadows and reflections from traced rays (modern look, on Vulkan GPUs that trace)
  --width N --height N window size
  --offscreen          no window: draw only when asked to (for agents and tests)
  --uncapped           draw as fast as the GPU can, not waiting for the display (for measuring)
  --control FILE       answer zenith-ctl, writing how to reach this game to FILE
  --help

Keys: WASD walk, Space jump, Shift sneak, Ctrl sprint, E inventory, T chat, / command, Q drop,
1-9 hotbar, F1 hide the HUD, F2 screenshot, F3 debug, F11 fullscreen, Esc menu.
Left mouse breaks and hits; right mouse places and uses. In creative, tap Space twice to fly.

Saves and settings live in ZENITH_HOME, or your user data folder.";

fn main() {
    let nowhere_to_write = console::attach();
    if nowhere_to_write {
        // A player who double-clicked has no terminal to read a crash in.
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            console::message(&format!("Zenith stopped: {info}"));
            default(info);
        }));
    }
    if let Err(e) = run() {
        eprintln!("zenith: {e}");
        if nowhere_to_write {
            console::message(&e);
        }
        std::process::exit(1);
    }
}

/// Where messages go when the game was not started from a terminal.
#[cfg(windows)]
#[allow(unsafe_code)]
mod console {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn AttachConsole(process: u32) -> i32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(owner: *mut c_void, text: *const u16, caption: *const u16, style: u32) -> i32;
    }

    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const MB_ICONERROR: u32 = 0x10;

    /// Finds somewhere for output to go. Built as a Windows application, the game starts with no
    /// console; run from a terminal, it borrows that one. Says whether there is nowhere at all, as when
    /// it is double-clicked, so that errors need a window of their own.
    pub fn attach() -> bool {
        // SAFETY: these only ask about, and attach, this process's own console. No pointers go in.
        let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        if !stderr.is_null() && stderr as isize != -1 {
            // Already a file, a pipe or a console: whoever started the game is listening.
            return false;
        }
        // SAFETY: as above.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) == 0 }
    }

    /// Shows a message in a window of its own.
    pub fn message(text: &str) {
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (text, caption) = (wide(text), wide("Zenith"));
        // SAFETY: both are null-terminated UTF-16 that outlive the call, and there is no owner window.
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_ICONERROR) };
    }
}

#[cfg(not(windows))]
mod console {
    pub fn attach() -> bool {
        false
    }

    pub fn message(_text: &str) {}
}

fn run() -> Result<(), String> {
    let mut launch = Launch { width: 1280, height: 720, ..Default::default() };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or_else(|| format!("{arg} needs {what}"));
        match arg.as_str() {
            "--world" => launch.world = Some(value("a name")?),
            "--seed" => launch.seed = Some(value("a seed")?),
            "--creative" => launch.creative = true,
            "--connect" => launch.connect = Some(value("an address")?),
            "--name" => launch.name = Some(value("a name")?),
            "--render-distance" => {
                launch.render_distance =
                    Some(value("a number")?.parse().map_err(|_| "--render-distance needs a number")?)
            }
            "--width" => launch.width = value("a number")?.parse().map_err(|_| "--width needs a number")?,
            "--height" => launch.height = value("a number")?.parse().map_err(|_| "--height needs a number")?,
            "--offscreen" => launch.offscreen = true,
            "--uncapped" => launch.uncapped = true,
            "--ray-tracing" => launch.ray_tracing = true,
            "--look" => {
                let name = value("classic or modern")?;
                launch.look = Some(Look::from_name(&name).ok_or(format!("--look is classic or modern, not {name}"))?);
            }
            "--control" => launch.control = Some(PathBuf::from(value("a file")?)),
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        }
    }
    if launch.offscreen { run_offscreen(launch) } else { run_window(launch) }
}
