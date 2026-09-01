//! Named model registry shared by all agents.
//!
//! On disk: `~/.mur/models.yaml`. Schema:
//!
//! ```yaml
//! schema_version: 1
//! models:
//!   anthropic_opus_4_7:
//!     provider: anthropic
//!     model: claude-opus-4-7
//!     secret: env:ANTHROPIC_API_KEY
//!     capabilities: [chat, tools]
//! ```

use crate::route::{RoutePolicy, RouteTier};
use crate::secret::SecretRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModelEntry {
    #[serde(default)]
    pub provider: String,
    /// Who makes this model — the models.dev catalog vendor (`deepseek`,
    /// `groq`, `mistral`, …).
    ///
    /// Distinct from `provider`, which is the wire protocol MUR dials: a
    /// DeepSeek entry is `provider: openai` + `vendor: deepseek`, because the
    /// runtime reaches it over the OpenAI protocol while the catalog files it
    /// under DeepSeek. Only recorded when the two differ — for Anthropic,
    /// OpenAI and Ollama the protocol already names the vendor.
    ///
    /// `None` on entries written before this field existed; readers should go
    /// through [`ModelEntry::vendor_candidates`] rather than reading it raw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    /// Routing tier: cheap/local vs frontier/expensive.
    /// When absent, the router infers based on provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<RouteTier>,
    /// Estimated USD cost per 1000 output tokens.
    /// Used for ledger cost estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_tokens: Option<f64>,
    /// Estimated USD cost per 1000 input tokens.
    /// New field for split input/output cost tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_per_1k: Option<f64>,
    /// Estimated USD cost per 1000 output tokens.
    /// New field for split input/output cost tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost_per_1k: Option<f64>,
    /// Model context window size in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// When the rates above were recorded.
    ///
    /// Vendors move prices; a rate written months ago is a guess wearing the
    /// costume of a fact, and nothing else on this struct can tell the two
    /// apart. `None` means unknown — entries predating this field, or hand-
    /// written ones — which is honest rather than defaulting to "fresh".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priced_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Vendor label implied by an endpoint host: `https://api.deepseek.com/v1` →
/// `deepseek`. Best-effort — a host that does not carry the vendor's name
/// (Google's `generativelanguage.googleapis.com`) yields the wrong label,
/// which is why `vendor` is recorded explicitly on new entries.
fn vendor_label_of_url(base_url: Option<&str>) -> Option<String> {
    let host = base_url?
        .split("//")
        .nth(1)
        .unwrap_or(base_url?)
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    let label = host.strip_prefix("api.").unwrap_or(host);
    let first = label.split('.').next().unwrap_or("");
    (!first.is_empty()).then(|| first.to_string())
}

impl ModelEntry {
    /// Resolve effective per-1k rates as `(input, output)`.
    ///
    /// The deprecated `cost_per_1k_tokens` is treated as the output rate and
    /// also as the input fallback, so legacy single-rate entries keep working.
    pub fn effective_costs(&self) -> (Option<f64>, Option<f64>) {
        let output = self.output_cost_per_1k.or(self.cost_per_1k_tokens);
        let input = self.input_cost_per_1k.or(self.cost_per_1k_tokens);
        (input, output)
    }

    /// Catalog vendor names to try for this entry, most specific first.
    ///
    /// The recorded `vendor` wins. Failing that — legacy entries, or anything
    /// written by hand — the host of `base_url` is tried
    /// (`https://api.deepseek.com` → `deepseek`), then `provider`, which names
    /// the vendor only when the vendor happens to have its own client.
    ///
    /// Every caller that asks an external catalog about an entry must go
    /// through this. Asking with `provider` alone reports every
    /// OpenAI-compatible third party as unknown.
    pub fn vendor_candidates(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(3);
        let mut push = |v: &str| {
            if !v.is_empty() && !out.iter().any(|e| e == v) {
                out.push(v.to_string());
            }
        };
        if let Some(v) = self.vendor.as_deref() {
            push(v);
        }
        if let Some(label) = vendor_label_of_url(self.base_url.as_deref()) {
            push(&label);
        }
        push(&self.provider);
        out
    }

    /// Whether this entry carries any rate at all.
    pub fn is_priced(&self) -> bool {
        let (input, output) = self.effective_costs();
        input.is_some() || output.is_some()
    }

    /// Stamp `priced_at` with `now`, but only if a rate is actually present —
    /// a date on an unpriced entry would claim a freshness it does not have.
    /// Never overwrites an existing stamp with an older one.
    pub fn stamp_priced_at(&mut self, now: chrono::DateTime<chrono::Utc>) {
        if self.is_priced() && self.priced_at.is_none_or(|prev| prev < now) {
            self.priced_at = Some(now);
        }
    }

    /// How long ago the rates were recorded, or `None` when unstamped.
    pub fn price_age(&self, now: chrono::DateTime<chrono::Utc>) -> Option<chrono::TimeDelta> {
        self.priced_at.map(|at| now - at)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RoleEntry {
    /// Registry model ID (key in `models:`) to use as primary.
    pub primary: String,
    /// Fallback model ID if primary is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Optional daily cost cap in USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_budget_per_day_usd: Option<f64>,
    /// If true, only use local models when handling sensitive data.
    #[serde(default)]
    pub privacy_local_only: bool,
    /// Per-role routing policy override.
    /// When absent, the router uses the default heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_policy: Option<RoutePolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRegistry {
    pub schema_version: u32,
    #[serde(default)]
    pub models: BTreeMap<String, ModelEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, RoleEntry>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self {
            schema_version: 1,
            models: BTreeMap::new(),
            roles: BTreeMap::new(),
        }
    }
}

impl ModelRegistry {
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let body = std::fs::read_to_string(path)?;
        if body.trim().is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_yaml_ng::from_str(&body)?)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_yaml_ng::to_string(self)?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn default_path() -> anyhow::Result<PathBuf> {
        // Honor MUR_HOME (used by test harnesses and Windows CI, where
        // `dirs::home_dir()` reads SHGetKnownFolderPath and ignores HOME).
        if let Ok(p) = std::env::var("MUR_HOME")
            && !p.is_empty()
        {
            return Ok(PathBuf::from(p).join("models.yaml"));
        }
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        Ok(home.join(".mur/models.yaml"))
    }

    /// Return the primary model ID for `role`, or the fallback if the primary
    /// is not in the `models` map, or `None` if the role is not configured.
    pub fn resolve_role(&self, role: &str) -> Option<&str> {
        let entry = self.roles.get(role)?;
        if self.models.contains_key(&entry.primary) {
            return Some(&entry.primary);
        }
        // primary not in registry — try fallback
        if let Some(fb) = &entry.fallback
            && self.models.contains_key(fb)
        {
            return Some(fb);
        }
        // role configured but no available model
        None
    }
}

use crate::agent::AgentProfile;
use crate::config::{DEFAULT_ROUTING_THRESHOLD, ModelSwitchConfig, RoutingConfig};

/// Build the ordered list of model_refs to try: `[primary, ...fallback]`.
/// Priority per-agent → global. The primary is de-duplicated out of the chain
/// (no point retrying the same ref back-to-back). Returns empty when nothing is
/// configured, so the caller keeps today's single-inline-model behaviour.
pub fn resolve_model_refs(
    profile: &AgentProfile,
    cfg: &ModelSwitchConfig,
    routed_primary: Option<String>,
) -> Vec<String> {
    let primary = routed_primary
        .or_else(|| profile.model_ref.clone())
        .or_else(|| cfg.default.clone());
    let chain = if !profile.fallback_chain.is_empty() {
        profile.fallback_chain.clone()
    } else {
        cfg.fallback_chain.clone()
    };
    let mut out: Vec<String> = Vec::new();
    if let Some(p) = primary {
        out.push(p);
    }
    for r in chain {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// Opt-in difficulty heuristic: pick `frontier` when the estimated input token
/// count exceeds the threshold, else `cheap`. `None` when misconfigured (caller
/// falls through to model_ref/global default).
pub fn choose_by_difficulty(est_input_tokens: u32, r: &RoutingConfig) -> Option<String> {
    let threshold = r
        .threshold_input_tokens
        .unwrap_or(DEFAULT_ROUTING_THRESHOLD);
    match (r.cheap.as_ref(), r.frontier.as_ref()) {
        (Some(cheap), Some(frontier)) => Some(if est_input_tokens > threshold {
            frontier.clone()
        } else {
            cheap.clone()
        }),
        _ => None,
    }
}

/// Registry capability strings. The baseline (`chat`) is legacy-permissive —
/// an entry with no `capabilities` at all predates the field and is assumed
/// chat-capable. Everything above the baseline is fail-closed.
pub const CAP_CHAT: &str = "chat";
pub const CAP_TOOLS: &str = "tools";
pub const CAP_VISION: &str = "vision";

/// A capability the request needs from whatever model serves it. Derived from
/// the request itself (an image in the messages, a tool list) and never from
/// config: a router may only substitute a model that can do the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// The request carries an image; the model has to be able to see it.
    Vision,
    /// The request declares tools; the model has to be able to call them.
    Tools,
}

impl Requirement {
    /// The registry capability an entry must declare to satisfy this.
    pub fn capability(self) -> &'static str {
        match self {
            Requirement::Vision => CAP_VISION,
            Requirement::Tools => CAP_TOOLS,
        }
    }

    /// Does an entry that declares NO capabilities at all satisfy this?
    ///
    /// The two requirements differ in how they fail, and the answer follows
    /// the failure mode rather than a blanket rule:
    ///
    /// - `Vision`: **no**. A model that cannot see answers an image request
    ///   with confident nonsense — silent, and unrecoverable for that turn.
    ///   That is the failure this gate exists to prevent, so silence about
    ///   vision is treated as absence of it.
    /// - `Tools`: **yes**. A model that cannot call tools fails loudly (the
    ///   provider rejects the request) and the existing retry/advance path
    ///   already handles it. Treating undeclared as incapable would drop every
    ///   entry written before `capabilities` existed — in practice most of a
    ///   real registry — out of the fallback chain of every tool-carrying turn,
    ///   which is a large regression bought for very little.
    ///
    /// An entry that DOES declare capabilities is taken at its word either
    /// way: if it enumerated what it can do and left `tools` out, that is a
    /// statement, not silence.
    fn permitted_when_undeclared(self) -> bool {
        match self {
            Requirement::Vision => false,
            Requirement::Tools => true,
        }
    }
}

/// Can this entry serve a request needing `reqs`?
///
/// No registry write path emits `vision` today, so a `Vision` requirement
/// disqualifies every current entry — auto-substitution goes inert for image
/// requests rather than answering them blind. The same code makes a finer
/// distinction the day entries start declaring it; there is no second version
/// of this function to write later.
pub fn satisfies(e: &ModelEntry, reqs: &[Requirement]) -> bool {
    let chat_capable = e.capabilities.is_empty() || e.capabilities.iter().any(|c| c == CAP_CHAT);
    if !chat_capable {
        return false;
    }
    reqs.iter().all(|r| {
        if e.capabilities.is_empty() {
            r.permitted_when_undeclared()
        } else {
            e.capabilities.iter().any(|c| c == r.capability())
        }
    })
}

/// Pick the cheapest registry entry that can serve a request needing `reqs`,
/// excluding `exclude` (the agent's own primary). None when no qualifying
/// entry exists → caller keeps normal candidates (fail-expensive).
pub fn pick_cheap_model(
    reg: &ModelRegistry,
    exclude: Option<&str>,
    reqs: &[Requirement],
) -> Option<String> {
    reg.models
        .iter()
        .filter(|(k, _)| exclude != Some(k.as_str()))
        .filter(|(_, e)| satisfies(e, reqs))
        .filter_map(|(k, e)| {
            // Not the deprecated field directly: `mur model add --output-cost`
            // deliberately leaves it unset, so reading it drops every entry
            // added with the current flags instead of ranking it.
            let (input, output) = e.effective_costs();
            output.or(input).map(|c| (c, k.clone()))
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, k)| k)
}

#[cfg(test)]
mod tests {

    #[test]
    fn vendor_candidates_prefer_the_recorded_vendor_then_the_host_then_provider() {
        // Recorded vendor wins — this is what new entries carry.
        let e = ModelEntry {
            provider: "openai".into(),
            vendor: Some("deepseek".into()),
            base_url: Some("https://api.deepseek.com/v1".into()),
            ..Default::default()
        };
        assert_eq!(e.vendor_candidates(), vec!["deepseek", "openai"]);

        // Legacy entry with no vendor: the endpoint host still identifies it,
        // which is how registries written before the field keep working.
        let legacy = ModelEntry {
            provider: "openai".into(),
            base_url: Some("https://api.deepseek.com/v1".into()),
            ..Default::default()
        };
        assert_eq!(legacy.vendor_candidates(), vec!["deepseek", "openai"]);

        // Nothing to infer: provider is all there is.
        let bare = ModelEntry {
            provider: "anthropic".into(),
            ..Default::default()
        };
        assert_eq!(bare.vendor_candidates(), vec!["anthropic"]);

        // No duplicate when host and provider agree.
        let same = ModelEntry {
            provider: "openai".into(),
            base_url: Some("https://api.openai.com/v1".into()),
            ..Default::default()
        };
        assert_eq!(same.vendor_candidates(), vec!["openai"]);
    }

    #[test]
    fn vendor_is_omitted_from_yaml_when_absent_and_round_trips_when_set() {
        let bare = ModelEntry {
            provider: "anthropic".into(),
            model: "claude-opus-5".into(),
            ..Default::default()
        };
        let y = serde_yaml_ng::to_string(&bare).unwrap();
        assert!(!y.contains("vendor"), "{y}");

        let tagged = ModelEntry {
            provider: "openai".into(),
            vendor: Some("groq".into()),
            model: "llama-3.3".into(),
            ..Default::default()
        };
        let y = serde_yaml_ng::to_string(&tagged).unwrap();
        let back: ModelEntry = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(back.vendor.as_deref(), Some("groq"));
    }
    use super::*;

    #[test]
    fn parses_full_registry() {
        let yaml = r#"
schema_version: 1
models:
  anthropic_opus_4_7:
    provider: anthropic
    model: claude-opus-4-7
    secret: env:ANTHROPIC_API_KEY
    capabilities: [chat, tools]
  ollama_llama3:
    provider: ollama
    model: llama3.2:3b
    base_url: http://127.0.0.1:11434
"#;
        let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(r.schema_version, 1);
        assert_eq!(r.models.len(), 2);
        let opus = r.models.get("anthropic_opus_4_7").unwrap();
        assert_eq!(opus.provider, "anthropic");
        assert_eq!(
            opus.secret,
            Some(SecretRef::Env("ANTHROPIC_API_KEY".into()))
        );
        assert!(r.models["ollama_llama3"].secret.is_none());
    }

    #[test]
    fn round_trip_preserves_shape() {
        let mut r = ModelRegistry::default();
        r.models.insert(
            "foo".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-opus-4-7".into(),
                base_url: None,
                secret: Some(SecretRef::Keychain {
                    service: "mur".into(),
                    account: "anthropic".into(),
                }),
                capabilities: vec!["chat".into()],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
                input_cost_per_1k: None,
                output_cost_per_1k: None,
                context_window: None,
                priced_at: None,
                ..Default::default()
            },
        );
        let s = serde_yaml_ng::to_string(&r).unwrap();
        let parsed: ModelRegistry = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(r, parsed);
    }

    #[test]
    fn rejects_unknown_secret_scheme() {
        let yaml = r#"
schema_version: 1
models:
  bad:
    provider: x
    model: y
    secret: bogus:value
"#;
        let r: Result<ModelRegistry, _> = serde_yaml_ng::from_str(yaml);
        assert!(r.is_err(), "should reject unknown scheme");
    }

    #[test]
    fn test_registry_roundtrip_with_roles() {
        let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
roles:
  reflector:
    primary: haiku
    fallback: null
    cost_budget_per_day_usd: 0.5
"#;
        let reg: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(reg.roles["reflector"].primary, "haiku");
        let back = serde_yaml_ng::to_string(&reg).unwrap();
        let reg2: ModelRegistry = serde_yaml_ng::from_str(&back).unwrap();
        assert_eq!(reg, reg2);
    }

    #[test]
    fn test_resolve_role_primary() {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "haiku".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
                input_cost_per_1k: None,
                output_cost_per_1k: None,
                context_window: None,
                priced_at: None,
                ..Default::default()
            },
        );
        reg.roles.insert(
            "reflector".into(),
            RoleEntry {
                primary: "haiku".into(),
                fallback: None,
                ..Default::default()
            },
        );
        assert_eq!(reg.resolve_role("reflector"), Some("haiku"));
    }

    #[test]
    fn test_resolve_role_fallback() {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "haiku".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
                input_cost_per_1k: None,
                output_cost_per_1k: None,
                context_window: None,
                priced_at: None,
                ..Default::default()
            },
        );
        reg.roles.insert(
            "reflector".into(),
            RoleEntry {
                primary: "nonexistent".into(),
                fallback: Some("haiku".into()),
                ..Default::default()
            },
        );
        assert_eq!(reg.resolve_role("reflector"), Some("haiku"));
    }

    #[test]
    fn test_resolve_role_none() {
        let reg = ModelRegistry::default();
        assert_eq!(reg.resolve_role("reflector"), None);
    }

    #[test]
    fn model_entry_parses_tier_field() {
        let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
    tier: local
  opus:
    provider: anthropic
    model: claude-opus-4-7
    tier: frontier
    cost_per_1k_tokens: 0.015
"#;
        let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(r.models["haiku"].tier, Some(RouteTier::Local));
        assert_eq!(r.models["opus"].tier, Some(RouteTier::Frontier));
        assert_eq!(r.models["opus"].cost_per_1k_tokens, Some(0.015));
        // Missing tier is None.
        let mut r2 = ModelRegistry::default();
        r2.models.insert(
            "x".into(),
            ModelEntry {
                provider: "ollama".into(),
                model: "llama3".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
                input_cost_per_1k: None,
                output_cost_per_1k: None,
                context_window: None,
                priced_at: None,
                ..Default::default()
            },
        );
        let yaml = serde_yaml_ng::to_string(&r2).unwrap();
        assert!(
            !yaml.contains("tier:"),
            "absent tier should not be serialized: {yaml}"
        );
    }

    #[test]
    fn role_entry_parses_route_policy() {
        let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
  opus:
    provider: anthropic
    model: claude-opus-4-7
roles:
  dev:
    primary: opus
    route_policy: !force_frontier
      model_id: opus
  reflector:
    primary: haiku
    route_policy: prefer_local
  curator:
    primary: haiku
    route_policy: force_local
  chat:
    primary: haiku
"#;
        let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(
            r.roles["dev"].route_policy,
            Some(RoutePolicy::ForceFrontier {
                model_id: "opus".into()
            })
        );
        assert_eq!(
            r.roles["reflector"].route_policy,
            Some(RoutePolicy::PreferLocal)
        );
        assert_eq!(
            r.roles["curator"].route_policy,
            Some(RoutePolicy::ForceLocal)
        );
        assert_eq!(r.roles["chat"].route_policy, None);
    }

    #[test]
    fn parses_split_cost_fields() {
        let yaml = r#"
schema_version: 1
models:
  opus:
    provider: anthropic
    model: claude-opus-4-8
    input_cost_per_1k: 0.005
    output_cost_per_1k: 0.025
    context_window: 200000
"#;
        let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        let e = r.models.get("opus").unwrap();
        assert_eq!(e.input_cost_per_1k, Some(0.005));
        assert_eq!(e.output_cost_per_1k, Some(0.025));
        assert_eq!(e.context_window, Some(200_000));
    }

    #[test]
    fn default_model_entry_is_empty() {
        let e = ModelEntry::default();
        assert!(e.provider.is_empty());
        assert_eq!(e.input_cost_per_1k, None);
        assert_eq!(e.output_cost_per_1k, None);
        assert_eq!(e.context_window, None);
    }

    #[test]
    fn effective_costs_fallback_matrix() {
        // legacy only → both fall back to the blended rate
        let mut e = ModelEntry {
            cost_per_1k_tokens: Some(0.01),
            ..Default::default()
        };
        assert_eq!(e.effective_costs(), (Some(0.01), Some(0.01)));

        // split only → split wins, legacy ignored
        e = ModelEntry {
            input_cost_per_1k: Some(0.005),
            output_cost_per_1k: Some(0.025),
            ..Default::default()
        };
        assert_eq!(e.effective_costs(), (Some(0.005), Some(0.025)));

        // both → split wins
        e = ModelEntry {
            cost_per_1k_tokens: Some(0.01),
            input_cost_per_1k: Some(0.005),
            output_cost_per_1k: Some(0.025),
            ..Default::default()
        };
        assert_eq!(e.effective_costs(), (Some(0.005), Some(0.025)));

        // none → none
        e = ModelEntry::default();
        assert_eq!(e.effective_costs(), (None, None));
    }
}

#[cfg(test)]
mod io_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_returns_empty_when_file_missing() {
        let dir = tempdir().unwrap();
        let r = ModelRegistry::load_from(&dir.path().join("nope.yaml")).unwrap();
        assert_eq!(r.models.len(), 0);
        assert_eq!(r.schema_version, 1);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("models.yaml");
        let mut r = ModelRegistry::default();
        r.models.insert(
            "x".into(),
            ModelEntry {
                provider: "ollama".into(),
                model: "llama3.2:3b".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
                input_cost_per_1k: None,
                output_cost_per_1k: None,
                context_window: None,
                priced_at: None,
                ..Default::default()
            },
        );
        r.save_to(&p).unwrap();
        let r2 = ModelRegistry::load_from(&p).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn save_uses_atomic_rename() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("models.yaml");
        ModelRegistry::default().save_to(&p).unwrap();
        let temp = dir.path().join("models.yaml.tmp");
        assert!(!temp.exists(), "atomic temp left behind");
    }
}

#[cfg(test)]
mod switch_tests {
    use super::*;
    use crate::agent::AgentProfile;
    use crate::config::{ModelSwitchConfig, RoutingConfig};

    fn profile(model_ref: Option<&str>, chain: &[&str]) -> AgentProfile {
        let mut p = AgentProfile::default_for_tests();
        p.model_ref = model_ref.map(|s| s.to_string());
        p.fallback_chain = chain.iter().map(|s| s.to_string()).collect();
        p
    }

    #[test]
    fn per_agent_primary_and_chain_win_over_global() {
        let cfg = ModelSwitchConfig {
            default: Some("global_default".into()),
            fallback_chain: vec!["g1".into(), "g2".into()],
            ..Default::default()
        };
        let p = profile(Some("agent_primary"), &["agent_primary", "agent_fb"]);
        // per-agent model_ref is primary; per-agent chain used; primary de-duped.
        assert_eq!(
            resolve_model_refs(&p, &cfg, None),
            vec!["agent_primary", "agent_fb"]
        );
    }

    #[test]
    fn falls_back_to_global_default_and_chain() {
        let cfg = ModelSwitchConfig {
            default: Some("global_default".into()),
            fallback_chain: vec!["g1".into(), "global_default".into()],
            ..Default::default()
        };
        let p = profile(None, &[]); // no per-agent model_ref or chain
        // primary = global default; global chain used; primary de-duped out.
        assert_eq!(
            resolve_model_refs(&p, &cfg, None),
            vec!["global_default", "g1"]
        );
    }

    #[test]
    fn routed_primary_overrides_model_ref() {
        let cfg = ModelSwitchConfig {
            fallback_chain: vec!["g1".into()],
            ..Default::default()
        };
        let p = profile(Some("agent_primary"), &[]);
        assert_eq!(
            resolve_model_refs(&p, &cfg, Some("frontier".into())),
            vec!["frontier", "g1"]
        );
    }

    #[test]
    fn no_config_no_agent_yields_empty() {
        // Nothing configured → empty vec (caller falls back to inline model).
        let cfg = ModelSwitchConfig::default();
        assert!(resolve_model_refs(&profile(None, &[]), &cfg, None).is_empty());
    }

    #[test]
    fn difficulty_picks_frontier_over_threshold() {
        let r = RoutingConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            frontier: Some("frontier".into()),
            threshold_input_tokens: Some(1000),
        };
        assert_eq!(choose_by_difficulty(1500, &r), Some("frontier".into()));
        assert_eq!(choose_by_difficulty(500, &r), Some("cheap".into()));
        // Misconfigured (missing frontier) → None (fall through).
        let bad = RoutingConfig {
            enabled: true,
            cheap: Some("c".into()),
            frontier: None,
            threshold_input_tokens: None,
        };
        assert_eq!(choose_by_difficulty(9999, &bad), None);
    }

    #[test]
    fn pick_cheap_model_lowest_cost_chat_excluding_primary() {
        let mut reg = ModelRegistry::default();
        let mk = |cost: f64, caps: &[&str]| ModelEntry {
            provider: "x".into(),
            model: "m".into(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            cost_per_1k_tokens: Some(cost),
            ..Default::default()
        };
        reg.models.insert("frontier".into(), mk(0.01, &["chat"]));
        reg.models.insert("cheap".into(), mk(0.0001, &["chat"]));
        reg.models
            .insert("embed".into(), mk(0.00001, &["embedding"])); // not chat → skip
        // cheapest chat-capable, excluding the agent's own primary:
        assert_eq!(
            pick_cheap_model(&reg, Some("cheap"), &[]),
            Some("frontier".into())
        ); // cheap excluded
        assert_eq!(pick_cheap_model(&reg, None, &[]), Some("cheap".into()));
        // no chat entries → None (Smart inert)
        let mut empty = ModelRegistry::default();
        empty.models.insert("e".into(), mk(0.0, &["embedding"]));
        assert_eq!(pick_cheap_model(&empty, None, &[]), None);
    }

    #[test]
    fn satisfies_is_permissive_at_baseline_and_fail_closed_above_it() {
        let mk = |caps: &[&str]| ModelEntry {
            provider: "x".into(),
            model: "m".into(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        // Baseline: an entry written before the field existed is still chat.
        assert!(satisfies(&mk(&[]), &[]));
        assert!(satisfies(&mk(&["chat"]), &[]));
        assert!(!satisfies(&mk(&["embedding"]), &[]));
        // Above baseline: unstated is not permission.
        assert!(!satisfies(&mk(&[]), &[Requirement::Vision]));
        assert!(!satisfies(&mk(&["chat"]), &[Requirement::Vision]));
        assert!(satisfies(&mk(&["chat", "vision"]), &[Requirement::Vision]));
        assert!(!satisfies(&mk(&["chat", "vision"]), &[Requirement::Tools]));
        assert!(satisfies(
            &mk(&["chat", "vision", "tools"]),
            &[Requirement::Vision, Requirement::Tools]
        ));
    }

    /// Tools and Vision disagree about silence on purpose. A tool-incapable
    /// model fails loudly and the chain advances; a blind one answers with
    /// confident nonsense. So an entry that declares nothing keeps its place in
    /// the chain for a tool turn — otherwise every pre-`capabilities` entry
    /// (most of a real registry) would drop out of every tool-carrying request
    /// — while the same silence disqualifies it for an image.
    #[test]
    fn undeclared_capabilities_pass_tools_but_never_vision() {
        let mk = |caps: &[&str]| ModelEntry {
            provider: "x".into(),
            model: "m".into(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        // Silence: permitted for tools, never for vision.
        assert!(satisfies(&mk(&[]), &[Requirement::Tools]));
        assert!(!satisfies(&mk(&[]), &[Requirement::Vision]));
        assert!(!satisfies(
            &mk(&[]),
            &[Requirement::Vision, Requirement::Tools]
        ));
        // A declaration is taken at its word in both directions.
        assert!(!satisfies(&mk(&["chat"]), &[Requirement::Tools]));
        assert!(satisfies(&mk(&["chat", "tools"]), &[Requirement::Tools]));
    }

    /// The incident, as a regression test: an image request against a registry
    /// where nothing declares vision must find no cheap candidate at all.
    #[test]
    fn pick_cheap_model_declines_when_no_entry_declares_the_requirement() {
        let mk = |cost: f64, caps: &[&str]| ModelEntry {
            provider: "x".into(),
            model: "m".into(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            cost_per_1k_tokens: Some(cost),
            ..Default::default()
        };
        let mut reg = ModelRegistry::default();
        reg.models
            .insert("cheap_text".into(), mk(0.0001, &["chat"]));
        reg.models.insert("legacy".into(), mk(0.0002, &[]));
        reg.models
            .insert("frontier".into(), mk(0.01, &["chat", "vision"]));
        // No requirement -> cheapest wins (today's behaviour, unchanged).
        assert_eq!(pick_cheap_model(&reg, None, &[]), Some("cheap_text".into()));
        // Vision required -> only the declaring entry qualifies, cost be damned.
        assert_eq!(
            pick_cheap_model(&reg, None, &[Requirement::Vision]),
            Some("frontier".into())
        );
        // Nothing declares vision -> None, so Smart goes inert.
        let mut blind = ModelRegistry::default();
        blind
            .models
            .insert("cheap_text".into(), mk(0.0001, &["chat"]));
        blind.models.insert("legacy".into(), mk(0.0002, &[]));
        assert_eq!(pick_cheap_model(&blind, None, &[Requirement::Vision]), None);
    }

    /// A price with no date is a guess wearing the costume of a fact. But a
    /// date on an entry that carries no price would be the same lie in the
    /// other direction, so the stamp is conditional on there being a rate.
    #[test]
    fn priced_at_stamps_only_priced_entries() {
        let now = chrono::Utc::now();

        let mut unpriced = ModelEntry {
            provider: "openai".into(),
            model: "local-thing".into(),
            ..Default::default()
        };
        unpriced.stamp_priced_at(now);
        assert_eq!(unpriced.priced_at, None);
        assert_eq!(unpriced.price_age(now), None);

        let mut priced = ModelEntry {
            output_cost_per_1k: Some(0.025),
            ..unpriced.clone()
        };
        priced.stamp_priced_at(now);
        assert_eq!(priced.priced_at, Some(now));

        // A legacy single-rate entry counts as priced.
        let mut legacy = ModelEntry {
            cost_per_1k_tokens: Some(0.01),
            ..unpriced.clone()
        };
        legacy.stamp_priced_at(now);
        assert!(legacy.priced_at.is_some());

        // Re-stamping never moves the date backwards.
        let earlier = now - chrono::TimeDelta::days(30);
        priced.stamp_priced_at(earlier);
        assert_eq!(priced.priced_at, Some(now));
    }

    /// Entries written before this field existed must keep loading, and must
    /// report an unknown age rather than inheriting today's date.
    #[test]
    fn registry_without_priced_at_still_loads_and_reports_unknown_age() {
        let yaml = r#"
schema_version: 1
models:
  opus:
    provider: anthropic
    model: claude-opus-5
    input_cost_per_1k: 0.005
    output_cost_per_1k: 0.025
"#;
        let reg: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        let e = &reg.models["opus"];
        assert_eq!(e.priced_at, None);
        assert_eq!(e.price_age(chrono::Utc::now()), None);
        // Round-trips without inventing the field.
        let out = serde_yaml_ng::to_string(&reg).unwrap();
        assert!(!out.contains("priced_at"), "{out}");
    }

    /// `mur model add --input-cost/--output-cost` leaves `cost_per_1k_tokens`
    /// unset, so an entry priced the current way must still be rankable.
    #[test]
    fn pick_cheap_model_sees_split_cost_entries() {
        let mut reg = ModelRegistry::default();
        let split = |input: f64, output: f64| ModelEntry {
            provider: "x".into(),
            model: "m".into(),
            capabilities: vec!["chat".into()],
            input_cost_per_1k: Some(input),
            output_cost_per_1k: Some(output),
            ..Default::default()
        };
        reg.models.insert("dear".into(), split(0.005, 0.025));
        reg.models.insert("cheap".into(), split(0.0001, 0.0004));
        assert_eq!(pick_cheap_model(&reg, None, &[]), Some("cheap".into()));

        // Input-only entries are priced too, rather than silently skipped.
        let mut input_only = ModelRegistry::default();
        input_only.models.insert(
            "in".into(),
            ModelEntry {
                provider: "x".into(),
                model: "m".into(),
                capabilities: vec!["chat".into()],
                input_cost_per_1k: Some(0.002),
                ..Default::default()
            },
        );
        assert_eq!(pick_cheap_model(&input_only, None, &[]), Some("in".into()));
    }
}
