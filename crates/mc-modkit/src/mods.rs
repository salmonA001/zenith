//! Adding, removing and updating the mods of an installation, with what they need.
use crate::minecraft::{self, InstalledMod, Instance};
use crate::sources::{Hash, Project, Source};
use std::path::Path;
use zenith_fetch::Http;

const MAX_JAR: u64 = 256 << 20;

fn check(hash: &Hash, bytes: &[u8], file: &str) -> Result<(), String> {
    let (expected, found) = match hash {
        Hash::Sha512(h) => (h, zenith_fetch::sha512_hex(bytes)),
        Hash::Sha1(h) => (h, zenith_fetch::sha1_hex(bytes)),
    };
    if expected.is_empty() {
        return Err(format!("{file} came with no hash to check it by, so it was not kept"));
    }
    if !found.eq_ignore_ascii_case(expected) {
        return Err(format!("{file} is not the file its source describes, so it was not kept"));
    }
    Ok(())
}

fn check_file_name(name: &str) -> Result<(), String> {
    let ok = name.ends_with(".jar") && !name.contains(['/', '\\', ':']) && !name.starts_with('.') && name.len() <= 200;
    if ok { Ok(()) } else { Err(format!("{name:?} is not a name a mod's file may have")) }
}

/// Downloads a release into the installation's mods folder, checked against its hash.
fn fetch_jar(
    http: &dyn Http,
    minecraft: &Path,
    instance: &Instance,
    url: &str,
    name: &str,
    hash: &Hash,
) -> Result<(), String> {
    check_file_name(name)?;
    let bytes = http.get(url, &[], MAX_JAR)?;
    check(hash, &bytes, name)?;
    let folder = instance.mods_folder(minecraft);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let partial = folder.join(format!(".{name}.part"));
    std::fs::write(&partial, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, folder.join(name)).map_err(|e| e.to_string())
}

/// Adds a mod and everything it needs that the installation does not have yet. Returns what was
/// added, each as "title version".
pub fn add(
    http: &dyn Http,
    source: &Source,
    minecraft: &Path,
    instance: &mut Instance,
    wanted: &str,
) -> Result<Vec<String>, String> {
    let asked = source.project(http, wanted)?;
    let mut queue: Vec<(Option<Project>, Option<String>, Option<String>, bool)> =
        vec![(Some(asked), None, None, false)];
    let mut added = Vec::new();
    while let Some((project, project_id, version_id, dependency)) = queue.pop() {
        let release = match (&project, &project_id, &version_id) {
            (_, _, Some(v)) => source.version(http, v)?,
            (Some(p), _, None) => source.latest(http, p, &instance.game, instance.loader)?,
            (None, Some(id), None) => {
                source.latest(http, &source.project(http, id)?, &instance.game, instance.loader)?
            }
            (None, None, None) => continue,
        };
        if instance.mods.iter().any(|m| m.source == source.name() && m.project == release.project) {
            continue;
        }
        let project = match project {
            Some(p) => p,
            None => source.project(http, &release.project)?,
        };
        fetch_jar(http, minecraft, instance, &release.url, &release.file_name, &release.hash)?;
        let needs: Vec<String> = release.needs.iter().filter_map(|n| n.project.clone()).collect();
        instance.mods.push(InstalledMod {
            source: source.name().into(),
            project: release.project.clone(),
            slug: project.slug.clone(),
            title: project.title.clone(),
            version: release.version.clone(),
            version_name: release.version_name.clone(),
            file: release.file_name.clone(),
            needs,
            dependency,
        });
        // Saved after every file, so that a failure part way leaves an honest list.
        minecraft::save(minecraft, instance)?;
        added.push(format!("{} {}", project.title, release.version_name));
        for need in release.needs {
            queue.push((None, need.project, need.version, true));
        }
    }
    if added.is_empty() {
        return Err(format!("{wanted} is already in {}", instance.name));
    }
    Ok(added)
}

fn find<'a>(instance: &'a Instance, wanted: &str) -> Option<&'a InstalledMod> {
    let w = wanted.to_lowercase();
    instance
        .mods
        .iter()
        .find(|m| m.slug.to_lowercase() == w || m.project.to_lowercase() == w || m.title.to_lowercase() == w)
}

/// Removes a mod, and what was only there because it needed it. Returns what went, and a note of
/// anything left that still needs the mod removed.
pub fn remove(minecraft: &Path, instance: &mut Instance, wanted: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let target = find(instance, wanted).ok_or_else(|| format!("{} has no mod called {wanted}", instance.name))?.clone();
    let mut gone = vec![target.clone()];
    instance.mods.retain(|m| m.project != target.project);
    // Dependencies nothing needs any more go too, until there are none left to find.
    loop {
        let orphan = instance
            .mods
            .iter()
            .position(|m| m.dependency && !instance.mods.iter().any(|o| o.needs.contains(&m.project)));
        let Some(i) = orphan else { break };
        gone.push(instance.mods.remove(i));
    }
    for m in &gone {
        let _ = std::fs::remove_file(instance.mods_folder(minecraft).join(&m.file));
    }
    let still_needed: Vec<String> =
        instance.mods.iter().filter(|m| m.needs.contains(&target.project)).map(|m| m.title.clone()).collect();
    minecraft::save(minecraft, instance)?;
    Ok((gone.into_iter().map(|m| m.title).collect(), still_needed))
}

/// Moves every mod to its newest version for the installation's game and loader.
pub fn update(
    http: &dyn Http,
    sources: &[Source],
    minecraft: &Path,
    instance: &mut Instance,
) -> Result<Vec<String>, String> {
    let mut changed = Vec::new();
    for i in 0..instance.mods.len() {
        let m = instance.mods[i].clone();
        let Some(source) = sources.iter().find(|s| s.name() == m.source) else {
            changed.push(format!("{}: skipped, as {} is not available (a CurseForge key?)", m.title, m.source));
            continue;
        };
        let project = crate::sources::Project { id: m.project.clone(), slug: m.slug.clone(), title: m.title.clone() };
        let release = source.latest(http, &project, &instance.game, instance.loader)?;
        if release.version == m.version {
            continue;
        }
        fetch_jar(http, minecraft, instance, &release.url, &release.file_name, &release.hash)?;
        if release.file_name != m.file {
            let _ = std::fs::remove_file(instance.mods_folder(minecraft).join(&m.file));
        }
        let entry = &mut instance.mods[i];
        entry.version = release.version.clone();
        entry.version_name = release.version_name.clone();
        entry.file = release.file_name.clone();
        entry.needs = release.needs.iter().filter_map(|n| n.project.clone()).collect();
        minecraft::save(minecraft, instance)?;
        changed.push(format!("{}: {} to {}", m.title, m.version_name, release.version_name));
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loaders::Loader;
    use zenith_fetch::{Canned, escape};

    const API: &str = "https://api.modrinth.com/v2";

    fn versions_url(project: &str) -> String {
        format!(
            "{API}/project/{project}/version?loaders={}&game_versions={}",
            escape("[\"fabric\"]"),
            escape("[\"1.21.4\"]")
        )
    }

    /// A Modrinth with two mods, one needing the other.
    fn modrinth() -> Canned {
        let http = Canned::new();
        let jar_a = b"jar of a".to_vec();
        let jar_b = b"jar of b".to_vec();
        http.answer(&format!("{API}/project/a-mod"), r#"{"id":"A","slug":"a-mod","title":"A Mod"}"#);
        http.answer(&format!("{API}/project/A"), r#"{"id":"A","slug":"a-mod","title":"A Mod"}"#);
        http.answer(&format!("{API}/project/B"), r#"{"id":"B","slug":"b-lib","title":"B Library"}"#);
        http.answer(
            &versions_url("A"),
            serde_json::json!([{"id": "a1", "project_id": "A", "version_number": "1.0", "version_type": "release",
                "files": [{"url": "https://cdn.modrinth.com/a.jar", "filename": "a-1.0.jar", "primary": true, "hashes": {"sha512": zenith_fetch::sha512_hex(&jar_a)}}],
                "dependencies": [{"project_id": "B", "dependency_type": "required"}]}])
            .to_string(),
        );
        http.answer(
            &versions_url("B"),
            serde_json::json!([{"id": "b1", "project_id": "B", "version_number": "2.0", "version_type": "release",
                "files": [{"url": "https://cdn.modrinth.com/b.jar", "filename": "b-2.0.jar", "primary": true, "hashes": {"sha512": zenith_fetch::sha512_hex(&jar_b)}}],
                "dependencies": []}])
            .to_string(),
        );
        http.answer("https://cdn.modrinth.com/a.jar", jar_a);
        http.answer("https://cdn.modrinth.com/b.jar", jar_b);
        http
    }

    fn setup(name: &str) -> (std::path::PathBuf, Instance) {
        let mc = std::env::temp_dir().join(format!("modkit-mods-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&mc);
        let instance = Instance {
            name: "test".into(),
            game: "1.21.4".into(),
            loader: Loader::Fabric,
            loader_version: "0.16.14".into(),
            version_id: "fabric-loader-0.16.14-1.21.4".into(),
            mods: Vec::new(),
        };
        minecraft::save(&mc, &instance).expect("saved");
        (mc, instance)
    }

    #[test]
    fn adding_a_mod_brings_what_it_needs_and_removing_it_takes_that_away() {
        let http = modrinth();
        let (mc, mut instance) = setup("deps");
        let added = add(&http, &Source::Modrinth, &mc, &mut instance, "a-mod").expect("added");
        assert_eq!(added, ["A Mod 1.0", "B Library 2.0"]);
        let folder = instance.mods_folder(&mc);
        assert!(folder.join("a-1.0.jar").is_file() && folder.join("b-2.0.jar").is_file());
        assert!(instance.mods[1].dependency, "B came because A needed it");
        assert_eq!(minecraft::load(&mc, "test").expect("saved").mods.len(), 2);
        assert!(add(&http, &Source::Modrinth, &mc, &mut instance, "a-mod").is_err(), "already there");

        let (gone, still) = remove(&mc, &mut instance, "a-mod").expect("removed");
        assert_eq!(gone, ["A Mod", "B Library"], "B goes with the only mod that needed it");
        assert!(still.is_empty());
        assert!(!folder.join("a-1.0.jar").exists() && !folder.join("b-2.0.jar").exists());
        assert!(instance.mods.is_empty());
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[test]
    fn a_file_that_does_not_match_its_hash_is_never_kept() {
        let http = modrinth();
        http.answer("https://cdn.modrinth.com/b.jar", b"something else entirely".to_vec());
        let (mc, mut instance) = setup("tampered");
        let e = add(&http, &Source::Modrinth, &mc, &mut instance, "a-mod").expect_err("refused");
        assert!(e.contains("not the file"), "{e}");
        assert!(!instance.mods_folder(&mc).join("b-2.0.jar").exists());
        assert_eq!(instance.mods.len(), 1, "A, which was fine, stays, and the list says so");
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[test]
    fn updating_replaces_old_files_with_new_ones() {
        let http = modrinth();
        let (mc, mut instance) = setup("update");
        add(&http, &Source::Modrinth, &mc, &mut instance, "a-mod").expect("added");
        let jar = b"jar of a, version 2".to_vec();
        http.answer(
            &versions_url("A"),
            serde_json::json!([{"id": "a2", "project_id": "A", "version_number": "2.0", "version_type": "release",
                "files": [{"url": "https://cdn.modrinth.com/a2.jar", "filename": "a-2.0.jar", "primary": true, "hashes": {"sha512": zenith_fetch::sha512_hex(&jar)}}],
                "dependencies": [{"project_id": "B", "dependency_type": "required"}]}])
            .to_string(),
        );
        http.answer("https://cdn.modrinth.com/a2.jar", jar);
        let changed = update(&http, &[Source::Modrinth], &mc, &mut instance).expect("updated");
        assert_eq!(changed, ["A Mod: 1.0 to 2.0"]);
        let folder = instance.mods_folder(&mc);
        assert!(folder.join("a-2.0.jar").is_file() && !folder.join("a-1.0.jar").exists());
        let _ = std::fs::remove_dir_all(&mc);
    }

    #[test]
    fn file_names_cannot_reach_out_of_the_mods_folder() {
        assert!(check_file_name("sodium-0.6.jar").is_ok());
        for bad in ["../evil.jar", "a/b.jar", "C:evil.jar", ".hidden.jar", "notajar.exe"] {
            assert!(check_file_name(bad).is_err(), "{bad}");
        }
    }
}
