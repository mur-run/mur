//! Per-platform OS-managed agent autostart.
//!
//! Agents are owned by the OS init system so they survive Hub crashes (spec §3.1).
//! Hub calls `register` + `start_service` after install, and `stop_service` +
//! `unregister` during uninstall. The status poller calls `is_running` every 5s.

use std::path::Path;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;

/// Register the agent as an OS-managed service and persist across reboots.
pub fn register(
    slug: &str,
    display_name: &str,
    runtime_binary: &Path,
    mur_home: &Path,
) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return macos::register(slug, display_name, runtime_binary, mur_home);
    #[cfg(target_os = "linux")]
    return linux::register(slug, display_name, runtime_binary, mur_home);
    #[cfg(target_os = "windows")]
    return windows::register(slug, display_name, runtime_binary, mur_home);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (slug, display_name, runtime_binary, mur_home);
        anyhow::bail!("autostart::register not implemented on this platform")
    }
}

/// Unregister and disable the agent service.
pub fn unregister(slug: &str, mur_home: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return macos::unregister(slug, mur_home);
    #[cfg(target_os = "linux")]
    return linux::unregister(slug, mur_home);
    #[cfg(target_os = "windows")]
    return windows::unregister(slug, mur_home);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (slug, mur_home);
        anyhow::bail!("autostart::unregister not implemented on this platform")
    }
}

/// Start the agent service immediately (does not block).
pub fn start_service(slug: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return macos::start_service(slug);
    #[cfg(target_os = "linux")]
    return linux::start_service(slug);
    #[cfg(target_os = "windows")]
    return windows::start_service(slug);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = slug;
        anyhow::bail!("autostart::start_service not implemented on this platform")
    }
}

/// Stop the agent service.
pub fn stop_service(slug: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return macos::stop_service(slug);
    #[cfg(target_os = "linux")]
    return linux::stop_service(slug);
    #[cfg(target_os = "windows")]
    return windows::stop_service(slug);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = slug;
        anyhow::bail!("autostart::stop_service not implemented on this platform")
    }
}

/// Returns true if the agent service is currently running.
pub fn is_running(slug: &str) -> bool {
    #[cfg(target_os = "macos")]
    return macos::is_running(slug);
    #[cfg(target_os = "linux")]
    return linux::is_running(slug);
    #[cfg(target_os = "windows")]
    return windows::is_running(slug);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = slug;
        false
    }
}
