use std::fs;
use std::path::{Path, PathBuf};

pub struct ScannedFile {
    pub relative_path: String,
    pub content: String,
    pub language: String,
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "build",
    "dist",
    "__pycache__",
    ".venv",
    "vendor",
    ".next",
    ".nuxt",
    ".turbo",
    ".svelte-kit",
    "Pods",
    ".build",
    "DerivedData",
];

fn language_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "go" => Some("go"),
        "py" => Some("python"),
        "swift" => Some("swift"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "cs" => Some("csharp"),
        "scala" => Some("scala"),
        "ex" | "exs" => Some("elixir"),
        "zig" => Some("zig"),
        "lua" => Some("lua"),
        "sh" | "bash" | "zsh" => Some("shell"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "json" => Some("json"),
        "md" => Some("markdown"),
        "sql" => Some("sql"),
        "proto" => Some("protobuf"),
        "tf" => Some("terraform"),
        "vue" => Some("vue"),
        "svelte" => Some("svelte"),
        _ => None,
    }
}

pub fn scan_project(project_path: &Path) -> Vec<ScannedFile> {
    let gitignore = load_gitignore(project_path);
    let mut files = Vec::new();
    walk_dir(project_path, project_path, &gitignore, &mut files);
    files
}

fn walk_dir(root: &Path, dir: &Path, gitignore_patterns: &[String], out: &mut Vec<ScannedFile>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            if SKIP_DIRS.iter().any(|&s| name == s) {
                continue;
            }
            let rel = relative_path(root, &path);
            if is_gitignored(&rel, gitignore_patterns, true) {
                continue;
            }
            walk_dir(root, &path, gitignore_patterns, out);
        } else if path.is_file() {
            let rel = relative_path(root, &path);
            if is_gitignored(&rel, gitignore_patterns, false) {
                continue;
            }

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let language = match language_for_ext(ext) {
                Some(lang) => lang,
                None => continue,
            };

            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if metadata.len() > 512 * 1024 {
                continue;
            }

            let content = match fs::read(&path) {
                Ok(bytes) => {
                    let check_len = bytes.len().min(512);
                    if bytes[..check_len].contains(&0) {
                        continue;
                    }
                    match String::from_utf8(bytes) {
                        Ok(s) => s,
                        Err(_) => continue,
                    }
                }
                Err(_) => continue,
            };

            out.push(ScannedFile {
                relative_path: rel,
                content,
                language: language.to_string(),
            });
        }
    }
}

fn load_gitignore(project_path: &Path) -> Vec<String> {
    let gitignore_path = project_path.join(".gitignore");
    let content = match fs::read_to_string(gitignore_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.strip_prefix('/').unwrap_or(l).to_string())
        .map(|l| l.strip_suffix('/').unwrap_or(&l).to_string())
        .collect()
}

fn is_gitignored(rel_path: &str, patterns: &[String], is_dir: bool) -> bool {
    for pattern in patterns {
        if rel_path == *pattern || rel_path.starts_with(&format!("{pattern}/")) {
            return true;
        }
        if !pattern.contains('/') {
            let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
            if file_name == *pattern {
                return true;
            }
            if is_dir {
                for component in rel_path.split('/') {
                    if component == *pattern {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn project_name_from_path(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        assert_eq!(language_for_ext("rs"), Some("rust"));
        assert_eq!(language_for_ext("ts"), Some("typescript"));
        assert_eq!(language_for_ext("py"), Some("python"));
        assert_eq!(language_for_ext("xyz"), None);
    }

    #[test]
    fn test_gitignore_matching() {
        let patterns = vec!["dist".to_string(), "*.log".to_string()];
        assert!(is_gitignored("dist", &patterns, true));
        assert!(is_gitignored("dist/index.js", &patterns, false));
        assert!(!is_gitignored("src/main.rs", &patterns, false));
    }

    #[test]
    fn test_project_name() {
        let path = PathBuf::from("/home/user/Projects/my-app");
        assert_eq!(project_name_from_path(&path), "my-app");
    }
}
