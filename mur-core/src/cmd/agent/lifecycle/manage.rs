use super::create::validate_name;
use super::*;

// ─── stop / remove / rename ──────────────────────────────────────────────

pub fn cmd_stop(name: &str) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    let lock_path = agent_home.join("running.lock");
    if !lock_path.exists() {
        bail!("agent '{name}' is not running");
    }
    let bytes = fs::read(&lock_path).with_context(|| format!("read {}", lock_path.display()))?;
    let lock: LockFile =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", lock_path.display()))?;

    // Load stop_timeout_secs from profile, default 15.
    let timeout = {
        let pp = agent_home.join("profile.yaml");
        fs::read_to_string(&pp)
            .ok()
            .and_then(|y| serde_yaml_ng::from_str::<_AgentProfile>(&y).ok())
            .map(|p| p.lifecycle.stop_timeout_secs)
            .unwrap_or(15)
    };

    // Take the agent out of its supervisor's hands before signalling it.
    // launchd/systemd restart the runtime the instant it dies (every service
    // `mur agent install-service` writes sets KeepAlive), so signalling alone
    // cannot stop a service-managed agent — it only swaps the pid.
    let had_service = super::super::service::stop_service(name);

    #[cfg(unix)]
    unsafe {
        libc::kill(lock.pid as libc::pid_t, libc::SIGTERM);
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    while std::time::Instant::now() < deadline {
        if !pid_alive(lock.pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if pid_alive(lock.pid) {
        #[cfg(unix)]
        unsafe {
            libc::kill(lock.pid as libc::pid_t, libc::SIGKILL);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // Supervisor normally removes the lock, but after SIGKILL nothing will —
    // or our sleep-fixture has no lock cleanup — so best-effort remove here.
    let _ = fs::remove_file(&lock_path);

    // Confirm, don't assert. Signalling the pid is not the same as the agent
    // being down: a launchd job with KeepAlive (every service `mur agent
    // install-service` writes) hands back a fresh pid within a second, and the
    // old code printed "Stopped agent" regardless — so the arrow read
    // 17402 → 17597 while the message said success.
    //
    // `stop_service` above removes that race for our own services. This loop
    // catches whatever else might be respawning it, and says so instead of
    // lying.
    if let Some(pid) = wait_for_no_live_lock(&lock_path) {
        bail!(
            "signalled '{name}' but it is running again as pid {pid} — something is respawning it.{}",
            if had_service {
                ""
            } else {
                " No mur service is installed for it; check for another supervisor."
            }
        );
    }
    if had_service {
        // "unloaded its service" read as permanent, and it is not: the
        // descriptor stays on disk with RunAtLoad and is not disabled, so the
        // next login starts the agent again. Say that here, where the user
        // forms the belief, instead of letting them discover it after a
        // reboot.
        println!("Stopped agent '{name}' and unloaded its service for this session");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        println!(
            "  It starts again at your next login. To keep it stopped: {}",
            super::super::service::lasting_stop_hint(name)
        );
    } else {
        println!("Stopped agent '{name}'");
    }
    Ok(())
}

/// Poll briefly for a *live* `running.lock` to reappear, returning the pid that
/// holds it. A supervisor that restarts the runtime needs a moment to do it, so
/// checking once immediately after the kill would always look clean.
fn wait_for_no_live_lock(lock_path: &Path) -> Option<u32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut seen = None;
    while std::time::Instant::now() < deadline {
        seen = fs::read(lock_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<LockFile>(&b).ok())
            .filter(|l| pid_alive(l.pid))
            .map(|l| l.pid);
        if seen.is_some() {
            return seen;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    seen
}

pub fn cmd_remove(name: &str, purge: bool, force: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    if !force {
        let unread = count_unread_companion_inbox(&agent_home);
        if unread > 0 {
            bail!(
                "agent '{name}' has {unread} unread companion message{}. Run 'mur agent companion inbox {name} --unread-only' to view, or pass --force to remove anyway",
                if unread == 1 { "" } else { "s" }
            );
        }
    }
    refuse_if_running(&agent_home, name)?;

    let bin_dir = resolve_bin_dir()?;
    let symlink = bin_dir.join(format!("mur_agent_{name}"));
    if symlink.symlink_metadata().is_ok() {
        fs::remove_file(&symlink).ok();
    }

    // The symlink above is what the service execs, so leaving the descriptor
    // behind leaves the supervisor retrying a path that no longer exists — and
    // loading it again at the next login. Remove goes with it.
    if let Some(path) = super::super::service::remove_service(name) {
        println!("Removed service {}", path.display());
    }

    if purge {
        if let Some(key_dir) = purge_agent_state(&agent_home)? {
            println!("Removed private key {}", key_dir.display());
        }
        // #712: a stale pin would make a new agent of this name refuse to start.
        if let Err(e) = mur_common::entitlements_pin::remove_pin(&mur_home, name) {
            eprintln!("warning: could not remove entitlement pin: {e}");
        }
        println!("Purged agent '{name}'");
    } else {
        println!(
            "Removed agent '{name}' (data preserved at {})",
            agent_home.display()
        );
    }
    Ok(())
}

/// Remove everything `--purge` promises: the agent home **and** the private key
/// that has lived outside it since #850 option (c) moved keys to
/// `<mur_home>/keys/<name>/`. Returns the key directory removed, if any, so the
/// caller can report it.
///
/// Removing only the agent home is what this fixes: `--purge` says it deletes
/// the agent's data, but a 32-byte Ed25519 private key kept outliving its
/// agent. Those orphans are invisible — nothing enumerates them — and they
/// accumulate silently, one per purge.
///
/// The key goes FIRST on purpose. If it fails the agent home is still intact
/// and the command can be retried; the reverse order fails into exactly the
/// state being fixed — a key with no agent to own it.
pub(super) fn purge_agent_state(agent_home: &Path) -> Result<Option<PathBuf>> {
    // `private_key_dir` returns its argument unchanged for directories that are
    // not an agent home (commander, publisher, the host key). Comparing guards
    // against deleting the agent home twice in that case.
    let key_dir = mur_common::identity::private_key_dir(agent_home);
    let removed = if key_dir != agent_home && key_dir.is_dir() {
        fs::remove_dir_all(&key_dir)
            .with_context(|| format!("remove_dir_all {}", key_dir.display()))?;
        Some(key_dir)
    } else {
        None
    };
    fs::remove_dir_all(agent_home)
        .with_context(|| format!("remove_dir_all {}", agent_home.display()))?;
    Ok(removed)
}

pub fn cmd_rename(old: &str, new: &str) -> Result<()> {
    validate_name(new)?;
    let mur_home = resolve_mur_home()?;
    let old_home = mur_home.join("agents").join(old);
    let new_home = mur_home.join("agents").join(new);
    if !old_home.exists() {
        bail!("agent '{old}' not found");
    }
    if new_home.exists() {
        bail!("agent '{new}' already exists");
    }
    refuse_if_running(&old_home, old)?;

    // Update profile.yaml name + updated_at before renaming the directory
    // so the on-disk value matches the directory once the rename completes.
    let profile_path = old_home.join("profile.yaml");
    let yaml = fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: _AgentProfile = serde_yaml_ng::from_str(&yaml)
        .with_context(|| format!("parse {}", profile_path.display()))?;
    profile.name = new.to_string();
    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let new_yaml = serde_yaml_ng::to_string(&profile).context("serialize profile.yaml")?;
    write_atomic(&profile_path, new_yaml.as_bytes())?;

    fs::rename(&old_home, &new_home)
        .with_context(|| format!("rename {} -> {}", old_home.display(), new_home.display()))?;
    if let Err(e) = mur_common::entitlements_pin::rename_pin(&mur_home, old, new) {
        eprintln!(
            "warning: could not move entitlement pin ({e}); run `mur agent perm reseal {new}`"
        );
    }

    let bin_dir = resolve_bin_dir()?;
    let old_symlink = bin_dir.join(format!("mur_agent_{old}"));
    let new_symlink = bin_dir.join(format!("mur_agent_{new}"));
    if old_symlink.symlink_metadata().is_ok() {
        let target =
            fs::read_link(&old_symlink).unwrap_or_else(|_| PathBuf::from("mur-agent-runtime"));
        fs::remove_file(&old_symlink).ok();
        if new_symlink.symlink_metadata().is_ok() {
            fs::remove_file(&new_symlink).ok();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &new_symlink).with_context(|| {
            format!("symlink {} -> {}", new_symlink.display(), target.display())
        })?;
        #[cfg(windows)]
        std::fs::copy(&target, &new_symlink)
            .with_context(|| format!("copy {} to {}", target.display(), new_symlink.display()))?;
    }

    println!("Renamed '{old}' -> '{new}'");
    Ok(())
}

/// Count files in `<agent_home>/companion/inbox/*.md` whose response line is `<unset>`.
/// Returns 0 if the directory does not exist (companion never enabled).
pub(super) fn count_unread_companion_inbox(agent_home: &Path) -> usize {
    let inbox = agent_home.join("companion/inbox");
    if !inbox.exists() {
        return 0;
    }
    let entries = match std::fs::read_dir(&inbox) {
        Ok(it) => it,
        Err(_) => return 0,
    };
    let mut n = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if let Ok(body) = std::fs::read_to_string(&path)
            && is_unread(&body)
        {
            n += 1;
        }
    }
    n
}

/// Returns `true` when the last `>>> response: ` line in `body` has the value `<unset>`.
fn is_unread(body: &str) -> bool {
    let marker = ">>> response: ";
    body.lines()
        .rev()
        .find_map(|l| l.strip_prefix(marker).map(|v| v.trim() == "<unset>"))
        .unwrap_or(false)
}
