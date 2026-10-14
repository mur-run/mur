//! Parse tests for `mur channel ...`, kept out of `cli/mod.rs` (800-line cap).

use super::{Cli, Commands};
use crate::cli::actions::ChannelAction;
use clap::Parser;

#[test]
fn cli_parses_channel_verify() {
    let cli = Cli::try_parse_from(["mur", "channel", "verify", "a", "b", "--json"]).unwrap();
    match cli.command {
        Commands::Channel {
            action: ChannelAction::Verify { channel_ids, json },
        } => {
            assert_eq!(channel_ids, ["a", "b"]);
            assert!(json);
        }
        _ => panic!("expected channel verify"),
    }
    let cli = Cli::try_parse_from(["mur", "channel", "verify"]).unwrap();
    match cli.command {
        Commands::Channel {
            action: ChannelAction::Verify { channel_ids, json },
        } => assert!(channel_ids.is_empty() && !json),
        _ => panic!("expected channel verify"),
    }
}
