use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::action::DeletionConfig;

/// Destructive patterns the guard detects.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum DestructivePattern {
    /// Shell: `rm`, `unlink`, `mv ... /dev/null`
    Rm { raw: String, paths: Vec<String> },
    /// Python: `os.remove`, `os.unlink`, `shutil.rmtree`
    PythonRemove { raw: String, paths: Vec<String> },
    /// MCP: tool named `delete_file` or similar
    McpDelete { tool_name: String, paths: Vec<String> },
    /// A2A: delete intent in message
    A2ADelete { paths: Vec<String> },
}

#[derive(Debug, Clone)]
pub enum GuardError {
    BatchTooLarge { count: usize, max: u32 },
    PathOutOfScope { path: PathBuf },
    WildcardRejected { path: String },
    Other(String),
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuardError::BatchTooLarge { count, max } => write!(f, "batch size {count} exceeds max {max}"),
            GuardError::PathOutOfScope { path } => write!(f, "path outside allowed scope: {}", path.display()),
            GuardError::WildcardRejected { path } => write!(f, "wildcard pattern rejected: {path}"),
            GuardError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl DestructivePattern {
    /// Scan a shell command string for destructive patterns.
    pub fn detect_in_shell(cmd: &str) -> Vec<DestructivePattern> {
        let mut patterns = Vec::new();
        let cmd_trimmed = cmd.trim();

        if let Some(rest) = cmd_trimmed
            .strip_prefix("rm ")
            .or_else(|| cmd_trimmed.strip_prefix("/bin/rm "))
            .or_else(|| cmd_trimmed.strip_prefix("/usr/bin/rm "))
        {
            let paths = extract_paths(rest);
            patterns.push(DestructivePattern::Rm {
                raw: cmd.to_string(),
                paths,
            });
        }

        if cmd_trimmed.starts_with("unlink ") {
            let paths = extract_paths(&cmd_trimmed["unlink ".len()..]);
            patterns.push(DestructivePattern::Rm {
                raw: cmd.to_string(),
                paths,
            });
        }

        if cmd_trimmed.contains(" /dev/null") || cmd_trimmed.contains(" /dev/null\n") {
            patterns.push(DestructivePattern::Rm {
                raw: cmd.to_string(),
                paths: vec![],
            });
        }

        patterns
    }

    /// Scan code (Python, etc.) for destructive calls.
    pub fn detect_in_code(code: &str) -> Vec<DestructivePattern> {
        let mut patterns = Vec::new();

        for keyword in &["os.remove", "os.unlink", "shutil.rmtree", "pathlib.Path.unlink"] {
            if code.contains(keyword) {
                let paths = extract_python_paths(code, keyword);
                patterns.push(DestructivePattern::PythonRemove {
                    raw: code.to_string(),
                    paths,
                });
            }
        }

        patterns
    }

    /// Check if any path in this pattern contains a wildcard.
    pub fn contains_wildcard(&self) -> bool {
        let paths = match self {
            DestructivePattern::Rm { paths, .. } => paths,
            DestructivePattern::PythonRemove { paths, .. } => paths,
            DestructivePattern::McpDelete { paths, .. } => paths,
            DestructivePattern::A2ADelete { paths } => paths,
        };
        paths.iter().any(|p| p.contains('*') || p.contains('?'))
    }

    /// Check if pattern matches MCP delete_file tool.
    pub fn detect_in_mcp_tool(tool_name: &str, arguments: &serde_json::Value) -> Vec<DestructivePattern> {
        let delete_tools = [
            "delete_file",
            "delete_files",
            "remove_file",
            "remove_files",
            "fs_delete",
            "fs.remove",
        ];
        if delete_tools.iter().any(|t| tool_name.eq_ignore_ascii_case(t))
            || tool_name.to_lowercase().contains("delete")
            || tool_name.to_lowercase().contains("remove")
        {
            let paths = extract_json_paths(arguments);
            return vec![DestructivePattern::McpDelete {
                tool_name: tool_name.to_string(),
                paths,
            }];
        }
        vec![]
    }
}

/// Pure guard logic — no I/O. Callable from Hook impl AND tests.
pub struct TrashGuardLogic {
    config: DeletionConfig,
}

impl TrashGuardLogic {
    pub fn new(config: DeletionConfig) -> Self {
        Self { config }
    }

    /// Check batch size is within limits.
    pub fn check_batch_size(&self, count: usize) -> Result<(), GuardError> {
        if count > self.config.max_batch as usize {
            return Err(GuardError::BatchTooLarge {
                count,
                max: self.config.max_batch,
            });
        }
        Ok(())
    }

    /// Check a path is within allowed scope (trusted_paths or not).
    pub fn check_path_scope(&self, path: &Path) -> Result<(), GuardError> {
        if self.config.trusted_paths.is_empty() {
            return Ok(());
        }
        let canonical = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf());
        for trusted in &self.config.trusted_paths {
            let trusted_path = PathBuf::from(trusted);
            let trusted_canonical = trusted_path
                .canonicalize()
                .unwrap_or_else(|_| trusted_path.clone());
            if canonical.starts_with(&trusted_canonical) {
                return Ok(());
            }
        }
        Err(GuardError::PathOutOfScope {
            path: path.to_path_buf(),
        })
    }

    /// Detect all destructive patterns in a tool call.
    pub fn detect(&self, tool_name: &str, tool_input: &serde_json::Value) -> Vec<DestructivePattern> {
        let mut patterns = Vec::new();

        patterns.extend(DestructivePattern::detect_in_mcp_tool(tool_name, tool_input));

        if let Some(cmd) = tool_input.get("command").and_then(|v| v.as_str()) {
            patterns.extend(DestructivePattern::detect_in_shell(cmd));
        }
        if let Some(code) = tool_input.get("code").and_then(|v| v.as_str()) {
            patterns.extend(DestructivePattern::detect_in_code(code));
        }

        if let Some(path) = tool_input.get("path").and_then(|v| v.as_str()) {
            if path.contains('*') || path.contains('?') {
                patterns.push(DestructivePattern::Rm {
                    raw: format!("delete path={path}"),
                    paths: vec![path.to_string()],
                });
            }
        }

        patterns
    }

    pub fn config(&self) -> &DeletionConfig {
        &self.config
    }
}

fn extract_paths(s: &str) -> Vec<String> {
    s.split_whitespace()
        .filter(|w| !w.starts_with('-') && !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

fn extract_python_paths(code: &str, _keyword: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for ch in ['\'', '\"'] {
        let pattern = format!("({ch}");
        for part in code.split(&pattern).skip(1) {
            if let Some(end) = part.find(ch) {
                paths.push(part[..end].to_string());
            }
        }
    }
    paths
}

fn extract_json_paths(args: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
        paths.push(path.to_string());
    }
    if let Some(paths_arr) = args.get("paths").and_then(|v| v.as_array()) {
        for p in paths_arr {
            if let Some(s) = p.as_str() {
                paths.push(s.to_string());
            }
        }
    }
    if let Some(file_path) = args.get("file_path").and_then(|v| v.as_str()) {
        paths.push(file_path.to_string());
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> DeletionConfig {
        DeletionConfig {
            trash_enabled: true,
            cancel_window_minutes: 10,
            trash_retention_days: 30,
            trash_max_mb: 1024,
            max_batch: 50,
            auto_permanent_delete: false,
            trusted_paths: vec![],
        }
    }

    #[test]
    fn detect_shell_rm_pattern() {
        let detections = DestructivePattern::detect_in_shell("rm -rf /tmp/foo");
        assert!(!detections.is_empty());
        assert!(detections.iter().any(|d| matches!(d, DestructivePattern::Rm { .. })));
    }

    #[test]
    fn detect_python_os_remove() {
        let detections = DestructivePattern::detect_in_code("os.remove('/tmp/x')");
        assert!(!detections.is_empty());
    }

    #[test]
    fn detect_wildcard_in_path() {
        let detections = DestructivePattern::detect_in_shell("rm /tmp/*.txt");
        assert!(detections.iter().any(|d| d.contains_wildcard()));
    }

    #[test]
    fn reject_batch_above_max() {
        let config = DeletionConfig { max_batch: 5, ..test_config() };
        let guard = TrashGuardLogic::new(config);
        let result = guard.check_batch_size(10);
        assert!(result.is_err());
        match result.unwrap_err() {
            GuardError::BatchTooLarge { count, max } => {
                assert_eq!(count, 10);
                assert_eq!(max, 5);
            }
            e => panic!("expected BatchTooLarge, got {e:?}"),
        }
    }

    #[test]
    fn allow_batch_at_or_below_max() {
        let config = DeletionConfig { max_batch: 5, ..test_config() };
        let guard = TrashGuardLogic::new(config);
        assert!(guard.check_batch_size(5).is_ok());
        assert!(guard.check_batch_size(1).is_ok());
    }

    #[test]
    fn path_within_allowed_scope() {
        let config = DeletionConfig {
            trusted_paths: vec!["/tmp/allowed/".into()],
            ..test_config()
        };
        let guard = TrashGuardLogic::new(config);
        let allowed = PathBuf::from("/tmp/allowed/test.txt");
        assert!(guard.check_path_scope(&allowed).is_ok());
    }

    #[test]
    fn path_outside_scope_rejected() {
        let config = DeletionConfig {
            trusted_paths: vec!["/tmp/allowed/".into()],
            ..test_config()
        };
        let guard = TrashGuardLogic::new(config);
        let denied = PathBuf::from("/etc/passwd");
        assert!(guard.check_path_scope(&denied).is_err());
    }
}
