//! The one launch command MUR lets serena use for a language server whose
//! default entry point is a shebang script (#1945).
//!
//! serena launches intelephense as `node_modules/.bin/intelephense --stdio`,
//! a `#!/usr/bin/env node` script under the agent-writable `SERENA_HOME`.
//! The macOS seal checks exec on the script's own path, so that never runs,
//! and granting exec there would let anything npm drops in run too. Instead
//! `mur code-nav setup` pre-installs the pinned package under MUR's tools
//! root and writes `ls_base_cmd: [node, <entry .js>]` into
//! `serena_config.yml`; the runtime preflight (C7) accepts an `ls_base_cmd`
//! only when it is exactly what [`canonical_base_cmd`] returns.
//!
//! Both sides call this function, so what setup writes and what C7 accepts
//! cannot drift. It lives here because `mur-agent-runtime` (C7) must not
//! depend on `mur-core` (setup).
//!
//! `--stdio` is deliberately absent: serena appends it itself
//! (`LanguageServerDependencyProviderSinglePath`, serena-agent 2.0.0.dev0),
//! so including it would pass it twice.

use std::path::{Path, PathBuf};

/// serena's `ls_specific_settings` key for intelephense.
pub const PHP_LS_ID: &str = "php";
/// Directory under `<mur_home>/tools/` the package is installed into.
pub const INTELEPHENSE_TOOL_DIR: &str = "intelephense";
/// npm package name.
pub const INTELEPHENSE_PACKAGE: &str = "intelephense";
/// serena's `DEFAULT_INTELEPHENSE_VERSION` at the serena pin. Bump together.
pub const INTELEPHENSE_PIN: &str = "1.14.4";
/// The interpreter, left bare so the sandbox seal resolves it against the
/// agent's spawn allowlist like any other `node` launch.
pub const NODE: &str = "node";
/// The package's `bin.intelephense` target, relative to the install dir.
const ENTRY_REL: [&str; 4] = [
    "node_modules",
    INTELEPHENSE_PACKAGE,
    "lib",
    "intelephense.js",
];

/// `<tools_dir>/intelephense/<pin>/`.
pub fn intelephense_dir(tools_dir: &Path) -> PathBuf {
    tools_dir.join(INTELEPHENSE_TOOL_DIR).join(INTELEPHENSE_PIN)
}

/// The `.js` entry node runs.
pub fn intelephense_entry(tools_dir: &Path) -> PathBuf {
    let mut p = intelephense_dir(tools_dir);
    p.extend(ENTRY_REL);
    p
}

/// The exact `ls_base_cmd` MUR writes for `ls_id`, or `None` when MUR never
/// overrides that server's base command (then C7 refuses any value). Also
/// `None` for a non-UTF-8 tools root, which YAML could not carry anyway.
pub fn canonical_base_cmd(ls_id: &str, tools_dir: &Path) -> Option<Vec<String>> {
    if ls_id != PHP_LS_ID {
        return None;
    }
    let entry = intelephense_entry(tools_dir);
    Some(vec![NODE.to_owned(), entry.to_str()?.to_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_gets_node_plus_entry_without_stdio() {
        let tools = Path::new("/h/.mur/tools");
        let cmd = canonical_base_cmd(PHP_LS_ID, tools).unwrap();
        assert_eq!(cmd.len(), 2, "serena appends --stdio itself: {cmd:?}");
        assert_eq!(cmd[0], NODE);
        assert_eq!(Path::new(&cmd[1]), intelephense_entry(tools));
        assert!(cmd[1].ends_with("intelephense.js"));
        assert!(Path::new(&cmd[1]).starts_with(intelephense_dir(tools)));
    }

    #[test]
    fn other_servers_have_no_canonical_base_cmd() {
        for id in ["python", "cpp", "rust", "typescript", "php_phpactor", ""] {
            assert!(canonical_base_cmd(id, Path::new("/t")).is_none(), "{id}");
        }
    }
}
