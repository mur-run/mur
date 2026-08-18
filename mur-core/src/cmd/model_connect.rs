//! `mur model connect` / `mur model import` — key-driven bulk provider setup
//! (the CLI counterpart of the Hub Model Library) and cross-machine registry
//! moves. `models.yaml` is commit-safe — secrets travel as refs, never values
//! — so "export" is copying the file, and `import` merges it here and reports
//! which secret refs still need a key on this machine.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::route::RouteTier;
use mur_common::secret::SecretRef;

use crate::model_discovery::{self, default_alias, wire_protocol_for};
use crate::model_prices;

/// Keychain service shared with the Hub Model Library — a key stored by
/// either surface is visible to the other.
const KEYCHAIN_SERVICE: &str = "mur";
/// Endpoint probe timeout, same as the Hub's discover flow.
const DISCOVER_TIMEOUT_SECS: u64 = 10;
/// Local runtime probe timeout, same as the Hub's sidebar probe.
const PROBE_TIMEOUT_SECS: u64 = 3;

// ── pure planning ──

/// Where the model list comes from.
#[derive(Debug, PartialEq)]
pub(crate) enum ListSource {
    /// models.dev catalog (cloud vendors — never probe their endpoint).
    Catalog,
    /// Live `GET {base}/v1/models` (custom endpoints — only they know).
    Live,
}

/// How `connect <vendor>` maps onto registry fields. `provider` is the wire
/// protocol the runtime dials (anthropic|openai); `vendor` keeps the
/// models.dev slug for listing, pricing, and alias prefixes.
#[derive(Debug)]
pub(crate) struct ConnectPlan {
    pub provider: String,
    pub vendor: String,
    pub base_url: Option<String>,
    pub list: ListSource,
}

/// Decide protocol / endpoint / list source for a vendor connect.
///
/// Native protocols (anthropic, openai) list from the catalog and may omit
/// the endpoint (canonical host). Any other catalog vendor is reachable only
/// through its OpenAI-compatible endpoint, so `--base-url` is required and
/// the entry's `provider:` is the `openai` protocol — the same convention
/// existing registries use (DeepSeek, local runtimes). A vendor the catalog
/// has never heard of needs both `--base-url` and live discovery.
pub(crate) fn plan_connect(
    vendor: &str,
    base_url: Option<String>,
    catalog_has_vendor: bool,
) -> Result<ConnectPlan> {
    // "Native" means the runtime has a client for this vendor by name, so the
    // entry can carry the vendor in `provider:` and omit the endpoint.
    let protocol = wire_protocol_for(vendor);
    if protocol == vendor {
        return Ok(ConnectPlan {
            provider: vendor.to_string(),
            vendor: vendor.to_string(),
            base_url,
            list: ListSource::Catalog,
        });
    }
    let Some(base) = base_url else {
        bail!(
            "vendor {vendor:?} is not a native protocol — pass its OpenAI-compatible \
             endpoint with --base-url (the entry is written as provider `openai`)"
        );
    };
    Ok(ConnectPlan {
        provider: protocol.to_string(),
        vendor: vendor.to_string(),
        base_url: Some(base),
        list: if catalog_has_vendor {
            ListSource::Catalog
        } else {
            ListSource::Live
        },
    })
}

/// Parse a selection like `all`, `2`, or `1,3-5` into 0-based indices
/// (deduped, in order). `n` is the list length shown to the user.
pub(crate) fn parse_selection(input: &str, n: usize) -> Result<Vec<usize>> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("all") {
        return Ok((0..n).collect());
    }
    if input.is_empty() {
        bail!("empty selection — enter numbers like `1,3-5`, or `all`");
    }
    let mut out: Vec<usize> = Vec::new();
    for part in input.split(',') {
        let part = part.trim();
        let (lo, hi) = match part.split_once('-') {
            Some((a, b)) => (a.trim().parse::<usize>()?, b.trim().parse::<usize>()?),
            None => {
                let v = part.parse::<usize>()?;
                (v, v)
            }
        };
        if lo == 0 || hi < lo || hi > n {
            bail!("selection {part:?} out of range 1..={n}");
        }
        for i in lo..=hi {
            if !out.contains(&(i - 1)) {
                out.push(i - 1);
            }
        }
    }
    Ok(out)
}

/// Outcome of merging an imported registry into the local one.
#[derive(Debug, Default)]
pub(crate) struct MergeReport {
    pub added: Vec<String>,
    pub skipped: Vec<String>,
    pub roles_added: Vec<String>,
    pub roles_skipped: Vec<String>,
}

/// Merge `incoming` into `local`. Existing aliases/roles are kept unless
/// `force`; nothing is ever deleted.
pub(crate) fn merge_registries(
    local: &mut ModelRegistry,
    incoming: ModelRegistry,
    force: bool,
) -> MergeReport {
    let mut report = MergeReport::default();
    for (alias, entry) in incoming.models {
        if local.models.contains_key(&alias) && !force {
            report.skipped.push(alias);
        } else {
            local.models.insert(alias.clone(), entry);
            report.added.push(alias);
        }
    }
    for (role, entry) in incoming.roles {
        if local.roles.contains_key(&role) && !force {
            report.roles_skipped.push(role);
        } else {
            local.roles.insert(role.clone(), entry);
            report.roles_added.push(role);
        }
    }
    report
}

// ── IO ──

fn read_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).context("read stdin")?;
    Ok(s.trim().to_string())
}

fn mur_home_of(registry_path: &Path) -> std::path::PathBuf {
    registry_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
}

/// Resolve the key to use for a vendor: reuse a stored ref on any existing
/// entry of the same protocol, else prompt (hidden) and store in the shared
/// Keychain service. Returns (ref for new entries, plaintext when freshly
/// prompted — only needed to probe a live endpoint).
async fn resolve_key(
    reg: &ModelRegistry,
    plan: &ConnectPlan,
    no_key: bool,
) -> Result<(Option<SecretRef>, Option<String>)> {
    if no_key {
        return Ok((None, None));
    }
    if let Some(existing) = reg
        .models
        .values()
        .find(|e| e.provider == plan.provider && e.secret.is_some())
        .and_then(|e| e.secret.clone())
    {
        eprintln!("reusing stored key ({existing})");
        return Ok((Some(existing), None));
    }
    eprint!("API key for {} (hidden; empty = none): ", plan.vendor);
    let key = rpassword::read_password().context("read hidden key")?;
    if key.trim().is_empty() {
        return Ok((None, None));
    }
    let account = plan.vendor.clone();
    mur_common::secret::keychain_set(KEYCHAIN_SERVICE, &account, key.trim())
        .await
        .with_context(|| format!("store key in Keychain {KEYCHAIN_SERVICE}/{account}"))?;
    eprintln!("stored in Keychain ({KEYCHAIN_SERVICE}/{account})");
    Ok((
        Some(SecretRef::Keychain {
            service: KEYCHAIN_SERVICE.to_string(),
            account,
        }),
        Some(key.trim().to_string()),
    ))
}

fn pick_from(models: &[String], all: bool, pick: Option<String>) -> Result<Vec<usize>> {
    for (i, m) in models.iter().enumerate() {
        println!("{:>3}. {m}", i + 1);
    }
    if all {
        return Ok((0..models.len()).collect());
    }
    let input = match pick {
        Some(p) => p,
        None => read_line("select models (e.g. 1,3-5 or all): ")?,
    };
    parse_selection(&input, models.len())
}

#[allow(clippy::too_many_arguments)]
fn add_entries(
    reg: &mut ModelRegistry,
    mur_home: &Path,
    alias_prefix: &str,
    vendor: &str,
    provider: &str,
    base_url: Option<&str>,
    secret: Option<&SecretRef>,
    tier: Option<RouteTier>,
    models: &[String],
    picked: &[usize],
) -> (Vec<String>, Vec<String>) {
    let is_local = matches!(tier, Some(RouteTier::Local));
    let (mut added, mut skipped) = (Vec::new(), Vec::new());
    for &i in picked {
        let model_id = &models[i];
        let alias = default_alias(alias_prefix, model_id);
        if reg.models.contains_key(&alias) {
            skipped.push(alias);
            continue;
        }
        let entry = ModelEntry {
            provider: provider.to_string(),
            // Only when it adds information: for anthropic/openai/ollama the
            // protocol already names the vendor.
            vendor: (vendor != provider).then(|| vendor.to_string()),
            model: model_id.clone(),
            base_url: base_url.map(str::to_string),
            secret: secret.cloned(),
            tier,
            ..Default::default()
        };
        // Price from the catalog under the VENDOR slug — the entry's
        // `provider:` is a wire protocol and means nothing to models.dev.
        let fetched = model_prices::lookup(mur_home, vendor, model_id, is_local);
        let mut entry = super::model::apply_fetched_prices(entry, fetched);
        entry.stamp_priced_at(chrono::Utc::now());
        reg.models.insert(alias.clone(), entry);
        added.push(alias);
    }
    (added, skipped)
}

fn summarize(added: &[String], skipped: &[String]) {
    if !added.is_empty() {
        println!("added {}: {}", added.len(), added.join(", "));
    }
    if !skipped.is_empty() {
        println!("skipped (already registered): {}", skipped.join(", "));
    }
    if added.is_empty() && skipped.is_empty() {
        println!("nothing selected");
    }
}

pub async fn cmd_connect(
    vendor: Option<String>,
    base_url: Option<String>,
    name: Option<String>,
    all: bool,
    pick: Option<String>,
    no_key: bool,
) -> Result<()> {
    let path = ModelRegistry::default_path()?;
    let mut reg = if path.exists() {
        ModelRegistry::load_from(&path)?
    } else {
        ModelRegistry::default()
    };
    let mur_home = mur_home_of(&path);

    let Some(vendor) = vendor else {
        // Custom endpoint without a vendor slug: needs --base-url + --name.
        if let Some(base) = base_url {
            let label = name.ok_or_else(|| {
                anyhow::anyhow!("--base-url without a vendor needs --name <alias-prefix>")
            })?;
            let plan = ConnectPlan {
                provider: "openai".into(),
                vendor: label.clone(),
                base_url: Some(base.clone()),
                list: ListSource::Live,
            };
            let (secret, plaintext) = resolve_key(&reg, &plan, no_key).await?;
            let key_for_probe = match (&plaintext, &secret) {
                (Some(k), _) => Some(k.clone()),
                (None, Some(s)) => s.resolve_to_string().await,
                _ => None,
            };
            let models = model_discovery::discover_models(
                &base,
                key_for_probe.as_deref(),
                DISCOVER_TIMEOUT_SECS,
            )?;
            if models.is_empty() {
                bail!("{base} answered with zero models");
            }
            let picked = pick_from(&models, all, pick)?;
            let (added, skipped) = add_entries(
                &mut reg,
                &mur_home,
                &label,
                &label,
                "openai",
                Some(base.as_str()),
                secret.as_ref(),
                None,
                &models,
                &picked,
            );
            reg.save_to(&path)?;
            summarize(&added, &skipped);
            return Ok(());
        }
        // Bare `connect`: probe local runtimes.
        println!("probing local runtimes (Ollama / MLX / LM Studio)…");
        let detected = model_discovery::probe_local(PROBE_TIMEOUT_SECS);
        if detected.is_empty() {
            println!("no local runtimes answered");
            return Ok(());
        }
        for d in detected {
            println!("\n{} ({}):", d.name, d.base_url);
            if d.models.is_empty() {
                println!("  (reachable, zero models)");
                continue;
            }
            let picked = pick_from(&d.models, all, pick.clone())?;
            // Ollama has a native client; other local runtimes speak the
            // openai protocol. Local tier prices as zero.
            let provider = wire_protocol_for(&d.key);
            let (added, skipped) = add_entries(
                &mut reg,
                &mur_home,
                &d.key,
                &d.key,
                provider,
                Some(d.base_url.as_str()),
                None,
                Some(RouteTier::Local),
                &d.models,
                &picked,
            );
            summarize(&added, &skipped);
        }
        reg.save_to(&path)?;
        return Ok(());
    };

    // Vendor flow.
    let catalog = model_prices::load_or_fetch(&mur_home);
    let catalog_models = catalog.as_ref().and_then(|c| c.provider_models(&vendor));
    let plan = plan_connect(&vendor, base_url, catalog_models.is_some())?;
    let (secret, plaintext) = resolve_key(&reg, &plan, no_key).await?;

    let models = match plan.list {
        ListSource::Catalog => catalog_models.filter(|v| !v.is_empty()).ok_or_else(|| {
            anyhow::anyhow!(
                "models.dev catalog unavailable or empty for {vendor:?} — check the network \
                 or run `mur model prices refresh`, or pass --base-url to probe the endpoint"
            )
        })?,
        ListSource::Live => {
            let base = plan.base_url.clone().expect("Live always has a base URL");
            let key_for_probe = match (&plaintext, &secret) {
                (Some(k), _) => Some(k.clone()),
                (None, Some(s)) => s.resolve_to_string().await,
                _ => None,
            };
            model_discovery::discover_models(
                &base,
                key_for_probe.as_deref(),
                DISCOVER_TIMEOUT_SECS,
            )?
        }
    };
    if models.is_empty() {
        bail!("no models found for {vendor}");
    }
    let picked = pick_from(&models, all, pick)?;
    let prefix = name.unwrap_or_else(|| plan.vendor.clone());
    let (added, skipped) = add_entries(
        &mut reg,
        &mur_home,
        &prefix,
        &plan.vendor,
        &plan.provider,
        plan.base_url.as_deref(),
        secret.as_ref(),
        None,
        &models,
        &picked,
    );
    reg.save_to(&path)?;
    summarize(&added, &skipped);
    Ok(())
}

pub async fn cmd_import(file: &Path, force: bool) -> Result<()> {
    let incoming = ModelRegistry::load_from(file)
        .with_context(|| format!("parse {} as models.yaml", file.display()))?;
    let path = ModelRegistry::default_path()?;
    let mut local = if path.exists() {
        ModelRegistry::load_from(&path)?
    } else {
        ModelRegistry::default()
    };

    let report = merge_registries(&mut local, incoming, force);
    local.save_to(&path)?;

    println!(
        "models: {} added, {} skipped · roles: {} added, {} skipped",
        report.added.len(),
        report.skipped.len(),
        report.roles_added.len(),
        report.roles_skipped.len(),
    );
    if !report.skipped.is_empty() {
        println!(
            "skipped (exists; --force overwrites): {}",
            report.skipped.join(", ")
        );
    }

    // Secrets travel as refs, never values — audit which refs resolve HERE.
    // Count what was actually checked: with nothing added there is nothing to
    // verify, and claiming "all resolve" would be a status line that lies.
    let mut checked = 0usize;
    let mut unresolved = Vec::new();
    for alias in &report.added {
        if let Some(s) = local.models.get(alias).and_then(|e| e.secret.as_ref()) {
            checked += 1;
            if !s.check().await {
                unresolved.push(format!("{alias} ({s})"));
            }
        }
    }
    if unresolved.is_empty() {
        if checked > 0 {
            println!("all {checked} imported secret refs resolve on this machine");
        }
    } else {
        println!("\nsecret refs that do NOT resolve here — re-key this machine:");
        for u in &unresolved {
            println!("  {u}");
        }
        println!("(store keys with `mur model connect <vendor>`, or recreate the ref's env/file)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_native_vendors_use_catalog_without_base() {
        let p = plan_connect("anthropic", None, true).unwrap();
        assert_eq!(p.provider, "anthropic");
        assert_eq!(p.list, ListSource::Catalog);
        assert_eq!(p.base_url, None);
    }

    #[test]
    fn plan_catalog_vendor_requires_base_and_maps_to_openai_protocol() {
        assert!(plan_connect("deepseek", None, true).is_err());
        let p = plan_connect("deepseek", Some("https://api.deepseek.com".into()), true).unwrap();
        assert_eq!(p.provider, "openai");
        assert_eq!(p.vendor, "deepseek");
        assert_eq!(p.list, ListSource::Catalog);
    }

    #[test]
    fn plan_unknown_vendor_probes_live() {
        let p = plan_connect("myproxy", Some("http://127.0.0.1:9999".into()), false).unwrap();
        assert_eq!(p.list, ListSource::Live);
        assert_eq!(p.provider, "openai");
    }

    #[test]
    fn selection_parses_all_singles_and_ranges() {
        assert_eq!(parse_selection("all", 4).unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(parse_selection("2", 4).unwrap(), vec![1]);
        assert_eq!(parse_selection("1,3-4", 4).unwrap(), vec![0, 2, 3]);
        assert_eq!(parse_selection("3-4, 1", 4).unwrap(), vec![2, 3, 0]);
        assert!(parse_selection("0", 4).is_err());
        assert!(parse_selection("5", 4).is_err());
        assert!(parse_selection("", 4).is_err());
        assert!(parse_selection("4-2", 4).is_err());
    }

    #[test]
    fn merge_skips_existing_unless_forced_and_never_deletes() {
        let mut local = ModelRegistry::default();
        local.models.insert(
            "keep".into(),
            ModelEntry {
                model: "local-version".into(),
                ..Default::default()
            },
        );
        let mut incoming = ModelRegistry::default();
        incoming.models.insert(
            "keep".into(),
            ModelEntry {
                model: "imported-version".into(),
                ..Default::default()
            },
        );
        incoming.models.insert("new".into(), ModelEntry::default());

        let r = merge_registries(&mut local, incoming.clone(), false);
        assert_eq!(r.added, vec!["new"]);
        assert_eq!(r.skipped, vec!["keep"]);
        assert_eq!(local.models["keep"].model, "local-version");

        let r = merge_registries(&mut local, incoming, true);
        assert!(r.skipped.is_empty());
        assert_eq!(local.models["keep"].model, "imported-version");
        assert_eq!(local.models.len(), 2, "merge never deletes");
    }
}
