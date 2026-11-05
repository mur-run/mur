//! User skins: `<mur home>/skins/<name>.yaml`, written by the operator and by
//! nothing else. A file is read when its name is resolved and when `/skin`
//! lists — no watcher, no hot reload — and a skin that passes is leaked to
//! `&'static` so every frame costs what a built-in costs. Ceiling, recorded
//! in the spec: one `Theme` (~1 KB) per successful load, bounded by how often
//! an operator types `/skin`.

pub mod parse;
pub mod value;
pub mod whitelist;

use super::{ANSI, Theme};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Directory under the mur home that holds user skin files.
pub const SKINS_DIR: &str = "skins";
/// File extension of a user skin.
pub const SKIN_EXT: &str = "yaml";
/// Why a built-in's name is never loaded from a file.
pub const SHADOWS: &str = "shadows a built-in skin";

/// Every user theme loaded this process, newest last, so `skin_name` can name
/// a leaked theme by address the way it names a built-in.
static LOADED: Mutex<Vec<(&'static str, &'static Theme)>> = Mutex::new(Vec::new());

pub fn skins_dir(home: &Path) -> PathBuf {
    home.join(SKINS_DIR)
}

/// `[a-z0-9-]+` — the only stems that are ever turned into a path.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Why a name did not resolve to a user skin.
#[derive(Debug, PartialEq)]
pub enum LoadError {
    /// No such file (or a name that could never be one).
    Missing,
    /// The file exists and was rejected whole; first problem first.
    Invalid(Vec<String>),
}

/// Load `<home>/skins/<name>.yaml`. A built-in name is never read from disk.
pub fn load(home: &Path, name: &str) -> Result<&'static Theme, LoadError> {
    if !valid_name(name) || super::builtin(name).is_some() {
        return Err(LoadError::Missing);
    }
    let path = skins_dir(home).join(format!("{name}.{SKIN_EXT}"));
    let text = std::fs::read_to_string(&path).map_err(|_| LoadError::Missing)?;
    let parsed = parse::parse(&text).map_err(LoadError::Invalid)?;
    let theme: &'static Theme = Box::leak(Box::new(parsed.theme));
    let name: &'static str = Box::leak(name.to_string().into_boxed_str());
    if let Ok(mut loaded) = LOADED.lock() {
        loaded.push((name, theme));
    }
    Ok(theme)
}

/// The name a loaded user theme was selected by.
pub fn loaded_name(theme: &'static Theme) -> Option<&'static str> {
    let loaded = LOADED.lock().ok()?;
    loaded
        .iter()
        .rev()
        .find(|(_, t)| std::ptr::eq(*t, theme))
        .map(|(n, _)| *n)
}

/// One file in `skins/`, as the `/skin` listing shows it.
#[derive(Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Empty when the file loads.
    pub errors: Vec<String>,
}

/// Every `*.yaml` in `skins/`, sorted by name, each validated in full. A
/// stem outside `[a-z0-9-]+` is listed as invalid rather than skipped, so a
/// file the operator put there never silently disappears.
pub fn list(home: &Path) -> Vec<Entry> {
    let Ok(dir) = std::fs::read_dir(skins_dir(home)) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = dir
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()? != SKIN_EXT {
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().into_owned();
            let errors = if super::builtin(&name).is_some() {
                vec![SHADOWS.to_string()]
            } else if !valid_name(&name) {
                vec!["a skin name is lowercase letters, digits and `-`".to_string()]
            } else {
                match std::fs::read_to_string(&path) {
                    Ok(text) => parse::parse(&text).err().unwrap_or_default(),
                    Err(e) => vec![format!("unreadable: {e}")],
                }
            };
            Some(Entry { name, errors })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `first (+N more)` — the one-row form of a rejection.
pub fn summary(errors: &[String]) -> String {
    let first = errors.first().map(String::as_str).unwrap_or("rejected");
    match errors.len() {
        0 | 1 => first.to_string(),
        n => format!("{first} (+{} more)", n - 1),
    }
}

/// What resolving a skin name produced besides the theme.
#[derive(Debug, PartialEq)]
pub enum Notice {
    /// The name is neither a built-in nor a file in `skins/`.
    Unknown,
    /// The file exists and was rejected; the `warn` row to print.
    Rejected(String),
}

/// The theme a session starts on. A broken file never half-applies: the
/// session starts on `ansi` and says why.
pub fn resolve_startup(home: &Path, name: &str) -> (&'static Theme, Option<Notice>) {
    if let Some((theme, _)) = super::builtin(name) {
        return (theme, None);
    }
    match load(home, name) {
        Ok(theme) => (theme, None),
        Err(LoadError::Missing) => (&ANSI, Some(Notice::Unknown)),
        Err(LoadError::Invalid(errs)) => (
            &ANSI,
            Some(Notice::Rejected(format!(
                "skin {name} not loaded: {} — using ansi",
                summary(&errs)
            ))),
        ),
    }
}

/// Built-in names, then every user skin that loads — the "valid:" list.
pub fn valid_names(home: &Path) -> String {
    let mut names = super::SKIN_NAMES.to_string();
    for e in list(home).into_iter().filter(|e| e.errors.is_empty()) {
        names.push_str(", ");
        names.push_str(&e.name);
    }
    names
}

#[cfg(test)]
mod tests;
