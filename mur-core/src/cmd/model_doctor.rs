//! `mur model doctor` — offline, read-only consistency check across the model
//! registry and every installed agent's profile.
//!
//! Providers rename and retire model ids continuously. MUR already absorbs that
//! well: agents point at a `model_ref` (a stable alias), so a rename is one edit
//! in `~/.mur/models.yaml` and every agent pointing at that key moves with it.
//! This command reports where that indirection has come apart. It never
//! rewrites a model id: which model an agent runs is a cost and behaviour
//! decision the operator made, and a silent auto-upgrade would change spend and
//! output quality behind their back.
//!
//! ## What the catalog check is, and is not
//!
//! It is NOT a deprecation check. models.dev keeps historical entries — at the
//! time of writing it lists `claude-sonnet-4-6` and `claude-sonnet-5` side by
//! side — so an id staying in the catalog says nothing about whether the
//! provider still serves it. Answering that needs a live `/v1/models` call
//! against each endpoint, which this command deliberately does not make.
//!
//! What it does catch is an id the catalog has never carried under any vendor:
//! a typo, a copied-wrong id, or a `provider:` field used as a vendor name when
//! it is really a protocol dialect. That last one is common — `deepseek` is
//! reached over the OpenAI wire protocol, so its registry entry reads
//! `provider: openai` while the catalog files it under vendor `deepseek`. New
//! entries record the vendor explicitly; for older ones it is inferred from
//! `base_url` before falling back to
//! `provider`, or every openai-compatible third party would be flagged.

use std::path::Path;

use anyhow::{Context, Result};
use mur_common::model::ModelRegistry;

/// How stale the cached catalog may be and still be worth consulting. Generous
/// on purpose: this command must work offline, and a month-old catalog still
/// answers "has this id ever existed" correctly for anything but the newest
/// releases.
const CATALOG_TTL_HOURS: u64 = 24 * 30;

fn host_of(base_url: Option<&str>) -> Option<String> {
    let u = base_url?;
    let host = u
        .split("//")
        .nth(1)
        .unwrap_or(u)
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    (!host.is_empty()).then(|| host.to_string())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub subject: String,
    pub detail: String,
}

impl Finding {
    fn error(subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            subject: subject.into(),
            detail: detail.into(),
        }
    }
    fn warn(subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: Level::Warn,
            subject: subject.into(),
            detail: detail.into(),
        }
    }
}

/// A model served from the local machine is never in a public catalog, so
/// "unknown to models.dev" says nothing about it. Judged by the endpoint rather
/// than the provider string, because local runtimes advertise themselves as
/// `openai`-compatible and would otherwise all be flagged.
fn is_local_endpoint(base_url: Option<&str>) -> bool {
    let Some(host) = host_of(base_url) else {
        return false;
    };
    matches!(
        host.as_str(),
        "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "::1"
    )
}

/// One agent's model facts, as read off disk.
pub struct AgentModel {
    pub name: String,
    pub model_ref: Option<String>,
    /// The legacy `model:` block: (provider, name).
    pub block: (String, String),
}

/// Answers "does the public catalog list this (vendor, model)". Borrowed rather
/// than owned so the caller keeps the parsed catalog.
pub type CatalogKnows<'a> = &'a dyn Fn(&str, &str) -> bool;

/// Pure decision core — no I/O, so the whole rule set is testable.
///
/// `catalog_knows` answers "does the public catalog list this (provider, model)";
/// `None` means no catalog was available and the catalog-backed check is skipped
/// rather than guessed at.
pub fn audit(
    reg: &ModelRegistry,
    agents: &[AgentModel],
    catalog_knows: Option<CatalogKnows<'_>>,
) -> Vec<Finding> {
    let mut out = Vec::new();

    // 0. Secrets that live on disk as plaintext.
    //
    //    Reported, never blocking (design decision, 2026-08-18): `file:` is the
    //    only backend that works on a headless Linux box with no Secret
    //    Service daemon, it is what `mur model import` carries between
    //    machines, and containers materialise secrets as files on purpose. A
    //    check that failed those would be a gate for something the user cannot
    //    fix — and this repo already learned where that ends, in eval.yml: a
    //    gate that fires for things you cannot fix is a gate that gets switched
    //    off.
    //
    //    So the defect this addresses is not that `file:` exists. It is that
    //    nothing said a plaintext ref was plaintext, which made it look like a
    //    choice rather than a default nobody revisited.
    for (key, e) in &reg.models {
        if let Some(mur_common::secret::SecretRef::File(path)) = &e.secret {
            // Only name a `connect` target when the candidate actually looks
            // like a vendor. `vendor_candidates` falls back to the endpoint
            // host, so an entry pointed at a local gateway yields "127" and
            // the suggestion becomes `mur model connect 127` — a command that
            // does nothing, printed with the authority of advice. Caught by
            // running this against a real registry rather than a fixture.
            let vendor = e
                .vendor_candidates()
                .into_iter()
                .find(|v| v.chars().all(|c| c.is_ascii_alphabetic() || c == '-'));
            // The recommendation carries a precondition, so it names it. A
            // keychain ref resolves through a session service — macOS Keychain,
            // Windows Credential Manager, Linux Secret Service — and a headless
            // box has none, which is why `file:` is the only backend there.
            // Advice that cannot be followed where it is printed is how a
            // warn-only check earns being ignored, and this one is warn-only
            // precisely so it stays readable.
            let fix = match vendor {
                Some(v) => format!(
                    "`mur model connect {v}`, or set `secret: keychain:<service>/<account>` \
                     — keychain needs a session credential service (macOS Keychain, Windows \
                     Credential Manager, Linux Secret Service), so on a headless box `file:` \
                     with 0600 permissions is the available answer, not a worse one"
                ),
                None => "set `secret: keychain:<service>/<account>` — keychain needs a session \
                     credential service, so on a headless box `file:` with 0600 permissions is \
                     the available answer, not a worse one"
                    .to_string(),
            };
            out.push(Finding::warn(
                key.clone(),
                format!(
                    "secret is plaintext on disk at {} — readable by anything that can read the path. \
                     Move it to the keychain: {fix}",
                    path.display(),
                ),
            ));
        }
    }

    // 0b. Subscription entries and the loopback contract.
    //
    //    `provider: anthropic` pointed at the local gateway *works* — the
    //    gateway attaches the Claude Code token to an authless request — but
    //    nothing stops a later `base_url` edit from landing the same entry on
    //    API billing. `provider: claude` is the same route with that edit
    //    refused at startup. Warn-only: the entry is not broken, it is
    //    unlabelled. The claude checks name what the runtime will refuse, so
    //    the user learns it here rather than from a failed agent start.
    for (key, e) in &reg.models {
        let loopback = is_local_endpoint(e.base_url.as_deref());
        match e.provider.as_str() {
            "anthropic" if loopback && e.secret.is_none() => out.push(Finding::warn(
                key.clone(),
                "rides a Claude subscription through the local gateway. `provider: claude` says so \
                 explicitly, labels it `billing: subscription`, and refuses a remote host or a \
                 secret — see docs/model-gateway.md",
            )),
            "claude" => {
                let path_ok = e
                    .base_url
                    .as_deref()
                    .and_then(|b| reqwest::Url::parse(b).ok())
                    .is_some_and(|u| u.path().trim_end_matches('/') == "/v1");
                if !loopback || !path_ok {
                    out.push(Finding::warn(
                        key.clone(),
                        "`provider: claude` must point at the loopback gateway \
                         `http://127.0.0.1:<port>/v1`; the runtime refuses to start this entry",
                    ));
                }
                if e.secret.is_some() {
                    out.push(Finding::warn(
                        key.clone(),
                        "`provider: claude` takes no `secret` — the gateway holds the Claude Code \
                         login; the runtime refuses to start this entry",
                    ));
                }
            }
            _ => {}
        }
    }

    // 1. Registry entries whose model id the catalog has never carried under
    //    any vendor we can name for them. See the module doc for what this does
    //    and does not prove.
    if let Some(knows) = catalog_knows {
        for (key, e) in &reg.models {
            if is_local_endpoint(e.base_url.as_deref()) {
                continue;
            }
            let vendors = e.vendor_candidates();
            if vendors.iter().any(|v| knows(v, &e.model)) {
                continue;
            }
            out.push(Finding::warn(
                format!("models.yaml/{key}"),
                format!(
                    "{} is not in the price catalog under {} — check the id for a \
                     typo, or that `provider:` names the right vendor. Not a \
                     deprecation check: the catalog keeps retired ids. If the id \
                     really moved, edit this one entry and every agent pointing at \
                     '{key}' follows.",
                    e.model,
                    vendors.join(" or ")
                ),
            ));
        }
    }

    for a in agents {
        let Some(key) = a.model_ref.as_deref() else {
            // No ref: the legacy block IS the source of truth here, which is
            // supported. Nothing to cross-check.
            continue;
        };
        // 2. A ref that resolves to nothing. Fatal at dial time.
        let Some(e) = reg.models.get(key) else {
            out.push(Finding::error(
                format!("agent/{}", a.name),
                format!("model_ref '{key}' is not in models.yaml — this agent cannot dial"),
            ));
            continue;
        };
        // 3. The legacy block disagreeing with the ref. Harmless to the runtime,
        //    which resolves the ref — but `mur agent companion preview` and
        //    anyone reading profile.yaml believe the block.
        if (a.block.0.as_str(), a.block.1.as_str()) != (e.provider.as_str(), e.model.as_str()) {
            out.push(Finding::warn(
                format!("agent/{}", a.name),
                format!(
                    "profile.yaml `model:` says {}/{} but model_ref '{key}' resolves to \
                     {}/{} — the ref is what runs. Any `mur agent` edit re-syncs the block.",
                    a.block.0, a.block.1, e.provider, e.model
                ),
            ));
        }
    }

    out.sort_by(|x, y| x.subject.cmp(&y.subject));
    out
}

/// Read every `<mur_home>/agents/*/profile.yaml`. Unreadable or unparseable
/// profiles are skipped rather than fatal: a doctor that dies on one bad file
/// tells you nothing about the other twenty-six.
pub fn read_agents(mur_home: &Path) -> Vec<AgentModel> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(mur_home.join("agents")) else {
        return out;
    };
    let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs {
        let Ok(yaml) = std::fs::read_to_string(dir.join("profile.yaml")) else {
            continue;
        };
        let Ok(p) = serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml) else {
            continue;
        };
        let Some(name) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push(AgentModel {
            name: name.to_string(),
            model_ref: p.model_ref.clone(),
            block: (p.model.provider.clone(), p.model.name.clone()),
        });
    }
    out
}

pub fn cmd_model_doctor() -> Result<()> {
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let reg_path = mur_home.join("models.yaml");
    let reg = ModelRegistry::load_from(&reg_path)
        .with_context(|| format!("load {}", reg_path.display()))?;
    let agents = read_agents(&mur_home);

    let catalog = crate::model_prices::load_cached(&mur_home, CATALOG_TTL_HOURS);
    if catalog.is_none() {
        println!(
            "note: no fresh price catalog cached — skipping the \
             renamed/retired check. Run `mur model prices refresh` (needs network)."
        );
    }
    let knows = catalog
        .as_ref()
        .map(|c| move |p: &str, m: &str| c.knows(p, m));
    let findings = audit(&reg, &agents, knows.as_ref().map(|f| f as CatalogKnows<'_>));

    if findings.is_empty() {
        println!(
            "ok — {} registry entries, {} agents, nothing inconsistent",
            reg.models.len(),
            agents.len()
        );
        return Ok(());
    }
    for f in &findings {
        let tag = match f.level {
            Level::Error => "error",
            Level::Warn => "warn ",
        };
        println!("{tag} {}: {}", f.subject, f.detail);
    }
    let errors = findings.iter().filter(|f| f.level == Level::Error).count();
    println!(
        "\n{} finding(s): {errors} error, {} warning — nothing was changed",
        findings.len(),
        findings.len() - errors
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::model::ModelEntry;

    fn reg_with(entries: &[(&str, &str, &str, Option<&str>)]) -> ModelRegistry {
        let mut reg = ModelRegistry::default();
        for (key, provider, model, base) in entries {
            reg.models.insert(
                (*key).to_string(),
                ModelEntry {
                    provider: (*provider).to_string(),
                    model: (*model).to_string(),
                    base_url: base.map(str::to_string),
                    ..Default::default()
                },
            );
        }
        reg
    }

    /// A `file:` ref is plaintext on disk. Nothing said so before, which made
    /// six of them on a real machine look like a choice rather than a default
    /// nobody revisited.
    /// The fix carries a precondition and must say so. `keychain:` resolves
    /// through a session credential service, and a headless box has none — so
    /// on the machines where `file:` is not a lazy default but the only option,
    /// advice that says "move it to the keychain" cannot be followed. A
    /// warn-only check that prints unfollowable advice is one people switch off.
    #[test]
    fn the_keychain_fix_names_what_it_needs_to_work() {
        let mut reg = reg_with(&[("anth", "anthropic", "claude-sonnet-5", None)]);
        reg.models.get_mut("anth").unwrap().secret = Some(mur_common::secret::SecretRef::File(
            "/x/anthropic.key".into(),
        ));
        let f = audit(&reg, &[], None);
        let hit = f.iter().find(|f| f.subject == "anth").unwrap();
        assert!(
            hit.detail.contains("session credential service"),
            "the precondition must be named: {}",
            hit.detail
        );
        assert!(
            hit.detail.contains("headless"),
            "the case where it cannot be followed must be named: {}",
            hit.detail
        );
        // And `file:` must not be left reading as carelessness where it is the
        // only thing that works.
        assert!(hit.detail.contains("not a worse one"), "{}", hit.detail);
    }

    #[test]
    fn a_plaintext_secret_is_named_with_the_path_and_the_fix() {
        let mut reg = reg_with(&[("anth", "anthropic", "claude-sonnet-5", None)]);
        reg.models.get_mut("anth").unwrap().secret = Some(mur_common::secret::SecretRef::File(
            "/Users/x/.mur/secrets/anthropic.key".into(),
        ));

        let f = audit(&reg, &[], None);
        let hit = f
            .iter()
            .find(|f| f.subject == "anth")
            .expect("no finding for the plaintext secret");
        assert_eq!(
            hit.level,
            Level::Warn,
            "must warn, never error — see the comment in audit"
        );
        assert!(
            hit.detail.contains("/Users/x/.mur/secrets/anthropic.key"),
            "{}",
            hit.detail
        );
        assert!(
            hit.detail.contains("mur model connect"),
            "no fix offered: {}",
            hit.detail
        );
    }

    /// Keychain and env refs are not on disk and must not be flagged —
    /// otherwise the warning appears on a correctly-configured machine and
    /// stops meaning anything.
    #[test]
    fn a_keychain_or_env_secret_is_not_flagged() {
        let mut reg = reg_with(&[
            ("kc", "anthropic", "claude-sonnet-5", None),
            ("ev", "anthropic", "claude-sonnet-5", None),
        ]);
        reg.models.get_mut("kc").unwrap().secret = Some(mur_common::secret::SecretRef::Keychain {
            service: "mur".into(),
            account: "anthropic".into(),
        });
        reg.models.get_mut("ev").unwrap().secret = Some(mur_common::secret::SecretRef::Env(
            "ANTHROPIC_API_KEY".into(),
        ));

        let f = audit(&reg, &[], None);
        assert!(
            !f.iter().any(|f| f.detail.contains("plaintext")),
            "flagged a secret that is not on disk: {f:?}"
        );
    }

    fn agent(name: &str, r: Option<&str>, provider: &str, model: &str) -> AgentModel {
        AgentModel {
            name: name.to_string(),
            model_ref: r.map(str::to_string),
            block: (provider.to_string(), model.to_string()),
        }
    }

    /// An id no catalog vendor has ever carried: a typo, or a copied-wrong id.
    #[test]
    fn an_id_the_catalog_never_carried_is_reported() {
        let reg = reg_with(&[("typo", "anthropic", "claude-sonnett-5", None)]);
        let knows = |_p: &str, m: &str| m == "claude-sonnet-5";
        let f = audit(&reg, &[], Some(&knows));
        assert_eq!(f.len(), 1, "{f:?}");
        // It must not claim to have detected a deprecation — the catalog keeps
        // retired ids, so it cannot know that.
        assert!(!f[0].detail.contains("retired it"), "{:?}", f[0]);
        assert!(f[0].detail.contains("typo"), "{:?}", f[0]);
        // And it points at the one edit that fixes every agent at once.
        assert!(
            f[0].detail.contains("every agent pointing at"),
            "{:?}",
            f[0]
        );
    }

    /// Caught by running the command for real against a live `~/.mur`: both
    /// DeepSeek entries were flagged as unknown. They were not — the registry's
    /// `provider: openai` is the *wire protocol*, while the catalog files them
    /// under vendor `deepseek`. Looking up only `provider` would flag every
    /// openai-compatible third party and train the operator to ignore this
    /// command entirely.
    #[test]
    fn an_openai_compatible_vendor_is_matched_by_its_endpoint_not_its_dialect() {
        let reg = reg_with(&[(
            "deepseek_v4_flash",
            "openai",
            "deepseek-v4-flash",
            Some("https://api.deepseek.com"),
        )]);
        // The real catalog shape: filed under `deepseek`, absent from `openai`.
        let knows = |v: &str, m: &str| v == "deepseek" && m == "deepseek-v4-flash";
        assert!(audit(&reg, &[], Some(&knows)).is_empty());
    }

    /// A local runtime is in no public catalog, so "unknown" is the normal case
    /// and flagging it would train the operator to ignore this command.
    #[test]
    fn a_local_endpoint_is_never_flagged_as_unknown() {
        let reg = reg_with(&[(
            "omlx",
            "openai",
            "Qwen3.5-4B-MLX-4bit",
            Some("http://127.0.0.1:8000/v1"),
        )]);
        let knows = |_p: &str, _m: &str| false;
        assert!(audit(&reg, &[], Some(&knows)).is_empty());
    }

    /// Without a catalog the check is skipped, not guessed. Reporting every
    /// entry as unknown when the cache is cold would be worse than silence.
    #[test]
    fn no_catalog_means_no_catalog_findings() {
        let reg = reg_with(&[("claude_sonnet", "anthropic", "claude-sonnet-4-6", None)]);
        assert!(audit(&reg, &[], None).is_empty());
    }

    #[test]
    fn a_dangling_ref_is_an_error_and_a_drifted_block_is_a_warning() {
        let reg = reg_with(&[("omlx", "openai", "Qwen3.5-4B-MLX-4bit", None)]);
        let agents = vec![
            agent("ghost", Some("never_registered"), "anthropic", "x"),
            agent(
                "repomanager",
                Some("omlx"),
                "anthropic",
                "claude-sonnet-4-6",
            ),
            agent("aligned", Some("omlx"), "openai", "Qwen3.5-4B-MLX-4bit"),
            agent("legacy", None, "ollama", "qwen3:4b"),
        ];
        let f = audit(&reg, &agents, None);
        assert_eq!(f.len(), 2, "{f:?}");

        let ghost = f.iter().find(|x| x.subject.contains("ghost")).unwrap();
        assert_eq!(ghost.level, Level::Error);
        assert!(ghost.detail.contains("cannot dial"));

        let repo = f
            .iter()
            .find(|x| x.subject.contains("repomanager"))
            .unwrap();
        assert_eq!(repo.level, Level::Warn);
        assert!(repo.detail.contains("the ref is what runs"), "{repo:?}");

        // An agent whose block already agrees, and one that never had a ref,
        // are both fine — the legacy block is a supported source of truth when
        // there is no ref to contradict it.
        assert!(!f.iter().any(|x| x.subject.contains("aligned")));
        assert!(!f.iter().any(|x| x.subject.contains("legacy")));
    }

    #[test]
    fn local_endpoint_detection_reads_the_host_not_the_scheme() {
        assert!(is_local_endpoint(Some("http://127.0.0.1:8000/v1")));
        assert!(is_local_endpoint(Some("http://localhost:11434")));
        assert!(!is_local_endpoint(Some("https://api.deepseek.com")));
        // A remote host that merely mentions localhost must not pass.
        assert!(!is_local_endpoint(Some("https://localhost.evil.com/v1")));
        assert!(!is_local_endpoint(None));
    }

    /// `provider: anthropic` at the gateway works but is unlabelled — one
    /// `base_url` edit moves it to API billing. `provider: claude` is the
    /// same route with that edit refused, so doctor points at the entries
    /// that could carry the label, and at claude entries the runtime will
    /// reject before an agent start does it for them.
    #[test]
    fn subscription_entries_are_checked_against_the_loopback_contract() {
        let mut reg = reg_with(&[
            (
                "gw_anthropic",
                "anthropic",
                "claude-opus-5",
                Some("http://127.0.0.1:8088"),
            ),
            ("api_anthropic", "anthropic", "claude-opus-5", None),
            (
                "good_claude",
                "claude",
                "claude-opus-5",
                Some("http://127.0.0.1:8088/v1"),
            ),
            (
                "remote_claude",
                "claude",
                "claude-opus-5",
                Some("https://api.anthropic.com/v1"),
            ),
            (
                "wrong_path_claude",
                "claude",
                "claude-opus-5",
                Some("http://127.0.0.1:8088"),
            ),
            (
                "secret_claude",
                "claude",
                "claude-opus-5",
                Some("http://127.0.0.1:8088/v1"),
            ),
        ]);
        reg.models.get_mut("secret_claude").unwrap().secret = Some(
            mur_common::secret::SecretRef::Env("ANTHROPIC_API_KEY".into()),
        );
        let findings = audit(&reg, &[], None);
        let subjects = |needle: &str| -> Vec<String> {
            findings
                .iter()
                .filter(|f| f.level == Level::Warn && f.detail.contains(needle))
                .map(|f| f.subject.clone())
                .collect()
        };
        assert_eq!(subjects("provider: claude` says so"), vec!["gw_anthropic"]);
        let mut refused = subjects("refuses to start");
        refused.sort();
        refused.dedup();
        assert_eq!(
            refused,
            vec!["remote_claude", "secret_claude", "wrong_path_claude"]
        );
        assert!(
            !findings
                .iter()
                .any(|f| f.subject == "good_claude" || f.subject == "api_anthropic")
        );
    }
}
