//! Where mods are published: catalogues on the web, in Git repositories, and the examples that come
//! with the game.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use zenith_fetch::Http;

/// The name a catalogue has, wherever it is kept.
pub const CATALOGUE_FILE: &str = "zenith-catalogue.json";
const MAX_CATALOGUE: u64 = 1 << 20;
pub(crate) const MAX_PACK: u64 = 4 << 20;

/// A list of mods, as its publisher writes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Catalogue {
    /// The version of this format, which is 1.
    pub zenith_catalogue: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub mods: Vec<CatalogueEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogueEntry {
    /// Lower-case letters, digits, `-` and `_`. It names the mod's folder in the library.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// The pack: a URL, or a path from where the catalogue is.
    pub file: String,
    /// The pack's SHA-256, in hex. A file that no longer matches it is refused. Only the examples,
    /// which are inside the game itself, go without.
    #[serde(default)]
    pub sha256: String,
}

/// Somewhere to look for mods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// The examples that come with the game.
    Examples,
    /// A catalogue on the web.
    Web { url: String },
    /// A Git repository with a catalogue at its top. A folder on this computer works as well.
    Git { url: String },
}

impl Source {
    /// What someone typed, as a source: a repository if it looks like one, otherwise a catalogue.
    pub fn from_text(text: &str) -> Result<Source, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("type the address of a catalogue or a Git repository".into());
        }
        if text.starts_with("https://") || text.starts_with("http://") {
            let path = text.split(['?', '#']).next().unwrap_or(text);
            if path.ends_with(".json") {
                zenith_fetch::check_url(text)?;
                return Ok(Source::Web { url: text.to_string() });
            }
            check_git(text)?;
            return Ok(Source::Git { url: text.to_string() });
        }
        if Path::new(text).is_dir() {
            return Ok(Source::Git { url: text.to_string() });
        }
        Err(format!("{text}: not a web address, and not a folder on this computer"))
    }

    /// A short name for a list.
    pub fn label(&self) -> String {
        match self {
            Source::Examples => "Examples".into(),
            Source::Web { url } | Source::Git { url } => {
                let trimmed =
                    url.trim_end_matches('/').trim_end_matches(".git").trim_end_matches(&format!("/{CATALOGUE_FILE}"));
                let without_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
                let mut parts: Vec<&str> = without_scheme.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
                if parts.len() > 2 {
                    parts = parts.split_off(parts.len() - 2);
                }
                parts.join("/")
            }
        }
    }
}

fn check_git(url: &str) -> Result<(), String> {
    if url.starts_with('-') || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("{url} is not a repository address"));
    }
    if url.starts_with("https://") || Path::new(url).is_dir() {
        return Ok(());
    }
    Err(format!("{url}: repositories have to be https:// addresses or folders on this computer"))
}

/// Where a listing's packs are fetched from.
#[derive(Clone, Debug)]
enum Base {
    Examples,
    Url(String),
    Folder(PathBuf),
}

/// A catalogue as it was found, with what is needed to fetch its packs.
#[derive(Clone, Debug)]
pub struct Listing {
    pub source: Source,
    pub catalogue: Catalogue,
    base: Base,
}

const EXAMPLES: [(&str, &str); 3] = [
    ("neon.json", include_str!("../builtin/neon.json")),
    ("masonry.json", include_str!("../builtin/masonry.json")),
    ("garden.json", include_str!("../builtin/garden.json")),
];

impl Listing {
    /// Looks in a source. A Git repository is cloned into `cache`, or brought up to date there.
    pub fn fetch(source: &Source, cache: &Path, http: &dyn Http) -> Result<Listing, String> {
        let (text, base) = match source {
            Source::Examples => (include_str!("../builtin/zenith-catalogue.json").as_bytes().to_vec(), Base::Examples),
            Source::Web { url } => (http.get(url, &[], MAX_CATALOGUE)?, Base::Url(url.clone())),
            Source::Git { url } => {
                let folder = git_checkout(url, cache)?;
                let path = folder.join(CATALOGUE_FILE);
                let text = std::fs::read(&path).map_err(|_| format!("{url} has no {CATALOGUE_FILE} at its top"))?;
                (text, Base::Folder(folder))
            }
        };
        let catalogue: Catalogue =
            serde_json::from_slice(&text).map_err(|e| format!("{}: not a catalogue: {e}", source.label()))?;
        check_catalogue(&catalogue)?;
        Ok(Listing { source: source.clone(), catalogue, base })
    }

    /// One entry's pack, checked against the hash the catalogue gives for it.
    pub fn pack_bytes(&self, entry: &CatalogueEntry, http: &dyn Http) -> Result<Vec<u8>, String> {
        let bytes = match &self.base {
            Base::Examples => {
                let (_, text) = EXAMPLES
                    .iter()
                    .find(|(name, _)| *name == entry.file)
                    .ok_or_else(|| format!("no example called {}", entry.file))?;
                return Ok(text.as_bytes().to_vec());
            }
            Base::Url(url) => http.get(&zenith_fetch::resolve(url, &entry.file)?, &[], MAX_PACK)?,
            Base::Folder(folder) => read_inside(folder, &entry.file)?,
        };
        let found = zenith_fetch::sha256_hex(&bytes);
        if !found.eq_ignore_ascii_case(entry.sha256.trim()) {
            return Err(format!("{} is not the file its catalogue describes: its SHA-256 is {found}", entry.name));
        }
        Ok(bytes)
    }
}

fn check_catalogue(c: &Catalogue) -> Result<(), String> {
    if c.zenith_catalogue != 1 {
        return Err(format!("{}: catalogue format {} is newer than this game knows", c.name, c.zenith_catalogue));
    }
    let mut seen = std::collections::HashSet::new();
    for m in &c.mods {
        if !valid_id(&m.id) {
            return Err(format!("{}: {:?} is not a mod id (lower-case letters, digits, - and _)", c.name, m.id));
        }
        if !seen.insert(&m.id) {
            return Err(format!("{}: {} is listed twice", c.name, m.id));
        }
        if m.file.is_empty() {
            return Err(format!("{}: {} has no file", c.name, m.id));
        }
    }
    Ok(())
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// A file from a repository, which has to be inside it: no climbing out, no links leading away.
fn read_inside(folder: &Path, relative: &str) -> Result<Vec<u8>, String> {
    let path = Path::new(relative);
    if path.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("{relative} is not a path inside the repository"));
    }
    let root = folder.canonicalize().map_err(|e| e.to_string())?;
    let file = root.join(path).canonicalize().map_err(|_| format!("{relative} is not in the repository"))?;
    if !file.starts_with(&root) {
        return Err(format!("{relative} leads out of the repository"));
    }
    let size = std::fs::metadata(&file).map_err(|e| e.to_string())?.len();
    if size > MAX_PACK {
        return Err(format!("{relative} is larger than a pack may be"));
    }
    std::fs::read(&file).map_err(|e| format!("{relative}: {e}"))
}

/// A repository's files on this computer, cloned the first time and updated after that.
fn git_checkout(url: &str, cache: &Path) -> Result<PathBuf, String> {
    check_git(url)?;
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let folder = cache.join(&zenith_fetch::sha256_hex(url.as_bytes())[..16]);
    if folder.join(".git").is_dir() && git(&["-C", &folder.to_string_lossy(), "pull", "--ff-only", "--quiet"]).is_ok() {
        return Ok(folder);
    }
    let _ = std::fs::remove_dir_all(&folder);
    git(&["clone", "--depth", "1", "--quiet", "--", url, &folder.to_string_lossy()])?;
    Ok(folder)
}

fn git(args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        // Links would let a repository point at files elsewhere on this computer, and changed line
        // endings would make its files differ from the hashes in its catalogue.
        .args(["-c", "core.symlinks=false", "-c", "core.autocrlf=false", "-c", "protocol.ext.allow=never"])
        .args(args)
        // A private repository should fail, not wait for a password nobody is there to type.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run git, which Git sources need: {e}"))?;
    if output.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&output.stderr).trim().to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_fetch::Canned;

    #[test]
    fn the_examples_are_a_valid_catalogue_with_packs_that_load() {
        let listing = Listing::fetch(&Source::Examples, Path::new("unused"), &Canned::new()).expect("the examples");
        assert_eq!(listing.catalogue.mods.len(), 3);
        let mut all = Vec::new();
        for entry in &listing.catalogue.mods {
            let bytes = listing.pack_bytes(entry, &Canned::new()).expect("a pack");
            let pack = zenith_content::Registry::parse_pack(&String::from_utf8_lossy(&bytes)).expect("it parses");
            zenith_content::Registry::build(std::slice::from_ref(&pack), &Default::default())
                .unwrap_or_else(|e| panic!("{} does not build: {e}", entry.id));
            all.push(pack);
        }
        // Someone will turn them all on at once.
        let registry = zenith_content::Registry::build(&all, &Default::default()).expect("all three together");
        for block in ["neon_pink", "marble", "lavender", "hedge"] {
            assert!(registry.block_by_name(block).is_some(), "{block}");
        }
        for recipe_output in ["neon_cyan", "dark_tiles", "marigold"] {
            assert!(
                !registry.recipes_for(registry.item_by_name(recipe_output).expect("an item")).is_empty(),
                "a recipe makes {recipe_output}"
            );
        }
    }

    #[test]
    fn what_is_typed_becomes_the_right_kind_of_source() {
        assert_eq!(
            Source::from_text(" https://a.com/zenith-catalogue.json "),
            Ok(Source::Web { url: "https://a.com/zenith-catalogue.json".into() })
        );
        assert_eq!(
            Source::from_text("https://github.com/someone/packs.git"),
            Ok(Source::Git { url: "https://github.com/someone/packs.git".into() })
        );
        assert_eq!(
            Source::from_text("https://github.com/someone/packs"),
            Ok(Source::Git { url: "https://github.com/someone/packs".into() })
        );
        assert!(Source::from_text("http://a.com/zenith-catalogue.json").is_err(), "plain http to the world");
        assert!(Source::from_text("-uhoh").is_err());
        assert!(Source::from_text("").is_err());
        assert_eq!(Source::Git { url: "https://github.com/someone/packs.git".into() }.label(), "someone/packs");
        assert_eq!(
            Source::Web { url: "https://mods.example.com/zenith/zenith-catalogue.json".into() }.label(),
            "mods.example.com/zenith"
        );
    }

    #[test]
    fn a_web_catalogue_is_fetched_and_its_packs_must_match_their_hashes() {
        let http = Canned::new();
        let pack = include_str!("../builtin/neon.json");
        let catalogue = serde_json::json!({
            "zenith_catalogue": 1, "name": "Test", "mods": [
                {"id": "neon", "name": "Neon", "version": "2", "file": "packs/neon.json", "sha256": zenith_fetch::sha256_hex(pack.as_bytes())},
                {"id": "liar", "name": "Liar", "version": "1", "file": "packs/liar.json", "sha256": "00"},
            ]
        });
        http.answer("https://mods.example.com/zenith-catalogue.json", catalogue.to_string());
        http.answer("https://mods.example.com/packs/neon.json", pack);
        http.answer("https://mods.example.com/packs/liar.json", pack);
        let source = Source::Web { url: "https://mods.example.com/zenith-catalogue.json".into() };
        let listing = Listing::fetch(&source, Path::new("unused"), &http).expect("fetched");
        assert_eq!(listing.pack_bytes(&listing.catalogue.mods[0], &http).expect("matches"), pack.as_bytes());
        let e = listing.pack_bytes(&listing.catalogue.mods[1], &http).expect_err("does not match");
        assert!(e.contains("SHA-256"), "{e}");
    }

    #[test]
    fn bad_catalogues_are_refused_with_a_reason() {
        let http = Canned::new();
        let url = "https://bad.example.com/zenith-catalogue.json";
        let source = Source::Web { url: url.into() };
        for (body, why) in [
            (r#"{"zenith_catalogue": 2, "name": "x", "mods": []}"#, "newer"),
            (
                r#"{"zenith_catalogue": 1, "name": "x", "mods": [{"id": "Bad Id", "name": "a", "version": "1", "file": "a.json"}]}"#,
                "not a mod id",
            ),
            (
                r#"{"zenith_catalogue": 1, "name": "x", "mods": [{"id": "a", "name": "a", "version": "1", "file": "a.json"}, {"id": "a", "name": "b", "version": "1", "file": "b.json"}]}"#,
                "twice",
            ),
            ("not json", "not a catalogue"),
        ] {
            http.answer(url, body);
            let e = Listing::fetch(&source, Path::new("unused"), &http).expect_err(why);
            assert!(e.contains(why), "{e}");
        }
    }

    #[test]
    fn a_repository_cannot_hand_over_files_from_outside_itself() {
        let dir = std::env::temp_dir().join(format!("zenith-mods-inside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo/packs")).expect("folders");
        std::fs::write(dir.join("repo/packs/a.json"), "{}").expect("a file");
        std::fs::write(dir.join("secret.json"), "{}").expect("a file");
        let repo = dir.join("repo");
        assert!(read_inside(&repo, "packs/a.json").is_ok());
        assert!(read_inside(&repo, "../secret.json").is_err());
        assert!(read_inside(&repo, &dir.join("secret.json").to_string_lossy()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
