//! Detection + recommendation flow for the "All local" path in `mur init`.
//!
//! Detects which local LLM runtimes are present on the host and presents
//! the user with a curated, multilingual-friendly model menu for whichever
//! backend wins the priority order:
//!
//!   oMLX.app (Apple Silicon)  >  mlx-lm CLI (Apple Silicon)  >  Ollama
//!
//! Both MLX backends serve an OpenAI-compatible HTTP API; they only
//! differ in launch flow and default port (oMLX → :8000, mlx-lm → :8080).

use std::io::{self, Write};
use std::path::Path;

use crate::discovery::aggregate::{MenuRowKind, build_llm_menu};
use crate::discovery::{Backend, DiscoveredModel};

#[derive(Debug, Clone, Copy)]
pub struct LocalRuntimes {
    pub ollama_installed: bool,
    pub ollama_running: bool,
    pub omlx_installed: bool,
    pub mlx_lm_installed: bool,
    pub apple_silicon: bool,
}

/// Write a discovered LLM model into the llm section of config.
/// Extracted for unit testability (no stdin).
fn apply_llm_model(config: &mut mur_common::config::Config, m: &DiscoveredModel) {
    match m.backend {
        Backend::Ollama => {
            config.llm.provider = "ollama".into();
            config.llm.model = m.id.clone();
            config.llm.api_key_env = None;
            config.llm.openai_url = None;
        }
        Backend::OMlx => {
            config.llm.provider = "openai".into();
            config.llm.model = m.id.clone();
            config.llm.api_key_env = Some("OMLX_API_KEY".into());
            config.llm.openai_url = Some("http://localhost:8000/v1".into());
        }
    }
}

/// Interactive LLM model selection using the discovery-based `build_llm_menu`.
///
/// Returns `Ok(true)` when config.llm was written, `Ok(false)` on Skip or
/// after triggering a pull (caller must re-run `mur init` to select the
/// newly pulled model).
///
pub fn select_local_llm(
    config: &mut mur_common::config::Config,
    available: &[DiscoveredModel],
) -> anyhow::Result<bool> {
    let rows = build_llm_menu(available);

    println!();
    println!("LLM model for pattern learning:");
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
            apply_llm_model(config, m);
            if m.backend == Backend::OMlx
                && std::env::var("OMLX_API_KEY").unwrap_or_default().is_empty()
            {
                println!();
                println!(
                    "  \u{26a0} Set OMLX_API_KEY before first use \
                     (any non-empty value works on localhost):"
                );
                println!("      export OMLX_API_KEY=local");
            }
            Ok(true)
        }
        MenuRowKind::Pull => {
            let pull_id = row.pull_id.as_ref().expect("pull rows always carry an id");
            // Ollama-style: lowercase first char and no '/' separator.
            // HF/oMLX-style: uppercase first char or contains '/'.
            let is_ollama_style = !pull_id.contains('/')
                && pull_id
                    .chars()
                    .next()
                    .map(|c| c.is_lowercase())
                    .unwrap_or(true);
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
                            "  \u{26a0} ollama pull exited with {}; LLM not configured.",
                            s
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        println!("  \u{26a0} Pull interrupted; re-run `mur init` to retry.");
                    }
                    Err(e) => {
                        println!(
                            "  \u{26a0} Could not invoke ollama: {e}; \
                             install from https://ollama.com"
                        );
                    }
                }
            } else {
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
            println!("  Keeping current LLM config.");
            Ok(false)
        }
    }
}

pub fn detect_local_runtimes() -> LocalRuntimes {
    let apple_silicon = cfg!(target_os = "macos") && cfg!(target_arch = "aarch64");
    LocalRuntimes {
        ollama_installed: which_exists("ollama"),
        ollama_running: ollama_running(),
        omlx_installed: apple_silicon && omlx_installed(),
        mlx_lm_installed: apple_silicon && mlx_lm_installed(),
        apple_silicon,
    }
}

fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ollama_running() -> bool {
    std::process::Command::new("ollama")
        .arg("list")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// oMLX is a native macOS app (https://omlx.ai). Look for the bundle in
/// either the system or user Applications folder.
fn omlx_installed() -> bool {
    if Path::new("/Applications/oMLX.app").exists() {
        return true;
    }
    if let Some(home) = dirs::home_dir()
        && home.join("Applications/oMLX.app").exists()
    {
        return true;
    }
    false
}

/// `mlx-lm` Python package — installed via `pip install mlx-lm`. Either
/// its CLI shims are on PATH or the module imports cleanly.
fn mlx_lm_installed() -> bool {
    if which_exists("mlx_lm.generate") || which_exists("mlx_lm.server") {
        return true;
    }
    std::process::Command::new("python3")
        .args(["-c", "import mlx_lm"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn print_runtime_summary(rt: &LocalRuntimes) {
    println!();
    if rt.ollama_running {
        println!("  ✓ Ollama detected (running)");
    } else if rt.ollama_installed {
        println!("  ✓ Ollama installed (daemon not running — start with `ollama serve`)");
    } else {
        println!("  ✗ Ollama not installed");
    }
    if rt.apple_silicon {
        if rt.omlx_installed {
            println!("  ✓ oMLX.app detected (Apple Silicon native)");
        } else {
            println!("  ✗ oMLX.app not installed (https://omlx.ai)");
        }
        if rt.mlx_lm_installed {
            println!("  ✓ mlx-lm CLI detected");
        } else {
            println!("  ✗ mlx-lm CLI not installed (`pip install mlx-lm`)");
        }
    }
}

pub fn print_install_help(apple_silicon: bool) {
    println!();
    println!("  ⚠ No local LLM runtime detected.");
    if apple_silicon {
        println!("    • oMLX.app: https://omlx.ai  (Apple Silicon native, GUI, recommended)");
        println!("    • mlx-lm:   pip install mlx-lm  (Apple Silicon native, CLI)");
        println!("    • Ollama:   https://ollama.com");
    } else {
        println!("    • Ollama: https://ollama.com");
    }
    println!("    Re-run `mur init` after installing.");
}

#[cfg(test)]
mod apply_llm_model_tests {
    use super::*;
    use crate::discovery::{Backend, DiscoveredModel, ModelKind};

    fn make_llm(id: &str, backend: Backend) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            backend,
            kind: ModelKind::Llm,
            dims: None,
            family: None,
            size_bytes: None,
            probed_at: None,
        }
    }

    #[test]
    fn ollama_backend_writes_ollama_provider() {
        let mut cfg = mur_common::config::Config::default();
        let m = make_llm("qwen3.5:4b", Backend::Ollama);
        apply_llm_model(&mut cfg, &m);
        assert_eq!(cfg.llm.provider, "ollama");
        assert_eq!(cfg.llm.model, "qwen3.5:4b");
        assert!(cfg.llm.api_key_env.is_none());
        assert!(cfg.llm.openai_url.is_none());
    }

    #[test]
    fn omlx_backend_writes_openai_provider_with_omlx_url() {
        let mut cfg = mur_common::config::Config::default();
        let m = make_llm("mlx-community/Qwen3.5-4B-4bit", Backend::OMlx);
        apply_llm_model(&mut cfg, &m);
        assert_eq!(cfg.llm.provider, "openai");
        assert_eq!(cfg.llm.model, "mlx-community/Qwen3.5-4B-4bit");
        assert_eq!(cfg.llm.api_key_env.as_deref(), Some("OMLX_API_KEY"));
        assert_eq!(
            cfg.llm.openai_url.as_deref(),
            Some("http://localhost:8000/v1")
        );
    }

    #[test]
    fn ollama_does_not_set_openai_url() {
        let mut cfg = mur_common::config::Config::default();
        cfg.llm.openai_url = Some("http://old-value".into());
        let m = make_llm("qwen3:4b", Backend::Ollama);
        apply_llm_model(&mut cfg, &m);
        assert!(cfg.llm.openai_url.is_none(), "Ollama must clear openai_url");
    }
}
