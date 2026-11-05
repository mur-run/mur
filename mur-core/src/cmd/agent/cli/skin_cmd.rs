//! `/skin` — list, or switch to, a built-in or user skin (spec §2, §6).
//! A user skin that fails to load is rejected whole: the current skin stays,
//! and `config.yaml` is written only after a successful load.

use super::app::App;
use super::theme::{self, user};
use super::welcome;
use std::io::IsTerminal;

pub(super) fn handle(app: &mut App, name: Option<&str>) {
    match name {
        None => app.push_system(listing(app)),
        Some(name) => switch(app, name),
    }
}

fn switch(app: &mut App, name: &str) {
    let loaded = match theme::builtin(name) {
        Some((t, _)) => Ok(t),
        None => user::load(&app.home, name),
    };
    match loaded {
        Ok(t) => {
            app.apply_theme(t);
            app.mascot_mode =
                welcome::resolve_mascot_mode(app.theme, std::io::stdout().is_terminal());
            match super::persist_skin(&app.home, name) {
                Ok(()) => app.push_system(skin_notice(name)),
                Err(e) => {
                    app.push_system(format!("{} · could not persist: {e}", skin_notice(name)))
                }
            }
        }
        Err(user::LoadError::Missing) => app.push_system(format!(
            "unknown skin '{name}' — valid: {}",
            user::valid_names(&app.home)
        )),
        Err(user::LoadError::Invalid(errs)) => app.push_warn(format!(
            "skin {name} not loaded: {} — keeping {}",
            user::summary(&errs),
            theme::skin_name(app.theme)
        )),
    }
}

/// `/skin` with no argument: the current skin, the built-ins, then every file
/// in `skins/` — a broken one as `name (invalid) — first error`, any further
/// errors indented under it.
pub(super) fn listing(app: &App) -> String {
    let mut out = format!(
        "current skin: {} — built-in: {}",
        theme::skin_name(app.theme),
        theme::SKIN_NAMES
    );
    let entries = user::list(&app.home);
    let dir = user::skins_dir(&app.home);
    if entries.is_empty() {
        out.push_str(&format!("\nuser skins: none in {}", dir.display()));
        return out;
    }
    out.push_str(&format!("\nuser skins ({}):", dir.display()));
    for e in entries {
        match e.errors.split_first() {
            None => out.push_str(&format!("\n  {}", e.name)),
            Some((first, rest)) => {
                out.push_str(&format!("\n  {} (invalid) — {first}", e.name));
                for more in rest {
                    out.push_str(&format!("\n      {more}"));
                }
            }
        }
    }
    out
}

/// The line an operator-typed `/skin <name>` prints (spec §2). Says what the
/// switch cannot do: rows already in scrollback keep the colours they were
/// painted with, and only a resume repaints them.
fn skin_notice(name: &str) -> String {
    format!("skin → {name} (earlier lines keep their colours; murmur --resume repaints them)")
}

#[cfg(test)]
#[path = "skin_cmd_tests.rs"]
mod tests;
