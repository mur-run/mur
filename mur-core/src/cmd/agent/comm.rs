//! A2A forwarders — `mur agent send` and `mur agent card`.

use anyhow::{Context, Result};
use mur_common::config::Config;

use crate::a2a_dial::{DialMode, dial_method};

use super::resolve_mur_home;

pub fn cmd_send(name: &str, message_json: &str, output_artifact_path: Option<&str>) -> Result<()> {
    let msg: serde_json::Value =
        serde_json::from_str(message_json).context("parse --message JSON")?;
    let home = resolve_mur_home()?;
    let mut params = serde_json::json!({"message": msg});
    if let Some(path) = output_artifact_path {
        params["output_artifact_path"] = serde_json::json!(path);
    }
    // `message/send` to an ephemeral runtime is meaningless — the task
    // would die with the process. Require the agent be running.
    let result = dial_method(
        &home,
        name,
        "message/send",
        params,
        DialMode::RequireRunning,
    )?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub fn cmd_card(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let mut result = dial_method(
        &home,
        name,
        "agent/card",
        serde_json::Value::Null,
        DialMode::Auto,
    )?;

    // Fitness section (M7a) — embedded into the card JSON so stdout stays valid JSON
    let cfg = Config::load_or_default(&home.join("config.yaml"));
    let fitness = crate::cross_agent::fitness::fitness(
        &home,
        name,
        chrono::Utc::now(),
        cfg.cross_agent.fitness_half_life_days,
        cfg.cross_agent.fitness_floor,
    )?;
    let fitness_json = serde_json::json!({
        "weight": fitness.weight,
        "success_rate": fitness.success_rate,
        "sample_size": fitness.sample_size,
        "recency_decay": fitness.recency_decay,
        "last_seen": fitness.last_seen.map(|t| t.to_rfc3339()),
    });
    if let Some(obj) = result.as_object_mut() {
        obj.insert("fitness".into(), fitness_json);
    }

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
