//! The creative menu. In 0.14 the *server* fills it: a ContainerSetContent for the
//! pseudo window 0x79. Without it a creative player has an empty item menu and
//! can never pick anything to build with.
//!
//! Only block / item ids that exist in 0.14.3 are listed (an unknown id shows up
//! as an "update" block on the client). Edit `LIST` to change the menu.

use crate::protocol::item::Slot;

/// (id, first meta, last meta). Every meta in the range becomes one entry.
const LIST: &[(i16, i16, i16)] = &[
    // ---- building blocks ----
    (1, 0, 6),   // stone, granite, diorite, andesite (+ polished)
    (2, 0, 0),   // grass
    (3, 0, 1),   // dirt, coarse dirt
    (4, 0, 0),   // cobblestone
    (5, 0, 5),   // planks
    (7, 0, 0),   // bedrock
    (12, 0, 1),  // sand
    (13, 0, 0),  // gravel
    (17, 0, 3),  // logs
    (162, 0, 1), // acacia / dark oak logs
    (18, 0, 3),  // leaves
    (161, 0, 1), // acacia / dark oak leaves
    (19, 0, 0),  // sponge
    (20, 0, 0),  // glass
    (102, 0, 0), // glass pane
    (24, 0, 2),  // sandstone
    (35, 0, 15), // wool
    (171, 0, 15), // carpet
    (82, 0, 0),  // clay
    (159, 0, 15), // stained clay
    (172, 0, 0), // hardened clay
    (45, 0, 0),  // bricks
    (98, 0, 3),  // stone bricks
    (48, 0, 0),  // moss stone
    (49, 0, 0),  // obsidian
    (79, 0, 0),  // ice
    (174, 0, 0), // packed ice
    (80, 0, 0),  // snow
    (87, 0, 0),  // netherrack
    (88, 0, 0),  // soul sand
    (89, 0, 0),  // glowstone
    (112, 0, 0), // nether brick
    (121, 0, 0), // end stone
    (110, 0, 0), // mycelium
    (155, 0, 2), // quartz block
    (170, 0, 0), // hay bale
    (173, 0, 0), // coal block
    (44, 0, 7),  // slabs
    // ---- ores / storage ----
    (14, 0, 0),
    (15, 0, 0),
    (16, 0, 0),
    (21, 0, 0),
    (56, 0, 0),
    (73, 0, 0),
    (129, 0, 0),
    (22, 0, 0),
    (41, 0, 0),
    (42, 0, 0),
    (57, 0, 0),
    (133, 0, 0),
    // ---- stairs / fences / doors ----
    (53, 0, 0),
    (67, 0, 0),
    (108, 0, 0),
    (109, 0, 0),
    (114, 0, 0),
    (128, 0, 0),
    (134, 0, 0),
    (135, 0, 0),
    (136, 0, 0),
    (163, 0, 0),
    (164, 0, 0),
    (156, 0, 0),
    (85, 0, 0),  // fence
    (107, 0, 0), // fence gate
    (139, 0, 1), // cobblestone wall
    (65, 0, 0),  // ladder
    // ---- functional / decoration ----
    (46, 0, 0),  // TNT
    (47, 0, 0),  // bookshelf
    (50, 0, 0),  // torch
    (54, 0, 0),  // chest
    (58, 0, 0),  // crafting table
    (61, 0, 0),  // furnace
    (81, 0, 0),  // cactus
    (86, 0, 0),  // pumpkin
    (91, 0, 0),  // jack o'lantern
    (103, 0, 0), // melon
    // ---- tools, weapons, armour ----
    (256, 0, 0),
    (257, 0, 0),
    (258, 0, 0),
    (259, 0, 0),
    (261, 0, 0),
    (262, 0, 0),
    (267, 0, 0),
    (268, 0, 0),
    (269, 0, 0),
    (270, 0, 0),
    (271, 0, 0),
    (272, 0, 0),
    (273, 0, 0),
    (274, 0, 0),
    (275, 0, 0),
    (276, 0, 0),
    (277, 0, 0),
    (278, 0, 0),
    (279, 0, 0),
    (283, 0, 0),
    (298, 0, 0),
    (299, 0, 0),
    (300, 0, 0),
    (301, 0, 0),
    (302, 0, 0),
    (303, 0, 0),
    (304, 0, 0),
    (305, 0, 0),
    (306, 0, 0),
    (307, 0, 0),
    (308, 0, 0),
    (309, 0, 0),
    (310, 0, 0),
    (311, 0, 0),
    (312, 0, 0),
    (313, 0, 0),
    (314, 0, 0),
    (315, 0, 0),
    (316, 0, 0),
    (317, 0, 0),
    // ---- misc items ----
    (260, 0, 0), // apple
    (264, 0, 0), // diamond
    (265, 0, 0), // iron ingot
    (266, 0, 0), // gold ingot
    (322, 0, 0), // golden apple
    (325, 0, 0), // bucket
    (332, 0, 0), // snowball
    (355, 0, 0), // bed (operators register team beds with it)
];

/// The menu, one `Slot` per entry (count 1).
pub fn items() -> Vec<Slot> {
    let mut v = Vec::new();
    for &(id, from, to) in LIST {
        for meta in from..=to {
            v.push(Slot::new(id, 1, meta));
        }
    }
    v
}

/// True if the client may legitimately hold this item (checked on
/// ContainerSetSlot in creative so a hacked client cannot invent ids).
pub fn contains(id: i16) -> bool {
    LIST.iter().any(|&(i, _, _)| i == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sane() {
        let v = items();
        assert!(v.len() > 100);
        assert!(v.iter().all(|s| s.id > 0 && s.count == 1));
        assert!(contains(1) && contains(355) && !contains(999));
    }
}
