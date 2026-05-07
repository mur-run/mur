use super::{SandboxPolicy, SandboxStatus};

pub fn apply_linux(_policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    Ok(SandboxStatus {
        platform: "linux-stub".to_string(),
        effective_abi: None,
        enforcing: false,
    })
}
