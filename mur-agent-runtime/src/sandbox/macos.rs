use super::{SandboxPolicy, SandboxStatus};

pub fn apply_macos(_policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    Ok(SandboxStatus {
        platform: "macos-stub".to_string(),
        effective_abi: None,
        enforcing: false,
    })
}
