//! Setup step: install the pinned `@playwright/mcp` into MUR's own tree, so
//! `record`/`replay` launch it with `node` and never resolve it through npx
//! at spawn time (`mur_browser::server` has the why).
//!
//! Runs here, at setup, because setup is the one moment outside any agent's
//! seal with the network in hand. Same consent rule as the Chromium download
//! (`setup::Consent`): the plan is printed first, then a literal `yes` or
//! `--yes`.

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Result, bail};
use mur_browser::server;

use super::setup::Consent;

/// Installs the package into a directory. Injected so tests exec nothing.
pub type Installer<'a> = &'a mut dyn FnMut(&Path) -> Result<()>;

/// Real installer: `npm install --ignore-scripts` into `dir`, then the same
/// registry-signature audit `mur agent mcp vendor` runs. An invalid signature
/// removes the tree, since a half-trusted install must not be launchable.
pub fn system_installer(dir: &Path) -> Result<()> {
    use crate::cmd::agent_mcp_vendor::{audit_signatures, npm_install};
    npm_install(dir, server::PACKAGE, server::VERSION)?;
    if let Some(a) = audit_signatures(dir)?
        && !a.invalid.is_empty()
    {
        let _ = std::fs::remove_dir_all(dir);
        bail!(
            "refusing the install: {} package(s) failed registry signature verification ({})",
            a.invalid.len(),
            a.invalid.join(", "),
        );
    }
    Ok(())
}

/// Ensure the pinned server is installed under `mur_home`. A no-op when it is.
pub fn ensure(
    consent: Consent,
    mur_home: &Path,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    install: Installer<'_>,
) -> Result<()> {
    let dir = server::install_dir(mur_home);
    if let Some(entry) = server::installed_entry(&dir) {
        writeln!(
            output,
            "  ✓ {}@{} installed at {}",
            server::PACKAGE,
            server::VERSION,
            entry.display()
        )?;
        return Ok(());
    }

    writeln!(output, "\nInstalling the browser MCP server does:")?;
    writeln!(
        output,
        "    run       npm install {}@{} --ignore-scripts",
        server::PACKAGE,
        server::VERSION
    )?;
    writeln!(output, "    into      {}", dir.display())?;
    writeln!(
        output,
        "    why       agents then launch it with `node`, with no registry fetch at run time"
    )?;
    if consent != Consent::Given && !crate::cmd::consent::literal_yes(input, output)? {
        writeln!(output, "  skipped — nothing was installed.")?;
        bail!("browser MCP server not installed; mur browser is not ready");
    }

    install(&dir)?;
    match server::installed_entry(&dir) {
        Some(entry) => {
            writeln!(output, "  ✓ installed {}", entry.display())?;
            Ok(())
        }
        None => {
            writeln!(
                output,
                "  ✗ install finished but no {}@{} entry in {}",
                server::PACKAGE,
                server::VERSION,
                dir.display()
            )?;
            bail!("browser MCP server install produced no launchable entry");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay down what a successful `npm install` leaves behind.
    fn lay_down(dir: &Path) {
        let pkg = dir.join("node_modules").join(server::PACKAGE);
        std::fs::create_dir_all(&pkg).unwrap();
        let manifest = serde_json::json!({
            "name": server::PACKAGE,
            "version": server::VERSION,
            "bin": { "playwright-mcp": "cli.js" },
        });
        std::fs::write(pkg.join("package.json"), manifest.to_string()).unwrap();
        std::fs::write(pkg.join("cli.js"), "").unwrap();
    }

    fn run(consent: Consent, answer: &str, home: &Path, ok: bool) -> (Result<()>, String, usize) {
        let mut out = Vec::new();
        let mut calls = 0;
        let mut install = |dir: &Path| {
            calls += 1;
            if ok {
                lay_down(dir);
            }
            Ok(())
        };
        let r = ensure(
            consent,
            home,
            &mut answer.as_bytes(),
            &mut out,
            &mut install,
        );
        (r, String::from_utf8(out).unwrap(), calls)
    }

    #[test]
    fn already_installed_asks_nothing_and_installs_nothing() {
        let t = tempfile::tempdir().unwrap();
        lay_down(&server::install_dir(t.path()));
        let (r, out, calls) = run(Consent::Ask, "", t.path(), true);
        assert!(r.is_ok());
        assert_eq!(calls, 0);
        assert!(out.contains("installed at"));
    }

    #[test]
    fn nothing_is_installed_without_a_literal_yes() {
        let t = tempfile::tempdir().unwrap();
        let (r, out, calls) = run(Consent::Ask, "y\n", t.path(), true);
        assert!(r.is_err());
        assert_eq!(calls, 0);
        assert!(
            out.contains("--ignore-scripts"),
            "plan printed first: {out}"
        );
    }

    #[test]
    fn yes_flag_installs_after_printing_the_plan() {
        let t = tempfile::tempdir().unwrap();
        let (r, out, calls) = run(Consent::Given, "", t.path(), true);
        assert!(r.is_ok(), "{out}");
        assert_eq!(calls, 1);
        assert!(out.find("npm install").unwrap() < out.find("✓ installed").unwrap());
    }

    #[test]
    fn an_install_that_leaves_no_entry_fails() {
        let t = tempfile::tempdir().unwrap();
        let (r, out, _) = run(Consent::Given, "", t.path(), false);
        assert!(r.is_err());
        assert!(out.contains("no launchable") || out.contains("no @playwright/mcp"));
    }
}
