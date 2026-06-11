//! Multi-agent orchestration for `mur agent cli a b c` — one multiplexer
//! pane per agent, each running single-name `mur agent cli <name>`.

use anyhow::{Result, bail};

pub fn run(names: &[String], _resume: bool, _auto: bool) -> Result<()> {
    bail!("multi-agent mode not yet implemented: {}", names.join(", "));
}
