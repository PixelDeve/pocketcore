//! Block hardness and mining speed (vanilla / PocketMine rules) for survival break timing.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Hand,
    Pickaxe,
    Axe,
    Shovel,
    Shears,
}

/// (hardness, preferred tool, minimum tool tier to harvest; 0 = anything). `None` = unbreakable.
fn info(id: u8) -> Option<(f64, Kind, u8)> {
    use Kind::*;
    Some(match id {
        0 | 7 | 8 | 9 | 10 | 11 | 95 => return None,
        1 => (1.5, Pickaxe, 1),
        2 => (0.6, Shovel, 0),
        3 => (0.5, Shovel, 0),
        4 => (2.0, Pickaxe, 1),
        5 => (2.0, Axe, 0),
        6 | 31 | 32 | 37 | 38 | 39 | 40 | 46 | 50 | 51 | 59 | 83 | 175 => (0.0, Hand, 0),
        12 => (0.5, Shovel, 0),
        13 => (0.6, Shovel, 0),
        14 | 56 | 129 => (3.0, Pickaxe, 3),
        15 | 21 | 22 => (3.0, Pickaxe, 2),
        16 | 73 | 74 => (3.0, Pickaxe, 1),
        17 | 162 => (2.0, Axe, 0),
        18 | 161 => (0.2, Shears, 0),
        19 => (0.6, Hand, 0),
        20 | 102 => (0.3, Hand, 0),
        24 => (0.8, Pickaxe, 1),
        26 => (0.2, Hand, 0),
        30 => (4.0, Hand, 0),
        35 => (0.8, Shears, 0),
        41 | 57 => (if id == 41 { 3.0 } else { 5.0 }, Pickaxe, 3),
        42 => (5.0, Pickaxe, 2),
        43 | 44 | 45 | 48 | 67 | 108 => (2.0, Pickaxe, 1),
        47 => (1.5, Axe, 0),
        49 => (50.0, Pickaxe, 4),
        53 | 85 | 107 | 134..=136 => (2.0, Axe, 0),
        54 | 58 => (2.5, Axe, 0),
        61 | 62 => (3.5, Pickaxe, 1),
        64 | 96 => (3.0, Axe, 0),
        65 => (0.4, Axe, 0),
        71 => (5.0, Pickaxe, 2),
        78 | 80 => (if id == 78 { 0.1 } else { 0.2 }, Shovel, 0),
        79 => (0.5, Pickaxe, 0),
        81 => (0.4, Hand, 0),
        82 => (0.6, Shovel, 0),
        87 => (0.4, Pickaxe, 1),
        89 => (0.3, Hand, 0),
        98 | 109 => (1.5, Pickaxe, 1),
        121 => (3.0, Pickaxe, 1),
        155 => (0.8, Pickaxe, 1),
        159 | 172 => (1.25, Pickaxe, 1),
        173 => (5.0, Pickaxe, 1),
        _ => (1.0, Hand, 0),
    })
}

/// (kind, tier, speed) of a held item.
fn tool(item: i16) -> Option<(Kind, u8, f64)> {
    Some(match item {
        270 => (Kind::Pickaxe, 1, 2.0),
        274 => (Kind::Pickaxe, 2, 4.0),
        257 => (Kind::Pickaxe, 3, 6.0),
        278 => (Kind::Pickaxe, 4, 8.0),
        285 => (Kind::Pickaxe, 1, 12.0),
        271 => (Kind::Axe, 1, 2.0),
        275 => (Kind::Axe, 2, 4.0),
        258 => (Kind::Axe, 3, 6.0),
        279 => (Kind::Axe, 4, 8.0),
        286 => (Kind::Axe, 1, 12.0),
        269 => (Kind::Shovel, 1, 2.0),
        273 => (Kind::Shovel, 2, 4.0),
        256 => (Kind::Shovel, 3, 6.0),
        277 => (Kind::Shovel, 4, 8.0),
        284 => (Kind::Shovel, 1, 12.0),
        359 => (Kind::Shears, 1, 5.0),
        _ => return None,
    })
}

/// Ticks needed to break block `id` with item `held` in survival (vanilla formula:
/// hardness x 1.5 with the right tool tier, x 5 without, divided by the tool speed).
/// `None` = not timed (air, bedrock, liquids).
pub fn break_ticks(id: u8, held: i16) -> Option<u32> {
    let (hardness, kind, tier) = info(id)?;
    if hardness <= 0.0 {
        return Some(0);
    }
    let (matches, tool_tier, mut speed) = match tool(held) {
        Some((k, t, s)) if k == kind => (true, t, s),
        _ => (false, 0, 1.0),
    };
    if kind == Kind::Shears && matches && id == 18 {
        speed = 15.0;
    }
    let can_harvest = tier == 0 || (matches && tool_tier >= tier);
    let secs = hardness * if can_harvest { 1.5 } else { 5.0 } / speed;
    Some((secs * 20.0 - 1e-6).ceil().max(0.0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_times() {
        assert_eq!(break_ticks(35, 0), Some(24), "wool by hand 1.2s");
        assert_eq!(break_ticks(35, 359), Some(5), "wool with shears 0.24s");
        assert_eq!(break_ticks(1, 0), Some(150), "stone by hand 7.5s");
        assert_eq!(break_ticks(1, 270), Some(23), "stone with a wooden pickaxe");
        assert_eq!(break_ticks(5, 258), Some(10), "planks with an iron axe 0.5s");
        assert_eq!(break_ticks(0, 0), None);
        assert_eq!(break_ticks(7, 0), None);
    }
}
