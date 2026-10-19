//! The one launch command MUR lets serena use for a language server whose
//! default entry point is a shebang script (#1945; TypeScript #1981).
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

/// A node-run language server MUR pre-installs under `<tools>/<tool_dir>/
/// <pin>/` and launches as `[node, <entry .js>]` (#1945, #1981). One row
/// per server; setup's npm installer, the config writer and C7 all read
/// this table, so adding a server is adding a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedNodeLs {
    /// serena's `ls_specific_settings` key.
    pub ls_id: &'static str,
    /// Directory under `<mur_home>/tools/`.
    pub tool_dir: &'static str,
    /// `(npm package, exact version)`, installed together. The first is
    /// the server itself; its version names the install dir.
    pub packages: &'static [(&'static str, &'static str)],
    /// The server's `bin` target, relative to the install dir.
    pub entry_rel: &'static [&'static str],
}

impl ManagedNodeLs {
    /// The server package's pinned version.
    pub fn pin(&self) -> &'static str {
        self.packages[0].1
    }

    /// `<tools_dir>/<tool_dir>/<pin>/`.
    pub fn dir(&self, tools_dir: &Path) -> PathBuf {
        tools_dir.join(self.tool_dir).join(self.pin())
    }

    /// The `.js` / `.mjs` node runs.
    pub fn entry(&self, tools_dir: &Path) -> PathBuf {
        let mut p = self.dir(tools_dir);
        p.extend(self.entry_rel);
        p
    }
}

/// serena's `ls_specific_settings` key for intelephense.
pub const PHP_LS_ID: &str = "php";
/// Directory under `<mur_home>/tools/` the package is installed into.
pub const INTELEPHENSE_TOOL_DIR: &str = "intelephense";
/// npm package name.
pub const INTELEPHENSE_PACKAGE: &str = "intelephense";
/// serena's `DEFAULT_INTELEPHENSE_VERSION` at the serena pin. Bump together.
pub const INTELEPHENSE_PIN: &str = "1.14.4";

pub const INTELEPHENSE: ManagedNodeLs = ManagedNodeLs {
    ls_id: PHP_LS_ID,
    tool_dir: INTELEPHENSE_TOOL_DIR,
    packages: &[(INTELEPHENSE_PACKAGE, INTELEPHENSE_PIN)],
    entry_rel: &[
        "node_modules",
        INTELEPHENSE_PACKAGE,
        "lib",
        "intelephense.js",
    ],
};

/// serena's `ls_specific_settings` key for typescript-language-server.
pub const TS_LS_ID: &str = "typescript";
pub const TS_LS_PACKAGE: &str = "typescript-language-server";
/// serena's `DEFAULT_TYPESCRIPT_LANGUAGE_SERVER_VERSION` at the serena pin.
pub const TS_LS_PIN: &str = "5.1.3";
/// serena's `DEFAULT_TYPESCRIPT_VERSION` at the serena pin. The server
/// resolves `typescript` from its own `node_modules` when the repo has none.
pub const TYPESCRIPT_PIN: &str = "5.9.3";

pub const TYPESCRIPT: ManagedNodeLs = ManagedNodeLs {
    ls_id: TS_LS_ID,
    tool_dir: TS_LS_PACKAGE,
    packages: &[(TS_LS_PACKAGE, TS_LS_PIN), ("typescript", TYPESCRIPT_PIN)],
    entry_rel: &["node_modules", TS_LS_PACKAGE, "lib", "cli.mjs"],
};

/// serena's `ls_specific_settings` key for the Vue server (#1982).
pub const VUE_LS_ID: &str = "vue";
/// Directory under `<mur_home>/tools/` the Vue server is installed into.
pub const VUE_TOOL_DIR: &str = "vue-language-server";
pub const VUE_LS_PACKAGE: &str = "@vue/language-server";
/// serena's `vue_language_server_version` default at the serena pin.
pub const VUE_LS_PIN: &str = "3.1.5";

/// The Vue server and its companion TypeScript server, installed together
/// (#1982). Deliberately NOT in [`MANAGED`]: serena's Vue server has no
/// `ls_base_cmd` hook — it always execs `vue-lsp/node_modules/.bin/*` under
/// its own resources dir — so setup links that dir to this install instead,
/// and no `ls_base_cmd` is ever written (or accepted by C7) for `vue`. The
/// TypeScript pins are [`TYPESCRIPT`]'s, because serena reads both from
/// `ls_specific_settings.typescript`, so the npm cache serves both installs.
pub const VUE: ManagedNodeLs = ManagedNodeLs {
    ls_id: VUE_LS_ID,
    tool_dir: VUE_TOOL_DIR,
    packages: &[
        (VUE_LS_PACKAGE, VUE_LS_PIN),
        ("typescript", TYPESCRIPT_PIN),
        (TS_LS_PACKAGE, TS_LS_PIN),
    ],
    entry_rel: &[
        "node_modules",
        "@vue",
        "language-server",
        "bin",
        "vue-language-server.js",
    ],
};

/// Every server MUR launches through a canonical `ls_base_cmd`.
pub const MANAGED: [ManagedNodeLs; 2] = [INTELEPHENSE, TYPESCRIPT];

/// The managed server for serena key `ls_id`, if MUR manages it.
pub fn managed(ls_id: &str) -> Option<&'static ManagedNodeLs> {
    MANAGED.iter().find(|m| m.ls_id == ls_id)
}

/// The interpreter, left bare so the sandbox seal resolves it against the
/// agent's spawn allowlist like any other `node` launch.
pub const NODE: &str = "node";

/// `<tools_dir>/intelephense/<pin>/`.
pub fn intelephense_dir(tools_dir: &Path) -> PathBuf {
    INTELEPHENSE.dir(tools_dir)
}

/// The `.js` entry node runs.
pub fn intelephense_entry(tools_dir: &Path) -> PathBuf {
    INTELEPHENSE.entry(tools_dir)
}

/// The exact `ls_base_cmd` MUR writes for `ls_id`, or `None` when MUR never
/// overrides that server's base command (then C7 refuses any value). Also
/// `None` for a non-UTF-8 tools root, which YAML could not carry anyway.
pub fn canonical_base_cmd(ls_id: &str, tools_dir: &Path) -> Option<Vec<String>> {
    let entry = managed(ls_id)?.entry(tools_dir);
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
    fn typescript_gets_node_plus_cli_mjs_without_stdio() {
        let tools = Path::new("/h/.mur/tools");
        let cmd = canonical_base_cmd(TS_LS_ID, tools).unwrap();
        assert_eq!(cmd.len(), 2, "serena appends --stdio itself: {cmd:?}");
        assert_eq!(cmd[0], NODE);
        assert_eq!(
            Path::new(&cmd[1]),
            Path::new(
                "/h/.mur/tools/typescript-language-server/5.1.3/node_modules/typescript-language-server/lib/cli.mjs"
            )
        );
    }

    /// serena's TypeScript server needs `typescript` next to it; both are
    /// pinned to serena's DEFAULT_* at the serena pin.
    #[test]
    fn typescript_installs_both_pinned_packages() {
        assert_eq!(
            TYPESCRIPT.packages,
            [
                ("typescript-language-server", "5.1.3"),
                ("typescript", "5.9.3")
            ]
        );
    }

    #[test]
    fn every_managed_server_is_found_by_its_ls_id() {
        for ls in MANAGED {
            assert_eq!(managed(ls.ls_id).map(|m| m.tool_dir), Some(ls.tool_dir));
        }
    }

    #[test]
    fn other_servers_have_no_canonical_base_cmd() {
        for id in [
            "python",
            "cpp",
            "rust",
            "typescript_vts",
            "php_phpactor",
            VUE_LS_ID,
            "",
        ] {
            assert!(canonical_base_cmd(id, Path::new("/t")).is_none(), "{id}");
        }
    }
}
