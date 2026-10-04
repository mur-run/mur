//! `ast_grep_search` — structural code search via a pinned ast-grep binary
//! (plan 2026-10-03 code-nav, phase 1).
//!
//! This module owns the parts that need no child process: the tool schema,
//! argument validation, path checks, binary resolution and argv
//! construction. Spawning, stream parsing and exit-code mapping are in
//! `ast_grep_run.rs`.
//!
//! Isolation (phase 0, item 2a): `-c <MUR-owned empty sgconfig>` is the
//! control that stops a repo's `sgconfig.yml` (and its `libraryPath`
//! dlopen vector) from loading; the MUR-owned cwd is a second layer only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Tool, ToolInputSchema, ToolParam};

pub const TOOL_NAME: &str = "ast_grep_search";

pub use mur_common::config::AST_GREP_PINNED_VERSION;

/// MUR-owned run directory (cwd + empty sgconfig) under mur home.
const RUNTIME_DIR: &str = "runtime";
const AST_GREP_DIR: &str = "ast-grep";
const SGCONFIG_FILE: &str = "sgconfig.yml";
/// MUR's config file under mur home (holds `search.ast_grep`).
const CONFIG_FILE: &str = "config.yaml";

/// Input-size bounds. Not config knobs: they guard argv size, not search cost.
pub const MAX_PATTERN_BYTES: usize = 4 * 1024;
pub const MAX_PATHS: usize = 64;
pub const MAX_GLOBS: usize = 64;
pub const MAX_GLOB_BYTES: usize = 512;
pub const MAX_LANG_BYTES: usize = 32;

/// `--strictness` values accepted by ast-grep 0.45.3 (phase 0, item 1).
pub const STRICTNESS_VALUES: &[&str] = &["cst", "smart", "ast", "relaxed", "signature", "template"];

/// Where the pinned binary must live: `<mur_home>/tools/ast-grep/<ver>/ast-grep[.exe]`.
/// Defined in `mur-common` so `mur code-nav setup` installs to the same path.
pub fn binary_path(mur_home: &Path) -> PathBuf {
    mur_common::config::ast_grep_binary_path(mur_home)
}

/// The pinned binary, or `None` ⇒ the tool is not registered. Never falls
/// back to `$PATH`: an unpinned ast-grep is not the one phase 0 verified.
pub fn resolve_binary(mur_home: &Path) -> Option<PathBuf> {
    let p = binary_path(mur_home);
    let meta = std::fs::metadata(&p).ok()?;
    if !meta.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(p)
}

/// MUR-owned cwd and empty sgconfig, recreated on every call so a tampered
/// file never survives into a run.
pub struct Isolation {
    pub cwd: PathBuf,
    pub sgconfig: PathBuf,
}

pub fn ensure_isolation(mur_home: &Path) -> std::io::Result<Isolation> {
    let cwd = mur_home.join(RUNTIME_DIR).join(AST_GREP_DIR);
    std::fs::create_dir_all(&cwd)?;
    let sgconfig = cwd.join(SGCONFIG_FILE);
    std::fs::write(&sgconfig, b"")?;
    Ok(Isolation { cwd, sgconfig })
}

/// Validated arguments for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchArgs {
    pub pattern: String,
    pub paths: Vec<PathBuf>,
    pub lang: Option<String>,
    pub globs: Vec<String>,
    pub strictness: Option<String>,
    pub max_results: Option<u32>,
    pub context: Option<u32>,
}

fn no_nul(field: &str, s: &str) -> Result<(), String> {
    if s.contains('\0') {
        return Err(format!("{field} must not contain NUL bytes"));
    }
    Ok(())
}

fn opt_u32(arguments: &Value, key: &str) -> Result<Option<u32>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| format!("{key} must be a non-negative integer")),
    }
}

fn str_array(arguments: &Value, key: &str) -> Result<Vec<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{key} must be an array of strings"))
            })
            .collect(),
        Some(_) => Err(format!("{key} must be an array of strings")),
    }
}

/// Each path must be absolute and exist; it is canonicalized so the argv
/// carries the resolved target, not a symlink the repo could repoint.
pub fn check_path(raw: &str) -> Result<PathBuf, String> {
    no_nul("path", raw)?;
    let p = Path::new(raw);
    if !p.is_absolute() {
        return Err(format!("path must be absolute: {raw}"));
    }
    std::fs::canonicalize(p).map_err(|e| format!("path not accessible: {raw}: {e}"))
}

pub fn parse_args(arguments: &Value) -> Result<SearchArgs, String> {
    let pattern = arguments
        .get("pattern")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or("pattern is required")?
        .to_owned();
    no_nul("pattern", &pattern)?;
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!("pattern exceeds {MAX_PATTERN_BYTES} bytes"));
    }

    let raw_paths = str_array(arguments, "paths")?;
    if raw_paths.is_empty() {
        return Err("paths is required (one or more absolute paths)".into());
    }
    if raw_paths.len() > MAX_PATHS {
        return Err(format!("at most {MAX_PATHS} paths"));
    }
    let paths = raw_paths
        .iter()
        .map(|p| check_path(p))
        .collect::<Result<Vec<_>, _>>()?;

    let lang = match arguments.get("lang").and_then(Value::as_str) {
        None => None,
        Some(l) => {
            let ok = !l.is_empty()
                && l.len() <= MAX_LANG_BYTES
                && l.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+#-_".contains(c));
            if !ok {
                return Err(format!("invalid lang: {l}"));
            }
            Some(l.to_owned())
        }
    };

    let globs = str_array(arguments, "globs")?;
    if globs.len() > MAX_GLOBS {
        return Err(format!("at most {MAX_GLOBS} globs"));
    }
    for g in &globs {
        no_nul("glob", g)?;
        if g.is_empty() || g.len() > MAX_GLOB_BYTES {
            return Err(format!("glob must be 1..={MAX_GLOB_BYTES} bytes"));
        }
    }

    let strictness = match arguments.get("strictness").and_then(Value::as_str) {
        None => None,
        Some(s) if STRICTNESS_VALUES.contains(&s) => Some(s.to_owned()),
        Some(s) => {
            return Err(format!(
                "strictness must be one of {STRICTNESS_VALUES:?}, got {s}"
            ));
        }
    };

    Ok(SearchArgs {
        pattern,
        paths,
        lang,
        globs,
        strictness,
        max_results: opt_u32(arguments, "max_results")?,
        context: opt_u32(arguments, "context")?,
    })
}

/// argv after the binary. Every value uses `--flag=value` and paths follow
/// `--`, so nothing the agent sends can be parsed as a flag (a pattern of
/// `-x` or a directory named `-x` stays data).
pub fn build_argv(
    args: &SearchArgs,
    sgconfig: &Path,
    context_lines: u32,
) -> Vec<std::ffi::OsString> {
    let mut v: Vec<std::ffi::OsString> = vec!["run".into()];
    let mut cfg = std::ffi::OsString::from("--config=");
    cfg.push(sgconfig);
    v.push(cfg);
    v.push(format!("--pattern={}", args.pattern).into());
    if let Some(l) = &args.lang {
        v.push(format!("--lang={l}").into());
    }
    if let Some(s) = &args.strictness {
        v.push(format!("--strictness={s}").into());
    }
    for g in &args.globs {
        v.push(format!("--globs={g}").into());
    }
    if context_lines > 0 {
        v.push(format!("--context={context_lines}").into());
    }
    v.push("--json=stream".into());
    v.push("--".into());
    v.extend(args.paths.iter().map(|p| p.as_os_str().to_owned()));
    v
}

fn param(t: &str, d: &str) -> ToolParam {
    ToolParam {
        param_type: t.into(),
        description: d.into(),
        default: None,
    }
}

pub fn tool() -> Tool {
    Tool {
        name: TOOL_NAME.into(),
        description: "Structural code search with ast-grep: match code by syntax-tree \
            pattern (e.g. `fn $NAME($$$ARGS)`), not text. Returns file, line, column, \
            matched text and metavariables. Results and output size are capped; a \
            capped result says truncated. Warnings from ast-grep (e.g. a pattern that \
            parsed with an ERROR node) are always returned — read them before \
            trusting an empty result."
            .into(),
        input_schema: ToolInputSchema {
            schema_type: "object".into(),
            properties: Some(BTreeMap::from([
                (
                    "pattern".into(),
                    param("string", "ast-grep pattern; $X matches one node, $$$X many"),
                ),
                (
                    "paths".into(),
                    param("array", "Absolute files or directories to search"),
                ),
                (
                    "lang".into(),
                    param(
                        "string",
                        "Language (e.g. rust, ts, python); omitted = inferred per file",
                    ),
                ),
                (
                    "globs".into(),
                    param("array", "Include globs; prefix with ! to exclude"),
                ),
                (
                    "strictness".into(),
                    param(
                        "string",
                        "cst | smart | ast | relaxed | signature | template (default smart)",
                    ),
                ),
                (
                    "max_results".into(),
                    param("integer", "Max matches; capped by config"),
                ),
                (
                    "context".into(),
                    param(
                        "integer",
                        "Context lines around each match; capped by config",
                    ),
                ),
            ])),
            required: Some(vec!["pattern".into(), "paths".into()]),
        },
    }
}

#[path = "ast_grep_run.rs"]
mod run;
pub use run::call;

#[path = "ast_grep_lang.rs"]
mod lang;

#[cfg(test)]
#[path = "ast_grep_tests.rs"]
mod tests;
