//! The value syntax of one token in a user skin file:
//! `[modifier …] [fg] [on bg]`, space-separated, case-insensitive, or `none`
//! alone for the empty style. Every value parses into a typed `Style`; no
//! string from the file ever reaches the terminal.

use ratatui::style::{Color, Modifier, Style};

/// Parse a token value into a `Style`. The error is one short clause naming
/// what was wrong, ready to follow `tokens.<key>: `.
pub fn parse_style(raw: &str) -> Result<Style, String> {
    let lower = raw.trim().to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.is_empty() {
        return Err("empty value (use `none` for no style)".into());
    }
    if words == ["none"] {
        return Ok(Style::new());
    }
    let mut style = Style::new();
    let mut i = 0;
    while let Some(m) = words.get(i).and_then(|w| modifier(w)) {
        style = style.add_modifier(m);
        i += 1;
    }
    if let Some(w) = words.get(i).filter(|w| **w != "on") {
        style = style.fg(colour(w)?);
        i += 1;
    }
    if words.get(i) == Some(&"on") {
        let Some(w) = words.get(i + 1) else {
            return Err("`on` needs a colour after it".into());
        };
        style = style.bg(colour(w)?);
        i += 2;
    }
    match words.get(i) {
        None => Ok(style),
        Some(w) => Err(format!(
            "unexpected `{w}` (expected `[modifier …] [colour] [on colour]`)"
        )),
    }
}

/// Parse `assumed_bg`: an `#rrggbb` colour and nothing else — the contrast
/// check needs a measurable background.
pub fn parse_hex(raw: &str) -> Result<Color, String> {
    let w = raw.trim().to_ascii_lowercase();
    match colour(&w)? {
        c @ Color::Rgb(..) => Ok(c),
        _ => Err(format!("`{w}` is not an #rrggbb colour")),
    }
}

fn modifier(w: &str) -> Option<Modifier> {
    Some(match w {
        "bold" => Modifier::BOLD,
        "dim" => Modifier::DIM,
        "italic" => Modifier::ITALIC,
        "underlined" => Modifier::UNDERLINED,
        "reversed" => Modifier::REVERSED,
        _ => return None,
    })
}

/// The sixteen ANSI slots by the names the spec documents, plus `default`.
const NAMED: [(&str, Color); 17] = [
    ("black", Color::Black),
    ("red", Color::Red),
    ("green", Color::Green),
    ("yellow", Color::Yellow),
    ("blue", Color::Blue),
    ("magenta", Color::Magenta),
    ("cyan", Color::Cyan),
    ("gray", Color::Gray),
    ("dark-gray", Color::DarkGray),
    ("light-red", Color::LightRed),
    ("light-green", Color::LightGreen),
    ("light-yellow", Color::LightYellow),
    ("light-blue", Color::LightBlue),
    ("light-magenta", Color::LightMagenta),
    ("light-cyan", Color::LightCyan),
    ("white", Color::White),
    ("default", Color::Reset),
];

fn colour(w: &str) -> Result<Color, String> {
    if let Some(hex) = w.strip_prefix('#') {
        let ok = hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(format!("`{w}` is not an #rrggbb colour"));
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
        return Ok(Color::Rgb(byte(0), byte(2), byte(4)));
    }
    NAMED
        .iter()
        .find(|(n, _)| *n == w)
        .map(|(_, c)| *c)
        .ok_or_else(|| format!("unknown word `{w}`"))
}
