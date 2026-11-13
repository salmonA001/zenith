//! Where mods come from: Modrinth, whose API is open to all, and CurseForge, which needs a key from
//! its developer console. Both answer the same few questions here.
use crate::loaders::Loader;
use serde_json::Value;
use zenith_fetch::{Http, escape};

const MODRINTH: &str = "https://api.modrinth.com/v2";
const CURSEFORGE: &str = "https://api.curseforge.com/v1";
/// CurseForge's numbers for the game, and for mods within it.
const CF_GAME: u32 = 432;
const CF_MODS: u32 = 6;
const MAX_JSON: u64 = 16 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Modrinth,
    CurseForge { key: String },
}

/// A search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
}

/// A project, as its source names it.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Hash {
    Sha512(String),
    Sha1(String),
}

/// Something a release needs: a project, sometimes at one exact version.
#[derive(Clone, Debug, PartialEq)]
pub struct Need {
    pub project: Option<String>,
    pub version: Option<String>,
}

/// One file of one version of a project.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub project: String,
    pub version: String,
    pub version_name: String,
    pub file_name: String,
    pub url: String,
    pub hash: Hash,
    pub needs: Vec<Need>,
}

impl Source {
    pub fn parse(name: &str, key: Option<&str>) -> Result<Source, String> {
        match name.to_ascii_lowercase().as_str() {
            "modrinth" => Ok(Source::Modrinth),
            "curseforge" | "cf" => match key {
                Some(k) if !k.trim().is_empty() => Ok(Source::CurseForge { key: k.trim().to_string() }),
                _ => Err("CurseForge needs an API key: --curseforge-key KEY, or CURSEFORGE_API_KEY. Get one at console.curseforge.com.".into()),
            },
            other => Err(format!("{other} is not a source: modrinth or curseforge")),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Source::Modrinth => "modrinth",
            Source::CurseForge { .. } => "curseforge",
        }
    }

    fn get(&self, http: &dyn Http, url: &str) -> Result<Value, String> {
        let bytes = match self {
            Source::Modrinth => http.get(url, &[], MAX_JSON)?,
            Source::CurseForge { key } => {
                http.get(url, &[("x-api-key", key), ("Accept", "application/json")], MAX_JSON)?
            }
        };
        serde_json::from_slice(&bytes).map_err(|e| format!("{url}: unexpected reply: {e}"))
    }

    /// The loaders whose mods an installation can use. Quilt runs most Fabric mods as well.
    fn loaders(loader: Loader) -> Vec<&'static str> {
        match loader {
            Loader::Quilt => vec!["quilt", "fabric"],
            other => vec![other.modrinth()],
        }
    }

    pub fn search(
        &self,
        http: &dyn Http,
        query: &str,
        game: &str,
        loader: Loader,
        limit: usize,
    ) -> Result<Vec<Hit>, String> {
        let limit = limit.clamp(1, 50);
        match self {
            Source::Modrinth => {
                let loaders: Vec<String> =
                    Self::loaders(loader).iter().map(|l| format!("\"categories:{l}\"")).collect();
                let facets = format!("[[\"project_type:mod\"],[{}],[\"versions:{game}\"]]", loaders.join(","));
                let url = format!(
                    "{MODRINTH}/search?query={}&limit={limit}&index=relevance&facets={}",
                    escape(query),
                    escape(&facets)
                );
                let answer = self.get(http, &url)?;
                Ok(answer["hits"]
                    .as_array()
                    .map(|hits| {
                        hits.iter()
                            .map(|h| Hit {
                                id: text(&h["project_id"]),
                                slug: text(&h["slug"]),
                                title: text(&h["title"]),
                                description: text(&h["description"]),
                                author: text(&h["author"]),
                                downloads: h["downloads"].as_u64().unwrap_or(0),
                            })
                            .collect()
                    })
                    .unwrap_or_default())
            }
            Source::CurseForge { .. } => {
                let url = format!(
                    "{CURSEFORGE}/mods/search?gameId={CF_GAME}&classId={CF_MODS}&searchFilter={}&gameVersion={}&modLoaderType={}&sortField=2&sortOrder=desc&pageSize={limit}",
                    escape(query),
                    escape(game),
                    loader.curseforge()
                );
                let answer = self.get(http, &url)?;
                Ok(answer["data"].as_array().map(|list| list.iter().map(cf_hit).collect()).unwrap_or_default())
            }
        }
    }

    /// A project from its id, or from the short name in its address.
    pub fn project(&self, http: &dyn Http, wanted: &str) -> Result<Project, String> {
        let wanted = wanted.trim();
        if wanted.is_empty() || wanted.contains(['/', '?', '#', '&', ' ']) {
            return Err(format!("{wanted:?} is not a project's id or short name"));
        }
        match self {
            Source::Modrinth => {
                let p = self
                    .get(http, &format!("{MODRINTH}/project/{}", escape(wanted)))
                    .map_err(|e| not_found(wanted, "Modrinth", e))?;
                Ok(Project { id: text(&p["id"]), slug: text(&p["slug"]), title: text(&p["title"]) })
            }
            Source::CurseForge { .. } => {
                let url = if wanted.bytes().all(|b| b.is_ascii_digit()) {
                    format!("{CURSEFORGE}/mods/{wanted}")
                } else {
                    format!("{CURSEFORGE}/mods/search?gameId={CF_GAME}&classId={CF_MODS}&slug={}", escape(wanted))
                };
                let answer = self.get(http, &url).map_err(|e| not_found(wanted, "CurseForge", e))?;
                let data = if answer["data"].is_array() { answer["data"][0].clone() } else { answer["data"].clone() };
                if data.is_null() {
                    return Err(format!("CurseForge has no mod called {wanted}"));
                }
                let hit = cf_hit(&data);
                Ok(Project { id: hit.id, slug: hit.slug, title: hit.title })
            }
        }
    }

    /// The newest version of a project for a game version and loader: a release if there is one.
    pub fn latest(&self, http: &dyn Http, project: &Project, game: &str, loader: Loader) -> Result<Release, String> {
        let none = || format!("{} has no version for {} on Minecraft {game}", project.title, loader.name());
        match self {
            Source::Modrinth => {
                let loaders: Vec<String> = Self::loaders(loader).iter().map(|l| format!("\"{l}\"")).collect();
                let url = format!(
                    "{MODRINTH}/project/{}/version?loaders={}&game_versions={}",
                    escape(&project.id),
                    escape(&format!("[{}]", loaders.join(","))),
                    escape(&format!("[\"{game}\"]"))
                );
                let list = self.get(http, &url)?;
                let list = list.as_array().ok_or_else(none)?;
                let chosen = list.iter().find(|v| v["version_type"] == "release").or(list.first()).ok_or_else(none)?;
                modrinth_release(chosen)
            }
            Source::CurseForge { .. } => {
                let url = format!(
                    "{CURSEFORGE}/mods/{}/files?gameVersion={}&modLoaderType={}&pageSize=50",
                    escape(&project.id),
                    escape(game),
                    loader.curseforge()
                );
                let answer = self.get(http, &url)?;
                let list = answer["data"].as_array().ok_or_else(none)?;
                // A release: CurseForge calls that release type 1.
                let chosen = list.iter().find(|f| f["releaseType"] == 1).or(list.first()).ok_or_else(none)?;
                self.cf_release(http, &project.id, chosen, &project.title)
            }
        }
    }

    /// One exact version, which only Modrinth's dependencies ask for.
    pub fn version(&self, http: &dyn Http, id: &str) -> Result<Release, String> {
        match self {
            Source::Modrinth => modrinth_release(&self.get(http, &format!("{MODRINTH}/version/{}", escape(id)))?),
            Source::CurseForge { .. } => Err("CurseForge dependencies name projects, not versions".into()),
        }
    }

    fn cf_release(&self, http: &dyn Http, project: &str, file: &Value, title: &str) -> Result<Release, String> {
        let file_id = file["id"].as_u64().map(|n| n.to_string()).unwrap_or_default();
        let url = match file["downloadUrl"].as_str() {
            Some(u) => u.to_string(),
            // Some authors keep their files to CurseForge's own site and app; asking again is the
            // only other way, and it gives the same answer for those.
            None => self.get(http, &format!("{CURSEFORGE}/mods/{project}/files/{file_id}/download-url"))?["data"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("the author of {title} lets only CurseForge's own site and app download it"))?,
        };
        let sha1 = file["hashes"]
            .as_array()
            .and_then(|h| h.iter().find(|x| x["algo"] == 1))
            .map(|x| text(&x["value"]))
            .unwrap_or_default();
        let needs = file["dependencies"]
            .as_array()
            .map(|deps| {
                deps.iter()
                    // Relation 3 is a required dependency.
                    .filter(|d| d["relationType"] == 3)
                    .filter_map(|d| d["modId"].as_u64())
                    .map(|id| Need { project: Some(id.to_string()), version: None })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Release {
            project: project.to_string(),
            version: file_id,
            version_name: text(&file["displayName"]),
            file_name: text(&file["fileName"]),
            url,
            hash: Hash::Sha1(sha1),
            needs,
        })
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn not_found(wanted: &str, source: &str, e: String) -> String {
    if e.contains("404") { format!("{source} has no mod called {wanted}") } else { e }
}

fn cf_hit(m: &Value) -> Hit {
    Hit {
        id: text(&m["id"]),
        slug: text(&m["slug"]),
        title: text(&m["name"]),
        description: text(&m["summary"]),
        author: m["authors"].as_array().and_then(|a| a.first()).map(|a| text(&a["name"])).unwrap_or_default(),
        downloads: m["downloadCount"].as_f64().unwrap_or(0.0) as u64,
    }
}

fn modrinth_release(v: &Value) -> Result<Release, String> {
    let files = v["files"].as_array().ok_or("a version with no files")?;
    let file = files.iter().find(|f| f["primary"] == true).or(files.first()).ok_or("a version with no files")?;
    let needs = v["dependencies"]
        .as_array()
        .map(|deps| {
            deps.iter()
                .filter(|d| d["dependency_type"] == "required")
                .map(|d| Need {
                    project: d["project_id"].as_str().map(str::to_string),
                    version: d["version_id"].as_str().map(str::to_string),
                })
                .filter(|n| n.project.is_some() || n.version.is_some())
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        project: text(&v["project_id"]),
        version: text(&v["id"]),
        version_name: text(&v["version_number"]),
        file_name: text(&file["filename"]),
        url: text(&file["url"]),
        hash: Hash::Sha512(text(&file["hashes"]["sha512"])),
        needs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenith_fetch::Canned;

    #[test]
    fn a_curseforge_source_needs_its_key() {
        assert!(Source::parse("curseforge", None).is_err());
        assert!(Source::parse("curseforge", Some("  ")).is_err());
        assert_eq!(Source::parse("cf", Some("k")), Ok(Source::CurseForge { key: "k".into() }));
        assert_eq!(Source::parse("Modrinth", None), Ok(Source::Modrinth));
        assert!(Source::parse("somewhere", None).is_err());
    }

    #[test]
    fn modrinth_search_asks_for_the_loader_and_game_and_reads_the_hits() {
        let http = Canned::new();
        let facets = escape(r#"[["project_type:mod"],["categories:quilt","categories:fabric"],["versions:1.21.4"]]"#);
        http.answer(
            &format!("{MODRINTH}/search?query=sodium&limit=5&index=relevance&facets={facets}"),
            r#"{"hits":[{"project_id":"AANobbMI","slug":"sodium","title":"Sodium","description":"Fast","author":"jellysquid3","downloads":1000}]}"#,
        );
        let hits = Source::Modrinth.search(&http, "sodium", "1.21.4", Loader::Quilt, 5).expect("hits");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].slug, "sodium");
        assert_eq!(hits[0].downloads, 1000);
    }

    #[test]
    fn the_newest_release_is_chosen_over_newer_betas() {
        let http = Canned::new();
        let url = format!(
            "{MODRINTH}/project/P1/version?loaders={}&game_versions={}",
            escape("[\"fabric\"]"),
            escape("[\"1.21.4\"]")
        );
        http.answer(
            &url,
            r#"[
                {"id":"v3","project_id":"P1","version_number":"3.0-beta","version_type":"beta","files":[{"url":"https://cdn.modrinth.com/b.jar","filename":"b.jar","primary":true,"hashes":{"sha512":"aa"}}],"dependencies":[]},
                {"id":"v2","project_id":"P1","version_number":"2.0","version_type":"release","files":[
                    {"url":"https://cdn.modrinth.com/extra.jar","filename":"extra.jar","primary":false,"hashes":{"sha512":"bb"}},
                    {"url":"https://cdn.modrinth.com/main.jar","filename":"main.jar","primary":true,"hashes":{"sha512":"cc"}}],
                 "dependencies":[{"project_id":"API","version_id":null,"dependency_type":"required"},{"project_id":"OPT","dependency_type":"optional"}]}
            ]"#,
        );
        let project = Project { id: "P1".into(), slug: "p".into(), title: "P".into() };
        let r = Source::Modrinth.latest(&http, &project, "1.21.4", Loader::Fabric).expect("a release");
        assert_eq!(r.version, "v2");
        assert_eq!(r.file_name, "main.jar", "the primary file");
        assert_eq!(r.hash, Hash::Sha512("cc".into()));
        assert_eq!(r.needs, vec![Need { project: Some("API".into()), version: None }], "only what is required");
    }

    #[test]
    fn curseforge_files_give_their_sha1_and_required_mods() {
        let http = Canned::new();
        http.answer(
            &format!("{CURSEFORGE}/mods/123/files?gameVersion=1.21.4&modLoaderType=4&pageSize=50"),
            r#"{"data":[{"id":555,"displayName":"Thing 1.0","fileName":"thing-1.0.jar","releaseType":1,"downloadUrl":"https://edge.forgecdn.net/files/thing-1.0.jar",
                "hashes":[{"value":"md5x","algo":2},{"value":"sha1x","algo":1}],"dependencies":[{"modId":9,"relationType":3},{"modId":10,"relationType":2}]}]}"#,
        );
        let source = Source::CurseForge { key: "k".into() };
        let project = Project { id: "123".into(), slug: "thing".into(), title: "Thing".into() };
        let r = source.latest(&http, &project, "1.21.4", Loader::Fabric).expect("a release");
        assert_eq!(r.hash, Hash::Sha1("sha1x".into()));
        assert_eq!(r.needs, vec![Need { project: Some("9".into()), version: None }]);
        assert_eq!(r.url, "https://edge.forgecdn.net/files/thing-1.0.jar");
    }

    #[test]
    fn a_project_that_does_not_exist_says_so() {
        let http = Canned::new();
        let e = Source::Modrinth.project(&http, "no-such-mod").expect_err("missing");
        assert!(e.contains("no mod called"), "{e}");
        assert!(Source::Modrinth.project(&http, "a/../b").is_err());
    }
}
