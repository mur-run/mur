//! Pure preflight for a `kind: serena` MCP entry (code-nav 2.3).
//!
//! Reads `serena_config.yml` (and, for C8 only, the resolved project's
//! `project.yml`) and refuses unless every check C1–C9 holds. It runs at
//! every spawn, not only at startup, because serena rewrites its own global
//! config at runtime.
//!
//! Key names and defaults are taken from serena-agent 2.0.0.dev0:
//! - `serena/config/serena_config.py` — `trusted_project_path_patterns`
//!   defaults to `["**"]`, `web_dashboard` to `True`,
//!   `project_serena_folder_location` to `"$projectDir/.serena"`; the
//!   placeholders are `$projectDir` and `$projectFolderName`, any other
//!   `$name` is an error, and the result goes through `os.path.abspath`.
//! - `solidlsp/dependency_provider.py` — the launch command is `ls_base_cmd`,
//!   else `[ls_path]`, else the default; then `ls_args` (replacing the
//!   default args) or the default args; then `ls_extra_args` appended.
//! - `solidlsp/language_servers/clangd_language_server.py` — settings live
//!   under `ls_specific_settings["cpp"]`; default args are
//!   `["--background-index"]`; `compile_commands_dir` defaults to
//!   `".serena"` and is joined onto the repository root.

use std::path::{Component, Path, PathBuf};

use serde_yaml_ng::Value;

use super::{SERENA_TOOL_ALLOWLIST, SerenaPaths};

/// Keys in `serena_config.yml` / `project.yml`.
/// serena's loader raises when this key is missing (C9).
const KEY_PROJECTS: &str = "projects";
const KEY_TRUSTED: &str = "trusted_project_path_patterns";
const KEY_FOLDER: &str = "project_serena_folder_location";
const KEY_FIXED: &str = "fixed_tools";
const KEY_EXCLUDED: &str = "excluded_tools";
const KEY_OPTIONAL: &str = "included_optional_tools";
const KEY_DASHBOARD: &str = "web_dashboard";
const KEY_LS_SETTINGS: &str = "ls_specific_settings";
/// Settings that replace the language-server executable (arbitrary exec).
const LS_EXEC_KEYS: [&str; 2] = ["ls_path", "ls_base_cmd"];
const KEY_LS_ARGS: &str = "ls_args";
const KEY_LS_EXTRA_ARGS: &str = "ls_extra_args";
/// serena accepts both spellings for the project's language list.
const KEY_LANGUAGES: [&str; 2] = ["language_servers", "languages"];

/// serena's language-server id for clangd.
const CPP_LS_ID: &str = "cpp";
const CLANGD_DEFAULT_ARGS: [&str; 1] = ["--background-index"];
const CLANGD_REQUIRED_ARG: &str = "--enable-config=false";
const CLANGD_FORBIDDEN_ARG_PREFIX: &str = "--query-driver";
const KEY_COMPILE_COMMANDS_DIR: &str = "compile_commands_dir";
const CLANGD_DEFAULT_COMPILE_COMMANDS_DIR: &str = ".serena";

const SERENA_DEFAULT_FOLDER: &str = "$projectDir/.serena";
const PLACEHOLDER_PROJECT_DIR: &str = "projectDir";
const PLACEHOLDER_PROJECT_FOLDER_NAME: &str = "projectFolderName";
const PROJECT_FILE: &str = "project.yml";

const ABSENT: &str = "<absent>";

/// One variant per check. Every variant names the file, the key, what was
/// found and what was expected, so the refusal is fixable from the message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SerenaPreflightError {
    #[error("serena C1: {file}: `{key}` is {found}, expected {expected}")]
    C1Home {
        file: PathBuf,
        key: String,
        found: String,
        expected: String,
    },
    #[error("serena C2: {file}: `{key}` is {found}, expected {expected}")]
    C2Config {
        file: PathBuf,
        key: String,
        found: String,
        expected: String,
    },
    #[error("serena C3: {file}: `{key}` is {found}, expected {expected}")]
    C3Trust {
        file: PathBuf,
        key: String,
        found: String,
        expected: String,
    },
    #[error("serena C4: {file}: `{key}` is {found}, expected {expected}")]
    C4ProjectFolder {
        file: PathBuf,
        key: String,
        found: String,
        expected: String,
    },
    #[error("serena C5: {file}: `{key}` is {found}, expected {expected}")]
    C5Tools {
        file: PathBuf,
        key: String,
        found: String,
        expected: String,
    },
    #[error("serena C6: {file}: `{key}` is {found}, expected {expected}")]
    C6Dashboard {
        file: PathBuf,
        key: String,
        found: String,
        expected: String,
    },
    #[error("serena C7: {file}: `{key}` is {found}, expected {expected}")]
    C7LsExec {
        file: PathBuf,
        key: String,
        found: String,
        expected: String,
    },
    /// C8 also says *why* C/C++ was considered enabled and how to fix it:
    /// a missing `project.yml` triggers it on non-C++ repos, and "C8
    /// failed" alone would leave the user guessing. Boxed to keep the
    /// error small.
    #[error("serena C8: {file}: `{key}` is {found}, expected {expected} ({hint})")]
    C8Clangd {
        file: PathBuf,
        key: String,
        found: String,
        expected: String,
        hint: Box<str>,
    },
    /// serena refuses to load a config without `projects` and its own error
    /// does not say which file; this names it before serena ever starts.
    #[error("serena C9: {file}: `{key}` is {found}, expected {expected}")]
    C9Projects {
        file: PathBuf,
        key: String,
        found: String,
        expected: String,
    },
}

/// Build a variant from `(file, key, found, expected)`.
macro_rules! fail {
    ($v:ident, $file:expr, $key:expr, $found:expr, $expected:expr) => {
        Err(SerenaPreflightError::$v {
            file: $file.to_path_buf(),
            key: $key.to_string(),
            found: $found.to_string(),
            expected: $expected.to_string(),
        })
    };
}

/// Run C1–C9 against what is on disk now.
pub fn preflight(paths: &SerenaPaths, project_root: &Path) -> Result<(), SerenaPreflightError> {
    let cfg_file = paths.config_file.as_path();

    // C1
    if !paths.home.is_dir() {
        return fail!(
            C1Home,
            paths.home,
            "SERENA_HOME",
            "not a directory",
            "an existing directory"
        );
    }

    // C2
    let text = match std::fs::read_to_string(cfg_file) {
        Ok(t) => t,
        Err(e) => return fail!(C2Config, cfg_file, "<file>", e, "a readable YAML file"),
    };
    let cfg: Value = match serde_yaml_ng::from_str(&text) {
        Ok(v @ Value::Mapping(_)) => v,
        Ok(other) => return fail!(C2Config, cfg_file, "<root>", kind(&other), "a mapping"),
        Err(e) => return fail!(C2Config, cfg_file, "<root>", e, "valid YAML"),
    };

    // C3: a missing key means serena's default `["**"]`, i.e. trust all.
    match cfg.get(KEY_TRUSTED) {
        Some(Value::Sequence(s)) if s.is_empty() => {}
        Some(v) => return fail!(C3Trust, cfg_file, KEY_TRUSTED, show(v), "[]"),
        None => {
            return fail!(
                C3Trust,
                cfg_file,
                KEY_TRUSTED,
                "<absent> (serena default [\"**\"])",
                "[]"
            );
        }
    }

    // C4
    let folder = check_project_folder(&cfg, cfg_file, paths, project_root)?;

    // C5: missing/null optional lists are serena's empty default.
    let fixed = string_list(cfg.get(KEY_FIXED));
    let mut want: Vec<&str> = SERENA_TOOL_ALLOWLIST.to_vec();
    want.sort_unstable();
    let mut got: Vec<&str> = fixed
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect();
    got.sort_unstable();
    got.dedup();
    if fixed.is_none() || got != want {
        let found = cfg.get(KEY_FIXED).map_or(ABSENT.to_owned(), show);
        return fail!(C5Tools, cfg_file, KEY_FIXED, found, format!("{want:?}"));
    }
    for key in [KEY_EXCLUDED, KEY_OPTIONAL] {
        match cfg.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::Sequence(s)) if s.is_empty() => {}
            Some(v) => return fail!(C5Tools, cfg_file, key, show(v), "[] or absent"),
        }
    }

    // C6: serena defaults the dashboard to on.
    match cfg.get(KEY_DASHBOARD) {
        Some(Value::Bool(false)) => {}
        Some(v) => return fail!(C6Dashboard, cfg_file, KEY_DASHBOARD, show(v), "false"),
        None => {
            return fail!(
                C6Dashboard,
                cfg_file,
                KEY_DASHBOARD,
                "<absent> (serena default true)",
                "false"
            );
        }
    }

    // C7
    let ls = match cfg.get(KEY_LS_SETTINGS) {
        None | Some(Value::Null) => None,
        Some(Value::Mapping(m)) => Some(m),
        Some(v) => return fail!(C7LsExec, cfg_file, KEY_LS_SETTINGS, kind(v), "a mapping"),
    };
    for (lang, settings) in ls.into_iter().flatten() {
        for exec in LS_EXEC_KEYS {
            if let Some(v) = settings.get(exec) {
                let key = format!("{KEY_LS_SETTINGS}.{}.{exec}", show(lang));
                return fail!(C7LsExec, cfg_file, key, show(v), ABSENT);
            }
        }
    }

    // C8
    let cpp = ls.and_then(|m| m.get(CPP_LS_ID));
    if let Some(why) = cpp_enabled(&folder, cpp.is_some()) {
        check_clangd(cpp, cfg_file, paths, project_root)
            .map_err(|e| e.into_c8(why, &folder, paths))?;
    }

    // C9: serena iterates `projects or []`, so null is its empty list; a
    // missing key makes serena raise at load.
    match cfg.get(KEY_PROJECTS) {
        Some(Value::Sequence(_) | Value::Null) => Ok(()),
        Some(v) => fail!(
            C9Projects,
            cfg_file,
            KEY_PROJECTS,
            kind(v),
            "a list (may be empty)"
        ),
        None => fail!(
            C9Projects,
            cfg_file,
            KEY_PROJECTS,
            ABSENT,
            "a list (may be empty)"
        ),
    }
}

/// Why C8 applies. Each reason maps to the fixes that actually help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CppReason {
    GlobalSettings,
    ProjectListsCpp,
    LanguagesUnknown,
}

/// A C8 failure before the reason/fix are attached.
struct ClangdViolation {
    file: PathBuf,
    key: String,
    found: String,
    expected: String,
}

impl ClangdViolation {
    fn into_c8(self, why: CppReason, folder: &Path, paths: &SerenaPaths) -> SerenaPreflightError {
        let project_file = folder.join(PROJECT_FILE);
        let lock_down = format!(
            "set `{KEY_LS_SETTINGS}.{CPP_LS_ID}.{KEY_LS_EXTRA_ARGS}: [{CLANGD_REQUIRED_ARG}]` \
             and `{KEY_LS_SETTINGS}.{CPP_LS_ID}.{KEY_COMPILE_COMMANDS_DIR}` to a directory under {}",
            paths.projects_dir.display()
        );
        let (why, fix) = match why {
            CppReason::GlobalSettings => (
                format!(
                    "`{KEY_LS_SETTINGS}.{CPP_LS_ID}` is set in {}",
                    self.file.display()
                ),
                lock_down,
            ),
            CppReason::ProjectListsCpp => (
                format!("{} lists `{CPP_LS_ID}`", project_file.display()),
                lock_down,
            ),
            CppReason::LanguagesUnknown => (
                format!(
                    "{} is missing or has no readable language list, so serena \
                     auto-detects languages and C/C++ cannot be ruled out",
                    project_file.display()
                ),
                format!(
                    "create {} with a `language_servers` list that excludes `{CPP_LS_ID}`, or {lock_down}",
                    project_file.display()
                ),
            ),
        };
        SerenaPreflightError::C8Clangd {
            file: self.file,
            key: self.key,
            found: self.found,
            expected: self.expected,
            hint: format!("C/C++ checked because {why}; fix: {fix}").into(),
        }
    }
}

/// Build a [`ClangdViolation`] from `(file, key, found, expected)`.
macro_rules! clangd_fail {
    ($file:expr, $key:expr, $found:expr, $expected:expr) => {
        Err(ClangdViolation {
            file: $file.to_path_buf(),
            key: $key.to_string(),
            found: $found.to_string(),
            expected: $expected.to_string(),
        })
    };
}

/// C4: resolve `project_serena_folder_location` the way serena does and
/// require it under `projects_dir` *and* existing — otherwise serena falls
/// back to the repo's own `.serena/`.
fn check_project_folder(
    cfg: &Value,
    cfg_file: &Path,
    paths: &SerenaPaths,
    project_root: &Path,
) -> Result<PathBuf, SerenaPreflightError> {
    let expected = format!(
        "an existing directory under {}",
        paths.projects_dir.display()
    );
    let template = match cfg.get(KEY_FOLDER) {
        Some(Value::String(s)) => s.as_str(),
        Some(v) => return fail!(C4ProjectFolder, cfg_file, KEY_FOLDER, show(v), expected),
        None => {
            let found = format!("<absent> (serena default {SERENA_DEFAULT_FOLDER})");
            return fail!(C4ProjectFolder, cfg_file, KEY_FOLDER, found, expected);
        }
    };
    let resolved = match substitute(template, project_root) {
        Ok(p) => normalize(&p),
        Err(name) => {
            let found = format!("{template:?} (unknown placeholder ${name})");
            return fail!(C4ProjectFolder, cfg_file, KEY_FOLDER, found, expected);
        }
    };
    let found = format!("{template:?} -> {}", resolved.display());
    if !resolved.starts_with(normalize(&paths.projects_dir)) || !resolved.is_dir() {
        return fail!(C4ProjectFolder, cfg_file, KEY_FOLDER, found, expected);
    }
    // A symlink inside projects_dir must not lead back into the repo.
    match (resolved.canonicalize(), paths.projects_dir.canonicalize()) {
        (Ok(real), Ok(base)) if real.starts_with(&base) => Ok(resolved),
        _ => fail!(C4ProjectFolder, cfg_file, KEY_FOLDER, found, expected),
    }
}

/// Whether clangd may run. The language list lives in the resolved
/// folder's `project.yml`; when that file is missing serena auto-detects
/// the languages on activation, so C/C++ cannot be ruled out and C8
/// applies.
fn cpp_enabled(folder: &Path, global_cpp_settings: bool) -> Option<CppReason> {
    if global_cpp_settings {
        return Some(CppReason::GlobalSettings);
    }
    let Ok(text) = std::fs::read_to_string(folder.join(PROJECT_FILE)) else {
        return Some(CppReason::LanguagesUnknown);
    };
    let Ok(doc) = serde_yaml_ng::from_str::<Value>(&text) else {
        return Some(CppReason::LanguagesUnknown);
    };
    let lists_cpp = match KEY_LANGUAGES.iter().find_map(|k| doc.get(*k)) {
        Some(Value::Sequence(langs)) => langs.iter().any(|l| l.as_str() == Some(CPP_LS_ID)),
        Some(Value::String(l)) => l == CPP_LS_ID,
        _ => return Some(CppReason::LanguagesUnknown),
    };
    lists_cpp.then_some(CppReason::ProjectListsCpp)
}

/// C8: effective clangd args (serena's assembly rule) and the
/// compile-commands directory.
fn check_clangd(
    cpp: Option<&Value>,
    cfg_file: &Path,
    paths: &SerenaPaths,
    project_root: &Path,
) -> Result<(), ClangdViolation> {
    let key = |k: &str| format!("{KEY_LS_SETTINGS}.{CPP_LS_ID}.{k}");
    let get = |k: &str| cpp.and_then(|c| c.get(k));

    let mut args: Vec<String> = string_list(get(KEY_LS_ARGS)).unwrap_or_else(|| {
        CLANGD_DEFAULT_ARGS
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    });
    args.extend(string_list(get(KEY_LS_EXTRA_ARGS)).unwrap_or_default());
    let args_key = format!("{} + {}", key(KEY_LS_ARGS), key(KEY_LS_EXTRA_ARGS));
    if !args.iter().any(|a| a == CLANGD_REQUIRED_ARG) {
        let expected = format!("to contain {CLANGD_REQUIRED_ARG}");
        return clangd_fail!(cfg_file, args_key, format!("{args:?}"), expected);
    }
    if args
        .iter()
        .any(|a| a.starts_with(CLANGD_FORBIDDEN_ARG_PREFIX))
    {
        let expected = format!("no {CLANGD_FORBIDDEN_ARG_PREFIX}");
        return clangd_fail!(cfg_file, args_key, format!("{args:?}"), expected);
    }

    let raw = match get(KEY_COMPILE_COMMANDS_DIR) {
        None => CLANGD_DEFAULT_COMPILE_COMMANDS_DIR.to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => {
            let expected = format!("a path under {}", paths.projects_dir.display());
            return clangd_fail!(cfg_file, key(KEY_COMPILE_COMMANDS_DIR), show(v), expected);
        }
    };
    let resolved = normalize(&project_root.join(&raw));
    if !resolved.starts_with(normalize(&paths.projects_dir)) {
        let found = format!("{raw:?} -> {}", resolved.display());
        let expected = format!("a path under {}", paths.projects_dir.display());
        return clangd_fail!(cfg_file, key(KEY_COMPILE_COMMANDS_DIR), found, expected);
    }
    Ok(())
}

/// Replace serena's two placeholders; `Err(name)` for any other `$name`.
fn substitute(template: &str, project_root: &Path) -> Result<PathBuf, String> {
    let folder_name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root = project_root.to_string_lossy();
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        // serena: `\$([A-Za-z_]\w*)`.
        let starts = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        if !starts {
            out.push('$');
            rest = after;
            continue;
        }
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        match name.as_str() {
            PLACEHOLDER_PROJECT_DIR => out.push_str(&root),
            PLACEHOLDER_PROJECT_FOLDER_NAME => out.push_str(&folder_name),
            _ => return Err(name),
        }
        rest = &after[name.len()..];
    }
    out.push_str(rest);
    Ok(PathBuf::from(out))
}

/// Lexical `os.path.abspath`-style normalization: drop `.`, resolve `..`.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A YAML list of strings; `None` when absent, null or not such a list.
fn string_list(v: Option<&Value>) -> Option<Vec<String>> {
    match v? {
        Value::Sequence(s) => s.iter().map(|x| x.as_str().map(str::to_owned)).collect(),
        _ => None,
    }
}

fn show(v: &Value) -> String {
    serde_yaml_ng::to_string(v)
        .map(|s| s.trim_end().replace('\n', " "))
        .unwrap_or_else(|_| kind(v).to_owned())
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a bool",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Sequence(_) => "a list",
        Value::Mapping(_) => "a mapping",
        Value::Tagged(_) => "a tagged value",
    }
}

#[cfg(test)]
mod tests;
