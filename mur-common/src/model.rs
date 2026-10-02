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

/// Who pays when this model answers.
///
/// A ChatGPT-subscription model (`provider: codex`) and an OpenAI Platform
/// model can share a model id and a wire format while landing on different
/// bills, so the registry says which. `None` on entries written before this
/// field existed — readers render that as unknown, never as free.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BillingMode {
    /// Covered by a flat subscription (ChatGPT Plus/Pro via Codex).
    Subscription,
    /// Metered per token against an API key.
    UsageBilled,
    /// Runs on this machine; no bill.
    Local,
}

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
    /// Output-token ceiling sent on every request to this model that does not
    /// set its own.
    ///
    /// On models with extended thinking this budget covers thinking AND the
    /// visible reply together, so a high `effort` can spend all of it before
    /// any text is produced. `None` leaves the provider client's own default
    /// in place (the Anthropic client's built-in constant; nothing at all for
    /// OpenAI-protocol clients, which then get the server's default). A
    /// request that names its own `max_tokens` keeps it — this is a default,
    /// not a clamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// When the rates above were recorded.
    ///
    /// Vendors move prices; a rate written months ago is a guess wearing the
    /// costume of a fact, and nothing else on this struct can tell the two
    /// apart. `None` means unknown — entries predating this field, or hand-
    /// written ones — which is honest rather than defaulting to "fresh".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priced_at: Option<chrono::DateTime<chrono::Utc>>,
    /// See [`BillingMode`]. `None` = unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing: Option<BillingMode>,
    /// Whether the model id came from the provider's live catalog
    /// (`Some(true)`) or was typed by hand when discovery failed
    /// (`Some(false)`). `None` on entries that predate the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_verified: Option<bool>,
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

const LOCAL_MODEL_PROVIDERS: &[&str] = &[
    "ollama",
    "mlx",
    "llamacpp",
    "llama_cpp",
    "localai",
    "lmstudio",
    "local",
];

/// Whether a provider is a known in-process or on-machine model backend.
pub fn provider_is_local(provider: &str) -> bool {
    LOCAL_MODEL_PROVIDERS.contains(&provider.to_ascii_lowercase().as_str())
}

/// Canonical billing inference for registry entries without explicit metadata.
pub fn inferred_billing_for_provider(provider: &str) -> BillingMode {
    if provider_is_local(provider) {
        BillingMode::Local
    } else if matches!(provider.to_ascii_lowercase().as_str(), "codex" | "claude") {
        BillingMode::Subscription
    } else {
        BillingMode::UsageBilled
    }
}

impl ModelEntry {
    /// How this model is paid for, for the cost gates. An explicit `billing:`
    /// is the answer; without one the provider decides what it can:
    /// `ollama` runs on this machine, `codex` and `claude` ride a flat
    /// subscription. Everything else — including a loopback `base_url`, which
    /// is just as often the model gateway fronting a metered API — is treated
    /// as metered. Guessing "free" is the one mistake a cost gate must not
    /// make; a wrong "metered" costs the user one line in `models.yaml`
    /// (`billing: local`), and the gate says so when it applies.
    pub fn billing_or_inferred(&self) -> BillingMode {
        self.billing
            .unwrap_or_else(|| inferred_billing_for_provider(&self.provider))
    }

    /// Effective route tier, honoring an explicit registry tier first.
    pub fn effective_route_tier(&self) -> RouteTier {
        self.tier.unwrap_or_else(|| {
            if provider_is_local(&self.provider) {
                RouteTier::Local
            } else {
                RouteTier::Frontier
            }
        })
    }
    /// Resolve effective per-1k rates as `(input, output)`.
    ///
    /// The deprecated `cost_per_1k_tokens` is treated as the output rate and
    /// also as the input fallback, so legacy single-rate entries keep working.
    pub fn effective_costs(&self) -> (Option<f64>, Option<f64>) {
        let output = self.output_cost_per_1k.or(self.cost_per_1k_tokens);
        let input = self.input_cost_per_1k.or(self.cost_per_1k_tokens);
        (input, output)
    }

    /// Projected USD cost for a token workload. A known rate on only one side
    /// is used for the other side; no rates remains unknown rather than free.
    pub fn projected_cost(&self, input_tokens: u64, output_tokens: u64) -> Option<f64> {
        let (input, output) = self.effective_costs();
        let fallback = input.or(output)?;
        let input = input.unwrap_or(fallback);
        let output = output.unwrap_or(fallback);
        Some(input_tokens as f64 / 1_000.0 * input + output_tokens as f64 / 1_000.0 * output)
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
        .filter_map(|(k, e)| e.projected_cost(4_000, 1_000).map(|c| (c, k.clone())))
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, k)| k)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod io_tests;

#[cfg(test)]
mod switch_tests;
