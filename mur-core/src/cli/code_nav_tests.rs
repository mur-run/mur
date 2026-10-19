//! `mur code-nav` argument parsing.

use clap::Parser;

use super::actions::CodeNavAction;
use super::{Cli, Commands};

const BASE: [&str; 6] = ["mur", "code-nav", "setup", "--agent", "a", "--with-serena"];

fn parse(extra: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from([&BASE[..], extra].concat())
}

/// #1992: `--lsp-only` is repeatable.
#[test]
fn lsp_only_is_repeatable() {
    let cli = parse(&["--lsp-only", "rust", "--lsp-only", "typescript"]).unwrap();
    let Commands::CodeNav {
        action: CodeNavAction::Setup { lsp_only, lsp, .. },
    } = cli.command
    else {
        panic!("not code-nav setup");
    };
    assert_eq!(lsp_only, ["rust", "typescript"]);
    assert!(lsp.is_empty());
}

/// #1992: one run either adds (`--lsp`) or replaces (`--lsp-only`).
#[test]
fn lsp_only_conflicts_with_lsp() {
    assert!(parse(&["--lsp", "rust", "--lsp-only", "rust"]).is_err());
}
