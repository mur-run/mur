//! Core verification engine — parses claims from documentation files
//! (file paths, CLI commands, code references) and checks them against
//! the actual project structure.

use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use regex::Regex;

// ── Claim types ────────────────────────────────────────────────────

/// A single verifiable claim extracted from a document.
#[derive(Debug, Clone, Serialize)]
pub struct Claim {
    pub kind: ClaimKind,
    pub source_file: String,
    pub line_number: usize,
    /// The raw text that was matched.
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// A file or directory path reference (e.g. `src/main.rs`, `~/.mur/patterns/`).
    FilePath(String),
    /// A CLI command reference (e.g. `mur research run`, `cargo build`).
    Command(String),
    /// A code-level reference (e.g. `YamlStore::default_store`, `Pattern::deref()`).
    CodeRef(String),
}

/// Result of verifying a single claim.
#[derive(Debug, Clone, Serialize)]
pub struct VerifiedClaim {
    pub claim: Claim,
    pub result: VerifyResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyResult {
    Valid,
    Invalid(String),
    Skipped(String),
}

/// Summary statistics for a verification run.
#[derive(Debug, Default, Serialize)]
pub struct VerifySummary {
    pub total: usize,
    pub valid: usize,
    pub invalid: usize,
    pub skipped: usize,
}

impl VerifySummary {
    pub fn from_results(results: &[VerifiedClaim]) -> Self {
        let mut s = Self {
            total: results.len(),
            ..Default::default()
        };
        for r in results {
            match &r.result {
                VerifyResult::Valid => s.valid += 1,
                VerifyResult::Invalid(_) => s.invalid += 1,
                VerifyResult::Skipped(_) => s.skipped += 1,
            }
        }
        s
    }
}

// ── Report output ──────────────────────────────────────────────────

/// A full verification report for serialization.
#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub struct VerifyReport {
    pub summary: VerifySummary,
    pub results: Vec<VerifiedClaim>,
}

#[allow(dead_code)]
impl VerifyReport {
    pub fn new(results: Vec<VerifiedClaim>) -> Self {
        let summary = VerifySummary::from_results(&results);
        Self { summary, results }
    }

    /// Serialize the report as JSON.
    pub fn to_json(&self) -> anyhow::Result<String> {
        serde_json::to_string_pretty(self).map_err(Into::into)
    }

    /// Render the report as Markdown.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str("# Verification Report\n\n");
        md.push_str(&format!(
            "**Total:** {} | **Valid:** {} | **Invalid:** {} | **Skipped:** {}\n\n",
            self.summary.total, self.summary.valid, self.summary.invalid, self.summary.skipped
        ));

        if self.summary.invalid == 0 {
            md.push_str("> All claims verified. Documentation is up to date.\n\n");
        } else {
            md.push_str(&format!(
                "> {} stale claim(s) found. Review and update documentation.\n\n",
                self.summary.invalid
            ));
        }

        // Group by source file
        let mut current_source = String::new();
        for r in &self.results {
            if r.claim.source_file != current_source {
                current_source = r.claim.source_file.clone();
                md.push_str(&format!("## {}\n\n", current_source));
                md.push_str("| Status | Line | Kind | Claim | Detail |\n");
                md.push_str("|--------|------|------|-------|--------|\n");
            }

            let (icon, detail) = match &r.result {
                VerifyResult::Valid => ("✅", String::new()),
                VerifyResult::Invalid(reason) => ("❌", reason.clone()),
                VerifyResult::Skipped(reason) => ("⏭️", reason.clone()),
            };

            let kind_label = match &r.claim.kind {
                ClaimKind::FilePath(_) => "path",
                ClaimKind::Command(_) => "cmd",
                ClaimKind::CodeRef(_) => "code",
            };

            md.push_str(&format!(
                "| {} | {} | {} | `{}` | {} |\n",
                icon, r.claim.line_number, kind_label, r.claim.raw, detail
            ));
        }

        md
    }
}

mod parse;

pub use parse::parse_claims;
#[cfg(test)]
use parse::{extract_code_refs, extract_commands, extract_file_paths, looks_like_path};

// ── Verification ───────────────────────────────────────────────────

/// Walk a `clap::Command` tree and collect all subcommand paths.
///
/// For example, given the `mur` CLI with subcommands `learn extract` and `learn cross`,
/// this produces: `{"mur learn", "mur learn extract", "mur learn cross", ...}`.
///
/// This replaces the old hardcoded list — any new subcommand added via clap derive
/// is automatically picked up.
pub fn collect_commands_from_clap(cmd: &clap::Command) -> HashSet<String> {
    let mut set = HashSet::new();
    fn walk(cmd: &clap::Command, prefix: &str, set: &mut HashSet<String>) {
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            // Skip clap's auto-generated "help" subcommands
            if name == "help" {
                continue;
            }
            let full = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix} {name}")
            };
            set.insert(full.clone());
            walk(sub, &full, set);
        }
    }
    walk(cmd, cmd.get_name(), &mut set);
    set
}

/// Global store for known commands, set from `main.rs` via [`set_known_commands`].
static KNOWN_COMMANDS: std::sync::RwLock<Option<HashSet<String>>> = std::sync::RwLock::new(None);

/// Register the known command set (call from `main.rs` after building the clap `Command`).
pub fn set_known_commands(commands: HashSet<String>) {
    *KNOWN_COMMANDS.write().unwrap() = Some(commands);
}

/// Get a clone of the known command set. Returns an empty set if never initialized.
fn known_mur_commands() -> HashSet<String> {
    KNOWN_COMMANDS.read().unwrap().clone().unwrap_or_default()
}

/// Verify a single claim against the project.
pub fn verify_claim(claim: &Claim, project_root: &Path) -> VerifyResult {
    match &claim.kind {
        ClaimKind::FilePath(path) => verify_file_path(path, project_root),
        ClaimKind::Command(cmd) => verify_command(cmd),
        ClaimKind::CodeRef(code_ref) => verify_code_ref(code_ref, project_root),
    }
}

/// Does the repo at `project_root` gitignore `rel_path`?
///
/// Matches the rules against the path string, so it works for a path that
/// does not exist and for one under a symlinked directory — both cases where
/// `git check-ignore` refuses. Not a git repo, or an unreadable ignore set,
/// answers `false` so the claim is still reported.
///
/// Ignore rules do not untrack: a file force-added under an ignored directory
/// (or a directory holding one) is tracked, so its absence is real breakage.
/// An unreadable index also answers `false`.
fn is_gitignored(project_root: &Path, rel_path: &str) -> bool {
    let Ok(repo) = git2::Repository::open(project_root) else {
        return false;
    };
    let rel = rel_path.trim_start_matches("./").trim_end_matches('/');
    if !repo.is_path_ignored(rel).unwrap_or(false) {
        return false;
    }
    let Ok(index) = repo.index() else {
        return false;
    };
    let dir_prefix = format!("{rel}/");
    !index.iter().any(|entry| {
        let path = String::from_utf8_lossy(&entry.path);
        path == rel || path.starts_with(&dir_prefix)
    })
}

/// Verify a file path claim.
fn verify_file_path(path_str: &str, project_root: &Path) -> VerifyResult {
    // Expand ~ to home dir
    let expanded = if let Some(stripped) = path_str.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            home.join(stripped)
        } else {
            return VerifyResult::Skipped("cannot resolve home directory".into());
        }
    } else if path_str.starts_with('/') {
        PathBuf::from(path_str)
    } else {
        // Relative to project root
        project_root.join(path_str)
    };

    // Strip trailing description after space (e.g. "src/main.rs — entry point")
    let check_path = expanded
        .to_str()
        .and_then(|s| s.split(" —").next())
        .and_then(|s| s.split(" —").next())
        .map(PathBuf::from)
        .unwrap_or(expanded);

    // Runtime paths under ~/.mur/ are expected to exist only at runtime
    if path_str.starts_with("~/.mur/")
        || path_str.starts_with("~/.claude/")
        || path_str.starts_with("~/.gemini/")
    {
        return VerifyResult::Skipped("runtime/tool config path".into());
    }

    // Paths containing glob wildcards — skip (e.g. patterns/*.yaml)
    if path_str.contains('*') {
        return VerifyResult::Skipped("glob pattern".into());
    }

    // External project references (absolute paths outside project root)
    if path_str.starts_with('/') && !path_str.starts_with(project_root.to_str().unwrap_or("")) {
        // Check if it actually exists
        if check_path.exists() {
            return VerifyResult::Valid;
        }
        return VerifyResult::Skipped("external path".into());
    }

    if check_path.exists() {
        return VerifyResult::Valid;
    }

    // Try with common suffixes stripped (e.g. trailing punctuation)
    let cleaned = path_str.trim_end_matches([',', '.', ')']);
    let cleaned_path = if cleaned.starts_with('/') {
        PathBuf::from(cleaned)
    } else {
        project_root.join(cleaned)
    };
    if cleaned_path.exists() {
        return VerifyResult::Valid;
    }

    // Try as a path relative to mur-core/src/ (module references like `capture/`, `store/`)
    if !path_str.starts_with('/') && !path_str.starts_with("~/") {
        let src_path = project_root.join("mur-core/src").join(path_str);
        if src_path.exists() {
            return VerifyResult::Valid;
        }
    }

    // `~/` paths name a location on the reader's machine, not in the repo.
    // Absence on this host proves nothing: `~/.config/autostart` is correct
    // on Linux yet missing on macOS/Windows. Existence still confirms it.
    if path_str.starts_with("~/") {
        return VerifyResult::Skipped("home path not present on this host".into());
    }

    // A missing path the repo gitignores is local-only by design (a linked-in
    // private docs clone, local specs): a clean checkout never has it, so its
    // absence is not staleness.
    if !path_str.starts_with('/') && is_gitignored(project_root, cleaned) {
        return VerifyResult::Skipped("gitignored local path".into());
    }

    VerifyResult::Invalid(format!("path not found: {}", check_path.display()))
}

/// Verify a CLI command claim against known subcommands.
fn verify_command(cmd: &str) -> VerifyResult {
    let known = known_mur_commands();

    // Check exact match first
    if known.contains(cmd) {
        return VerifyResult::Valid;
    }

    // Check if it's a prefix of a known command (e.g. "mur research" matches "mur research run")
    let is_parent = known.iter().any(|k| k.starts_with(cmd) && k != cmd);
    if is_parent {
        return VerifyResult::Valid;
    }

    // Non-mur commands (cargo, etc.) — skip
    if !cmd.starts_with("mur ") {
        return VerifyResult::Skipped("not a mur command".into());
    }

    VerifyResult::Invalid(format!("unknown subcommand: {cmd}"))
}

/// Verify a code reference by searching the source tree.
fn verify_code_ref(code_ref: &str, project_root: &Path) -> VerifyResult {
    // Build the grep pattern based on the reference type
    let pattern = if let Some(name) = code_ref.strip_prefix("fn ") {
        format!(r"fn\s+{name}\s*[<(]")
    } else if code_ref.starts_with("struct ")
        || code_ref.starts_with("enum ")
        || code_ref.starts_with("trait ")
    {
        let parts: Vec<&str> = code_ref.splitn(2, ' ').collect();
        let keyword = parts[0];
        let name = parts.get(1).unwrap_or(&"");
        format!(r"{keyword}\s+{name}\s*[<{{(]?")
    } else if code_ref.starts_with("impl ") {
        // impl Trait for Type
        code_ref.replace(' ', r"\s+")
    } else if code_ref.contains("::") {
        // Type::method — search for the method name in an impl block
        let parts: Vec<&str> = code_ref.splitn(2, "::").collect();
        let type_name = parts[0];
        let method = parts.get(1).unwrap_or(&"");
        // Search for both the type and the method existing in the codebase
        // First check if the type exists
        if !grep_source(
            project_root,
            &format!(r"(struct|enum|trait|type)\s+{type_name}\b"),
        ) {
            return VerifyResult::Invalid(format!("type `{type_name}` not found in source"));
        }
        // Then check if the method exists
        let method_pattern = format!(r"fn\s+{method}\s*[<(]");
        if grep_source(project_root, &method_pattern) {
            return VerifyResult::Valid;
        }
        return VerifyResult::Invalid(format!("method `{method}` not found in source"));
    } else {
        // Standalone PascalCase name — bare word match. Covers enum variants
        // (e.g. `StateChange`), which aren't declared via `enum StateChange`
        // themselves — they live inside their enum's braces.
        format!(r"\b{code_ref}\b")
    };

    if grep_source(project_root, &pattern) {
        VerifyResult::Valid
    } else {
        VerifyResult::Invalid(format!("code reference `{code_ref}` not found in source"))
    }
}

/// Search Rust source files for a regex pattern. Returns true if found.
fn grep_source(project_root: &Path, pattern: &str) -> bool {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => return false,
    };

    // Walk Rust source files
    walk_rust_files(project_root, &|content| re.is_match(content))
}

/// Walk all .rs files under project_root and apply a predicate to their content.
fn walk_rust_files(dir: &Path, predicate: &dyn Fn(&str) -> bool) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden dirs and target/
        if name_str.starts_with('.') || name_str == "target" {
            continue;
        }

        if path.is_dir() {
            if walk_rust_files(&path, predicate) {
                return true;
            }
        } else if name_str.ends_with(".rs")
            && let Ok(content) = std::fs::read_to_string(&path)
            && predicate(&content)
        {
            return true;
        }
    }

    false
}

// ── Public API ─────────────────────────────────────────────────────

/// Verify all claims in a single file.
pub fn verify_file(file_path: &Path, project_root: &Path) -> anyhow::Result<Vec<VerifiedClaim>> {
    let content = std::fs::read_to_string(file_path)?;
    let source = file_path
        .strip_prefix(project_root)
        .unwrap_or(file_path)
        .to_string_lossy()
        .to_string();

    let claims = parse_claims(&content, &source);
    let results: Vec<VerifiedClaim> = claims
        .into_iter()
        .map(|c| {
            let result = verify_claim(&c, project_root);
            VerifiedClaim { claim: c, result }
        })
        .collect();

    Ok(results)
}

/// Verify claims across multiple documentation files.
pub fn verify_docs(project_root: &Path) -> anyhow::Result<Vec<VerifiedClaim>> {
    let doc_files = find_doc_files(project_root);
    let mut all_results = Vec::new();

    for file_path in doc_files {
        let results = verify_file(&file_path, project_root)?;
        all_results.extend(results);
    }

    Ok(all_results)
}

/// Directories (relative to the project root) scanned for `.md` docs, with
/// how many directory levels below each to descend. OpenSpec nests specs as
/// `specs/<cap>/spec.md` and `changes/<id>/specs/<cap>/spec.md`, so it needs
/// more depth than the flat `plans/` and `research/` trees. Both the
/// conventional `openspec/` and the `docs/openspec/` location are scanned.
/// `docs/superpowers/plans` holds the plans; `docs/superpowers/specs` is
/// deliberately not scanned.
const DOC_SCAN_DIRS: &[(&str, usize)] = &[
    ("plans", 1),
    ("research", 1),
    ("openspec", 4),
    ("docs/openspec", 4),
    ("docs/superpowers/plans", 1),
];

/// Directory names never descended into: archived OpenSpec changes are a
/// historical record whose paths are expected to have gone stale.
const DOC_SCAN_SKIP_DIRS: &[&str] = &["archive"];

/// Push every `.md` file in `dir` and up to `max_depth` levels below it.
fn collect_md_files(dir: &Path, max_depth: usize, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            let skipped = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| DOC_SCAN_SKIP_DIRS.contains(&n));
            if max_depth > 0 && !skipped {
                collect_md_files(&path, max_depth - 1, files);
            }
        } else if path.extension().is_some_and(|e| e == "md") {
            files.push(path);
        }
    }
}

/// Find documentation files worth scanning.
fn find_doc_files(project_root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    // Check well-known documentation files
    let candidates = [
        "README.md",
        "CLAUDE.md",
        "CONTRIBUTING.md",
        "ARCHITECTURE.md",
        "docs/README.md",
    ];

    for name in &candidates {
        let p = project_root.join(name);
        if p.exists() {
            files.push(p);
        }
    }

    // Also scan doc directories for .md files, each down to its own depth.
    for (dir_name, max_depth) in DOC_SCAN_DIRS {
        collect_md_files(&project_root.join(dir_name), *max_depth, &mut files);
    }

    files
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
