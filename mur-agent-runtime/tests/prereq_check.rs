use mur_agent_runtime::export::prereq_check::{check_mcp_prereqs, format_missing_error};
#[cfg(unix)]
use mur_agent_runtime::export::prereq_check::which_exists;

// Uses `sh` as the "exists" baseline — POSIX-only.
#[cfg(unix)]
#[test]
fn check_finds_missing_basenames() {
    let manifest = r#"
schema: mur-agent-package/1
prerequisites:
  mcp_servers:
    - name: real
      command_basename: sh
    - name: ghost
      command_basename: definitely-not-installed-binary-xyz123
"#;
    let missing = check_mcp_prereqs(manifest).unwrap();
    assert_eq!(missing.len(), 1);
    assert!(missing[0].contains("xyz123"));
}

#[test]
fn check_returns_empty_when_no_prereqs() {
    let manifest = "schema: mur-agent-package/1\n";
    let missing = check_mcp_prereqs(manifest).unwrap();
    assert!(missing.is_empty());
}

#[cfg(unix)]
#[test]
fn which_exists_finds_sh() {
    // /bin/sh is a POSIX requirement — should always be on PATH on Unix CI.
    assert!(which_exists("sh") || which_exists("/bin/sh"));
}

#[test]
fn format_missing_error_renders_hint() {
    let body = format_missing_error(&["npx".into(), "uvx".into()]);
    assert!(body.starts_with("error: missing MCP prerequisites:"));
    assert!(body.contains("- npx"));
    assert!(body.contains("- uvx"));
    assert!(body.contains("hint:"));
}
