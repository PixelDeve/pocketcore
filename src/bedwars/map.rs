//! Setup data of the original (template) map: waiting room, team spawns,
//! resource spawners, shop NPCs and team beds.
//!
//! Stored in `maps/original.toml` (plain text, hand-editable) and written
//! after every setup command. Every cloned match instance reads the same
//! data, so nothing is ever configured per instance.

use std::path::Path;

use super::toml_lite::{self, float_text, quote, write_atomic, Doc};

pub const TEAM_COUNT: usize = 4;
pub const TEAM_NAMES: [&str; TEAM_COUNT] = ["red", "green", "yellow", "blue"];
/// Chat colour codes (section sign + code).
pub const TEAM_COLORS: [&str; TEAM_COUNT] = ["\u{a7}c", "\u{a7}a", "\u{a7}e", "\u{a7}9"];

pub fn team_index(name: &str) -> Option<usize> {
    let n = name.trim().to_ascii_lowercase();
    TEAM_NAMES.iter().position(|t| *t == n)
}

/// "§cRed" style coloured team name.
pub fn team_label(team: usize) -> String {
    let n = TEAM_NAMES[team % TEAM_COUNT];
    let mut cap = n.to_owned();
    cap[..1].make_ascii_uppercase();
    format!("{}{}", TEAM_COLORS[team % TEAM_COUNT], cap)
}

/// Feet position plus facing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnerKind {
    /// Iron and gold.
    Basic,
    /// Emeralds.
    Advanced,
}

impl SpawnerKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Advanced => "advance",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "basic" => Some(Self::Basic),
            "advance" | "advanced" => Some(Self::Advanced),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spawner {
    pub kind: SpawnerKind,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockXyz {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// A registered bed: both halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BedSpot {
    pub foot: BlockXyz,
    pub head: BlockXyz,
}

impl BedSpot {
    pub fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        let p = BlockXyz { x, y, z };
        p == self.foot || p == self.head
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapConfig {
    pub waiting_room: Option<Spot>,
    pub teams: [Option<Spot>; TEAM_COUNT],
    pub spawners: Vec<Spawner>,
    pub npcs: Vec<Spot>,
    pub beds: [Option<BedSpot>; TEAM_COUNT],
}

impl MapConfig {
    /// What is still missing before a match can run (empty = ready).
    pub fn problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        if self.waiting_room.is_none() {
            p.push("waiting room (/waitingroom create)".to_owned());
        }
        for t in 0..TEAM_COUNT {
            if self.teams[t].is_none() {
                p.push(format!("{} team spawn (/team create {})", TEAM_NAMES[t], TEAM_NAMES[t]));
            }
            if self.beds[t].is_none() {
                p.push(format!("{} bed (/bed team {})", TEAM_NAMES[t], TEAM_NAMES[t]));
            }
        }
        p
    }

    pub fn bed_at(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        (0..TEAM_COUNT).find(|&t| self.beds[t].map_or(false, |b| b.contains(x, y, z)))
    }

    /// A same-kind spawner on the same block is replaced, not duplicated.
    pub fn add_spawner(&mut self, s: Spawner) {
        self.spawners.retain(|o| !(o.x == s.x && o.y == s.y && o.z == s.z));
        self.spawners.push(s);
    }

    /// Removes the spawner nearest to the point if it is within `radius`.
    pub fn remove_spawner_near(&mut self, x: f32, y: f32, z: f32, radius: f32) -> Option<Spawner> {
        let idx = nearest(self.spawners.iter().map(|s| (s.x as f32 + 0.5, s.y as f32 + 0.5, s.z as f32 + 0.5)), (x, y, z), radius)?;
        Some(self.spawners.remove(idx))
    }

    /// NPCs closer than one block replace each other.
    pub fn add_npc(&mut self, s: Spot) {
        self.npcs.retain(|o| dist2((o.x, o.y, o.z), (s.x, s.y, s.z)) > 1.0);
        self.npcs.push(s);
    }

    pub fn remove_npc_near(&mut self, x: f32, y: f32, z: f32, radius: f32) -> Option<Spot> {
        let idx = nearest(self.npcs.iter().map(|n| (n.x, n.y, n.z)), (x, y, z), radius)?;
        Some(self.npcs.remove(idx))
    }

    // ---- text form ---------------------------------------------------------------------

    pub fn to_text(&self) -> String {
        let mut t = String::from("# Original map setup. Written by the setup commands; safe to hand-edit while the server is stopped.\n\n");
        if let Some(s) = &self.waiting_room {
            t.push_str("[waiting_room]\n");
            push_spot(&mut t, s);
            t.push('\n');
        }
        for (i, spot) in self.teams.iter().enumerate() {
            if let Some(s) = spot {
                t.push_str(&format!("[[team]]\nname = {}\n", quote(TEAM_NAMES[i])));
                push_spot(&mut t, s);
                t.push('\n');
            }
        }
        for s in &self.spawners {
            t.push_str(&format!("[[spawner]]\nkind = {}\nx = {}\ny = {}\nz = {}\n\n", quote(s.kind.name()), s.x, s.y, s.z));
        }
        for n in &self.npcs {
            t.push_str("[[shop_npc]]\n");
            push_spot(&mut t, n);
            t.push('\n');
        }
        for (i, bed) in self.beds.iter().enumerate() {
            if let Some(b) = bed {
                t.push_str(&format!(
                    "[[bed]]\nteam = {}\nfoot = [{}, {}, {}]\nhead = [{}, {}, {}]\n\n",
                    quote(TEAM_NAMES[i]),
                    b.foot.x,
                    b.foot.y,
                    b.foot.z,
                    b.head.x,
                    b.head.y,
                    b.head.z
                ));
            }
        }
        t
    }

    pub fn parse(text: &str) -> Result<MapConfig, String> {
        let doc: Doc = toml_lite::parse(text)?;
        let mut cfg = MapConfig::default();
        for sec in &doc.sections[1..] {
            match sec.name.as_str() {
                "waiting_room" => cfg.waiting_room = Some(read_spot(sec, "waiting_room")?),
                "team" => {
                    let name = sec.str("name").ok_or("[[team]] without name")?;
                    let i = team_index(name).ok_or_else(|| format!("unknown team {name:?}"))?;
                    cfg.teams[i] = Some(read_spot(sec, "team")?);
                }
                "spawner" => {
                    let kind = sec.str("kind").and_then(SpawnerKind::from_name).ok_or("[[spawner]] needs kind = basic|advance")?;
                    let (x, y, z) = (req_i32(sec, "x")?, req_i32(sec, "y")?, req_i32(sec, "z")?);
                    cfg.add_spawner(Spawner { kind, x, y, z });
                }
                "shop_npc" => cfg.npcs.push(read_spot(sec, "shop_npc")?),
                "bed" => {
                    let name = sec.str("team").ok_or("[[bed]] without team")?;
                    let i = team_index(name).ok_or_else(|| format!("unknown team {name:?}"))?;
                    cfg.beds[i] = Some(BedSpot { foot: read_xyz(sec, "foot")?, head: read_xyz(sec, "head")? });
                }
                other => return Err(format!("unknown section [{other}]")),
            }
        }
        Ok(cfg)
    }

    /// Missing file = empty config.
    pub fn load(path: &Path) -> Result<MapConfig, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => MapConfig::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MapConfig::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_atomic(path, &self.to_text())
    }
}

fn push_spot(t: &mut String, s: &Spot) {
    t.push_str(&format!(
        "x = {}\ny = {}\nz = {}\nyaw = {}\npitch = {}\n",
        float_text(s.x as f64),
        float_text(s.y as f64),
        float_text(s.z as f64),
        float_text(s.yaw as f64),
        float_text(s.pitch as f64)
    ));
}

fn read_spot(sec: &toml_lite::Section, what: &str) -> Result<Spot, String> {
    let f = |k: &str| sec.float(k).map(|v| v as f32).ok_or_else(|| format!("[{what}] needs a number `{k}`"));
    Ok(Spot { x: f("x")?, y: f("y")?, z: f("z")?, yaw: sec.float("yaw").unwrap_or(0.0) as f32, pitch: sec.float("pitch").unwrap_or(0.0) as f32 })
}

fn req_i32(sec: &toml_lite::Section, k: &str) -> Result<i32, String> {
    sec.int(k).and_then(|v| i32::try_from(v).ok()).ok_or_else(|| format!("[[{}]] needs an integer `{k}`", sec.name))
}

fn read_xyz(sec: &toml_lite::Section, k: &str) -> Result<BlockXyz, String> {
    let v = sec.floats(k).filter(|v| v.len() == 3 && v.iter().all(|f| f.fract() == 0.0)).ok_or_else(|| format!("[[bed]] `{k}` must be [x, y, z] integers"))?;
    Ok(BlockXyz { x: v[0] as i32, y: v[1] as i32, z: v[2] as i32 })
}

fn dist2(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let (dx, dy, dz) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    dx * dx + dy * dy + dz * dz
}

fn nearest(points: impl Iterator<Item = (f32, f32, f32)>, from: (f32, f32, f32), radius: f32) -> Option<usize> {
    points
        .enumerate()
        .map(|(i, p)| (i, dist2(p, from)))
        .filter(|&(_, d)| d <= radius * radius)
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(x: f32, z: f32) -> Spot {
        Spot { x, y: 64.0, z, yaw: 90.5, pitch: -10.0 }
    }

    fn full() -> MapConfig {
        let mut m = MapConfig::default();
        m.waiting_room = Some(spot(0.5, 0.5));
        for t in 0..TEAM_COUNT {
            m.teams[t] = Some(spot(20.5 * t as f32, -30.5));
            let b = BlockXyz { x: 20 * t as i32, y: 64, z: -28 };
            m.beds[t] = Some(BedSpot { foot: b, head: BlockXyz { x: b.x, y: b.y, z: b.z + 1 } });
        }
        m.add_spawner(Spawner { kind: SpawnerKind::Basic, x: 3, y: 64, z: -3 });
        m.add_spawner(Spawner { kind: SpawnerKind::Advanced, x: -7, y: 70, z: 9 });
        m.add_npc(spot(5.5, 5.5));
        m
    }

    #[test]
    fn round_trips_through_text() {
        let m = full();
        let back = MapConfig::parse(&m.to_text()).unwrap();
        assert_eq!(back, m);
        assert!(back.problems().is_empty());
    }

    #[test]
    fn problems_lists_what_is_missing() {
        let mut m = MapConfig::default();
        assert_eq!(m.problems().len(), 1 + 2 * TEAM_COUNT);
        m = full();
        m.teams[2] = None;
        m.beds[0] = None;
        let p = m.problems();
        assert_eq!(p.len(), 2);
        assert!(p[0].contains("red bed") && p[0].contains("/bed team red"));
        assert!(p[1].contains("yellow team spawn") && p[1].contains("/team create yellow"));
    }

    #[test]
    fn bed_lookup_covers_both_halves() {
        let m = full();
        assert_eq!(m.bed_at(20, 64, -28), Some(1));
        assert_eq!(m.bed_at(20, 64, -27), Some(1));
        assert_eq!(m.bed_at(20, 64, -26), None);
        assert_eq!(m.bed_at(0, 65, -28), None);
    }

    #[test]
    fn spawners_and_npcs_replace_or_remove() {
        let mut m = full();
        m.add_spawner(Spawner { kind: SpawnerKind::Advanced, x: 3, y: 64, z: -3 });
        assert_eq!(m.spawners.len(), 2);
        assert_eq!(m.spawners.iter().find(|s| s.x == 3).unwrap().kind, SpawnerKind::Advanced);
        assert!(m.remove_spawner_near(100.0, 64.0, 100.0, 2.0).is_none());
        assert_eq!(m.remove_spawner_near(3.5, 64.5, -2.5, 2.0).unwrap().x, 3);
        m.add_npc(spot(5.9, 5.5));
        assert_eq!(m.npcs.len(), 1);
        m.add_npc(spot(9.5, 5.5));
        assert_eq!(m.npcs.len(), 2);
        assert!(m.remove_npc_near(9.0, 64.0, 5.0, 2.0).is_some());
        assert_eq!(m.npcs.len(), 1);
    }

    #[test]
    fn rejects_unknown_things() {
        assert!(MapConfig::parse("[[team]]\nname=\"purple\"\nx=1\ny=2\nz=3\n").unwrap_err().contains("unknown team"));
        assert!(MapConfig::parse("[nonsense]\n").is_err());
        assert!(MapConfig::parse("[[bed]]\nteam=\"red\"\nfoot=[1,2]\nhead=[1,2,3]\n").is_err());
        assert!(MapConfig::parse("[[spawner]]\nkind=\"basic\"\nx=1\ny=2\n").is_err());
    }

    #[test]
    fn save_and_load_file() {
        let dir = std::env::temp_dir().join(format!("pc_map_test_{}", std::process::id()));
        let path = dir.join("maps").join("original.toml");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(MapConfig::load(&path).unwrap(), MapConfig::default());
        let m = full();
        m.save(&path).unwrap();
        assert_eq!(MapConfig::load(&path).unwrap(), m);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn team_names() {
        assert_eq!(team_index(" Yellow "), Some(2));
        assert_eq!(team_index("pink"), None);
        assert_eq!(team_label(3), "\u{a7}9Blue");
    }
}
