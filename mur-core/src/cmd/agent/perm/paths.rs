//! `mur agent perm` filesystem grants: read/write/deny paths.

use anyhow::{Result, bail};
use mur_common::LockFile;

use super::super::{load_profile_for_edit, save_profile};
use super::warn_if_running;

/// `mur agent perm list-paths` — what this agent may touch, and what actually
/// took effect.
///
/// `profile.yaml` is a request; the kernel enforces the profile as it stood
/// when the agent sealed. This is the only surface that shows both.
pub fn cmd_perm_list_paths(name: &str) -> Result<()> {
    let (path, profile) = load_profile_for_edit(name)?;
    let lock = path
        .parent()
        .map(|d| d.join("running.lock"))
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<LockFile>(&b).ok());
    let agent_home = super::super::resolve_mur_home()?.join("agents").join(name);
    let chain = mur_agent_runtime::sandbox::launch_chain::LaunchChain::new(&agent_home);
    print!(
        "{}",
        super::super::perm_view::paths_picture(name, &profile, lock.as_ref(), Some(&chain))
    );
    Ok(())
}

/// Refuse a filesystem grant the sandbox would silently discard.
///
/// `SandboxPolicy::from_entitlements` drops entitlement paths that do not exist
/// when the profile is sealed (Issue 16 — a dead grant destabilizes unrelated
/// write checks). Accepting one here is the worst outcome: the CLI reports
/// success, the profile lists the path, `restart` says it applied, and the
/// kernel still returns EPERM with nothing in between explaining why.
fn reject_dead_grant(path_arg: &str) -> Result<()> {
    let p = mur_agent_runtime::sandbox::policy::expand_entitlement_path(path_arg);
    if std::fs::metadata(&p).is_ok() {
        return Ok(());
    }
    anyhow::bail!(
        "{} does not exist.\n\
         The sandbox drops grants for paths that are missing when the agent \
         starts, so this one would be accepted here and still denied by the \
         kernel. Create it first, then re-run this command:\n    mkdir -p {}",
        p.display(),
        p.display()
    )
}

/// Refuse a grant that no sandbox would honour, before it reaches the profile.
///
/// Distinct from `reject_dead_grant`, which refuses a path that does not exist
/// yet. This one refuses paths that must never be granted at all.
pub(crate) fn reject_ungrantable_path(name: &str, path_arg: &str, write: bool) -> Result<()> {
    reject_ungrantable(name, path_arg, write)
}

fn reject_ungrantable(name: &str, path_arg: &str, write: bool) -> Result<()> {
    use mur_agent_runtime::sandbox::launch_chain::{LaunchChain, is_overbroad_grant_root};

    let p = mur_agent_runtime::sandbox::policy::expand_entitlement_path(path_arg);
    let agent_home = super::super::resolve_mur_home()?.join("agents").join(name);
    let chain = LaunchChain::new(&agent_home);

    let hit = if write {
        chain.protects_write(&p)
    } else {
        chain.protects_read(&p)
    };
    if let Some(reason) = hit {
        anyhow::bail!(
            "{} is part of MUR's launch chain and can never be granted: {reason}",
            p.display()
        );
    }

    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    if is_overbroad_grant_root(&p, &home) {
        anyhow::bail!(
            "{} is too broad to grant — it covers the whole machine, the whole \
             home directory, or a volume root. Grant the specific project dir instead.",
            p.display()
        );
    }
    Ok(())
}

pub fn cmd_perm_allow_read(name: &str, path_arg: &str) -> Result<()> {
    reject_ungrantable(name, path_arg, false)?;
    reject_dead_grant(path_arg)?;
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile
        .entitlements
        .filesystem
        .read
        .iter()
        .any(|p| p == path_arg)
    {
        profile
            .entitlements
            .filesystem
            .read
            .push(path_arg.to_string());
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_allow_write(name: &str, path_arg: &str) -> Result<()> {
    reject_ungrantable(name, path_arg, true)?;
    reject_dead_grant(path_arg)?;
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile
        .entitlements
        .filesystem
        .write
        .iter()
        .any(|p| p == path_arg)
    {
        profile
            .entitlements
            .filesystem
            .write
            .push(path_arg.to_string());
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_deny_path(name: &str, path_arg: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile
        .entitlements
        .filesystem
        .deny
        .iter()
        .any(|p| p == path_arg)
    {
        profile
            .entitlements
            .filesystem
            .deny
            .push(path_arg.to_string());
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

/// Drop one path from one grant list. `deny_path` ADDS to the deny list; this
/// is the only way to take a grant back short of editing profile.yaml.
pub fn remove_path(
    fs: &mut mur_common::agent::FilesystemEntitlement,
    verb: &str,
    path_arg: &str,
) -> Result<bool> {
    let list = match verb {
        "read" => &mut fs.read,
        "write" => &mut fs.write,
        "deny" => &mut fs.deny,
        other => bail!("remove-path: unknown list '{other}' (read, write, deny)"),
    };
    let before = list.len();
    list.retain(|p| p != path_arg);
    Ok(list.len() != before)
}

pub fn cmd_perm_remove_path(name: &str, verb: &str, path_arg: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !remove_path(&mut profile.entitlements.filesystem, verb, path_arg)? {
        bail!("'{path_arg}' is not in the {verb} list of '{name}'");
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::remove_path;

    #[test]
    fn remove_path_takes_one_grant_back_and_reports_whether_it_was_there() {
        let mut fs = mur_common::agent::FilesystemEntitlement {
            read: vec!["/a".into()],
            write: vec!["/b".into(), "/c".into()],
            deny: vec![],
        };
        assert!(remove_path(&mut fs, "write", "/b").unwrap());
        assert_eq!(fs.write, vec!["/c"]);
        assert!(
            !remove_path(&mut fs, "write", "/b").unwrap(),
            "already gone"
        );
        assert_eq!(fs.read, vec!["/a"], "other lists untouched");
        assert!(remove_path(&mut fs, "exec", "/a").is_err());
    }
}
