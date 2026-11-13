//! The mods installed on this computer: one folder each, and a file saying which are turned off and
//! where to look for more.
use crate::catalogue::{CatalogueEntry, Listing, MAX_PACK, Source, valid_id};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zenith_content::{Pack, Registry};
use zenith_fetch::Http;

const SETTINGS_FILE: &str = "library.json";
const PACK_FILE: &str = "pack.json";
const MANIFEST_FILE: &str = "mod.json";

/// Where Zenith keeps saves, settings and mods: `ZENITH_HOME`, or the user's data folder.
pub fn default_home() -> PathBuf {
    if let Some(h) = std::env::var_os("ZENITH_HOME") {
        return PathBuf::from(h);
    }
    if let Some(a) = std::env::var_os("APPDATA") {
        return PathBuf::from(a).join("Zenith");
    }
    if let Some(h) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(h).join("zenith");
    }
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h).join(".local/share/zenith");
    }
    PathBuf::from(".zenith")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    sources: Vec<Source>,
    /// Installed mods that are turned off. Everything else installed is on.
    off: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sources: vec![Source::Examples], off: Vec::new() }
    }
}

/// What the library knows about an installed mod.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// Where it came from; `None` for a pack someone put in the folder by hand.
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub manifest: Manifest,
    pub enabled: bool,
}

/// How an entry in a catalogue stands against what is installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryState {
    NotInstalled,
    Installed,
    /// Installed, at another version: the one given.
    Update(String),
}

pub struct Library {
    dir: PathBuf,
    settings: Settings,
}

impl Library {
    /// The library in a Zenith home folder, made if it is not there yet. Packs put straight into
    /// the folder by hand become installed mods.
    pub fn open(home: &Path) -> Result<Self, String> {
        let dir = home.join("mods");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let settings = match std::fs::read(dir.join(SETTINGS_FILE)) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", dir.join(SETTINGS_FILE).display()))?
            }
            Err(_) => Settings::default(),
        };
        let library = Self { dir, settings };
        library.adopt_loose_packs();
        Ok(library)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where Git sources are cloned.
    pub fn cache(&self) -> PathBuf {
        self.dir.join(".sources")
    }

    pub fn sources(&self) -> &[Source] {
        &self.settings.sources
    }

    pub fn add_source(&mut self, source: Source) -> Result<(), String> {
        if !self.settings.sources.contains(&source) {
            self.settings.sources.push(source);
            self.save()?;
        }
        Ok(())
    }

    pub fn remove_source(&mut self, index: usize) -> Result<(), String> {
        if index >= self.settings.sources.len() {
            return Err(format!("there is no source {}", index + 1));
        }
        self.settings.sources.remove(index);
        self.save()
    }

    /// Every installed mod, by name.
    pub fn installed(&self) -> Vec<Installed> {
        let mut list = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return list };
        for entry in entries.flatten() {
            let Ok(bytes) = std::fs::read(entry.path().join(MANIFEST_FILE)) else { continue };
            let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else { continue };
            let enabled = !self.settings.off.contains(&manifest.id);
            list.push(Installed { manifest, enabled });
        }
        list.sort_by_cached_key(|m| m.manifest.name.to_lowercase());
        list
    }

    pub fn state_of(&self, entry: &CatalogueEntry) -> EntryState {
        match self.manifest(&entry.id) {
            None => EntryState::NotInstalled,
            Some(m) if m.version == entry.version => EntryState::Installed,
            Some(m) => EntryState::Update(m.version),
        }
    }

    fn manifest(&self, id: &str) -> Option<Manifest> {
        if !valid_id(id) {
            return None;
        }
        let bytes = std::fs::read(self.dir.join(id).join(MANIFEST_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn set_enabled(&mut self, id: &str, on: bool) -> Result<(), String> {
        if self.manifest(id).is_none() {
            return Err(format!("no mod called {id} is installed"));
        }
        self.settings.off.retain(|o| o != id);
        if !on {
            self.settings.off.push(id.to_string());
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        if self.manifest(id).is_none() {
            return Err(format!("no mod called {id} is installed"));
        }
        std::fs::remove_dir_all(self.dir.join(id)).map_err(|e| e.to_string())?;
        self.settings.off.retain(|o| o != id);
        self.save()
    }

    /// Installs, or updates, one mod from a catalogue. It is checked before anything is written: its
    /// hash, that it parses, and that it would build into a registry on its own.
    pub fn install(&self, listing: &Listing, entry: &CatalogueEntry, http: &dyn Http) -> Result<Manifest, String> {
        install_into(&self.dir, listing, entry, http)
    }

    /// The packs of every mod that is turned on, in name order.
    pub fn enabled_packs(&self) -> Result<Vec<Pack>, String> {
        let mut packs = Vec::new();
        for m in self.installed().into_iter().filter(|m| m.enabled) {
            let path = self.dir.join(&m.manifest.id).join(PACK_FILE);
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", m.manifest.name))?;
            packs.push(Registry::parse_pack(&text).map_err(|e| format!("{}: {e}", m.manifest.name))?);
        }
        Ok(packs)
    }

    fn save(&self) -> Result<(), String> {
        let text = serde_json::to_vec_pretty(&self.settings).map_err(|e| e.to_string())?;
        write_atomically(&self.dir.join(SETTINGS_FILE), &text)
    }

    /// A pack someone dropped into the folder becomes a mod of its own, named after the pack.
    fn adopt_loose_packs(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "json") || path.file_name().is_some_and(|n| n == SETTINGS_FILE) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(pack) = Registry::parse_pack(&text) else { continue };
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let id: String = stem
                .to_lowercase()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
                .take(64)
                .collect();
            if !valid_id(&id) || self.dir.join(&id).exists() {
                continue;
            }
            let manifest = Manifest {
                id: id.clone(),
                name: if pack.name.is_empty() { stem.clone() } else { pack.name.clone() },
                version: "local".into(),
                description: "Added to the mods folder by hand.".into(),
                author: String::new(),
                source: None,
                sha256: zenith_fetch::sha256_hex(text.as_bytes()),
            };
            if write_mod(&self.dir, &manifest, text.as_bytes()).is_ok() {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// What `Library::install` does, needing only the folder, so that it can run on another thread.
pub fn install_into(
    dir: &Path,
    listing: &Listing,
    entry: &CatalogueEntry,
    http: &dyn Http,
) -> Result<Manifest, String> {
    if !valid_id(&entry.id) {
        return Err(format!("{:?} is not a mod id", entry.id));
    }
    let bytes = listing.pack_bytes(entry, http)?;
    if bytes.len() as u64 > MAX_PACK {
        return Err(format!("{} is larger than a pack may be", entry.name));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("{} is not text", entry.name))?;
    let pack = Registry::parse_pack(&text).map_err(|e| format!("{}: {e}", entry.name))?;
    Registry::build(&[pack], &Default::default()).map_err(|e| format!("{} would not load: {e}", entry.name))?;
    let manifest = Manifest {
        id: entry.id.clone(),
        name: entry.name.clone(),
        version: entry.version.clone(),
        description: entry.description.clone(),
        author: entry.author.clone(),
        source: Some(listing.source.clone()),
        sha256: zenith_fetch::sha256_hex(text.as_bytes()),
    };
    write_mod(dir, &manifest, text.as_bytes())?;
    Ok(manifest)
}

/// Writes a mod's folder whole: into a fresh folder first, which then takes the old one's place.
fn write_mod(dir: &Path, manifest: &Manifest, pack: &[u8]) -> Result<(), String> {
    let fresh = dir.join(format!(".{}.new", manifest.id));
    let _ = std::fs::remove_dir_all(&fresh);
    std::fs::create_dir_all(&fresh).map_err(|e| e.to_string())?;
    std::fs::write(fresh.join(PACK_FILE), pack).map_err(|e| e.to_string())?;
    let text = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
    std::fs::write(fresh.join(MANIFEST_FILE), text).map_err(|e| e.to_string())?;
    let target = dir.join(&manifest.id);
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&fresh, &target).map_err(|e| format!("{}: {e}", target.display()))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("json.new");
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_fetch::Canned;

    struct Home(PathBuf);
    impl Home {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("zenith-library-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn examples() -> Listing {
        Listing::fetch(&Source::Examples, Path::new("unused"), &Canned::new()).expect("the examples")
    }

    #[test]
    fn a_new_library_looks_in_the_examples_and_has_nothing_installed() {
        let home = Home::new("new");
        let library = Library::open(&home.0).expect("opens");
        assert_eq!(library.sources(), [Source::Examples]);
        assert!(library.installed().is_empty());
        assert!(library.enabled_packs().expect("packs").is_empty());
    }

    #[test]
    fn installing_turning_off_and_removing_a_mod() {
        let home = Home::new("cycle");
        let mut library = Library::open(&home.0).expect("opens");
        let listing = examples();
        let neon = listing.catalogue.mods.iter().find(|m| m.id == "neon").expect("neon").clone();
        assert_eq!(library.state_of(&neon), EntryState::NotInstalled);
        library.install(&listing, &neon, &Canned::new()).expect("installs");
        assert_eq!(library.state_of(&neon), EntryState::Installed);
        assert_eq!(library.installed().len(), 1);
        assert!(library.installed()[0].enabled, "a new mod is on");
        let packs = library.enabled_packs().expect("packs");
        assert_eq!(packs.len(), 1);
        let registry = Registry::build(&packs, &Default::default()).expect("builds");
        assert!(registry.block_by_name("neon_pink").is_some());

        library.set_enabled("neon", false).expect("turns off");
        assert!(library.enabled_packs().expect("packs").is_empty());
        // It stays off when the library is opened again.
        let mut library = Library::open(&home.0).expect("reopens");
        assert!(!library.installed()[0].enabled);
        library.set_enabled("neon", true).expect("turns on");
        assert_eq!(library.enabled_packs().expect("packs").len(), 1);

        library.remove("neon").expect("removes");
        assert!(library.installed().is_empty());
        assert!(library.remove("neon").is_err());
        assert!(library.set_enabled("nothing", true).is_err());
    }

    #[test]
    fn a_newer_version_in_a_catalogue_is_an_update_that_replaces_the_old() {
        let home = Home::new("update");
        let library = Library::open(&home.0).expect("opens");
        let listing = examples();
        let mut masonry = listing.catalogue.mods.iter().find(|m| m.id == "masonry").expect("masonry").clone();
        library.install(&listing, &masonry, &Canned::new()).expect("installs");
        masonry.version = "9.9.9".into();
        assert_eq!(library.state_of(&masonry), EntryState::Update("1.2.0".into()));
        library.install(&listing, &masonry, &Canned::new()).expect("updates");
        assert_eq!(library.state_of(&masonry), EntryState::Installed);
        assert_eq!(library.installed().len(), 1);
    }

    #[test]
    fn a_pack_dropped_into_the_folder_becomes_a_mod() {
        let home = Home::new("loose");
        std::fs::create_dir_all(home.0.join("mods")).expect("folder");
        std::fs::write(home.0.join("mods").join("My Lights.json"), include_str!("../builtin/neon.json"))
            .expect("a pack");
        std::fs::write(home.0.join("mods").join("broken.json"), "{ not json").expect("junk");
        let library = Library::open(&home.0).expect("opens");
        let installed = library.installed();
        assert_eq!(installed.len(), 1, "only the pack that parses");
        assert_eq!(installed[0].manifest.id, "my_lights");
        assert_eq!(installed[0].manifest.source, None);
        assert!(!home.0.join("mods").join("My Lights.json").exists(), "moved into its own folder");
        assert!(home.0.join("mods").join("broken.json").exists(), "junk is left where it was");
    }

    #[test]
    fn a_pack_that_would_not_load_is_never_installed() {
        let home = Home::new("refuse");
        let library = Library::open(&home.0).expect("opens");
        let http = Canned::new();
        let bad = r#"{"name": "bad", "blocks": [{"name": "stone", "display": "Not stone"}]}"#;
        let catalogue = serde_json::json!({"zenith_catalogue": 1, "name": "T", "mods": [
            {"id": "bad", "name": "Bad", "version": "1", "file": "bad.json", "sha256": zenith_fetch::sha256_hex(bad.as_bytes())}
        ]});
        http.answer("https://t.example.com/zenith-catalogue.json", catalogue.to_string());
        http.answer("https://t.example.com/bad.json", bad);
        let listing = Listing::fetch(
            &Source::Web { url: "https://t.example.com/zenith-catalogue.json".into() },
            Path::new("unused"),
            &http,
        )
        .expect("listed");
        let e = library.install(&listing, &listing.catalogue.mods[0], &http).expect_err("refused");
        assert!(e.contains("already exists"), "{e}");
        assert!(library.installed().is_empty());
    }

    #[test]
    fn sources_can_be_added_once_and_removed() {
        let home = Home::new("sources");
        let mut library = Library::open(&home.0).expect("opens");
        let web = Source::Web { url: "https://a.example.com/zenith-catalogue.json".into() };
        library.add_source(web.clone()).expect("added");
        library.add_source(web.clone()).expect("a second time changes nothing");
        assert_eq!(library.sources(), [Source::Examples, web.clone()]);
        let mut library = Library::open(&home.0).expect("reopens");
        assert_eq!(library.sources().len(), 2);
        library.remove_source(0).expect("removed");
        assert_eq!(library.sources(), [web]);
        assert!(library.remove_source(5).is_err());
    }

    #[test]
    fn a_git_repository_is_a_source() {
        if std::process::Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipped: no git here");
            return;
        }
        let home = Home::new("git");
        let repo = home.0.join("repo");
        std::fs::create_dir_all(repo.join("packs")).expect("folders");
        let pack = include_str!("../builtin/garden.json");
        std::fs::write(repo.join("packs/garden.json"), pack).expect("pack");
        let catalogue = serde_json::json!({"zenith_catalogue": 1, "name": "Repo", "mods": [
            {"id": "garden", "name": "Garden", "version": "3", "file": "packs/garden.json", "sha256": zenith_fetch::sha256_hex(pack.as_bytes())}
        ]});
        std::fs::write(repo.join(crate::CATALOGUE_FILE), catalogue.to_string()).expect("catalogue");
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=test", "-c", "user.email=test@example.com", "-C", &repo.to_string_lossy()])
                .args(args)
                .output()
                .expect("git runs")
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "--quiet"]);
        run(&["add", "."]);
        run(&["commit", "--quiet", "-m", "packs"]);

        let library = Library::open(&home.0).expect("opens");
        let source = Source::from_text(&repo.to_string_lossy()).expect("a folder is a repository source");
        let listing = Listing::fetch(&source, &library.cache(), &zenith_fetch::Curl).expect("cloned");
        assert_eq!(listing.catalogue.name, "Repo");
        library.install(&listing, &listing.catalogue.mods[0], &zenith_fetch::Curl).expect("installed");
        assert_eq!(library.installed()[0].manifest.version, "3");
        // Fetching again updates the clone rather than failing on it.
        Listing::fetch(&source, &library.cache(), &zenith_fetch::Curl).expect("updated");
    }
}
