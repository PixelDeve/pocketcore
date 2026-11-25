//! Chest-GUI shop. A fake chest is shown above the player; every slot is a category or an item.
//! Tapping a slot buys / navigates. Anything the client moves is cancelled by re-sending the
//! window. `/shop chat` (per player) or `shop_gui = false` (server.toml) falls back to the
//! chat shop in `cmds.rs`.

use super::shop::Shop;
use super::*;
use crate::protocol::item::Slot;
use crate::protocol::play::{self, BlockRecord, CHEST_BLOCK, CHEST_SLOTS, WINDOW_SHOP};

/// Ticks between the fake chest block and the open packet (clients drop a window that opens
/// before its block entity has arrived).
const OPEN_DELAY: u64 = 5;
const BACK_SLOT: usize = CHEST_SLOTS - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Categories,
    Items(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    None,
    Category(usize),
    Buy(usize, usize),
    Back,
}

#[derive(Clone, Debug)]
pub struct Gui {
    pub pos: (i32, i32, i32),
    pub open_at: u64,
    pub opened: bool,
    pub view: View,
    pub slots: Vec<Slot>,
    pub acts: Vec<Act>,
}

/// Slots + actions for one view of the shop.
pub fn layout(shop: &Shop, view: View, team: usize) -> (Vec<Slot>, Vec<Act>) {
    let mut slots = vec![Slot::EMPTY; CHEST_SLOTS];
    let mut acts = vec![Act::None; CHEST_SLOTS];
    match view {
        View::Categories => {
            for (i, c) in shop.categories.iter().take(CHEST_SLOTS).enumerate() {
                slots[i] = Slot::new(c.icon, 1, 0);
                acts[i] = Act::Category(i);
            }
        }
        View::Items(c) => {
            if let Some(cat) = shop.categories.get(c) {
                for (i, it) in cat.items.iter().take(BACK_SLOT).enumerate() {
                    slots[i] = Slot::new(it.id, it.amount, it.resolved_meta(team));
                    acts[i] = Act::Buy(c, i);
                }
            }
            slots[BACK_SLOT] = Slot::new(262, 1, 0); // arrow = back
            acts[BACK_SLOT] = Act::Back;
        }
    }
    (slots, acts)
}

impl Bedwars {
    pub fn gui_enabled_for(&self, eid: i64) -> bool {
        self.cfg.shop_gui && !self.chat_pref.contains(&eid)
    }

    fn chest_pos(core: &Core, eid: i64) -> Option<(i32, i32, i32)> {
        let p = core.players.get(&eid)?;
        let feet = (p.y - crate::state::EYE_HEIGHT).floor() as i32;
        let y = if feet + 2 <= 120 { feet + 2 } else { (feet - 2).max(1) };
        Some((p.x.floor() as i32, y, p.z.floor() as i32))
    }

    /// Starts opening the chest shop. Falls back to the chat shop if there is nowhere to put it.
    pub fn open_gui(&mut self, core: &mut Core, eid: i64) {
        if self.guis.contains_key(&eid) {
            self.close_gui(core, eid);
        }
        let Some(pos) = Self::chest_pos(core, eid) else { return };
        let team = self.team_of(core, eid).unwrap_or(0);
        let (slots, acts) = layout(&self.shop, View::Categories, team);
        core.send(eid, play::update_block(&[BlockRecord { x: pos.0, y: pos.1, z: pos.2, id: CHEST_BLOCK, meta: 0 }]));
        core.send(eid, play::chest_block_entity(pos.0, pos.1, pos.2, &self.shop.title));
        let open_at = core.tick + OPEN_DELAY;
        self.guis.insert(eid, Gui { pos, open_at, opened: false, view: View::Categories, slots, acts });
    }

    fn send_gui(core: &mut Core, eid: i64, g: &Gui) {
        core.send(eid, play::container_set_content(WINDOW_SHOP, &g.slots, &[]));
    }

    pub fn close_gui(&mut self, core: &mut Core, eid: i64) {
        if let Some(g) = self.guis.remove(&eid) {
            if g.opened {
                core.send(eid, play::container_close(WINDOW_SHOP));
            }
            core.resync_block(eid, g.pos.0, g.pos.1, g.pos.2);
        }
    }

    /// Opens windows whose block entity has had time to arrive; closes the ones whose owner walked off.
    pub fn tick_gui(&mut self, core: &mut Core, tick: u64) {
        let ids: Vec<i64> = self.guis.keys().copied().collect();
        for eid in ids {
            if !core.players.contains_key(&eid) {
                self.guis.remove(&eid);
                continue;
            }
            let Some(g) = self.guis.get_mut(&eid) else { continue };
            if !g.opened {
                if tick >= g.open_at {
                    g.opened = true;
                    let g = g.clone();
                    core.send(eid, play::container_open(WINDOW_SHOP, 0, CHEST_SLOTS as i16, g.pos.0, g.pos.1, g.pos.2));
                    Self::send_gui(core, eid, &g);
                }
            } else if tick % 10 == 0 && !Self::near_shop(core, eid) {
                self.close_gui(core, eid);
            }
        }
    }

    /// Client sent a slot change for some window. True if the shop consumed it.
    pub fn on_container_slot(&mut self, core: &mut Core, eid: i64, window: u8, slot: i16) -> bool {
        if window != WINDOW_SHOP {
            return false;
        }
        let Some(g) = self.guis.get(&eid) else { return false };
        if !g.opened {
            return true;
        }
        let act = usize::try_from(slot).ok().and_then(|s| g.acts.get(s).copied()).unwrap_or(Act::None);
        let team = self.team_of(core, eid);
        match act {
            Act::None => {}
            Act::Category(c) => self.goto(core, eid, View::Items(c), true),
            Act::Back => self.goto(core, eid, View::Categories, false),
            Act::Buy(c, i) => {
                if let Some(item) = self.shop.categories.get(c).and_then(|cat| cat.items.get(i)).cloned() {
                    match team {
                        Some(t) => {
                            Self::purchase(core, eid, &item, t);
                        }
                        None => core.send_text(eid, text_type::POPUP, "", "\u{a7}7Preview only: purchases work during a match."),
                    }
                }
            }
        }
        // whatever the client moved, put the window and inventory back to the truth
        if let Some(g) = self.guis.get(&eid).cloned() {
            Self::send_gui(core, eid, &g);
        }
        core.send_inventory(eid);
        true
    }

    fn goto(&mut self, core: &mut Core, eid: i64, view: View, list: bool) {
        let team = self.team_of(core, eid).unwrap_or(0);
        let (slots, acts) = layout(&self.shop, view, team);
        if let Some(g) = self.guis.get_mut(&eid) {
            g.view = view;
            g.slots = slots;
            g.acts = acts;
        }
        if let (true, View::Items(c)) = (list, view) {
            // item names and prices cannot be drawn on 0.14 chest slots: show them in chat
            if let Some(cat) = self.shop.categories.get(c) {
                core.send_text(eid, text_type::RAW, "", &format!("\u{a7}6{} \u{a7}7(tap an item to buy)", cat.name));
                for it in &cat.items {
                    core.send_text(eid, text_type::RAW, "", &format!("\u{a7}7- {}", it.describe()));
                }
            }
        }
    }

    /// Client closed a window. True if it was the shop.
    pub fn on_container_close(&mut self, core: &mut Core, eid: i64, window: u8) -> bool {
        if window != WINDOW_SHOP || !self.guis.contains_key(&eid) {
            return false;
        }
        if let Some(g) = self.guis.remove(&eid) {
            core.send(eid, play::container_close(WINDOW_SHOP));
            core.resync_block(eid, g.pos.0, g.pos.1, g.pos.2);
        }
        true
    }
}
