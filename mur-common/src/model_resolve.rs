//! First-run model-resolution decision tree (spec §7.3). Pure logic shared
//! by the CLI (Plan 1b) and the Hub GUI wizard (Plan 2). Hardware detection
//! and the actual pull/key-entry actions live in the surface layers; this
//! module only decides the recommended default given a hint + hardware.

use crate::muragent::manifest::ModelHint;

/// Recipient hardware snapshot. `apple_silicon` and `ollama_present` inform
/// which options a surface offers; the recommendation itself keys off RAM
/// and the hint's `local_capable` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hardware {
    pub total_ram_gb: u32,
    pub apple_silicon: bool,
    pub ollama_present: bool,
}

/// The highlighted default the wizard should pre-select. All escape hatches
/// (local pull / paste key / endpoint) remain available regardless (§7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recommendation {
    Local,
    Cloud,
    CloudOrSmallerLocal,
    NeutralMenu,
}

pub fn recommend(hint: Option<&ModelHint>, hw: &Hardware) -> Recommendation {
    match hint {
        None => Recommendation::NeutralMenu,
        Some(h) if !h.local_capable => Recommendation::Cloud,
        Some(h) if hw.total_ram_gb >= h.min_ram_gb => Recommendation::Local,
        Some(_) => Recommendation::CloudOrSmallerLocal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muragent::manifest::ModelTier;

    fn hint(local: bool, min_ram: u32) -> ModelHint {
        ModelHint {
            provider: "p".into(),
            name: "n".into(),
            tier: if local { ModelTier::Small } else { ModelTier::Frontier },
            min_ram_gb: min_ram,
            local_capable: local,
        }
    }
    fn hw(ram: u32) -> Hardware {
        Hardware { total_ram_gb: ram, apple_silicon: true, ollama_present: true }
    }

    #[test]
    fn no_hint_is_neutral() {
        assert_eq!(recommend(None, &hw(16)), Recommendation::NeutralMenu);
    }
    #[test]
    fn frontier_is_cloud() {
        assert_eq!(recommend(Some(&hint(false, 0)), &hw(16)), Recommendation::Cloud);
    }
    #[test]
    fn local_with_enough_ram_is_local() {
        assert_eq!(recommend(Some(&hint(true, 8)), &hw(16)), Recommendation::Local);
    }
    #[test]
    fn local_without_ram_is_cloud_or_smaller() {
        assert_eq!(
            recommend(Some(&hint(true, 16)), &hw(8)),
            Recommendation::CloudOrSmallerLocal
        );
    }
}
