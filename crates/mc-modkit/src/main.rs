//! `mc-modkit`: mod loaders and mods for your own copy of Minecraft: Java Edition.
//!
//! It is a separate tool that happens to live beside Zenith and share its download code. It is not
//! part of Zenith, which cannot run these mods, and it is not an official Minecraft product.
mod loaders;
mod minecraft;
mod mods;
mod sources;

use loaders::Loader;
use minecraft::Instance;
use sources::Source;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use zenith_fetch::Curl;

const USAGE: &str = "\
mc-modkit: mod loaders and mods for your own copy of Minecraft: Java Edition

Each installation it makes has its own folder of mods and its own entry in the official launcher,
so it never changes the game you already have. Not an official Minecraft product.

USAGE:
  mc-modkit                                   step by step, asking as it goes
  mc-modkit loaders                           the loaders it can install
  mc-modkit versions [--loader L] [--all]     game versions a loader supports (releases unless --all)
  mc-modkit new NAME --loader L [--game V] [--loader-version X]
                                              a new installation (default: the newest game version)
  mc-modkit list                              the installations it has made
  mc-modkit search QUERY [--for NAME | --game V --loader L] [--limit N]
  mc-modkit add NAME MOD...                   mods, by the short name in their address, and what they need
  mc-modkit mods NAME                         what an installation has
  mc-modkit remove NAME MOD                   a mod, and what only it needed
  mc-modkit update NAME                       every mod to its newest version
  mc-modkit delete NAME --yes                 an installation, its mods and its launcher entry

LOADERS: fabric, quilt, forge, neoforge. Forge and NeoForge run their own installers, which need Java.

OPTIONS, anywhere:
  --minecraft DIR        the game's folder (default: the official launcher's)
  --source S             modrinth (default) or curseforge
  --curseforge-key KEY   CurseForge's API key, from console.curseforge.com, or CURSEFORGE_API_KEY
";

struct Options {
    minecraft: PathBuf,
    source: String,
    curseforge_key: Option<String>,
    loader: Option<String>,
    game: Option<String>,
    loader_version: Option<String>,
    for_instance: Option<String>,
    limit: usize,
    all: bool,
    yes: bool,
    words: Vec<String>,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Options, String> {
        let mut o = Options {
            minecraft: minecraft::default_folder(),
            source: "modrinth".into(),
            curseforge_key: std::env::var("CURSEFORGE_API_KEY").ok(),
            loader: None,
            game: None,
            loader_version: None,
            for_instance: None,
            limit: 10,
            all: false,
            yes: false,
            words: Vec::new(),
        };
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            let mut value = |what: &str| it.next().ok_or_else(|| format!("{arg} needs {what}"));
            match arg.as_str() {
                "--minecraft" => o.minecraft = PathBuf::from(value("a folder")?),
                "--source" => o.source = value("modrinth or curseforge")?,
                "--curseforge-key" => o.curseforge_key = Some(value("a key")?),
                "--loader" => o.loader = Some(value("a loader")?),
                "--game" => o.game = Some(value("a game version")?),
                "--loader-version" => o.loader_version = Some(value("a version")?),
                "--for" => o.for_instance = Some(value("an installation")?),
                "--limit" => o.limit = value("a number")?.parse().map_err(|_| "--limit needs a number")?,
                "--all" => o.all = true,
                "--yes" => o.yes = true,
                "--help" | "-h" => o.words = vec!["help".into()],
                flag if flag.starts_with("--") => return Err(format!("unknown option {flag}\n\n{USAGE}")),
                word => o.words.push(word.to_string()),
            }
        }
        Ok(o)
    }

    fn source(&self) -> Result<Source, String> {
        Source::parse(&self.source, self.curseforge_key.as_deref())
    }

    /// Every source this run can use, for updates that may need either.
    fn sources(&self) -> Vec<Source> {
        let mut list = vec![Source::Modrinth];
        if let Ok(cf) = Source::parse("curseforge", self.curseforge_key.as_deref()) {
            list.push(cf);
        }
        list
    }

    fn word(&self, i: usize, what: &str) -> Result<&str, String> {
        self.words.get(i).map(String::as_str).ok_or_else(|| format!("{} needs {what}\n\n{USAGE}", self.words[0]))
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("mc-modkit: {e}");
        std::process::exit(1);
    }
}

/// The time now, as the launcher writes it: 2026-11-03T12:00:00.000Z.
fn now() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Days to a civil date, after Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

fn run() -> Result<(), String> {
    let o = Options::parse(std::env::args().skip(1).collect())?;
    let http = Curl;
    let Some(command) = o.words.first().cloned() else {
        return guided(&o);
    };
    match command.as_str() {
        "help" => print!("{USAGE}"),
        "loaders" => {
            for l in Loader::ALL {
                let how = if matches!(l, Loader::Forge | Loader::NeoForge) {
                    "its own installer, which needs Java"
                } else {
                    "its launcher profile"
                };
                println!("{:<9} {}: installed with {how}", l.modrinth(), l.name());
            }
        }
        "versions" => {
            let loader = Loader::parse(o.loader.as_deref().unwrap_or("fabric"))?;
            let list = loaders::game_versions(&http, loader, o.all)?;
            println!("{}", list.join("\n"));
        }
        "new" => {
            let name = o.word(1, "a name")?;
            let loader =
                Loader::parse(o.loader.as_deref().ok_or("new needs --loader: fabric, quilt, forge or neoforge")?)?;
            let instance = create(&http, &o, name, loader, o.game.clone())?;
            println!(
                "Made {}. In the Minecraft Launcher, it is the installation called \"{} ({} {})\".",
                instance.summary(),
                name,
                loader.name(),
                instance.game
            );
            println!("Add mods with: mc-modkit add {name} MOD");
        }
        "list" => {
            let all = minecraft::list(&o.minecraft);
            if all.is_empty() {
                println!("None yet, in {}. mc-modkit new NAME --loader fabric makes one.", o.minecraft.display());
            }
            for i in all {
                println!("{}", i.summary());
            }
        }
        "search" => {
            let query = o.words[1..].join(" ");
            if query.is_empty() {
                return Err("search needs something to search for".into());
            }
            let (game, loader) = match &o.for_instance {
                Some(name) => {
                    let i = minecraft::load(&o.minecraft, name)?;
                    (i.game, i.loader)
                }
                None => (
                    o.game.clone().ok_or("search needs --for NAME, or --game V and --loader L")?,
                    Loader::parse(o.loader.as_deref().ok_or("search needs --for NAME, or --game V and --loader L")?)?,
                ),
            };
            for hit in o.source()?.search(&http, &query, &game, loader, o.limit)? {
                println!(
                    "{:<28} {} by {} ({} downloads)\n{:<28} {}",
                    hit.slug, hit.title, hit.author, hit.downloads, "", hit.description
                );
            }
        }
        "add" => {
            let name = o.word(1, "an installation")?;
            let wanted = &o.words[2..];
            if wanted.is_empty() {
                return Err("add needs at least one mod".into());
            }
            let mut instance = minecraft::load(&o.minecraft, name)?;
            let source = o.source()?;
            for w in wanted {
                for line in mods::add(&http, &source, &o.minecraft, &mut instance, w)? {
                    println!("Added {line}");
                }
            }
        }
        "mods" => {
            let instance = minecraft::load(&o.minecraft, o.word(1, "an installation")?)?;
            if instance.mods.is_empty() {
                println!("{} has no mods yet.", instance.name);
            }
            for m in &instance.mods {
                let why = if m.dependency { "  (needed by another)" } else { "" };
                println!("{:<28} {} {} from {}{why}", m.slug, m.title, m.version_name, m.source);
            }
        }
        "remove" => {
            let mut instance = minecraft::load(&o.minecraft, o.word(1, "an installation")?)?;
            let (gone, still) = mods::remove(&o.minecraft, &mut instance, o.word(2, "a mod")?)?;
            for g in gone {
                println!("Removed {g}");
            }
            if !still.is_empty() {
                println!("Note: {} still need it, and may not start without it.", still.join(", "));
            }
        }
        "update" => {
            let mut instance = minecraft::load(&o.minecraft, o.word(1, "an installation")?)?;
            let changed = mods::update(&http, &o.sources(), &o.minecraft, &mut instance)?;
            if changed.is_empty() {
                println!("Everything in {} is up to date.", instance.name);
            }
            for c in changed {
                println!("{c}");
            }
        }
        "delete" => {
            let instance = minecraft::load(&o.minecraft, o.word(1, "an installation")?)?;
            if !o.yes {
                return Err(format!(
                    "this deletes {} and its mods folder: add --yes to go ahead",
                    instance.folder(&o.minecraft).display()
                ));
            }
            minecraft::remove_profile(&o.minecraft, &instance.name)?;
            std::fs::remove_dir_all(instance.folder(&o.minecraft)).map_err(|e| e.to_string())?;
            println!("Deleted {}.", instance.name);
        }
        other => return Err(format!("no command called {other}\n\n{USAGE}")),
    }
    Ok(())
}

/// Makes an installation: the loader, its folder, and its entry in the launcher.
fn create(http: &Curl, o: &Options, name: &str, loader: Loader, game: Option<String>) -> Result<Instance, String> {
    minecraft::check_name(name)?;
    if minecraft::load(&o.minecraft, name).is_ok() {
        return Err(format!("there is already an installation called {name}"));
    }
    if !o.minecraft.is_dir() {
        return Err(format!(
            "{} does not exist: is Minecraft installed? --minecraft DIR says where",
            o.minecraft.display()
        ));
    }
    let game = match game {
        Some(g) => g,
        None => loaders::game_versions(http, loader, false)?.into_iter().next().ok_or("no game versions were found")?,
    };
    let loader_version = match &o.loader_version {
        Some(v) => v.clone(),
        None => loaders::latest_loader(http, loader, &game)?,
    };
    let version_id = loaders::install(http, loader, &game, &loader_version, &o.minecraft)?;
    let instance = Instance { name: name.to_string(), game, loader, loader_version, version_id, mods: Vec::new() };
    minecraft::save(&o.minecraft, &instance)?;
    minecraft::add_profile(&o.minecraft, &instance, &now())?;
    Ok(instance)
}

fn ask(question: &str) -> Result<String, String> {
    print!("{question}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    if read == 0 {
        return Err("stopped".into());
    }
    Ok(line.trim().to_string())
}

/// The step-by-step way: an installation, then mods for it, one search at a time.
fn guided(o: &Options) -> Result<(), String> {
    let http = Curl;
    println!("mc-modkit: mod loaders and mods for your own copy of Minecraft: Java Edition.");
    println!(
        "Game folder: {}{}",
        o.minecraft.display(),
        if o.minecraft.is_dir() { "" } else { " (not found: --minecraft DIR says where)" }
    );
    let existing = minecraft::list(&o.minecraft);
    for (i, inst) in existing.iter().enumerate() {
        println!("  {}. {}", i + 1, inst.summary());
    }
    println!("  n. A new installation");
    let choice = ask("Which one? [n] ")?;
    let mut instance = match choice.parse::<usize>() {
        Ok(n) if n >= 1 && n <= existing.len() => existing[n - 1].clone(),
        _ => {
            for (i, l) in Loader::ALL.iter().enumerate() {
                println!("  {}. {}", i + 1, l.name());
            }
            let loader = match ask("Loader? [1] ")?.parse::<usize>() {
                Ok(n) if (1..=4).contains(&n) => Loader::ALL[n - 1],
                _ => Loader::Fabric,
            };
            let newest = loaders::game_versions(&http, loader, false)?
                .into_iter()
                .next()
                .ok_or("no game versions were found")?;
            let game = ask(&format!("Game version? [{newest}] "))?;
            let game = if game.is_empty() { newest } else { game };
            let suggested = format!("{}-{}", loader.modrinth(), game);
            let name = ask(&format!("Name? [{suggested}] "))?;
            let name = if name.is_empty() { suggested } else { name };
            let made = create(&http, o, &name, loader, Some(game))?;
            println!("Made {}.", made.summary());
            made
        }
    };
    let source = o.source()?;
    loop {
        let query = ask(&format!("Search {} for a mod (Enter when done): ", source.name()))?;
        if query.is_empty() {
            break;
        }
        let hits = source.search(&http, &query, &instance.game, instance.loader, o.limit)?;
        if hits.is_empty() {
            println!("Nothing for {} {} matches that.", instance.loader.name(), instance.game);
            continue;
        }
        for (i, h) in hits.iter().enumerate() {
            println!("  {}. {} by {}: {}", i + 1, h.title, h.author, h.description);
        }
        for n in ask("Which to add? (numbers, or Enter for none) ")?
            .split([' ', ','])
            .filter_map(|w| w.parse::<usize>().ok())
        {
            let Some(hit) = hits.get(n.wrapping_sub(1)) else { continue };
            match mods::add(&http, &source, &o.minecraft, &mut instance, &hit.slug) {
                Ok(lines) => lines.iter().for_each(|l| println!("Added {l}")),
                Err(e) => println!("{e}"),
            }
        }
    }
    println!(
        "Done. In the Minecraft Launcher, choose \"{} ({} {})\" and press Play.",
        instance.name,
        instance.loader.name(),
        instance.game
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_is_written_as_the_launcher_writes_it() {
        let t = now();
        assert_eq!(t.len(), "2026-11-03T12:00:00.000Z".len());
        assert!(t.ends_with(".000Z") && t.as_bytes()[4] == b'-' && t.as_bytes()[10] == b'T');
        assert!(t.as_str() > "2026-01-01");
    }

    #[test]
    fn options_go_anywhere_and_words_stay_in_order() {
        let o = Options::parse(
            ["add", "--minecraft", "X", "mine", "sodium", "--source", "modrinth", "lithium"].map(String::from).to_vec(),
        )
        .expect("parses");
        assert_eq!(o.words, ["add", "mine", "sodium", "lithium"]);
        assert_eq!(o.minecraft, PathBuf::from("X"));
        assert!(Options::parse(vec!["--nonsense".into()]).is_err());
    }
}
