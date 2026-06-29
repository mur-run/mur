#![allow(dead_code, unused_imports)]
//! Build a per-agent prompt that constrains the agent to its assigned region.

use super::RegionAssignment;
use crate::parallel::semantic::SemanticUnit;

/// Build a prompt addition that constrains an agent to only its assigned units.
pub fn region_prompt(file: &str, units: &[SemanticUnit], assigned: &RegionAssignment) -> String {
    let mut s = format!(
        "You are editing `{file}`. Only modify the following items — do not touch anything else:\n"
    );
    for name in &assigned.unit_names {
        if let Some(u) = units.iter().find(|u| &u.name == name) {
            s.push_str(&format!(
                "  - {:?} `{}` (lines {}–{})\n",
                u.kind,
                u.name,
                u.line_range.start + 1,
                u.line_range.end
            ));
        } else {
            s.push_str(&format!("  - `{name}`\n"));
        }
    }
    s.push_str("Leave every other function, struct, impl, and item exactly as-is.");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::semantic::UnitKind;

    fn unit(name: &str, l0: u32, l1: u32) -> SemanticUnit {
        SemanticUnit {
            kind: UnitKind::Fn,
            name: name.into(),
            byte_range: 0..1,
            line_range: l0..l1,
            content_hash: [0u8; 32],
            dependencies: vec![],
        }
    }

    #[test]
    fn prompt_names_only_assigned_units_and_file() {
        let units = vec![unit("alpha", 0, 5), unit("beta", 6, 9), unit("gamma", 10, 20)];
        let assigned = RegionAssignment {
            track_name: "t0".into(),
            unit_names: vec!["alpha".into(), "gamma".into()],
        };
        let p = region_prompt("src/widget.rs", &units, &assigned);
        assert!(p.contains("src/widget.rs"));
        assert!(p.contains("alpha"));
        assert!(p.contains("gamma"));
        assert!(!p.contains("beta"), "must not mention another agent's unit");
        // 1-based line numbers
        assert!(p.contains("1") && p.contains("20"));
        // states don't-touch-others constraint
        assert!(p.to_lowercase().contains("only"));
    }
}
