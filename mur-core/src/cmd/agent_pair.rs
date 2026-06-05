//! `mur agent pair [name]` — print a QR + URI to pair a phone (MUR mobile app)
//! with a local agent over the LAN.
//!
//! The token and port come from `crate::mobile`, the same source the daemon's
//! `mobile_server` reads, so the QR always matches what is actually served.

use anyhow::Result;
use qrcode::render::unicode;
use qrcode::QrCode;

use crate::cmd::agent::resolve_mur_home;
use crate::mobile;

pub fn cmd_pair(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let token = mobile::ensure_pair_token(&home)?;
    let port = mobile::mobile_port();
    let host = mobile::lan_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let uri = mobile::pairing_uri(&host, port, &token, name);

    let code = QrCode::new(uri.as_bytes())?;
    let qr = code
        .render::<unicode::Dense1x2>()
        .quiet_zone(true)
        .build();

    println!("\nScan with the MUR mobile app to pair with agent \"{name}\":\n");
    println!("{qr}");
    println!(
        "  Address : ws://{host}:{port}{}",
        mur_common::mobile::MOBILE_WS_PATH
    );
    println!("  Agent   : {name}");
    println!("  Token   : {token}");
    println!("  URI     : {uri}\n");
    println!("Phone and Mac must share a Wi-Fi/LAN, and the murmurd daemon must be running.");
    Ok(())
}
