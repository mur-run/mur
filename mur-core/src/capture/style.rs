//! Code style analysis — detect coding conventions from source files.
//!
//! Scans source files in the current project to detect naming conventions,
//! indentation, line length, and import ordering. Generates a "code-style"
//! pattern from the analysis.

use std::collections::HashMap;
use std::path::Path;

/// Detected coding conventions from source analysis.
#[derive(Debug, Default)]
pub struct StyleAnalysis {
    pub naming: NamingConvention,
    pub indentation: IndentStyle,
    pub max_line_length: usize,
    pub import_ordering: ImportOrdering,
    pub files_scanned: usize,
    #[allow(dead_code)] // Used by binary
    pub language: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamingConvention {
    #[default]
    SnakeCase,
    CamelCase,
    PascalCase,
    Mixed,
}

impl std::fmt::Display for NamingConvention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NamingConvention::SnakeCase => write!(f, "snake_case"),
            NamingConvention::CamelCase => write!(f, "camelCase"),
            NamingConvention::PascalCase => write!(f, "PascalCase"),
            NamingConvention::Mixed => write!(f, "mixed"),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum IndentStyle {
    #[default]
    Spaces2,
    Spaces4,
    Tabs,
    Mixed,
}

impl std::fmt::Display for IndentStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndentStyle::Spaces2 => write!(f, "2 spaces"),
            IndentStyle::Spaces4 => write!(f, "4 spaces"),
            IndentStyle::Tabs => write!(f, "tabs"),
            IndentStyle::Mixed => write!(f, "mixed"),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum ImportOrdering {
    #[default]
    Grouped,
    Alphabetical,
    Unordered,
}

impl std::fmt::Display for ImportOrdering {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportOrdering::Grouped => write!(f, "grouped (stdlib, external, local)"),
            ImportOrdering::Alphabetical => write!(f, "alphabetical"),
            ImportOrdering::Unordered => write!(f, "unordered"),
        }
    }
}

/// File extensions to scan per language.
fn extensions_for_language(lang: &str) -> &[&str] {
    match lang.to_lowercase().as_str() {
        "rust" => &["rs"],
        "python" => &["py"],
        "javascript" | "typescript" => &["js", "ts", "jsx", "tsx"],
        "swift" => &["swift"],
        "go" => &["go"],
        "ruby" => &["rb"],
        "java" => &["java"],
        "kotlin" => &["kt", "kts"],
        "c" | "cpp" | "c++" => &["c", "cpp", "h", "hpp"],
        "php" => &["php"],
        _ => &[],
    }
}

/// Analyze coding style in a project directory.
pub fn analyze_style(project_dir: &Path, language: &str) -> StyleAnalysis {
    let extensions = extensions_for_language(language);
    if extensions.is_empty() {
        return StyleAnalysis {
            language: language.to_string(),
            ..Default::default()
        };
    }

    let mut files_scanned = 0usize;
    let mut indent_counts: HashMap<&str, usize> = HashMap::new();
    let mut naming_counts: HashMap<NamingConvention, usize> = HashMap::new();
    let mut max_line = 0usize;
    let mut import_lines: Vec<String> = Vec::new();

    // Walk source files (skip hidden dirs, target/, node_modules/, etc.)
    let source_files = collect_source_files(project_dir, extensions);

    for path in source_files.iter().take(50) {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        files_scanned += 1;

        for line in content.lines() {
            // Track max line length
            let len = line.len();
            if len > max_line {
                max_line = len;
            }

            // Detect indentation
            if line.starts_with('\t') {
                *indent_counts.entry("tabs").or_insert(0) += 1;
            } else if line.starts_with("    ") {
                *indent_counts.entry("spaces4").or_insert(0) += 1;
            } else if line.starts_with("  ") && !line.starts_with("    ") {
                *indent_counts.entry("spaces2").or_insert(0) += 1;
            }

            // Detect naming conventions from function/variable definitions
            detect_naming_in_line(line, language, &mut naming_counts);

            // Collect import lines
            if is_import_line(line, language) {
                import_lines.push(line.trim().to_string());
            }
        }
    }

    let indentation = determine_indent_style(&indent_counts);
    let naming = determine_naming_convention(&naming_counts);
    let import_ordering = determine_import_ordering(&import_lines, language);

    StyleAnalysis {
        naming,
        indentation,
        max_line_length: max_line,
        import_ordering,
        files_scanned,
        language: language.to_string(),
    }
}

/// Collect source files, skipping build/dependency directories.
fn collect_source_files(dir: &Path, extensions: &[&str]) -> Vec<std::path::PathBuf> {
    let skip_dirs = [
        "target",
        "node_modules",
        ".git",
        "build",
        "dist",
        "vendor",
        ".build",
        "__pycache__",
        ".venv",
        "venv",
    ];

    let mut files = Vec::new();
    collect_files_recursive(dir, extensions, &skip_dirs, &mut files, 0);
    files
}

fn collect_files_recursive(
    dir: &Path,
    extensions: &[&str],
    skip_dirs: &[&str],
    files: &mut Vec<std::path::PathBuf>,
    depth: usize,
) {
    if depth > 10 || files.len() >= 100 {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if !skip_dirs.contains(&name) {
                    collect_files_recursive(&path, extensions, skip_dirs, files, depth + 1);
                }
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
                && extensions.contains(&ext)
            {
                files.push(path);
            }
        }
    }
}

/// Detect naming convention from a source line.
fn detect_naming_in_line(
    line: &str,
    language: &str,
    counts: &mut HashMap<NamingConvention, usize>,
) {
    let trimmed = line.trim();

    // Language-specific function/variable definition patterns
    let identifiers: Vec<&str> = match language.to_lowercase().as_str() {
        "rust" => extract_identifiers_rust(trimmed),
        "python" => extract_identifiers_python(trimmed),
        "javascript" | "typescript" => extract_identifiers_js(trimmed),
        "swift" => extract_identifiers_swift(trimmed),
        "go" => extract_identifiers_go(trimmed),
        _ => vec![],
    };

    for id in identifiers {
        if id.len() < 2 {
            continue;
        }
        let convention = classify_identifier(id);
        *counts.entry(convention).or_insert(0) += 1;
    }
}

fn extract_identifiers_rust(line: &str) -> Vec<&str> {
    let mut ids = Vec::new();
    if let Some(rest) = line
        .strip_prefix("fn ")
        .or_else(|| line.strip_prefix("pub fn "))
        && let Some(name) = rest.split('(').next()
    {
        ids.push(name.trim());
    }
    if let Some(rest) = line
        .strip_prefix("let ")
        .or_else(|| line.strip_prefix("let mut "))
        && let Some(name) = rest.split([':', '=']).next()
    {
        ids.push(name.trim());
    }
    ids
}

fn extract_identifiers_python(line: &str) -> Vec<&str> {
    let mut ids = Vec::new();
    if let Some(rest) = line.strip_prefix("def ")
        && let Some(name) = rest.split('(').next()
    {
        ids.push(name.trim());
    }
    if !line.starts_with('#')
        && !line.starts_with("class ")
        && let Some(name) = line.split('=').next()
    {
        let name = name.trim();
        if !name.contains(' ') && !name.contains('[') && !name.contains('.') && !name.is_empty() {
            ids.push(name);
        }
    }
    ids
}

fn extract_identifiers_js(line: &str) -> Vec<&str> {
    let mut ids = Vec::new();
    for prefix in ["function ", "const ", "let ", "var "] {
        if let Some(rest) = line.strip_prefix(prefix)
            && let Some(name) = rest.split(['(', '=', ':', ' ']).next()
        {
            let name = name.trim();
            if !name.is_empty() {
                ids.push(name);
            }
        }
    }
    ids
}

fn extract_identifiers_swift(line: &str) -> Vec<&str> {
    let mut ids = Vec::new();
    for prefix in ["func ", "var ", "let "] {
        if let Some(rest) = line
            .strip_prefix(prefix)
            .or_else(|| line.strip_prefix(&format!("public {prefix}")))
            .or_else(|| line.strip_prefix(&format!("private {prefix}")))
            && let Some(name) = rest.split(['(', ':', '=', ' ']).next()
        {
            let name = name.trim();
            if !name.is_empty() {
                ids.push(name);
            }
        }
    }
    ids
}

fn extract_identifiers_go(line: &str) -> Vec<&str> {
    let mut ids = Vec::new();
    if let Some(rest) = line.strip_prefix("func ") {
        // Skip receiver: func (r *Receiver) Name(...)
        let rest = if rest.starts_with('(') {
            rest.split_once(") ").map(|x| x.1).unwrap_or(rest)
        } else {
            rest
        };
        if let Some(name) = rest.split('(').next() {
            ids.push(name.trim());
        }
    }
    ids
}

/// Classify an identifier as snake_case, camelCase, or PascalCase.
fn classify_identifier(id: &str) -> NamingConvention {
    if id.contains('_') {
        NamingConvention::SnakeCase
    } else if id.chars().next().is_some_and(|c| c.is_uppercase()) {
        NamingConvention::PascalCase
    } else if id.chars().any(|c| c.is_uppercase()) {
        NamingConvention::CamelCase
    } else {
        // all lowercase, no underscores — treat as snake_case
        NamingConvention::SnakeCase
    }
}

fn is_import_line(line: &str, language: &str) -> bool {
    let trimmed = line.trim();
    match language.to_lowercase().as_str() {
        "rust" => trimmed.starts_with("use "),
        "python" => trimmed.starts_with("import ") || trimmed.starts_with("from "),
        "javascript" | "typescript" => {
            trimmed.starts_with("import ") || trimmed.starts_with("require(")
        }
        "swift" => trimmed.starts_with("import "),
        "go" => trimmed.starts_with("import ") || trimmed == "import (",
        "java" | "kotlin" => trimmed.starts_with("import "),
        _ => false,
    }
}

fn determine_indent_style(counts: &HashMap<&str, usize>) -> IndentStyle {
    let tabs = counts.get("tabs").copied().unwrap_or(0);
    let s4 = counts.get("spaces4").copied().unwrap_or(0);
    let s2 = counts.get("spaces2").copied().unwrap_or(0);
    let total = tabs + s4 + s2;

    if total == 0 {
        return IndentStyle::Spaces4;
    }

    if tabs > s4 + s2 {
        IndentStyle::Tabs
    } else if s4 > s2 + tabs {
        IndentStyle::Spaces4
    } else if s2 > s4 + tabs {
        IndentStyle::Spaces2
    } else {
        IndentStyle::Mixed
    }
}

fn determine_naming_convention(counts: &HashMap<NamingConvention, usize>) -> NamingConvention {
    let total: usize = counts.values().sum();
    if total == 0 {
        return NamingConvention::SnakeCase;
    }

    let best = counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(conv, _)| *conv)
        .unwrap_or(NamingConvention::SnakeCase);

    let best_count = counts.get(&best).copied().unwrap_or(0);
    if best_count as f64 / total as f64 > 0.7 {
        best
    } else {
        NamingConvention::Mixed
    }
}

fn determine_import_ordering(import_lines: &[String], _language: &str) -> ImportOrdering {
    if import_lines.len() < 3 {
        return ImportOrdering::Unordered;
    }

    // Check if imports are alphabetically sorted
    let sorted = import_lines.windows(2).all(|w| w[0] <= w[1]);
    if sorted {
        return ImportOrdering::Alphabetical;
    }

    // Check for grouping (blank lines or std-first patterns)
    ImportOrdering::Grouped
}

/// Format analysis results as pattern content.
pub fn format_as_pattern_content(analysis: &StyleAnalysis) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "Naming convention: {} (detected from {} files)",
        analysis.naming, analysis.files_scanned
    ));
    parts.push(format!("Indentation: {}", analysis.indentation));
    parts.push(format!(
        "Max line length observed: {} chars",
        analysis.max_line_length
    ));
    parts.push(format!("Import ordering: {}", analysis.import_ordering));
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_snake_case() {
        assert_eq!(
            classify_identifier("my_function"),
            NamingConvention::SnakeCase
        );
        assert_eq!(classify_identifier("get_data"), NamingConvention::SnakeCase);
    }

    #[test]
    fn test_classify_camel_case() {
        assert_eq!(
            classify_identifier("myFunction"),
            NamingConvention::CamelCase
        );
        assert_eq!(classify_identifier("getData"), NamingConvention::CamelCase);
    }

    #[test]
    fn test_classify_pascal_case() {
        assert_eq!(
            classify_identifier("MyStruct"),
            NamingConvention::PascalCase
        );
        assert_eq!(classify_identifier("GetData"), NamingConvention::PascalCase);
    }

    #[test]
    fn test_indent_detection() {
        let mut counts = HashMap::new();
        counts.insert("spaces4", 100);
        counts.insert("tabs", 5);
        assert_eq!(determine_indent_style(&counts), IndentStyle::Spaces4);
    }

    #[test]
    fn test_indent_tabs() {
        let mut counts = HashMap::new();
        counts.insert("tabs", 100);
        counts.insert("spaces4", 5);
        assert_eq!(determine_indent_style(&counts), IndentStyle::Tabs);
    }

    #[test]
    fn test_is_import_rust() {
        assert!(is_import_line("use std::collections::HashMap;", "rust"));
        assert!(!is_import_line("let x = 5;", "rust"));
    }

    #[test]
    fn test_is_import_python() {
        assert!(is_import_line("import os", "python"));
        assert!(is_import_line("from pathlib import Path", "python"));
        assert!(!is_import_line("x = 5", "python"));
    }

    #[test]
    fn test_extract_identifiers_rust() {
        let ids = extract_identifiers_rust("fn my_function(x: i32) -> bool {");
        assert_eq!(ids, vec!["my_function"]);

        let ids = extract_identifiers_rust("pub fn getData() -> String {");
        assert_eq!(ids, vec!["getData"]);
    }

    #[test]
    fn test_analyze_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = analyze_style(tmp.path(), "rust");
        assert_eq!(result.files_scanned, 0);
    }

    #[test]
    fn test_analyze_rust_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("main.rs"),
            "use std::io;\n\nfn my_function() {\n    let my_var = 5;\n    println!(\"{}\", my_var);\n}\n",
        ).unwrap();
        let result = analyze_style(tmp.path(), "rust");
        assert_eq!(result.files_scanned, 1);
        assert_eq!(result.naming, NamingConvention::SnakeCase);
        assert_eq!(result.indentation, IndentStyle::Spaces4);
    }
}
