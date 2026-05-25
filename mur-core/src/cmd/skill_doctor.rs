//! `mur skill doctor` — read-only skill health checks (M5a).

use anyhow::Result;

pub fn cmd_doctor(
    _names: &[String],
    _checks: &[String],
    _json: bool,
    _strict: bool,
    fix: bool,
    apply: bool,
) -> Result<()> {
    if fix {
        eprintln!("warning: --fix is accepted but not yet implemented (requires M5b). Showing findings only.");
    }
    if apply {
        eprintln!("warning: --apply requires --fix and M5b's repair engine. Showing findings only.");
    }
    println!("No findings — doctor implementation in progress (M5a).");
    Ok(())
}
