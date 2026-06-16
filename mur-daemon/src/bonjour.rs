//! Advertise the mobile WebSocket endpoint via mDNS/Bonjour so iOS devices
//! on the same LAN can DISCOVER it (find where the Mac is). Discovery only —
//! the TXT record carries NO secret: a pairing token would otherwise be
//! continuously multicast to every device on the LAN (RFC 8882/8386). The
//! enrollment token travels out-of-band in the QR/URI instead.
//!
//! Service type: `_mur._tcp`
//! TXT record:   `port=<n>  agent=mur  v=2`   (no token)

use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

pub fn advertise(port: u16) {
    std::thread::spawn(move || {
        if let Err(e) = run(port) {
            tracing::warn!(error = %e, "bonjour: advertisement failed (non-fatal)");
        }
    });
}

fn run(port: u16) -> anyhow::Result<()> {
    let mdns = ServiceDaemon::new()?;

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "mur-host".to_string());
    let instance = format!("MUR on {hostname}");

    // Discovery-only TXT: no secret. `v=2` signals the token left the TXT (the
    // app must obtain the enrollment code from the QR, not the broadcast).
    let mut properties = HashMap::new();
    properties.insert("agent".to_string(), "mur".to_string());
    properties.insert("v".to_string(), "2".to_string());

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
