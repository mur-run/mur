//! Stub for M1.6 — interactive + non-interactive onboarding wizard.

use std::path::PathBuf;

pub async fn run(_name: &str, _answers: Option<PathBuf>, _re_init: bool) -> anyhow::Result<()> {
    anyhow::bail!("M1.5 stub — implemented in M1.6")
}
