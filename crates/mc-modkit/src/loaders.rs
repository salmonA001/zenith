//! Mod loaders, and putting one into the game's folder the way its own installer would.
//!
//! Fabric and Quilt publish a launcher profile for every pair of game and loader versions, so
//! installing one is writing that profile where the launcher looks. Forge and NeoForge have to patch
//! the game, which only their own installers do: those are fetched from the projects' own Maven
//! repositories and run with Java.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::path::Path;
use zenith_fetch::Http;

const MAX_META: u64 = 16 << 20;
const MAX_INSTALLER: u64 = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Loader {
    Fabric,
    Quilt,
    Forge,
    NeoForge,
}

impl Loader {
    pub const ALL: [Loader; 4] = [Loader::Fabric, Loader::Quilt, Loader::Forge, Loader::NeoForge];

    pub fn parse(text: &str) -> Result<Loader, String> {
        match text.to_ascii_lowercase().as_str() {
            "fabric" => Ok(Loader::Fabric),
            "quilt" => Ok(Loader::Quilt),
            "forge" => Ok(Loader::Forge),
            "neoforge" | "neo" => Ok(Loader::NeoForge),
            other => Err(format!("{other} is not a loader: fabric, quilt, forge or neoforge")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Loader::Fabric => "Fabric",
            Loader::Quilt => "Quilt",
            Loader::Forge => "Forge",
            Loader::NeoForge => "NeoForge",
        }
    }

    /// How Modrinth names it.
    pub fn modrinth(self) -> &'static str {
        match self {
            Loader::Fabric => "fabric",
            Loader::Quilt => "quilt",
            Loader::Forge => "forge",
            Loader::NeoForge => "neoforge",
        }
    }

    /// CurseForge's number for it.
    pub fn curseforge(self) -> u32 {
        match self {
            Loader::Forge => 1,
            Loader::Fabric => 4,
            Loader::Quilt => 5,
            Loader::NeoForge => 6,
        }
    }
}

/// Orders version names as people mean them: 1.21.10 after 1.21.9, and a release after its betas.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    fn parts(s: &str) -> Vec<Result<u64, String>> {
        let mut out = Vec::new();
        let mut current = String::new();
        let mut digits = false;
        let mut push = |token: &str, digits: bool| {
            // Dots only separate: what follows them is what counts.
            if token.chars().all(|c| c == '.') {
                return;
            }
            out.push(if digits { token.parse().map_err(|_| token.to_string()) } else { Err(token.to_string()) });
        };
        for c in s.chars() {
            let is_digit = c.is_ascii_digit();
            if !current.is_empty() && is_digit != digits {
                push(&current, digits);
                current.clear();
            }
            digits = is_digit;
            current.push(c);
        }
        if !current.is_empty() {
            push(&current, digits);
        }
        out
    }
    let (pa, pb) = (parts(a), parts(b));
    for (x, y) in pa.iter().zip(&pb) {
        let order = match (x, y) {
            (Ok(x), Ok(y)) => x.cmp(y),
            (Err(x), Err(y)) => x.cmp(y),
            // Where one goes on with a number and the other with a word, the word is a pre-release.
            (Ok(_), Err(_)) => Ordering::Greater,
            (Err(_), Ok(_)) => Ordering::Less,
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    // "1.0" is newer than "1.0-beta", but "1.0.1" is newer than "1.0".
    match pa.len().cmp(&pb.len()) {
        Ordering::Equal => Ordering::Equal,
        Ordering::Less => match pb.get(pa.len()) {
            Some(Ok(_)) => Ordering::Less,
            _ => Ordering::Greater,
        },
        Ordering::Greater => match pa.get(pb.len()) {
            Some(Ok(_)) => Ordering::Greater,
            _ => Ordering::Less,
        },
    }
}

fn get_json(http: &dyn Http, url: &str) -> Result<Value, String> {
    let bytes = http.get(url, &[], MAX_META)?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{url}: unexpected reply: {e}"))
}

fn get_text(http: &dyn Http, url: &str) -> Result<String, String> {
    String::from_utf8(http.get(url, &[], MAX_META)?).map_err(|_| format!("{url}: not text"))
}

/// Every `<version>` in a Maven repository's listing.
fn maven_versions(xml: &str) -> Vec<String> {
    xml.split("<version>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</version>").map(|(v, _)| v.trim().to_string()))
        .collect()
}

/// The game version a NeoForge version is for. Before the game's year-based versions, 21.1.x was
/// for 1.21.1, 20.4.x for 1.20.4 and 21.0.x for 1.21. Since then the whole game version comes first:
/// 26.1.2.123 is for 26.1.2, and 26.3.0.91 for 26.3. (Checked against NeoForge's own listing.)
pub fn neoforge_game_version(neoforge: &str) -> Option<String> {
    let numbers: Vec<u32> = neoforge.split(['.', '-']).map_while(|p| p.parse().ok()).collect();
    let (&major, &minor) = (numbers.first()?, numbers.get(1)?);
    if major < 25 {
        return Some(if minor == 0 { format!("1.{major}") } else { format!("1.{major}.{minor}") });
    }
    match numbers.get(2) {
        Some(&patch) if patch > 0 => Some(format!("{major}.{minor}.{patch}")),
        _ => Some(format!("{major}.{minor}")),
    }
}

const FORGE_PROMOTIONS: &str = "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/net/neoforged/neoforge";

/// The game versions a loader supports, newest first: releases only, unless `everything`.
pub fn game_versions(http: &dyn Http, loader: Loader, everything: bool) -> Result<Vec<String>, String> {
    let mut versions: Vec<String> = match loader {
        Loader::Fabric | Loader::Quilt => {
            let url = if loader == Loader::Fabric {
                "https://meta.fabricmc.net/v2/versions/game"
            } else {
                "https://meta.quiltmc.org/v3/versions/game"
            };
            let list = get_json(http, url)?;
            list.as_array()
                .ok_or("an unexpected list of versions")?
                .iter()
                .filter(|v| everything || v["stable"].as_bool().unwrap_or(false))
                .filter_map(|v| v["version"].as_str().map(str::to_string))
                .collect()
        }
        Loader::Forge => {
            let promos = get_json(http, FORGE_PROMOTIONS)?;
            let mut list: Vec<String> = promos["promos"]
                .as_object()
                .ok_or("an unexpected list of Forge versions")?
                .keys()
                .filter_map(|k| k.rsplit_once('-').map(|(game, _)| game.to_string()))
                .filter(|g| everything || !g.contains(['_', '-']))
                .collect();
            list.sort_by(|a, b| compare_versions(b, a));
            list.dedup();
            return Ok(list);
        }
        Loader::NeoForge => {
            let xml = get_text(http, &format!("{NEOFORGE_MAVEN}/maven-metadata.xml"))?;
            let mut list: Vec<String> = maven_versions(&xml)
                .iter()
                .filter(|v| everything || !v.contains("beta"))
                .filter_map(|v| neoforge_game_version(v))
                .collect();
            list.sort_by(|a, b| compare_versions(b, a));
            list.dedup();
            return Ok(list);
        }
    };
    versions.dedup();
    Ok(versions)
}

/// The newest loader for a game version: its stable builds before the rest.
pub fn latest_loader(http: &dyn Http, loader: Loader, game: &str) -> Result<String, String> {
    let none = || format!("{} has nothing for Minecraft {game}", loader.name());
    match loader {
        Loader::Fabric | Loader::Quilt => {
            let url = match loader {
                Loader::Fabric => {
                    format!("https://meta.fabricmc.net/v2/versions/loader/{}", zenith_fetch::escape(game))
                }
                _ => format!("https://meta.quiltmc.org/v3/versions/loader/{}", zenith_fetch::escape(game)),
            };
            let list = get_json(http, &url)?;
            let mut list: Vec<&Value> = list.as_array().ok_or_else(none)?.iter().collect();
            // Quilt's list is in no particular order, so put the newest first before choosing.
            let version = |v: &Value| v["loader"]["version"].as_str().unwrap_or("").to_string();
            list.sort_by(|a, b| compare_versions(&version(b), &version(a)));
            let stable =
                |v: &&&Value| v["loader"]["stable"].as_bool().unwrap_or_else(|| !version(v).contains(['-', '+']));
            list.iter().find(stable).or(list.first()).map(|v| version(v)).filter(|v| !v.is_empty()).ok_or_else(none)
        }
        Loader::Forge => {
            let promos = get_json(http, FORGE_PROMOTIONS)?;
            ["recommended", "latest"]
                .iter()
                .find_map(|kind| promos["promos"][format!("{game}-{kind}")].as_str())
                .map(str::to_string)
                .ok_or_else(none)
        }
        Loader::NeoForge => {
            let xml = get_text(http, &format!("{NEOFORGE_MAVEN}/maven-metadata.xml"))?;
            let mut matching: Vec<String> = maven_versions(&xml)
                .into_iter()
                .filter(|v| neoforge_game_version(v).as_deref() == Some(game))
                .collect();
            matching.sort_by(|a, b| compare_versions(b, a));
            matching.iter().find(|v| !v.contains("beta")).or(matching.first()).cloned().ok_or_else(none)
        }
    }
}

/// Puts a loader into the game's folder, ready for the launcher. Returns the version id the
/// launcher will run.
pub fn install(http: &dyn Http, loader: Loader, game: &str, version: &str, minecraft: &Path) -> Result<String, String> {
    match loader {
        Loader::Fabric | Loader::Quilt => {
            let url = match loader {
                Loader::Fabric => format!(
                    "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
                    zenith_fetch::escape(game),
                    zenith_fetch::escape(version)
                ),
                _ => format!(
                    "https://meta.quiltmc.org/v3/versions/loader/{}/{}/profile/json",
                    zenith_fetch::escape(game),
                    zenith_fetch::escape(version)
                ),
            };
            let profile = get_json(http, &url)?;
            let id = profile["id"].as_str().ok_or("the loader's profile has no id")?.to_string();
            if id.is_empty() || id.contains(['/', '\\', ':']) || id.contains("..") {
                return Err(format!("the loader's profile has a strange id: {id}"));
            }
            let folder = minecraft.join("versions").join(&id);
            std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
            let text = serde_json::to_vec_pretty(&profile).map_err(|e| e.to_string())?;
            std::fs::write(folder.join(format!("{id}.json")), text).map_err(|e| e.to_string())?;
            // The launcher wants a jar beside every version, even one that borrows its parent's.
            // Fabric's own installer leaves an empty one too.
            std::fs::write(folder.join(format!("{id}.jar")), b"").map_err(|e| e.to_string())?;
            Ok(id)
        }
        Loader::Forge | Loader::NeoForge => {
            let (url, id, flag) = match loader {
                Loader::Forge => (
                    format!(
                        "https://maven.minecraftforge.net/net/minecraftforge/forge/{game}-{version}/forge-{game}-{version}-installer.jar"
                    ),
                    format!("{game}-forge-{version}"),
                    "--installClient",
                ),
                _ => (
                    format!("{NEOFORGE_MAVEN}/{version}/neoforge-{version}-installer.jar"),
                    format!("neoforge-{version}"),
                    "--install-client",
                ),
            };
            let jar = http.get(&url, &[], MAX_INSTALLER)?;
            let folder = minecraft.join("modkit").join(".installers");
            std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
            let path = folder.join(url.rsplit('/').next().unwrap_or("installer.jar"));
            std::fs::write(&path, jar).map_err(|e| e.to_string())?;
            let java = which_java().ok_or_else(|| {
                format!(
                    "{} has to be installed by its own installer, which needs Java. Install Java, or run {} yourself.",
                    loader.name(),
                    path.display()
                )
            })?;
            let output = std::process::Command::new(java)
                .arg("-jar")
                .arg(&path)
                .arg(flag)
                .arg(minecraft)
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| format!("could not run Java: {e}"))?;
            if !output.status.success() {
                let tail: String = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(format!("{}'s installer failed:\n{tail}", loader.name()));
            }
            if !minecraft.join("versions").join(&id).is_dir() {
                return Err(format!("{}'s installer finished, but {id} is not in the versions folder", loader.name()));
            }
            Ok(id)
        }
    }
}

/// Java, from JAVA_HOME or the PATH.
fn which_java() -> Option<std::path::PathBuf> {
    let exe = if cfg!(windows) { "java.exe" } else { "java" };
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let candidate = Path::new(&home).join("bin").join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    std::process::Command::new(exe).arg("-version").output().ok().map(|_| exe.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_fetch::Canned;

    #[test]
    fn versions_order_as_people_mean_them() {
        assert_eq!(compare_versions("1.21.10", "1.21.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.21", "1.21.1"), Ordering::Less);
        assert_eq!(compare_versions("1.0", "1.0-beta"), Ordering::Greater);
        assert_eq!(compare_versions("21.1.77", "21.1.77"), Ordering::Equal);
        assert_eq!(compare_versions("0.16.5", "0.16.14"), Ordering::Less);
    }

    #[test]
    fn neoforge_versions_name_their_game_version() {
        assert_eq!(neoforge_game_version("21.1.77").as_deref(), Some("1.21.1"));
        assert_eq!(neoforge_game_version("21.0.167").as_deref(), Some("1.21"));
        assert_eq!(neoforge_game_version("20.4.237").as_deref(), Some("1.20.4"));
        assert_eq!(neoforge_game_version("26.1.0.5-beta").as_deref(), Some("26.1"));
        assert_eq!(neoforge_game_version("26.3.0.91").as_deref(), Some("26.3"));
        assert_eq!(neoforge_game_version("26.1.2.123").as_deref(), Some("26.1.2"));
        assert_eq!(neoforge_game_version("21.11.38").as_deref(), Some("1.21.11"));
        assert_eq!(neoforge_game_version("junk"), None);
    }

    #[test]
    fn fabric_game_versions_are_releases_unless_asked_for_everything() {
        let http = Canned::new();
        http.answer(
            "https://meta.fabricmc.net/v2/versions/game",
            r#"[{"version":"25w14a","stable":false},{"version":"1.21.5","stable":true},{"version":"1.21.4","stable":true}]"#,
        );
        assert_eq!(game_versions(&http, Loader::Fabric, false).expect("versions"), ["1.21.5", "1.21.4"]);
        assert_eq!(game_versions(&http, Loader::Fabric, true).expect("versions").len(), 3);
    }

    #[test]
    fn the_newest_stable_fabric_loader_is_chosen() {
        let http = Canned::new();
        http.answer(
            "https://meta.fabricmc.net/v2/versions/loader/1.21.4",
            r#"[{"loader":{"version":"0.17.0-beta.1","stable":false}},{"loader":{"version":"0.16.14","stable":true}},{"loader":{"version":"0.16.13","stable":true}}]"#,
        );
        assert_eq!(latest_loader(&http, Loader::Fabric, "1.21.4").as_deref(), Ok("0.16.14"));
        assert!(latest_loader(&http, Loader::Fabric, "9.9").is_err());
    }

    #[test]
    fn quilt_loaders_in_no_order_still_give_the_newest_stable() {
        let http = Canned::new();
        http.answer(
            "https://meta.quiltmc.org/v3/versions/loader/1.21.4",
            r#"[{"loader":{"version":"0.20.0-beta.9"}},{"loader":{"version":"0.24.0"}},{"loader":{"version":"0.29.1-beta.2"}},{"loader":{"version":"0.28.4"}},{"loader":{"version":"0.26.0"}}]"#,
        );
        assert_eq!(latest_loader(&http, Loader::Quilt, "1.21.4").as_deref(), Ok("0.28.4"));
    }

    #[test]
    fn forge_and_neoforge_versions_come_from_their_own_listings() {
        let http = Canned::new();
        http.answer(
            FORGE_PROMOTIONS,
            r#"{"promos":{"1.20.1-latest":"47.4.0","1.20.1-recommended":"47.3.0","1.21.4-latest":"54.1.0"}}"#,
        );
        assert_eq!(game_versions(&http, Loader::Forge, false).expect("versions"), ["1.21.4", "1.20.1"]);
        assert_eq!(latest_loader(&http, Loader::Forge, "1.20.1").as_deref(), Ok("47.3.0"), "recommended first");
        assert_eq!(latest_loader(&http, Loader::Forge, "1.21.4").as_deref(), Ok("54.1.0"));
        http.answer(
            &format!("{NEOFORGE_MAVEN}/maven-metadata.xml"),
            "<metadata><versioning><versions><version>21.1.76</version><version>21.1.77</version><version>21.2.0-beta</version></versions></versioning></metadata>",
        );
        assert_eq!(latest_loader(&http, Loader::NeoForge, "1.21.1").as_deref(), Ok("21.1.77"));
        assert_eq!(
            latest_loader(&http, Loader::NeoForge, "1.21.2").as_deref(),
            Ok("21.2.0-beta"),
            "a beta when there is nothing else"
        );
        assert_eq!(game_versions(&http, Loader::NeoForge, false).expect("versions"), ["1.21.1"]);
    }

    #[test]
    fn installing_fabric_writes_its_profile_where_the_launcher_looks() {
        let http = Canned::new();
        http.answer(
            "https://meta.fabricmc.net/v2/versions/loader/1.21.4/0.16.14/profile/json",
            r#"{"id":"fabric-loader-0.16.14-1.21.4","inheritsFrom":"1.21.4","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient","libraries":[]}"#,
        );
        let dir = std::env::temp_dir().join(format!("modkit-fabric-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let id = install(&http, Loader::Fabric, "1.21.4", "0.16.14", &dir).expect("installed");
        assert_eq!(id, "fabric-loader-0.16.14-1.21.4");
        let folder = dir.join("versions").join(&id);
        let written: Value =
            serde_json::from_slice(&std::fs::read(folder.join(format!("{id}.json"))).expect("profile")).expect("json");
        assert_eq!(written["inheritsFrom"], "1.21.4");
        assert!(folder.join(format!("{id}.jar")).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_profile_with_a_path_for_an_id_is_refused() {
        let http = Canned::new();
        http.answer(
            "https://meta.quiltmc.org/v3/versions/loader/1.21.4/0.28.0/profile/json",
            r#"{"id":"../../escape"}"#,
        );
        let dir = std::env::temp_dir().join(format!("modkit-quilt-{}", std::process::id()));
        assert!(install(&http, Loader::Quilt, "1.21.4", "0.28.0", &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
