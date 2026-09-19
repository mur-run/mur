//! The search providers `mur deep-research` can be given a key for.
//!
//! Lives in mur-common because BOTH ends need the same names and neither can
//! see the other: `mur-core` owns the `mur deep-research secret` command that
//! WRITES a key, and `mur-research-gateway` is the standalone binary that
//! READS it. mur-core does not depend on the gateway crate (and must not —
//! the gateway is deliberately dependency-light), so a provider list defined
//! in either one would have to be duplicated in the other, and a config key
//! spelled `serpapi_api_key_ref` on the write side and `serp_api_key_ref` on
//! the read side would store a secret that is never found. One enum, both
//! ends (CLAUDE.md rule 1).

/// A web-search backend the gateway's `search` tool can use instead of
/// scraping DuckDuckGo's keyless HTML endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SearchProvider {
    /// Brave Search API — the default and the only one that predates this
    /// enum. Free tier covers a personal deep-research user.
    Brave,
    /// Tavily — search API built for LLM agents.
    Tavily,
    /// SerpApi — Google results via a scraping API.
    SerpApi,
    /// Firecrawl — search + page extraction.
    Firecrawl,
}

/// The order the gateway tries configured providers in when the operator has
/// not named one explicitly. Brave is first because it is the documented
/// default and the only provider any existing install can already have a key
/// for — adding three more must never silently move an existing user off the
/// backend they configured.
pub const PROVIDER_PREFERENCE: [SearchProvider; 4] = [
    SearchProvider::Brave,
    SearchProvider::Tavily,
    SearchProvider::SerpApi,
    SearchProvider::Firecrawl,
];

/// Keychain service name every MUR credential is filed under. Matches
/// `mur-core`'s `sources::credentials::SERVICE`, and is the `mur` in the
/// documented `keychain:mur/brave` ref.
pub const KEYCHAIN_SERVICE: &str = "mur";

impl SearchProvider {
    /// Every provider, in preference order.
    pub fn all() -> [SearchProvider; 4] {
        PROVIDER_PREFERENCE
    }

    /// Lowercase identifier used in the CLI flag (`--brave`), the keychain
    /// account, and the config key prefix. One slug drives all three so they
    /// cannot drift apart.
    pub fn slug(self) -> &'static str {
        match self {
            SearchProvider::Brave => "brave",
            SearchProvider::Tavily => "tavily",
            SearchProvider::SerpApi => "serpapi",
            SearchProvider::Firecrawl => "firecrawl",
        }
    }

    /// How the provider writes its own name, for anything a human reads.
    pub fn display_name(self) -> &'static str {
        match self {
            SearchProvider::Brave => "Brave Search",
            SearchProvider::Tavily => "Tavily",
            SearchProvider::SerpApi => "SerpApi",
            SearchProvider::Firecrawl => "Firecrawl",
        }
    }

    /// `research_gateway.<this>` — the config.yaml key holding a `SecretRef`
    /// string rather than the secret itself.
    pub fn config_key_ref(self) -> String {
        format!("{}_api_key_ref", self.slug())
    }

    /// `research_gateway.<this>` — the legacy plaintext key. Still read (an
    /// existing `brave_api_key` must keep working) but never written by
    /// `mur deep-research secret`.
    pub fn config_key_plain(self) -> String {
        format!("{}_api_key", self.slug())
    }

    /// Environment override, highest precedence of all.
    pub fn env_var(self) -> String {
        format!("MUR_RESEARCH_{}_KEY", self.slug().to_ascii_uppercase())
    }

    /// Keychain account this provider's key is stored under.
    pub fn keychain_account(self) -> &'static str {
        self.slug()
    }

    /// The `SecretRef` string written into config.yaml — the reference is safe
    /// to commit and log; the key itself never enters the file.
    pub fn keychain_ref(self) -> String {
        format!("keychain:{KEYCHAIN_SERVICE}/{}", self.keychain_account())
    }

    /// Where a user gets a key. Printed by the setup command, because "get an
    /// API key" without a URL is a scavenger hunt.
    pub fn signup_url(self) -> &'static str {
        match self {
            SearchProvider::Brave => "https://brave.com/search/api/",
            SearchProvider::Tavily => "https://app.tavily.com/",
            SearchProvider::SerpApi => "https://serpapi.com/manage-api-key",
            SearchProvider::Firecrawl => "https://www.firecrawl.dev/app/api-keys",
        }
    }
}

impl std::fmt::Display for SearchProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.slug())
    }
}

impl std::str::FromStr for SearchProvider {
    type Err = String;

    /// Accepts the slug in any case, plus the spellings a user is likely to
    /// type by hand (`serp-api`, `serp_api`) — a rejected name here costs a
    /// round trip for no safety gain.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase().replace(['-', '_'], "");
        match normalized.as_str() {
            "brave" | "bravesearch" => Ok(SearchProvider::Brave),
            "tavily" => Ok(SearchProvider::Tavily),
            "serpapi" | "serp" => Ok(SearchProvider::SerpApi),
            "firecrawl" => Ok(SearchProvider::Firecrawl),
            _ => Err(format!(
                "unknown search provider '{s}' — expected one of: {}",
                SearchProvider::all()
                    .iter()
                    .map(|p| p.slug())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// The whole reason this enum is in mur-common: the writer (mur-core) and
    /// the reader (the gateway) must derive the SAME strings. Pin them, so a
    /// rename on one side fails here instead of silently storing a key the
    /// gateway never looks for.
    #[test]
    fn config_keys_and_refs_are_derived_from_one_slug() {
        assert_eq!(SearchProvider::Brave.config_key_ref(), "brave_api_key_ref");
        assert_eq!(SearchProvider::Brave.config_key_plain(), "brave_api_key");
        assert_eq!(SearchProvider::Brave.env_var(), "MUR_RESEARCH_BRAVE_KEY");
        assert_eq!(SearchProvider::Brave.keychain_ref(), "keychain:mur/brave");

        assert_eq!(
            SearchProvider::SerpApi.config_key_ref(),
            "serpapi_api_key_ref"
        );
        assert_eq!(
            SearchProvider::Firecrawl.env_var(),
            "MUR_RESEARCH_FIRECRAWL_KEY"
        );
    }

    /// `brave_api_key_ref` / `MUR_RESEARCH_BRAVE_KEY` / `keychain:mur/brave`
    /// already exist in shipped configs and in the gateway's operator advice.
    /// Deriving them from the enum must reproduce them EXACTLY, or this change
    /// silently orphans every existing Brave key.
    #[test]
    fn brave_spellings_match_what_already_ships() {
        let cfg = SearchProvider::Brave;
        // As printed by fetcher.rs's search_blocked_error operator advice.
        assert_eq!(cfg.config_key_ref(), "brave_api_key_ref");
        assert_eq!(cfg.keychain_ref(), "keychain:mur/brave");
        // As read by config.rs's ENV_BRAVE_KEY.
        assert_eq!(cfg.env_var(), "MUR_RESEARCH_BRAVE_KEY");
    }

    #[test]
    fn parses_case_and_punctuation_variants() {
        assert_eq!(
            SearchProvider::from_str("Brave").unwrap(),
            SearchProvider::Brave
        );
        assert_eq!(
            SearchProvider::from_str("  TAVILY  ").unwrap(),
            SearchProvider::Tavily
        );
        // A user typing the product name by hand gets all three spellings.
        for s in ["serpapi", "SerpApi", "serp-api", "serp_api"] {
            assert_eq!(
                SearchProvider::from_str(s).unwrap(),
                SearchProvider::SerpApi,
                "failed to parse {s}"
            );
        }
    }

    #[test]
    fn unknown_provider_names_the_valid_choices() {
        let err = SearchProvider::from_str("google").unwrap_err();
        assert!(err.contains("google"), "{err}");
        // The message must list what IS accepted, not just reject.
        for p in SearchProvider::all() {
            assert!(err.contains(p.slug()), "{err} is missing {p}");
        }
    }

    /// Brave must stay first: it is the only provider an existing install can
    /// already hold a key for, and auto-selection walks this order.
    #[test]
    fn brave_leads_the_preference_order() {
        assert_eq!(PROVIDER_PREFERENCE[0], SearchProvider::Brave);
        assert_eq!(SearchProvider::all().len(), 4);
    }

    #[test]
    fn slugs_are_unique() {
        let mut slugs: Vec<_> = SearchProvider::all().iter().map(|p| p.slug()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), 4, "two providers share a slug");
    }
}
