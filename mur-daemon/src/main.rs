mod action_tick;
mod bonjour;
mod consumer;
mod fleet_tick;
mod inbox;
mod lock;
mod mobile_server;
mod relay_client;
mod signal_server;
mod skill_upgrade_tick;
mod snapshot_requests;
mod sleep;
mod store_health;
mod stt_sink;
mod tts_sink;

use anyhow::Result;
use chrono::Utc;
use lock::{LockState, is_healthy, lock_path, read_lock, write_lock};
use mur_core::inject::event::{EventKind, NormalizedEvent};
use mur_core::inject::index::{format_l0, load as load_capability_index};
use std::time::Instant;

const L0_BUDGET_CHARS: usize = 2400;

fn process_event(event: &NormalizedEvent) -> Result<()> {
    match event.kind {
        EventKind::Prompt => {
            let Some(ref session_id) = event.session_id else {
                return Ok(());
            };
            let index = load_capability_index().unwrap_or_else(|_| {
                mur_core::inject::index::CapabilityIndex {
                    entries: vec![],
                    project: None,
                }
            });
            let content = format_l0(&index, L0_BUDGET_CHARS);
            if !content.is_empty() {
                let path = inbox::inbox_path(session_id);
                inbox::write_inbox(&path, &content)?;
            }
        }
        EventKind::Stop => {
            let mur_bin = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("mur")))
                .unwrap_or_else(|| std::path::PathBuf::from("mur"));
            for subcmd in &["sync", "evolve", "emerge"] {
                if let Ok(mut child) = std::process::Command::new(&mur_bin)
                    .arg(subcmd)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    // Detached fire-and-forget: reap on a background thread so
                    // the child never lingers as a zombie (std::process::Child
                    // has no built-in reaper, unlike tokio's).
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let lock_file = lock_path();

    // Bail if another healthy instance is running; steal stale lock
    if let Some(existing) = read_lock(&lock_file)?
        && is_healthy(&existing)
    {
        eprintln!("murmurd already running (pid {})", existing.pid);
        std::process::exit(1);
    }

    let started = Utc::now();
    let state = LockState {
        pid: std::process::id(),
        started_at: started,
        heartbeat_at: started,
    };
    write_lock(&lock_file, &state)?;
    eprintln!("murmurd started (pid {})", state.pid);

    // §4.2.7 — startup store health checks (best-effort)
    let mur_dir = mur_core::store::yaml::default_mur_dir();
    store_health::run(&mur_dir);

    // E5 — start HTTP signal server (best-effort; failure is non-fatal)
    match signal_server::ensure_token() {
        Ok(token) => {
            signal_server::spawn(token);
        }
        Err(e) => eprintln!("murmurd: signal-server token error: {e:#}"),
    }

    // Memory-federation P0 — serve signed snapshot requests (best-effort loop;
    // verification + scope assembly happen here, outside every agent sandbox).
    snapshot_requests::spawn(mur_dir.clone());

    // P1 — start the mobile WebSocket endpoint (best-effort; failure is non-fatal).
    // No persistent pairing token: enrollment uses on-demand single-use windows
    // minted by `mur agent pair` / the Hub. One shared enrollment lock makes a
    // window's single-use claim atomic across the LAN and relay transports.
    {
        let enroll_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        mobile_server::spawn(mur_dir.clone(), enroll_lock.clone());
        bonjour::advertise(mur_core::mobile::mobile_port());

        // Sweep expired (unclaimed) pairing windows off disk so a lapsed window
        // can't be revived by rolling the wall clock back.
        {
            let home = mur_dir.clone();
            tokio::spawn(async move {
                let mut iv = tokio::time::interval(std::time::Duration::from_secs(
                    mur_core::mobile::PAIR_WINDOW_SWEEP_SECS,
                ));
                loop {
                    iv.tick().await;
                    mur_core::mobile::sweep_expired_pair_window(&home);
                }
            });
        }

        // P4: start relay client if configured in ~/.mur/config.yaml
        let cfg = mur_common::config::Config::load_or_default(&mur_dir.join("config.yaml"));
        if let (Some(relay_url), Some(api_key)) =
            (cfg.mobile_relay.relay_url, cfg.mobile_relay.api_key)
        {
            relay_client::spawn(mur_dir.clone(), relay_url, api_key, enroll_lock.clone());
        }
    }

    let queue_file = consumer::queue_path();
    let offset_file = consumer::offset_path();
    let mut offset = consumer::read_offset(&offset_file);

    // Heartbeat: update lock every 10 seconds
    let lock_file_hb = lock_file.clone();
    let pid = state.pid;
    let started_hb = state.started_at;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let new_state = LockState {
                pid,
                started_at: started_hb,
                heartbeat_at: Utc::now(),
            };
            let _ = write_lock(&lock_file_hb, &new_state);
        }
    });

    // Action pipeline tick: every 30 seconds
    let mur_home_tick = mur_dir.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(30));
        tick.tick().await; // skip first immediate fire
        loop {
            tick.tick().await;
            if let Err(e) = action_tick::scan_all_agents(&mur_home_tick) {
                tracing::error!(error = %e, "action_tick failed");
            }
            // Phase 2b: auto-run any fleets whose interval trigger is due.
            fleet_tick::tick(&mur_home_tick);
            // Daily auto-upgrade of origin-stamped (registry-installed) skills.
            skill_upgrade_tick::tick(&mur_home_tick);
        }
    });

    // Event loop: poll queue every second; track idle for sleep cycle.
    let mut poll = tokio::time::interval(tokio::time::Duration::from_secs(1));
    let mut last_event_at = Instant::now();
    let mut sleep_cycle_fired = false;
    loop {
        poll.tick().await;
        let (events, new_offset) = consumer::drain_new(&queue_file, offset)?;
        let had_events = new_offset > offset;
        if had_events {
            consumer::write_offset(&offset_file, new_offset)?;
            offset = new_offset;
            last_event_at = Instant::now();
            sleep_cycle_fired = false;
        }
        for event in &events {
            if let Err(e) = process_event(event) {
                eprintln!("murmurd: process_event error: {e:#}");
            }
        }
        // Fire sleep cycle once per idle period when opt-in enabled.
        if !sleep_cycle_fired && sleep::is_enabled() {
            let threshold = std::time::Duration::from_secs(sleep::idle_threshold_minutes() * 60);
            if last_event_at.elapsed() >= threshold {
                sleep::run_sleep_cycle();
                sleep_cycle_fired = true;
            }
        }
    }
}
