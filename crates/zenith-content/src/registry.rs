//! The registry: everything that exists, compiled for fast lookup, and what content packs add.
use crate::block::*;
use crate::builtin::{self, id};
use crate::item::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A content pack, as read from a JSON file in a server's `packs` folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pack {
    pub name: String,
    pub blocks: Vec<BlockDef>,
    pub items: Vec<ItemDef>,
    pub recipes: Vec<RecipeDef>,
    pub smelts: Vec<SmeltDef>,
}

/// The most a registry can hold: enough for any real pack, and little enough to read from a stranger.
const MAX_ITEMS: usize = 32768;
const MAX_TILES: usize = 4096;

/// Which numbers a world has already given to pack content, so that saves stay valid when packs
/// are added, removed or reordered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stable {
    pub blocks: Vec<String>,
    pub items: Vec<String>,
}

/// One row per block kind, for the hot paths. Always 4096 long, so a kind out of a chunk from the
/// network can index these without a check.
#[derive(Clone)]
pub struct Lut {
    pub opacity: Box<[u8; 4096]>,
    pub emission: Box<[u8; 4096]>,
    /// Hides what is behind its faces, and darkens corners next to it.
    pub opaque: Box<[bool; 4096]>,
    /// Has any collision box.
    pub collides: Box<[bool; 4096]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ingredient {
    Item(u16),
    Tag(u16),
}

#[derive(Clone, Debug)]
struct Recipe {
    width: usize,
    height: usize,
    cells: Vec<Option<Ingredient>>,
    loose: Vec<Ingredient>,
    out: u16,
    count: u8,
}

/// The result of a crafting grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Crafted {
    pub item: u16,
    pub count: u8,
}

#[derive(Clone, Serialize, Deserialize)]
struct Data {
    blocks: Vec<BlockDef>,
    items: Vec<ItemDef>,
    recipes: Vec<RecipeDef>,
    smelts: Vec<SmeltDef>,
    stable: Stable,
}

pub struct Registry {
    data: Data,
    pub lut: Lut,
    block_names: HashMap<String, u16>,
    item_names: HashMap<String, u16>,
    block_items: Vec<Option<u16>>,
    item_tags: Vec<Vec<u16>>,
    recipes: Vec<Recipe>,
    smelts: HashMap<u16, u16>,
    tiles: Vec<Tex>,
    block_tiles: Vec<[u16; 6]>,
}

impl Registry {
    /// Just the game's own content.
    pub fn builtin() -> Self {
        Self::build(&[], &Stable::default()).expect("the built-in content is valid")
    }

    /// The game's content plus `packs`. Pack content keeps the numbers `stable` gives it; content
    /// new to this world gets the next free ones, and content whose pack is gone keeps its number
    /// as a visible placeholder so nothing else in the world shifts.
    pub fn build(packs: &[Pack], stable: &Stable) -> Result<Self, String> {
        let mut blocks = builtin::blocks();
        blocks.resize_with(id::FIRST_PACK as usize, || BlockDef {
            name: String::new(),
            shape: Shape::Empty,
            opacity: 0,
            has_item: false,
            drop: Some(String::new()),
            hardness: 0.0,
            replaceable: true,
            ..Default::default()
        });
        let mut pack_blocks: Vec<BlockDef> = stable
            .blocks
            .iter()
            .map(|name| BlockDef {
                name: name.clone(),
                display: format!("Missing: {name}"),
                tex: Faces::all(Tex::new(Pattern::Noise, [255, 0, 220], [20, 0, 20], 1)),
                has_item: false,
                ..Default::default()
            })
            .collect();
        let mut pack_items: Vec<ItemDef> = stable
            .items
            .iter()
            .map(|name| ItemDef { name: name.clone(), display: format!("Missing: {name}"), ..Default::default() })
            .collect();
        let mut recipes = builtin::recipes();
        let mut smelts = builtin::smelts();
        for pack in packs {
            for b in &pack.blocks {
                if b.name.is_empty() {
                    return Err(format!("pack {}: a block has no name", pack.name));
                }
                if blocks.iter().any(|o| o.name == b.name) {
                    return Err(format!("pack {}: block {} already exists", pack.name, b.name));
                }
                match pack_blocks.iter_mut().find(|o| o.name == b.name) {
                    Some(slot) => *slot = b.clone(),
                    None => pack_blocks.push(b.clone()),
                }
            }
            for i in &pack.items {
                if i.name.is_empty() {
                    return Err(format!("pack {}: an item has no name", pack.name));
                }
                match pack_items.iter_mut().find(|o| o.name == i.name) {
                    Some(slot) => *slot = i.clone(),
                    None => pack_items.push(i.clone()),
                }
            }
            recipes.extend(pack.recipes.iter().cloned());
            smelts.extend(pack.smelts.iter().cloned());
        }
        if blocks.len() + pack_blocks.len() > 4096 {
            return Err("more than 4096 block kinds".into());
        }
        let stable_out = Stable {
            blocks: pack_blocks.iter().map(|b| b.name.clone()).collect(),
            items: pack_items.iter().map(|i| i.name.clone()).collect(),
        };
        blocks.append(&mut pack_blocks);

        // Block items come first: one per block with an item, then the rest.
        let mut items: Vec<ItemDef> = Vec::new();
        for (kind, b) in blocks.iter().enumerate() {
            if b.has_item && !b.name.is_empty() {
                items.push(ItemDef {
                    name: b.name.clone(),
                    display: b.display.clone(),
                    kind: ItemKind::Block(kind as u16),
                    icon: Icon::Block,
                    tags: builtin::block_item_tags(&b.name),
                    burn_ticks: builtin::block_item_burn(&b.name),
                    ..Default::default()
                });
            }
        }
        items.extend(builtin::items());
        items.append(&mut pack_items);

        Self::compile(Data { blocks, items, recipes, smelts, stable: stable_out })
    }

    fn compile(data: Data) -> Result<Self, String> {
        let mut lut = Lut {
            opacity: Box::new([0; 4096]),
            emission: Box::new([0; 4096]),
            opaque: Box::new([false; 4096]),
            collides: Box::new([false; 4096]),
        };
        let mut block_names = HashMap::new();
        for (kind, b) in data.blocks.iter().enumerate() {
            if kind >= 4096 {
                return Err("more than 4096 block kinds".into());
            }
            lut.opacity[kind] = b.opacity;
            lut.emission[kind] = b.emission;
            lut.opaque[kind] = b.is_full_opaque();
            lut.collides[kind] = !b.shape.collision(0).is_empty();
            if !b.name.is_empty() {
                block_names.insert(b.name.clone(), kind as u16);
            }
        }

        let mut item_names = HashMap::new();
        let mut block_items = vec![None; data.blocks.len()];
        let mut tag_names: HashMap<String, u16> = HashMap::new();
        let mut item_tags = Vec::with_capacity(data.items.len());
        if data.items.len() > MAX_ITEMS {
            return Err(format!("more than {MAX_ITEMS} items"));
        }
        for (i, item) in data.items.iter().enumerate() {
            if item_names.insert(item.name.clone(), i as u16).is_some() {
                return Err(format!("item {} defined twice", item.name));
            }
            if let ItemKind::Block(kind) = item.kind {
                let slot = block_items
                    .get_mut(kind as usize)
                    .ok_or_else(|| format!("item {} places a block that does not exist", item.name))?;
                *slot = Some(i as u16);
            }
            let tags: Vec<u16> = item
                .tags
                .iter()
                .map(|t| {
                    let next = tag_names.len() as u16;
                    *tag_names.entry(t.clone()).or_insert(next)
                })
                .collect();
            item_tags.push(tags);
        }

        let find_item = |name: &str| item_names.get(name).copied().ok_or_else(|| format!("unknown item {name}"));
        let ingredient = |name: &str| -> Result<Ingredient, String> {
            match name.strip_prefix('#') {
                Some(tag) => {
                    tag_names.get(tag).map(|&t| Ingredient::Tag(t)).ok_or_else(|| format!("unknown tag {tag}"))
                }
                None => find_item(name).map(Ingredient::Item),
            }
        };
        let mut recipes = Vec::with_capacity(data.recipes.len());
        for def in &data.recipes {
            let out = find_item(&def.out)?;
            let height = def.pattern.len();
            let width = def.pattern.iter().map(|r| r.chars().count()).max().unwrap_or(0);
            if width > 3 || height > 3 {
                return Err(format!("recipe for {} is larger than 3x3", def.out));
            }
            let mut cells = Vec::with_capacity(width * height);
            for row in &def.pattern {
                let mut chars = row.chars();
                for _ in 0..width {
                    match chars.next() {
                        None | Some(' ') => cells.push(None),
                        Some(c) => {
                            let name =
                                def.key.get(&c).ok_or_else(|| format!("recipe for {}: no key for {c:?}", def.out))?;
                            cells.push(Some(ingredient(name)?));
                        }
                    }
                }
            }
            let loose = def.inputs.iter().map(|n| ingredient(n)).collect::<Result<Vec<_>, _>>()?;
            if cells.is_empty() && loose.is_empty() {
                return Err(format!("recipe for {} has no ingredients", def.out));
            }
            recipes.push(Recipe { width, height, cells, loose, out, count: def.count.max(1) });
        }
        let mut smelts = HashMap::new();
        for s in &data.smelts {
            smelts.insert(find_item(&s.input)?, find_item(&s.output)?);
        }

        // Identical descriptions share a texture layer. Each one has to be painted, so a registry
        // does not get to ask for an unlimited number.
        let distinct: std::collections::HashSet<Tex> = data
            .blocks
            .iter()
            .flat_map(|b| [b.tex.top, b.tex.bottom, b.tex.side].into_iter().chain(b.tex.front))
            .collect();
        if distinct.len() > MAX_TILES {
            return Err(format!("more than {MAX_TILES} textures"));
        }
        let mut tiles: Vec<Tex> = Vec::new();
        let mut tile_index: HashMap<Tex, u16> = HashMap::new();
        let mut layer = |t: Tex| -> u16 {
            *tile_index.entry(t).or_insert_with(|| {
                tiles.push(t);
                (tiles.len() - 1) as u16
            })
        };
        let block_tiles = data
            .blocks
            .iter()
            .map(|b| {
                let (top, bottom, side) = (layer(b.tex.top), layer(b.tex.bottom), layer(b.tex.side));
                let front = b.tex.front.map_or(side, &mut layer);
                // Indexed by `Dir`: down, up, north, south, west, east.
                [bottom, top, side, front, side, side]
            })
            .collect();

        Ok(Self { data, lut, block_names, item_names, block_items, item_tags, recipes, smelts, tiles, block_tiles })
    }

    /// Sent to clients when they join.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(&self.data).expect("registry data always serialises")
    }

    /// Reads what `to_bytes` wrote, from a server the client may not trust: bad content comes back
    /// as an error, never a panic later.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let data: Data = postcard::from_bytes(bytes).map_err(|e| format!("bad registry: {e}"))?;
        if data.blocks.is_empty() {
            return Err("bad registry: no blocks".into());
        }
        Self::compile(data)
    }

    /// The numbers this registry gave to pack content, to save with the world.
    pub fn stable(&self) -> &Stable {
        &self.data.stable
    }

    pub fn blocks(&self) -> &[BlockDef] {
        &self.data.blocks
    }
    pub fn items(&self) -> &[ItemDef] {
        &self.data.items
    }
    #[inline]
    pub fn block(&self, state: BlockState) -> &BlockDef {
        self.data.blocks.get(state.kind() as usize).unwrap_or(&self.data.blocks[0])
    }
    pub fn item(&self, item: u16) -> Option<&ItemDef> {
        self.data.items.get(item as usize)
    }
    pub fn block_by_name(&self, name: &str) -> Option<u16> {
        self.block_names.get(name).copied()
    }
    pub fn item_by_name(&self, name: &str) -> Option<u16> {
        self.item_names.get(name).copied()
    }
    /// The item that places a block kind.
    pub fn item_of_block(&self, kind: u16) -> Option<u16> {
        self.block_items.get(kind as usize).copied().flatten()
    }
    /// What a furnace turns this item into.
    pub fn smelt_result(&self, item: u16) -> Option<u16> {
        self.smelts.get(&item).copied()
    }
    /// Every texture to paint, each once.
    pub fn tiles(&self) -> &[Tex] {
        &self.tiles
    }
    /// The texture layer of one face of a block kind, indexed by `Dir`.
    #[inline]
    pub fn face_tile(&self, kind: u16, face: usize) -> u16 {
        self.block_tiles.get(kind as usize).map_or(0, |t| t[face])
    }

    fn satisfies(&self, want: Ingredient, item: u16) -> bool {
        match want {
            Ingredient::Item(i) => i == item,
            Ingredient::Tag(t) => self.item_tags.get(item as usize).is_some_and(|tags| tags.contains(&t)),
        }
    }

    /// What a crafting grid of `size` x `size` cells makes, wherever in the grid the shape sits.
    pub fn craft(&self, grid: &[Option<u16>], size: usize) -> Option<Crafted> {
        if grid.len() != size * size {
            return None;
        }
        let filled: Vec<u16> = grid.iter().flatten().copied().collect();
        if filled.is_empty() {
            return None;
        }
        // The bounding box of what is in the grid.
        let (mut x0, mut y0, mut x1, mut y1) = (size, size, 0, 0);
        for y in 0..size {
            for x in 0..size {
                if grid[y * size + x].is_some() {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
        'recipes: for r in &self.recipes {
            if !r.loose.is_empty() {
                if r.loose.len() != filled.len() {
                    continue;
                }
                // Each ingredient takes one item of the grid; none may be left over.
                let mut left = filled.clone();
                for &want in &r.loose {
                    match left.iter().position(|&i| self.satisfies(want, i)) {
                        Some(p) => {
                            left.swap_remove(p);
                        }
                        None => continue 'recipes,
                    }
                }
                return Some(Crafted { item: r.out, count: r.count });
            }
            if r.width != w || r.height != h {
                continue;
            }
            for y in 0..h {
                for x in 0..w {
                    let have = grid[(y0 + y) * size + x0 + x];
                    let want = r.cells[y * w + x];
                    let good = match (want, have) {
                        (None, None) => true,
                        (Some(want), Some(item)) => self.satisfies(want, item),
                        _ => false,
                    };
                    if !good {
                        continue 'recipes;
                    }
                }
            }
            return Some(Crafted { item: r.out, count: r.count });
        }
        None
    }

    /// Ticks to break a block with an item in hand, or `None` if it cannot be broken. The server
    /// counts these for real; the client counts them too, to draw the cracks.
    pub fn dig_ticks(&self, state: BlockState, held: Option<u16>) -> Option<f32> {
        let def = self.block(state);
        if def.hardness < 0.0 {
            return None;
        }
        let (tool, tier, speed) = self.tool_of(held);
        let right_tool = def.tool == Tool::Any || def.tool == tool;
        let harvest = def.tier == 0 || (tool == def.tool && tier >= def.tier);
        let multiplier = if right_tool { speed as f32 } else { 1.0 };
        Some((def.hardness * if harvest { 30.0 } else { 100.0 } / multiplier).max(0.0))
    }

    /// Whether breaking a block with this item yields its drop.
    pub fn can_harvest(&self, state: BlockState, held: Option<u16>) -> bool {
        let def = self.block(state);
        let (tool, tier, _) = self.tool_of(held);
        def.tier == 0 || (tool == def.tool && tier >= def.tier)
    }

    fn tool_of(&self, held: Option<u16>) -> (Tool, u8, u8) {
        match held.and_then(|h| self.item(h)).map(|i| i.kind) {
            Some(ItemKind::Tool { tool, tier, speed, .. }) => (tool, tier, speed),
            _ => (Tool::Any, 0, 1),
        }
    }

    /// The recipes that make an item, as written.
    pub fn recipes_for(&self, item: u16) -> Vec<&RecipeDef> {
        let name = self.item(item).map(|i| i.name.as_str());
        self.data.recipes.iter().filter(|r| Some(r.out.as_str()) == name).collect()
    }

    /// Whether an item fits a recipe's ingredient name, which may be a `#tag`.
    pub fn item_matches(&self, item: u16, ingredient: &str) -> bool {
        let Some(def) = self.item(item) else { return false };
        match ingredient.strip_prefix('#') {
            Some(tag) => def.tags.iter().any(|t| t == tag),
            None => def.name == ingredient,
        }
    }

    /// Reads a pack from JSON text.
    pub fn parse_pack(json: &str) -> Result<Pack, String> {
        serde_json::from_str(json).map_err(|e| format!("bad content pack: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(reg: &Registry, name: &str) -> u16 {
        reg.item_by_name(name).unwrap_or_else(|| panic!("no item {name}"))
    }

    #[test]
    fn a_registry_from_a_stranger_cannot_point_an_item_at_a_block_that_is_not_there() {
        let mut data = Registry::builtin().data.clone();
        let bad = data.blocks.len() as u16 + 5;
        data.items[0].kind = ItemKind::Block(bad);
        let bytes = postcard::to_stdvec(&data).expect("serialises");
        let err = Registry::from_bytes(&bytes).err().expect("refused");
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn a_registry_cannot_ask_for_endless_textures() {
        let mut data = Registry::builtin().data.clone();
        let template = data.blocks[1].clone();
        for i in 0..1400u32 {
            let mut b = template.clone();
            b.name = format!("many{i}");
            b.tex.top.seed = i * 3;
            b.tex.bottom.seed = i * 3 + 1;
            b.tex.side.seed = i * 3 + 2;
            data.blocks.push(b);
        }
        let bytes = postcard::to_stdvec(&data).expect("serialises");
        let err = Registry::from_bytes(&bytes).err().expect("refused");
        assert!(err.contains("textures"), "{err}");
    }

    #[test]
    fn fixed_block_numbers_match_their_names() {
        // These numbers are in saved worlds.
        let reg = Registry::builtin();
        for (kind, name) in [
            (id::STONE, "stone"),
            (id::WATER, "water"),
            (id::OAK_LOG, "oak_log"),
            (id::TORCH, "torch"),
            (id::CHEST, "chest"),
            (id::TNT, "tnt"),
        ] {
            assert_eq!(reg.blocks()[kind as usize].name, name);
            assert_eq!(reg.block_by_name(name), Some(kind));
        }
    }

    #[test]
    fn every_recipe_output_and_block_drop_exists() {
        let reg = Registry::builtin();
        for b in reg.blocks() {
            if let Some(drop) = &b.drop {
                assert!(drop.is_empty() || reg.item_by_name(drop).is_some(), "{} drops {drop}", b.name);
            } else if !b.name.is_empty() {
                assert!(reg.item_by_name(&b.name).is_some() || b.hardness < 0.0, "{} drops itself", b.name);
            }
        }
    }

    #[test]
    fn shaped_recipes_match_anywhere_in_the_grid_and_with_any_tagged_item() {
        let reg = Registry::builtin();
        let (birch, oak, stick) = (item(&reg, "birch_planks"), item(&reg, "oak_planks"), item(&reg, "stick"));
        // Top right of a 3x3 grid, in mixed woods.
        let mut grid = vec![None; 9];
        grid[1] = Some(birch);
        grid[2] = Some(oak);
        grid[4] = Some(oak);
        grid[5] = Some(birch);
        assert_eq!(reg.craft(&grid, 3), Some(Crafted { item: item(&reg, "bench"), count: 1 }));
        // A stick where a plank should be fails.
        grid[5] = Some(stick);
        assert_eq!(reg.craft(&grid, 3), None);
    }

    #[test]
    fn a_pattern_needs_its_gaps_empty() {
        let reg = Registry::builtin();
        let cobble = item(&reg, "cobble");
        let mut ring = vec![Some(cobble); 9];
        ring[4] = None;
        assert_eq!(reg.craft(&ring, 3).map(|c| c.item), Some(item(&reg, "furnace")));
        ring[4] = Some(cobble);
        assert_eq!(reg.craft(&ring, 3), None, "the middle must be empty");
    }

    #[test]
    fn loose_recipes_ignore_position_but_not_extra_items() {
        let reg = Registry::builtin();
        let (log, oak) = (item(&reg, "oak_log"), item(&reg, "oak_planks"));
        assert_eq!(reg.craft(&[None, None, Some(log), None], 2), Some(Crafted { item: oak, count: 4 }));
        assert_eq!(reg.craft(&[Some(log), None, Some(log), None], 2), None);
    }

    #[test]
    fn an_empty_grid_or_a_wrong_size_makes_nothing() {
        let reg = Registry::builtin();
        assert_eq!(reg.craft(&[None; 9], 3), None);
        assert_eq!(reg.craft(&[None; 3], 2), None);
    }

    #[test]
    fn every_tool_tier_can_be_crafted_from_its_material() {
        let reg = Registry::builtin();
        let stick = Some(item(&reg, "stick"));
        for (material, head) in
            [("wood", "oak_planks"), ("stone", "cobble"), ("iron", "iron_ingot"), ("crystal", "crystal")]
        {
            let h = Some(item(&reg, head));
            let grid = [h, h, h, None, stick, None, None, stick, None];
            assert_eq!(reg.craft(&grid, 3).map(|c| c.item), Some(item(&reg, &format!("{material}_pick"))));
        }
    }

    #[test]
    fn registry_survives_the_trip_to_a_client() {
        let reg = Registry::builtin();
        let copy = Registry::from_bytes(&reg.to_bytes()).expect("round trip");
        assert_eq!(copy.blocks(), reg.blocks());
        assert_eq!(copy.items(), reg.items());
        assert_eq!(copy.tiles(), reg.tiles());
        assert_eq!(copy.face_tile(id::GRASS, 1), reg.face_tile(id::GRASS, 1));
        assert!(Registry::from_bytes(&[1, 2, 3]).is_err());
        assert!(Registry::from_bytes(&[]).is_err());
    }

    #[test]
    fn furnace_front_faces_south_and_other_sides_do_not() {
        let reg = Registry::builtin();
        let tile = |face| reg.face_tile(id::FURNACE, face);
        assert_ne!(tile(3), tile(2));
        assert_eq!(tile(2), tile(4));
    }

    fn pack(name: &str, blocks: &[&str]) -> Pack {
        Pack {
            name: name.into(),
            blocks: blocks
                .iter()
                .map(|b| BlockDef { name: b.to_string(), display: b.to_string(), ..Default::default() })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn pack_blocks_keep_their_numbers_when_packs_are_reordered_or_removed() {
        let first =
            Registry::build(&[pack("a", &["amber"]), pack("b", &["basalt"])], &Stable::default()).expect("builds");
        let amber = first.block_by_name("amber").expect("amber");
        let basalt = first.block_by_name("basalt").expect("basalt");
        assert_eq!(amber, id::FIRST_PACK);

        // The same world, packs swapped and one new block ahead of them.
        let swapped =
            Registry::build(&[pack("n", &["new"]), pack("b", &["basalt"]), pack("a", &["amber"])], first.stable())
                .expect("builds");
        assert_eq!(swapped.block_by_name("amber"), Some(amber));
        assert_eq!(swapped.block_by_name("basalt"), Some(basalt));
        assert_eq!(swapped.block_by_name("new"), Some(basalt + 1));

        // The pack that held amber is gone: its number stays taken, by a placeholder.
        let removed = Registry::build(&[pack("b", &["basalt"])], first.stable()).expect("builds");
        assert_eq!(removed.block_by_name("basalt"), Some(basalt));
        assert_eq!(removed.block_by_name("amber"), Some(amber));
        assert!(removed.blocks()[amber as usize].display.starts_with("Missing"));
    }

    #[test]
    fn a_pack_cannot_redefine_a_game_block() {
        assert!(Registry::build(&[pack("evil", &["stone"])], &Stable::default()).is_err());
    }

    #[test]
    fn packs_load_from_json_with_defaults_for_everything_unsaid() {
        let pack = Registry::parse_pack(
            r#"{"name":"neon","blocks":[{"name":"neon","display":"Neon","emission":12,
                "tex":{"top":{"a":[255,0,128]},"bottom":{},"side":{}}}],
                "recipes":[{"inputs":["sand"],"out":"neon","count":2}]}"#,
        )
        .expect("parses");
        let reg = Registry::build(&[pack], &Stable::default()).expect("builds");
        let neon = reg.block_by_name("neon").expect("neon");
        assert_eq!(reg.lut.emission[neon as usize], 12);
        assert!(reg.lut.opaque[neon as usize]);
        let sand = reg.item_by_name("sand");
        assert_eq!(reg.craft(&[sand, None, None, None], 2).map(|c| c.count), Some(2));
    }
}
