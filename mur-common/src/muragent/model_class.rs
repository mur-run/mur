//! Static model-class table: map a (provider, name) binding to a `ModelHint`
//! (tier, RAM estimate, local-capability). Heuristic + conservative on
//! unknown providers. See spec §7.1.

use crate::muragent::manifest::{ModelHint, ModelTier};

const LOCAL_PROVIDERS: &[&str] =
    &["ollama", "mlx", "llamacpp", "llama_cpp", "localai", "lmstudio"];
const CLOUD_PROVIDERS: &[&str] = &[
    "anthropic", "openai", "google", "gemini", "mistral", "groq", "cohere",
    "deepseek", "xai", "openrouter",
];
const SMALL_MARKERS: &[&str] =
    &["1b", "1.5b", "2b", "3b", "mini", "nano", "haiku", "flash", "small", "tiny"];
const LARGE_LOCAL_MARKERS: &[&str] = &["70b", "72b", "405b", "mixtral", "command-r-plus"];

/// Map a model binding to a `ModelHint`. Local providers classify by size
/// markers in the name; known cloud providers are frontier unless a "small"
/// variant marker is present; unknown providers fall back to a conservative
/// local-capable mid tier (the wizard still offers all options).
pub fn classify(provider: &str, name: &str) -> ModelHint {
    let p = provider.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|m| n.contains(m));

    if LOCAL_PROVIDERS.contains(&p.as_str()) {
        let (tier, min_ram_gb) = if has(SMALL_MARKERS) {
            (ModelTier::Small, 8)
        } else if has(LARGE_LOCAL_MARKERS) {
            (ModelTier::Frontier, 64)
        } else {
            (ModelTier::Mid, 16)
        };
        return ModelHint {
            provider: provider.to_string(),
            name: name.to_string(),
            tier,
            min_ram_gb,
            local_capable: true,
        };
    }

    if CLOUD_PROVIDERS.contains(&p.as_str()) {
        let tier = if has(SMALL_MARKERS) {
            ModelTier::Mid
        } else {
            ModelTier::Frontier
        };
        return ModelHint {
            provider: provider.to_string(),
            name: name.to_string(),
            tier,
            min_ram_gb: 0,
            local_capable: false,
        };
    }

    // Unknown provider: conservative — assume a local-capable mid model.
    ModelHint {
        provider: provider.to_string(),
        name: name.to_string(),
        tier: ModelTier::Mid,
        min_ram_gb: 16,
        local_capable: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_small_model() {
        let h = classify("ollama", "llama3.2:3b");
        assert_eq!(h.tier, ModelTier::Small);
        assert!(h.local_capable);
        assert_eq!(h.min_ram_gb, 8);
    }

    #[test]
    fn cloud_frontier_model() {
        let h = classify("anthropic", "claude-opus-4-7");
        assert_eq!(h.tier, ModelTier::Frontier);
        assert!(!h.local_capable);
        assert_eq!(h.min_ram_gb, 0);
    }

    #[test]
    fn cloud_small_variant_is_mid() {
        let h = classify("openai", "gpt-4o-mini");
        assert_eq!(h.tier, ModelTier::Mid);
        assert!(!h.local_capable);
    }

    #[test]
    fn unknown_provider_is_conservative_local_mid() {
        let h = classify("acme-llm", "whatever-v2");
        assert_eq!(h.tier, ModelTier::Mid);
        assert!(h.local_capable);
        assert_eq!(h.min_ram_gb, 16);
    }
}
