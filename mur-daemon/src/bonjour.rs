//! Advertise the mobile WebSocket endpoint via mDNS/Bonjour so iOS devices
//! on the same LAN can discover it without scanning a QR code.
//!
//! Service type: `_mur._tcp`
//! TXT record:   `port=<n>  token=<pair-token>  agent=mur`

use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

pub fn advertise(port: u16, pair_token: &str) {
    let token = pair_token.to_string();
    std::thread::spawn(move || {
        if let Err(e) = run(port, &token) {
            tracing::warn!(error = %e, "bonjour: advertisement failed (non-fatal)");
        }
    });
}

fn run(port: u16, pair_token: &str) -> anyhow::Result<()> {
    let mdns = ServiceDaemon::new()?;

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "mur-host".to_string());
    let instance = format!("MUR on {hostname}");

    let mut properties = HashMap::new();
    properties.insert("token".to_string(), pair_token.to_string());
    properties.insert("agent".to_string(), "mur".to_string());
    properties.insert("v".to_string(), "1".to_string());

    let service = ServiceInfo::new(
        "_mur._tcp.local.",
        &instance,
        &format!("{hostname}.local."),
        (),
        port,
        Some(properties),
    )?;

    mdns.register(service)?;
    tracing::info!(port, instance = %instance, "bonjour: advertising _mur._tcp");

    // Block forever — the thread keeps the mdns daemon alive.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
