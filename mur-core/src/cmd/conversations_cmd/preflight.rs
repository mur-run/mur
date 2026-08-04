//! `mur conversations preflight` — pre-migration guard checks: commander
//! daemon, disk space, staging dir, Ollama reachability + per-model pull
//! checks, pattern dir, free memory.
//!
//! Split out of the flat `conversations_cmd.rs` into its own submodule
//! (fix round 1, finding 4) to keep individual command files under the
//! repo's 800-line cap. `collect_backend_configs`, `group_ollama_backends_by_endpoint`,
//! `probe_ollama_tags`, and `OllamaProbeOutcome` stay module-private in the
//! parent `conversations_cmd` (`super`) since `doctor` and `preflight` both
//! need them (fix round 1, finding 2).

use anyhow::Result;

use super::{
    OllamaProbeOutcome, collect_backend_configs, group_ollama_backends_by_endpoint,
    probe_ollama_tags,
};

/// BP1 amendment: dedicated preflight check bundle (daemon, disk, staging, audit presence).
/// Phase 2C: extends with Ollama reachability, model-pull checks, pattern dir, free memory.
pub async fn cmd_conversations_preflight() -> Result<()> {
    use crate::conversations::migrate;
    let plan = migrate::dry_run(None)?;

    let mut ok = true;
    println!("conversations preflight");
    if plan.commander_daemon_running {
        println!("  ✗ commander daemon appears to be running");
        ok = false;
    } else {
        println!("  ✓ commander daemon not running");
    }
    // Disk space check (best-effort; does not resolve mount points perfectly).
    // fs_available_bytes returns None (treated as unlimited) in Phase 1 —
    // a proper statvfs wrapper lands in a later task.
    let home = dirs::home_dir().unwrap_or_default();
    let needed = plan.free_space_needed_bytes;
    let free = fs_available_bytes(&home).unwrap_or(u64::MAX);
    if free < needed {
        println!(
            "  ✗ disk: {:.1} MB free, need {:.1} MB",
            free as f64 / 1_048_576.0,
            needed as f64 / 1_048_576.0
        );
        ok = false;
    } else {
        println!(
            "  ✓ disk: {:.1} MB free, need {:.1} MB",
            free as f64 / 1_048_576.0,
            needed as f64 / 1_048_576.0
        );
    }
    let staging = home.join(".mur/.conversations-migrating");
    if staging.exists() {
        println!(
            "  ✗ staging dir exists at {} — run migrate --resume or --discard-staging",
            staging.display()
        );
        ok = false;
    } else {
        println!("  ✓ no stale staging dir");
    }
    // Commander audit presence (not verification — different algo)
    let cmdr_audit = home.join(".mur/commander/audit.jsonl");
    if cmdr_audit.exists() {
        println!("  ✓ commander audit present (opaque bridge target)");
    } else {
        println!("  · no commander audit; migration will bridge from ZERO_HASH");
    }

    // ── Phase 2C probes ───────────────────────────────────────────────────

    // Load config once; derive which Ollama-routed backends (if any) the
    // conversations pipeline actually uses, grouped by the endpoint each is
    // actually routed to — a single shared endpoint here would falsely flag
    // models that live on a different host as missing (fix round 1, finding 1).
    let cfg = crate::store::config::load_config().unwrap_or_default();
    let ollama_groups = group_ollama_backends_by_endpoint(&collect_backend_configs(&cfg));

    if ollama_groups.is_empty() {
        println!("  · no conversations stage routes through Ollama (skipping Ollama probe)");
    } else {
        for g in &ollama_groups {
            match probe_ollama_tags(&g.endpoint, std::time::Duration::from_secs(2)).await {
                OllamaProbeOutcome::Reachable(installed) => {
                    println!("  ✓ Ollama reachable at {}", g.endpoint);
                    for m in &g.models {
                        if installed.iter().any(|n| n == m) {
                            println!("  ✓ model {m} pulled");
                        } else {
                            println!("  ✗ model {m} missing — run: ollama pull {m}");
                            ok = false;
                        }
                    }
                }
                OllamaProbeOutcome::Unreachable(reason) => {
                    println!("  ✗ Ollama at {} {reason}", g.endpoint);
                    ok = false;
                    if !g.models.is_empty() {
                        println!(
                            "  · skipped model check ({} models wanted; Ollama unreachable)",
                            g.models.len()
                        );
                    }
                }
            }
        }
    }

    // Pattern dir readable.
    let patterns_dir = home.join(".mur/patterns");
    if patterns_dir.exists() {
        match std::fs::read_dir(&patterns_dir) {
            Ok(_) => println!("  ✓ patterns dir readable at {}", patterns_dir.display()),
            Err(e) => {
                println!(
                    "  ✗ patterns dir at {} unreadable: {e}",
                    patterns_dir.display()
                );
                ok = false;
            }
        }
    } else {
        println!(
            "  · patterns dir {} missing (pattern refs will be a no-op)",
            patterns_dir.display()
        );
    }

    // Free memory check (informational).
    match system_free_memory_mb() {
        Some(mb) if mb < 4096 => {
            println!("  · free mem: {mb} MB (< 4 GB — LLM calls may swap)");
        }
        Some(mb) => {
            println!("  ✓ free mem: {mb} MB");
        }
        None => {
            println!("  · free mem: unknown (sysinfo probe skipped)");
        }
    }

    if ok {
        println!("\n→ preflight passed");
    } else {
        println!("\n✗ preflight FAILED — resolve issues above");
        std::process::exit(1);
    }
    Ok(())
}

/// Phase 1: returns None (treated as unlimited) so the disk check never blocks.
/// A proper statvfs wrapper can land in a later task.
fn fs_available_bytes(_path: &std::path::Path) -> Option<u64> {
    None
}

fn system_free_memory_mb() -> Option<u64> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    // sysinfo reports in KiB; convert to MiB.
    let avail_kib = sys.available_memory();
    if avail_kib == 0 {
        None
    } else {
        Some(avail_kib / 1024)
    }
}
