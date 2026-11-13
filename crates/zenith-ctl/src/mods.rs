//! `zenith-ctl mods`: the mod library from the command line. It needs no game running: it works on
//! the same folder the game's Mods screen does.
use serde_json::{Value, json};
use zenith_mods::{EntryState, Library, Listing, Source};

pub const USAGE: &str = "\
zenith-ctl mods: the mod library, as the game's Mods screen sees it

USAGE:
  zenith-ctl mods [list]              installed mods, and whether each is on
  zenith-ctl mods sources             where it looks for mods, numbered
  zenith-ctl mods add-source TEXT     a catalogue's https:// address, a Git repository, or a folder
  zenith-ctl mods remove-source N     stop looking somewhere
  zenith-ctl mods browse [N]          what a source has (default: every source)
  zenith-ctl mods install ID [N]      install a mod, from source N or the first that has it
  zenith-ctl mods update              install every newer version the sources have
  zenith-ctl mods on ID | off ID      turn a mod on or off
  zenith-ctl mods remove ID           uninstall a mod
  zenith-ctl mods convert PATH        make a mod out of a Minecraft mod's .jar, or every .jar in a
                                      folder: its blocks, items and recipes, not its code

The library is the mods folder in ZENITH_HOME, or the user's Zenith data folder.
";

fn source_number(text: Option<&String>, count: usize) -> Result<Option<usize>, String> {
    let Some(text) = text else { return Ok(None) };
    let n: usize = text.parse().map_err(|_| format!("{text} is not a source number"))?;
    if n == 0 || n > count {
        return Err(format!("there is no source {n}: zenith-ctl mods sources lists them"));
    }
    Ok(Some(n - 1))
}

fn listings(library: &Library, only: Option<usize>) -> Vec<(usize, Result<Listing, String>)> {
    library
        .sources()
        .iter()
        .enumerate()
        .filter(|(i, _)| only.is_none_or(|o| o == *i))
        .map(|(i, s)| (i, Listing::fetch(s, &library.cache(), &zenith_fetch::Curl)))
        .collect()
}

fn state_text(state: &EntryState) -> String {
    match state {
        EntryState::NotInstalled => "not installed".into(),
        EntryState::Installed => "installed".into(),
        EntryState::Update(old) => format!("update from {old}"),
    }
}

pub fn run(args: &[String]) -> Result<Value, String> {
    let mut library = Library::open(&zenith_mods::default_home())?;
    let word = args.first().map(String::as_str).unwrap_or("list");
    let arg = |i: usize, what: &str| args.get(i).cloned().ok_or_else(|| format!("mods {word} needs {what}"));
    Ok(match word {
        "list" => json!(
            library
                .installed()
                .iter()
                .map(|m| json!({
                    "id": m.manifest.id, "name": m.manifest.name, "version": m.manifest.version, "on": m.enabled,
                    "from": m.manifest.source.as_ref().map_or("the mods folder".to_string(), Source::label),
                }))
                .collect::<Vec<_>>()
        ),
        "sources" => json!(
            library
                .sources()
                .iter()
                .enumerate()
                .map(|(i, s)| json!({"number": i + 1, "source": s, "label": s.label()}))
                .collect::<Vec<_>>()
        ),
        "add-source" => {
            let source = Source::from_text(&arg(1, "an address")?)?;
            library.add_source(source.clone())?;
            json!({"added": source, "number": library.sources().iter().position(|s| *s == source).map_or(0, |i| i + 1)})
        }
        "remove-source" => {
            let n = source_number(args.get(1), library.sources().len())?.ok_or("mods remove-source needs a number")?;
            let gone = library.sources()[n].clone();
            library.remove_source(n)?;
            json!({"removed": gone})
        }
        "browse" => {
            let only = source_number(args.get(1), library.sources().len())?;
            json!(
                listings(&library, only)
                    .into_iter()
                    .map(|(i, l)| match l {
                        Ok(l) => json!({
                            "number": i + 1, "catalogue": l.catalogue.name,
                            "mods": l.catalogue.mods.iter().map(|e| json!({
                                "id": e.id, "name": e.name, "version": e.version, "author": e.author,
                                "description": e.description, "state": state_text(&library.state_of(e)),
                            })).collect::<Vec<_>>(),
                        }),
                        Err(e) => json!({"number": i + 1, "error": e}),
                    })
                    .collect::<Vec<_>>()
            )
        }
        "install" => {
            let id = arg(1, "a mod id")?;
            let only = source_number(args.get(2), library.sources().len())?;
            let mut why = Vec::new();
            for (_, listing) in listings(&library, only) {
                match listing {
                    Ok(listing) => {
                        if let Some(entry) = listing.catalogue.mods.iter().find(|e| e.id == id) {
                            let m = library.install(&listing, entry, &zenith_fetch::Curl)?;
                            return Ok(
                                json!({"installed": m.id, "name": m.name, "version": m.version, "from": listing.source.label()}),
                            );
                        }
                    }
                    Err(e) => why.push(e),
                }
            }
            let mut message = format!("no source has a mod called {id}");
            if !why.is_empty() {
                message.push_str(&format!(" (and some could not be read: {})", why.join("; ")));
            }
            return Err(message);
        }
        "update" => {
            let mut updated = Vec::new();
            let mut problems = Vec::new();
            for (_, listing) in listings(&library, None) {
                let listing = match listing {
                    Ok(l) => l,
                    Err(e) => {
                        problems.push(e);
                        continue;
                    }
                };
                for entry in &listing.catalogue.mods {
                    if let EntryState::Update(old) = library.state_of(entry) {
                        match library.install(&listing, entry, &zenith_fetch::Curl) {
                            Ok(m) => updated.push(json!({"id": m.id, "from": old, "to": m.version})),
                            Err(e) => problems.push(e),
                        }
                    }
                }
            }
            json!({"updated": updated, "problems": problems})
        }
        "on" | "off" => {
            let id = arg(1, "a mod id")?;
            library.set_enabled(&id, word == "on")?;
            json!({"id": id, "on": word == "on"})
        }
        "remove" => {
            let id = arg(1, "a mod id")?;
            library.remove(&id)?;
            json!({"removed": id})
        }
        "convert" => {
            let path = std::path::PathBuf::from(arg(1, "a .jar file or a folder of them")?);
            let jars: Vec<std::path::PathBuf> = if path.is_dir() {
                let entries = std::fs::read_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let mut jars: Vec<_> =
                    entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jar")).collect();
                jars.sort();
                jars
            } else {
                vec![path]
            };
            if jars.is_empty() {
                return Err("there are no .jar files there".into());
            }
            let mut done = Vec::new();
            for jar in jars {
                let file = jar.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                done.push(match zenith_mods::convert_jar(&jar) {
                    // A mod with nothing to convert is reported, and adds nothing to the library.
                    Ok(c) => {
                        let installed = c.report.blocks + c.report.items > 0;
                        let error = if installed { library.add_converted(&c).err() } else { None };
                        json!({
                            "file": file, "id": c.manifest.id, "name": c.manifest.name,
                            "installed": installed && error.is_none(), "error": error,
                            "blocks": c.report.blocks, "items": c.report.items, "recipes": c.report.recipes,
                            "blocks_left_out": c.report.blocks_left, "recipes_left_out": c.report.recipes_left,
                            "notes": c.report.notes,
                        })
                    }
                    Err(e) => json!({"file": file, "installed": false, "error": e}),
                });
            }
            json!(done)
        }
        "help" | "--help" | "-h" => json!(USAGE),
        other => return Err(format!("no mods command called {other}\n\n{USAGE}")),
    })
}
