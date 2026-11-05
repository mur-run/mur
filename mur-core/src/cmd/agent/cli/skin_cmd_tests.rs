//! `/skin` guards from spec §7 that need an `App`.

use super::*;
use crate::cmd::agent::cli::persist::Session;
use crate::cmd::agent::cli::theme::{MUR, user::skins_dir};

fn app_on_mur(files: &[(&str, &str)]) -> (tempfile::TempDir, App) {
    let home = tempfile::tempdir().unwrap();
    let dir = skins_dir(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    let session = Session::create(home.path(), "a").unwrap();
    let app = App::new(home.path().to_path_buf(), "a".into(), session, &MUR);
    (home, app)
}

fn last(app: &App) -> &str {
    &app.messages.last().expect("a notice").text
}

const BROKEN: &str = "inherits: mur\ntokens:\n  warn: \"#0b0b1a\"\n  accent: blod\n";

#[test]
fn runtime_broken_keeps_current() {
    let (home, mut app) = app_on_mur(&[("broken.yaml", BROKEN)]);
    let config = home.path().join("config.yaml");
    let before = std::fs::read_to_string(&config).ok();
    handle(&mut app, Some("broken"));
    assert!(std::ptr::eq(app.theme, &MUR));
    let row = last(&app);
    assert!(row.starts_with("skin broken not loaded: "), "{row}");
    assert!(row.contains("(+"), "{row}");
    assert!(row.ends_with("— keeping mur"), "{row}");
    assert_eq!(
        std::fs::read_to_string(&config).ok(),
        before,
        "config was written"
    );
}

#[test]
fn a_valid_user_skin_applies_and_persists() {
    let (home, mut app) = app_on_mur(&[(
        "dusk.yaml",
        "inherits: mur\ntokens:\n  accent: \"#ff9e64\"\n",
    )]);
    handle(&mut app, Some("dusk"));
    assert_eq!(theme::skin_name(app.theme), "dusk");
    assert!(last(&app).starts_with("skin → dusk"), "{}", last(&app));
    let cfg = std::fs::read_to_string(home.path().join("config.yaml")).unwrap();
    assert!(cfg.contains("skin: dusk"), "{cfg}");
}

#[test]
fn skin_list_shows_user_skins() {
    let (_home, mut app) = app_on_mur(&[
        (
            "dusk.yaml",
            "inherits: mur\ntokens:\n  accent: \"#ff9e64\"\n",
        ),
        ("broken.yaml", BROKEN),
        ("notes.txt", "not a skin"),
    ]);
    handle(&mut app, None);
    let out = last(&app).to_string();
    assert!(
        out.starts_with("current skin: mur — built-in: ansi, light, mur, clay"),
        "{out}"
    );
    assert!(out.contains("\n  dusk"), "{out}");
    assert!(out.contains("\n  broken (invalid) — tokens."), "{out}");
    assert!(
        out.contains("\n      "),
        "further errors are indented: {out}"
    );
    assert!(!out.contains("notes"), "{out}");
}

#[test]
fn unknown_name_lists_valid_user_skins() {
    let (_home, mut app) = app_on_mur(&[("dusk.yaml", "inherits: mur\n")]);
    handle(&mut app, Some("neon"));
    assert!(std::ptr::eq(app.theme, &MUR));
    assert_eq!(
        last(&app),
        "unknown skin 'neon' — valid: ansi, light, mur, clay, dusk"
    );
}
