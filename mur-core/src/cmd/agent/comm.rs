//! A2A forwarders — `mur agent send` and `mur agent card`.

use anyhow::{Context, Result};

use crate::a2a_dial::{DialMode, dial_method};

use super::resolve_mur_home;

pub fn cmd_send(name: &str, message_json: &str) -> Result<()> {
    let msg: serde_json::Value =
        serde_json::from_str(message_json).context("parse --message JSON")?;
    let home = resolve_mur_home()?;
    let params = serde_json::json!({"message": msg});
    // `message/send` to an ephemeral runtime is meaningless — the task
    // would die with the process. Require the agent be running.
    let result = dial_method(&home, name, "message/send", params, DialMode::RequireRunning)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub fn cmd_card(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let result = dial_method(&home, name, "agent/card", serde_json::Value::Null, DialMode::Auto)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
