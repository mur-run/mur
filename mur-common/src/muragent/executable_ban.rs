//! MCP command validation — deny-list, permit-list, metacharacter scan.
//!
//! Spec §6.4 step 2: reject executable content in `.muragent` packages.

/// File extensions that are forbidden inside a `.muragent` (case-insensitive).
const FORBIDDEN_EXTENSIONS: &[&str] = &[
    ".so",
    ".dylib",
    ".dll",
    ".exe",
    ".dmg",
    ".pkg",
    ".msi",
    ".appimage",
    ".elf",
    ".wasm",
    ".bin",
    ".sys",
    ".ko",
    ".kext",
    ".app",
    ".sh",
    ".bash",
    ".zsh",
    ".fish",
    ".py",
    ".rb",
    ".pl",
    ".php",
    ".lua",
];

/// Additional forbidden extensions that match versioned shared libraries
/// (e.g., `.so.1`, `.so.0.1.0`).
const FORBIDDEN_VERSIONED_PREFIXES: &[&str] = &[".so.", ".dylib."];

/// Shell interpreters forbidden as MCP command basenames.
const INTERPRETER_DENYLIST: &[&str] = &[
    "sh", "bash", "zsh", "dash", "fish", "python", "python3", "ruby", "perl", "php", "node",
    "deno", "bun", "lua", "luajit", "awk", "rscript", "groovy", "kotlin", "scala", "jq",
    "execline", "rc",
];

/// Inline-code-execution flags that must not appear in MCP args.
const CODE_EXECUTION_FLAGS: &[&str] = &[
    "-e",
    "--eval",
    "-c",
    "--command",
    "-r",
    "--require",
    "-exec",
    "--exec",
];

/// Shell metacharacters forbidden in MCP commands and args.
const SHELL_METACHARS: &[char] = &['|', ';', '&', '$', '`', '>', '<'];

/// Permit-list for MCP command basenames.
const PERMIT_LIST: &[&str] = &[
    "npx",
    "uvx",
    "docker",
    "podman",
    "git",
    "gh",
    "npm",
    "yarn",
    "pnpm",
    "curl",
    "wget",
    "jq",
    "rg",
    "fd",
    "sd",
    "bat",
    "delta",
    "ghostscript",
    "imagemagick",
    "ffmpeg",
    "sqlite3",
    "psql",
    "mysql",
    "redis-cli",
];

/// Validate a file path inside the tarball for executable content.
pub fn check_extension(path: &str) -> Result<(), String> {
    // Assets inside Commander's data namespace may contain .js/.ts as data.
    if path.starts_with("assets/commander/") {
        return Ok(());
    }

    let lower = path.to_lowercase();

    for ext in FORBIDDEN_EXTENSIONS {
        if lower.ends_with(ext) {
            return Err(format!("forbidden file extension '{ext}' in path '{path}'"));
        }
    }

    for prefix in FORBIDDEN_VERSIONED_PREFIXES {
        if let Some(pos) = lower.find(prefix) {
            let remainder = &lower[pos + prefix.len()..];
            if !remainder.is_empty() && remainder.chars().all(|c| c.is_ascii_digit() || c == '.') {
                return Err(format!("forbidden versioned library '{path}'"));
            }
        }
    }

    Ok(())
}

/// Validate an MCP server command string.
pub fn check_mcp_command(command: &str, args: &[String]) -> Result<(), String> {
    if command.starts_with('/') || command.contains('/') || command.contains('\\') {
        return Err(format!(
            "MCP command must be basename-only, got '{command}'"
        ));
    }

    if command.contains(SHELL_METACHARS) || command.contains(char::is_whitespace) {
        return Err(format!("invalid characters in command '{command}'"));
    }

    let basename = command.to_lowercase();

    if INTERPRETER_DENYLIST.contains(&basename.as_str()) {
        return Err(format!(
            "interpreter '{basename}' not allowed as MCP command"
        ));
    }

    for arg in args {
        for flag in CODE_EXECUTION_FLAGS {
            if arg == *flag {
                return Err(format!(
                    "code-execution flag '{flag}' not allowed in MCP args"
                ));
            }
        }
    }

    for arg in args {
        if arg.contains(SHELL_METACHARS) {
            return Err(format!("shell metacharacters in arg '{arg}'"));
        }
    }

    let combined = format!("{command} {}", args.join(" "));
    if (combined.contains("install") || combined.contains(" add "))
        && (args.iter().any(|a| a == "&&" || a == ";" || a == "|"))
    {
        return Err(format!(
            "package-manager install chain detected: '{combined}'"
        ));
    }

    if !PERMIT_LIST.contains(&basename.as_str()) {
        tracing::warn!("MCP command '{command}' not in v1 permit-list");
    }

    Ok(())
}

/// Check tar entry mode bits: regular files with execute bit are rejected.
/// Directories with execute bit are fine.
pub fn check_mode_bits(mode: u32, is_directory: bool) -> Result<(), String> {
    if !is_directory && (mode & 0o111) != 0 {
        return Err(format!(
            "regular file has execute permission bits set (mode {mode:o})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shared_library_extension() {
        assert!(check_extension("lib/libevil.so").is_err());
        assert!(check_extension("lib/libevil.SO").is_err());
        assert!(check_extension("lib/libevil.dylib").is_err());
        assert!(check_extension("payload.dll").is_err());
        assert!(check_extension("tool.exe").is_err());
    }

    #[test]
    fn rejects_versioned_shared_library() {
        assert!(check_extension("lib/libfoo.so.1").is_err());
        assert!(check_extension("lib/libfoo.so.0.1.0").is_err());
    }

    #[test]
    fn rejects_wasm_and_elf() {
        assert!(check_extension("plugin.wasm").is_err());
        assert!(check_extension("binary.elf").is_err());
    }

    #[test]
    fn rejects_script_extensions() {
        assert!(check_extension("setup.sh").is_err());
        assert!(check_extension("setup.bash").is_err());
        assert!(check_extension("helper.py").is_err());
        assert!(check_extension("filter.lua").is_err());
    }

    #[test]
    fn allows_commander_assets() {
        assert!(check_extension("assets/commander/workflows/example.js").is_ok());
        assert!(check_extension("assets/commander/programs/research.md").is_ok());
    }

    #[test]
    fn allows_safe_assets() {
        assert!(check_extension("manifest.yaml").is_ok());
        assert!(check_extension("icon/icon-512.png").is_ok());
        assert!(check_extension("voice/voice.yaml").is_ok());
    }

    #[test]
    fn rejects_interpreter_commands() {
        assert!(check_mcp_command("python3", &[]).is_err());
        assert!(check_mcp_command("bash", &[]).is_err());
        assert!(check_mcp_command("node", &[]).is_err());
    }

    #[test]
    fn rejects_inline_code_flags() {
        assert!(check_mcp_command("uvx", &["-e".into(), "print 1".into()]).is_err());
        assert!(check_mcp_command("npx", &["-e".into()]).is_err());
        assert!(check_mcp_command("some-tool", &["--eval".into()]).is_err());
    }

    #[test]
    fn rejects_shell_metacharacters_in_args() {
        assert!(check_mcp_command("echo", &["hello; rm -rf /".into()]).is_err());
        assert!(check_mcp_command("uvx", &["foo|bar".into()]).is_err());
    }

    #[test]
    fn rejects_install_chains() {
        let args: Vec<String> = ["install", "pkg", "&&", "rm"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(check_mcp_command("uvx", &args).is_err());
    }

    #[test]
    fn allows_safe_commands() {
        assert!(check_mcp_command("uvx", &[]).is_ok());
        assert!(check_mcp_command("npx", &[]).is_ok());
        let docker_args: Vec<String> = ["run", "image"].iter().map(|s| s.to_string()).collect();
        assert!(check_mcp_command("docker", &docker_args).is_ok());
        let gh_args: Vec<String> = ["issue", "list"].iter().map(|s| s.to_string()).collect();
        assert!(check_mcp_command("gh", &gh_args).is_ok());
    }

    #[test]
    fn warns_on_unknown_command() {
        assert!(check_mcp_command("my-custom-tool", &[]).is_ok());
    }

    #[test]
    fn rejects_absolute_command_path() {
        assert!(check_mcp_command("/usr/bin/npx", &[]).is_err());
    }

    #[test]
    fn rejects_path_separator_in_command() {
        assert!(check_mcp_command("bin/npx", &[]).is_err());
    }

    #[test]
    fn rejects_shell_metachar_in_command() {
        assert!(check_mcp_command("foo|bar", &[]).is_err());
    }

    #[test]
    fn rejects_execute_bit_on_regular_file() {
        assert!(check_mode_bits(0o755, false).is_err());
        assert!(check_mode_bits(0o644, false).is_ok());
        assert!(check_mode_bits(0o755, true).is_ok());
    }
}
