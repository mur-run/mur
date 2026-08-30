//! `mur agent stats` and `mur agent logs` — telemetry and log inspection.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::resolve_mur_home;

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[allow(dead_code)]
pub struct TokenTotals {
    pub llm_calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Fold `gen_ai.usage.*` telemetry/*.jsonl under `agent_dir`.
#[allow(dead_code)]
pub fn agent_token_totals(agent_dir: &Path) -> TokenTotals {
    let mut t = TokenTotals::default();
    let telemetry_dir = agent_dir.join("telemetry");
    let Ok(entries) = std::fs::read_dir(&telemetry_dir) else {
        return t;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in body.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v.get("gen_ai.request.model").is_some() {
                t.llm_calls += 1;
                t.input_tokens += v["gen_ai.usage.input_tokens"].as_u64().unwrap_or(0);
                t.output_tokens += v["gen_ai.usage.output_tokens"].as_u64().unwrap_or(0);
            }
        }
    }
    t
}

pub fn cmd_stats(name: &str) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let dir = mur_home.join("agents").join(name);
    if !dir.exists() {
        bail!("agent '{name}' not found");
    }

    // Extract token/call totals via agent_token_totals.
    let totals = agent_token_totals(&dir);

    // Separate loop for latency and error metrics (not entangled with token counting).
    let telemetry_dir = dir.join("telemetry");
    let mut latency_total: u64 = 0;
    let mut errors: u64 = 0;

    if telemetry_dir.exists() {
        for entry in fs::read_dir(&telemetry_dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let body =
                fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            for line in body.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // LLM call rows carry the OTel `gen_ai.*` namespace.
                if v.get("gen_ai.request.model").is_some() {
                    latency_total += v["latency_ms"].as_u64().unwrap_or(0);
                }
                if v.get("kind").is_some() && v.get("recoverable").is_some() {
                    errors += 1;
                }
            }
        }
    }

    let avg_latency = latency_total.checked_div(totals.llm_calls).unwrap_or(0);
    println!("agent: {name}");
    println!("llm_calls: {}", totals.llm_calls);
    println!("input_tokens: {}", totals.input_tokens);
    println!("output_tokens: {}", totals.output_tokens);
    println!("avg_latency_ms: {avg_latency}");
    println!("errors: {errors}");
    Ok(())
}

/// Where a runtime's stderr can land. Two launch paths write two *different*
/// files: `mur agent start` writes into the agent home, while the service
/// supervisor redirects to /tmp (macOS) or the journal (Linux, no file). So
/// `logs` cannot assume which supervisor is in charge — it takes whichever
/// candidate was written last.
#[cfg(target_os = "macos")]
fn log_candidates(agent_home: &Path, name: &str) -> Vec<std::path::PathBuf> {
    vec![
        agent_home.join("stderr.log"),
        super::service::service_stderr_log(name),
    ]
}

/// The systemd unit sets no `StandardError=`, so the runtime's stderr goes to
/// the journal and there is no second file to consider.
#[cfg(not(target_os = "macos"))]
fn log_candidates(agent_home: &Path, _name: &str) -> Vec<std::path::PathBuf> {
    vec![agent_home.join("stderr.log")]
}

fn newest_log(agent_home: &Path, name: &str) -> Option<std::path::PathBuf> {
    log_candidates(agent_home, name)
        .into_iter()
        .filter_map(|p| Some((fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, p)| p)
}

pub fn cmd_logs(name: &str, tail: usize) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let dir = mur_home.join("agents").join(name);
    if !dir.exists() {
        bail!("agent '{name}' not found");
    }
    let Some(log_path) = newest_log(&dir, name) else {
        eprintln!("(no log file for '{name}' yet)");
        if cfg!(target_os = "linux") {
            eprintln!(
                "  a service-managed agent logs to the journal, not a file:\n    journalctl --user -u mur-agent-{name}.service"
            );
        }
        return Ok(());
    };
    // Name the source: two launch paths write two files, and the one this
    // picked may be days stale if the agent moved to the other path.
    eprintln!("── {} ──", log_path.display());
    let body =
        fs::read_to_string(&log_path).with_context(|| format!("read {}", log_path.display()))?;
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(tail);
    for line in &lines[start..] {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Only the two-candidate tests below need this, and they are macOS-only.
    #[cfg(target_os = "macos")]
    fn touch(p: &std::path::Path, ago: u64) {
        use std::time::{Duration, SystemTime};
        std::fs::write(p, "x").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(ago))
            .unwrap();
    }

    #[test]
    fn newest_log_is_none_when_no_launch_path_has_written() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            super::newest_log(home.path(), "nobody-has-run-me"),
            None,
            "no candidate exists, so there is nothing honest to show"
        );
    }

    /// The bug: `mur agent start` writes `<agent>/stderr.log`, the launchd unit
    /// writes /tmp. Reading only the first showed days-stale lines as if live.
    #[test]
    #[cfg(target_os = "macos")]
    fn newest_log_prefers_the_service_path_when_it_is_fresher() {
        let home = tempfile::tempdir().unwrap();
        // Unique name so the hardcoded /tmp path cannot collide with a real agent.
        let name = format!("mur-test-{}", std::process::id());
        let svc = super::super::service::service_stderr_log(&name);
        touch(&home.path().join("stderr.log"), 86_400); // yesterday
        touch(&svc, 1); // a second ago
        let picked = super::newest_log(home.path(), &name);
        let _ = std::fs::remove_file(&svc);
        assert_eq!(picked, Some(svc), "the fresher of the two must win");
    }

    /// ...and the preference is by mtime, not by a fixed ranking of the paths.
    #[test]
    #[cfg(target_os = "macos")]
    fn newest_log_prefers_the_agent_home_when_it_is_fresher() {
        let home = tempfile::tempdir().unwrap();
        let name = format!("mur-test-rev-{}", std::process::id());
        let svc = super::super::service::service_stderr_log(&name);
        let own = home.path().join("stderr.log");
        touch(&svc, 86_400);
        touch(&own, 1);
        let picked = super::newest_log(home.path(), &name);
        let _ = std::fs::remove_file(&svc);
        assert_eq!(picked, Some(own), "mtime decides, not candidate order");
    }

    /// Non-macOS has no second file at all (systemd logs to the journal), so
    /// the candidate list must not grow a path nothing ever writes.
    #[test]
    fn candidate_count_matches_what_this_platform_actually_writes() {
        let home = tempfile::tempdir().unwrap();
        let n = super::log_candidates(home.path(), "a").len();
        assert_eq!(n, if cfg!(target_os = "macos") { 2 } else { 1 });
    }

    use super::*;

    #[test]
    fn token_totals_folds_gen_ai_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let tdir = tmp.path().join("telemetry");
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(
            tdir.join("a.jsonl"),
            concat!(
                r#"{"gen_ai.request.model":"m","gen_ai.usage.input_tokens":100,"gen_ai.usage.output_tokens":40}"#,
                "\n",
                r#"{"not":"an llm row"}"#,
                "\n",
                r#"{"gen_ai.request.model":"m","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5}"#,
            ),
        )
        .unwrap();
        let t = agent_token_totals(tmp.path());
        assert_eq!((t.llm_calls, t.input_tokens, t.output_tokens), (2, 110, 45));
    }
}
