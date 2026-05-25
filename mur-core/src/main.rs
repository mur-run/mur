use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod a2a_dial;
mod auth;
mod bridge_keychain;
mod capture;
mod codebase;
// `--format card` is a D4 milestone; M2.7 only ships the schema + helper
// (callable from the library), so the bin target sees the types as unused.
#[allow(dead_code)]
mod character_card;
mod cli;
mod cmd;
mod community;
mod context_api;
mod conversations;
mod daemon;
mod dashboard;
mod dispatch;
// Discovery items (cache constants, LLM preference table, test-only fns) are
// defined for library consumers and tests; the binary only uses a subset.
#[allow(dead_code)]
mod discovery;
mod evolve;
mod executor;
mod extract;
mod extract_llm;
mod federation;
mod gep;
mod inject;
mod interactive;
mod paths;
mod retrieve;
mod server;
#[cfg(feature = "server")]
mod server_agents;
mod session;
#[allow(dead_code)]
mod skill_gen;
mod skill_stats;
#[cfg(feature = "sources")]
mod sources;
mod store;
mod sync;
mod team;
mod update;
mod verify;

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
    let cli = Cli::parse();
    dispatch::run(cli).await
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
