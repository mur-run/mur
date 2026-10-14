//! The `secret` check for a model entry that names no `secret` ref (#1880).

use super::Check;

/// The environment variable a cloud provider falls back to when its model
/// entry names no `secret` ref. `None` for providers that take no implicit key
/// (`claude`/`codex` go through the loopback gateway, `echo` is a stub).
fn key_name(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        _ => None,
    }
}

/// The `secret` check for a model entry that carries no `secret` ref (#1880).
///
/// Such an entry is not "needs no secret" for a cloud provider: the runtime
/// reads the agent's keychain entry, then the environment, and the agent has
/// no working model when both are empty. `None` means the provider takes no
/// implicit key, or a loopback base_url that does not authenticate, so the
/// caller keeps its "needs no secret" line. Pure so it is testable without a
/// keychain: the caller supplies what it found.
fn check(
    provider: &str,
    base_url: Option<&str>,
    agent: &str,
    in_keychain: bool,
    in_env: bool,
) -> Option<Check> {
    let key = key_name(provider)?;
    if provider == "openai"
        && base_url.is_some_and(mur_agent_runtime::llm::loopback::is_loopback_base_url)
    {
        return None;
    }
    Some(if in_keychain {
        Check::new("secret", true, format!("{key} is in the agent's keychain"))
    } else if in_env {
        Check::new(
            "secret",
            true,
            format!(
                "{key} is set in this shell only — a running agent may not inherit it; \
                 store it with `mur agent secret {agent} set {key}`"
            ),
        )
    } else {
        Check::new(
            "secret",
            false,
            format!(
                "{key} is not set (agent keychain and environment both empty), so the agent \
                 has no working model. Fix: `mur agent secret {agent} set {key}`, then \
                 `mur agent restart {agent}`."
            ),
        )
    })
}

/// Probe the real keychain/env for [`check`]. Presence only:
/// `keychain_item_exists` never reads the value, so doctor cannot trigger an
/// authorization prompt here.
pub(super) fn status(entry: &mur_common::model::ModelEntry, agent: &str) -> Check {
    let Some(key) = key_name(&entry.provider) else {
        return Check::new("secret", true, "model needs no secret");
    };
    let in_keychain =
        mur_common::secret::keychain_item_exists("mur-agent", &format!("{agent}/{key}"))
            .unwrap_or(false);
    let in_env = std::env::var_os(key).is_some_and(|v| !v.is_empty());
    check(
        &entry.provider,
        entry.base_url.as_deref(),
        agent,
        in_keychain,
        in_env,
    )
    .unwrap_or_else(|| Check::new("secret", true, "model needs no secret"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1880: a model entry with no `secret` ref is not "needs no secret" for a
    /// cloud provider — the runtime falls back to the agent's keychain entry and
    /// then the environment, and fails the agent when both are empty.
    #[test]
    fn a_cloud_provider_with_no_key_anywhere_fails_the_secret_check() {
        let c = check("anthropic", None, "mur", false, false).unwrap();
        assert!(!c.ok, "{}", c.detail);
        assert_eq!(c.name, "secret");
        assert!(c.detail.contains("ANTHROPIC_API_KEY"), "{}", c.detail);
        assert!(
            c.detail
                .contains("mur agent secret mur set ANTHROPIC_API_KEY"),
            "{}",
            c.detail
        );
        let o = check("openai", None, "mur", false, false).unwrap();
        assert!(!o.ok && o.detail.contains("OPENAI_API_KEY"), "{}", o.detail);
    }

    #[test]
    fn a_keychain_entry_or_env_var_satisfies_the_check() {
        let k = check("anthropic", None, "mur", true, false).unwrap();
        assert!(k.ok, "{}", k.detail);
        let e = check("anthropic", None, "mur", false, true).unwrap();
        assert!(e.ok, "{}", e.detail);
        // Env alone is only this shell's environment; a running agent may not
        // inherit it, so the pass must say so rather than claim certainty.
        assert!(e.detail.contains("this shell"), "{}", e.detail);
    }

    /// Providers that never take an implicit key must not be flagged: the Claude
    /// subscription goes through the loopback gateway, and a loopback
    /// OpenAI-compatible server does not authenticate.
    #[test]
    fn providers_without_an_implicit_key_are_not_checked() {
        assert!(check("claude", None, "mur", false, false).is_none());
        assert!(check("echo", None, "mur", false, false).is_none());
        assert!(
            check(
                "openai",
                Some("http://127.0.0.1:1234/v1"),
                "mur",
                false,
                false
            )
            .is_none()
        );
    }
}
