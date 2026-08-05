//! `mur update` CLI verb — translates clap args into `crate::update::run`.

use anyhow::Result;

use crate::update::{self, UpdateOptions};

pub(crate) fn cmd_update(check_only: bool, restart_agents: bool) -> Result<()> {
    update::run(UpdateOptions {
        check_only,
        restart_agents,
    })
}
