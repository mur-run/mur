//! Detection + recommendation flow for the "All local" path in `mur init`.
//!
//! Detects which local LLM runtimes are present on the host
//! (Ollama everywhere; MLX on Apple Silicon) and presents the user with
//! a curated, multilingual-friendly model menu for whichever backend
//! they pick.

use anyhow::Result;
use std::io::{self, Write};

#[derive(Debug, Clone, Copy)]
pub struct LocalRuntimes {
    pub ollama_installed: bool,
    pub ollama_running: bool,
    pub mlx_installed: bool,
    pub apple_silicon: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelRec {
    pub id: &'static str,
    pub display: &'static str,
    pub ram_gb: f32,
    pub note: &'static str,
}

// Curated 2026-Q2 picks. Multilingual-first ordering — leading option must
// handle Chinese well because mur has a meaningful zh user base.
pub const OLLAMA_RECS: &[ModelRec] = &[
    ModelRec {
        id: "qwen3.5:4b",
        display: "qwen3.5:4b",
        ram_gb: 3.5,
        note: "multilingual, 256K context",
    },
    ModelRec {
        id: "gemma4:e2b",
        display: "gemma4:e2b",
        ram_gb: 7.2,
        note: "Google frontier MoE, multimodal",
    },
    ModelRec {
        id: "qwen3.5:9b",
        display: "qwen3.5:9b",
        ram_gb: 6.6,
        note: "Qwen larger, multilingual",
    },
];

pub const MLX_RECS: &[ModelRec] = &[
    ModelRec {
        id: "mlx-community/Qwen3.5-4B-4bit",
        display: "Qwen3.5-4B (MLX 4bit)",
        ram_gb: 3.0,
        note: "multilingual, 256K context",
    },
    ModelRec {
        id: "mlx-community/gemma-4-e2b-4bit",
        display: "Gemma4-E2B (MLX 4bit)",
        ram_gb: 4.5,
        note: "Google frontier, multimodal",
    },
    ModelRec {
        id: "mlx-community/Qwen3.5-9B-4bit",
        display: "Qwen3.5-9B (MLX 4bit)",
        ram_gb: 5.5,
        note: "better quality",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalBackend {
    Ollama,
    Mlx,
}

pub fn detect_local_runtimes() -> LocalRuntimes {
    let apple_silicon = cfg!(target_os = "macos") && cfg!(target_arch = "aarch64");
    LocalRuntimes {
        ollama_installed: which_exists("ollama"),
        ollama_running: ollama_running(),
        mlx_installed: apple_silicon && mlx_installed(),
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

fn mlx_installed() -> bool {
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
        if rt.mlx_installed {
            println!("  ✓ MLX detected (Apple Silicon native)");
        } else {
            println!("  ✗ MLX not installed (Apple Silicon supports it — `pip install mlx-lm`)");
        }
    }
}

pub fn print_install_help(apple_silicon: bool) {
    println!();
    println!("  ⚠ No local LLM runtime detected.");
    if apple_silicon {
        println!("    • Ollama: https://ollama.com");
        println!("    • MLX:    pip install mlx-lm  (Apple Silicon native, ~15-30% faster)");
    } else {
        println!("    • Ollama: https://ollama.com");
    }
    println!("    Re-run `mur init` after installing.");
}

/// Pick the local backend. On Apple Silicon, MLX always wins when present
/// (~15-30% faster, lower memory) — we don't waste a prompt on it. Returns
/// `None` only when no runtime is installed.
pub fn prompt_backend(rt: &LocalRuntimes) -> Result<Option<LocalBackend>> {
    print_runtime_summary(rt);

    match (rt.ollama_installed, rt.mlx_installed) {
        (false, false) => {
            print_install_help(rt.apple_silicon);
            Ok(None)
        }
        (_, true) => {
            // MLX available → always prefer it on Apple Silicon. To override,
            // uninstall MLX or edit ~/.mur/config.yaml after init.
            println!();
            println!("  → Using MLX (Apple Silicon native).");
            Ok(Some(LocalBackend::Mlx))
        }
        (true, false) => Ok(Some(LocalBackend::Ollama)),
    }
}

/// Render a numbered model menu and read the user's choice. Returns the
/// chosen `ModelRec`. Defaults to the first entry on empty / invalid input.
pub fn select_model(recs: &[ModelRec]) -> Result<&ModelRec> {
    println!();
    println!("LLM model for pattern learning:");
    for (i, r) in recs.iter().enumerate() {
        println!(
            "  {}) {:<32} — {}, ~{:.1}GB RAM",
            i + 1,
            r.display,
            r.note,
            r.ram_gb
        );
    }
    print!("Choose [1-{}] (default: 1): ", recs.len());
    io::stdout().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    let idx = s
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|&n| n >= 1 && n <= recs.len())
        .map(|n| n - 1)
        .unwrap_or(0);
    Ok(&recs[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rec_lists_nonempty_and_well_formed() {
        for recs in [OLLAMA_RECS, MLX_RECS] {
            assert!(!recs.is_empty(), "rec list must not be empty");
            for r in recs {
                assert!(!r.id.is_empty(), "id required");
                assert!(!r.display.is_empty(), "display required");
                assert!(!r.note.is_empty(), "note required");
                assert!(r.ram_gb > 0.0, "ram_gb must be positive");
            }
        }
    }

    #[test]
    fn mlx_ids_use_mlx_community_prefix() {
        for r in MLX_RECS {
            assert!(
                r.id.starts_with("mlx-community/"),
                "MLX recs must reference mlx-community/* on HF: got {}",
                r.id
            );
        }
    }

    #[test]
    fn ollama_ids_use_tag_form() {
        for r in OLLAMA_RECS {
            assert!(
                r.id.contains(':'),
                "Ollama model ids should be `name:tag`: got {}",
                r.id
            );
        }
    }
}
