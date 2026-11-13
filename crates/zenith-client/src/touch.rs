//! Playing with a touch screen. On the world: a stick under the left thumb to move, the rest of the
//! screen to look around, a finger resting on the world to break what is there and a quick tap to
//! place or use, and buttons to jump, sneak, open the inventory and pause. A touch on a menu is a
//! click; the app handles that itself. Positions are physical pixels, as touches arrive.
use crate::game::{Game, Overlay};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use winit::event::TouchPhase;
use zenith_content::ItemKind;

/// How long a finger rests on the world before it starts breaking what is there.
const HOLD: Duration = Duration::from_millis(300);
/// How long a tap holds "use", long enough for a tick of the game to see it.
const TAP: Duration = Duration::from_millis(120);
/// How far a resting finger may wander, as a share of the screen's height.
const STILL: f32 = 0.025;
/// Turning: how many pixels of mouse movement a pixel of finger movement is worth.
const LOOK: f64 = 0.55;
/// The stick's reach from where the thumb came down, as a share of the screen's height.
const REACH: f32 = 0.12;
/// The UI's hotbar: nine slots of this size, three units up from the bottom (see hud.rs).
const SLOT: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Jump,
    Sneak,
    Inventory,
    Pause,
}

impl Button {
    pub const ALL: [Button; 4] = [Button::Jump, Button::Sneak, Button::Inventory, Button::Pause];

    pub fn label(self) -> &'static str {
        match self {
            Button::Jump => "Jump",
            Button::Sneak => "Sneak",
            Button::Inventory => "Items",
            Button::Pause => "||",
        }
    }

    /// Centre and half-size in pixels, on a screen of `size`: jump and sneak under the right thumb,
    /// the inventory and pause in the top corners.
    pub fn place(self, size: (f32, f32)) -> ((f32, f32), f32) {
        let (w, h) = size;
        match self {
            Button::Jump => ((w - 0.14 * h, h - 0.18 * h), 0.085 * h),
            Button::Sneak => ((w - 0.36 * h, h - 0.12 * h), 0.065 * h),
            Button::Inventory => ((w - 0.1 * h, 0.1 * h), 0.06 * h),
            Button::Pause => ((0.1 * h, 0.1 * h), 0.06 * h),
        }
    }

    fn contains(self, size: (f32, f32), p: (f32, f32)) -> bool {
        let ((x, y), half) = self.place(size);
        // A little larger than drawn: thumbs are not precise.
        (p.0 - x).abs() < half * 1.25 && (p.1 - y).abs() < half * 1.25
    }
}

#[derive(Clone, Copy, Debug)]
enum Role {
    Stick { anchor: (f32, f32) },
    Look { start: (f32, f32), last: (f32, f32), at: Instant, wandered: bool, breaking: bool },
    Button(Button),
    Hotbar,
}

/// What the HUD needs to draw the controls.
#[derive(Clone, Copy, Debug, Default)]
pub struct TouchView {
    /// A touch has been seen, so the controls are drawn from now on.
    pub active: bool,
    pub size: (f32, f32),
    /// Where the thumb came down and where it is now, while the stick is held.
    pub stick: Option<((f32, f32), (f32, f32))>,
}

#[derive(Default)]
pub struct Touch {
    seen: bool,
    fingers: HashMap<u64, Role>,
    /// Where the thumb on the stick is now.
    stick_at: Option<(f32, f32)>,
    tap_until: Option<Instant>,
}

impl Touch {
    pub fn view(&self, size: (f32, f32)) -> TouchView {
        let stick = self.fingers.values().find_map(|r| match r {
            Role::Stick { anchor } => Some(*anchor),
            _ => None,
        });
        TouchView { active: self.seen, size, stick: stick.map(|a| (a, self.stick_at.unwrap_or(a))) }
    }

    /// A touch while nothing covers the world.
    pub fn on_world(
        &mut self,
        game: &mut Game,
        id: u64,
        phase: TouchPhase,
        p: (f32, f32),
        size: (f32, f32),
        ui_scale: f32,
    ) {
        self.seen = true;
        match phase {
            TouchPhase::Started => {
                let role = if let Some(b) = Button::ALL.into_iter().find(|b| b.contains(size, p)) {
                    match b {
                        Button::Jump => game.held.jump = true,
                        Button::Sneak => game.held.sneak = true,
                        Button::Inventory => game.toggle_inventory(),
                        Button::Pause => game.overlay = Overlay::Pause,
                    }
                    Role::Button(b)
                } else if let Some(slot) = hotbar_slot(size, ui_scale, p) {
                    game.select_slot(slot);
                    Role::Hotbar
                } else if p.0 < size.0 * 0.4 {
                    self.stick_at = Some(p);
                    Role::Stick { anchor: p }
                } else {
                    Role::Look { start: p, last: p, at: Instant::now(), wandered: false, breaking: false }
                };
                self.fingers.insert(id, role);
            }
            TouchPhase::Moved => match self.fingers.get_mut(&id) {
                Some(Role::Stick { anchor }) => {
                    let reach = size.1 * REACH;
                    let (x, y) = ((p.0 - anchor.0) / reach, (p.1 - anchor.1) / reach);
                    game.held.forward = y < -0.35;
                    game.held.back = y > 0.35;
                    game.held.left = x < -0.35;
                    game.held.right = x > 0.35;
                    // Pushed well past the rim: run.
                    game.held.sprint = y < -1.2;
                    self.stick_at = Some(p);
                }
                Some(Role::Look { start, last, wandered, .. }) => {
                    game.turn((p.0 - last.0) as f64 * LOOK, (p.1 - last.1) as f64 * LOOK);
                    *last = p;
                    let (dx, dy) = (p.0 - start.0, p.1 - start.1);
                    *wandered |= (dx * dx + dy * dy).sqrt() > size.1 * STILL;
                }
                _ => {}
            },
            TouchPhase::Ended | TouchPhase::Cancelled => match self.fingers.remove(&id) {
                Some(Role::Stick { .. }) => {
                    let h = &mut game.held;
                    (h.forward, h.back, h.left, h.right, h.sprint) = (false, false, false, false, false);
                    self.stick_at = None;
                }
                Some(Role::Look { at, wandered, breaking, .. }) => {
                    if breaking {
                        game.held.attack = false;
                        game.held.use_item = false;
                    } else if !wandered && at.elapsed() < HOLD && phase == TouchPhase::Ended {
                        game.held.use_item = true;
                        self.tap_until = Some(Instant::now() + TAP);
                    }
                }
                Some(Role::Button(Button::Jump)) => game.held.jump = false,
                Some(Role::Button(Button::Sneak)) => game.held.sneak = false,
                _ => {}
            },
        }
    }

    /// Once a frame: a finger resting on the world long enough starts breaking, or with a bow, a
    /// shield or food in hand, using it for as long as it rests; and a tap's use ends.
    pub fn update(&mut self, game: &mut Game) {
        let reg = game.session().reg.clone();
        let held = game.session().held().and_then(|s| reg.item(s.item)).map(|i| i.kind);
        let for_use = matches!(held, Some(ItemKind::Bow { .. } | ItemKind::Shield { .. } | ItemKind::Food { .. }));
        for role in self.fingers.values_mut() {
            if let Role::Look { at, wandered: false, breaking, .. } = role
                && !*breaking
                && at.elapsed() >= HOLD
            {
                *breaking = true;
                if for_use {
                    game.held.use_item = true;
                } else {
                    game.held.attack = true;
                }
            }
        }
        if self.tap_until.is_some_and(|t| Instant::now() >= t) {
            self.tap_until = None;
            // Not if another finger rests with a bow, a shield or food: that use goes on.
            let resting = for_use && self.fingers.values().any(|r| matches!(r, Role::Look { breaking: true, .. }));
            if !resting {
                game.held.use_item = false;
            }
        }
    }

    /// Lets go of everything, as when a screen opens over the world.
    pub fn release(&mut self, game: &mut Game) {
        if !self.fingers.is_empty() || self.tap_until.is_some() {
            self.fingers.clear();
            self.stick_at = None;
            self.tap_until = None;
            game.held = Default::default();
        }
    }
}

/// The hotbar slot under a point, if any.
fn hotbar_slot(size: (f32, f32), ui_scale: f32, p: (f32, f32)) -> Option<u8> {
    let (w, h) = (size.0 / ui_scale, size.1 / ui_scale);
    let (x, y) = (p.0 / ui_scale, p.1 / ui_scale);
    let x0 = ((w - SLOT * 9.0) / 2.0).floor();
    let y0 = h - SLOT - crate::hud::BELOW_HOTBAR;
    if y < y0 - 2.0 || y > y0 + SLOT + 2.0 || x < x0 || x >= x0 + SLOT * 9.0 {
        return None;
    }
    Some(((x - x0) / SLOT) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hotbar_is_found_where_the_hud_draws_it() {
        // 1920x1080 at UI scale 3: 640x360 units, slots from x 230 to 410, y 337 to 357.
        let size = (1920.0, 1080.0);
        assert_eq!(hotbar_slot(size, 3.0, (231.0 * 3.0, 347.0 * 3.0)), Some(0));
        assert_eq!(hotbar_slot(size, 3.0, (409.0 * 3.0, 347.0 * 3.0)), Some(8));
        assert_eq!(hotbar_slot(size, 3.0, (320.0 * 3.0, 300.0 * 3.0)), None);
        assert_eq!(hotbar_slot(size, 3.0, (100.0 * 3.0, 347.0 * 3.0)), None);
    }

    #[test]
    fn buttons_do_not_overlap_each_other_or_the_stick_side() {
        let size = (2400.0, 1080.0);
        for a in Button::ALL {
            for b in Button::ALL {
                if a != b {
                    let ((x, y), _) = a.place(size);
                    assert!(!b.contains(size, (x, y)), "{a:?} sits on {b:?}");
                }
            }
        }
        // Jump and sneak are for the right thumb.
        for b in [Button::Jump, Button::Sneak] {
            assert!(b.place(size).0.0 > size.0 * 0.5);
        }
    }
}
