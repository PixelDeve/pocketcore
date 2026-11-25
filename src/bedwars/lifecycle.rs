//! End-to-end match lifecycle on a real `Core` (no sockets): instancing, bots after the wait,
//! start, bed rules, friendly fire, elimination, win, teardown.

use std::net::SocketAddr;

use super::map::*;
use super::*;
use crate::entity::player::Player;
use crate::protocol::Quirks;
use crate::state::{GameConfig, Pvp};
use crate::world::{Generator, World};

fn core() -> Core {
    let quirks = Quirks { chunk_order_byte: true, start_game_tail: vec![0; 16], join_extras: false, legacy_adventure_flags: true, spawn_respawn: false, adventure_permission: false, inventory_placeholders: true, enforce_break_time: false };
    let cfg = GameConfig {
        max_players: 100,
        max_view: 4,
        gamemode: 0,
        quirks,
        trace: false,
        hotbar_offset: 9,
        pvp: Pvp { knockback_h: 0.4, knockback_v: 0.4, attack_cooldown_ticks: 8, max_reach: 6.0 },
        broadcast_radius: 96.0,
    };
    Core::new(cfg, World::new(Generator::Flat, Some(6), (0, 4, 0), true))
}

fn spot(x: f32, z: f32) -> Spot {
    Spot { x, y: 5.0, z, yaw: 0.0, pitch: 0.0 }
}

fn setup_template(c: &mut Core, dir: &std::path::Path) -> Bedwars {
    let mut bw = Bedwars::new(BwConfig { dir: dir.to_path_buf(), autosave_ticks: 0, ..BwConfig::default() }, c);
    bw.map.waiting_room = Some(spot(0.5, 0.5));
    for t in 0..TEAM_COUNT {
        let x = 20 + t as i32 * 8;
        bw.map.teams[t] = Some(spot(x as f32 + 0.5, 20.5));
        bw.map.beds[t] = Some(BedSpot {
            foot: BlockXyz { x, y: 5, z: 24 },
            head: BlockXyz { x, y: 5, z: 25 },
        });
        c.set_block(TEMPLATE, x, 5, 24, BED_BLOCK as u8, 0);
        c.set_block(TEMPLATE, x, 5, 25, BED_BLOCK as u8, 8);
    }
    bw.map.spawners.push(Spawner { kind: SpawnerKind::Basic, x: 10, y: 5, z: 10 });
    bw.map.spawners.push(Spawner { kind: SpawnerKind::Advanced, x: 12, y: 5, z: 12 });
    assert!(bw.map.problems().is_empty());
    bw
}

fn login(c: &mut Core, bw: &mut Bedwars, name: &str, n: u16) -> i64 {
    let eid = c.alloc_eid();
    let (wid, s) = bw.assign_login(c);
    let addr: SocketAddr = format!("127.0.0.1:{}", 40000 + n).parse().unwrap();
    let mut p = Player::new(addr, eid, name.into(), [n as u8; 16], n as i64, false, vec![0; 8192], (s.x, s.y + 1.62, s.z));
    p.world = wid;
    p.state = crate::entity::player::PlayerState::Spawned;
    c.players.insert(eid, p);
    c.by_addr.insert(addr, eid);
    bw.on_join(c, eid);
    eid
}

fn run(c: &mut Core, bw: &mut Bedwars, from: u64, to: u64) {
    for t in from..to {
        c.tick = t;
        bw.on_tick(c, t);
    }
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pc_bw_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn full_match_lifecycle() {
    let dir = tmpdir("life");
    let mut c = core();
    let mut bw = setup_template(&mut c, &dir);

    // --- instancing: players never share the template, and a busy match never blocks joiners
    let a = login(&mut c, &mut bw, "Alice", 1);
    let w1 = c.world_of(a);
    // Start the match from an arbitrary point in the waiting world; the team
    // spawn, not the player's current position, must win at match start.
    {
        let p = c.players.get_mut(&a).unwrap();
        p.x = 6.5;
        p.y = 18.62;
        p.z = -5.5;
    }
    assert_ne!(w1, TEMPLATE);
    assert_eq!(bw.instances[&w1].phase, Phase::Waiting);
    assert_eq!(c.players[&a].gamemode, 2, "lobby = adventure");
    // inherited setup: the clone has the template's beds
    assert_eq!(c.get_block(w1, 20, 5, 24).0, BED_BLOCK as u8);

    // --- 40 s without enough humans -> bots fill every slot, then countdown, then start
    run(&mut c, &mut bw, 1, 500);
    assert_eq!(bw.instances[&w1].members.len(), 1, "no bots before the wait is over");
    run(&mut c, &mut bw, 500, 1000);
    let inst = &bw.instances[&w1];
    assert_eq!(inst.members.len(), bw.capacity());
    assert_eq!(inst.members.values().filter(|m| m.bot).count(), bw.capacity() - 1);
    for t in 0..TEAM_COUNT {
        assert_eq!(inst.team_count(t), 2, "balanced teams");
    }
    run(&mut c, &mut bw, 1000, 1200);
    assert_eq!(bw.instances[&w1].phase, Phase::Running);
    let assigned_team = bw.instances[&w1].members[&a].team;
    let assigned_spawn = bw.map.teams[assigned_team].unwrap();
    assert_eq!(
        (c.players[&a].x, c.players[&a].y, c.players[&a].z),
        (assigned_spawn.x, assigned_spawn.y + 1.62, assigned_spawn.z),
        "match start must move the player from any waiting-room position to their assigned base"
    );
    assert_eq!(c.players[&a].gamemode, 0);
    assert_eq!(c.players[&a].count_item(KIT_SWORD), 1);
    assert_eq!(bw.instances[&w1].emerald_holograms.len(), 1);
    let hologram_eid = bw.instances[&w1].emerald_holograms[0].0;
    assert!(c.npcs.get(&hologram_eid).unwrap().hologram);

    // Resource caps are independent and the emerald hologram keeps its
    // countdown text updated while the generator is running.
    bw.cfg.iron_every = 1;
    bw.cfg.gold_every = 1;
    bw.cfg.emerald_every = 1;
    bw.brains.clear(); // keep this test focused on generators, not bot combat
    for t in 1200..1250 {
        c.tick = t;
        bw.tick_running(&mut c, w1, t);
    }
    assert_eq!(c.items.values().filter(|i| i.world == w1 && i.item.id == 265).count(), 20);
    assert_eq!(c.items.values().filter(|i| i.world == w1 && i.item.id == 266).count(), 10);
    assert_eq!(c.items.values().filter(|i| i.world == w1 && i.item.id == 388).count(), 4);
    assert!(c.npcs.get(&hologram_eid).unwrap().name.contains("Next: 1s"));

    // a running match does not block a new joiner: a second clone appears
    let b = login(&mut c, &mut bw, "Bob", 2);
    let w2 = c.world_of(b);
    assert_ne!(w2, w1);
    assert_ne!(w2, TEMPLATE);
    assert_eq!(bw.instances[&w2].phase, Phase::Waiting);
    // Bob leaving cleans up his empty lobby eventually
    bw.on_leave(&mut c, b);
    c.remove_player(b);

    // --- rules inside the running match
    let my_team = bw.instances[&w1].members[&a].team;
    let enemy = (my_team + 1) % TEAM_COUNT;
    let own_bed = bw.map.beds[my_team].unwrap();
    let en_bed = bw.map.beds[enemy].unwrap();
    // own bed cannot be broken
    assert!(bw.on_block_break(&mut c, a, own_bed.foot.x, 5, own_bed.foot.z).is_some());
    // map blocks (not player-placed) cannot be broken
    assert!(bw.on_block_break(&mut c, a, 3, 3, 3).is_some());
    // player-placed blocks can
    assert!(bw.on_block_place(&c, a, 15, 6, 15, 24).is_none());
    c.set_block(w1, 15, 6, 15, 24, 0);
    bw.instances.get_mut(&w1).unwrap().placed.insert((15, 6, 15));
    assert!(bw.on_block_break(&mut c, a, 15, 6, 15).is_none());
    // void-height placement limit
    assert!(bw.on_block_place(&c, a, 15, 200, 15, 24).is_some());

    // friendly fire never lands; enemies do
    let mates: Vec<i64> = bw.instances[&w1].members.iter().filter(|(e, m)| m.team == my_team && **e != a).map(|(e, _)| *e).collect();
    assert_eq!(mates.len(), 1);
    assert!(bw.on_damage(&c, mates[0], Some(a), 5.0).is_none());
    assert!(bw.on_damage(&c, a, Some(mates[0]), 5.0).is_none());
    let foes: Vec<i64> = bw.instances[&w1].members.iter().filter(|(_, m)| m.team == enemy).map(|(e, _)| *e).collect();
    assert!(bw.on_damage(&c, foes[0], Some(a), 5.0).is_some());

    // enemy bed breaks; both halves vanish in this world only
    assert!(bw.on_block_break(&mut c, a, en_bed.head.x, 5, en_bed.head.z).is_none());
    assert!(!bw.instances[&w1].bed_alive[enemy]);
    assert_eq!(c.get_block(w1, en_bed.foot.x, 5, en_bed.foot.z).0, 0);
    assert_eq!(c.get_block(w1, en_bed.head.x, 5, en_bed.head.z).0, 0);
    assert_eq!(c.get_block(TEMPLATE, en_bed.foot.x, 5, en_bed.foot.z).0, BED_BLOCK as u8, "template untouched");
    assert_eq!(c.get_block(w2, en_bed.foot.x, 5, en_bed.foot.z).0, BED_BLOCK as u8, "other matches untouched");

    // a death with the bed intact respawns, without it eliminates for good
    let own_mate = mates[0];
    bw.on_death(&mut c, own_mate, None);
    assert!(bw.instances[&w1].members[&own_mate].alive, "bed intact -> respawns");
    bw.on_death(&mut c, foes[0], Some(a));
    assert!(!bw.instances[&w1].members[&foes[0]].alive, "bed gone -> eliminated");

    // --- win: break the remaining beds, eliminate everyone else
    for t in 0..TEAM_COUNT {
        if t == my_team || t == enemy {
            continue;
        }
        let b = bw.map.beds[t].unwrap();
        assert!(bw.on_block_break(&mut c, a, b.foot.x, 5, b.foot.z).is_none());
    }
    let others: Vec<i64> = bw.instances[&w1].members.iter().filter(|(_, m)| m.team != my_team).map(|(e, _)| *e).collect();
    for e in others {
        bw.on_death(&mut c, e, Some(a));
    }
    assert!(matches!(bw.instances[&w1].phase, Phase::Ending { .. }));
    assert_eq!(bw.instances[&w1].winner, Some(my_team));
    let st = bw.stats.get("Alice");
    assert!(st.wins == 1 && st.beds == 3, "{st:?}");

    // --- teardown: bots and the cloned world are gone, humans are back in a fresh lobby
    run(&mut c, &mut bw, 1200, 1500);
    assert!(!bw.instances.contains_key(&w1));
    assert!(!c.worlds.contains_key(&w1));
    assert!(c.players.values().all(|p| !p.is_bot), "bots removed");
    let back = c.world_of(a);
    assert_ne!(back, w1);
    assert_ne!(back, TEMPLATE);
    assert!(bw.instances.contains_key(&back) || c.players[&a].switching);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn full_lobby_starts_without_bots_and_template_stays_empty() {
    let dir = tmpdir("full");
    let mut c = core();
    let mut bw = setup_template(&mut c, &dir);
    let mut ids = Vec::new();
    for i in 0..8u16 {
        ids.push(login(&mut c, &mut bw, &format!("P{i}"), 10 + i));
    }
    let w = c.world_of(ids[0]);
    assert!(ids.iter().all(|e| c.world_of(*e) == w), "joiners fill one lobby first");
    run(&mut c, &mut bw, 1, 150);
    assert_eq!(bw.instances[&w].phase, Phase::Running);
    assert!(c.players.values().all(|p| !p.is_bot));
    // the ninth player gets a different world
    let n = login(&mut c, &mut bw, "Ninth", 99);
    assert_ne!(c.world_of(n), w);
    assert!(c.players_in(TEMPLATE).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incomplete_map_never_starts() {
    let dir = tmpdir("inc");
    let mut c = core();
    let mut bw = Bedwars::new(BwConfig { dir: dir.clone(), autosave_ticks: 0, ..BwConfig::default() }, &mut c);
    let a = login(&mut c, &mut bw, "Solo", 5);
    let w = c.world_of(a);
    run(&mut c, &mut bw, 1, 1500);
    assert_eq!(bw.instances[&w].phase, Phase::Waiting);
    assert!(c.players.values().all(|p| !p.is_bot));
    let _ = std::fs::remove_dir_all(&dir);
}

fn sent(c: &Core, id: u8) -> usize {
    c.out.iter().filter(|o| o.data.first() == Some(&id)).count()
}

#[test]
fn chest_gui_shop_flow_and_chat_fallback() {
    use crate::protocol::{ids, play::WINDOW_SHOP};
    let dir = tmpdir("gui");
    let mut c = core();
    let mut bw = setup_template(&mut c, &dir);
    let ids8: Vec<i64> = (0..8u16).map(|i| login(&mut c, &mut bw, &format!("G{i}"), 200 + i)).collect();
    let w = c.world_of(ids8[0]);
    run(&mut c, &mut bw, 1, 150);
    assert_eq!(bw.instances[&w].phase, Phase::Running);
    let a = ids8[0];
    let (px, py, pz) = { let p = &c.players[&a]; (p.x, p.y, p.z) };
    c.add_npc(w, Vec3 { x: px + 1.0, y: py - 1.62, z: pz }, 0.0, SHOP_NAME, crate::entity::skin::npc_skin());
    let team = bw.team_of(&c, a).unwrap();
    c.give_item(a, 265, 40, 0);
    c.out.clear();

    // open: fake chest + tile first, window only after the delay
    bw.open_menu(&mut c, a);
    assert!(bw.guis.contains_key(&a));
    assert_eq!(sent(&c, ids::UPDATE_BLOCK), 1);
    assert_eq!(sent(&c, ids::BLOCK_ENTITY_DATA), 1);
    assert_eq!(sent(&c, ids::CONTAINER_OPEN), 0);
    let at = c.tick;
    run(&mut c, &mut bw, at + 1, at + 8);
    assert_eq!(sent(&c, ids::CONTAINER_OPEN), 1);
    assert!(sent(&c, ids::CONTAINER_SET_CONTENT) >= 1);
    assert!(bw.guis[&a].opened);

    // tap category 0 -> its items; tap item 0 -> bought; wrong window / empty slot ignored
    assert!(!bw.on_container_slot(&mut c, a, 0, 0), "inventory window is not ours");
    assert!(bw.on_container_slot(&mut c, a, WINDOW_SHOP, 0));
    assert_eq!(bw.guis[&a].view, gui::View::Items(0));
    let item = bw.shop.categories[0].items[0].clone();
    let before = c.players[&a].count_item(item.currency.item_id());
    let owned = c.players[&a].count_item(item.id);
    assert!(bw.on_container_slot(&mut c, a, WINDOW_SHOP, 0));
    let after = c.players[&a].count_item(item.currency.item_id());
    assert_eq!(before - after, item.price, "price taken");
    assert_eq!(c.players[&a].count_item(item.id) - owned, item.amount as u32, "item delivered");
    let _ = team;
    assert!(bw.on_container_slot(&mut c, a, WINDOW_SHOP, 15), "empty slot is swallowed, nothing happens");
    assert_eq!(c.players[&a].count_item(item.currency.item_id()), after);
    // back, then close restores the block
    assert!(bw.on_container_slot(&mut c, a, WINDOW_SHOP, (crate::protocol::play::CHEST_SLOTS - 1) as i16));
    assert_eq!(bw.guis[&a].view, gui::View::Categories);
    c.out.clear();
    assert!(bw.on_container_close(&mut c, a, WINDOW_SHOP));
    assert!(!bw.guis.contains_key(&a));
    assert_eq!(sent(&c, ids::UPDATE_BLOCK), 1, "fake chest removed");

    // walking away closes it
    bw.open_menu(&mut c, a);
    let at = c.tick;
    run(&mut c, &mut bw, at + 1, at + 8);
    c.players.get_mut(&a).unwrap().x += 50.0;
    let at = c.tick;
    run(&mut c, &mut bw, at + 1, at + 25);
    assert!(!bw.guis.contains_key(&a));

    // chat fallback: /shop chat switches modes, chat numbers work
    c.players.get_mut(&a).unwrap().x -= 50.0;
    assert!(bw.command(&mut c, crate::commands::Sender::Player(a), "shop chat"));
    bw.open_menu(&mut c, a);
    assert!(!bw.guis.contains_key(&a));
    assert!(bw.on_chat_reply(&mut c, a, "1"));
    assert!(bw.command(&mut c, crate::commands::Sender::Player(a), "shop gui"));
    bw.open_menu(&mut c, a);
    assert!(bw.guis.contains_key(&a));
    // server-wide switch
    bw.cfg.shop_gui = false;
    bw.close_gui(&mut c, a);
    bw.open_menu(&mut c, a);
    assert!(!bw.guis.contains_key(&a));
    let _ = std::fs::remove_dir_all(&dir);
}
