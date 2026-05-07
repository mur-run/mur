use super::{SandboxPolicy, SandboxStatus};

pub fn apply_windows(_policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    Ok(SandboxStatus {
        platform: "windows-stub".to_string(),
        effective_abi: None,
        enforcing: false,
    })
}
