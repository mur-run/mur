//! `mur init` steps C2–C9: install hooks / context for the non-Claude AI
//! tools that are present on this machine (Auggie, Gemini CLI, Copilot CLI,
//! Cursor, Codex CLI, OpenCode, Amp, file-based tools such as Aider).

use super::*;

/// Install every per-tool integration whose config directory exists,
/// appending each tool's label to `hooks_installed`.
pub(super) fn install_tool_hooks(
    home: &Path,
    mur_dir: &Path,
    is_mur_hook_cmd: &dyn Fn(&str) -> bool,
    hooks_installed: &mut Vec<&'static str>,
) -> Result<()> {
    // ─── Step C2: Install Auggie hooks in settings.json ──────────
    let auggie_dir = home.join(".augment");
    if auggie_dir.exists() {
        let auggie_settings_path = auggie_dir.join("settings.json");
        let mut auggie_settings: serde_json::Value = if auggie_settings_path.exists() {
            let data = std::fs::read_to_string(&auggie_settings_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let tool_script = hooks_dir.join("on-tool.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        // Auggie supports full Claude Code-compatible hooks:
        // PreToolUse, PostToolUse, Stop, SessionStart, SessionEnd
        let mur_hooks = serde_json::json!({
            "PreToolUse": [{
                "hooks": [{"type": "command", "command": format!("bash {}", prompt_script.display())}],
                "matcher": ""
            }],
            "PostToolUse": [{
                "hooks": [{"type": "command", "command": format!("bash {}", tool_script.display())}],
                "matcher": ""
            }],
            "Stop": [{
                "hooks": [{"type": "command", "command": format!("bash {}", stop_script.display())}]
            }]
        });

        // Merge: preserve existing hooks, overwrite mur-managed ones
        let existing_hooks = auggie_settings
            .get("hooks")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let mut merged = existing_hooks.as_object().cloned().unwrap_or_default();
        if let Some(mur_obj) = mur_hooks.as_object() {
            for (k, v) in mur_obj {
                merged.insert(k.clone(), v.clone());
            }
        }
        auggie_settings["hooks"] = serde_json::Value::Object(merged);

        let pretty = serde_json::to_string_pretty(&auggie_settings)?;
        std::fs::write(&auggie_settings_path, pretty)?;
        hooks_installed.push("Auggie");
    }

    // ─── Step C3: Install Gemini CLI hooks in settings.json ──────
    let gemini_dir = home.join(".gemini");
    if gemini_dir.exists() {
        let gemini_settings_path = gemini_dir.join("settings.json");
        let mut gemini_settings: serde_json::Value = if gemini_settings_path.exists() {
            let data = std::fs::read_to_string(&gemini_settings_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        let tool_script = hooks_dir.join("on-tool.sh");

        // Gemini CLI v0.26.0+ hook events
        let mur_hooks = serde_json::json!({
            "BeforeAgent": [{
                "hooks": [{"type": "command", "command": format!("bash {}", prompt_script.display())}]
            }],
            "AfterTool": [{
                "hooks": [{"type": "command", "command": format!("bash {}", tool_script.display())}]
            }],
            "SessionEnd": [{
                "hooks": [{"type": "command", "command": format!("bash {}", stop_script.display())}]
            }]
        });

        let existing_hooks = gemini_settings
            .get("hooks")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let mut merged = existing_hooks.as_object().cloned().unwrap_or_default();
        if let Some(mur_obj) = mur_hooks.as_object() {
            for (k, v) in mur_obj {
                merged.insert(k.clone(), v.clone());
            }
        }
        gemini_settings["hooks"] = serde_json::Value::Object(merged);

        let pretty = serde_json::to_string_pretty(&gemini_settings)?;
        std::fs::write(&gemini_settings_path, pretty)?;
        hooks_installed.push("Gemini CLI");
    }

    // ─── Step C4: Install GitHub Copilot CLI hooks ───────────────
    // Copilot CLI (GA 2026-02-25) reads hooks from:
    //   - ~/.github/hooks.json (global)
    //   - .github/hooks.json (project-level)
    // Format: { version: 1, hooks: { eventName: [{ type, bash, timeoutSec }] } }
    // Events: sessionStart, sessionEnd, userPromptSubmitted, preToolUse, postToolUse
    let copilot_hooks_dir = home.join(".github");
    {
        std::fs::create_dir_all(&copilot_hooks_dir)?;
        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let tool_script = hooks_dir.join("on-tool.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        let hooks_path = copilot_hooks_dir.join("hooks.json");
        let mut hooks_json: serde_json::Value = if hooks_path.exists() {
            let data = std::fs::read_to_string(&hooks_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({"version": 1, "hooks": {}}))
        } else {
            serde_json::json!({"version": 1, "hooks": {}})
        };

        let hook_defs = [
            ("sessionStart", format!("bash {}", prompt_script.display())),
            (
                "userPromptSubmitted",
                format!("bash {}", prompt_script.display()),
            ),
            ("postToolUse", format!("bash {}", tool_script.display())),
            ("sessionEnd", format!("bash {}", stop_script.display())),
        ];

        let hooks_obj = hooks_json
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Copilot hooks.json is not a JSON object"))?
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (event_name, script_cmd) in &hook_defs {
            let event_arr = hooks_obj
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("hooks field is not a JSON object"))?
                .entry(*event_name)
                .or_insert_with(|| serde_json::json!([]));
            let arr = event_arr
                .as_array_mut()
                .ok_or_else(|| anyhow::anyhow!("hook event '{event_name}' is not an array"))?;
            // Remove existing mur hooks
            arr.retain(|entry| {
                entry
                    .get("bash")
                    .and_then(|c| c.as_str())
                    .map(|c| !is_mur_hook_cmd(c))
                    .unwrap_or(true)
            });
            arr.push(serde_json::json!({
                "type": "command",
                "bash": script_cmd,
                "comment": "mur-managed-hook",
                "timeoutSec": 30
            }));
        }

        let pretty = serde_json::to_string_pretty(&hooks_json)?;
        std::fs::write(&hooks_path, pretty)?;
        hooks_installed.push("Copilot CLI");
    }

    // ─── Step C5: OpenClaw ──────────────────────────────────────
    // OpenClaw skills are handled via symlinks in ensure_mur_skill (Step C11).
    // Detection is handled in the detected tools section below.

    // ─── Step C6: Install Cursor hooks ────────────────────────────
    let cursor_dir = home.join(".cursor");
    if cursor_dir.exists() {
        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let tool_script = hooks_dir.join("on-tool.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        let cursor_hooks_path = cursor_dir.join("hooks.json");
        let mut cursor_hooks: serde_json::Value = if cursor_hooks_path.exists() {
            let data = std::fs::read_to_string(&cursor_hooks_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({"version": 1, "hooks": {}}))
        } else {
            serde_json::json!({"version": 1, "hooks": {}})
        };

        let mur_hook_marker = "mur-managed-hook";

        // Cursor hooks format: { version: 1, hooks: { eventName: [{ command: "..." }] } }
        let hook_defs = [
            (
                "beforeSubmitPrompt",
                prompt_script.to_string_lossy().to_string(),
            ),
            (
                "beforeShellExecution",
                tool_script.to_string_lossy().to_string(),
            ),
            ("stop", stop_script.to_string_lossy().to_string()),
        ];

        let hooks_obj = cursor_hooks
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Cursor hooks.json is not a JSON object"))?
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (event_name, script_path) in &hook_defs {
            let event_arr = hooks_obj
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("hooks field is not a JSON object"))?
                .entry(*event_name)
                .or_insert_with(|| serde_json::json!([]));
            let arr = event_arr
                .as_array_mut()
                .ok_or_else(|| anyhow::anyhow!("hook event '{event_name}' is not an array"))?;
            arr.retain(|entry| {
                entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| !c.contains(mur_hook_marker) && !is_mur_hook_cmd(c))
                    .unwrap_or(true)
            });
            arr.push(serde_json::json!({
                "command": format!("bash {}", script_path)
            }));
        }

        let pretty = serde_json::to_string_pretty(&cursor_hooks)?;
        std::fs::write(&cursor_hooks_path, pretty)?;
        hooks_installed.push("Cursor");
    }

    // ─── Step C7: Install Codex CLI integration ──────────────────
    let codex_dir = home.join(".codex");
    if codex_dir.exists() {
        // Codex reads AGENTS.md — we add a mur context section
        // Also set developer_instructions in config.toml
        let config_path = codex_dir.join("config.toml");
        if config_path.exists() {
            let mut config_content = std::fs::read_to_string(&config_path)?;
            let mur_instruction = "# mur-managed: inject learning context\n# Run `mur context --compact` before sessions for pattern injection\n";
            if !config_content.contains("mur-managed") {
                config_content.push_str(&format!(
                    "\n{}\ndeveloper_instructions = \"Before coding, check if mur has relevant patterns: run `mur context --compact` in the project directory.\"\n",
                    mur_instruction
                ));
                std::fs::write(&config_path, config_content)?;
            }
        }
        hooks_installed.push("Codex CLI");
    }

    // ─── Step C8a: Install OpenCode plugin ─────────────────────────
    // OpenCode uses JS/TS plugins in ~/.config/opencode/plugins/
    let opencode_plugins = home.join(".config").join("opencode").join("plugins");
    if home.join(".config").join("opencode").exists()
        || std::process::Command::new("which")
            .arg("opencode")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    {
        std::fs::create_dir_all(&opencode_plugins)?;
        let plugin_path = opencode_plugins.join("mur-plugin.ts");
        let hooks_dir = mur_dir.join("hooks");
        let plugin_content = format!(
            r#"// MUR learning plugin for OpenCode
// Auto-generated by `mur init --hooks`
import {{ execSync }} from "child_process";

export const MurPlugin = async ({{ project, $ }}) => {{
  // Inject MUR context at session start
  try {{
    execSync("bash {on_prompt}", {{ stdio: "pipe", timeout: 30000 }});
  }} catch (_) {{}}

  return {{
    "session.created": async (_input) => {{
      try {{
        execSync("bash {on_prompt}", {{ stdio: "pipe", timeout: 30000 }});
      }} catch (_) {{}}
    }},
    "tool.execute.after": async (_input) => {{
      try {{
        execSync("bash {on_tool}", {{ stdio: "pipe", timeout: 10000 }});
      }} catch (_) {{}}
    }},
    "session.updated": async (input) => {{
      // On session end, trigger learning
      if (input?.status === "complete" || input?.status === "error") {{
        try {{
          execSync("bash {on_stop}", {{ stdio: "pipe", timeout: 30000 }});
        }} catch (_) {{}}
      }}
    }},
  }};
}};
"#,
            on_prompt = hooks_dir.join("on-prompt.sh").display(),
            on_tool = hooks_dir.join("on-tool.sh").display(),
            on_stop = hooks_dir.join("on-stop.sh").display(),
        );
        std::fs::write(&plugin_path, plugin_content)?;
        hooks_installed.push("OpenCode");
    }

    // ─── Step C8b: Install Amp hooks ──────────────────────────────
    // Amp uses Claude Code hook format in AGENTS.md frontmatter or ~/.amp/hooks.json
    // Also supports .agents/skills/ for skills
    let amp_exists = std::process::Command::new("which")
        .arg("amp")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if amp_exists {
        let amp_dir = home.join(".amp");
        std::fs::create_dir_all(&amp_dir)?;
        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let tool_script = hooks_dir.join("on-tool.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        // Amp uses same format as Claude Code hooks
        let hooks_path = amp_dir.join("hooks.json");
        let amp_hooks = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "hooks": [{"type": "command", "command": format!("bash {}", prompt_script.display())}],
                    "matcher": ""
                }],
                "PostToolUse": [{
                    "hooks": [{"type": "command", "command": format!("bash {}", tool_script.display())}],
                    "matcher": ""
                }],
                "Stop": [{
                    "hooks": [{"type": "command", "command": format!("bash {}", stop_script.display())}],
                    "matcher": ""
                }]
            }
        });
        let pretty = serde_json::to_string_pretty(&amp_hooks)?;
        std::fs::write(&hooks_path, pretty)?;
        hooks_installed.push("Amp");
    }

    // ─── Step C9: Generate context files for file-based tools ────
    // Aider, Cline, Windsurf, Amazon Q use file-based instructions
    // Generate a shared mur context file that can be referenced
    let mur_context_path = mur_dir.join("context.md");
    let mur_context = r#"# MUR Context
# Auto-generated by `mur init --hooks`. Updated by `mur context --file`.
# This file is referenced by Aider, Cline, Windsurf, and other file-based tools.

## How to use MUR with this tool

MUR captures learning patterns from your coding sessions.
Run `mur context` to see relevant patterns for your current project.
Run `mur search <query>` to find specific patterns.
Run `mur learn` to extract new patterns from recent sessions.

## Quick reference

- Patterns: ~/.mur/patterns/
- Workflows: ~/.mur/workflows/
- Dashboard: `mur serve --open`
"#;
    std::fs::write(&mur_context_path, mur_context)?;

    // Aider: add to .aider.conf.yml if it exists
    let aider_conf = home.join(".aider.conf.yml");
    if aider_conf.exists() {
        let content = std::fs::read_to_string(&aider_conf)?;
        if !content.contains(".mur/context.md") {
            let mut new_content = content;
            new_content.push_str(&format!(
                "\n# mur-managed: auto-load learning context\nread:\n  - {}\n",
                mur_context_path.display()
            ));
            std::fs::write(&aider_conf, new_content)?;
            hooks_installed.push("Aider");
        }
    } else {
        // Create minimal .aider.conf.yml
        let aider_config = format!(
            "# mur-managed: auto-load learning context\nread:\n  - {}\n",
            mur_context_path.display()
        );
        std::fs::write(&aider_conf, aider_config)?;
        hooks_installed.push("Aider");
    }

    Ok(())
}
