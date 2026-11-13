//! Items in slots: stacking, clicking, crafting and furnaces. Pure rules over plain data.
use serde::{Deserialize, Serialize};
use zenith_content::{ItemKind, ItemStack, Registry};
use zenith_protocol::{ClickButton, slots};

pub type Slot = Option<ItemStack>;

pub const INVENTORY_SIZE: usize = slots::INVENTORY as usize;
pub const CHEST_SIZE: usize = 27;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    pub slots: Vec<Slot>,
    pub selected: u8,
    pub cursor: Slot,
    /// What is worn, from head to feet.
    #[serde(default)]
    pub armor: [Slot; 4],
}

impl Default for Inventory {
    fn default() -> Self {
        Self { slots: vec![None; INVENTORY_SIZE], selected: 0, cursor: None, armor: [None; 4] }
    }
}

/// Which armour slot an item goes in, if it is armour.
pub fn armor_slot(reg: &Registry, item: u16) -> Option<usize> {
    match reg.item(item).map(|i| i.kind) {
        Some(ItemKind::Armor { slot, .. }) => Some(slot as usize % 4),
        _ => None,
    }
}

fn max_stack(reg: &Registry, item: u16) -> u8 {
    reg.item(item).map_or(1, |i| i.max_stack.max(1))
}

impl Inventory {
    pub fn held(&self) -> Slot {
        self.slots[self.selected as usize % 9]
    }

    /// Adds as much of `stack` as fits, hotbar first. Returns what did not.
    pub fn add(&mut self, reg: &Registry, stack: ItemStack) -> Slot {
        add_to(reg, &mut self.slots, stack)
    }

    /// Takes `n` from the selected slot. Returns whether it had that many.
    pub fn consume_held(&mut self, n: u8) -> bool {
        let slot = &mut self.slots[self.selected as usize % 9];
        match slot {
            Some(s) if s.count >= n => {
                s.count -= n;
                if s.count == 0 {
                    *slot = None;
                }
                true
            }
            _ => false,
        }
    }

    /// The armour points worn, from all four slots.
    pub fn armor_points(&self, reg: &Registry) -> u8 {
        self.armor
            .iter()
            .flatten()
            .map(|s| match reg.item(s.item).map(|i| i.kind) {
                Some(ItemKind::Armor { points, .. }) => points,
                _ => 0,
            })
            .sum()
    }

    /// Wears every piece worn down by a hit. A piece breaks when used up.
    pub fn wear_armor(&mut self, reg: &Registry) {
        for slot in &mut self.armor {
            if let Some(s) = slot
                && let Some(ItemKind::Armor { durability, .. }) = reg.item(s.item).map(|i| i.kind)
            {
                s.damage += 1;
                if s.damage >= durability {
                    *slot = None;
                }
            }
        }
    }

    /// Takes one of an item from wherever in the inventory it is. Returns whether there was one.
    pub fn take(&mut self, item: u16) -> bool {
        let Some(slot) = self.slots.iter_mut().find(|s| s.is_some_and(|s| s.item == item)) else { return false };
        take_one(slot);
        true
    }

    /// Wears the held tool down. It breaks when used up.
    pub fn damage_held(&mut self, reg: &Registry) {
        let slot = &mut self.slots[self.selected as usize % 9];
        if let Some(s) = slot
            && let Some(durability) = reg.item(s.item).and_then(|i| i.kind.durability())
        {
            s.damage += 1;
            if s.damage >= durability {
                *slot = None;
            }
        }
    }
}

/// Merges into matching stacks first, then fills empty slots.
pub fn add_to(reg: &Registry, slots: &mut [Slot], mut stack: ItemStack) -> Slot {
    let cap = max_stack(reg, stack.item);
    for s in slots.iter_mut().flatten() {
        if s.stacks_with(&stack) && s.count < cap {
            let moved = (cap - s.count).min(stack.count);
            s.count += moved;
            stack.count -= moved;
            if stack.count == 0 {
                return None;
            }
        }
    }
    for s in slots.iter_mut() {
        if s.is_none() {
            let moved = stack.count.min(cap);
            *s = Some(ItemStack { count: moved, ..stack });
            stack.count -= moved;
            if stack.count == 0 {
                return None;
            }
        }
    }
    Some(stack)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Furnace {
    pub input: Slot,
    pub fuel: Slot,
    pub output: Slot,
    pub burn_left: u16,
    pub burn_total: u16,
    pub cook: u16,
}

pub const COOK_TICKS: u16 = 100;

impl Furnace {
    fn can_cook(&self, reg: &Registry) -> Option<u16> {
        let result = reg.smelt_result(self.input?.item)?;
        match self.output {
            None => Some(result),
            Some(o) if o.item == result && o.count < max_stack(reg, result) => Some(result),
            _ => None,
        }
    }

    /// One tick. Returns whether anything changed that a viewer would see.
    pub fn tick(&mut self, reg: &Registry) -> bool {
        let before = (self.burn_left, self.cook);
        let result = self.can_cook(reg);
        if self.burn_left > 0 {
            self.burn_left -= 1;
        }
        if self.burn_left == 0
            && result.is_some()
            && let Some(fuel) = self.fuel
        {
            let burn = reg.item(fuel.item).map_or(0, |i| i.burn_ticks);
            if burn > 0 {
                self.burn_left = burn;
                self.burn_total = burn;
                take_one(&mut self.fuel);
            }
        }
        match (self.burn_left > 0, result) {
            (true, Some(result)) => {
                self.cook += 1;
                if self.cook >= COOK_TICKS {
                    self.cook = 0;
                    take_one(&mut self.input);
                    match &mut self.output {
                        Some(o) => o.count += 1,
                        None => self.output = Some(ItemStack::new(result, 1)),
                    }
                }
            }
            _ => self.cook = self.cook.saturating_sub(2),
        }
        before != (self.burn_left, self.cook)
    }

    pub fn progress(&self) -> [u8; 2] {
        let cook = (self.cook as u32 * 255 / COOK_TICKS as u32) as u8;
        let fuel = if self.burn_total == 0 { 0 } else { (self.burn_left as u32 * 255 / self.burn_total as u32) as u8 };
        [cook, fuel]
    }
}

fn take_one(slot: &mut Slot) {
    if let Some(s) = slot {
        s.count -= 1;
        if s.count == 0 {
            *slot = None;
        }
    }
}

/// The parts of a screen a click can touch besides the player's own inventory.
pub enum Holder<'a> {
    /// A crafting grid of `size` x `size`, kept in the player's `craft` slots.
    Crafting {
        size: usize,
        grid: &'a mut Vec<Slot>,
    },
    Furnace(&'a mut Furnace),
    Chest(&'a mut Vec<Slot>),
}

/// What crafting makes of a grid right now.
pub fn craft_result(reg: &Registry, size: usize, grid: &[Slot]) -> Slot {
    let cells: Vec<Option<u16>> = grid.iter().take(size * size).map(|s| s.map(|i| i.item)).collect();
    reg.craft(&cells, size).map(|c| ItemStack::new(c.item, c.count))
}

fn click_slot(reg: &Registry, cursor: &mut Slot, slot: &mut Slot, button: ClickButton, take_only: bool) {
    match button {
        ClickButton::Left => match (*cursor, *slot) {
            (None, None) => {}
            (None, Some(_)) => {
                *cursor = slot.take();
            }
            (Some(_), _) if take_only => {}
            (Some(c), None) => {
                *slot = Some(c);
                *cursor = None;
            }
            (Some(c), Some(s)) if c.stacks_with(&s) => {
                let moved = (max_stack(reg, s.item) - s.count.min(max_stack(reg, s.item))).min(c.count);
                *slot = Some(ItemStack { count: s.count + moved, ..s });
                *cursor = if moved == c.count { None } else { Some(ItemStack { count: c.count - moved, ..c }) };
            }
            (Some(_), Some(_)) => std::mem::swap(cursor, slot),
        },
        ClickButton::Right => match (*cursor, *slot) {
            (None, None) => {}
            (None, Some(s)) => {
                let half = s.count.div_ceil(2);
                *cursor = Some(ItemStack { count: half, ..s });
                *slot = if s.count == half { None } else { Some(ItemStack { count: s.count - half, ..s }) };
            }
            (Some(_), _) if take_only => {}
            (Some(c), None) => {
                *slot = Some(ItemStack { count: 1, ..c });
                *cursor = if c.count == 1 { None } else { Some(ItemStack { count: c.count - 1, ..c }) };
            }
            (Some(c), Some(s)) if c.stacks_with(&s) && s.count < max_stack(reg, s.item) => {
                *slot = Some(ItemStack { count: s.count + 1, ..s });
                *cursor = if c.count == 1 { None } else { Some(ItemStack { count: c.count - 1, ..c }) };
            }
            (Some(_), Some(_)) => std::mem::swap(cursor, slot),
        },
        ClickButton::Quick => {}
    }
}

/// Handles a click on `slot`. Bad slot numbers do nothing.
pub fn click(reg: &Registry, inv: &mut Inventory, holder: &mut Holder, slot: u16, button: ClickButton) {
    if (slots::ARMOR..slots::ARMOR + 4).contains(&slot) {
        let i = (slot - slots::ARMOR) as usize;
        if button == ClickButton::Quick {
            if let Some(stack) = inv.armor[i].take() {
                inv.armor[i] = add_to(reg, &mut inv.slots, stack);
            }
            return;
        }
        // Only the piece for this slot goes in it.
        if inv.cursor.is_some_and(|c| armor_slot(reg, c.item) != Some(i)) {
            return;
        }
        click_slot(reg, &mut inv.cursor, &mut inv.armor[i], button, false);
        return;
    }
    if slot < slots::INVENTORY {
        let i = slot as usize;
        // In the player's own screen, shift-clicking a piece of armour puts it on.
        if button == ClickButton::Quick
            && matches!(holder, Holder::Crafting { size: 2, .. })
            && let Some(stack) = inv.slots[i]
            && let Some(a) = armor_slot(reg, stack.item)
            && inv.armor[a].is_none()
        {
            inv.armor[a] = inv.slots[i].take();
            return;
        }
        if button == ClickButton::Quick {
            let Some(stack) = inv.slots[i].take() else { return };
            let left = match holder {
                Holder::Chest(cells) => add_to(reg, cells, stack),
                Holder::Furnace(f) => quick_to_furnace(reg, f, stack),
                Holder::Crafting { .. } => {
                    // Between the hotbar and the rest of the inventory.
                    let (hot, main) = inv.slots.split_at_mut(9);
                    if i < 9 { add_to(reg, main, stack) } else { add_to(reg, hot, stack) }
                }
            };
            if let Some(left) = left {
                inv.slots[i] = Some(left);
            }
            return;
        }
        click_slot(reg, &mut inv.cursor, &mut inv.slots[i], button, false);
        return;
    }
    let Some(i) = slot.checked_sub(slots::SCREEN).map(usize::from) else { return };
    match holder {
        Holder::Crafting { size, grid } => {
            let size = *size;
            let cells = size * size;
            if i < cells {
                if button == ClickButton::Quick {
                    if let Some(stack) = grid[i].take() {
                        grid[i] = inv.add(reg, stack);
                    }
                } else {
                    click_slot(reg, &mut inv.cursor, &mut grid[i], button, false);
                }
            } else if i == cells {
                take_craft_result(reg, inv, size, grid, button);
            }
        }
        Holder::Furnace(f) => {
            let is_output = i == 2;
            let cell = match i {
                0 => &mut f.input,
                1 => &mut f.fuel,
                2 => &mut f.output,
                _ => return,
            };
            if button == ClickButton::Quick {
                if let Some(stack) = cell.take() {
                    *cell = inv.add(reg, stack);
                }
            } else {
                click_slot(reg, &mut inv.cursor, cell, button, is_output);
            }
        }
        Holder::Chest(cells) => {
            let Some(cell) = cells.get_mut(i) else { return };
            if button == ClickButton::Quick {
                if let Some(stack) = cell.take() {
                    *cell = inv.add(reg, stack);
                }
            } else {
                click_slot(reg, &mut inv.cursor, cell, button, false);
            }
        }
    }
}

fn quick_to_furnace(reg: &Registry, f: &mut Furnace, stack: ItemStack) -> Slot {
    let target = if reg.smelt_result(stack.item).is_some() {
        &mut f.input
    } else if reg.item(stack.item).is_some_and(|i| i.burn_ticks > 0) {
        &mut f.fuel
    } else {
        return Some(stack);
    };
    match target {
        None => {
            *target = Some(stack);
            None
        }
        Some(t) if t.stacks_with(&stack) => {
            let room = max_stack(reg, t.item).saturating_sub(t.count).min(stack.count);
            t.count += room;
            let left = stack.count - room;
            (left > 0).then_some(ItemStack { count: left, ..stack })
        }
        Some(_) => Some(stack),
    }
}

fn take_craft_result(reg: &Registry, inv: &mut Inventory, size: usize, grid: &mut [Slot], button: ClickButton) {
    let consume = |grid: &mut [Slot]| {
        for cell in grid.iter_mut().take(size * size) {
            take_one(cell);
        }
    };
    if button == ClickButton::Quick {
        // Keep crafting into the inventory while the recipe holds and there is room.
        for _ in 0..64 {
            let Some(result) = craft_result(reg, size, grid) else { return };
            let mut trial = inv.slots.clone();
            if add_to(reg, &mut trial, result).is_some() {
                return;
            }
            inv.slots = trial;
            consume(grid);
        }
        return;
    }
    let Some(result) = craft_result(reg, size, grid) else { return };
    match inv.cursor {
        None => inv.cursor = Some(result),
        Some(c) if c.stacks_with(&result) && c.count as u16 + result.count as u16 <= max_stack(reg, c.item) as u16 => {
            inv.cursor = Some(ItemStack { count: c.count + result.count, ..c });
        }
        Some(_) => return,
    }
    consume(grid);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> Registry {
        Registry::builtin()
    }
    fn id(r: &Registry, name: &str) -> u16 {
        r.item_by_name(name).unwrap_or_else(|| panic!("no {name}"))
    }
    fn s(r: &Registry, name: &str, count: u8) -> ItemStack {
        ItemStack::new(id(r, name), count)
    }
    fn total(inv: &Inventory, item: u16) -> u32 {
        inv.slots
            .iter()
            .chain(std::iter::once(&inv.cursor))
            .flatten()
            .filter(|s| s.item == item)
            .map(|s| s.count as u32)
            .sum()
    }

    #[test]
    fn adding_merges_then_fills_then_reports_what_is_left() {
        let r = reg();
        let mut inv = Inventory::default();
        assert_eq!(inv.add(&r, s(&r, "dirt", 60)), None);
        assert_eq!(inv.add(&r, s(&r, "dirt", 10)), None);
        assert_eq!(inv.slots[0].map(|x| x.count), Some(64));
        assert_eq!(inv.slots[1].map(|x| x.count), Some(6));
        let mut full = Inventory { slots: vec![Some(s(&r, "stone", 64)); INVENTORY_SIZE], ..Default::default() };
        assert_eq!(full.add(&r, s(&r, "dirt", 5)).map(|x| x.count), Some(5));
    }

    #[test]
    fn tools_never_stack() {
        let r = reg();
        let mut inv = Inventory::default();
        let pick = s(&r, "iron_pick", 1);
        inv.add(&r, pick);
        inv.add(&r, pick);
        assert_eq!(inv.slots[0], Some(pick));
        assert_eq!(inv.slots[1], Some(pick));
    }

    #[test]
    fn a_worn_tool_does_not_merge_with_a_new_one() {
        let r = reg();
        let pick = s(&r, "stone_pick", 1);
        assert!(!ItemStack { damage: 3, ..pick }.stacks_with(&pick));
    }

    #[test]
    fn a_tool_breaks_when_it_runs_out_of_durability() {
        let r = reg();
        let mut inv = Inventory::default();
        inv.slots[0] = Some(ItemStack { damage: 59, ..s(&r, "wood_pick", 1) });
        inv.damage_held(&r);
        assert_eq!(inv.slots[0], None);
    }

    fn crafting(grid: &mut Vec<Slot>) -> Holder<'_> {
        Holder::Crafting { size: 3, grid }
    }

    #[test]
    fn left_click_picks_up_puts_down_merges_and_swaps() {
        let r = reg();
        let mut inv = Inventory::default();
        let mut grid = vec![None; 9];
        let mut h = crafting(&mut grid);
        inv.slots[0] = Some(s(&r, "dirt", 10));
        inv.slots[1] = Some(s(&r, "dirt", 60));
        inv.slots[2] = Some(s(&r, "stone", 1));
        click(&r, &mut inv, &mut h, 0, ClickButton::Left);
        assert_eq!((inv.cursor.map(|c| c.count), inv.slots[0]), (Some(10), None));
        click(&r, &mut inv, &mut h, 1, ClickButton::Left);
        assert_eq!(inv.slots[1].map(|c| c.count), Some(64), "merged up to the cap");
        assert_eq!(inv.cursor.map(|c| c.count), Some(6), "the rest stays on the cursor");
        click(&r, &mut inv, &mut h, 2, ClickButton::Left);
        assert_eq!(inv.cursor.map(|c| c.item), Some(id(&r, "stone")), "different items swap");
        assert_eq!(inv.slots[2].map(|c| c.count), Some(6));
    }

    #[test]
    fn right_click_splits_and_places_one_at_a_time() {
        let r = reg();
        let mut inv = Inventory::default();
        let mut grid = vec![None; 9];
        let mut h = crafting(&mut grid);
        inv.slots[0] = Some(s(&r, "dirt", 7));
        click(&r, &mut inv, &mut h, 0, ClickButton::Right);
        assert_eq!(inv.cursor.map(|c| c.count), Some(4));
        assert_eq!(inv.slots[0].map(|c| c.count), Some(3));
        click(&r, &mut inv, &mut h, 1, ClickButton::Right);
        assert_eq!(inv.slots[1].map(|c| c.count), Some(1));
        click(&r, &mut inv, &mut h, 0, ClickButton::Right);
        assert_eq!(inv.slots[0].map(|c| c.count), Some(4));
        assert_eq!(inv.cursor.map(|c| c.count), Some(2));
    }

    #[test]
    fn crafting_consumes_one_of_each_ingredient_and_stops_when_they_run_out() {
        let r = reg();
        let mut inv = Inventory::default();
        let mut grid = vec![None; 4];
        grid[0] = Some(s(&r, "oak_log", 2));
        let mut h = Holder::Crafting { size: 2, grid: &mut grid };
        let result_slot = slots::SCREEN + 4;
        click(&r, &mut inv, &mut h, result_slot, ClickButton::Left);
        assert_eq!(inv.cursor, Some(s(&r, "oak_planks", 4)));
        click(&r, &mut inv, &mut h, result_slot, ClickButton::Left);
        assert_eq!(inv.cursor, Some(s(&r, "oak_planks", 8)));
        click(&r, &mut inv, &mut h, result_slot, ClickButton::Left);
        assert_eq!(inv.cursor, Some(s(&r, "oak_planks", 8)), "no logs left");
        assert_eq!(grid[0], None);
    }

    #[test]
    fn shift_clicking_the_result_crafts_as_many_as_it_can() {
        let r = reg();
        let mut inv = Inventory::default();
        let mut grid = vec![None; 4];
        grid[0] = Some(s(&r, "oak_log", 5));
        let mut h = Holder::Crafting { size: 2, grid: &mut grid };
        click(&r, &mut inv, &mut h, slots::SCREEN + 4, ClickButton::Quick);
        assert_eq!(total(&inv, id(&r, "oak_planks")), 20);
        assert_eq!(grid[0], None);
    }

    #[test]
    fn crafting_never_creates_or_destroys_items_on_a_full_cursor() {
        let r = reg();
        let mut inv = Inventory { cursor: Some(s(&r, "stone", 1)), ..Default::default() };
        let mut grid = vec![None; 4];
        grid[0] = Some(s(&r, "oak_log", 1));
        let mut h = Holder::Crafting { size: 2, grid: &mut grid };
        click(&r, &mut inv, &mut h, slots::SCREEN + 4, ClickButton::Left);
        assert_eq!(inv.cursor, Some(s(&r, "stone", 1)), "different item in hand: nothing happens");
        assert_eq!(grid[0].map(|x| x.count), Some(1), "and nothing was consumed");
    }

    #[test]
    fn shift_click_moves_between_chest_and_inventory_and_keeps_every_item() {
        let r = reg();
        let mut inv = Inventory::default();
        let mut chest = vec![None; CHEST_SIZE];
        inv.slots[3] = Some(s(&r, "cobble", 30));
        let mut h = Holder::Chest(&mut chest);
        click(&r, &mut inv, &mut h, 3, ClickButton::Quick);
        assert_eq!(inv.slots[3], None);
        click(&r, &mut inv, &mut h, slots::SCREEN, ClickButton::Quick);
        assert_eq!(total(&inv, id(&r, "cobble")), 30);
    }

    #[test]
    fn armour_goes_on_its_own_slot_only_and_comes_off_again() {
        let r = reg();
        let mut inv = Inventory::default();
        let mut grid = vec![None; 9];
        let mut own = Holder::Crafting { size: 2, grid: &mut grid };
        inv.slots[9] = Some(s(&r, "copper_helmet", 1));
        inv.slots[10] = Some(s(&r, "dirt", 5));
        // Shift-clicked in the player's own screen, a piece goes on.
        click(&r, &mut inv, &mut own, 9, ClickButton::Quick);
        assert_eq!(inv.armor[0].map(|a| a.item), Some(id(&r, "copper_helmet")));
        assert_eq!(inv.armor_points(&r), 2);
        // Dirt is not a helmet, nor are boots.
        click(&r, &mut inv, &mut own, 10, ClickButton::Left);
        click(&r, &mut inv, &mut own, slots::ARMOR + 3, ClickButton::Left);
        assert!(inv.armor[3].is_none());
        click(&r, &mut inv, &mut own, 10, ClickButton::Left);
        inv.cursor = Some(s(&r, "copper_boots", 1));
        click(&r, &mut inv, &mut own, slots::ARMOR, ClickButton::Left);
        assert_eq!(inv.armor[0].map(|a| a.item), Some(id(&r, "copper_helmet")), "boots are not for the head");
        click(&r, &mut inv, &mut own, slots::ARMOR + 3, ClickButton::Left);
        assert!(inv.cursor.is_none() && inv.armor[3].is_some());
        // And off again, into the inventory.
        click(&r, &mut inv, &mut own, slots::ARMOR, ClickButton::Quick);
        assert!(inv.armor[0].is_none());
        assert!(inv.slots.iter().flatten().any(|st| st.item == id(&r, "copper_helmet")));
    }

    #[test]
    fn bad_slot_numbers_are_ignored() {
        let r = reg();
        let mut inv = Inventory::default();
        let mut chest = vec![None; CHEST_SIZE];
        let mut h = Holder::Chest(&mut chest);
        for slot in [50, 99, slots::SCREEN + 27, u16::MAX] {
            click(&r, &mut inv, &mut h, slot, ClickButton::Left);
        }
        assert_eq!(inv, Inventory::default());
    }

    #[test]
    fn a_furnace_burns_fuel_to_cook_and_stops_without_it() {
        let r = reg();
        let mut f = Furnace { input: Some(s(&r, "iron_ore", 3)), fuel: Some(s(&r, "stick", 1)), ..Default::default() };
        // A stick burns for 100 ticks: exactly one item.
        for _ in 0..150 {
            f.tick(&r);
        }
        assert_eq!(f.output, Some(s(&r, "iron_ingot", 1)));
        assert_eq!(f.input.map(|x| x.count), Some(2));
        assert_eq!(f.fuel, None);
        assert_eq!(f.burn_left, 0);
    }

    #[test]
    fn a_furnace_will_not_mix_outputs() {
        let r = reg();
        let mut f = Furnace {
            input: Some(s(&r, "sand", 1)),
            fuel: Some(s(&r, "coal", 1)),
            output: Some(s(&r, "iron_ingot", 1)),
            ..Default::default()
        };
        for _ in 0..300 {
            f.tick(&r);
        }
        assert_eq!(f.input.map(|x| x.count), Some(1));
        assert_eq!(f.fuel.map(|x| x.count), Some(1), "no fuel wasted on a blocked output");
    }

    #[test]
    fn the_output_slot_only_gives_it_never_takes() {
        let r = reg();
        let mut f = Furnace { output: Some(s(&r, "iron_ingot", 1)), ..Default::default() };
        let mut inv = Inventory { cursor: Some(s(&r, "dirt", 1)), ..Default::default() };
        let mut h = Holder::Furnace(&mut f);
        click(&r, &mut inv, &mut h, slots::SCREEN + 2, ClickButton::Left);
        assert_eq!(inv.cursor, Some(s(&r, "dirt", 1)));
        inv.cursor = None;
        click(&r, &mut inv, &mut h, slots::SCREEN + 2, ClickButton::Left);
        assert_eq!(inv.cursor, Some(s(&r, "iron_ingot", 1)));
    }
}
