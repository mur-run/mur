//! argv[0] dispatch + spoof defense.

const SYMLINK_PREFIX: &str = "mur_agent_";
const RUNTIME_BASENAME: &str = "mur-agent-runtime";

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("invoked as 'mur-agent-runtime' directly; pass --profile <name>")]
    BareRuntime,
    #[error("unknown invocation basename: {0}")]
    UnknownBasename(String),
    #[error("argv[0] '{argv0}' does not match profile.name '{profile_name}'")]
    SpoofDetected { argv0: String, profile_name: String },
}

/// Extract profile name from argv[0] basename (stripping .exe on Windows).
/// Returns BareRuntime if invoked as `mur-agent-runtime` itself (caller should
/// then parse a --profile flag).
pub fn extract_profile_name(argv0: &str) -> Result<String, DispatchError> {
    // Split on both `/` and `\` so Windows-style paths work on any host.
    let raw = argv0.rsplit(['/', '\\']).next().unwrap_or("");
    if raw.is_empty() {
        return Err(DispatchError::UnknownBasename(argv0.to_string()));
    }
    let basename = raw.strip_suffix(".exe").unwrap_or(raw);
    if basename == RUNTIME_BASENAME {
        return Err(DispatchError::BareRuntime);
    }
    match basename.strip_prefix(SYMLINK_PREFIX) {
        Some(name) if !name.is_empty() => Ok(name.to_string()),
        _ => Err(DispatchError::UnknownBasename(basename.to_string())),
    }
}

/// Cross-check argv[0]-derived name against the profile's declared name.
/// Called after profile load; refuses to run on mismatch.
pub fn verify_name_match(argv0_name: &str, profile_name: &str) -> Result<(), DispatchError> {
    if argv0_name == profile_name {
        Ok(())
    } else {
        Err(DispatchError::SpoofDetected {
            argv0: argv0_name.to_string(),
            profile_name: profile_name.to_string(),
        })
    }
}
