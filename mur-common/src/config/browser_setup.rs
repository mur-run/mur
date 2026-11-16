//! Browser setup journey switches (`browser_setup:`).

use super::*;

/// The `browser_request` route ships dark (#2360): nothing yet shows the
/// user a card for a recorded request, and the route tells the agent to stop
/// and not offer the setup commands. Flip this default once murmur or Hub
/// renders the card.
pub const BROWSER_SETUP_DEFAULT_REQUEST_ROUTE: bool = false;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSetupConfig {
    /// On: every agent gets the built-in `browser_request` tool and the
    /// `## Browser` prompt section. Off: neither is registered. An explicit
    /// profile `Deny` still wins when on.
    #[serde(default = "browser_setup_default_request_route")]
    pub request_route: bool,
}

impl Default for BrowserSetupConfig {
    fn default() -> Self {
        Self {
            request_route: browser_setup_default_request_route(),
        }
    }
}

fn browser_setup_default_request_route() -> bool {
    BROWSER_SETUP_DEFAULT_REQUEST_ROUTE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_route_is_off_when_the_block_is_absent() {
        let c: Config = serde_yaml::from_str("{}").unwrap();
        assert_eq!(
            c.browser_setup.request_route,
            BROWSER_SETUP_DEFAULT_REQUEST_ROUTE
        );
        assert!(!c.browser_setup.request_route);
    }

    #[test]
    fn request_route_is_off_when_the_block_is_empty() {
        let c: Config = serde_yaml::from_str("browser_setup: {}\n").unwrap();
        assert!(!c.browser_setup.request_route);
    }

    #[test]
    fn request_route_can_be_turned_on() {
        let c: Config = serde_yaml::from_str("browser_setup:\n  request_route: true\n").unwrap();
        assert!(c.browser_setup.request_route);
    }

    #[test]
    fn the_switch_survives_a_round_trip() {
        let c: Config = serde_yaml::from_str("browser_setup:\n  request_route: true\n").unwrap();
        let back: Config = serde_yaml::from_str(&serde_yaml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.browser_setup, c.browser_setup);
    }
}
