//! The game's folder, the official launcher's list of installations, and the installations this tool
//! makes: each one a folder of its own, so its mods never mix with anything else's.
use crate::loaders::Loader;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const PROFILES: &str = "launcher_profiles.json";
const MANIFEST: &str = "modkit.json";

/// Where the official launcher keeps the game.
pub fn default_folder() -> PathBuf {
    if cfg!(windows)
        && let Some(app_data) = std::env::var_os("APPDATA")
    {
        return PathBuf::from(app_data).join(".minecraft");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        return home.join("Library").join("Application Support").join("minecraft");
    }
    home.join(".minecraft")
}

/// A mod in an installation, and where it came from, so that it can be updated or removed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledMod {
    /// `modrinth` or `curseforge`.
    pub source: String,
    pub project: String,
    pub slug: String,
    pub title: String,
    pub version: String,
    pub version_name: String,
    pub file: String,
    /// The projects it needs, by id.
    #[serde(default)]
    pub needs: Vec<String>,
    /// Added because something else needed it, not because it was asked for.
    #[serde(default)]
    pub dependency: bool,
}

/// An installation this tool made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub name: String,
    pub game: String,
    pub loader: Loader,
    pub loader_version: String,
    /// The version the launcher runs, such as `fabric-loader-0.16.14-1.21.4`.
    pub version_id: String,
    #[serde(default)]
    pub mods: Vec<InstalledMod>,
}

impl Instance {
    pub fn folder(&self, minecraft: &Path) -> PathBuf {
        instance_folder(minecraft, &self.name)
    }

    pub fn mods_folder(&self, minecraft: &Path) -> PathBuf {
        self.folder(minecraft).join("mods")
    }

    pub fn summary(&self) -> String {
        format!("{} ({} {}, {} mods)", self.name, self.loader.name(), self.game, self.mods.len())
    }
}

pub fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.');
    if ok { Ok(()) } else { Err(format!("{name:?}: a name is letters, digits, -, _ and ., up to 40 of them")) }
}

fn instance_folder(minecraft: &Path, name: &str) -> PathBuf {
    minecraft.join("modkit").join(name)
}

pub fn load(minecraft: &Path, name: &str) -> Result<Instance, String> {
    check_name(name)?;
    let path = instance_folder(minecraft, name).join(MANIFEST);
    let bytes = std::fs::read(&path)
        .map_err(|_| format!("there is no installation called {name}: mc-modkit list shows them"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn save(minecraft: &Path, instance: &Instance) -> Result<(), String> {
    let folder = instance.folder(minecraft);
    std::fs::create_dir_all(folder.join("mods")).map_err(|e| format!("{}: {e}", folder.display()))?;
    let text = serde_json::to_vec_pretty(instance).map_err(|e| e.to_string())?;
    write_atomically(&folder.join(MANIFEST), &text)
}

pub fn list(minecraft: &Path) -> Vec<Instance> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(minecraft.join("modkit")) {
        for entry in entries.flatten() {
            if let Ok(bytes) = std::fs::read(entry.path().join(MANIFEST))
                && let Ok(instance) = serde_json::from_slice::<Instance>(&bytes)
            {
                out.push(instance);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn profile_key(name: &str) -> String {
    format!("modkit-{name}")
}

/// Puts an installation in the launcher's list, with its own game folder. Everything else in the
/// launcher's file stays as it was, and the first time this tool touches it a copy is kept.
pub fn add_profile(minecraft: &Path, instance: &Instance, now: &str) -> Result<(), String> {
    let path = minecraft.join(PROFILES);
    let mut all: Value = match std::fs::read(&path) {
        Ok(bytes) => {
            let backup = minecraft.join(format!("{PROFILES}.modkit-backup"));
            if !backup.exists() {
                std::fs::write(&backup, &bytes).map_err(|e| format!("{}: {e}", backup.display()))?;
            }
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?
        }
        Err(_) => json!({ "profiles": {}, "settings": {}, "version": 3 }),
    };
    if !all.is_object() {
        return Err(format!("{} is not what the launcher writes", path.display()));
    }
    if !all["profiles"].is_object() {
        all["profiles"] = json!({});
    }
    let game_dir = instance.folder(minecraft);
    all["profiles"][profile_key(&instance.name)] = json!({
        "name": format!("{} ({} {})", instance.name, instance.loader.name(), instance.game),
        "type": "custom",
        "created": now,
        "lastUsed": now,
        "icon": "Furnace",
        "lastVersionId": instance.version_id,
        "gameDir": game_dir.to_string_lossy(),
    });
    let text = serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(minecraft).map_err(|e| e.to_string())?;
    write_atomically(&path, &text)
}

pub fn remove_profile(minecraft: &Path, name: &str) -> Result<(), String> {
    let path = minecraft.join(PROFILES);
    let Ok(bytes) = std::fs::read(&path) else { return Ok(()) };
    let mut all: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(profiles) = all["profiles"].as_object_mut() {
        profiles.remove(&profile_key(name));
    }
    let text = serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?;
    write_atomically(&path, &text)
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("json.modkit-new");
    std::fs::write(&temporary, bytes).map_err(|e| format!("{}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("modkit-mc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("folder");
        dir
    }

    fn instance() -> Instance {
        Instance {
            name: "speedy".into(),
            game: "1.21.4".into(),
            loader: Loader::Fabric,
            loader_version: "0.16.14".into(),
            version_id: "fabric-loader-0.16.14-1.21.4".into(),
            mods: Vec::new(),
        }
    }

    #[test]
    fn names_are_safe_folder_names() {
        assert!(check_name("speedy-1.21_test").is_ok());
        for bad in ["", "../up", "a/b", "a b", ".hidden", &"x".repeat(41)] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_profile_joins_the_launcher_list_and_leaves_the_rest_alone() {
        let mc = temp("profiles");
        let original = json!({
            "profiles": { "abc": { "name": "Mine", "type": "custom", "lastVersionId": "1.21.4" } },
            "settings": { "keepLauncherOpen": true },
            "version": 3,
            "somethingNew": [1, 2, 3],
        });
        std::fs::write(mc.join(PROFILES), original.to_string()).expect("profiles");
        let i = instance();
        add_profile(&mc, &i, "2026-11-03T00:00:00.000Z").expect("added");
        let after: Value = serde_json::from_slice(&std::fs::read(mc.join(PROFILES)).expect("read")).expect("json");
        assert_eq!(after["profiles"]["abc"], original["profiles"]["abc"], "someone else's profile is untouched");
        assert_eq!(after["settings"], original["settings"]);
        assert_eq!(after["somethingNew"], original["somethingNew"], "fields this tool does not know survive");
        let mine = &after["profiles"]["modkit-speedy"];
        assert_eq!(mine["lastVersionId"], "fabric-loader-0.16.14-1.21.4");
        assert!(mine["gameDir"].as_str().expect("a folder").ends_with("speedy"));
        let backup: Value =
            serde_json::from_slice(&std::fs::read(mc.join("launcher_profiles.json.modkit-backup")).expect("backup"))
                .expect("json");
        assert_eq!(backup, original, "the first copy is kept");

        remove_profile(&mc, "speedy").expect("removed");
        let after: Value = serde_json::from_slice(&std::fs::read(mc.join(PROFILES)).expect("read")).expect("json");
        assert!(after["profiles"].get("modkit-speedy").is_none());
        assert_eq!(after["profiles"]["abc"], original["profiles"]["abc"]);
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[test]
    fn with_no_launcher_file_one_is_made() {
        let mc = temp("fresh");
        add_profile(&mc, &instance(), "now").expect("added");
        let after: Value = serde_json::from_slice(&std::fs::read(mc.join(PROFILES)).expect("read")).expect("json");
        assert!(after["profiles"]["modkit-speedy"].is_object());
        assert!(!mc.join("launcher_profiles.json.modkit-backup").exists(), "nothing to keep a copy of");
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[test]
    fn installations_are_saved_listed_and_loaded() {
        let mc = temp("instances");
        let i = instance();
        save(&mc, &i).expect("saved");
        assert!(i.mods_folder(&mc).is_dir());
        assert_eq!(list(&mc), vec![i.clone()]);
        assert_eq!(load(&mc, "speedy").expect("loaded"), i);
        assert!(load(&mc, "nothing").is_err());
        let _ = std::fs::remove_dir_all(&mc);
    }
}
