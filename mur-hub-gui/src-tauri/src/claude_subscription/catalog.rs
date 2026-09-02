//! Models on a Claude plan come from the models.dev catalog — the same
//! source the Hub's Anthropic discovery already uses instead of probing the
//! endpoint. There is no `/v1/models` call: the gateway would forward it
//! with the OAuth token, and the endpoint is not part of the subscription
//! contract.

use crate::chatgpt_subscription::SubscriptionModelView;
use std::path::Path;

const CATALOG_VENDOR: &str = "anthropic";
const DEFAULT_INPUT_MODALITIES: [&str; 2] = ["text", "image"];

/// `None` when no catalog is reachable (fresh cache, network, stale cache
/// all failed) — the panel then offers the unverified-id field.
pub fn catalog_models(mur_home: &Path) -> Option<Vec<SubscriptionModelView>> {
    let ids = mur_core::model_prices::load_or_fetch(mur_home)?.provider_models(CATALOG_VENDOR)?;
    Some(models_from_ids(ids))
}

/// The catalog has no display name, default marker, or effort list, so
/// every row is the id and nothing is pre-selected.
pub fn models_from_ids(ids: Vec<String>) -> Vec<SubscriptionModelView> {
    ids.into_iter()
        .map(|id| SubscriptionModelView {
            display_name: id.clone(),
            id,
            is_default: false,
            reasoning_efforts: vec![],
            input_modalities: DEFAULT_INPUT_MODALITIES.map(String::from).to_vec(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_become_plain_rows_with_no_default() {
        let rows = models_from_ids(vec!["claude-opus-5".into(), "claude-sonnet-5".into()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "claude-opus-5");
        assert_eq!(rows[0].display_name, "claude-opus-5");
        assert!(rows.iter().all(|r| !r.is_default));
        assert_eq!(rows[1].input_modalities, vec!["text", "image"]);
    }

    /// A seeded cache is read without touching the network; the vendor key
    /// is `anthropic`, and other vendors' models do not leak in.
    #[test]
    fn catalog_models_come_from_the_cached_anthropic_entry() {
        let home = tempfile::tempdir().unwrap();
        let path = mur_core::model_prices::cache_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
              "anthropic": { "models": { "claude-b": {}, "claude-a": {} } },
              "openai": { "models": { "gpt-x": {} } }
            }"#,
        )
        .unwrap();
        let rows = catalog_models(home.path()).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["claude-a", "claude-b"], "sorted, anthropic only");
    }
}
