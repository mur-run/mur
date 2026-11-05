//! Turn the text of one skin file into a resolved `Theme` — inheritance,
//! derivation, `assumed_bg` and the contrast check — or the full list of
//! reasons it was rejected. Pure: no file system, so every rule is testable
//! on a string.

use super::super::{ANSI, Theme, contrast};
use super::value::{parse_hex, parse_style};
use super::whitelist::{self, Class, LAYOUT_KEYS};
use ratatui::style::{Color, Style};
use ratatui::widgets::BorderType;
use serde_yaml::Value;

const TOP_KEYS: [&str; 4] = ["inherits", "assumed_bg", "tokens", "layout"];
/// The `inherits:` value that starts from nothing.
pub const INHERITS_NONE: &str = "none";
const MAX_PADDING: u64 = 4;

/// A theme that passed every rule.
#[derive(Debug, Clone, Copy)]
pub struct Parsed {
    pub theme: Theme,
    pub assumed_bg: Option<Color>,
}

/// Parse a skin file. `Err` carries every problem found, first one first.
pub fn parse(text: &str) -> Result<Parsed, Vec<String>> {
    let doc: Value =
        serde_yaml::from_str(text).map_err(|e| vec![format!("not valid YAML: {e}")])?;
    let Value::Mapping(top) = doc else {
        return Err(vec![format!(
            "the file must be a mapping of {}",
            TOP_KEYS.join(", ")
        )]);
    };
    let mut errs = Vec::new();
    for k in top.keys() {
        let k = key_str(k);
        if !TOP_KEYS.contains(&k.as_str()) {
            errs.push(format!("unknown key `{k}`"));
        }
    }

    let inherits = match top.get("inherits") {
        None => {
            errs.push("`inherits` is required (a built-in skin name, or `none`)".into());
            None
        }
        Some(Value::String(s)) if s == INHERITS_NONE => Some(None),
        Some(Value::String(s)) => match super::super::builtin(s) {
            Some(b) => Some(Some(b)),
            None => {
                errs.push(format!(
                    "unknown `inherits: {s}` (expected {}, or none)",
                    super::super::SKIN_NAMES
                ));
                None
            }
        },
        Some(_) => {
            errs.push("`inherits` must be a skin name".into());
            None
        }
    };

    let mut theme = match inherits {
        Some(Some((base, _))) => *base,
        _ => blank(),
    };
    let tokens = section(&top, "tokens", &mut errs);
    let mut given = Vec::new();
    for (k, v) in &tokens {
        let Some(row) = whitelist::token(k) else {
            errs.push(format!("unknown key `tokens.{k}`"));
            continue;
        };
        let parsed = match v {
            Value::String(s) => parse_style(s),
            _ => Err("must be a string such as \"bold #d97757 on #262626\"".into()),
        };
        match parsed {
            Ok(style) => {
                *(row.field)(&mut theme) = style;
                given.push(row.key);
            }
            Err(e) => errs.push(format!("tokens.{k}: {e}")),
        }
    }
    if matches!(inherits, Some(None)) {
        for row in whitelist::TOKENS {
            if given.contains(&row.key) {
                continue;
            }
            match row.class {
                Class::Required => errs.push(format!(
                    "tokens.{} is required under `inherits: none`",
                    row.key
                )),
                Class::Optional => *(row.field)(&mut theme) = (row.derive)(&theme),
            }
        }
    }
    apply_layout(&mut theme, &section(&top, "layout", &mut errs), &mut errs);

    let file_bg = match top.get("assumed_bg") {
        None => None,
        Some(Value::String(s)) => match parse_hex(s) {
            Ok(c) => Some(c),
            Err(e) => {
                errs.push(format!("assumed_bg: {e}"));
                None
            }
        },
        Some(_) => {
            errs.push("assumed_bg: must be a string such as \"#101020\"".into());
            None
        }
    };
    let base_bg = match inherits {
        Some(Some((_, bg))) => bg,
        _ => None,
    };
    let assumed_bg = file_bg.or(base_bg);
    let has_rgb = whitelist::styles(&theme)
        .iter()
        .any(|s| is_rgb(s.fg) || is_rgb(s.bg));
    match assumed_bg {
        Some(bg) if has_rgb => {
            for f in contrast::check(&theme, bg) {
                errs.push(contrast_error(&f, bg));
            }
        }
        None if has_rgb && inherits.is_some() => errs.push(
            "assumed_bg is required: the skin uses #rrggbb colours and its base has no background"
                .into(),
        ),
        _ => {}
    }

    if errs.is_empty() {
        Ok(Parsed { theme, assumed_bg })
    } else {
        Err(errs)
    }
}

/// Every token empty, layout at the documented `inherits: none` defaults.
fn blank() -> Theme {
    let mut t = ANSI;
    for row in whitelist::TOKENS {
        *(row.field)(&mut t) = Style::new();
    }
    t.border_type = BorderType::Plain;
    t.inner_padding = 1;
    t.compact_input = false;
    t
}

fn section(top: &serde_yaml::Mapping, name: &str, errs: &mut Vec<String>) -> Vec<(String, Value)> {
    match top.get(name) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Mapping(m)) => m.iter().map(|(k, v)| (key_str(k), v.clone())).collect(),
        Some(_) => {
            errs.push(format!("`{name}` must be a mapping"));
            Vec::new()
        }
    }
}

fn apply_layout(theme: &mut Theme, layout: &[(String, Value)], errs: &mut Vec<String>) {
    for (k, v) in layout {
        if !LAYOUT_KEYS.contains(&k.as_str()) {
            errs.push(format!("unknown key `layout.{k}`"));
            continue;
        }
        match (k.as_str(), v) {
            ("border_type", Value::String(s)) => match s.to_ascii_lowercase().as_str() {
                "plain" => theme.border_type = BorderType::Plain,
                "rounded" => theme.border_type = BorderType::Rounded,
                "double" => theme.border_type = BorderType::Double,
                "thick" => theme.border_type = BorderType::Thick,
                other => errs.push(format!(
                    "layout.border_type: unknown `{other}` (plain | rounded | double | thick)"
                )),
            },
            ("inner_padding", Value::Number(n)) => match n.as_u64() {
                Some(p) if p <= MAX_PADDING => theme.inner_padding = p as u8,
                _ => errs.push(format!(
                    "layout.inner_padding: {n} is out of range (0–{MAX_PADDING})"
                )),
            },
            ("compact_input", Value::Bool(b)) => theme.compact_input = *b,
            ("border_type", _) => errs.push("layout.border_type: must be a word".into()),
            ("inner_padding", _) => errs.push("layout.inner_padding: must be a number".into()),
            _ => errs.push(format!("layout.{k}: must be true or false")),
        }
    }
}

fn key_str(k: &Value) -> String {
    match k {
        Value::String(s) => s.clone(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

fn is_rgb(c: Option<Color>) -> bool {
    matches!(c, Some(Color::Rgb(..)))
}

fn hex(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        other => format!("{other:?}"),
    }
}

/// `tokens.warn 2.1:1 on #101020 (needs 4.5:1)` — the file's own key names,
/// and the terminal background spelled as the colour the operator wrote.
fn contrast_error(f: &contrast::Failure, bg: Color) -> String {
    let on = match f.on {
        "assumed_bg" => hex(bg),
        "its own bg" => "its own bg".to_string(),
        surface => format!("tokens.{surface}"),
    };
    format!(
        "tokens.{} {:.1}:1 on {on} (needs {}:1)",
        f.fg, f.ratio, f.min
    )
}
