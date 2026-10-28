//! The startup-override flags shared by `mur agent start` and `restart`
//! (spec §CLI). One struct, flattened into both, so the two cannot drift.

use clap::{ArgGroup, Args, ValueEnum};
use mur_common::agent::OverrideFlags;

/// The group `--confirm` requires. Built by hand with `#[group(skip)]`:
/// the derive's own struct group appends every field after any `args = [..]`,
/// so `--confirm` would sit in the group it requires and satisfy itself.
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
#[group(skip)]
#[command(group(
    ArgGroup::new("override_flag")
        .multiple(true)
        .args(["no_hitl", "net", "full_perms", "dangerously_skip_all"])
))]
pub struct OverrideArgs {
    /// Next boot only: calls that would ask for approval run without asking
    /// (Spend, Privileged and undeclared-tier calls still ask)
    #[arg(long)]
    pub no_hitl: bool,
    /// Next boot only: open shell / child-process network egress (MCP
    /// servers stay behind the egress proxy)
    #[arg(long, value_enum, value_name = "MODE")]
    pub net: Option<NetMode>,
    /// Next boot only: reset the filesystem deny list to the defaults and
    /// allow any process spawn
    #[arg(long)]
    pub full_perms: bool,
    /// Next boot only: --no-hitl, --net=unrestricted and --full-perms
    #[arg(long)]
    pub dangerously_skip_all: bool,
    /// Confirm the override without a terminal (scripts). Not `--yes`
    #[arg(long, requires = "override_flag")]
    pub confirm: bool,
}

/// `--net` takes exactly one value today.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetMode {
    Unrestricted,
}

impl OverrideArgs {
    /// The flags asked for; all off means a plain start / restart.
    pub fn flags(&self) -> OverrideFlags {
        let all = self.dangerously_skip_all;
        OverrideFlags {
            no_hitl: all || self.no_hitl,
            net_unrestricted: all || self.net == Some(NetMode::Unrestricted),
            full_perms: all || self.full_perms,
        }
    }

    pub fn any(&self) -> bool {
        self.flags() != OverrideFlags::default()
    }
}
