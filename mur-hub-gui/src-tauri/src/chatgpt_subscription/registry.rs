//! Registry commands for the ChatGPT Subscription provider (`provider: codex`).
//!
//! Deliberately not routed through `add_models`: that path builds a
//! SecretRef and maps a UI vendor onto a wire protocol, and a subscription
//! entry has neither — no secret, and `codex` *is* the protocol.

use super::CHATGPT_GATEWAY_BASE;
use mur_common::model::{BillingMode, ModelEntry, ModelRegistry};
use mur_common::route::RouteTier;
use serde::Deserialize;

const MAX_ALIAS_LEN: usize = 64;

#[derive(Deserialize)]
pub struct ChatGptModelPick {
    pub model: String,
    pub alias: String,
    /// `true` when the id came from `model/list`; `false` for a hand-typed
    /// id entered because discovery failed.
    pub verified: bool,
}

/// A registry alias is a YAML key and (via `mur agent …`) a CLI argument:
/// what `default_alias` emits plus `.` and `-`, nothing path- or shell-like.
fn validate_alias(alias: &str) -> Result<(), String> {
    if alias.is_empty() {
        return Err("alias must not be empty".into());
    }
    if alias.len() > MAX_ALIAS_LEN {
        return Err(format!("alias longer than {MAX_ALIAS_LEN} characters"));
    }
    if alias.starts_with('.')
        || !alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(format!(
            "alias {alias:?}: use letters, digits, '_', '-' or '.', not starting with '.'"
        ));
    }
    Ok(())
}

pub fn subscription_entry(provider: &str, base_url: &str, pick: &ChatGptModelPick) -> ModelEntry {
    ModelEntry {
        provider: provider.into(),
        model: pick.model.clone(),
        base_url: Some(base_url.into()),
        secret: None,
        tier: Some(RouteTier::Frontier),
        billing: Some(BillingMode::Subscription),
        catalog_verified: Some(pick.verified),
        ..Default::default()
    }
}

pub fn chatgpt_entry(pick: &ChatGptModelPick) -> ModelEntry {
    subscription_entry("codex", CHATGPT_GATEWAY_BASE, pick)
}

/// Validate every pick first, then insert; an existing alias always wins,
/// whatever provider it belongs to. Returns how many were inserted.
pub fn add_subscription_models(
    reg: &mut ModelRegistry,
    provider: &str,
    base_url: &str,
    picks: &[ChatGptModelPick],
) -> Result<u32, String> {
    let mut seen = std::collections::HashSet::new();
    for pick in picks {
        validate_alias(&pick.alias)?;
        if pick.model.trim().is_empty() || pick.model.chars().any(char::is_control) {
            return Err(format!("model id {:?} is not usable", pick.model));
        }
        if !seen.insert(pick.alias.as_str()) {
            return Err(format!("alias {:?} given twice", pick.alias));
        }
    }
    let mut added = 0;
    for pick in picks {
        if !reg.models.contains_key(&pick.alias) {
            reg.models.insert(
                pick.alias.clone(),
                subscription_entry(provider, base_url, pick),
            );
            added += 1;
        }
    }
    Ok(added)
}

pub fn add_chatgpt_models(
    reg: &mut ModelRegistry,
    picks: &[ChatGptModelPick],
) -> Result<u32, String> {
    add_subscription_models(reg, "codex", CHATGPT_GATEWAY_BASE, picks)
}

/// Remove only what a subscription provider wrote: matching `provider`
/// *and* subscription billing. A hand-authored entry without the billing
/// marker is left alone. Returns how many were removed.
pub fn disconnect_subscription(reg: &mut ModelRegistry, provider: &str) -> u32 {
    let before = reg.models.len();
    reg.models
        .retain(|_, e| !(e.provider == provider && e.billing == Some(BillingMode::Subscription)));
    (before - reg.models.len()) as u32
}

pub fn disconnect_chatgpt(reg: &mut ModelRegistry) -> u32 {
    disconnect_subscription(reg, "codex")
}

#[tauri::command]
pub fn chatgpt_models_add(picks: Vec<ChatGptModelPick>) -> Result<(), String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    add_chatgpt_models(&mut reg, &picks)?;
    reg.save_to(&path).map_err(|e| e.to_string())
}

/// Disconnect MUR from the subscription: registry entries only. The Codex
/// login and the gateway are untouched — other Codex clients keep working.
#[tauri::command]
pub fn chatgpt_disconnect() -> Result<u32, String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    let removed = disconnect_chatgpt(&mut reg);
    if removed > 0 {
        reg.save_to(&path).map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(model: &str, alias: &str, verified: bool) -> ChatGptModelPick {
        ChatGptModelPick {
            model: model.into(),
            alias: alias.into(),
            verified,
        }
    }

    #[test]
    fn a_subscription_pick_becomes_exactly_the_documented_entry() {
        let mut reg = ModelRegistry::default();
        let n = add_chatgpt_models(&mut reg, &[pick("gpt-5.6-sol", "chatgpt_sol", true)]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            reg.models["chatgpt_sol"],
            ModelEntry {
                provider: "codex".into(),
                model: "gpt-5.6-sol".into(),
                base_url: Some("http://127.0.0.1:8088/codex/v1".into()),
                secret: None,
                tier: Some(RouteTier::Frontier),
                billing: Some(BillingMode::Subscription),
                catalog_verified: Some(true),
                ..Default::default()
            }
        );
        // Serialized form carries no secret and no key material.
        let yaml = serde_yaml_ng::to_string(&reg).unwrap();
        assert!(!yaml.contains("secret"), "{yaml}");
        assert!(yaml.contains("billing: subscription"), "{yaml}");
    }

    #[test]
    fn existing_aliases_always_win_and_bad_aliases_are_refused() {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "mine".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-opus-5".into(),
                ..Default::default()
            },
        );
        let n = add_chatgpt_models(&mut reg, &[pick("gpt-5.6-sol", "mine", true)]).unwrap();
        assert_eq!(n, 0);
        assert_eq!(
            reg.models["mine"].provider, "anthropic",
            "overwrote a foreign entry"
        );

        for bad in [
            "",
            "../x",
            "a/b",
            "a b",
            "a\u{7}b",
            ".hidden",
            "x:y",
            &"a".repeat(65),
        ] {
            let err = add_chatgpt_models(&mut reg, &[pick("m", bad, true)]).err();
            assert!(err.is_some(), "{bad:?} accepted");
        }
        let dup = add_chatgpt_models(
            &mut reg,
            &[pick("a", "same", true), pick("b", "same", true)],
        );
        assert!(dup.err().unwrap().contains("twice"));
        assert!(
            !reg.models.contains_key("same"),
            "a failed batch wrote nothing"
        );
        assert!(add_chatgpt_models(&mut reg, &[pick("", "ok", false)]).is_err());
    }

    #[test]
    fn disconnect_removes_only_subscription_codex_entries() {
        let mut reg = ModelRegistry::default();
        add_chatgpt_models(
            &mut reg,
            &[
                pick("gpt-5.6-sol", "sub_a", true),
                pick("gpt-5.6-mini", "sub_b", false),
            ],
        )
        .unwrap();
        reg.models.insert(
            "hand_codex".into(),
            ModelEntry {
                provider: "codex".into(),
                model: "gpt-5.6-sol".into(),
                base_url: Some("http://127.0.0.1:8088/codex/v1".into()),
                ..Default::default()
            },
        );
        reg.models.insert(
            "openai_paid".into(),
            ModelEntry {
                provider: "openai".into(),
                model: "gpt-5.6-sol".into(),
                billing: Some(BillingMode::UsageBilled),
                ..Default::default()
            },
        );
        assert_eq!(disconnect_chatgpt(&mut reg), 2);
        let left: Vec<_> = reg.models.keys().cloned().collect();
        assert_eq!(left, ["hand_codex", "openai_paid"]);
        assert_eq!(disconnect_chatgpt(&mut reg), 0);
    }

    #[test]
    fn disconnect_is_scoped_to_its_own_provider() {
        let mut reg = ModelRegistry::default();
        add_chatgpt_models(&mut reg, &[pick("gpt-5.6-sol", "gpt", true)]).unwrap();
        add_subscription_models(
            &mut reg,
            "claude",
            "http://127.0.0.1:8088/v1",
            &[pick("claude-opus-5", "opus", true)],
        )
        .unwrap();
        assert_eq!(reg.models["opus"].provider, "claude");
        assert_eq!(
            reg.models["opus"].base_url.as_deref(),
            Some("http://127.0.0.1:8088/v1")
        );
        assert_eq!(reg.models["opus"].billing, Some(BillingMode::Subscription));
        assert!(reg.models["opus"].secret.is_none());
        assert_eq!(disconnect_subscription(&mut reg, "claude"), 1);
        assert!(
            reg.models.contains_key("gpt"),
            "a ChatGPT entry survived a Claude disconnect"
        );
        assert_eq!(disconnect_chatgpt(&mut reg), 1);
    }
}
