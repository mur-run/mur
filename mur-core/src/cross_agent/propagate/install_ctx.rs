//! `InstallContext` distinguishes manual installs from auto-propagated
//! installs so the credit ledger can be written with correct `kind`
//! and evidence (M7c §3.5).

#[derive(Debug, Clone)]
pub enum InstallContext {
    /// User typed `mur skill install ...` (or any non-propagate path).
    Manual,
    /// Triggered by `skill-propagate` sweep — the source agent's fitness
    /// and sample count at decision time are captured for the ledger.
    AutoPropagate {
        source_fitness: f64,
        source_samples: u64,
    },
}

impl InstallContext {
    pub fn is_auto_propagate(&self) -> bool {
        matches!(self, InstallContext::AutoPropagate { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_is_not_auto() {
        assert!(!InstallContext::Manual.is_auto_propagate());
    }

    #[test]
    fn auto_is_auto() {
        assert!(
            InstallContext::AutoPropagate {
                source_fitness: 0.5,
                source_samples: 3
            }
            .is_auto_propagate()
        );
    }
}
