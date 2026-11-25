//! Minimal TOML subset, std only (no serde / toml crate needed).
//!
//! Supported: `# comments`, `[section]`, `[[section]]` (repeatable), and
//! `key = value` where value is an integer, float, boolean, `"basic string"`
//! (escapes `\\ \" \n \t \r \uXXXX`), `'literal string'` or an array of those
//! (may span several lines). Sections are kept in file order, which matters
//! for the shop menu. A `[[category.item]]` section belongs to the closest
//! preceding `[[category]]`; the loaders implement that rule themselves.

use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Array(Vec<Value>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Section {
    /// Dotted name without brackets; empty for the top of the file.
    pub name: String,
    /// `[[name]]` rather than `[name]`.
    pub is_array: bool,
    pub values: Vec<(String, Value)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Doc {
    pub sections: Vec<Section>,
}

impl Section {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Value::Int(i)) => Some(*i),
            Some(Value::Float(f)) if f.fract() == 0.0 => Some(*f as i64),
            _ => None,
        }
    }

    pub fn float(&self, key: &str) -> Option<f64> {
        match self.get(key) {
            Some(Value::Float(f)) => Some(*f),
            Some(Value::Int(i)) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn floats(&self, key: &str) -> Option<Vec<f64>> {
        match self.get(key) {
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| match v {
                    Value::Float(f) => Some(*f),
                    Value::Int(i) => Some(*i as f64),
                    _ => None,
                })
                .collect(),
            _ => None,
        }
    }
}

impl Doc {
    /// First `[name]` / `[[name]]` section with this name.
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == name)
    }

    /// Every section with this name, in file order.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Section> + 'a {
        self.sections.iter().filter(move |s| s.name == name)
    }
}

pub fn parse(text: &str) -> Result<Doc, String> {
    let mut doc = Doc { sections: vec![Section::default()] };
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line_no = i + 1;
        let line = strip_comment(lines[i]).trim().to_owned();
        i += 1;
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("[[") {
            let name = rest.strip_suffix("]]").ok_or_else(|| format!("line {line_no}: unterminated [[section]]"))?;
            doc.sections.push(Section { name: name.trim().to_owned(), is_array: true, values: Vec::new() });
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let name = rest.strip_suffix(']').ok_or_else(|| format!("line {line_no}: unterminated [section]"))?;
            doc.sections.push(Section { name: name.trim().to_owned(), is_array: false, values: Vec::new() });
            continue;
        }
        let (key, rest) = line.split_once('=').ok_or_else(|| format!("line {line_no}: expected key = value"))?;
        let key = key.trim().trim_matches('"').to_owned();
        if key.is_empty() {
            return Err(format!("line {line_no}: empty key"));
        }
        // arrays may continue on following lines until the brackets balance
        let mut value_text = rest.trim().to_owned();
        while depth(&value_text) > 0 {
            if i >= lines.len() {
                return Err(format!("line {line_no}: unterminated array"));
            }
            value_text.push(' ');
            value_text.push_str(strip_comment(lines[i]).trim());
            i += 1;
        }
        let value = parse_value(&value_text).map_err(|e| format!("line {line_no}: {e}"))?;
        doc.sections.last_mut().expect("root section exists").values.push((key, value));
    }
    Ok(doc)
}

/// Removes a trailing `# comment` that is not inside a string.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '#' => return &line[..i],
                _ => {}
            },
        }
    }
    line
}

/// Unclosed `[` count outside strings.
fn depth(s: &str) -> i32 {
    let mut d = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in s.chars() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '[' => d += 1,
                ']' => d -= 1,
                _ => {}
            },
        }
    }
    d
}

fn parse_value(s: &str) -> Result<Value, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("missing value".into());
    }
    if let Some(inner) = s.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or("unterminated array")?;
        let mut items = Vec::new();
        for part in split_top_level(inner) {
            let part = part.trim();
            if !part.is_empty() {
                items.push(parse_value(part)?);
            }
        }
        return Ok(Value::Array(items));
    }
    if s.starts_with('"') {
        return parse_basic_string(s).map(Value::Str);
    }
    if let Some(rest) = s.strip_prefix('\'') {
        let body = rest.strip_suffix('\'').ok_or("unterminated literal string")?;
        return Ok(Value::Str(body.to_owned()));
    }
    match s {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    let clean: String = s.chars().filter(|&c| c != '_').collect();
    if let Ok(i) = clean.parse::<i64>() {
        return Ok(Value::Int(i));
    }
    if let Ok(f) = clean.parse::<f64>() {
        if f.is_finite() {
            return Ok(Value::Float(f));
        }
    }
    Err(format!("cannot parse value {s:?}"))
}

fn split_top_level(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut d = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '[' => d += 1,
                ']' => d -= 1,
                ',' if d == 0 => {
                    parts.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    parts.push(&s[start..]);
    parts
}

fn parse_basic_string(s: &str) -> Result<String, String> {
    let mut chars = s.chars();
    chars.next(); // opening quote
    let mut out = String::new();
    let mut closed = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                closed = true;
                break;
            }
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let cp = u32::from_str_radix(&hex, 16).map_err(|_| format!("bad \\u escape {hex:?}"))?;
                    out.push(char::from_u32(cp).ok_or("bad unicode escape")?);
                }
                other => return Err(format!("bad escape \\{}", other.map(String::from).unwrap_or_default())),
            },
            c => out.push(c),
        }
    }
    if !closed {
        return Err("unterminated string".into());
    }
    let tail: String = chars.collect();
    if !tail.trim().is_empty() {
        return Err(format!("unexpected text after string: {tail:?}"));
    }
    Ok(out)
}

// ---- writing --------------------------------------------------------------------------

/// `"..."` with the escapes the parser understands.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Shortest text that parses back to the same f64 (always has a `.` so it
/// stays a float).
pub fn float_text(v: f64) -> String {
    let t = format!("{v}");
    if t.contains('.') || t.contains('e') || t.contains("inf") || t.contains("NaN") {
        t
    } else {
        format!("{t}.0")
    }
}

/// Writes `text` to `path` through a temporary file so a crash cannot leave
/// a half-written config behind.
pub fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_values_and_comments() {
        let doc = parse(
            "# top\nname = \"Bed # wars\"  # trailing\nn = 1_000\nf = 2.5\non = true\n\n[[category]]\nname = 'Blocks'\nids = [1, 2,\n  3]\n[[category.item]]\nlabel = \"\\u00a7aWool\\n2\"\n",
        )
        .unwrap();
        assert_eq!(doc.sections[0].str("name"), Some("Bed # wars"));
        assert_eq!(doc.sections[0].int("n"), Some(1000));
        assert_eq!(doc.sections[0].float("f"), Some(2.5));
        assert_eq!(doc.sections[0].bool("on"), Some(true));
        let cat = doc.section("category").unwrap();
        assert!(cat.is_array);
        assert_eq!(cat.str("name"), Some("Blocks"));
        assert_eq!(cat.floats("ids"), Some(vec![1.0, 2.0, 3.0]));
        assert_eq!(doc.section("category.item").unwrap().str("label"), Some("\u{a7}aWool\n2"));
        assert_eq!(doc.all("category").count(), 1);
    }

    #[test]
    fn errors_name_the_line() {
        assert!(parse("a = \n").unwrap_err().contains("line 1"));
        assert!(parse("ok = 1\nbad line\n").unwrap_err().contains("line 2"));
        assert!(parse("[x\n").unwrap_err().contains("line 1"));
        assert!(parse("a = \"open\n").is_err());
        assert!(parse("a = [1, 2\n").is_err());
        assert!(parse("a = \"x\" y\n").is_err());
    }

    #[test]
    fn quote_roundtrips() {
        for s in ["plain", "a\"b\\c", "line\nbreak\ttab", "\u{a7}6gold", "ctl\u{1}x"] {
            let doc = parse(&format!("k = {}\n", quote(s))).unwrap();
            assert_eq!(doc.sections[0].str("k"), Some(s));
        }
    }

    #[test]
    fn float_text_stays_float() {
        for v in [0.0, 1.0, -28.5, 0.1, 1e-7, 123456.789] {
            let doc = parse(&format!("k = {}\n", float_text(v))).unwrap();
            assert_eq!(doc.sections[0].float("k"), Some(v));
        }
        assert!(float_text(3.0).contains('.'));
    }
}
