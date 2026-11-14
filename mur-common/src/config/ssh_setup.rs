//! SSH setup journey switches (`ssh_setup:`).

use super::*;

/// Dedicated-key generation and public-key install are offered.
pub const SSH_SETUP_DEFAULT_GENERATE_KEY: bool = true;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshSetupConfig {
    /// Off: SSH setup offers no dedicated key and no public-key install.
    /// A key that already exists is still loaded at start and still removed
    /// with its agent, so turning this off strands no host.
    #[serde(default = "ssh_setup_default_generate_key")]
    pub generate_key: bool,
}

impl Default for SshSetupConfig {
    fn default() -> Self {
        Self {
            generate_key: ssh_setup_default_generate_key(),
        }
    }
}

fn ssh_setup_default_generate_key() -> bool {
    SSH_SETUP_DEFAULT_GENERATE_KEY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_key_is_on_when_the_block_is_absent() {
        let c: Config = serde_yaml::from_str("{}").unwrap();
        assert_eq!(c.ssh_setup.generate_key, SSH_SETUP_DEFAULT_GENERATE_KEY);
        assert!(c.ssh_setup.generate_key);
    }

    #[test]
    fn generate_key_is_on_when_the_block_is_empty() {
        let c: Config = serde_yaml::from_str("ssh_setup: {}\n").unwrap();
        assert!(c.ssh_setup.generate_key);
    }

    #[test]
    fn generate_key_can_be_turned_off() {
        let c: Config = serde_yaml::from_str("ssh_setup:\n  generate_key: false\n").unwrap();
        assert!(!c.ssh_setup.generate_key);
    }

    #[test]
    fn the_switch_survives_a_round_trip() {
        let c: Config = serde_yaml::from_str("ssh_setup:\n  generate_key: false\n").unwrap();
        let back: Config = serde_yaml::from_str(&serde_yaml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.ssh_setup, c.ssh_setup);
        // An unrelated config keeps the default through a round trip.
        let plain: Config = serde_yaml::from_str("llm: {}\n").unwrap();
        let back: Config = serde_yaml::from_str(&serde_yaml::to_string(&plain).unwrap()).unwrap();
        assert_eq!(back.ssh_setup, SshSetupConfig::default());
    }
}
