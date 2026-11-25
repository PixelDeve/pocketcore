//! Luau plugin host (mlua).
//!
//! * Every `plugins/*.luau` file runs in its own environment table; top-level
//!   functions named like an event (`on_tick`, `on_player_join`, ...) become
//!   that plugin's handlers. Files are re-loaded when their mtime changes.
//! * Handlers returning `true` cancel the event; `on_player_damage` may return
//!   a number to override the damage.
//! * Each handler call has a wall-clock budget; a runaway loop is aborted
//!   with an error instead of freezing the server.
//! * Plugins get `world`, `server`, `log()`; no `io`, no `os`, no `require`.
//! * `world` addresses the original map (world 0); match worlds are managed
//!   natively by the Bedwars layer.

use std::cell::{Cell, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlua::{Function, Lua, MetaMethod, Table, UserData, UserDataMethods, Value, Variadic, VmState};

use crate::state::{Core, Hit, EYE_HEIGHT, TEMPLATE};
use crate::protocol::item::Slot;
use crate::protocol::play::text_type;
use crate::binary::Vec3;

pub const EVENTS: [&str; 12] = [
    "on_server_start",
    "on_tick",
    "on_player_join",
    "on_player_quit",
    "on_player_chat",
    "on_player_move",
    "on_block_place",
    "on_block_break",
    "on_player_damage",
    "on_entity_interact",
    "on_player_death",
    "on_player_respawn",
];

const CALL_BUDGET: Duration = Duration::from_millis(50);

pub struct ScriptConfig {
    pub enabled: bool,
    pub dir: PathBuf,
    /// Check plugin files for changes every N ticks (0 = never).
    pub reload_check_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Pass,
    Cancel,
    Damage(f64),
}

struct Plugin {
    handlers: HashMap<&'static str, Function>,
    modified: SystemTime,
}

pub struct ScriptHost {
    lua: Lua,
    core: Rc<RefCell<Core>>,
    plugins: HashMap<String, Plugin>,
    cfg: ScriptConfig,
    deadline: Rc<Cell<Option<Instant>>>,
}

// ---------------------------------------------------------------------------
// Userdata handed to scripts
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct PlayerRef {
    eid: i64,
    core: Rc<RefCell<Core>>,
}

impl PlayerRef {
    fn core(&self) -> mlua::Result<RefMut<'_, Core>> {
        self.core.try_borrow_mut().map_err(|_| mlua::Error::runtime("game state is busy"))
    }
}

struct WorldRef {
    core: Rc<RefCell<Core>>,
}

struct ServerRef {
    core: Rc<RefCell<Core>>,
}

fn busy() -> mlua::Error {
    mlua::Error::runtime("game state is busy")
}

fn clamp_u8(v: u32) -> u8 {
    v.min(255) as u8
}

impl UserData for PlayerRef {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::ToString, |_, t, ()| Ok(format!("Player({})", t.eid)));
        m.add_method("get_eid", |_, t, ()| Ok(t.eid));
        m.add_method("is_online", |_, t, ()| Ok(t.core()?.players.contains_key(&t.eid)));
        m.add_method("get_name", |_, t, ()| Ok(t.core()?.players.get(&t.eid).map(|p| p.name.clone())));
        m.add_method("get_position", |lua, t, ()| {
            let c = t.core()?;
            let Some(p) = c.players.get(&t.eid) else { return Ok(None) };
            let tbl = lua.create_table()?;
            tbl.set("x", p.x as f64)?;
            tbl.set("y", (p.y - EYE_HEIGHT) as f64)?;
            tbl.set("z", p.z as f64)?;
            Ok(Some(tbl))
        });
        m.add_method("get_health", |_, t, ()| Ok(t.core()?.players.get(&t.eid).map(|p| p.health as f64)));
        // get_world(): 0 = original map, otherwise the id of a match world
        m.add_method("get_world", |_, t, ()| Ok(t.core()?.players.get(&t.eid).map(|p| p.world)));
        m.add_method("get_gamemode", |_, t, ()| Ok(t.core()?.players.get(&t.eid).map(|p| p.gamemode)));
        m.add_method("is_dead", |_, t, ()| Ok(t.core()?.players.get(&t.eid).map_or(false, |p| p.dead)));
        m.add_method("send_message", |_, t, msg: String| {
            t.core()?.send_text(t.eid, text_type::RAW, "", &msg);
            Ok(())
        });
        m.add_method("send_popup", |_, t, msg: String| {
            t.core()?.send_text(t.eid, text_type::POPUP, "", &msg);
            Ok(())
        });
        m.add_method("send_tip", |_, t, msg: String| {
            t.core()?.send_text(t.eid, text_type::TIP, "", &msg);
            Ok(())
        });
        m.add_method("teleport", |_, t, (x, y, z): (f32, f32, f32)| {
            t.core()?.teleport(t.eid, x, y, z);
            Ok(())
        });
        m.add_method("set_gamemode", |_, t, mode: i32| {
            t.core()?.set_gamemode(t.eid, mode.clamp(0, 3));
            Ok(())
        });
        // give_item(id, count = 1, meta = 0) -> items that did not fit
        m.add_method("give_item", |_, t, (id, count, meta): (i32, Option<u32>, Option<i32>)| {
            let left = t.core()?.give_item(t.eid, id.clamp(1, 32767) as i16, clamp_u8(count.unwrap_or(1)), meta.unwrap_or(0) as i16);
            Ok(left as u32)
        });
        m.add_method("clear_inventory", |_, t, ()| {
            t.core()?.clear_inventory(t.eid);
            Ok(())
        });
        // set_armor(helmet, chestplate, leggings, boots) item ids, 0 = none
        m.add_method("set_armor", |_, t, (h, c, l, b): (i32, i32, i32, i32)| {
            t.core()?.set_armor(t.eid, [h as i16, c as i16, l as i16, b as i16]);
            Ok(())
        });
        // set_knockback(horizontal, vertical): applied when THIS player is hit
        m.add_method("set_knockback", |_, t, (h, v): (f32, f32)| {
            if let Some(p) = t.core()?.players.get_mut(&t.eid) {
                p.kb = Some((h.clamp(0.0, 3.0), v.clamp(0.0, 3.0)));
            }
            Ok(())
        });
        m.add_method("set_spawn", |_, t, (x, y, z): (f32, f32, f32)| {
            if let Some(p) = t.core()?.players.get_mut(&t.eid) {
                p.spawn_override = Some((x, y, z));
            }
            Ok(())
        });
        m.add_method("set_health", |_, t, hp: f32| {
            t.core()?.set_health(t.eid, hp);
            Ok(())
        });
        m.add_method("kick", |_, t, reason: Option<String>| {
            t.core()?.kick(t.eid, &reason.unwrap_or_else(|| "Kicked".into()));
            Ok(())
        });
    }
}

impl UserData for WorldRef {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("get_block", |_, t, (x, y, z): (i32, i32, i32)| {
            let mut c = t.core.try_borrow_mut().map_err(|_| busy())?;
            let (id, meta) = c.get_block(TEMPLATE, x, y, z);
            Ok((id as u32, meta as u32))
        });
        m.add_method("set_block", |_, t, (x, y, z, id, meta): (i32, i32, i32, u32, Option<u32>)| {
            let mut c = t.core.try_borrow_mut().map_err(|_| busy())?;
            Ok(c.set_block(TEMPLATE, x, y, z, clamp_u8(id), clamp_u8(meta.unwrap_or(0))))
        });
        // spawn_item(x, y, z, id, count = 1, meta = 0) -> entity id
        m.add_method("spawn_item", |_, t, (x, y, z, id, count, meta): (f32, f32, f32, i32, Option<u32>, Option<i32>)| {
            let mut c = t.core.try_borrow_mut().map_err(|_| busy())?;
            let slot = Slot::new(id.clamp(1, 32767) as i16, clamp_u8(count.unwrap_or(1)), meta.unwrap_or(0) as i16);
            Ok(c.spawn_item(TEMPLATE, Vec3 { x, y, z }, slot))
        });
        m.add_method("reset_arena", |_, t, ()| {
            let mut c = t.core.try_borrow_mut().map_err(|_| busy())?;
            Ok(c.reset_arena(TEMPLATE) as u32)
        });
        // arena building: disable tracking, build, then commit_baseline()
        m.add_method("set_tracking", |_, t, on: bool| {
            if let Some(w) = t.core.try_borrow_mut().map_err(|_| busy())?.worlds.get_mut(&TEMPLATE) {
                w.set_tracking(on);
            }
            Ok(())
        });
        m.add_method("commit_baseline", |_, t, ()| {
            if let Some(w) = t.core.try_borrow_mut().map_err(|_| busy())?.worlds.get_mut(&TEMPLATE) {
                w.commit_baseline();
            }
            Ok(())
        });
        m.add_method("get_spawn", |lua, t, ()| {
            let c = t.core.try_borrow().map_err(|_| busy())?;
            let tbl = lua.create_table()?;
            let sp = c.spawn_of(TEMPLATE);
            tbl.set("x", sp.0)?;
            tbl.set("y", sp.1)?;
            tbl.set("z", sp.2)?;
            Ok(tbl)
        });
    }
}

impl UserData for ServerRef {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("broadcast", |_, t, msg: String| {
            t.core.try_borrow_mut().map_err(|_| busy())?.broadcast_all_text(text_type::RAW, &msg);
            Ok(())
        });
        m.add_method("get_tick", |_, t, ()| Ok(t.core.try_borrow().map_err(|_| busy())?.tick));
        m.add_method("get_player_count", |_, t, ()| {
            Ok(t.core.try_borrow().map_err(|_| busy())?.players.len() as u32)
        });
        m.add_method("get_players", |lua, t, ()| {
            let eids: Vec<i64> = {
                let c = t.core.try_borrow().map_err(|_| busy())?;
                c.players.values().filter(|p| p.state == crate::entity::player::PlayerState::Spawned).map(|p| p.eid).collect()
            };
            let list = lua.create_table()?;
            for (i, eid) in eids.into_iter().enumerate() {
                list.set(i + 1, lua.create_userdata(PlayerRef { eid, core: Rc::clone(&t.core) })?)?;
            }
            Ok(list)
        });
        m.add_method("get_player", |_, t, name: String| {
            let c = t.core.try_borrow().map_err(|_| busy())?;
            let eid = c.players.values().find(|p| p.name.eq_ignore_ascii_case(&name)).map(|p| p.eid);
            Ok(eid.map(|eid| PlayerRef { eid, core: Rc::clone(&t.core) }))
        });
    }
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

impl ScriptHost {
    pub fn new(core: Rc<RefCell<Core>>, cfg: ScriptConfig) -> Self {
        let lua = Lua::new();
        let deadline: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        if let Err(e) = Self::setup(&lua, &core, &deadline) {
            log::error!("scripting setup failed: {e}");
        }
        Self { lua, core, plugins: HashMap::new(), cfg, deadline }
    }

    fn setup(lua: &Lua, core: &Rc<RefCell<Core>>, deadline: &Rc<Cell<Option<Instant>>>) -> mlua::Result<()> {
        let g = lua.globals();
        g.set("world", WorldRef { core: Rc::clone(core) })?;
        g.set("server", ServerRef { core: Rc::clone(core) })?;
        let logger = lua.create_function(|_, msg: String| {
            log::info!("[plugin] {msg}");
            Ok(())
        })?;
        g.set("log", logger.clone())?;
        g.set("print", logger)?;

        let dl = Rc::clone(deadline);
        lua.set_interrupt(move |_| {
            if let Some(t) = dl.get() {
                if Instant::now() > t {
                    return Err(mlua::Error::runtime("script exceeded its time budget"));
                }
            }
            Ok(VmState::Continue)
        });
        lua.sandbox(true)?;
        Ok(())
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }

    fn has(&self, ev: &str) -> bool {
        self.plugins.values().any(|p| p.handlers.contains_key(ev))
    }

    // ---- loading ----------------------------------------------------------------------

    pub fn load_all(&mut self) {
        if !self.cfg.enabled {
            return;
        }
        self.scan(false);
        log::info!("{} plugin(s) loaded from {}", self.plugins.len(), self.cfg.dir.display());
    }

    /// Hot reload: call every tick; it only touches the disk periodically.
    pub fn poll_reload(&mut self, tick: u64) {
        if !self.cfg.enabled || self.cfg.reload_check_ticks == 0 || tick % self.cfg.reload_check_ticks != 0 {
            return;
        }
        self.scan(true);
    }

    fn scan(&mut self, reloading: bool) {
        let Ok(rd) = fs::read_dir(&self.cfg.dir) else { return };
        let mut seen: HashSet<String> = HashSet::new();
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("luau") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else { continue };
            seen.insert(name.clone());
            let modified = entry.metadata().and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
            if self.plugins.get(&name).map_or(false, |p| p.modified == modified) {
                continue;
            }
            self.load_plugin(&name, &path, modified, reloading);
        }
        let gone: Vec<String> = self.plugins.keys().filter(|k| !seen.contains(*k)).cloned().collect();
        for g in gone {
            log::info!("plugin {g} unloaded");
            self.plugins.remove(&g);
        }
    }

    fn load_plugin(&mut self, name: &str, path: &std::path::Path, modified: SystemTime, reloading: bool) {
        let result = (|| -> Result<Plugin, String> {
            let src = fs::read_to_string(path).map_err(|e| e.to_string())?;
            let env = self.lua.create_table().map_err(|e| e.to_string())?;
            let mt = self.lua.create_table().map_err(|e| e.to_string())?;
            mt.set("__index", self.lua.globals()).map_err(|e| e.to_string())?;
            let _ = env.set_metatable(Some(mt));
            self.deadline.set(Some(Instant::now() + CALL_BUDGET * 10));
            let r = self.lua.load(src.as_str()).set_name(format!("={name}")).set_environment(env.clone()).exec();
            self.deadline.set(None);
            r.map_err(|e| e.to_string())?;
            Ok(Plugin { handlers: Self::collect_handlers(&env), modified })
        })();
        match result {
            Ok(plugin) => {
                log::info!("plugin {name} {} ({} handlers)", if reloading { "reloaded" } else { "loaded" }, plugin.handlers.len());
                let start = plugin.handlers.get("on_server_start").cloned();
                self.plugins.insert(name.to_owned(), plugin);
                if reloading {
                    if let Some(f) = start {
                        self.run(name, "on_server_start", &f, Variadic::new());
                    }
                }
            }
            Err(e) => {
                log::error!("plugin {name}: {e}");
                // remember the mtime so a broken file is not retried every poll
                match self.plugins.get_mut(name) {
                    Some(p) => p.modified = modified,
                    None => {
                        self.plugins.insert(name.to_owned(), Plugin { handlers: HashMap::new(), modified });
                    }
                }
            }
        }
    }

    fn collect_handlers(env: &Table) -> HashMap<&'static str, Function> {
        let mut handlers = HashMap::new();
        for ev in EVENTS {
            if let Ok(Value::Function(f)) = env.raw_get::<Value>(ev) {
                handlers.insert(ev, f);
            }
        }
        handlers
    }

    // ---- calling ------------------------------------------------------------------------

    fn run(&self, plugin: &str, ev: &str, f: &Function, args: Variadic<Value>) -> Verdict {
        self.deadline.set(Some(Instant::now() + CALL_BUDGET));
        let r = f.call::<Value>(args);
        self.deadline.set(None);
        match r {
            Ok(Value::Boolean(true)) => Verdict::Cancel,
            Ok(Value::Number(n)) => Verdict::Damage(n),
            Ok(Value::Integer(n)) => Verdict::Damage(n as f64),
            Ok(_) => Verdict::Pass,
            Err(e) => {
                log::error!("[{plugin}] {ev}: {e}");
                Verdict::Pass
            }
        }
    }

    fn call(&self, ev: &str, args: Vec<Value>) -> Verdict {
        let args: Variadic<Value> = args.into_iter().collect();
        let mut verdict = Verdict::Pass;
        for (name, p) in &self.plugins {
            let Some(f) = p.handlers.get(ev) else { continue };
            match self.run(name, ev, f, args.clone()) {
                Verdict::Cancel => return Verdict::Cancel,
                Verdict::Damage(n) => verdict = Verdict::Damage(n),
                Verdict::Pass => {}
            }
        }
        verdict
    }

    fn player(&self, eid: i64) -> Value {
        match self.lua.create_userdata(PlayerRef { eid, core: Rc::clone(&self.core) }) {
            Ok(u) => Value::UserData(u),
            Err(_) => Value::Nil,
        }
    }

    fn pos(&self, x: f32, y: f32, z: f32) -> Value {
        let Ok(t) = self.lua.create_table() else { return Value::Nil };
        let _ = t.set("x", x as f64);
        let _ = t.set("y", y as f64);
        let _ = t.set("z", z as f64);
        Value::Table(t)
    }

    // ---- events (called by the game layer; no core borrow may be held) -------------------

    pub fn server_start(&self) {
        if self.has("on_server_start") {
            self.call("on_server_start", vec![]);
        }
    }

    pub fn tick(&self, tick: u64) {
        if self.has("on_tick") {
            self.call("on_tick", vec![Value::Number(tick as f64)]);
        }
    }

    pub fn player_join(&self, eid: i64) {
        if self.has("on_player_join") {
            self.call("on_player_join", vec![self.player(eid)]);
        }
    }

    pub fn player_quit(&self, eid: i64) {
        if self.has("on_player_quit") {
            self.call("on_player_quit", vec![self.player(eid)]);
        }
    }

    /// True if a plugin handled (cancelled) the chat message / command.
    pub fn chat(&self, eid: i64, msg: &str) -> bool {
        if !self.has("on_player_chat") {
            return false;
        }
        let Ok(m) = self.lua.create_string(msg) else { return false };
        self.call("on_player_chat", vec![self.player(eid), Value::String(m)]) == Verdict::Cancel
    }

    pub fn wants_moves(&self) -> bool {
        self.has("on_player_move")
    }

    /// Feet position.
    pub fn moved(&self, eid: i64, x: f32, y: f32, z: f32) {
        self.call("on_player_move", vec![self.player(eid), self.pos(x, y, z)]);
    }

    pub fn block_place(&self, eid: i64, x: i32, y: i32, z: i32, id: u8, meta: u8) -> bool {
        if !self.has("on_block_place") {
            return false;
        }
        let pos = self.pos(x as f32, y as f32, z as f32);
        let args = vec![self.player(eid), pos, Value::Number(id as f64), Value::Number(meta as f64)];
        self.call("on_block_place", args) == Verdict::Cancel
    }

    pub fn block_break(&self, eid: i64, x: i32, y: i32, z: i32) -> bool {
        if !self.has("on_block_break") {
            return false;
        }
        let pos = self.pos(x as f32, y as f32, z as f32);
        self.call("on_block_break", vec![self.player(eid), pos]) == Verdict::Cancel
    }

    /// Returns the final damage, or `None` if cancelled.
    pub fn damage(&self, victim: i64, attacker: Option<i64>, dmg: f32) -> Option<f32> {
        if !self.has("on_player_damage") {
            return Some(dmg);
        }
        let atk = attacker.map_or(Value::Nil, |a| self.player(a));
        match self.call("on_player_damage", vec![self.player(victim), atk, Value::Number(dmg as f64)]) {
            Verdict::Cancel => None,
            Verdict::Damage(n) => Some(n as f32),
            Verdict::Pass => Some(dmg),
        }
    }

    pub fn entity_interact(&self, eid: i64, target: i64) {
        if self.has("on_entity_interact") {
            self.call("on_entity_interact", vec![self.player(eid), Value::Number(target as f64)]);
        }
    }

    pub fn player_death(&self, victim: i64, killer: Option<i64>) {
        if self.has("on_player_death") {
            let k = killer.map_or(Value::Nil, |a| self.player(a));
            self.call("on_player_death", vec![self.player(victim), k]);
        }
    }

    pub fn player_respawn(&self, eid: i64) {
        if self.has("on_player_respawn") {
            self.call("on_player_respawn", vec![self.player(eid)]);
        }
    }
}

/// Maps a core hit result for callers that only care about death.
pub fn died(h: Hit) -> bool {
    h == Hit::Died
}
