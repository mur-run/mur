//! A2A forwarders — `mur agent send` and `mur agent card`.

use anyhow::{Context, Result};
use mur_common::config::Config;

use crate::a2a_dial::{DialMode, dial_method};

use super::resolve_mur_home;

pub fn cmd_send(name: &str, message_json: &str) -> Result<()> {
    let msg: serde_json::Value =
        serde_json::from_str(message_json).context("parse --message JSON")?;
    let home = resolve_mur_home()?;
    let params = serde_json::json!({"message": msg});
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
    let result = dial_method(
        &home,
        name,
        "agent/card",
        serde_json::Value::Null,
        DialMode::Auto,
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);

    // Fitness section (M7a)
    let cfg = Config::load_or_default(&home.join("config.yaml"));
    let fitness = crate::cross_agent::fitness::fitness(
        &home,
        name,
        chrono::Utc::now(),
        cfg.cross_agent.fitness_half_life_days,
        cfg.cross_agent.fitness_floor,
    )?;
    println!();
    if fitness.sample_size == 0 {
        println!("Fitness: (no usage data)");
    } else {
        println!("Fitness");
        println!("  weight:        {:.3}", fitness.weight);
        println!(
            "  success_rate:  {:.3} ({} ok / {} fail / {} total)",
            fitness.success_rate,
            // Reconstruct from rate + sample (we don't store the components
            // separately on AgentFitness, but this is a display-only best-effort)
            (fitness.success_rate * fitness.sample_size as f64).round() as u64,
            fitness.sample_size.saturating_sub(
                (fitness.success_rate * fitness.sample_size as f64).round() as u64,
            ),
            fitness.sample_size,
        );
        println!(
            "  recency:       {:.3} (last seen {})",
            fitness.recency_decay,
            fitness
                .last_seen
                .map(|t| {
                    let ago = chrono::Utc::now() - t;
                    format!("{:.1} days ago", ago.num_seconds() as f64 / 86_400.0)
                })
                .unwrap_or_else(|| "never".into()),
        );
        println!(
            "  half_life:     {} days  floor: {:.2}",
            cfg.cross_agent.fitness_half_life_days, cfg.cross_agent.fitness_floor,
        );
    }
    Ok(())
}
