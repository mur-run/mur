use super::{SandboxPolicy, SandboxStatus};
use anyhow::Context;
use std::ffi::CString;

pub fn apply_macos(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    let profile = build_sbpl_profile(policy);
    let profile_c = CString::new(profile).context("SBPL profile to CString")?;

    let mut error_buf: *mut libc::c_char = std::ptr::null_mut();
    // parameters is a null-terminated array of (key, value, ..., NULL) pairs.
    // We pass no template parameters.
    let params: [*const libc::c_char; 1] = [std::ptr::null()];

    let rc = unsafe {
        sandbox_init_with_parameters(
            profile_c.as_ptr(),
            0, // flags — always 0
            params.as_ptr(),
            &mut error_buf,
        )
    };

    if rc != 0 {
        let msg = if error_buf.is_null() {
            "unknown SBPL error".to_string()
        } else {
            let s = unsafe { std::ffi::CStr::from_ptr(error_buf) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(error_buf) };
            s
        };
        tracing::warn!(error = %msg, "macOS sandbox_init failed; running advisory-only");
        return Ok(SandboxStatus {
            platform: "macos-sbpl-failed".to_string(),
            effective_abi: None,
            enforcing: false,
        });
    }

    Ok(SandboxStatus {
        platform: "macos-sbpl".to_string(),
        effective_abi: None,
        enforcing: true,
    })
}

/// Build an SBPL profile string from the policy.
/// Strategy: allow everything (allow default), deny explicit paths,
/// then restrict writes to only allowed write paths.
pub fn build_sbpl_profile(policy: &SandboxPolicy) -> String {
    let mut lines = vec!["(version 1)".to_string(), "(allow default)".to_string()];

    // Deny reads AND writes to explicitly denied paths.
    for path in &policy.fs_deny {
        let p = path.to_string_lossy();
        lines.push(format!("(deny file-write* (subpath \"{p}\"))"));
        lines.push(format!("(deny file-read* (subpath \"{p}\"))"));
    }

    // Allow writes to explicitly allowed write paths.
    for path in &policy.fs_write {
        let p = path.to_string_lossy();
        lines.push(format!("(allow file-write* (subpath \"{p}\"))"));
    }

    // Network restrictions.
    if let Some(hosts) = &policy.net_allow_hosts {
        if hosts.is_empty() {
            // Off: deny all outbound TCP.
            lines.push("(deny network-outbound)".to_string());
        } else {
            // Restricted: deny all, then allow listed hosts on ports 443 and 80.
            lines.push("(deny network-outbound)".to_string());
            for host in hosts {
                lines.push(format!(
                    "(allow network-outbound (remote tcp \"{host}:443\"))"
                ));
                lines.push(format!(
                    "(allow network-outbound (remote tcp \"{host}:80\"))"
                ));
            }
        }
    }
    // None (Unrestricted): leave network unblocked — (allow default) covers it.

    lines.join("\n")
}

unsafe extern "C" {
    fn sandbox_init_with_parameters(
        profile: *const libc::c_char,
        flags: u64,
        parameters: *const *const libc::c_char,
        errorbuf: *mut *mut libc::c_char,
    ) -> libc::c_int;

    fn sandbox_free_error(errorbuf: *mut libc::c_char);
}
