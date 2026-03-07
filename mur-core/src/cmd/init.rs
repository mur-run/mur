use anyhow::Result;
use std::io::{self, Write};

use crate::store::yaml::YamlStore;

pub(crate) fn cmd_init(hooks_flag: bool) -> Result<()> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let mur_dir = home.join(".mur");

    // ─── Step A: Create directory structure ───────────────────────
    let dirs_to_create = [
        mur_dir.clone(),
        mur_dir.join("patterns"),
        mur_dir.join("workflows"),
        mur_dir.join("session").join("recordings"),
        mur_dir.join("hooks"),
        mur_dir.join("index"),
    ];
    for d in &dirs_to_create {
        std::fs::create_dir_all(d)?;
    }

    // ─── Step E: Write default config.yaml if not exists ─────────
    let config_path = mur_dir.join("config.yaml");
    if !config_path.exists() {
        let default_config = r#"# MUR Configuration
# See: https://github.com/mur-run/mur

tools:
  claude:
    enabled: true
  gemini:
    enabled: true

search:
  provider: ollama
  model: qwen3-embedding:0.6b

learning:
  llm:
    provider: ollama
    model: llama3.2:3b
"#;
        std::fs::write(&config_path, default_config)?;
    }

    // ─── Determine whether to install hooks ──────────────────────
    let install_hooks = if hooks_flag {
        true
    } else {
        // Interactive prompt
        print!("Install hooks for AI tools? [Y/n] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        let answer = answer.trim().to_lowercase();
        answer.is_empty() || answer == "y" || answer == "yes"
    };

    let mut hooks_installed = Vec::new();

    if install_hooks {
        // ─── Step B: Write hook scripts ──────────────────────────
        let on_prompt = r#"#!/bin/bash
# mur-managed-hook v5
INPUT=$(cat /dev/stdin 2>/dev/null || echo '{}')
MUR=$(which mur 2>/dev/null || echo "mur")
$MUR context --compact 2>/dev/null || true
if [ -f ~/.mur/session/active.json ]; then
  PROMPT=$(echo "$INPUT" | jq -r '.prompt // empty' 2>/dev/null)
  if [ -n "$PROMPT" ]; then
    $MUR session record --event-type user --content "$PROMPT" 2>/dev/null || true
  fi
fi
exit 0
"#;

        let on_tool = r#"#!/bin/bash
# mur-managed-hook v5
MUR=$(which mur 2>/dev/null || echo "mur")
if [ -f ~/.mur/session/active.json ]; then
  INPUT=$(cat /dev/stdin 2>/dev/null || echo '{}')
  TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
  TOOL_INPUT=$(echo "$INPUT" | jq -c '.tool_input // {}' 2>/dev/null)
  if [ -n "$TOOL" ]; then
    $MUR session record --event-type tool_call --tool "$TOOL" --content "$TOOL_INPUT" 2>/dev/null || true
  fi
fi
"#;

        let on_stop = r#"#!/bin/bash
# mur-managed-hook v6
INPUT=$(cat /dev/stdin 2>/dev/null || echo '{}')
MUR=$(which mur 2>/dev/null || echo "mur")

# Record session stop event
if [ -f ~/.mur/session/active.json ]; then
  STOP_REASON=$(echo "$INPUT" | jq -r '.stop_reason // "turn_end"' 2>/dev/null)
  $MUR session record --event-type assistant --content "[stop: $STOP_REASON]" 2>/dev/null || true
fi

# Background: full learning pipeline
(
  # 1. Sync patterns to AI tool configs
  $MUR sync --quiet 2>/dev/null

  # 2. Run decay + maturity evaluation
  $MUR evolve 2>/dev/null

  # 3. Extract behavior fingerprints from latest session (no LLM, pure regex)
  LATEST=$(ls -t ~/.mur/session/recordings/*.jsonl 2>/dev/null | head -1)
  if [ -n "$LATEST" ]; then
    $MUR learn extract --file "$LATEST" --fingerprint 2>/dev/null
  fi

  # 4. Detect emergent patterns from accumulated fingerprints (no LLM)
  $MUR emerge 2>/dev/null
) &

exit 0
"#;

        let hooks = [
            ("on-prompt.sh", on_prompt),
            ("on-tool.sh", on_tool),
            ("on-stop.sh", on_stop),
        ];

        for (filename, content) in &hooks {
            let path = mur_dir.join("hooks").join(filename);
            std::fs::write(&path, content)?;
            // Make executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            }
        }

        // ─── Step C: Install Claude Code hooks in settings.json ──
        let claude_dir = home.join(".claude");
        std::fs::create_dir_all(&claude_dir)?;
        let settings_path = claude_dir.join("settings.json");

        let mut settings: serde_json::Value = if settings_path.exists() {
            let content = std::fs::read_to_string(&settings_path)?;
            serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        let hooks_dir = mur_dir.join("hooks");
        let mur_hook_marker = "mur-managed-hook";

        // Define the hooks we want to install
        let hook_defs = [
            (
                "UserPromptSubmit",
                hooks_dir.join("on-prompt.sh").to_string_lossy().to_string(),
            ),
            (
                "PostToolUse",
                hooks_dir.join("on-tool.sh").to_string_lossy().to_string(),
            ),
            (
                "Stop",
                hooks_dir.join("on-stop.sh").to_string_lossy().to_string(),
            ),
        ];

        let hooks_obj = settings
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (event_name, script_path) in &hook_defs {
            let event_arr = hooks_obj
                .as_object_mut()
                .unwrap()
                .entry(*event_name)
                .or_insert_with(|| serde_json::json!([]));

            let arr = event_arr.as_array_mut().unwrap();

            // Remove any existing mur-managed hooks (by checking command contains mur hooks dir)
            arr.retain(|entry| {
                // Check flat format: { command: "..." }
                if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
                    return !cmd.contains(mur_hook_marker) && !cmd.contains(".mur/hooks/");
                }
                // Check nested format: { hooks: [{ command: "..." }] }
                if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
                    return !hooks.iter().any(|h| {
                        h.get("command")
                            .and_then(|c| c.as_str())
                            .map(|c| c.contains(".mur/hooks/"))
                            .unwrap_or(false)
                    });
                }
                true
            });

            // Add our hook (Claude Code format: { hooks: [...], matcher: "" })
            arr.push(serde_json::json!({
                "hooks": [{
                    "type": "command",
                    "command": format!("bash {}", script_path),
                }],
                "matcher": ""
            }));
        }

        // Write settings back with pretty formatting
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;

        hooks_installed.push("Claude Code");
    }

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
        for (k, v) in mur_hooks.as_object().unwrap() {
            merged.insert(k.clone(), v.clone());
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
        for (k, v) in mur_hooks.as_object().unwrap() {
            merged.insert(k.clone(), v.clone());
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

        let mur_marker = ".mur/hooks/";
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
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (event_name, script_cmd) in &hook_defs {
            let event_arr = hooks_obj
                .as_object_mut()
                .unwrap()
                .entry(*event_name)
                .or_insert_with(|| serde_json::json!([]));
            let arr = event_arr.as_array_mut().unwrap();
            // Remove existing mur hooks
            arr.retain(|entry| {
                entry
                    .get("bash")
                    .and_then(|c| c.as_str())
                    .map(|c| !c.contains(mur_marker))
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

    // ─── Step C5: Install OpenClaw hooks ─────────────────────────
    let openclaw_config_path = home.join(".openclaw").join("config.json");
    if openclaw_config_path.exists() {
        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        let mut oc_config: serde_json::Value = {
            let data = std::fs::read_to_string(&openclaw_config_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({}))
        };

        // OpenClaw uses a hooks array in config.json
        let mur_hooks = serde_json::json!([
            {
                "id": "mur-on-prompt",
                "event": "session.start",
                "command": format!("bash {}", prompt_script.display())
            },
            {
                "id": "mur-on-stop",
                "event": "session.end",
                "command": format!("bash {}", stop_script.display())
            }
        ]);

        // Replace existing mur hooks, keep others
        let existing_hooks = oc_config
            .get("hooks")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();
        let mut kept: Vec<serde_json::Value> = existing_hooks
            .into_iter()
            .filter(|h| {
                h.get("id")
                    .and_then(|id| id.as_str())
                    .map(|id| !id.starts_with("mur-"))
                    .unwrap_or(true)
            })
            .collect();
        if let Some(arr) = mur_hooks.as_array() {
            kept.extend(arr.clone());
        }
        oc_config["hooks"] = serde_json::Value::Array(kept);

        let pretty = serde_json::to_string_pretty(&oc_config)?;
        std::fs::write(&openclaw_config_path, pretty)?;
        hooks_installed.push("OpenClaw");
    }

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
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (event_name, script_path) in &hook_defs {
            let event_arr = hooks_obj
                .as_object_mut()
                .unwrap()
                .entry(*event_name)
                .or_insert_with(|| serde_json::json!([]));
            let arr = event_arr.as_array_mut().unwrap();
            arr.retain(|entry| {
                entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| !c.contains(mur_hook_marker) && !c.contains(".mur/hooks/"))
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

    // ─── Step C10: Detect and print setup hints for file-based tools ─
    // Zed reads: .rules > .cursorrules > .windsurfrules > AGENTS.md (first match wins)
    // Junie reads: .junie/guidelines.md
    // Trae reads: .trae/rules/
    // These are project-level, so we just print hints
    let file_based_hints: Vec<(&str, &str)> = vec![
        (
            "Zed",
            "Add `See ~/.mur/context.md` to your AGENTS.md or .rules file",
        ),
        (
            "Junie",
            "Add `See ~/.mur/context.md` to .junie/guidelines.md",
        ),
        ("Trae", "Add `See ~/.mur/context.md` to .trae/rules/mur.md"),
        ("Cline/Roo", "Add `See ~/.mur/context.md` to .clinerules"),
        ("Windsurf", "Add `See ~/.mur/context.md` to .windsurfrules"),
    ];

    // ─── Step C11: Install AI tool skills ────────────────────────
    // Skills teach AI tools about mur commands and how to interact
    // with the pattern system (feedback, create, search, etc.)
    if install_hooks {
        let _ = crate::cmd::sync_cmd::ensure_mur_skill(&home);
    }

    // ─── Step C12: Scan for existing AI tool rules ────────────────
    if let Ok(cwd) = std::env::current_dir() {
        use crate::capture::import;

        let detected_files = import::detect_files(&cwd);
        if !detected_files.is_empty() {
            println!();
            println!("  Scanning for existing AI tool rules...");

            let mut file_summaries = Vec::new();
            let mut all_candidates = Vec::new();
            for path in &detected_files {
                if let Ok(candidates) = import::extract_from_file(path) {
                    let filename = path.file_name().unwrap_or_default().to_string_lossy();
                    file_summaries.push(format!("{} ({} rules)", filename, candidates.len()));
                    all_candidates.extend(candidates);
                }
            }

            if !all_candidates.is_empty() {
                for summary in &file_summaries {
                    println!("     Found: {}", summary);
                }

                print!("\n  Import as MUR patterns? [Y/n] ");
                io::stdout().flush()?;
                let mut import_answer = String::new();
                io::stdin().read_line(&mut import_answer)?;
                let import_answer = import_answer.trim().to_lowercase();
                let do_import =
                    import_answer.is_empty() || import_answer == "y" || import_answer == "yes";

                if do_import {
                    let store = YamlStore::default_store()?;
                    let existing: std::collections::HashSet<String> =
                        store.list_names()?.into_iter().collect();
                    let patterns = import::candidates_to_patterns(all_candidates, &existing);
                    let count = patterns.len();
                    for pattern in &patterns {
                        store.save(pattern)?;
                    }
                    println!("  Imported {} patterns (Project tier)", count);
                    println!();
                    println!("  These patterns now work across ALL your AI tools.");
                }
            }
        }
    }

    // ─── Step G: Interactive LLM/Embedding setup ─────────────────
    println!();
    println!("Model setup — MUR uses two types of models:");
    println!();
    println!("  📚 LLM (pattern learning)");
    println!("     Understands code, extracts patterns. Cloud models are MUCH better.");
    println!("     Called rarely (only during `mur learn`), so cost is minimal.");
    println!();
    println!("  🔍 Embedding (semantic search)");
    println!("     Converts text to vectors for similarity matching. Simpler task.");
    println!("     Called every AI session, so local = free + instant + no API dependency.");
    println!();
    println!("Setup mode:");
    println!("  1) Cloud LLM + local embedding (recommended — best of both worlds)");
    println!("  2) All cloud — API keys required for both");
    println!("  3) All local — Ollama, free, runs on your machine");
    println!("  4) Skip — keep current config");
    print!("Choose [1/2/3/4] (default: 1): ");
    io::stdout().flush()?;
    let mut model_choice = String::new();
    io::stdin().read_line(&mut model_choice)?;
    let model_choice = model_choice.trim().to_string();
    let model_choice = if model_choice.is_empty() {
        "1"
    } else {
        model_choice.as_str()
    };

    // Load config (just written with defaults above)
    let mut config = crate::store::config::load_config()?;

    // Helper: select cloud LLM provider
    let select_cloud_llm =
        |config: &mut mur_common::config::Config| -> Result<(&'static str, &'static str, &'static str, bool)> {
            println!();
            println!("Cloud LLM provider:");
            println!("  1) OpenRouter (recommended — access to many models)");
            println!("  2) OpenAI");
            println!("  3) Gemini");
            println!("  4) Anthropic");
            print!("Choose [1/2/3/4] (default: 1): ");
            io::stdout().flush()?;
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;

            let (provider, llm_model, env_var, is_openrouter) = match choice.trim() {
                "2" => ("openai", "gpt-4o-mini", "OPENAI_API_KEY", false),
                "3" => ("gemini", "gemini-2.0-flash", "GEMINI_API_KEY", false),
                "4" => (
                    "anthropic",
                    "claude-sonnet-4-20250514",
                    "ANTHROPIC_API_KEY",
                    false,
                ),
                _ => (
                    "openai",
                    "google/gemini-2.5-flash",
                    "OPENROUTER_API_KEY",
                    true,
                ),
            };

            if std::env::var(env_var).is_ok() {
                println!("  ✓ {} detected", env_var);
            } else {
                println!(
                    "  ⚠ {} not set — set it before using MUR learning features",
                    env_var
                );
            }

            let openrouter_url = "https://openrouter.ai/api/v1".to_string();
            config.llm.provider = provider.to_string();
            config.llm.model = llm_model.to_string();
            config.llm.api_key_env = Some(env_var.to_string());
            config.llm.openai_url = if is_openrouter {
                Some(openrouter_url)
            } else {
                None
            };

            Ok((provider, env_var, llm_model, is_openrouter))
        };

    // Helper: select local Ollama embedding model
    let select_ollama_embedding = |config: &mut mur_common::config::Config| -> Result<()> {
        println!();
        println!("Embedding model (Ollama):");
        println!("  1) qwen3-embedding:0.6b — fast, ~1.5GB RAM (recommended)");
        println!("  2) qwen3-embedding:4b  — better multilingual, ~8GB RAM");
        println!("  3) qwen3-embedding:8b  — best quality (MTEB #1), ~16GB RAM");
        println!("  4) nomic-embed-text    — lightweight alternative, ~300MB RAM");
        print!("Choose [1/2/3/4] (default: 1): ");
        io::stdout().flush()?;
        let mut choice = String::new();
        io::stdin().read_line(&mut choice)?;

        let (model, dims) = match choice.trim() {
            "2" => ("qwen3-embedding:4b", 2560),
            "3" => ("qwen3-embedding:8b", 4096),
            "4" => ("nomic-embed-text", 768),
            _ => ("qwen3-embedding:0.6b", 1024),
        };

        config.embedding.provider = "ollama".to_string();
        config.embedding.model = model.to_string();
        config.embedding.dimensions = dims;
        config.embedding.api_key_env = None;
        config.embedding.openai_url = None;
        Ok(())
    };

    // Helper: select cloud embedding provider
    let select_cloud_embedding = |config: &mut mur_common::config::Config,
                                  llm_provider: &str,
                                  llm_env_var: &str|
     -> Result<()> {
        println!();
        println!("Embedding provider:");
        let cloud_label = match llm_provider {
            "openai" => "OpenAI — text-embedding-3-small (same API key)",
            "gemini" => "Gemini — text-embedding-004 (same API key)",
            "anthropic" => "Voyage — voyage-3-lite (same API key)",
            _ => "Cloud embedding",
        };
        println!("  1) {} (recommended)", cloud_label);
        println!("  2) Local Ollama — free, no API dependency");
        print!("Choose [1/2] (default: 1): ");
        io::stdout().flush()?;
        let mut choice = String::new();
        io::stdin().read_line(&mut choice)?;

        if choice.trim() == "2" {
            // Delegate to Ollama selection
            select_ollama_embedding(config)?;
        } else {
            let (provider, model, dims) = match llm_provider {
                "openai" => ("openai", "text-embedding-3-small", 1536),
                "gemini" => ("gemini", "text-embedding-004", 768),
                "anthropic" => ("anthropic", "voyage-3-lite", 1024),
                _ => ("openai", "text-embedding-3-small", 1536),
            };
            config.embedding.provider = provider.to_string();
            config.embedding.model = model.to_string();
            config.embedding.dimensions = dims;
            config.embedding.api_key_env = Some(llm_env_var.to_string());
            config.embedding.openai_url = None;
        }
        Ok(())
    };

    match model_choice {
        "1" => {
            // Cloud LLM + local embedding (recommended)
            let (_provider, _env_var, llm_model, is_openrouter) = select_cloud_llm(&mut config)?;
            select_ollama_embedding(&mut config)?;

            crate::store::config::save_config(&config)?;
            let llm_display = if is_openrouter {
                "openrouter"
            } else {
                _provider
            };
            println!(
                "  ✓ Config: {} (LLM) + ollama/{} (search) / {}",
                llm_display, config.embedding.model, llm_model
            );
        }
        "2" => {
            // All cloud
            let (provider, env_var, llm_model, is_openrouter) = select_cloud_llm(&mut config)?;

            if is_openrouter {
                // OpenRouter doesn't offer embeddings, use cloud or ollama
                println!();
                println!("  ℹ OpenRouter doesn't provide embedding APIs.");
                println!("    Pick a separate embedding provider:");
                println!("  1) OpenAI — text-embedding-3-small (requires OPENAI_API_KEY)");
                println!("  2) Local Ollama — free");
                print!("Choose [1/2] (default: 1): ");
                io::stdout().flush()?;
                let mut choice = String::new();
                io::stdin().read_line(&mut choice)?;

                if choice.trim() == "2" {
                    select_ollama_embedding(&mut config)?;
                } else {
                    config.embedding.provider = "openai".to_string();
                    config.embedding.model = "text-embedding-3-small".to_string();
                    config.embedding.dimensions = 1536;
                    config.embedding.api_key_env = Some("OPENAI_API_KEY".to_string());
                    config.embedding.openai_url = None;
                    if std::env::var("OPENAI_API_KEY").is_err() {
                        println!("  ⚠ OPENAI_API_KEY not set — set it for embedding to work");
                    }
                }
            } else {
                select_cloud_embedding(&mut config, provider, env_var)?;
            }

            crate::store::config::save_config(&config)?;
            let llm_display = if is_openrouter {
                "openrouter"
            } else {
                provider
            };
            println!(
                "  ✓ Config: {} (LLM) + {}/{} (search) / {}",
                llm_display, config.embedding.provider, config.embedding.model, llm_model
            );
        }
        "3" => {
            // All local (Ollama)
            let ollama_running = std::process::Command::new("ollama")
                .arg("list")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);

            if !ollama_running {
                println!();
                println!("  ⚠ Ollama not detected. Install from https://ollama.com");
                println!("  Using default models in config (pull them after installing).");
            } else {
                println!("  ✓ Ollama detected");
            }

            println!();
            println!("LLM model for pattern learning:");
            println!("  1) llama3.2:3b   — lightweight, ~2GB RAM");
            println!("  2) llama3.1:8b   — better quality, ~5GB RAM");
            println!("  3) qwen3:4b      — good for code, ~3GB RAM");
            print!("Choose [1/2/3] (default: 1): ");
            io::stdout().flush()?;
            let mut llm_choice = String::new();
            io::stdin().read_line(&mut llm_choice)?;
            let llm_model = match llm_choice.trim() {
                "2" => "llama3.1:8b",
                "3" => "qwen3:4b",
                _ => "llama3.2:3b",
            };

            config.llm.provider = "ollama".to_string();
            config.llm.model = llm_model.to_string();
            config.llm.api_key_env = None;
            config.llm.openai_url = None;

            select_ollama_embedding(&mut config)?;

            crate::store::config::save_config(&config)?;
            println!(
                "  ✓ Config: ollama/{} (LLM) + ollama/{} (search)",
                llm_model, config.embedding.model
            );
        }
        _ => {
            // Skip — keep current config
            println!("  Keeping current config.");
        }
    }

    // ─── Step H: Community sharing opt-in ──────────────────────────
    println!();
    print!("Enable community pattern sharing? [y/N] ");
    io::stdout().flush()?;
    let mut community_answer = String::new();
    io::stdin().read_line(&mut community_answer)?;
    let community_enabled = {
        let a = community_answer.trim().to_lowercase();
        a == "y" || a == "yes"
    };

    if community_enabled {
        // Reload config in case model setup saved changes
        config = crate::store::config::load_config().unwrap_or(config);
        config.community.enabled = true;
        let _ = crate::store::config::save_config(&config);
        println!("  Community sharing enabled.");
        println!("  Run `mur login` to authenticate and start sharing patterns.");
    }

    // ─── Step D: Detect other tools ──────────────────────────────
    let gemini_settings = home.join(".gemini").join("settings.json");
    let cursor_rules = std::env::current_dir().ok().map(|d| d.join(".cursorrules"));

    let mut detected_tools = Vec::new();

    if gemini_settings.exists() || home.join(".gemini").exists() {
        detected_tools.push("Gemini CLI");
        // Antigravity uses Gemini under the hood — same hooks apply
        detected_tools.push("Antigravity");
    }
    if let Some(ref cr) = cursor_rules
        && cr.exists()
    {
        detected_tools.push("Cursor");
    }

    // Check for CLI-based AI tools via `which`
    let cli_tools = [
        ("codex", "Codex"),
        ("auggie", "Auggie"),
        ("aider", "Aider"),
        ("openclaw", "OpenClaw"),
        ("opencode", "OpenCode"),
        ("amp", "Amp"),
        ("zed", "Zed"),
    ];
    for (binary, name) in &cli_tools {
        if std::process::Command::new("which")
            .arg(binary)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            detected_tools.push(name);
        }
    }

    // Check for GitHub Copilot config directory
    if home.join(".config").join("github-copilot").exists() || home.join(".copilot").exists() {
        detected_tools.push("GitHub Copilot");
    }

    // Check for Cline/Roo (VS Code extension — detect .clinerules in cwd)
    if let Ok(cwd) = std::env::current_dir()
        && (cwd.join(".clinerules").exists() || cwd.join(".roomodes").exists())
    {
        detected_tools.push("Cline/Roo");
    }

    // Check for Windsurf
    if let Ok(cwd) = std::env::current_dir()
        && (cwd.join(".windsurfrules").exists() || cwd.join(".windsurf").exists())
    {
        detected_tools.push("Windsurf");
    }

    // Check for Amazon Q
    if home.join(".amazonq").exists() {
        detected_tools.push("Amazon Q");
    }

    // Check for JetBrains Junie
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(".junie").exists() {
            detected_tools.push("Junie");
        }
        if cwd.join(".trae").exists() {
            detected_tools.push("Trae");
        }
    }

    // ─── Step F: Print summary ───────────────────────────────────
    let pattern_count = if mur_dir.join("patterns").exists() {
        std::fs::read_dir(mur_dir.join("patterns"))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "yaml" || ext == "yml")
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };

    println!();
    println!("✅ MUR initialized!");
    println!();
    println!("  📁 Data directory: ~/.mur/");
    if !hooks_installed.is_empty() {
        println!("  🪝 Hooks installed: {}", hooks_installed.join(", "));
    } else {
        println!("  🪝 Hooks: not installed (run `mur init --hooks` to install)");
    }
    println!(
        "  📝 Patterns: {} {}",
        pattern_count,
        if pattern_count == 0 {
            "(run `mur new` to create your first)"
        } else {
            ""
        }
    );

    // Show detected tools
    if !detected_tools.is_empty() {
        println!();
        println!("  🔍 Detected tools: {}", detected_tools.join(", "));
    }

    // Show file-based tool hints
    let show_hints: Vec<_> = file_based_hints
        .iter()
        .filter(|(tool, _)| detected_tools.contains(tool))
        .collect();
    if !show_hints.is_empty() {
        println!();
        println!("  📝 File-based tools (add MUR context manually):");
        for (tool, hint) in &show_hints {
            println!("    💡 {}: {}", tool, hint);
        }
    }

    println!();
    println!("  Next steps:");
    println!("    1. Start coding — MUR injects patterns automatically via hooks");
    println!("    2. Run `mur context --file` to update context for file-based tools");
    println!("    3. Run `mur search <query>` to find patterns");
    if community_enabled {
        println!("    4. Run `mur login` to authenticate for community sharing");
        println!("    5. Run `mur community list` to browse community patterns");
    }
    println!();

    Ok(())
}
