//! User-skin guards from spec §7, parser-level.

use super::parse::{Parsed, parse};
use super::value::parse_style;
use super::whitelist::{self, Class, LAYOUT_KEYS, TOKENS};
use super::*;
use crate::cmd::agent::cli::theme::{ASSUMED_BG_MUR, MUR};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

fn ok(text: &str) -> Parsed {
    parse(text).unwrap_or_else(|e| panic!("expected to load, got {e:?}"))
}

fn errs(text: &str) -> Vec<String> {
    match parse(text) {
        Ok(_) => panic!("expected a rejection"),
        Err(e) => e,
    }
}

/// The ten required tokens, all named slots, so no contrast check applies.
const REQUIRED_NAMED: &str = "
  text: white
  muted: gray
  emphasis: bold white
  accent: cyan
  accent_alt: green
  ok: green
  warn: yellow
  error: red
  border: dark-gray
  badge: reversed cyan";

fn none_file(tokens: &str) -> String {
    format!("inherits: none\ntokens:{tokens}\n")
}

/// Every `Theme` field, by exhaustive destructuring: adding a field to
/// `Theme` stops this compiling until it is named here — and then the
/// assertions below demand a whitelist row for it.
macro_rules! theme_fields {
    ($($f:ident),* $(,)?) => {{
        let Theme { $($f: _),* } = MUR;
        [$(stringify!($f)),*]
    }};
}

#[test]
fn whitelist_covers_theme() {
    let fields = theme_fields!(
        text,
        muted,
        emphasis,
        accent,
        accent_alt,
        ok,
        warn,
        error,
        code,
        diff_add_mark,
        diff_del_mark,
        diff_add_bg,
        diff_del_bg,
        diff_add_text,
        diff_del_text,
        border,
        surface,
        surface_alt,
        settlement_surface,
        settlement_text,
        settlement_muted,
        settlement_accent,
        settlement_ok,
        settlement_warn,
        settlement_error,
        badge,
        badge_warn,
        border_type,
        inner_padding,
        compact_input,
    );
    let rows: Vec<&str> = TOKENS.iter().map(|r| r.key).chain(LAYOUT_KEYS).collect();
    for f in fields {
        let n = rows.iter().filter(|r| **r == f).count();
        assert_eq!(n, 1, "Theme.{f} has {n} whitelist rows");
    }
    for r in &rows {
        assert!(
            fields.contains(r),
            "whitelist row `{r}` names no Theme field"
        );
    }
}

#[test]
fn whitelist_classes() {
    let p = ok(&none_file(REQUIRED_NAMED));
    let mut t = p.theme;
    for row in TOKENS.iter().filter(|r| r.class == Class::Optional) {
        let want = (row.derive)(&p.theme);
        assert_eq!(
            *(row.field)(&mut t),
            want,
            "tokens.{} is not its derivation",
            row.key
        );
    }
    assert_eq!(
        p.theme.diff_add_text, p.theme.ok,
        "no tint → the ink carries the change"
    );
    assert_eq!(
        p.theme.badge_warn,
        p.theme
            .warn
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(p.theme.border_type, BorderType::Plain);
    assert_eq!(p.theme.inner_padding, 1);
    assert!(!p.theme.compact_input);

    for row in TOKENS.iter().filter(|r| r.class == Class::Required) {
        let missing: String = REQUIRED_NAMED
            .lines()
            .filter(|l| !l.trim_start().starts_with(&format!("{}:", row.key)))
            .collect::<Vec<_>>()
            .join("\n");
        let e = errs(&none_file(&format!("\n{missing}")));
        assert!(
            e.iter().any(|x| x.contains(&format!("tokens.{}", row.key))),
            "missing {} not named: {e:?}",
            row.key
        );
    }
}

#[test]
fn none_with_bg_but_missing_required() {
    let tokens: String = REQUIRED_NAMED
        .lines()
        .filter(|l| !l.trim_start().starts_with("warn:"))
        .collect::<Vec<_>>()
        .join("\n");
    let e = errs(&format!(
        "inherits: none\nassumed_bg: \"#101020\"\ntokens:\n{tokens}\n"
    ));
    assert!(e.iter().any(|x| x.contains("tokens.warn")), "{e:?}");
}

#[test]
fn inherits_fills_unset() {
    let p = ok("inherits: mur\ntokens:\n  accent: \"#ffb020\"\n");
    assert_eq!(
        p.theme.accent,
        Style::new().fg(Color::Rgb(0xff, 0xb0, 0x20))
    );
    let (mut got, mut base) = (p.theme, MUR);
    for row in TOKENS.iter().filter(|r| r.key != "accent") {
        assert_eq!(
            *(row.field)(&mut got),
            *(row.field)(&mut base),
            "tokens.{}",
            row.key
        );
    }
    assert_eq!(p.theme.border_type, MUR.border_type);
    assert_eq!(p.theme.inner_padding, MUR.inner_padding);
    assert_eq!(p.theme.compact_input, MUR.compact_input);
    assert_eq!(p.assumed_bg, Some(ASSUMED_BG_MUR));
}

#[test]
fn unknown_key_rejected() {
    for (file, want) in [
        ("inherits: mur\ncolours: {}\n", "unknown key `colours`"),
        (
            "inherits: mur\ntokens:\n  accnet: red\n",
            "unknown key `tokens.accnet`",
        ),
        (
            "inherits: mur\nlayout:\n  padding: 2\n",
            "unknown key `layout.padding`",
        ),
    ] {
        let e = errs(file);
        assert!(e.iter().any(|x| x.contains(want)), "{file:?} → {e:?}");
    }
}

#[test]
fn value_syntax() {
    let rgb = |r, g, b| Color::Rgb(r, g, b);
    assert_eq!(
        parse_style("bold #d97757 on #262626"),
        Ok(Style::new()
            .add_modifier(Modifier::BOLD)
            .fg(rgb(0xd9, 0x77, 0x57))
            .bg(rgb(0x26, 0x26, 0x26)))
    );
    assert_eq!(
        parse_style("on #16162e"),
        Ok(Style::new().bg(rgb(0x16, 0x16, 0x2e)))
    );
    assert_eq!(
        parse_style("dim cyan"),
        Ok(Style::new().add_modifier(Modifier::DIM).fg(Color::Cyan))
    );
    assert_eq!(parse_style("default"), Ok(Style::new().fg(Color::Reset)));
    assert_eq!(parse_style("none"), Ok(Style::new()));
    assert_eq!(parse_style("BOLD Light-Red"), parse_style("bold light-red"));
    for bad in ["#12345", "blod red", "red on", "red blue", ""] {
        assert!(parse_style(bad).is_err(), "{bad:?} parsed");
    }
}

#[test]
fn layout_override() {
    let p = ok(
        "inherits: mur\nlayout:\n  border_type: double\n  inner_padding: 2\n  compact_input: true\n",
    );
    assert_eq!(p.theme.border_type, BorderType::Double);
    assert_eq!(p.theme.inner_padding, 2);
    assert!(p.theme.compact_input);
    let e = errs("inherits: mur\nlayout:\n  inner_padding: 9\n");
    assert!(
        e.iter().any(|x| x.contains("layout.inner_padding")),
        "{e:?}"
    );
}

#[test]
fn assumed_bg_resolution() {
    let p = ok("inherits: mur\ntokens:\n  accent: \"#ffb020\"\n");
    assert_eq!(p.assumed_bg, Some(Color::Rgb(0x0b, 0x0b, 0x1a)));
    for base in ["ansi", "none"] {
        let tokens = if base == "none" {
            REQUIRED_NAMED.replace("cyan\n", "\"#ffb020\"\n")
        } else {
            "\n  accent: \"#ffb020\"".to_string()
        };
        let e = errs(&format!("inherits: {base}\ntokens:{tokens}\n"));
        assert!(
            e.iter().any(|x| x.contains("assumed_bg is required")),
            "{base}: {e:?}"
        );
    }
    assert_eq!(ok(&none_file(REQUIRED_NAMED)).assumed_bg, None);
}

#[test]
fn contrast_rejects_invisible_ink() {
    let e = errs("inherits: mur\ntokens:\n  warn: \"#0b0b1a\"\n");
    assert!(
        e.iter()
            .any(|x| x.starts_with("tokens.warn 1.0:1 on #0b0b1a (needs 4.5:1)")),
        "{e:?}"
    );
    // Passes undimmed; DIM blends it halfway into the navy and it fails.
    ok("inherits: mur\ntokens:\n  muted: \"#9a9ac4\"\n");
    let e = errs("inherits: mur\ntokens:\n  muted: \"dim #9a9ac4\"\n");
    assert!(e.iter().any(|x| x.starts_with("tokens.muted ")), "{e:?}");
}

#[test]
fn multiple_errors_first_plus_count() {
    let e = errs("inherits: mur\ntokens:\n  bogus: red\n  accent: blod\n  warn: red on\n");
    assert_eq!(e.len(), 3, "{e:?}");
    let row = summary(&e);
    assert!(row.starts_with(&e[0]), "{row}");
    assert!(row.ends_with("(+2 more)"), "{row}");
    assert_eq!(summary(&e[..1]), e[0], "one error carries no count");
}

#[test]
fn unknown_inherits_and_missing_inherits() {
    assert!(errs("tokens: {}\n")[0].contains("`inherits` is required"));
    assert!(errs("inherits: neon\n")[0].contains("unknown `inherits: neon`"));
    assert!(errs("inherits: [\n")[0].starts_with("not valid YAML"));
}

fn home_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let dir = skins_dir(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    home
}

#[test]
fn cannot_shadow_builtin() {
    let home = home_with(&[("mur.yaml", "inherits: none\n")]);
    assert_eq!(load(home.path(), "mur").unwrap_err(), LoadError::Missing);
    let (t, notice) = resolve_startup(home.path(), "mur");
    assert!(std::ptr::eq(t, &MUR) && notice.is_none());
    let listed = list(home.path());
    assert_eq!(listed[0].name, "mur");
    assert_eq!(listed[0].errors, vec![SHADOWS.to_string()]);
}

#[test]
fn startup_broken_falls_back_to_ansi() {
    let home = home_with(&[(
        "broken.yaml",
        "inherits: mur\ntokens:\n  warn: \"#0b0b1a\"\n",
    )]);
    let (t, notice) = resolve_startup(home.path(), "broken");
    assert!(std::ptr::eq(t, &super::super::ANSI));
    let Some(Notice::Rejected(row)) = notice else {
        panic!("{notice:?}")
    };
    assert!(
        row.starts_with("skin broken not loaded: tokens.warn"),
        "{row}"
    );
    assert!(row.ends_with("— using ansi"), "{row}");
    let (_, unknown) = resolve_startup(home.path(), "nope");
    assert_eq!(unknown, Some(Notice::Unknown));
}

#[test]
fn a_loaded_skin_is_named_by_its_stem() {
    let home = home_with(&[(
        "dusk.yaml",
        "inherits: mur\ntokens:\n  accent: \"#ff9e64\"\n",
    )]);
    let t = load(home.path(), "dusk").unwrap();
    assert_eq!(super::super::skin_name(t), "dusk");
    assert!(valid_names(home.path()).ends_with(", dusk"));
}

#[test]
fn names_outside_the_charset_never_become_paths() {
    let home = home_with(&[]);
    for bad in ["../config", "Dusk", "a/b", ""] {
        assert_eq!(
            load(home.path(), bad).unwrap_err(),
            LoadError::Missing,
            "{bad:?}"
        );
    }
}

#[test]
fn derivations_never_read_another_skin() {
    // Every optional row derives from the file's own tokens: a blank theme
    // derives only empty or modifier-only styles, never a colour.
    let p = ok(&none_file(REQUIRED_NAMED));
    for s in whitelist::styles(&p.theme) {
        assert!(!matches!(s.fg, Some(Color::Rgb(..))), "{s:?}");
    }
}

/// The file the spec and README show must be one that loads.
#[test]
fn the_documented_example_loads() {
    let p = ok(r##"
inherits: mur
assumed_bg: "#101020"
tokens:
  accent: "#ff9e64"
  muted: "#a0a0c8"
  surface_alt: "on #16162e"
  badge_warn: "bold #101020 on #f2c76a"
layout:
  border_type: rounded
"##);
    assert_eq!(p.assumed_bg, Some(Color::Rgb(0x10, 0x10, 0x20)));
    assert_eq!(p.theme.border_type, BorderType::Rounded);
}
