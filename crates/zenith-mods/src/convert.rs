//! Makes a Zenith content pack out of a Minecraft mod's jar, as far as that can be done.
//!
//! A Minecraft mod is two things: data that says what it adds (which blocks and items, what they
//! are called, how they are crafted, pictures of them), and Java code that says what they do. The
//! data can be read and turned into a pack: each block that is a plain cube, a plant, a slab or
//! stairs becomes one here, coloured from its own picture; each item becomes a material, a tool or
//! food by its name; and each recipe whose ingredients all exist here comes along. The code cannot
//! be: nothing a mod's blocks do, no machines, creatures, screens or changes to how the game is
//! drawn. A mod that is all code, as many are, leaves nothing to convert, and the report says so.
//!
//! Nothing in the jar is run, and no picture is copied: only two colours are taken from each.
use crate::library::Manifest;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use zenith_content::{
    BlockDef, Faces, Icon, ItemDef, ItemKind, Layer, Pack, Pattern, RecipeDef, Registry, Shape, SmeltDef, Sound,
    Stable, Tex, Tool,
};

/// The most blocks and items one converted pack holds: the registry has room for a few thousand.
const MOST: usize = 1200;
/// The largest file read out of a jar.
const LARGEST: u64 = 8 << 20;

/// What came of converting a jar.
#[derive(Clone, Debug)]
pub struct Converted {
    pub manifest: Manifest,
    pub pack: Pack,
    pub report: Report,
}

/// What was converted and what was left behind, in numbers, with a line or two of why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub blocks: usize,
    pub items: usize,
    pub recipes: usize,
    /// Blocks of shapes that have no like here: doors, fences, machines with models of their own.
    pub blocks_left: usize,
    /// Recipes that need something that does not exist here.
    pub recipes_left: usize,
    pub notes: Vec<String>,
}

/// A zip archive's files, by name: enough of the format for jars, which are small and unencrypted.
struct Jar {
    bytes: Vec<u8>,
    /// Name to (method, compressed size, size, offset of the local header).
    files: BTreeMap<String, (u16, u64, u64, u64)>,
}

fn le(bytes: &[u8], at: usize, n: usize) -> Option<u64> {
    let s = bytes.get(at..at + n)?;
    Some(s.iter().rev().fold(0u64, |v, &b| (v << 8) | u64::from(b)))
}

impl Jar {
    fn open(bytes: Vec<u8>) -> Result<Self, String> {
        let bad = || "this is not a jar file that can be read".to_string();
        // The end record is the last thing in the file, bar a comment.
        let from = bytes.len().saturating_sub(66_000);
        let end = (from..bytes.len().saturating_sub(21))
            .rev()
            .find(|&i| bytes[i..].starts_with(b"PK\x05\x06"))
            .ok_or_else(bad)?;
        let count = le(&bytes, end + 10, 2).ok_or_else(bad)?;
        let mut at = le(&bytes, end + 16, 4).ok_or_else(bad)? as usize;
        let mut files = BTreeMap::new();
        for _ in 0..count {
            if !bytes.get(at..).is_some_and(|b| b.starts_with(b"PK\x01\x02")) {
                return Err(bad());
            }
            let field = |off: usize, n: usize| le(&bytes, at + off, n).ok_or_else(bad);
            let (method, packed, size) = (field(10, 2)? as u16, field(20, 4)?, field(24, 4)?);
            let (name_len, extra, comment, local) = (field(28, 2)?, field(30, 2)?, field(32, 2)?, field(42, 4)?);
            let name = bytes.get(at + 46..at + 46 + name_len as usize).ok_or_else(bad)?;
            if let Ok(name) = std::str::from_utf8(name) {
                files.insert(name.to_string(), (method, packed, size, local));
            }
            at += 46 + (name_len + extra + comment) as usize;
        }
        Ok(Self { bytes, files })
    }

    fn read(&self, name: &str) -> Option<Vec<u8>> {
        let &(method, packed, size, local) = self.files.get(name)?;
        if size > LARGEST {
            return None;
        }
        let local = local as usize;
        let start = local + 30 + le(&self.bytes, local + 26, 2)? as usize + le(&self.bytes, local + 28, 2)? as usize;
        let data = self.bytes.get(start..start + packed as usize)?;
        match method {
            0 => Some(data.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(size as usize);
                flate2::read::DeflateDecoder::new(data).take(LARGEST).read_to_end(&mut out).ok()?;
                Some(out)
            }
            _ => None,
        }
    }

    fn json(&self, name: &str) -> Option<Value> {
        serde_json::from_slice(&self.read(name)?).ok()
    }
}

/// `ns:path` apart, with `minecraft` where no namespace is given.
fn split(id: &str) -> (&str, &str) {
    id.trim_start_matches('#').split_once(':').unwrap_or(("minecraft", id.trim_start_matches('#')))
}

/// A name of this pack's own for something a mod calls `ns:path`.
fn own_name(ns: &str, path: &str) -> String {
    let clean = |s: &str| {
        s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect::<String>()
    };
    format!("{}_{}", clean(ns), clean(path.rsplit('/').next().unwrap_or(path)))
}

/// `iron_ingot` as `Iron Ingot`.
fn title(path: &str) -> String {
    path.split('_').filter(|w| !w.is_empty()).map(|w| w[..1].to_uppercase() + &w[1..]).collect::<Vec<_>>().join(" ")
}

/// What Zenith calls the things of the game mods are written for, where it has a like thing.
fn known_here(path: &str) -> Option<&'static str> {
    Some(match path {
        "stick" => "stick",
        "coal" | "charcoal" => "coal",
        "iron_ingot" => "iron_ingot",
        "gold_ingot" => "gold_ingot",
        "copper_ingot" => "copper_ingot",
        "diamond" | "emerald" | "amethyst_shard" | "quartz" => "crystal",
        "redstone" | "glowstone_dust" | "blaze_powder" => "ember_dust",
        "cobblestone" => "cobble",
        "stone" => "stone",
        "sand" => "sand",
        "gravel" => "gravel",
        "dirt" => "dirt",
        "clay_ball" => "clay_ball",
        "clay" => "clay",
        "brick" => "brick_item",
        "bricks" => "brick",
        "stone_bricks" => "stone_brick",
        "glass" => "glass",
        "obsidian" => "obsidian",
        "sandstone" => "sandstone",
        "snow_block" => "snow_block",
        "ice" => "ice",
        "oak_planks" | "jungle_planks" | "acacia_planks" | "dark_oak_planks" => "oak_planks",
        "birch_planks" => "birch_planks",
        "spruce_planks" => "pine_planks",
        "oak_log" | "jungle_log" | "dark_oak_log" => "oak_log",
        "birch_log" => "birch_log",
        "spruce_log" => "pine_log",
        "acacia_log" => "acacia_log",
        "string" => "sinew",
        "feather" => "feather",
        "wheat" => "grain",
        "wheat_seeds" => "seeds",
        "bread" => "bread",
        "apple" => "apple",
        "bucket" => "bucket",
        "water_bucket" => "water_bucket",
        "lava_bucket" => "lava_bucket",
        "torch" => "torch",
        "chest" => "chest",
        "furnace" => "furnace",
        "crafting_table" => "bench",
        "ladder" => "ladder",
        "tnt" => "tnt",
        "arrow" => "arrow",
        "bow" => "bow",
        "shield" => "shield",
        "fishing_rod" => "fishing_rod",
        "white_wool" => "wool_white",
        "red_wool" => "wool_red",
        "blue_wool" => "wool_blue",
        "green_wool" => "wool_green",
        "yellow_wool" => "wool_yellow",
        "black_wool" => "wool_black",
        "iron_ore" => "iron_ore",
        "gold_ore" => "gold_ore",
        "copper_ore" => "copper_ore",
        "coal_ore" => "coal_ore",
        _ => return None,
    })
}

/// A tag, as the groups Zenith's recipes know.
fn known_tag(path: &str) -> Option<&'static str> {
    Some(match path.trim_end_matches('s') {
        "plank" => "#planks",
        "log" | "logs_that_burn" => "#log",
        "wool" => "#wool",
        "ingots/iron" | "iron_ingot" => "iron_ingot",
        "ingots/gold" | "gold_ingot" => "gold_ingot",
        "ingots/copper" | "copper_ingot" => "copper_ingot",
        "rods/wooden" | "wooden_rod" | "stick" => "stick",
        "gems/diamond" | "diamond" => "crystal",
        "dusts/redstone" | "redstone_dust" => "ember_dust",
        "cobblestone" | "cobblestones/normal" => "cobble",
        "stone" => "stone",
        "sand" => "sand",
        "string" => "sinew",
        "feather" => "feather",
        "glass_block" | "glass_blocks/colorles" | "glas" => "glass",
        _ => return None,
    })
}

/// Two colours out of a picture: the mean of its lighter half and of its darker half.
fn two_colours(png: &[u8]) -> Option<([u8; 3], [u8; 3])> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let step = info.color_type.samples();
    // An animated picture is its frames one under another: the first square is enough.
    let rows = (info.height.min(info.width) as usize).max(1);
    let mut pixels: Vec<[u8; 3]> = buf[..info.line_size * rows.min(info.height as usize)]
        .chunks_exact(step)
        .filter(|p| step < 4 || p[step - 1] > 127)
        .map(|p| if step >= 3 { [p[0], p[1], p[2]] } else { [p[0]; 3] })
        .collect();
    if pixels.is_empty() {
        return None;
    }
    pixels.sort_by_key(|p| u32::from(p[0]) * 3 + u32::from(p[1]) * 6 + u32::from(p[2]));
    let mean = |ps: &[[u8; 3]]| {
        let n = ps.len().max(1) as u32;
        let sum =
            ps.iter().fold([0u32; 3], |s, p| [s[0] + u32::from(p[0]), s[1] + u32::from(p[1]), s[2] + u32::from(p[2])]);
        [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]
    };
    let half = pixels.len() / 2;
    Some((mean(&pixels[half..]), mean(&pixels[..half.max(1)])))
}

/// Every string in a JSON value, for finding the first that names a model.
fn strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        Value::Object(o) => o.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

fn seed_of(name: &str) -> u32 {
    name.bytes().fold(2166136261u32, |h, b| (h ^ u32::from(b)).wrapping_mul(16777619)) % 100_000
}

/// How a block's pictures are painted here, by what it is called.
fn pattern_for(name: &str) -> Pattern {
    let has = |w: &str| name.contains(w);
    if has("brick") || has("tile") {
        Pattern::Bricks
    } else if has("plank") || has("wood") && !has("log") {
        Pattern::Planks
    } else if has("log") || has("stem") {
        Pattern::LogSide
    } else if has("ore") {
        Pattern::Ore
    } else if has("leaves") {
        Pattern::Leaves
    } else if has("glass") {
        Pattern::Glass
    } else if has("cobble") {
        Pattern::Cobble
    } else if has("sand") {
        Pattern::Sand
    } else if has("gravel") {
        Pattern::Gravel
    } else if has("wool") || has("carpet") {
        Pattern::Wool
    } else if has("lamp") || has("lantern") || has("glow") {
        Pattern::Lamp
    } else if has("polished") || has("smooth") || has("cut_") || has("chiseled") || has("block") {
        Pattern::Blocks
    } else if has("stone") || has("rock") || has("slate") || has("granite") || has("basalt") {
        Pattern::Speckle
    } else {
        Pattern::Noise
    }
}

struct Source<'a> {
    jar: &'a Jar,
    lang: BTreeMap<String, String>,
}

impl Source<'_> {
    fn display(&self, kind: &str, ns: &str, path: &str) -> String {
        self.lang.get(&format!("{kind}.{ns}.{path}")).cloned().unwrap_or_else(|| title(path))
    }

    /// A texture's two colours, from a reference such as `ns:block/name`.
    fn colours(&self, reference: &str) -> Option<([u8; 3], [u8; 3])> {
        let (ns, path) = split(reference);
        two_colours(&self.jar.read(&format!("assets/{ns}/textures/{path}.png"))?)
    }

    /// A model's parent and textures, with its parent's textures behind its own.
    fn model(&self, reference: &str, depth: u8) -> (String, BTreeMap<String, String>) {
        let (ns, path) = split(reference);
        let Some(json) = self.jar.json(&format!("assets/{ns}/models/{path}.json")) else {
            return (reference.to_string(), BTreeMap::new());
        };
        let parent = json["parent"].as_str().unwrap_or("").to_string();
        let (mut root, mut textures) = if depth < 4 && !parent.is_empty() {
            self.model(&parent, depth + 1)
        } else {
            (parent.clone(), BTreeMap::new())
        };
        if root.is_empty() {
            root = parent;
        }
        if let Some(map) = json["textures"].as_object() {
            for (k, v) in map {
                if let Some(v) = v.as_str() {
                    textures.insert(k.clone(), v.to_string());
                }
            }
        }
        (root, textures)
    }

    fn block(&self, ns: &str, path: &str) -> Result<BlockDef, &'static str> {
        let state = self.jar.json(&format!("assets/{ns}/blockstates/{path}.json")).ok_or("no state")?;
        let mut all = Vec::new();
        strings(&state, &mut all);
        let model = all.into_iter().find(|s| s.contains("block/")).ok_or("no model")?;
        let (root, textures) = self.model(model, 0);
        // What it is shaped like, from the model it is built on and what it is called.
        let shape = if root.contains("cross") || root.contains("tinted_cross") {
            Shape::Cross
        } else if path.ends_with("_slab") || root.contains("slab") {
            Shape::Slab
        } else if path.ends_with("_stairs") || root.contains("stairs") {
            Shape::Stairs
        } else if root.contains("cube") || root.contains("leaves") || root.contains("orientable") {
            Shape::Cube
        } else {
            return Err("a shape with no like here");
        };
        let tex = |keys: &[&str]| -> Option<Tex> {
            let found = keys.iter().find_map(|k| textures.get(*k))?;
            // A texture may point at another by `#name`.
            let found = found.strip_prefix('#').and_then(|k| textures.get(k)).unwrap_or(found);
            let (a, b) = self.colours(found)?;
            Some(Tex::new(pattern_for(path), a, b, seed_of(found)))
        };
        let side = tex(&["side", "all", "cross", "texture", "wall", "particle"])
            .or_else(|| {
                self.colours(&format!("{ns}:block/{path}"))
                    .map(|(a, b)| Tex::new(pattern_for(path), a, b, seed_of(path)))
            })
            .ok_or("no picture")?;
        let top = tex(&["top", "end", "up", "all"]).unwrap_or(side);
        let bottom = tex(&["bottom", "end", "down", "all"]).unwrap_or(top);
        let log_top =
            |t: Tex| if t.pattern == Pattern::LogSide && t != side { Tex { pattern: Pattern::LogTop, ..t } } else { t };
        let plant = shape == Shape::Cross;
        let side = if plant { Tex { pattern: Pattern::Flower, ..side } } else { side };
        let (top, bottom) = if plant { (side, side) } else { (log_top(top), log_top(bottom)) };
        let has = |w: &str| path.contains(w);
        let glass = has("glass");
        let leaves = has("leaves");
        let wooden = has("plank") || has("log") || has("wood");
        Ok(BlockDef {
            name: own_name(ns, path),
            display: self.display("block", ns, path),
            shape,
            layer: if plant || glass || leaves { Layer::Cutout } else { Layer::Opaque },
            opacity: if plant || glass {
                0
            } else if leaves {
                1
            } else if shape == Shape::Cube {
                15
            } else {
                0
            },
            emission: if has("lamp") || has("lantern") || has("glow") || has("torch") { 14 } else { 0 },
            hardness: if plant {
                0.0
            } else if glass || leaves {
                0.3
            } else if wooden {
                1.2
            } else {
                1.6
            },
            tool: if plant || glass || leaves {
                Tool::Any
            } else if wooden {
                Tool::Axe
            } else if has("dirt") || has("sand") || has("gravel") || has("mud") || has("clay") {
                Tool::Shovel
            } else {
                Tool::Pick
            },
            tex: Faces { top, bottom, side, front: None },
            sound: if plant || leaves {
                Sound::Grass
            } else if glass {
                Sound::Glass
            } else if wooden {
                Sound::Wood
            } else if has("sand") || has("gravel") {
                Sound::Sand
            } else if has("wool") {
                Sound::Wool
            } else {
                Sound::Stone
            },
            replaceable: plant,
            fragile: plant,
            flammable: if leaves || plant {
                60
            } else if wooden {
                15
            } else {
                0
            },
            ..Default::default()
        })
    }

    fn item(&self, ns: &str, path: &str) -> Option<ItemDef> {
        let (_, textures) = self.model(&format!("{ns}:item/{path}"), 0);
        let colour = textures
            .get("layer0")
            .and_then(|t| self.colours(t))
            .or_else(|| self.colours(&format!("{ns}:item/{path}")))
            .map(|(a, _)| a)?;
        let ends = |w: &str| path.ends_with(w);
        let has = |w: &str| path.contains(w);
        let tool = [
            ("_pickaxe", Tool::Pick),
            ("_axe", Tool::Axe),
            ("_shovel", Tool::Shovel),
            ("_sword", Tool::Sword),
            ("_hoe", Tool::Hoe),
        ]
        .into_iter()
        .find(|(w, _)| ends(w))
        .map(|(_, t)| t);
        let food = [
            "apple", "bread", "cooked", "stew", "soup", "pie", "cookie", "berry", "berries", "cake", "jerky",
            "sandwich",
        ];
        let (kind, icon, max_stack) = if let Some(tool) = tool {
            let bonus = [1, 2, 0, 3, 0][[Tool::Pick, Tool::Axe, Tool::Shovel, Tool::Sword, Tool::Hoe]
                .iter()
                .position(|t| *t == tool)
                .unwrap_or(0)];
            (
                ItemKind::Tool { tool, tier: 4, durability: 400, damage: 5 + bonus, speed: 6 },
                Icon::Tool(tool, colour),
                1,
            )
        } else if food.iter().any(|w| has(w)) {
            (ItemKind::Food { heal: 4 }, Icon::Meat(colour), 64)
        } else if has("ingot") || has("plate") || has("bar") {
            (ItemKind::Material, Icon::Ingot(colour), 64)
        } else if has("gem") || has("crystal") || has("shard") || has("diamond") || has("jewel") {
            (ItemKind::Material, Icon::Gem(colour), 64)
        } else if has("dust") || has("powder") {
            (ItemKind::Material, Icon::Dust(colour), 64)
        } else {
            (ItemKind::Material, Icon::Lump(colour), 64)
        };
        Some(ItemDef {
            name: own_name(ns, path),
            display: self.display("item", ns, path),
            kind,
            max_stack,
            icon,
            ..Default::default()
        })
    }
}

/// What a recipe calls for, as a name here, if there is one.
fn ingredient(v: &Value, own: &BTreeSet<String>) -> Option<String> {
    let named = |id: &str| -> Option<String> {
        let (ns, path) = split(id);
        if id.starts_with('#') {
            return known_tag(path).map(str::to_string);
        }
        let mine = own_name(ns, path);
        if own.contains(&mine) {
            Some(mine)
        } else if ns == "minecraft" {
            known_here(path).map(str::to_string)
        } else {
            None
        }
    };
    match v {
        Value::String(s) => named(s),
        Value::Array(a) => a.iter().find_map(|v| ingredient(v, own)),
        Value::Object(o) => {
            if let Some(tag) = o.get("tag").and_then(Value::as_str) {
                known_tag(split(tag).1).map(str::to_string)
            } else {
                o.get("item").or_else(|| o.get("id")).and_then(Value::as_str).and_then(named)
            }
        }
        _ => None,
    }
}

fn recipe(json: &Value, own: &BTreeSet<String>) -> Option<Result<RecipeDef, SmeltDef>> {
    let kind = split(json["type"].as_str()?).1;
    let result = &json["result"];
    let out = ingredient(result, own).filter(|n| !n.starts_with('#'))?;
    let count = result["count"].as_u64().unwrap_or(1).clamp(1, 64) as u8;
    match kind {
        "crafting_shaped" => {
            let pattern: Vec<String> =
                json["pattern"].as_array()?.iter().filter_map(|r| r.as_str().map(str::to_string)).collect();
            if pattern.is_empty() || pattern.len() > 3 || pattern.iter().any(|r| r.chars().count() > 3) {
                return None;
            }
            let mut key = BTreeMap::new();
            for (k, v) in json["key"].as_object()? {
                key.insert(k.chars().next()?, ingredient(v, own)?);
            }
            // Every letter in the shape must be something.
            if pattern.iter().flat_map(|r| r.chars()).any(|c| c != ' ' && !key.contains_key(&c)) {
                return None;
            }
            Some(Ok(RecipeDef { pattern, key, inputs: Vec::new(), out, count }))
        }
        "crafting_shapeless" => {
            let list = json["ingredients"].as_array()?;
            let inputs: Option<Vec<String>> = list.iter().map(|v| ingredient(v, own)).collect();
            let inputs = inputs.filter(|i| !i.is_empty() && i.len() <= 9)?;
            Some(Ok(RecipeDef { pattern: Vec::new(), key: BTreeMap::new(), inputs, out, count }))
        }
        "smelting" => {
            let input = ingredient(&json["ingredient"], own).filter(|n| !n.starts_with('#'))?;
            Some(Err(SmeltDef { input, output: out }))
        }
        _ => None,
    }
}

/// What a jar says of itself: its id, name, version and who made it.
fn about(jar: &Jar, fallback: &str) -> (String, String, String, String) {
    if let Some(json) = jar.json("fabric.mod.json").or_else(|| jar.json("quilt.mod.json")) {
        let text = |k: &str| json[k].as_str().unwrap_or("").to_string();
        let author =
            json["authors"][0].as_str().or_else(|| json["authors"][0]["name"].as_str()).unwrap_or("").to_string();
        return (text("id"), text("name"), text("version"), author);
    }
    let toml = ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"].iter().find_map(|f| jar.read(f));
    if let Some(text) = toml.and_then(|b| String::from_utf8(b).ok()) {
        let field = |name: &str| {
            text.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim() == name)
                .map(|(_, v)| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                .unwrap_or_default()
        };
        return (field("modId"), field("displayName"), field("version"), field("authors"));
    }
    (fallback.to_string(), String::new(), String::new(), String::new())
}

/// Reads a Minecraft mod's jar and makes a content pack of what it declares.
pub fn convert_jar(path: &Path) -> Result<Converted, String> {
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    convert_bytes(bytes, &file)
}

/// The same, from the jar's bytes; `file` is what to call it in the report.
pub fn convert_bytes(bytes: Vec<u8>, file: &str) -> Result<Converted, String> {
    let jar = Jar::open(bytes)?;
    let stem = file.trim_end_matches(".jar");
    let (id, name, version, author) = about(&jar, stem);
    let clean: String = id
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .take(56)
        .collect();
    let id = format!("mc_{}", if clean.is_empty() { "mod".to_string() } else { clean });

    // What the jar has, by where it keeps it.
    let under = |top: &str, folder: &str| -> Vec<(String, String)> {
        jar.files
            .keys()
            .filter_map(|f| {
                let rest = f.strip_prefix(top)?.strip_suffix(".json")?;
                let (ns, rest) = rest.split_once('/')?;
                let path = rest.strip_prefix(folder)?;
                (ns != "minecraft" && !path.contains('/')).then(|| (ns.to_string(), path.to_string()))
            })
            .collect()
    };
    let mut lang = BTreeMap::new();
    for f in jar.files.keys().filter(|f| f.starts_with("assets/") && f.ends_with("/lang/en_us.json")) {
        if let Some(Value::Object(map)) = jar.json(f) {
            lang.extend(map.into_iter().filter_map(|(k, v)| Some((k, v.as_str()?.to_string()))));
        }
    }
    let source = Source { jar: &jar, lang };
    let mut report = Report::default();
    let mut pack = Pack { name: id.clone(), ..Default::default() };
    let mut own = BTreeSet::new();
    for (ns, path) in under("assets/", "blockstates/") {
        match source.block(&ns, &path) {
            Ok(b) if pack.blocks.len() < MOST => {
                own.insert(b.name.clone());
                pack.blocks.push(b);
            }
            _ => report.blocks_left += 1,
        }
    }
    for (ns, path) in under("assets/", "models/item/") {
        if own.contains(&own_name(&ns, &path)) || pack.items.len() >= MOST {
            continue;
        }
        if let Some(item) = source.item(&ns, &path) {
            own.insert(item.name.clone());
            pack.items.push(item);
        }
    }
    let mut seen = BTreeSet::new();
    for f in jar.files.keys().filter(|f| {
        f.starts_with("data/")
            && !f.starts_with("data/minecraft/")
            && (f.contains("/recipe/") || f.contains("/recipes/"))
    }) {
        let Some(json) = jar.json(f) else { continue };
        match recipe(&json, &own) {
            // Only what makes or uses something of the mod's own is the mod's to add.
            Some(Ok(r)) if own.contains(&r.out) || r.key.values().chain(&r.inputs).any(|n| own.contains(n)) => {
                if seen.insert(format!("{r:?}")) {
                    pack.recipes.push(r);
                }
            }
            Some(Err(s)) if own.contains(&s.output) || own.contains(&s.input) => {
                if seen.insert(format!("{s:?}")) {
                    pack.smelts.push(s);
                }
            }
            Some(_) => {}
            None => report.recipes_left += 1,
        }
    }
    // The pack has to stand up as a registry beside the game's own content.
    while let Err(e) = Registry::build(std::slice::from_ref(&pack), &Stable::default()) {
        // A recipe the registry will not have goes, and the rest stays.
        if pack.recipes.pop().is_none() && pack.smelts.pop().is_none() {
            return Err(format!("{file} converts to a pack the game will not load: {e}"));
        }
        report.recipes_left += 1;
    }
    report.blocks = pack.blocks.len();
    report.items = pack.items.len();
    report.recipes = pack.recipes.len() + pack.smelts.len();
    let classes = jar.files.keys().filter(|f| f.ends_with(".class")).count();
    if report.blocks + report.items == 0 {
        report.notes.push(format!(
            "Nothing to convert: this mod declares no blocks or items of shapes Zenith has. It is {classes} classes of \
             Java code, which only Minecraft can run."
        ));
    } else if classes > 0 {
        report.notes.push(format!(
            "What the mod's {classes} classes of Java code do did not come with it: its blocks and items are here to \
             build and craft with, and do nothing of their own."
        ));
    }
    if report.blocks_left > 0 {
        report.notes.push(format!("{} blocks have shapes with no like here, and were left out.", report.blocks_left));
    }
    let shown = if name.is_empty() { title(stem) } else { name };
    let manifest = Manifest {
        id,
        name: format!("{shown} (converted)"),
        version: if version.is_empty() || version.contains('$') { "converted".into() } else { version },
        description: format!(
            "Converted from {file}: {} blocks, {} items, {} recipes. None of the mod's code came with it.",
            report.blocks, report.items, report.recipes
        ),
        author,
        source: None,
        sha256: String::new(),
    };
    Ok(Converted { manifest, pack, report })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A jar with these files, stored without compression.
    pub(crate) fn jar_of(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data) in files {
            let offset = out.len() as u32;
            let sizes = [(data.len() as u32).to_le_bytes(), (data.len() as u32).to_le_bytes()].concat();
            let common =
                [&[0u8; 2][..], &[0; 2], &[0; 4], &[0; 4], &sizes, &(name.len() as u16).to_le_bytes(), &[0; 2]]
                    .concat();
            out.extend_from_slice(b"PK\x03\x04\x14\x00");
            out.extend_from_slice(&common);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(b"PK\x01\x02\x14\x00\x14\x00");
            central.extend_from_slice(&common);
            central.extend_from_slice(&[0; 10]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let at = out.len() as u32;
        let n = (files.len() as u16).to_le_bytes();
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06\x00\x00\x00\x00");
        out.extend_from_slice(&[n, n].concat());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&[0; 2]);
        out
    }

    /// A 4 by 4 picture, half one colour and half another.
    fn png_of(light: [u8; 3], dark: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 4, 4);
        encoder.set_color(png::ColorType::Rgb);
        let pixels: Vec<u8> = (0..16).flat_map(|i| if i < 8 { light } else { dark }).collect();
        encoder.write_header().expect("header").write_image_data(&pixels).expect("pixels");
        out
    }

    fn sample() -> Vec<u8> {
        let j = |s: &str| s.as_bytes().to_vec();
        jar_of(&[
            ("fabric.mod.json", j(r#"{"id":"gems","name":"Gems","version":"1.2","authors":["someone"]}"#)),
            ("gems/Main.class", vec![0xCA, 0xFE]),
            ("assets/gems/lang/en_us.json", j(r#"{"block.gems.ruby_block":"Block of Ruby","item.gems.ruby":"Ruby"}"#)),
            ("assets/gems/blockstates/ruby_block.json", j(r#"{"variants":{"":{"model":"gems:block/ruby_block"}}}"#)),
            (
                "assets/gems/models/block/ruby_block.json",
                j(r#"{"parent":"minecraft:block/cube_all","textures":{"all":"gems:block/ruby_block"}}"#),
            ),
            ("assets/gems/textures/block/ruby_block.png", png_of([220, 40, 60], [120, 10, 30])),
            ("assets/gems/blockstates/ruby_door.json", j(r#"{"variants":{"":{"model":"gems:block/ruby_door"}}}"#)),
            ("assets/gems/models/block/ruby_door.json", j(r#"{"parent":"minecraft:block/door_bottom_left"}"#)),
            (
                "assets/gems/models/item/ruby.json",
                j(r#"{"parent":"minecraft:item/generated","textures":{"layer0":"gems:item/ruby"}}"#),
            ),
            ("assets/gems/textures/item/ruby.png", png_of([230, 50, 70], [230, 50, 70])),
            ("assets/gems/models/item/ruby_block.json", j(r#"{"parent":"gems:block/ruby_block"}"#)),
            (
                "data/gems/recipe/ruby_block.json",
                j(
                    r#"{"type":"minecraft:crafting_shaped","pattern":["xx","xx"],"key":{"x":{"item":"gems:ruby"}},"result":{"id":"gems:ruby_block","count":1}}"#,
                ),
            ),
            (
                "data/gems/recipe/ruby_torch.json",
                j(
                    r#"{"type":"minecraft:crafting_shapeless","ingredients":[{"item":"gems:ruby"},{"tag":"c:rods/wooden"}],"result":{"id":"minecraft:torch","count":4}}"#,
                ),
            ),
            (
                "data/gems/recipe/ruby_dragon.json",
                j(
                    r#"{"type":"minecraft:crafting_shapeless","ingredients":[{"item":"minecraft:dragon_egg"}],"result":{"id":"gems:ruby"}}"#,
                ),
            ),
        ])
    }

    #[test]
    fn a_mods_blocks_items_and_recipes_become_a_pack_and_its_code_does_not() {
        let c = convert_bytes(sample(), "gems-1.2.jar").expect("converts");
        assert_eq!((c.manifest.id.as_str(), c.manifest.name.as_str()), ("mc_gems", "Gems (converted)"));
        assert_eq!((c.report.blocks, c.report.items, c.report.recipes), (1, 1, 2));
        assert_eq!((c.report.blocks_left, c.report.recipes_left), (1, 1), "a door, and a recipe out of a dragon's egg");
        let block = &c.pack.blocks[0];
        assert_eq!((block.name.as_str(), block.display.as_str()), ("gems_ruby_block", "Block of Ruby"));
        assert_eq!(block.tex.side.a, [220, 40, 60], "coloured from its own picture");
        assert_eq!(block.tex.side.b, [120, 10, 30]);
        assert_eq!(c.pack.items[0].display, "Ruby");
        assert!(c.report.notes.iter().any(|n| n.contains("Java")));
        // It loads beside the game's own content, and its recipes work there.
        let reg = Registry::build(&[c.pack], &Stable::default()).expect("a registry");
        assert!(reg.block_by_name("gems_ruby_block").is_some());
        let ruby = reg.item_by_name("gems_ruby").expect("ruby");
        let made = reg.craft(&[Some(ruby), Some(ruby), Some(ruby), Some(ruby)], 2);
        assert_eq!(made.map(|s| s.item), reg.item_by_name("gems_ruby_block"));
    }

    #[test]
    fn a_mod_that_is_all_code_leaves_nothing_and_says_so() {
        let jar = jar_of(&[
            (
                "META-INF/mods.toml",
                b"modLoader=\"javafml\"\n[[mods]]\nmodId=\"farsight\"\ndisplayName=\"Far Sight\"\nversion=\"2.0\"\n"
                    .to_vec(),
            ),
            ("com/example/Render.class", vec![0xCA, 0xFE]),
        ]);
        let c = convert_bytes(jar, "farsight.jar").expect("reads");
        assert_eq!((c.report.blocks, c.report.items, c.report.recipes), (0, 0, 0));
        assert_eq!(c.manifest.id, "mc_farsight");
        assert!(c.report.notes[0].starts_with("Nothing to convert"), "{:?}", c.report.notes);
        assert!(convert_bytes(b"not a jar".to_vec(), "x.jar").is_err());
    }
}
