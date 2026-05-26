//! CLI dispatcher for `mur skill recombine` (M7b).

use std::path::Path;

use crate::cross_agent::recombine::peer_ref::parse_ref;
use crate::cross_agent::recombine::{
    RecombineOptions, RecombineOutcome, RecombineStrategy, run_recombine,
};

#[allow(clippy::too_many_arguments)]
pub async fn cmd_recombine(
    home: &Path,
    a: &str,
    b: &str,
    strategy: RecombineStrategy,
    name: Option<String>,
    dry_run: bool,
    agent: Option<String>,
    json: bool,
) -> i32 {
    let current_agent = match agent.or_else(|| std::env::var("MUR_AGENT").ok()) {
        Some(s) => s,
        None => {
            eprintln!("error: --agent <name> required (or set MUR_AGENT)");
            return 2;
        }
    };

    let a_ref = match parse_ref(a) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let b_ref = match parse_ref(b) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let opts = RecombineOptions {
        a_ref,
        b_ref,
        strategy,
        output_name: name,
        dry_run,
        current_agent,
    };

    match run_recombine(home, &opts).await {
        Ok(outcome) => {
            if json {
                if let Err(e) = print_json(&outcome) {
                    eprintln!("error: {e}");
                    return 1;
                }
            } else {
                print_human(&outcome);
            }
            0
        }
        Err(e) => {
            let msg = e.to_string();
            let code = classify_error(&msg);
            eprintln!("error: {msg}");
            code
        }
    }
}

fn print_human(o: &RecombineOutcome) {
    if let Some(path) = &o.written_to {
        println!(
            "Recombined into '{}' (strategy={}, lifecycle=Draft)",
            o.output_name,
            o.strategy.as_str()
        );
        println!("  Manifest: {}", path.display());
        println!("  Stats:    Draft");
        println!("  Evolution log: 1 Recombined event appended");
    } else {
        println!("--- Dry run (strategy={}) ---", o.strategy.as_str());
        println!("{}", o.manifest_yaml);
        println!("--- End (no files written) ---");
    }
}

fn print_json(o: &RecombineOutcome) -> anyhow::Result<()> {
    let v = serde_json::json!({
        "output_name": o.output_name,
        "strategy": o.strategy.as_str(),
        "written_to": o.written_to.as_ref().map(|p| p.display().to_string()),
        "evolution_event_appended": o.evolution_event_appended,
        "manifest_yaml": o.manifest_yaml,
    });
    serde_json::to_writer_pretty(std::io::stdout(), &v)?;
    println!();
    Ok(())
}

/// Map error messages to spec §8 exit codes.
fn classify_error(msg: &str) -> i32 {
    if msg.contains("not found") {
        2
    } else if msg.contains("intersection produced empty") {
        3
    } else if msg.contains("no model") || msg.contains("mur model add") {
        4
    } else if msg.contains("already exists") {
        6
    } else {
        5
    }
}
