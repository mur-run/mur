//! Task 3.1: the pure setup planner. Flags + detected prerequisites in,
//! three tables (install, permissions, LSP risk) out. Reads nothing, spawns
//! nothing, writes nothing; the caller detects and the consent step applies.
//!
//! Tier source: the "LSP risk tiers" table in
//! `docs/superpowers/plans/2026-10-03-code-nav-astgrep-serena-plan.md`
//! (after #1698). Rust, Kotlin and TypeScript are High on item 17 evidence;
//! Go, Java and Swift by design (item 15); Ruby provisionally (fail closed).
//! Languages not yet probed against a hostile repo are refused, not guessed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use mur_common::config::AST_GREP_PINNED_VERSION;

/// Install-table names.
pub const AST_GREP: &str = "ast-grep";
pub const SERENA: &str = "serena";
/// serena-agent version every Phase 0/2 finding and the item 17 matrix were
/// taken against. Task 3.3 installs exactly this.
pub const SERENA_PIN: &str = "2.0.0.dev0";
/// Install-time prerequisite for serena (P3-D2). MUR does not install uv.
pub const UV: &str = "uv";
// `<mur_home>/tools/<name>/<version>/` — the managed-tools root, shared
// with the ast-grep resolver in `mur-mcp-server`.
use mur_common::config::MUR_TOOLS_DIR as TOOLS_DIR;

/// Languages serena may run, but setup refuses until each one has been
/// probed against a hostile repo (item 17: not yet tiered).
const UNTIERED: [&str; 7] = ["dart", "bash", "csharp", "fsharp", "scala", "elixir", "c#"];

/// Accepted spellings that are not a [`Lang::flag`]. `rust-full` is the
/// same server as `rust` in v1 (item 16); both are kept so a v2 shim can make
/// `rust` the degraded mode without changing the CLI.
const ALIASES: [(&str, Lang); 1] = [("rust-full", Lang::Rust)];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flags {
    pub with_serena: bool,
    pub no_ast_grep: bool,
    /// `--lsp <lang>`, repeatable. Non-empty replaces the default set.
    pub lsp: Vec<String>,
}

/// What the caller found on the host. Only `PATH` lookups for now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detected {
    pub on_path: BTreeSet<String>,
}

impl Detected {
    fn has(&self, tool: &str) -> bool {
        self.on_path.contains(tool)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Low,
    MediumContained,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lang {
    Python,
    Php,
    Lua,
    Cpp,
    Rust,
    Kotlin,
    TypeScript,
    Go,
    Java,
    Swift,
    Ruby,
}

impl Lang {
    /// Table order: Low, Medium, then High.
    pub const ALL: [Lang; 11] = [
        Lang::Python,
        Lang::Php,
        Lang::Lua,
        Lang::Cpp,
        Lang::Rust,
        Lang::Kotlin,
        Lang::TypeScript,
        Lang::Go,
        Lang::Java,
        Lang::Swift,
        Lang::Ruby,
    ];

    /// The `--lsp` value.
    pub fn flag(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::Php => "php",
            Lang::Lua => "lua",
            Lang::Cpp => "cpp",
            Lang::Rust => "rust",
            Lang::Kotlin => "kotlin",
            Lang::TypeScript => "typescript",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::Swift => "swift",
            Lang::Ruby => "ruby",
        }
    }

    pub fn tier(self) -> Tier {
        match self {
            Lang::Python | Lang::Php | Lang::Lua => Tier::Low,
            Lang::Cpp => Tier::MediumContained,
            Lang::Rust
            | Lang::Kotlin
            | Lang::TypeScript
            | Lang::Go
            | Lang::Java
            | Lang::Swift
            | Lang::Ruby => Tier::High,
        }
    }

    /// Tools the language server needs on `PATH` at run time (item 17
    /// acquisition table, finding 4). Lua, C/C++, Java and Kotlin are
    /// fetched by serena itself, so they need nothing up front.
    pub fn prerequisites(self) -> &'static [&'static str] {
        match self {
            Lang::Python => &["uvx"],
            Lang::TypeScript | Lang::Php => &["node", "npm"],
            Lang::Ruby => &["ruby", "gem"],
            Lang::Rust => &["rust-analyzer"],
            Lang::Go => &["go", "gopls"],
            Lang::Swift => &["sourcekit-lsp"],
            Lang::Lua | Lang::Cpp | Lang::Java | Lang::Kotlin => &[],
        }
    }

    /// Text the LSP-risk table must show for this row, where the plan
    /// mandates one.
    pub fn note(self) -> Option<&'static str> {
        Some(match self {
            Lang::Rust => {
                "`--lsp rust` ≡ `--lsp rust-full` in v1: opening a repo runs its build scripts \
                 (build.rs, proc-macros, cargo check)"
            }
            Lang::Kotlin => "opening a repo runs its build scripts (settings.gradle.kts)",
            Lang::TypeScript => {
                "opening a repo runs its own TypeScript (repo node_modules/typescript)"
            }
            Lang::Ruby => "provisional: bundle exec may evaluate the repo Gemfile",
            Lang::Cpp => "contained: --enable-config=false, no --query-driver",
            Lang::Lua => "safe only while LuaLS refuses untrusted plugins; re-test on pin bumps",
            Lang::Python | Lang::Php | Lang::Go | Lang::Java | Lang::Swift => return None,
        })
    }

    fn parse(raw: &str) -> Result<Lang> {
        let s = raw.trim().to_ascii_lowercase();
        if let Some(l) = Lang::ALL.into_iter().find(|l| l.flag() == s) {
            return Ok(l);
        }
        if let Some((_, l)) = ALIASES.iter().find(|(a, _)| *a == s) {
            return Ok(*l);
        }
        if UNTIERED.contains(&s.as_str()) {
            bail!(
                "--lsp {raw}: not offered by setup in v1 — its hostile-repo behaviour \
                 has not been probed, so it has no risk tier"
            );
        }
        let known: Vec<_> = Lang::ALL.iter().map(|l| l.flag()).collect();
        bail!(
            "--lsp {raw}: unknown language (known: {})",
            known.join(", ")
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRow {
    pub name: &'static str,
    pub version: &'static str,
    pub dir: PathBuf,
    /// Install-time prerequisite not on `PATH`; the row is then not applied.
    pub missing: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    /// Binary the language servers spawn.
    Spawn(String),
    /// Directory the agent must be able to read.
    Read(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspStatus {
    Enabled,
    /// Not requested; shown with the flag that turns it on.
    Skipped {
        enable_flag: String,
    },
    /// Requested but a prerequisite is missing. Never silent (finding 4).
    Disabled {
        missing: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspRow {
    pub lang: Lang,
    pub tier: Tier,
    pub status: LspStatus,
    pub note: Option<&'static str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub install: Vec<InstallRow>,
    pub permissions: Vec<Permission>,
    /// Every v1 language when serena is requested, empty otherwise.
    pub lsp: Vec<LspRow>,
}

impl Plan {
    /// One line per thing that was asked for and will not happen, e.g.
    /// `python disabled: uv not found on PATH`. Printed and recorded in the
    /// setup manifest (finding 4).
    pub fn disabled_lines(&self) -> Vec<String> {
        let tools = self.install.iter().filter_map(|r| {
            r.missing
                .map(|m| format!("{} disabled: {m} not found on PATH", r.name))
        });
        let langs = self.lsp.iter().filter_map(|r| match &r.status {
            LspStatus::Disabled { missing } => Some(format!(
                "{} disabled: {missing} not found on PATH",
                r.lang.flag()
            )),
            _ => None,
        });
        tools.chain(langs).collect()
    }
}

fn tool_dir(mur_home: &Path, name: &str, version: &str) -> PathBuf {
    mur_home.join(TOOLS_DIR).join(name).join(version)
}

/// Build the plan. Errors only for flags that cannot mean anything; missing
/// tools are rows, not errors.
pub fn plan(mur_home: &Path, flags: &Flags, detected: &Detected) -> Result<Plan> {
    if !flags.with_serena && !flags.lsp.is_empty() {
        bail!("--lsp only applies with --with-serena");
    }
    let requested: BTreeSet<Lang> = flags
        .lsp
        .iter()
        .map(|s| Lang::parse(s))
        .collect::<Result<_>>()?;

    let mut out = Plan::default();
    if !flags.no_ast_grep {
        out.install.push(InstallRow {
            name: AST_GREP,
            version: AST_GREP_PINNED_VERSION,
            dir: tool_dir(mur_home, AST_GREP, AST_GREP_PINNED_VERSION),
            missing: None,
        });
    }
    if !flags.with_serena {
        return Ok(out);
    }

    let serena_dir = tool_dir(mur_home, SERENA, SERENA_PIN);
    let serena_missing = (!detected.has(UV)).then_some(UV);
    out.install.push(InstallRow {
        name: SERENA,
        version: SERENA_PIN,
        dir: serena_dir.clone(),
        missing: serena_missing,
    });

    let wanted = |l: Lang| {
        if requested.is_empty() {
            l.tier() != Tier::High
        } else {
            requested.contains(&l)
        }
    };
    let mut spawn = BTreeSet::new();
    for lang in Lang::ALL {
        let status = if !wanted(lang) {
            LspStatus::Skipped {
                enable_flag: format!("--lsp {}", lang.flag()),
            }
        } else if let Some(m) = serena_missing {
            LspStatus::Disabled { missing: m.into() }
        } else if let Some(m) = lang.prerequisites().iter().find(|t| !detected.has(t)) {
            LspStatus::Disabled {
                missing: (*m).into(),
            }
        } else {
            spawn.extend(lang.prerequisites().iter().map(|t| t.to_string()));
            LspStatus::Enabled
        };
        out.lsp.push(LspRow {
            lang,
            tier: lang.tier(),
            status,
            note: lang.note(),
        });
    }

    if serena_missing.is_none() {
        out.permissions.push(Permission::Read(serena_dir));
        out.permissions
            .extend(spawn.into_iter().map(Permission::Spawn));
    }
    Ok(out)
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
