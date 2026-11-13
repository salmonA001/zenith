//! Rules about the whole workspace, checked rather than hoped for.
use std::process::Command;

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn dependencies(package: &str) -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .args(["tree", "-p", package, "-e", "normal", "--prefix", "none", "--no-dedupe"])
        .current_dir(repo_root())
        .output()
        .expect("cargo tree runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The dedicated server, the headless agent and the mesher must build and run with no graphics at
/// all. In the project this one is modelled on, the server is the game binary with its window off.
#[test]
fn nothing_headless_links_a_gpu_or_a_window() {
    for package in [
        "zenith-core",
        "zenith-content",
        "zenith-world",
        "zenith-protocol",
        "zenith-server",
        "zenith-agent",
        "zenith-mesh",
        "zenith-ctl",
        "zenith-fetch",
        "zenith-mods",
    ] {
        let tree = dependencies(package);
        for forbidden in ["wgpu", "winit", "naga", "cpal"] {
            assert!(!tree.lines().any(|l| l.starts_with(&format!("{forbidden} "))), "{package} depends on {forbidden}");
        }
    }
}

/// Each layer may only use the ones below it.
#[test]
fn layers_only_point_downwards() {
    let allowed: &[(&str, &[&str])] = &[
        ("zenith-core", &[]),
        ("zenith-content", &[]),
        ("zenith-world", &["zenith-core", "zenith-content"]),
        ("zenith-protocol", &["zenith-core", "zenith-content", "zenith-world"]),
        ("zenith-server", &["zenith-core", "zenith-content", "zenith-world", "zenith-protocol", "zenith-mods"]),
        ("zenith-agent", &["zenith-core", "zenith-content", "zenith-world", "zenith-protocol"]),
        ("zenith-mesh", &["zenith-core", "zenith-content", "zenith-world"]),
        ("zenith-fetch", &[]),
        ("zenith-mods", &["zenith-content", "zenith-fetch"]),
    ];
    for (package, may_use) in allowed {
        let manifest =
            std::fs::read_to_string(repo_root().join("crates").join(package).join("Cargo.toml")).expect("manifest");
        let normal = manifest.split("[dev-dependencies]").next().unwrap_or_default();
        for line in normal.lines() {
            if let Some(name) = line.split('=').next().map(str::trim)
                && name.starts_with("zenith-")
                && !name.contains(' ')
                && !line.starts_with("name")
            {
                assert!(may_use.contains(&name), "{package} must not depend on {name}");
            }
        }
    }
}

/// The reference page is generated: if a command changes, the page has to change with it.
#[test]
fn the_command_reference_is_up_to_date() {
    let page = std::fs::read_to_string(repo_root().join("docs").join("commands.md")).expect("docs/commands.md exists");
    let fresh = zenith_agent::commands::reference_markdown();
    assert_eq!(page.replace("\r\n", "\n"), fresh, "regenerate with: zenith-ctl help --markdown > docs/commands.md");
}

/// A save is an unsafe-free zone: every crate forbids what it does not need.
#[test]
fn no_crate_writes_unsafe_code() {
    let manifest = std::fs::read_to_string(repo_root().join("Cargo.toml")).expect("manifest");
    assert!(manifest.contains("unsafe_code = \"deny\""));
    for entry in std::fs::read_dir(repo_root().join("crates")).expect("crates") {
        let path = entry.expect("entry").path();
        let own = std::fs::read_to_string(path.join("Cargo.toml")).expect("manifest");
        assert!(
            own.contains("[lints]") && own.contains("workspace = true"),
            "{} opts out of the workspace lints",
            path.display()
        );
    }
}
