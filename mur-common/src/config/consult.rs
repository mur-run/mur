//! Channel consult tuning (`consult:`).

use super::*;

/// Default lifetime of an `Ask`, in seconds: how long after it is written a
/// consult may still answer it.
pub const CONSULT_DEFAULT_ASK_EXPIRY_SECS: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsultConfig {
    /// Seconds an `Ask` stays answerable. Signed into the event as
    /// `expires_at_ms`; consult refuses an `Ask` past it. `0` is not a
    /// usable window (every `Ask` would be born expired), so it means the
    /// default.
    #[serde(default = "consult_default_ask_expiry_secs")]
    pub ask_expiry_secs: u64,
}

fn consult_default_ask_expiry_secs() -> u64 {
    CONSULT_DEFAULT_ASK_EXPIRY_SECS
}

impl Default for ConsultConfig {
    fn default() -> Self {
        Self {
            ask_expiry_secs: CONSULT_DEFAULT_ASK_EXPIRY_SECS,
        }
    }
}

impl ConsultConfig {
    /// The window an `Ask` written now is answerable for.
    pub fn ask_expiry(&self) -> std::time::Duration {
        let secs = match self.ask_expiry_secs {
            0 => CONSULT_DEFAULT_ASK_EXPIRY_SECS,
            s => s,
        };
        std::time::Duration::from_secs(secs)
    }

    /// `expires_at_ms` for an `Ask` written at `now_ms` (Unix milliseconds).
    /// Saturates rather than wrapping on an absurd configured window.
    pub fn ask_expires_at_ms(&self, now_ms: u64) -> u64 {
        let window = u64::try_from(self.ask_expiry().as_millis()).unwrap_or(u64::MAX);
        now_ms.saturating_add(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_without_consult_block_uses_the_default() {
        let c: Config = serde_yaml::from_str("llm: {}\n").unwrap();
        assert_eq!(c.consult.ask_expiry_secs, CONSULT_DEFAULT_ASK_EXPIRY_SECS);
        assert_eq!(c.consult.ask_expires_at_ms(1_000), 1_000 + 600_000);
    }

    #[test]
    fn configured_window_is_used() {
        let c: Config = serde_yaml::from_str("consult:\n  ask_expiry_secs: 30\n").unwrap();
        assert_eq!(c.consult.ask_expires_at_ms(5_000), 35_000);
    }

    #[test]
    fn zero_means_default_not_born_expired() {
        let c = ConsultConfig { ask_expiry_secs: 0 };
        assert_eq!(c.ask_expiry().as_secs(), CONSULT_DEFAULT_ASK_EXPIRY_SECS);
    }

    #[test]
    fn huge_window_saturates() {
        let c = ConsultConfig {
            ask_expiry_secs: u64::MAX,
        };
        assert_eq!(c.ask_expires_at_ms(10), u64::MAX);
    }
}
