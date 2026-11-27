//! Tests for `mur model doctor` (`model_doctor.rs`).

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
fn remove_finds_only_the_agents_that_reference_the_key() {
    let agents = vec![
        agent("a", Some("k1"), "claude", "m"),
        agent("b", Some("k2"), "claude", "m"),
        agent("c", None, "claude", "m"),
        agent("d", Some("k1"), "claude", "m"),
    ];
    assert_eq!(agents_referencing(&agents, "k1"), vec!["a", "d"]);
    assert!(agents_referencing(&agents, "nope").is_empty());
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
    reg.models.get_mut("secret_claude").unwrap().secret = Some(mur_common::secret::SecretRef::Env(
        "ANTHROPIC_API_KEY".into(),
    ));
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

/// A rate on a subscription or local entry is a contradiction: the
/// budget gates and the murmur footer skip it (the bill is flat or
/// nil), while anything that reads the rate directly still prices it.
/// A zero rate is what `mur model add` writes for local tiers, so it is
/// not flagged; an explicit `billing:` wins over the provider default.
#[test]
fn a_nonzero_rate_on_an_unbilled_entry_is_reported() {
    let mut reg = reg_with(&[
        ("codex_priced", "codex", "gpt-5.5", None),
        ("ollama_priced", "ollama", "qwen3", None),
        ("ollama_zero", "ollama", "qwen3", None),
        ("codex_unpriced", "codex", "gpt-5.5", None),
        ("codex_legacy", "codex", "gpt-5.5", None),
        ("api_priced", "openai", "gpt-5.5", None),
        (
            "labelled_local",
            "openai",
            "qwen3",
            Some("http://127.0.0.1:11434/v1"),
        ),
        ("metered_codex", "codex", "gpt-5.5", None),
    ]);
    let mut rate = |key: &str, i: f64, o: f64| {
        let e = reg.models.get_mut(key).unwrap();
        e.input_cost_per_1k = Some(i);
        e.output_cost_per_1k = Some(o);
    };
    rate("codex_priced", 0.00125, 0.01);
    rate("ollama_priced", 0.0, 0.002);
    rate("ollama_zero", 0.0, 0.0);
    rate("api_priced", 0.00125, 0.01);
    rate("labelled_local", 0.001, 0.002);
    rate("metered_codex", 0.00125, 0.01);
    reg.models.get_mut("labelled_local").unwrap().billing = Some(BillingMode::Local);
    reg.models.get_mut("metered_codex").unwrap().billing = Some(BillingMode::UsageBilled);
    // A legacy single-rate entry is still a rate.
    reg.models
        .get_mut("codex_legacy")
        .unwrap()
        .cost_per_1k_tokens = Some(0.01);

    let findings = audit(&reg, &[], None);
    let mut flagged: Vec<&str> = findings
        .iter()
        .filter(|f| f.level == Level::Warn && f.detail.contains("per-1k rate"))
        .map(|f| f.subject.as_str())
        .collect();
    flagged.sort();
    assert_eq!(
        flagged,
        vec![
            "codex_legacy",
            "codex_priced",
            "labelled_local",
            "ollama_priced"
        ]
    );

    let codex = findings
        .iter()
        .find(|f| f.subject == "codex_priced")
        .unwrap();
    assert!(codex.detail.contains("subscription"), "{}", codex.detail);
    assert!(
        codex.detail.contains("inferred from `provider: codex`"),
        "{}",
        codex.detail
    );
    assert!(
        codex.detail.contains("billing: usage_billed"),
        "{}",
        codex.detail
    );
    let labelled = findings
        .iter()
        .find(|f| f.subject == "labelled_local")
        .unwrap();
    assert!(
        labelled.detail.contains("`billing: local`"),
        "{}",
        labelled.detail
    );
}
