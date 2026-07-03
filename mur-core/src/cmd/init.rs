use anyhow::Result;
use std::io::{self, Write};

/// Drive an async future from a sync caller that's already inside the
/// `mur-main` multi-threaded tokio runtime (see `main.rs::main`).
///
/// `tokio::task::block_in_place` releases the current worker thread so we
/// can synchronously block on `Handle::current().block_on(future)`. We
/// cannot use `Runtime::new(...).block_on(...)` here because constructing a
/// new runtime inside an existing runtime panics ("Cannot start a runtime
/// from within a runtime").
fn block_on_in_runtime<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
}

/// Run embedding + LLM discovery against all detected local runtimes,
/// blocking the caller's thread. When `refresh` is true, the on-disk cache
/// is deleted first.
fn discover_blocking(refresh: bool) -> Result<Vec<crate::discovery::DiscoveredModel>> {
    if refresh {
        let cache_path = crate::discovery::cache::DiscoveryCache::default_path();
        let _ = std::fs::remove_file(&cache_path);
    }
    block_on_in_runtime(crate::discovery::run_all())
}

/// Best-effort hardcoded dims for known model ids when the `/v1/embeddings`
/// or `/api/embed` probe fails. Returns `None` for unknown ids.
fn fallback_dims_for(id: &str) -> Option<usize> {
    if id.contains("Qwen3-Embedding-0.6B") || id.contains("qwen3-embedding:0.6b") {
        Some(1024)
    } else if id.contains("Qwen3-Embedding-4B") || id.contains("qwen3-embedding:4b") {
        Some(2560)
    } else if id.contains("Qwen3-Embedding-8B") || id.contains("qwen3-embedding:8b") {
        Some(4096)
    } else if id.contains("bge-m3") {
        Some(1024)
    } else if id.contains("nomic-embed-text") || id.contains("embeddinggemma") {
        Some(768)
    } else {
        None
    }
}

fn apply_conversations_model(config: &mut mur_common::config::Config, model: &str) {
    config.conversations.ask.model = model.to_string();
    config.conversations.compact.extractive_model = model.to_string();
    config.conversations.rollup.extractive_model = model.to_string();
}

fn select_conversations_models(
    config: &mut mur_common::config::Config,
    available: &[crate::discovery::DiscoveredModel],
    default_model: &str,
) -> Result<()> {
    use crate::discovery::aggregate::build_llm_menu;

    let llm_rows = build_llm_menu(available);
    let best_local = llm_rows
        .first()
        .and_then(|r| r.model.as_ref())
        .map(|m| m.id.as_str())
        .unwrap_or(default_model);

    if llm_rows.is_empty() {
        apply_conversations_model(config, default_model);
        return Ok(());
    }

    println!();
    println!("Conversation models — compact / ask / rollup run locally even in cloud mode.");
    println!("  1) Use {}  [recommended]", best_local);
    println!("  2) Pick from discovered models");
    println!("  3) Skip — keep defaults");
    print!("Choose [1-3] (default: 1): ");
    io::stdout().flush()?;

    let mut s = String::new();
    match io::stdin().read_line(&mut s) {
        Ok(0) | Err(_) => return Ok(()),
        Ok(_) => {}
    }

    match s.trim() {
        "" | "1" => apply_conversations_model(config, best_local),
        "2" => {
            println!();
            for (i, r) in llm_rows.iter().enumerate() {
                println!("  {}) {}", i + 1, r.label);
            }
            print!("Choose [1-{}] (default: 1): ", llm_rows.len());
            io::stdout().flush()?;
            let mut s2 = String::new();
            match io::stdin().read_line(&mut s2) {
                Ok(0) | Err(_) => {
                    apply_conversations_model(config, best_local);
                    return Ok(());
                }
                Ok(_) => {}
            }
            let idx = s2
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|&n| n >= 1 && n <= llm_rows.len())
                .map(|n| n - 1)
                .unwrap_or(0);
            let chosen = llm_rows[idx]
                .model
                .as_ref()
                .map(|m| m.id.as_str())
                .unwrap_or(best_local);
            apply_conversations_model(config, chosen);
        }
        _ => {}
    }
    Ok(())
}

/// Select a local embedding model via discovery.
///
/// `available` is the merged list of discovered models from
/// `discover_blocking()`. Returns `Ok(true)` if config was written,
/// `Ok(false)` if the user picked Skip (or a [pull] row).
fn select_local_embedding(
    config: &mut mur_common::config::Config,
    available: &[crate::discovery::DiscoveredModel],
) -> Result<bool> {
    use crate::discovery::Backend;
    use crate::discovery::aggregate::{MenuRowKind, build_embedding_menu};

    let rows = build_embedding_menu(available);

    println!();
    println!("Embedding model — local discovery:");
    for (i, r) in rows.iter().enumerate() {
        println!("  {}) {}", i + 1, r.label);
    }
    print!("Choose [1-{}] (default: 1): ", rows.len());
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    let idx = s
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|&n| n >= 1 && n <= rows.len())
        .map(|n| n - 1)
        .unwrap_or(0);
    let row = &rows[idx];

    match row.kind {
        MenuRowKind::Auto | MenuRowKind::Pulled => {
            let m = row
                .model
                .as_ref()
                .expect("auto/pulled rows always carry a model");
            // If dims weren't populated at discovery time (oMLX entries before
            // probe), issue a 1-token /v1/embeddings call to learn them now.
            let dims = match m.dims {
                Some(d) => d,
                None => {
                    use crate::discovery::Discovery as _;
                    let probe = match m.backend {
                        Backend::Ollama => {
                            let d = crate::discovery::ollama::OllamaDiscovery::new(
                                "http://localhost:11434",
                            );
                            block_on_in_runtime(d.probe_embedding(&m.id))
                        }
                        Backend::OMlx => {
                            let d = crate::discovery::omlx::OMlxDiscovery::with_api_key(
                                "http://localhost:8000/v1",
                                crate::discovery::resolve_omlx_api_key(),
                            );
                            block_on_in_runtime(d.probe_embedding(&m.id))
                        }
                    };
                    match probe {
                        Ok(p) => p.dims,
                        Err(e) => {
                            println!(
                                "  \u{26a0} Probe failed: {e}; using preference-table fallback"
                            );
                            fallback_dims_for(&m.id).unwrap_or(1024)
                        }
                    }
                }
            };
            match m.backend {
                Backend::Ollama => {
                    config.embedding.provider = "ollama".into();
                    config.embedding.model = m.id.clone();
                    config.embedding.dimensions = dims;
                    config.embedding.api_key_env = None;
                    config.embedding.openai_url = None;
                }
                Backend::OMlx => {
                    config.embedding.provider = "omlx".into();
                    config.embedding.model = m.id.clone();
                    config.embedding.dimensions = dims;
                    config.embedding.api_key_env = Some("OMLX_API_KEY".into());
                    config.embedding.openai_url = Some("http://localhost:8000/v1".into());
                    if std::env::var("OMLX_API_KEY").unwrap_or_default().is_empty() {
                        println!();
                        println!(
                            "  \u{26a0} Set OMLX_API_KEY before first use (any non-empty value works on localhost):"
                        );
                        println!("      export OMLX_API_KEY=local");
                    }
                }
            }
            Ok(true)
        }
        MenuRowKind::Pull => {
            let pull_id = row.pull_id.as_ref().expect("pull rows always carry an id");
            // Heuristic: Ollama-style tags contain ':' or are short known IDs;
            // HF ids contain '/' and are routed to oMLX (no CLI pull).
            let is_ollama_style = !pull_id.contains('/')
                && (pull_id.contains(':')
                    || matches!(
                        pull_id.as_str(),
                        "bge-m3" | "nomic-embed-text" | "all-minilm" | "embeddinggemma"
                    ));
            if is_ollama_style {
                println!();
                println!("  Pulling {} via Ollama...", pull_id);
                let st = std::process::Command::new("ollama")
                    .arg("pull")
                    .arg(pull_id)
                    .status();
                match st {
                    Ok(s) if s.success() => {
                        println!("  \u{2713} Pulled. Re-run `mur init` to select it.");
                    }
                    Ok(s) => {
                        println!(
                            "  \u{26a0} ollama pull exited with {}; embedding not configured.",
                            s
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        println!("  \u{26a0} Pull interrupted; re-run `mur init` to retry.");
                    }
                    Err(e) => {
                        println!(
                            "  \u{26a0} Could not invoke ollama: {e}; install from https://ollama.com"
                        );
                    }
                }
            } else {
                // HF id form — oMLX path; oMLX has no CLI pull mechanism.
                println!();
                println!(
                    "  Open oMLX.app \u{2192} Models \u{2192} search '{}' \u{2192} Pull",
                    pull_id
                );
                println!("  Then re-run `mur init`.");
            }
            Ok(false)
        }
        MenuRowKind::Skip => {
            println!("  Keeping current embedding config.");
            Ok(false)
        }
    }
}

pub(crate) const HOOK_SCRIPT_PROMPT: &str = "#!/bin/bash
# mur-managed-hook v7 — generated by `mur init --hooks`
exec mur hook prompt --tool \"${MUR_TOOL:-claude}\"
";

pub(crate) const HOOK_SCRIPT_TOOL: &str = "#!/bin/bash
# mur-managed-hook v7 — generated by `mur init --hooks`
exec mur hook tool --tool \"${MUR_TOOL:-claude}\"
";

pub(crate) const HOOK_SCRIPT_STOP: &str = "#!/bin/bash
# mur-managed-hook v7 — generated by `mur init --hooks`
exec mur hook stop --tool \"${MUR_TOOL:-claude}\"
";

pub(crate) const HOOK_SCRIPT_SESSION_START: &str = "#!/bin/bash
# mur-managed-hook v7 — generated by `mur init --hooks`
exec mur hook session-start --tool \"${MUR_TOOL:-claude}\"
";

/// Derive Claude Code async flags for a given hook event name.
///
/// Returns `(async_flag, rewake_flag)`:
/// - `async_flag=true` for `UserPromptSubmit` (async execution)
/// - `rewake_flag=true` for `Stop` (async re-wake after completion)
/// - Both false for other events
fn hook_async_flags(event_name: &str) -> (bool, bool) {
    (
        matches!(event_name, "UserPromptSubmit"),
        matches!(event_name, "Stop"),
    )
}

pub(crate) fn cmd_init(hooks_flag: bool, refresh_discovery: bool) -> Result<()> {
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
        crate::store::config::save_config(&mur_common::config::Config::default())?;
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
        let on_prompt = HOOK_SCRIPT_PROMPT;
        let on_tool = HOOK_SCRIPT_TOOL;
        let on_stop = HOOK_SCRIPT_STOP;

        let hooks = [
            ("on-prompt.sh", on_prompt),
            ("on-tool.sh", on_tool),
            ("on-stop.sh", on_stop),
            ("on-session-start.sh", HOOK_SCRIPT_SESSION_START),
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
                "PreToolUse",
                hooks_dir.join("on-tool.sh").to_string_lossy().to_string(),
            ),
            (
                "PostToolUse",
                hooks_dir.join("on-tool.sh").to_string_lossy().to_string(),
            ),
            (
                "Stop",
                hooks_dir.join("on-stop.sh").to_string_lossy().to_string(),
            ),
            (
                "SessionStart",
                hooks_dir
                    .join("on-session-start.sh")
                    .to_string_lossy()
                    .to_string(),
            ),
        ];

        let hooks_obj = settings
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Claude settings.json is not a JSON object"))?
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

            // Add our hook with Claude Code async flags
            let (async_flag, rewake_flag) = hook_async_flags(event_name);
            let mut hook_entry = serde_json::json!({
                "hooks": [{
                    "type": "command",
                    "command": format!("bash {}", script_path),
                }],
                "matcher": ""
            });
            if async_flag {
                hook_entry["hooks"][0]["async"] = serde_json::json!(true);
            }
            if rewake_flag {
                hook_entry["hooks"][0]["asyncRewake"] = serde_json::json!(true);
            }
            arr.push(hook_entry);
        }

        // Write settings back with pretty formatting
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;

        hooks_installed.push("Claude Code");

        // Install murmurd as login service
        let murmurd_bin = super::init_daemon::murmurd_bin_path();
        match super::init_daemon::install_daemon_service(&murmurd_bin) {
            Ok(true) => println!("  murmurd autostart installed (login service)."),
            Ok(false) => println!(
                "  murmurd autostart: unsupported platform.\n  Run `mur murmurd start --detach` manually."
            ),
            Err(e) => eprintln!("  murmurd install warning: {e:#}"),
        }
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
        let _ = super::sync_cmd::ensure_mur_skill(&home, &mur_common::trust::mur_home());
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
            println!("  1) OpenRouter (recommended — access to many models, one API key)");
            println!("  2) OpenAI");
            println!("  3) Gemini");
            println!("  4) Anthropic");
            print!("Choose [1/2/3/4] (default: 1): ");
            io::stdout().flush()?;
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;

            let provider_choice = choice.trim();

            // Show model recommendations for the chosen provider
            match provider_choice {
                "2" => {
                    println!();
                    println!("OpenAI models:");
                    println!("  Best quality:  gpt-4o          ($2.50/$10 per 1M tokens)");
                    println!("  Best value:    gpt-4o-mini     ($0.15/$0.60 per 1M tokens) ← default");
                }
                "3" => {
                    println!();
                    println!("Gemini models:");
                    println!("  Best quality:  gemini-2.5-pro  ($1.25/$10 per 1M tokens)");
                    println!("  Best value:    gemini-2.5-flash ($0.15/$0.60 per 1M tokens) ← default");
                }
                "4" => {
                    println!();
                    println!("Anthropic models:");
                    println!("  Best quality:  claude-opus-4.6   ($15/$75 per 1M tokens) ← default");
                    println!("  Best value:    claude-sonnet-4.6  ($3/$15 per 1M tokens)");
                    println!("  Budget:        claude-haiku-4.5   ($0.80/$4 per 1M tokens)");
                }
                _ => {
                    println!();
                    println!("OpenRouter — recommended models:");
                    println!("  Best quality:  anthropic/claude-sonnet-4  ($3/$15 per 1M tokens)");
                    println!("  Best value:    google/gemini-2.5-flash    ($0.15/$0.60 per 1M tokens) ← default");
                    println!("  Budget:        google/gemini-2.0-flash    ($0.10/$0.40 per 1M tokens)");
                }
            }
            println!();
            println!("  Tip: You can change the model later in ~/.mur/config.yaml");

            let (provider, llm_model, env_var, is_openrouter) = match provider_choice {
                "2" => ("openai", "gpt-4o-mini", "OPENAI_API_KEY", false),
                "3" => ("gemini", "gemini-2.5-flash", "GEMINI_API_KEY", false),
                "4" => (
                    "anthropic",
                    "claude-opus-4-6",
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

    // Helper: select cloud embedding provider (cloud LLM path).
    // When the user picks "Local", delegates to `select_local_embedding`.
    let select_cloud_embedding = |config: &mut mur_common::config::Config,
                                  llm_provider: &str,
                                  llm_env_var: &str|
     -> Result<()> {
        println!();
        println!("Embedding provider:");
        let cloud_label = match llm_provider {
            "openai" => "OpenAI \u{2014} text-embedding-3-small (same API key)",
            "gemini" => "Gemini \u{2014} text-embedding-004 (same API key)",
            "anthropic" => "Voyage \u{2014} voyage-3-lite (same API key)",
            _ => "Cloud embedding",
        };
        println!("  1) {} (recommended)", cloud_label);
        println!("  2) Local (Ollama / oMLX) \u{2014} free, no API dependency");
        print!("Choose [1/2] (default: 1): ");
        io::stdout().flush()?;
        let mut choice = String::new();
        io::stdin().read_line(&mut choice)?;

        if choice.trim() == "2" {
            // Delegate to discovery-based local embedding selection
            let available = discover_blocking(refresh_discovery)?;
            select_local_embedding(config, &available)?;
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
            let available = discover_blocking(refresh_discovery)?;
            select_local_embedding(&mut config, &available)?;

            // conversations — background tasks always use local models
            {
                use crate::discovery::aggregate::build_llm_menu;
                let default_llm = build_llm_menu(&available)
                    .into_iter()
                    .next()
                    .and_then(|r| r.model)
                    .map(|m| m.id)
                    .unwrap_or_else(|| mur_common::config::DEFAULT_LOCAL_LLM_MODEL.to_string());
                select_conversations_models(&mut config, &available, &default_llm)?;
            }

            crate::store::config::save_config(&config)?;
            let llm_display = if is_openrouter {
                "openrouter"
            } else {
                _provider
            };
            println!(
                "  \u{2713} Config: {} (LLM) + {}/{} (search) / {}",
                llm_display, config.embedding.provider, config.embedding.model, llm_model
            );
        }
        "2" => {
            // All cloud
            let (provider, env_var, llm_model, is_openrouter) = select_cloud_llm(&mut config)?;

            if is_openrouter {
                // OpenRouter doesn't offer embeddings, use cloud or local discovery
                println!();
                println!("  \u{2139} OpenRouter doesn't provide embedding APIs.");
                println!("    Pick a separate embedding provider:");
                println!("  1) OpenAI \u{2014} text-embedding-3-small (requires OPENAI_API_KEY)");
                println!("  2) Local (Ollama / oMLX) \u{2014} free");
                print!("Choose [1/2] (default: 1): ");
                io::stdout().flush()?;
                let mut choice = String::new();
                io::stdin().read_line(&mut choice)?;

                if choice.trim() == "2" {
                    let available = discover_blocking(refresh_discovery)?;
                    select_local_embedding(&mut config, &available)?;
                } else {
                    config.embedding.provider = "openai".to_string();
                    config.embedding.model = "text-embedding-3-small".to_string();
                    config.embedding.dimensions = 1536;
                    config.embedding.api_key_env = Some("OPENAI_API_KEY".to_string());
                    config.embedding.openai_url = None;
                    if std::env::var("OPENAI_API_KEY").is_err() {
                        println!(
                            "  \u{26a0} OPENAI_API_KEY not set \u{2014} set it for embedding to work"
                        );
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
            use crate::cmd::init_local::{
                detect_local_runtimes, print_install_help, print_runtime_summary, select_local_llm,
            };

            let runtimes = detect_local_runtimes();
            print_runtime_summary(&runtimes);

            if !runtimes.ollama_installed && !runtimes.omlx_installed && !runtimes.mlx_lm_installed
            {
                print_install_help(runtimes.apple_silicon);
            } else {
                let available = discover_blocking(refresh_discovery)?;
                let wrote_llm = select_local_llm(&mut config, &available)?;
                if wrote_llm {
                    select_local_embedding(&mut config, &available)?;

                    // conversations — use the same local model
                    let chosen_llm = config.llm.model.clone();
                    select_conversations_models(&mut config, &available, &chosen_llm)?;

                    crate::store::config::save_config(&config)?;
                    println!(
                        "  \u{2713} Config: {}/{} (LLM) + {}/{} (search)",
                        config.llm.provider,
                        config.llm.model,
                        config.embedding.provider,
                        config.embedding.model
                    );
                }
            }
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
        println!("  Run `mur auth login` to authenticate and start sharing patterns.");
    }

    // ─── Step I: Device sync setup ──────────────────────────────
    println!();
    println!("Device sync — keep patterns in sync across machines:");
    println!();
    println!("  1) Cloud sync (recommended)");
    println!("     Auto conflict resolution, 3-month free trial, just works.");
    println!("  2) Git sync (free)");
    println!("     Use your own git repo to sync ~/.mur/patterns/");
    println!("  3) Skip (local only)");
    print!("Choose [1/2/3] (default: 1): ");
    io::stdout().flush()?;
    let mut sync_choice = String::new();
    io::stdin().read_line(&mut sync_choice)?;
    let sync_choice = sync_choice.trim();

    config = crate::store::config::load_config().unwrap_or(config);

    match sync_choice {
        "" | "1" => {
            config.sync.method = "cloud".to_string();
            config.sync.auto = true;
            config.sync.git_remote = None;
            crate::store::config::save_config(&config)?;
            println!("  ✓ Cloud sync enabled (auto-sync on).");
            println!("  Run `mur auth login` to authenticate and activate sync.");
        }
        "2" => {
            config.sync.method = "git".to_string();
            config.sync.auto = true;
            print!("  Git remote URL (e.g. git@github.com:you/mur-data.git): ");
            io::stdout().flush()?;
            let mut remote_url = String::new();
            io::stdin().read_line(&mut remote_url)?;
            let remote_url = remote_url.trim().to_string();
            if !remote_url.is_empty() {
                config.sync.git_remote = Some(remote_url.clone());
                println!("  ✓ Git sync enabled with remote: {}", remote_url);
            } else {
                println!(
                    "  ⚠ No remote URL provided. Set sync.git_remote in ~/.mur/config.yaml later."
                );
            }
            crate::store::config::save_config(&config)?;
        }
        _ => {
            config.sync.method = "local".to_string();
            config.sync.auto = false;
            config.sync.git_remote = None;
            crate::store::config::save_config(&config)?;
            println!("  Keeping local only. Run `mur init` again to enable sync later.");
        }
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
        println!("    4. Run `mur auth login` to authenticate for community sharing");
        println!("    5. Run `mur community list` to browse community patterns");
    }
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_scripts_use_unified_entry() {
        assert!(
            HOOK_SCRIPT_PROMPT.contains("mur hook prompt"),
            "on-prompt.sh must call mur hook prompt"
        );
        assert!(
            !HOOK_SCRIPT_PROMPT.contains("mur context"),
            "on-prompt.sh must NOT call mur context"
        );
        assert!(
            HOOK_SCRIPT_TOOL.contains("mur hook tool"),
            "on-tool.sh must call mur hook tool"
        );
        assert!(
            HOOK_SCRIPT_STOP.contains("mur hook stop"),
            "on-stop.sh must call mur hook stop"
        );
        assert!(
            HOOK_SCRIPT_SESSION_START.contains("mur hook session-start"),
            "on-session-start.sh must call mur hook session-start"
        );
        assert!(
            HOOK_SCRIPT_PROMPT.contains("v7"),
            "hook scripts must be version v7"
        );
    }
}

#[cfg(test)]
mod select_local_embedding_tests {
    use crate::discovery::aggregate::{MenuRowKind, build_embedding_menu};
    use crate::discovery::{Backend, DiscoveredModel, ModelKind};

    fn ollama_qwen() -> DiscoveredModel {
        DiscoveredModel {
            id: "qwen3-embedding:0.6b".into(),
            backend: Backend::Ollama,
            kind: ModelKind::Embedding,
            dims: Some(1024),
            family: Some("bert".into()),
            size_bytes: None,
            probed_at: None,
        }
    }

    /// Auto row, when chosen, must carry enough info to write
    /// `cfg.embedding.{provider, model, dimensions, ollama_endpoint}`.
    #[test]
    fn auto_row_carries_full_model_info() {
        let rows = build_embedding_menu(&[ollama_qwen()]);
        let auto = rows.iter().find(|r| r.kind == MenuRowKind::Auto).unwrap();
        let m = auto.model.as_ref().unwrap();
        assert_eq!(m.backend, Backend::Ollama);
        assert_eq!(m.id, "qwen3-embedding:0.6b");
        assert_eq!(m.dims, Some(1024));
    }
}

#[cfg(test)]
mod refresh_flag_tests {
    /// Compile-time check that `cmd_init` accepts the new `refresh_discovery` flag.
    #[test]
    fn cmd_init_accepts_refresh_discovery() {
        let _ = super::cmd_init as fn(bool, bool) -> anyhow::Result<()>;
    }
}

#[cfg(test)]
mod cloud_embedding_tests {
    use crate::store::embedding::{EmbeddingConfig, EmbeddingProvider};
    use mur_common::config::Config;

    /// When user picks Anthropic as cloud LLM and Voyage as embedding,
    /// embedding.openai_url must be None (Voyage uses its own canonical
    /// URL, not OpenRouter's).
    #[test]
    fn cloud_embedding_does_not_inherit_llm_openai_url() {
        let mut cfg = Config::default();
        cfg.llm.provider = "openai".into();
        cfg.llm.openai_url = Some("https://openrouter.ai/api/v1".into());
        // simulate what select_cloud_embedding does for anthropic→voyage:
        cfg.embedding.provider = "anthropic".into();
        cfg.embedding.model = "voyage-3-lite".into();
        cfg.embedding.api_key_env = Some("ANTHROPIC_API_KEY".into());
        cfg.embedding.openai_url = None;

        // Round-trip through EmbeddingConfig — should resolve OpenAI variant
        // with default base_url (api.openai.com), NOT OpenRouter.
        let ec = EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            EmbeddingProvider::OpenAI { base_url, .. } => {
                assert_eq!(base_url, "https://api.openai.com/v1");
            }
            _ => panic!("expected OpenAI variant"),
        }
    }
}

#[cfg(test)]
mod runtime_regression_tests {
    //! Regression: `discover_blocking` and the dims-probe path are called
    //! synchronously from `cmd_init`, which itself runs inside the
    //! mur-main multi-threaded tokio runtime (see `main.rs::main`).
    //!
    //! Earlier M5 code constructed a NEW `tokio::runtime::Runtime` and
    //! called `.block_on(...)` on it — that panics with "Cannot start a
    //! runtime from within a runtime" because runtime construction inside
    //! an existing runtime is forbidden. The fix uses
    //! `tokio::task::block_in_place` + `Handle::current().block_on`.
    //!
    //! These tests reproduce the original panic conditions to make sure
    //! the bug doesn't return.
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn block_on_in_runtime_does_not_panic_inside_existing_runtime() {
        // The bug: building a new Runtime here would panic.
        // The fix: block_on_in_runtime uses Handle::current().block_on
        // via block_in_place, which is safe.
        let result = block_on_in_runtime(async { 42 });
        assert_eq!(result, 42);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn discover_blocking_does_not_panic_inside_existing_runtime() {
        // discover_blocking calls block_on_in_runtime; this is the exact
        // path cmd_init runs through. Either no local runtimes are detected
        // (returns empty Vec) or some are present (returns their models) —
        // either way it must not panic.
        let result = discover_blocking(false);
        assert!(
            result.is_ok(),
            "discover_blocking should not error inside a tokio runtime: {:?}",
            result.err()
        );
    }
}

#[cfg(test)]
mod claude_hook_flags_tests {
    use super::hook_async_flags;
    use std::collections::HashMap;

    #[test]
    fn claude_hooks_have_async_flags() {
        let events = [
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "SessionStart",
        ];
        let mut results: HashMap<&str, serde_json::Value> = HashMap::new();
        for event_name in events {
            let (async_flag, rewake_flag) = hook_async_flags(event_name);
            let mut hook_entry = serde_json::json!({
                "hooks": [{"type": "command", "command": "bash /tmp/hook.sh"}],
                "matcher": ""
            });
            if async_flag {
                hook_entry["hooks"][0]["async"] = serde_json::json!(true);
            }
            if rewake_flag {
                hook_entry["hooks"][0]["asyncRewake"] = serde_json::json!(true);
            }
            results.insert(event_name, hook_entry);
        }
        assert_eq!(
            results["UserPromptSubmit"]["hooks"][0]["async"],
            serde_json::json!(true)
        );
        assert_eq!(
            results["Stop"]["hooks"][0]["asyncRewake"],
            serde_json::json!(true)
        );
        assert!(results["PreToolUse"]["hooks"][0].get("async").is_none());
        assert!(
            results["PostToolUse"]["hooks"][0]
                .get("asyncRewake")
                .is_none()
        );
        assert!(
            results["SessionStart"]["hooks"][0].get("async").is_none(),
            "SessionStart should not have async flag"
        );
        assert!(
            results["SessionStart"]["hooks"][0]
                .get("asyncRewake")
                .is_none(),
            "SessionStart should not have asyncRewake flag"
        );
    }
}
