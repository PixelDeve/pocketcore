//! Bot AI. A bot belongs to one team and guards that team's island:
//! it only ever targets members of *other* teams, never leaves a leash circle
//! around its island spawn, and walks back home when nothing is in range.

use crate::binary::Vec3;
use crate::protocol::{packets, play};
use crate::state::{Core, WorldId, EYE_HEIGHT};

use super::{Bedwars, Phase};

/// Enemies closer than this (blocks) are noticed.
pub const AGGRO: f32 = 12.0;
/// Bots never wander further than this from their island spawn.
pub const LEASH: f32 = 14.0;
/// Melee distance.
pub const MELEE: f32 = 3.0;
const CHASE_SPEED: f32 = 0.2;
const HOME_SPEED: f32 = 0.12;
const SWING_EVERY: u64 = 10;

#[derive(Clone, Debug)]
pub struct Brain {
    pub team: usize,
    /// Feet position of the island spawn.
    pub home: Vec3,
    pub target: Option<i64>,
    pub vy: f32,
    pub on_ground: bool,
    pub next_swing: u64,
    pub last_pos: Vec3,
    /// Player-placed block the bot is breaking to get through: position and the tick it started.
    pub mine: Option<((i32, i32, i32), u64)>,
}

impl Brain {
    pub fn new(team: usize) -> Self {
        Self { team, home: Vec3::default(), target: None, vy: 0.0, on_ground: true, next_swing: 0, last_pos: Vec3::default(), mine: None }
    }
}

/// Blocks a bot cannot walk through.
pub fn is_solid(id: u8) -> bool {
    !matches!(id, 0 | 6 | 8 | 9 | 10 | 11 | 26 | 27 | 28 | 30 | 31 | 32 | 37 | 38 | 39 | 40 | 50 | 51 | 55 | 59 | 63 | 64 | 65 | 66 | 68 | 69 | 70 | 71 | 72 | 75 | 76 | 77 | 78 | 83 | 106 | 141 | 142 | 175)
}

/// Half the bot's body width: bots are 0.5 wide and 1.8 tall.
const HALF: f32 = 0.25;

/// True if the bot's body with feet at height `y` and centre (x, z) overlaps a solid block.
fn body_blocked(solid: &mut dyn FnMut(i32, i32, i32) -> bool, x: f32, y: f32, z: f32) -> bool {
    let (lo, hi) = (y.floor() as i32, (y + 1.79).floor() as i32);
    for (ox, oz) in [(-HALF, -HALF), (HALF, -HALF), (-HALF, HALF), (HALF, HALF)] {
        let (cx, cz) = ((x + ox).floor() as i32, (z + oz).floor() as i32);
        for yy in lo..=hi {
            if solid(cx, yy, cz) {
                return true;
            }
        }
    }
    false
}

/// Ground within 3 blocks below the column (no stepping into voids).
fn has_ground(solid: &mut dyn FnMut(i32, i32, i32) -> bool, x: f32, y: f32, z: f32) -> bool {
    let (bx, fy, bz) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
    (1..=3).any(|k| solid(bx, fy - k, bz))
}

/// One 20 Hz physics step. `goal` is a horizontal target; `solid(x, y, z)`
/// answers block queries. Returns the new feet position.
///
/// * walks at most `speed` blocks towards the goal and never enters a solid block
///   (the whole 0.5 x 1.8 body is checked, and it slides along walls),
/// * jumps up single-block steps (rising first, so it never clips into the block),
/// * refuses to step onto a column with no ground within 3 blocks (no voids),
/// * refuses to step outside `leash` (centre, radius) unless that moves it closer.
pub fn step(
    mut p: Vec3,
    vy: &mut f32,
    on_ground: &mut bool,
    goal: Option<(f32, f32)>,
    speed: f32,
    leash: Option<(Vec3, f32)>,
    solid: &mut dyn FnMut(i32, i32, i32) -> bool,
) -> Vec3 {
    if let Some((gx, gz)) = goal {
        let (dx, dz) = (gx - p.x, gz - p.z);
        let d = (dx * dx + dz * dz).sqrt();
        if d > 0.05 {
            let s = speed.min(d);
            let (nx, nz) = (p.x + dx / d * s, p.z + dz / d * s);
            let inside = |x: f32, z: f32| leash.map_or(true, |(c, r)| (x - c.x).powi(2) + (z - c.z).powi(2) <= r * r);
            let closer = leash.map_or(true, |(c, _)| (nx - c.x).powi(2) + (nz - c.z).powi(2) < (p.x - c.x).powi(2) + (p.z - c.z).powi(2));
            if inside(nx, nz) || closer {
                if !body_blocked(solid, nx, p.y, nz) {
                    if has_ground(solid, nx, p.y, nz) {
                        p.x = nx;
                        p.z = nz;
                    }
                } else if (nx - p.x).abs() > 1e-4 && !body_blocked(solid, nx, p.y, p.z) && has_ground(solid, nx, p.y, p.z) {
                    p.x = nx; // slide along the wall (x axis)
                } else if (nz - p.z).abs() > 1e-4 && !body_blocked(solid, p.x, p.y, nz) && has_ground(solid, p.x, p.y, nz) {
                    p.z = nz; // slide along the wall (z axis)
                } else if *on_ground && !body_blocked(solid, nx, p.y + 1.0, nz) && !body_blocked(solid, p.x, p.y + 1.0, p.z) {
                    *vy = 0.5; // hop up the step (peak ~1.3 blocks); the move follows once the feet are above the block
                    *on_ground = false;
                }
            }
        }
    }
    *vy = (*vy - 0.08) * 0.98;
    let ny = p.y + *vy;
    let (cx, cz) = (p.x.floor() as i32, p.z.floor() as i32);
    if *vy <= 0.0 {
        let below = ny.floor() as i32;
        if solid(cx, below, cz) {
            p.y = below as f32 + 1.0;
            *vy = 0.0;
            *on_ground = true;
        } else {
            p.y = ny;
            *on_ground = false;
        }
    } else if solid(cx, (ny + 1.8).floor() as i32, cz) {
        *vy = 0.0; // bumped the ceiling
    } else {
        p.y = ny;
        *on_ground = false;
    }
    p
}

fn yaw_towards(from: Vec3, to: Vec3) -> f32 {
    let (dx, dz) = (to.x - from.x, to.z - from.z);
    (-dx).atan2(dz).to_degrees()
}

impl Bedwars {
    pub fn tick_bots(&mut self, core: &mut Core, wid: WorldId, tick: u64) {
        let Some(inst) = self.instances.get(&wid) else { return };
        if inst.phase != Phase::Running {
            return;
        }
        let void_y = self.cfg.void_y;
        let bots: Vec<(i64, usize)> = inst.members.iter().filter(|(_, m)| m.bot && m.alive).map(|(e, m)| (*e, m.team)).collect();
        for (eid, team) in bots {
            let Some(mut brain) = self.brains.get(&eid).cloned() else { continue };
            let Some(p) = core.players.get(&eid) else { continue };
            if p.dead {
                continue;
            }
            let feet = Vec3 { x: p.x, y: p.y - EYE_HEIGHT, z: p.z };
            if feet.y < void_y {
                if core.set_health(eid, 0.0) == crate::state::Hit::Died {
                    self.on_death(core, eid, None);
                }
                continue;
            }
            // --- target selection (re-evaluated every few ticks) ---
            let valid = |core: &Core, inst: &super::Instance, t: i64| -> bool {
                let Some(m) = inst.members.get(&t) else { return false };
                let Some(tp) = core.players.get(&t) else { return false };
                m.alive && m.team != team && !tp.dead && !tp.hidden && !tp.switching && tp.gamemode == 0 && tp.world == wid
            };
            let inst = &self.instances[&wid];
            if tick % 5 == eid as u64 % 5 || brain.target.map_or(true, |t| !valid(core, inst, t)) {
                brain.target = None;
                let mut best: Option<(f32, i64)> = None;
                for (t, _) in inst.members.iter().filter(|(_, m)| m.team != team && m.alive) {
                    if !valid(core, inst, *t) {
                        continue;
                    }
                    let tp = &core.players[t];
                    let d = ((tp.x - feet.x).powi(2) + (tp.z - feet.z).powi(2)).sqrt();
                    let dh = ((tp.x - brain.home.x).powi(2) + (tp.z - brain.home.z).powi(2)).sqrt();
                    if d <= AGGRO && dh <= LEASH + 4.0 && best.map_or(true, |(bd, _)| d < bd) {
                        best = Some((d, *t));
                    }
                }
                brain.target = best.map(|(_, t)| t);
            }
            // --- goal ---
            let target_pos = brain.target.and_then(|t| core.players.get(&t)).map(|tp| Vec3 { x: tp.x, y: tp.y - EYE_HEIGHT, z: tp.z });
            let (goal, speed) = match target_pos {
                Some(tp) => {
                    let d = ((tp.x - feet.x).powi(2) + (tp.z - feet.z).powi(2)).sqrt();
                    (if d > 1.6 { Some((tp.x, tp.z)) } else { None }, CHASE_SPEED)
                }
                None => {
                    let d = ((brain.home.x - feet.x).powi(2) + (brain.home.z - feet.z).powi(2)).sqrt();
                    (if d > 1.5 { Some((brain.home.x, brain.home.z)) } else { None }, HOME_SPEED)
                }
            };
            // --- move ---
            let leash = Some((brain.home, LEASH));
            let (mut vy, mut on_ground) = (brain.vy, brain.on_ground);
            let new = {
                let mut solid = |x: i32, y: i32, z: i32| is_solid(core.get_block(wid, x, y, z).0);
                step(feet, &mut vy, &mut on_ground, goal, speed, leash, &mut solid)
            };
            brain.vy = vy;
            brain.on_ground = on_ground;
            // --- blocked by something the players built: break through it, then carry on ---
            let stuck = goal.is_some() && on_ground && (new.x - feet.x).abs() < 1e-4 && (new.z - feet.z).abs() < 1e-4;
            if let (true, Some((gx, gz))) = (stuck, goal) {
                let (dx, dz) = (gx - new.x, gz - new.z);
                let d = (dx * dx + dz * dz).sqrt().max(1e-3);
                let (px, pz) = (new.x + dx / d * 0.6, new.z + dz / d * 0.6);
                let (cx, cz, fy) = (px.floor() as i32, pz.floor() as i32, new.y.floor() as i32);
                let cell = [fy, fy + 1].into_iter().map(|y| (cx, y, cz)).find(|&(x, y, z)| is_solid(core.get_block(wid, x, y, z).0));
                let placed = cell.filter(|c| self.instances.get(&wid).map_or(false, |i| i.placed.contains(c)));
                match placed {
                    Some((x, y, z)) => {
                        let (id, meta) = core.get_block(wid, x, y, z);
                        let need = crate::blocks::break_ticks(id, 0).unwrap_or(20).clamp(10, 60) as u64;
                        let started = match brain.mine {
                            Some((c, t0)) if c == (x, y, z) => t0,
                            _ => {
                                brain.mine = Some(((x, y, z), tick));
                                tick
                            }
                        };
                        if tick % 6 == 0 {
                            core.broadcast(wid, &play::animate(1, eid));
                        }
                        if tick.saturating_sub(started) >= need {
                            if core.set_block(wid, x, y, z, 0, 0) {
                                let centre = Vec3 { x: x as f32 + 0.5, y: y as f32 + 0.5, z: z as f32 + 0.5 };
                                core.broadcast(wid, &play::level_event(play::EVENT_PARTICLE_DESTROY, centre, id as i32 | ((meta as i32) << 8)));
                            }
                            if let Some(inst) = self.instances.get_mut(&wid) {
                                inst.placed.remove(&(x, y, z));
                            }
                            brain.mine = None;
                        }
                    }
                    None => brain.mine = None,
                }
            } else {
                brain.mine = None;
            }
            // --- look: the head follows the nearest enemy (the target if there is one) ---
            let watch: Option<Vec3> = target_pos.or_else(|| {
                let inst = self.instances.get(&wid)?;
                let mut best: Option<(f32, Vec3)> = None;
                for (t, m) in inst.members.iter() {
                    if m.team == team || !m.alive {
                        continue;
                    }
                    let Some(tp) = core.players.get(t) else { continue };
                    if tp.dead || tp.hidden || tp.world != wid {
                        continue;
                    }
                    let d = ((tp.x - new.x).powi(2) + (tp.z - new.z).powi(2)).sqrt();
                    if d <= 16.0 && best.map_or(true, |(bd, _)| d < bd) {
                        best = Some((d, Vec3 { x: tp.x, y: tp.y - EYE_HEIGHT, z: tp.z }));
                    }
                }
                best.map(|(_, v)| v)
            });
            let (yaw, pitch) = match watch {
                Some(w) => {
                    let hd = ((w.x - new.x).powi(2) + (w.z - new.z).powi(2)).sqrt();
                    (yaw_towards(new, w), (-(w.y - new.y)).atan2(hd.max(0.01)).to_degrees().clamp(-60.0, 60.0))
                }
                None => match goal {
                    Some((gx, gz)) => (yaw_towards(new, Vec3 { x: gx, y: new.y, z: gz }), 0.0),
                    None => (core.players[&eid].yaw, 0.0),
                },
            };
            let moved = (new.x - feet.x).abs() > 1e-4 || (new.y - feet.y).abs() > 1e-4 || (new.z - feet.z).abs() > 1e-4;
            let turned = (yaw - core.players[&eid].yaw).abs() > 1.0 || (pitch - core.players[&eid].pitch).abs() > 1.0;
            if moved || turned {
                if let Some(p) = core.players.get_mut(&eid) {
                    p.x = new.x;
                    p.y = new.y + EYE_HEIGHT;
                    p.z = new.z;
                    p.yaw = yaw;
                    p.pitch = pitch;
                }
                let pos = Vec3 { x: new.x, y: new.y + EYE_HEIGHT, z: new.z };
                let pkt = packets::move_player(eid, pos, yaw, yaw, pitch, packets::MOVE_NORMAL, on_ground);
                core.broadcast(wid, &pkt);
            }
            brain.last_pos = new;
            // --- attack ---
            if let (Some(t), Some(tp)) = (brain.target, target_pos) {
                let d = ((tp.x - new.x).powi(2) + (tp.y - new.y).powi(2) + (tp.z - new.z).powi(2)).sqrt();
                if d <= MELEE && tick >= brain.next_swing {
                    brain.next_swing = tick + SWING_EVERY;
                    core.broadcast(wid, &play::animate(1, eid));
                    self.attack(core, eid, t);
                }
            }
            self.brains.insert(eid, brain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat floor: solid for y <= 3, optional extra blocks.
    fn floor(extra: &'static [(i32, i32, i32)]) -> impl FnMut(i32, i32, i32) -> bool {
        move |x, y, z| (y <= 3 && x.abs() < 10 && z.abs() < 10) || extra.contains(&(x, y, z))
    }

    #[test]
    fn stands_still_on_the_floor() {
        let (mut vy, mut g) = (0.0, true);
        let mut p = Vec3 { x: 0.5, y: 4.0, z: 0.5 };
        for _ in 0..40 {
            p = step(p, &mut vy, &mut g, None, 0.2, None, &mut floor(&[]));
        }
        assert_eq!(p.y, 4.0);
        assert!(g);
    }

    #[test]
    fn walks_towards_goal_without_overshooting() {
        let (mut vy, mut g) = (0.0, true);
        let mut p = Vec3 { x: 0.5, y: 4.0, z: 0.5 };
        for _ in 0..100 {
            p = step(p, &mut vy, &mut g, Some((5.5, 0.5)), 0.2, None, &mut floor(&[]));
        }
        assert!((p.x - 5.5).abs() < 1e-3, "x = {}", p.x);
    }

    #[test]
    fn hops_up_a_one_block_step_but_not_a_wall() {
        let (mut vy, mut g) = (0.0, true);
        let mut p = Vec3 { x: 0.5, y: 4.0, z: 0.5 };
        for _ in 0..60 {
            p = step(p, &mut vy, &mut g, Some((3.5, 0.5)), 0.2, None, &mut floor(&[(2, 4, 0), (3, 4, 0), (4, 4, 0)]));
        }
        assert_eq!(p.y, 5.0, "should stand on the step");
        // two-high wall: stays put
        let (mut vy, mut g) = (0.0, true);
        let mut p = Vec3 { x: 0.5, y: 4.0, z: 0.5 };
        for _ in 0..60 {
            p = step(p, &mut vy, &mut g, Some((3.5, 0.5)), 0.2, None, &mut floor(&[(2, 4, 0), (2, 5, 0), (2, 6, 0)]));
        }
        assert!(p.x < 2.0 && p.y == 4.0);
    }

    #[test]
    fn never_steps_off_the_island_edge() {
        let (mut vy, mut g) = (0.0, true);
        let mut p = Vec3 { x: 8.5, y: 4.0, z: 0.5 };
        for _ in 0..100 {
            p = step(p, &mut vy, &mut g, Some((30.5, 0.5)), 0.2, None, &mut floor(&[]));
        }
        assert!(p.x < 10.0 && p.y == 4.0, "p = {p:?}");
    }

    #[test]
    fn leash_keeps_the_bot_near_home_but_lets_it_return() {
        let home = Vec3 { x: 0.5, y: 4.0, z: 0.5 };
        let (mut vy, mut g) = (0.0, true);
        let mut p = home;
        for _ in 0..200 {
            p = step(p, &mut vy, &mut g, Some((9.5, 0.5)), 0.2, Some((home, 5.0)), &mut floor(&[]));
        }
        assert!(p.x <= 5.6, "x = {}", p.x);
        for _ in 0..200 {
            p = step(p, &mut vy, &mut g, Some((0.5, 0.5)), 0.2, Some((home, 5.0)), &mut floor(&[]));
        }
        assert!((p.x - 0.5).abs() < 0.05);
    }

    #[test]
    fn falls_and_lands() {
        let (mut vy, mut g) = (0.0, false);
        let mut p = Vec3 { x: 0.5, y: 12.0, z: 0.5 };
        for _ in 0..80 {
            p = step(p, &mut vy, &mut g, None, 0.2, None, &mut floor(&[]));
        }
        assert_eq!(p.y, 4.0);
        assert!(g);
    }

    #[test]
    fn yaw_faces_the_target() {
        let o = Vec3::default();
        assert!((yaw_towards(o, Vec3 { x: 0.0, y: 0.0, z: 5.0 }) - 0.0).abs() < 1e-3); // +z = yaw 0
        assert!((yaw_towards(o, Vec3 { x: -5.0, y: 0.0, z: 0.0 }) - 90.0).abs() < 1e-3); // -x = yaw 90
    }
}
