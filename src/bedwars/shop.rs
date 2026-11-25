//! Shop catalogue, loaded from `shop.toml` (edit the file, then `/bedwars reload`
//! or restart; no recompiling). Pure data: presentation and purchasing live in
//! the match code.
//!
//! ```toml
//! title = "Item Shop"
//! [[category]]
//! name = "Blocks"
//! icon = 35
//! [[category.item]]
//! name = "Wool"
//! id = 35
//! meta = -1        # -1 = the buyer's team colour
//! amount = 16
//! currency = "iron"   # iron | gold | emerald
//! price = 4
//! ```

use super::toml_lite::{self, Doc, Section};

pub const IRON: i16 = 265;
pub const GOLD: i16 = 266;
pub const EMERALD: i16 = 388;

pub const MAX_PRICE: i64 = 2304; // 36 slots x 64

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Currency {
    Iron,
    Gold,
    Emerald,
}

impl Currency {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "iron" => Some(Self::Iron),
            "gold" => Some(Self::Gold),
            "emerald" => Some(Self::Emerald),
            _ => None,
        }
    }

    pub fn item_id(self) -> i16 {
        match self {
            Self::Iron => IRON,
            Self::Gold => GOLD,
            Self::Emerald => EMERALD,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Iron => "iron",
            Self::Gold => "gold",
            Self::Emerald => "emerald",
        }
    }

    /// Chat label with a colour code.
    pub fn label(self) -> &'static str {
        match self {
            Self::Iron => "\u{a7}7Iron",
            Self::Gold => "\u{a7}6Gold",
            Self::Emerald => "\u{a7}aEmerald",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmorSet {
    Leather,
    Chain,
    Iron,
    Diamond,
    Gold,
}

impl ArmorSet {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "leather" => Some(Self::Leather),
            "chain" | "chainmail" => Some(Self::Chain),
            "iron" => Some(Self::Iron),
            "diamond" => Some(Self::Diamond),
            "gold" | "golden" => Some(Self::Gold),
            _ => None,
        }
    }

    /// Helmet, chestplate, leggings, boots item ids.
    pub fn ids(self) -> [i16; 4] {
        match self {
            Self::Leather => [298, 299, 300, 301],
            Self::Chain => [302, 303, 304, 305],
            Self::Iron => [306, 307, 308, 309],
            Self::Diamond => [310, 311, 312, 313],
            Self::Gold => [314, 315, 316, 317],
        }
    }

    /// Higher = better; a set never replaces a stronger one.
    pub fn tier(self) -> u8 {
        match self {
            Self::Leather => 0,
            Self::Gold => 1,
            Self::Chain => 2,
            Self::Iron => 3,
            Self::Diamond => 4,
        }
    }

    pub fn from_chestplate(id: i16) -> Option<Self> {
        [Self::Leather, Self::Chain, Self::Iron, Self::Diamond, Self::Gold].into_iter().find(|s| s.ids()[1] == id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShopItem {
    pub name: String,
    pub id: i16,
    /// Damage / colour. `-1` = the buyer's team colour.
    pub meta: i16,
    pub amount: u8,
    pub currency: Currency,
    pub price: u32,
    /// Buying this equips a whole armour set instead of giving `id`.
    pub armor_set: Option<ArmorSet>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Category {
    pub name: String,
    pub icon: i16,
    pub items: Vec<ShopItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shop {
    pub title: String,
    pub categories: Vec<Category>,
}

/// Wool damage values for the four teams (red, green, yellow, blue).
pub fn team_color_meta(team: usize) -> i16 {
    [14, 5, 4, 11].get(team).copied().unwrap_or(0)
}

impl ShopItem {
    pub fn resolved_meta(&self, team: usize) -> i16 {
        if self.meta < 0 {
            team_color_meta(team)
        } else {
            self.meta
        }
    }

    /// `1. Wool x16 - 4 Iron` style line (without the leading number).
    pub fn describe(&self) -> String {
        let qty = if self.amount > 1 { format!(" \u{a7}7x{}", self.amount) } else { String::new() };
        format!("\u{a7}f{}{} \u{a7}8- \u{a7}e{} {}", self.name, qty, self.price, self.currency.label())
    }
}

impl Shop {
    pub fn parse(text: &str) -> Result<Shop, String> {
        let doc: Doc = toml_lite::parse(text)?;
        let title = doc.sections[0].str("title").unwrap_or("Item Shop").to_owned();
        let mut categories: Vec<Category> = Vec::new();
        for sec in &doc.sections[1..] {
            match sec.name.as_str() {
                "category" => categories.push(parse_category(sec, categories.len() + 1)?),
                "category.item" => {
                    let cat = categories.last_mut().ok_or("[[category.item]] before any [[category]]")?;
                    let n = cat.items.len() + 1;
                    cat.items.push(parse_item(sec, &cat.name, n)?);
                }
                other => return Err(format!("unknown section [{other}] (expected [[category]] / [[category.item]])")),
            }
        }
        if categories.is_empty() {
            return Err("shop has no categories".into());
        }
        if let Some(c) = categories.iter().find(|c| c.items.is_empty()) {
            return Err(format!("category {:?} has no items", c.name));
        }
        if categories.len() > 9 {
            return Err("at most 9 categories are supported".into());
        }
        Ok(Shop { title, categories })
    }

    pub fn default_text() -> &'static str {
        DEFAULT_SHOP
    }

    pub fn builtin() -> Shop {
        Shop::parse(DEFAULT_SHOP).expect("built-in shop.toml is valid")
    }

    /// Reads `path`; if the file is missing writes the default one first.
    pub fn load_or_create(path: &std::path::Path) -> Result<Shop, String> {
        match std::fs::read_to_string(path) {
            Ok(mut text) => {
                let parsed = Shop::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                let has = |id| parsed.categories.iter().flat_map(|c| c.items.iter()).any(|item| item.id == id);
                let mut additions = String::new();
                if !has(46) {
                    additions.push_str("\n\n[[category.item]]\nname = \"TNT\"\nid = 46\namount = 1\ncurrency = \"gold\"\nprice = 5\n");
                }
                if !has(165) {
                    additions.push_str("\n[[category.item]]\nname = \"Slime Block\"\nid = 165\namount = 1\ncurrency = \"emerald\"\nprice = 2\n");
                }
                if !additions.is_empty() {
                    text.push_str(&additions);
                    toml_lite::write_atomic(path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                Shop::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                toml_lite::write_atomic(path, DEFAULT_SHOP).map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(Shop::builtin())
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

fn need_int(sec: &Section, key: &str, what: &str) -> Result<i64, String> {
    sec.int(key).ok_or_else(|| format!("{what}: missing or non-integer `{key}`"))
}

fn parse_category(sec: &Section, n: usize) -> Result<Category, String> {
    let name = sec.str("name").ok_or_else(|| format!("category #{n}: missing `name`"))?.to_owned();
    let icon = need_int(sec, "icon", &format!("category {name:?}"))?;
    if !(1..=511).contains(&icon) {
        return Err(format!("category {name:?}: icon id {icon} out of range 1..=511"));
    }
    Ok(Category { name, icon: icon as i16, items: Vec::new() })
}

fn parse_item(sec: &Section, cat: &str, n: usize) -> Result<ShopItem, String> {
    let name = sec.str("name").ok_or_else(|| format!("{cat} item #{n}: missing `name`"))?.to_owned();
    let what = format!("{cat} / {name}");
    let id = need_int(sec, "id", &what)?;
    if !(1..=511).contains(&id) {
        return Err(format!("{what}: id {id} out of range 1..=511"));
    }
    let meta = sec.int("meta").unwrap_or(0);
    if !(-1..=32767).contains(&meta) {
        return Err(format!("{what}: meta {meta} out of range -1..=32767"));
    }
    let amount = sec.int("amount").unwrap_or(1);
    if !(1..=64).contains(&amount) {
        return Err(format!("{what}: amount {amount} out of range 1..=64"));
    }
    let cur = sec.str("currency").ok_or_else(|| format!("{what}: missing `currency`"))?;
    let currency = Currency::from_name(cur).ok_or_else(|| format!("{what}: currency {cur:?} is not iron/gold/emerald"))?;
    let price = need_int(sec, "price", &what)?;
    if !(0..=MAX_PRICE).contains(&price) {
        return Err(format!("{what}: price {price} out of range 0..={MAX_PRICE}"));
    }
    let armor_set = match sec.str("armor_set") {
        Some(s) => Some(ArmorSet::from_name(s).ok_or_else(|| format!("{what}: unknown armor_set {s:?}"))?),
        None => None,
    };
    Ok(ShopItem { name, id: id as i16, meta: meta as i16, amount: amount as u8, currency, price: price as u32, armor_set })
}

const DEFAULT_SHOP: &str = r#"# Bedwars shop. Edit freely, then run /bedwars reload (or restart the server).
#
# [[category]]            name, icon (item id shown in the menu)
# [[category.item]]       belongs to the closest [[category]] above it
#   name      text shown in the menu (colour codes: use the section sign or §)
#   id        item id                     meta     damage / colour, -1 = buyer's team colour
#   amount    stack size given (1-64)     currency iron | gold | emerald
#   price     how many of that currency   armor_set (optional) leather|chain|iron|diamond|gold
#                                         -> equips the whole set as permanent armour

title = "§6Item Shop"

[[category]]
name = "Blocks"
icon = 35

[[category.item]]
name = "Wool"
id = 35
meta = -1
amount = 16
currency = "iron"
price = 4

[[category.item]]
name = "Wood Planks"
id = 5
amount = 16
currency = "gold"
price = 4

[[category.item]]
name = "Glass"
id = 20
amount = 4
currency = "iron"
price = 12

[[category.item]]
name = "Sandstone"
id = 24
amount = 12
currency = "iron"
price = 24

[[category.item]]
name = "Obsidian"
id = 49
amount = 4
currency = "emerald"
price = 4

[[category.item]]
name = "Ladder"
id = 65
amount = 8
currency = "iron"
price = 4

[[category.item]]
name = "TNT"
id = 46
amount = 1
currency = "gold"
price = 5

[[category.item]]
name = "Slime Block"
id = 165
amount = 1
currency = "emerald"
price = 2

[[category]]
name = "Weapons"
icon = 267

[[category.item]]
name = "Stone Sword"
id = 272
currency = "iron"
price = 10

[[category.item]]
name = "Iron Sword"
id = 267
currency = "gold"
price = 7

[[category.item]]
name = "Diamond Sword"
id = 276
currency = "emerald"
price = 4

[[category]]
name = "Armor"
icon = 303

[[category.item]]
name = "Chainmail Armor"
id = 303
armor_set = "chain"
currency = "iron"
price = 40

[[category.item]]
name = "Iron Armor"
id = 307
armor_set = "iron"
currency = "gold"
price = 12

[[category.item]]
name = "Diamond Armor"
id = 311
armor_set = "diamond"
currency = "emerald"
price = 6

[[category]]
name = "Tools"
icon = 257

[[category.item]]
name = "Shears"
id = 359
currency = "iron"
price = 20

[[category.item]]
name = "Wooden Pickaxe"
id = 270
currency = "iron"
price = 10

[[category.item]]
name = "Iron Pickaxe"
id = 257
currency = "gold"
price = 3

[[category.item]]
name = "Stone Axe"
id = 275
currency = "iron"
price = 10

[[category.item]]
name = "Iron Axe"
id = 258
currency = "gold"
price = 3
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_shop_is_valid_and_ordered() {
        let s = Shop::builtin();
        assert_eq!(s.title, "\u{a7}6Item Shop");
        assert_eq!(s.categories.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Blocks", "Weapons", "Armor", "Tools"]);
        let wool = &s.categories[0].items[0];
        assert_eq!((wool.id, wool.meta, wool.amount, wool.price, wool.currency), (35, -1, 16, 4, Currency::Iron));
        assert_eq!(wool.resolved_meta(0), 14);
        assert_eq!(wool.resolved_meta(3), 11);
        assert_eq!(s.categories[2].items[0].armor_set, Some(ArmorSet::Chain));
    }

    #[test]
    fn rejects_bad_files_with_a_reason() {
        assert!(Shop::parse("").unwrap_err().contains("no categories"));
        assert!(Shop::parse("[[category.item]]\nname=\"x\"\n").unwrap_err().contains("before any"));
        let cat = "[[category]]\nname=\"A\"\nicon=1\n";
        assert!(Shop::parse(cat).unwrap_err().contains("no items"));
        let bad_cur = format!("{cat}[[category.item]]\nname=\"x\"\nid=1\ncurrency=\"dirt\"\nprice=1\n");
        assert!(Shop::parse(&bad_cur).unwrap_err().contains("currency"));
        let bad_amt = format!("{cat}[[category.item]]\nname=\"x\"\nid=1\namount=65\ncurrency=\"iron\"\nprice=1\n");
        assert!(Shop::parse(&bad_amt).unwrap_err().contains("amount"));
        let ok = format!("{cat}[[category.item]]\nname=\"x\"\nid=1\ncurrency=\"Gold\"\nprice=0\n");
        assert_eq!(Shop::parse(&ok).unwrap().categories[0].items[0].currency, Currency::Gold);
        assert!(Shop::parse("[bogus]\n").unwrap_err().contains("unknown section"));
    }

    #[test]
    fn armor_sets() {
        assert_eq!(ArmorSet::Iron.ids(), [306, 307, 308, 309]);
        assert_eq!(ArmorSet::from_chestplate(311), Some(ArmorSet::Diamond));
        assert!(ArmorSet::Diamond.tier() > ArmorSet::Iron.tier());
        assert!(ArmorSet::Iron.tier() > ArmorSet::Chain.tier());
        assert!(ArmorSet::Chain.tier() > ArmorSet::Leather.tier());
    }

    #[test]
    fn load_or_create_writes_default_once() {
        let dir = std::env::temp_dir().join(format!("pc_shop_test_{}", std::process::id()));
        let path = dir.join("shop.toml");
        let _ = std::fs::remove_dir_all(&dir);
        let first = Shop::load_or_create(&path).unwrap();
        assert!(path.exists());
        std::fs::write(&path, DEFAULT_SHOP.replace("price = 4\n", "price = 5\n")).unwrap();
        let edited = Shop::load_or_create(&path).unwrap();
        assert_ne!(first, edited);
        std::fs::write(&path, "title = 1\n[[category]]\n").unwrap();
        assert!(Shop::load_or_create(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn describe_line() {
        let s = Shop::builtin();
        assert_eq!(s.categories[0].items[0].describe(), "\u{a7}fWool \u{a7}7x16 \u{a7}8- \u{a7}e4 \u{a7}7Iron");
        assert_eq!(s.categories[1].items[0].describe(), "\u{a7}fStone Sword \u{a7}8- \u{a7}e10 \u{a7}7Iron");
    }
}
