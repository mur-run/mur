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

// ── Parsing ────────────────────────────────────────────────────────

/// Extract all verifiable claims from document content.
pub fn parse_claims(content: &str, source_file: &str) -> Vec<Claim> {
    let mut claims = Vec::new();
    let mut seen = HashSet::new();

    for (line_idx, line) in content.lines().enumerate() {
        let line_number = line_idx + 1;

        // Skip pure comment / frontmatter lines
        if line.trim_start().starts_with('#') && !line.contains('`') {
            continue;
        }

        // 1. File path claims — from inline code or code blocks
        for path in extract_file_paths(line) {
            let key = format!("path:{}", path);
            if seen.insert(key) {
                claims.push(Claim {
                    kind: ClaimKind::FilePath(path.clone()),
                    source_file: source_file.to_string(),
                    line_number,
                    raw: path,
                });
            }
        }

        // 2. CLI command claims — `mur <subcommand>` references
        for cmd in extract_commands(line) {
            let key = format!("cmd:{}", cmd);
            if seen.insert(key) {
                claims.push(Claim {
                    kind: ClaimKind::Command(cmd.clone()),
                    source_file: source_file.to_string(),
                    line_number,
                    raw: cmd,
                });
            }
        }

        // 3. Code reference claims — `Struct::method`, `fn name`, etc.
        for code_ref in extract_code_refs(line) {
            let key = format!("code:{}", code_ref);
            if seen.insert(key) {
                claims.push(Claim {
                    kind: ClaimKind::CodeRef(code_ref.clone()),
                    source_file: source_file.to_string(),
                    line_number,
                    raw: code_ref,
                });
            }
        }
    }

    claims
}

/// Extract file/directory path references from a line.
fn extract_file_paths(line: &str) -> Vec<String> {
    let mut paths = Vec::new();

    // Match paths inside backticks: `some/path.rs` or `~/.mur/config.yaml`
    let backtick_re = Regex::new(r"`([^`]+)`").unwrap();
    for cap in backtick_re.captures_iter(line) {
        let text = &cap[1];
        if looks_like_path(text) {
            paths.push(text.to_string());
        }
    }

    // Match bare paths that look like project-relative file refs
    // e.g. src/main.rs, internal/api/handlers/
    let bare_re = Regex::new(
        r"(?:^|\s|[(\[])([a-zA-Z_][a-zA-Z0-9_\-.]*/[a-zA-Z0-9_\-./]*\.[a-zA-Z]{1,10})(?:\s|[)\].,;:]|$)",
    )
    .unwrap();
    for cap in bare_re.captures_iter(line) {
        let text = cap[1].to_string();
        if looks_like_path(&text) && !paths.contains(&text) {
            paths.push(text);
        }
    }

    paths
}

/// Heuristic: does this string look like a file path?
fn looks_like_path(s: &str) -> bool {
    // Must contain a slash or start with ~/
    if !s.contains('/') && !s.starts_with("~/") {
        return false;
    }
    // Must not be a URL
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("git://") {
        return false;
    }
    // Must not be a plain CLI flag or env var
    if s.starts_with("--") || s.starts_with("$") {
        return false;
    }
    // Must not contain shell metacharacters (it's a command, not a path)
    if s.contains('|') || s.contains('>') || s.contains('<') || s.contains('&') {
        return false;
    }
    // Must not look like a CLI invocation with flags
    if s.contains(" --") || s.contains(" -") {
        return false;
    }
    // Must not contain template variables
    if s.contains('{') || s.contains('}') {
        return false;
    }
    // Should have a file extension or end with /
    let has_ext = s.rsplit('/').next().is_some_and(|last| last.contains('.'));
    let is_dir = s.ends_with('/');
    let has_known_prefix = s.starts_with("~/")
        || s.starts_with("src/")
        || s.starts_with("internal/")
        || s.starts_with("cmd/")
        || s.starts_with("mur-")
        || s.starts_with("./")
        || s.starts_with("plans/")
        || s.starts_with("openspec/")
        || s.starts_with("research/")
        || s.starts_with("dashboard/");

    has_ext || is_dir || has_known_prefix
}

/// Subcommand words extracted from known commands (auto-populated from clap tree).
/// Used as a heuristic during doc parsing to distinguish subcommands from arguments.
fn subcommand_words() -> HashSet<String> {
    known_mur_commands()
        .iter()
        .flat_map(|cmd| {
            // "mur workflow schedule list" → ["workflow", "schedule", "list"]
            cmd.split_whitespace()
                .skip(1)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Extract `mur <subcommand>` command references.
fn extract_commands(line: &str) -> Vec<String> {
    let mut cmds = Vec::new();
    let known_words = subcommand_words();

    // Match: `mur <words>` or mur <words> in code blocks
    let re = Regex::new(r"(?:`|^|\s)(mur\s+[a-z][a-z0-9\- ]*)").unwrap();
    for cap in re.captures_iter(line) {
        let cmd = cap[1].trim().to_string();
        // Keep only known subcommand words after "mur"
        let parts: Vec<&str> = cmd
            .split_whitespace()
            .enumerate()
            .take_while(|(i, p)| {
                *i == 0 || (known_words.contains(*p) && !p.starts_with('-') && !p.starts_with('<'))
            })
            .map(|(_, p)| p)
            .collect();
        if parts.len() >= 2 {
            let normalized = parts.join(" ");
            if !cmds.contains(&normalized) {
                cmds.push(normalized);
            }
        }
    }

    // Also match: `cargo run -- <command>` references
    let cargo_re = Regex::new(r"cargo\s+run\s+--\s+(\w+)").unwrap();
    for cap in cargo_re.captures_iter(line) {
        let subcmd = format!("mur {}", &cap[1]);
        if !cmds.contains(&subcmd) {
            cmds.push(subcmd);
        }
    }

    cmds
}

/// Extract code-level references like `Struct::method` or `fn function_name`.
fn extract_code_refs(line: &str) -> Vec<String> {
    let mut refs = Vec::new();

    // Match Type::method or Type::method() inside backticks
    let method_re = Regex::new(r"`([A-Z][a-zA-Z0-9_]*::[a-z_][a-zA-Z0-9_]*)\(?`?").unwrap();
    for cap in method_re.captures_iter(line) {
        let r = cap[1].to_string();
        if !refs.contains(&r) {
            refs.push(r);
        }
    }

    // Match `fn function_name` or fn function_name()
    let fn_re = Regex::new(r"`?fn\s+([a-z_][a-z0-9_]*)`?").unwrap();
    for cap in fn_re.captures_iter(line) {
        let r = format!("fn {}", &cap[1]);
        if !refs.contains(&r) {
            refs.push(r);
        }
    }

    // Match `struct Name` or `enum Name`
    let struct_re = Regex::new(r"`((?:struct|enum|trait)\s+[A-Z][a-zA-Z0-9_]*)`").unwrap();
    for cap in struct_re.captures_iter(line) {
        let r = cap[1].to_string();
        if !refs.contains(&r) {
            refs.push(r);
        }
    }

    // Match `impl Trait for Type` patterns
    let impl_re = Regex::new(r"`(impl\s+\w+(?:<[^>]+>)?\s+for\s+\w+)`").unwrap();
    for cap in impl_re.captures_iter(line) {
        let r = cap[1].to_string();
        if !refs.contains(&r) {
            refs.push(r);
        }
    }

    // Match standalone `TypeName` that look like struct/enum refs (PascalCase, backtick-wrapped)
    let type_re = Regex::new(r"`([A-Z][a-zA-Z0-9]*(?:[A-Z][a-z]+)+)`").unwrap();
    for cap in type_re.captures_iter(line) {
        let name = &cap[1];
        // Skip common non-code words
        if !matches!(
            name,
            "README"
                | "CLAUDE"
                | "YAML"
                | "JSON"
                | "TOML"
                | "HTTP"
                | "HTTPS"
                | "UUID"
                | "UTC"
                // External crate types discussed in docs, not local code
                | "FromRequestParts"
                | "ConnectInfo"
        ) {
            let r = name.to_string();
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
    }

    refs
}

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

    // Also scan plans/ and openspec/ for .md files (one level deep)
    for dir_name in &["plans", "openspec", "research"] {
        let dir = project_root.join(dir_name);
        if dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "md") {
                    files.push(path);
                    continue;
                }
                if path.is_dir()
                    && let Ok(sub) = std::fs::read_dir(&path)
                {
                    for s in sub.flatten() {
                        let sp = s.path();
                        if sp.extension().is_some_and(|e| e == "md") {
                            files.push(sp);
                        }
                    }
                }
            }
        }
    }

    files
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_paths_backtick() {
        let paths =
            extract_file_paths("Check `src/main.rs` and `mur-common/src/config.rs` for details.");
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(paths.contains(&"mur-common/src/config.rs".to_string()));
    }

    #[test]
    fn test_extract_file_paths_home() {
        let paths = extract_file_paths("Data at `~/.mur/patterns/*.yaml`");
        assert!(paths.contains(&"~/.mur/patterns/*.yaml".to_string()));
    }

    #[test]
    fn test_extract_file_paths_ignores_urls() {
        let paths = extract_file_paths("Visit `https://example.com/path/to/file.html`");
        assert!(paths.is_empty());
    }

    #[test]
    fn test_extract_commands() {
        // Initialize known subcommand words so the parser can recognize them
        set_known_commands(
            ["mur learn", "mur learn extract", "mur stats", "mur verify"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let cmds = extract_commands("Run `mur learn extract --llm` to start");
        assert!(cmds.contains(&"mur learn extract".to_string()));
    }

    #[test]
    fn test_extract_commands_cargo_run() {
        let cmds = extract_commands("e.g. cargo run -- search \"swift testing\"");
        assert!(cmds.contains(&"mur search".to_string()));
    }

    #[test]
    fn test_extract_code_refs_method() {
        let refs = extract_code_refs("`Pattern::deref()` forwards to `KnowledgeBase`");
        assert!(refs.contains(&"Pattern::deref".to_string()));
        assert!(refs.contains(&"KnowledgeBase".to_string()));
    }

    #[test]
    fn test_extract_code_refs_fn() {
        let refs = extract_code_refs("calls `fn score_and_rank_generic()` for ranking");
        assert!(refs.contains(&"fn score_and_rank_generic".to_string()));
    }

    #[test]
    fn test_extract_code_refs_struct() {
        let refs = extract_code_refs("`struct ResearchReport` holds the output");
        assert!(refs.contains(&"struct ResearchReport".to_string()));
    }

    #[test]
    fn test_verify_command_known() {
        set_known_commands(
            ["mur stats", "mur verify", "mur learn", "mur learn extract"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        assert_eq!(verify_command("mur stats"), VerifyResult::Valid);
        assert_eq!(verify_command("mur verify"), VerifyResult::Valid);
    }

    #[test]
    fn test_verify_command_unknown() {
        assert!(matches!(
            verify_command("mur foobar"),
            VerifyResult::Invalid(_)
        ));
    }

    #[test]
    fn test_verify_command_parent() {
        // "mur learn" is a valid parent of "mur learn extract"
        set_known_commands(
            ["mur learn", "mur learn extract", "mur learn cross"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        assert_eq!(verify_command("mur learn"), VerifyResult::Valid);
    }

    #[test]
    fn test_verify_command_non_mur() {
        assert!(matches!(
            verify_command("cargo build"),
            VerifyResult::Skipped(_)
        ));
    }

    #[test]
    fn test_looks_like_path() {
        assert!(looks_like_path("src/main.rs"));
        assert!(looks_like_path("mur-core/src/lib.rs"));
        assert!(looks_like_path("~/.mur/config.yaml"));
        assert!(looks_like_path("internal/api/handlers/"));
        assert!(!looks_like_path("https://example.com/foo"));
        assert!(!looks_like_path("--flag-name"));
    }

    #[test]
    fn test_parse_claims_dedup() {
        let content = "See `src/main.rs` and also `src/main.rs` again.";
        let claims = parse_claims(content, "test.md");
        let path_claims: Vec<_> = claims
            .iter()
            .filter(|c| matches!(c.kind, ClaimKind::FilePath(_)))
            .collect();
        // Should only appear once
        assert_eq!(path_claims.len(), 1);
    }

    #[test]
    fn test_summary() {
        let results = vec![
            VerifiedClaim {
                claim: Claim {
                    kind: ClaimKind::FilePath("a.rs".into()),
                    source_file: "t.md".into(),
                    line_number: 1,
                    raw: "a.rs".into(),
                },
                result: VerifyResult::Valid,
            },
            VerifiedClaim {
                claim: Claim {
                    kind: ClaimKind::FilePath("b.rs".into()),
                    source_file: "t.md".into(),
                    line_number: 2,
                    raw: "b.rs".into(),
                },
                result: VerifyResult::Invalid("not found".into()),
            },
        ];
        let summary = VerifySummary::from_results(&results);
        assert_eq!(summary.total, 2);
        assert_eq!(summary.valid, 1);
        assert_eq!(summary.invalid, 1);
    }
}
