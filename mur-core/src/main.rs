use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod a2a_dial;
mod agent_wizard;
mod auth;
mod bridge_keychain;
mod capabilities;
mod capture;
mod channel_verify;
mod channel_writer;
mod codebase;
// `--format card` is a D4 milestone; M2.7 only ships the schema + helper
// (callable from the library), so the bin target sees the types as unused.
#[allow(dead_code)]
mod character_card;
mod cli;
mod cmd;
mod community;
// Shared with the daemon + lib; the binary uses only a subset of the helpers.
mod context_api;
mod conversations;
#[allow(dead_code)]
mod cross_agent;
mod daemon;
mod dashboard;
mod dispatch;
#[allow(dead_code)]
mod mobile;
mod model_discovery;
mod model_prices;
#[allow(dead_code)]
mod model_setup;
// Discovery items (cache constants, LLM preference table, test-only fns) are
// defined for library consumers and tests; the binary only uses a subset.
mod action_pipeline;
#[allow(dead_code)]
mod discovery;
mod evolve;
mod executor;
mod extract;
mod extract_llm;
#[allow(dead_code)]
mod federation;
mod harvest;
mod hitl;
mod inject;
#[allow(dead_code)]
mod install_request;
mod interactive;
#[allow(dead_code, unused_imports)]
mod nudge;
mod official;
mod open_items;
mod parallel;
mod paths;
// wired via dispatch in the lib crate; this bin re-declaration is unused.
#[allow(dead_code)]
mod recommend;
mod retrieve;
mod route;
// wired via dispatch in the lib crate; this bin re-declaration is unused.
#[allow(dead_code)]
mod schedule_status;
mod server;
#[cfg(feature = "server")]
mod server_agents;
mod session;
mod skill_consolidate;
#[allow(dead_code)]
mod skill_gen;
#[allow(dead_code)]
mod skill_index;
mod skill_lifecycle;
mod skill_llm;
mod skill_repair;
mod skill_stats;
mod skill_traces;
#[cfg(feature = "sources")]
mod sources;
mod store;
#[allow(dead_code)]
mod sync;
mod team;
mod update;
mod verify;
mod yaml_edit;

use cli::Cli;

/// Load .env file from `~/.mur/.env` (preferred), `~/.mur/commander/.env`, or `./.env` (fallback).
fn load_dotenv() {
    let home = dirs::home_dir();
    if let Some(path) = home.as_ref().map(|h| h.join(".mur").join(".env"))
        && path.exists()
        && dotenvy::from_path(&path).is_ok()
    {
        return;
    }
    if let Some(path) = home
        .as_ref()
        .map(|h| h.join(".mur").join("commander").join(".env"))
        && path.exists()
        && dotenvy::from_path(&path).is_ok()
    {
        return;
    }
    let _ = dotenvy::dotenv();
}

/// Windows has a 1 MB default main-thread stack, vs 8 MB on Unix. Our `async fn
/// async_main` Future is large (many subcommand handler states combined via
/// `match cli.command`), and blew the stack on Windows CI. Move the runtime
/// onto a worker thread with an explicit 8 MB stack to match Unix behavior.
fn main() -> Result<()> {
    std::thread::Builder::new()
        .name("mur-main".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime")
                .block_on(async_main())
        })
        .expect("spawn main worker thread")
        .join()
        .expect("mur-main thread panicked")
}

async fn async_main() -> Result<()> {
    load_dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("warn,lance=warn,lancedb=warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = parse_cli()?;
    dispatch::run(cli).await
}

/// Parse argv, honoring the `murmur` symlink: `murmur <names…>` is
/// rewritten to `mur agent cli <names…>`. `murmur` with no agent name
/// falls back to the concierge agent, or lists agents and exits.
fn parse_cli() -> Result<Cli> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if cli::murmur::is_murmur_invocation(args.first()) {
        let home = cmd::agent::resolve_mur_home()?;
        let concierge = home
            .join("agents")
            .join("mur")
            .join("profile.yaml")
            .is_file();
        return match cli::murmur::map_args(&args[1..], concierge) {
            Some(argv) => Ok(Cli::parse_from(argv)),
            None => {
                eprintln!("murmur: no agent name given and no concierge agent installed.");
                eprintln!("Available agents:");
                let _ = cmd::agent::cmd_list(false);
                std::process::exit(2);
            }
        };
    }
    Ok(Cli::parse())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Commands;
    use clap::Parser;

    /// Helper: parse a full argv and return the `Commands::Ask` variant fields
    /// we care about. Panics on any other variant.
    fn parse_ask(argv: &[&str]) -> (bool, Option<String>) {
        let cli = Cli::try_parse_from(argv).expect("parse argv");
        match cli.command {
            Commands::Ask {
                no_summarize,
                summarize_model,
                ..
            } => (no_summarize, summarize_model),
            _ => panic!("expected Ask variant"),
        }
    }

    #[test]
    fn ask_parses_no_summarize_flag() {
        let (no_summarize, model) = parse_ask(&["mur", "ask", "q?", "--no-summarize"]);
        assert!(no_summarize);
        assert!(model.is_none());
    }

    #[test]
    fn ask_parses_summarize_model_flag() {
        let (no_summarize, model) =
            parse_ask(&["mur", "ask", "q?", "--summarize-model", "qwen3:4b"]);
        assert!(!no_summarize);
        assert_eq!(model.as_deref(), Some("qwen3:4b"));
    }

    #[test]
    fn ask_rejects_no_summarize_with_summarize_model() {
        // clap `conflicts_with` on --no-summarize guards this.
        let r = Cli::try_parse_from([
            "mur",
            "ask",
            "q?",
            "--no-summarize",
            "--summarize-model",
            "qwen3:4b",
        ]);
        assert!(r.is_err(), "clap must reject the conflict");
        let msg = r.err().unwrap().to_string();
        assert!(
            msg.contains("--summarize-model") || msg.contains("summarize-model"),
            "error should mention the conflicting flag; got: {msg}"
        );
    }
}
