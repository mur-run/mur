//! `mur agent pair [name]` — mint a single-use enrollment window and print a
//! QR/URI to pair a phone (MUR mobile app) with a local agent over the LAN.
//! Also `mur agent devices` / `mur agent unpair <fingerprint>` to manage the
//! paired phones.
//!
//! The window is minted into `<home>/mobile/pair-window.json` (token HASH only),
//! the same file the daemon's `mobile_server` claims at enrollment, so the QR
//! always matches what is actually served — and the secret expires + is single-use.

use anyhow::Result;
use qrcode::QrCode;
use qrcode::render::unicode;

use crate::cmd::agent::resolve_mur_home;
use crate::mobile;

pub fn cmd_pair(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    // Mint a fresh single-use enrollment window. Only the token's hash is written
    // to disk (0600); the plaintext token below rides the QR out-of-band and is
    // valid once, for a short TTL.
    // `did` (the agent's Ed25519 identity) is bound into the QR so the phone can
    // authenticate the daemon during the proof handshake. Required — without it
    // the phone cannot verify which daemon it reached on the TLS-less LAN.
    let did = mobile::daemon_id(&home, name).ok_or_else(|| {
        anyhow::anyhow!(
            "agent \"{name}\" has no identity yet — start it once (e.g. `mur agent run {name}`) before pairing"
        )
    })?;
    let (window_id, token) = mobile::mint_pair_window(&home, name)?;
    let ttl = mobile::pair_window_ttl_secs();
    let port = mobile::mobile_port();
    let host = mobile::lan_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let uri = mobile::pairing_uri(&host, port, &window_id, &token, &did, name);

    let code = QrCode::new(uri.as_bytes())?;
    let qr = code.render::<unicode::Dense1x2>().quiet_zone(true).build();

    println!("\nScan with the MUR mobile app to pair with agent \"{name}\":\n");
    println!("{qr}");
    println!(
        "  Address : ws://{host}:{port}{}",
        mur_common::mobile::MOBILE_WS_PATH
    );
    println!("  Agent   : {name}");
    println!("  Expires : in {ttl}s — single-use (re-run `mur agent pair` to add another device)");
    println!("  URI     : {uri}\n");
    println!("Phone and Mac must share a Wi-Fi/LAN, and the murmurd daemon must be running.");
    Ok(())
}

/// `mur agent devices` — list paired phones (fingerprint + pubkey).
pub fn cmd_devices() -> Result<()> {
    let home = resolve_mur_home()?;
    let devices = mobile::list_paired_devices(&home);
    if devices.is_empty() {
        println!("No paired devices. Run `mur agent pair` to add one.");
        return Ok(());
    }
    println!("Paired devices ({}):", devices.len());
    for (pubkey, fp) in devices {
        println!("  {fp}  {pubkey}");
    }
    println!("\nRemove one with: mur agent unpair <fingerprint>");
    Ok(())
}

/// `mur agent unpair <fingerprint>` — revoke a paired phone by fingerprint
/// prefix (or full pubkey). Running sessions are not torn down here; the device
/// simply cannot re-authenticate afterward.
pub fn cmd_unpair(frag: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    match mobile::remove_paired_device(&home, frag)? {
        Some(pubkey) => {
            println!(
                "Unpaired device {} ({pubkey}).",
                mobile::device_fingerprint(&pubkey)
            );
            Ok(())
        }
        None => anyhow::bail!("no paired device matches `{frag}` (see `mur agent devices`)"),
    }
}
