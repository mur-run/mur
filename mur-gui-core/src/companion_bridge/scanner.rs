//! One-shot scan of an inbox directory — returns all parseable `.md` files
//! sorted by `generated_at`.

use std::path::Path;

use anyhow::Result;

use super::event::{BridgeEvent, parse_inbox_md};

pub fn scan_pending(inbox_dir: &Path) -> Result<Vec<BridgeEvent>> {
    if !inbox_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(inbox_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        match parse_inbox_md(&path) {
            Ok(ev) => out.push(ev),
            Err(e) => {
                tracing::warn!(
                    "companion_bridge: skipping malformed inbox file {}: {e:#}",
                    path.display()
                );
            }
        }
    }
    out.sort_by(|a, b| a.generated_at.cmp(&b.generated_at));
    Ok(out)
}
