use regex::Regex;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub chunk_type: String,
    pub symbol: Option<String>,
    pub content: String,
    pub line_start: u32,
    pub line_end: u32,
}

const SMALL_FILE_LINES: usize = 50;
const WINDOW_SIZE: usize = 80;
const WINDOW_OVERLAP: usize = 20;

pub fn chunk_file(content: &str, language: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = content.lines().collect();

    if lines.len() < SMALL_FILE_LINES {
        return vec![Chunk {
            chunk_type: "file".to_string(),
            symbol: None,
            content: content.to_string(),
            line_start: 1,
            line_end: lines.len() as u32,
        }];
    }

    let boundary_re = boundary_regex(language);
    match boundary_re {
        Some(re) => split_by_boundaries(&lines, &re, language),
        None => sliding_window(&lines),
    }
}

fn boundary_regex(language: &str) -> Option<Regex> {
    let pattern = match language {
        "rust" => r"^(pub\s+)?(async\s+)?(fn|impl|struct|enum|mod|trait)\s",
        "typescript" | "javascript" | "vue" | "svelte" => {
            r"^(export\s+)?(default\s+)?(async\s+)?(function|class|const|let|interface|type|enum)\s"
        }
        "go" => r"^(func|type)\s",
        "python" => r"^(async\s+)?(def|class)\s",
        "swift" => {
            r"^(\s*)(public\s+|private\s+|internal\s+|open\s+|fileprivate\s+)?(static\s+)?(func|class|struct|enum|protocol|extension)\s"
        }
        "java" | "kotlin" => {
            r"^(\s*)(public|private|protected|internal)?\s*(static\s+)?(class|interface|fun|func|enum|object)\s"
        }
        "ruby" => r"^(\s*)(def|class|module)\s",
        "php" => {
            r"^(\s*)(public|private|protected)?\s*(static\s+)?(function|class|interface|trait)\s"
        }
        "c" | "cpp" => r"^(\w[\w\s\*]*)\s+(\w+)\s*\(",
        "elixir" => r"^(\s*)(def|defp|defmodule)\s",
        _ => return None,
    };
    Regex::new(pattern).ok()
}

fn split_by_boundaries(lines: &[&str], re: &Regex, language: &str) -> Vec<Chunk> {
    let mut boundaries: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if re.is_match(line) {
            boundaries.push(i);
        }
    }

    if boundaries.is_empty() {
        return sliding_window(lines);
    }

    let mut chunks = Vec::new();

    if boundaries[0] > 0 {
        let content: String = lines[..boundaries[0]].join("\n");
        if content.trim().len() > 10 {
            chunks.push(Chunk {
                chunk_type: "block".to_string(),
                symbol: None,
                content,
                line_start: 1,
                line_end: boundaries[0] as u32,
            });
        }
    }

    for (idx, &start) in boundaries.iter().enumerate() {
        let end = if idx + 1 < boundaries.len() {
            boundaries[idx + 1]
        } else {
            lines.len()
        };

        let chunk_content: String = lines[start..end].join("\n");
        let symbol = extract_symbol(lines[start], language);
        let chunk_type = detect_chunk_type(lines[start], language);

        chunks.push(Chunk {
            chunk_type,
            symbol,
            content: chunk_content,
            line_start: (start + 1) as u32,
            line_end: end as u32,
        });
    }

    chunks
}

fn extract_symbol(line: &str, language: &str) -> Option<String> {
    let pattern = match language {
        "rust" => r"(?:pub\s+)?(?:async\s+)?(?:fn|impl|struct|enum|mod|trait)\s+(\w+)",
        "typescript" | "javascript" | "vue" | "svelte" => {
            r"(?:export\s+)?(?:default\s+)?(?:async\s+)?(?:function|class|const|let|interface|type|enum)\s+(\w+)"
        }
        "go" => r"(?:func|type)\s+(?:\([^)]*\)\s+)?(\w+)",
        "python" => r"(?:async\s+)?(?:def|class)\s+(\w+)",
        "swift" => r"(?:func|class|struct|enum|protocol|extension)\s+(\w+)",
        "java" | "kotlin" => r"(?:class|interface|fun|func|enum|object)\s+(\w+)",
        "ruby" => r"(?:def|class|module)\s+(\w+)",
        "php" => r"(?:function|class|interface|trait)\s+(\w+)",
        "elixir" => r"(?:def|defp|defmodule)\s+(\w+)",
        _ => return None,
    };
    Regex::new(pattern)
        .ok()
        .and_then(|re| re.captures(line))
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn detect_chunk_type(line: &str, language: &str) -> String {
    let trimmed = line.trim();
    match language {
        "rust" => {
            if trimmed.contains("fn ") {
                "function"
            } else if trimmed.contains("impl ") {
                "impl"
            } else if trimmed.contains("struct ") {
                "struct"
            } else if trimmed.contains("enum ") {
                "enum"
            } else if trimmed.contains("trait ") {
                "trait"
            } else if trimmed.contains("mod ") {
                "module"
            } else {
                "block"
            }
        }
        "typescript" | "javascript" | "vue" | "svelte" => {
            if trimmed.contains("function ") || trimmed.contains("=> ") {
                "function"
            } else if trimmed.contains("class ") {
                "class"
            } else if trimmed.contains("interface ") || trimmed.contains("type ") {
                "type"
            } else {
                "block"
            }
        }
        "go" => {
            if trimmed.starts_with("func") {
                "function"
            } else {
                "type"
            }
        }
        "python" => {
            if trimmed.contains("class ") {
                "class"
            } else {
                "function"
            }
        }
        "swift" => {
            if trimmed.contains("func ") {
                "function"
            } else if trimmed.contains("class ") {
                "class"
            } else if trimmed.contains("struct ") {
                "struct"
            } else if trimmed.contains("enum ") {
                "enum"
            } else {
                "block"
            }
        }
        _ => {
            if trimmed.contains("class ") {
                "class"
            } else if trimmed.contains("func") || trimmed.contains("def ") {
                "function"
            } else {
                "block"
            }
        }
    }
    .to_string()
}

fn sliding_window(lines: &[&str]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < lines.len() {
        let end = (start + WINDOW_SIZE).min(lines.len());
        let content: String = lines[start..end].join("\n");
        chunks.push(Chunk {
            chunk_type: "block".to_string(),
            symbol: None,
            content,
            line_start: (start + 1) as u32,
            line_end: end as u32,
        });
        if end >= lines.len() {
            break;
        }
        start += WINDOW_SIZE - WINDOW_OVERLAP;
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_small_file_single_chunk() {
        let content = "fn main() {\n    println!(\"hello\");\n}";
        let chunks = chunk_file(content, "rust");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_type, "file");
    }

    #[test]
    fn test_rust_function_boundaries() {
        let mut lines = Vec::new();
        lines.push("use std::io;");
        lines.push("");
        lines.push("pub fn foo() {");
        lines.extend(std::iter::repeat_n("    let x = 1;", 25));
        lines.push("}");
        lines.push("");
        lines.push("pub async fn bar() {");
        lines.extend(std::iter::repeat_n("    let y = 2;", 25));
        lines.push("}");

        let content = lines.join("\n");
        let chunks = chunk_file(&content, "rust");
        assert!(chunks.len() >= 2);
        let symbols: Vec<_> = chunks.iter().filter_map(|c| c.symbol.as_deref()).collect();
        assert!(symbols.contains(&"foo"));
        assert!(symbols.contains(&"bar"));
    }

    #[test]
    fn test_sliding_window() {
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");
        let chunks = chunk_file(&content, "unknown_lang");
        assert!(chunks.len() > 1);
        assert_eq!(chunks[0].chunk_type, "block");
    }

    #[test]
    fn test_python_boundaries() {
        let mut lines = Vec::new();
        lines.push("import os");
        lines.push("");
        lines.push("class MyClass:");
        lines.extend(std::iter::repeat_n("    pass", 25));
        lines.push("");
        lines.push("def my_function():");
        lines.extend(std::iter::repeat_n("    pass", 25));

        let content = lines.join("\n");
        let chunks = chunk_file(&content, "python");
        let symbols: Vec<_> = chunks.iter().filter_map(|c| c.symbol.as_deref()).collect();
        assert!(symbols.contains(&"MyClass"));
        assert!(symbols.contains(&"my_function"));
    }
}
