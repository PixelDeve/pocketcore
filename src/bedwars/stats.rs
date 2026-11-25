//! Persistent per-player stats and coin rewards (`stats.toml`).

use std::collections::BTreeMap;
use std::path::Path;

use super::toml_lite::{self, quote, write_atomic};

pub const COINS_PER_KILL: u32 = 10;
pub const COINS_PER_BED: u32 = 25;
pub const COINS_PER_WIN: u32 = 100;
pub const COINS_PER_GAME: u32 = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerStats {
    pub wins: u32,
    pub games: u32,
    pub kills: u32,
    pub beds: u32,
    pub coins: u32,
}

#[derive(Debug, Default)]
pub struct Stats {
    /// Keyed by lower-case name.
    pub players: BTreeMap<String, PlayerStats>,
    pub dirty: bool,
}

impl Stats {
    pub fn get(&self, name: &str) -> PlayerStats {
        self.players.get(&name.to_ascii_lowercase()).copied().unwrap_or_default()
    }

    pub fn entry(&mut self, name: &str) -> &mut PlayerStats {
        self.dirty = true;
        self.players.entry(name.to_ascii_lowercase()).or_default()
    }

    pub fn add_kill(&mut self, name: &str) {
        let e = self.entry(name);
        e.kills += 1;
        e.coins += COINS_PER_KILL;
    }

    pub fn add_bed(&mut self, name: &str) {
        let e = self.entry(name);
        e.beds += 1;
        e.coins += COINS_PER_BED;
    }

    /// Returns the coins this game paid out.
    pub fn finish_game(&mut self, name: &str, won: bool) -> u32 {
        let e = self.entry(name);
        e.games += 1;
        let paid = COINS_PER_GAME + if won { COINS_PER_WIN } else { 0 };
        if won {
            e.wins += 1;
        }
        e.coins += paid;
        paid
    }

    pub fn to_text(&self) -> String {
        let mut t = String::from("# Bedwars stats (written automatically)\n\n");
        for (name, s) in &self.players {
            t.push_str(&format!(
                "[[player]]\nname = {}\nwins = {}\ngames = {}\nkills = {}\nbeds = {}\ncoins = {}\n\n",
                quote(name),
                s.wins,
                s.games,
                s.kills,
                s.beds,
                s.coins
            ));
        }
        t
    }

    pub fn parse(text: &str) -> Result<Stats, String> {
        let doc = toml_lite::parse(text)?;
        let mut st = Stats::default();
        for sec in doc.all("player") {
            let name = sec.str("name").ok_or("[[player]] without name")?.to_ascii_lowercase();
            let n = |k: &str| sec.int(k).unwrap_or(0).clamp(0, u32::MAX as i64) as u32;
            st.players.insert(name, PlayerStats { wins: n("wins"), games: n("games"), kills: n("kills"), beds: n("beds"), coins: n("coins") });
        }
        Ok(st)
    }

    pub fn load(path: &Path) -> Result<Stats, String> {
        match std::fs::read_to_string(path) {
            Ok(t) => Stats::parse(&t).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Stats::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        self.dirty = false;
        write_atomic(path, &self.to_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewards_accumulate_and_roundtrip() {
        let mut s = Stats::default();
        s.add_kill("Steve");
        s.add_kill("steve");
        s.add_bed("STEVE");
        assert_eq!(s.finish_game("Steve", true), COINS_PER_GAME + COINS_PER_WIN);
        assert_eq!(s.finish_game("Alex", false), COINS_PER_GAME);
        let st = s.get("steve");
        assert_eq!((st.kills, st.beds, st.wins, st.games), (2, 1, 1, 1));
        assert_eq!(st.coins, 2 * COINS_PER_KILL + COINS_PER_BED + COINS_PER_GAME + COINS_PER_WIN);
        let back = Stats::parse(&s.to_text()).unwrap();
        assert_eq!(back.players, s.players);
        assert_eq!(back.get("nobody"), PlayerStats::default());
    }
}
