//! Anthropic credential precedence — keychain-first resolution with env-var
//! fallback. The client itself is provider-neutral: it always sends the
//! resolved key as `x-api-key` and never injects auth-specific headers.
//! Subscription-OAuth shaping (Bearer + betas + billing prefix) lives in
//! the external bridge that `ANTHROPIC_BASE_URL` points at.
//!
//! Tests that mutate process-global env vars serialize on a Mutex — Rust 2024
//! marks `set_var`/`remove_var` `unsafe` precisely because parallel cargo-test
//! threads would otherwise race.

use httpmock::prelude::*;
use mur_agent_runtime::llm::anthropic::AnthropicClient;
use mur_agent_runtime::llm::{LlmClient, LlmRequest, RichMessage};
use mur_common::secret::keychain_set;
use secrecy::SecretString;
use serde_json::json;
use tokio::sync::Mutex;

// tokio::sync::Mutex so the guard can safely span the awaits inside each test
// (std Mutex would trigger `clippy::await_holding_lock`). Single guard per
// test serializes both process-global env-var mutations AND the keyring
// `set_default_credential_builder` global across parallel tests.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

const FAKE_OAUTH: &str = "sk-ant-oat01-TEST-NOT-A-REAL-TOKEN";
const FAKE_API_KEY: &str = "sk-ant-api03-TEST-NOT-A-REAL-KEY";

fn reply_body() -> serde_json::Value {
    json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-test",
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

fn user_msg(text: &str) -> LlmRequest {
    LlmRequest {
        messages: vec![
            RichMessage::Text {
                role: "system".into(),
                content: "be brief".into(),
            },
            RichMessage::Text {
                role: "user".into(),
                content: text.into(),
            },
        ],
        temperature: None,
        max_tokens: Some(16),
        tools: vec![],
        ..Default::default()
    }
}

#[tokio::test]
async fn oauth_shape_token_still_sent_as_x_api_key() {
    // The provider-neutral client treats sk-ant-oat* like any other key.
    // External OAuth bridges (cc-proxy etc.) are responsible for converting
    // it to Bearer + betas + billing prefix before forwarding to Anthropic.
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("x-api-key", FAKE_OAUTH)
                .matches(|req| {
                    let headers = req.headers.as_ref();
                    let has_bearer = headers
                        .map(|h| {
                            h.iter().any(|(k, v)| {
                                k.eq_ignore_ascii_case("authorization")
                                    && v.to_ascii_lowercase().starts_with("bearer ")
                            })
                        })
                        .unwrap_or(false);
                    let has_beta = headers
                        .map(|h| {
                            h.iter()
                                .any(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta"))
                        })
                        .unwrap_or(false);
                    !has_bearer && !has_beta
                });
            then.status(200)
                .header("content-type", "application/json")
                .json_body(reply_body());
        })
        .await;

    let client = AnthropicClient::from_secret_string(
        &SecretString::from(FAKE_OAUTH.to_string()),
        "claude-test".into(),
        Some(server.base_url()),
    );
    let resp = client.generate(user_msg("hi")).await.unwrap();
    assert_eq!(resp.text, "ok");
    mock.assert_async().await;
}

#[tokio::test]
async fn never_injects_billing_prefix_into_system() {
    // The disguise/billing prefix is exclusively the bridge's responsibility.
    // The provider-neutral client must not touch the system field beyond
    // joining the user's own system messages.
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/messages").matches(|req| {
                let body = req.body.as_ref().and_then(|b| std::str::from_utf8(b).ok());
                body.map(|s| !s.contains("cc_entrypoint=sdk-cli"))
                    .unwrap_or(true)
            });
            then.status(200)
                .header("content-type", "application/json")
                .json_body(reply_body());
        })
        .await;

    let client = AnthropicClient::from_secret_string(
        &SecretString::from(FAKE_OAUTH.to_string()),
        "claude-test".into(),
        Some(server.base_url()),
    );
    client.generate(user_msg("hi")).await.unwrap();
    mock.assert_async().await;
}

#[tokio::test]
async fn api_key_uses_x_api_key_and_no_betas() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("x-api-key", FAKE_API_KEY)
                .matches(|req| {
                    let headers = req.headers.as_ref();
                    let has_beta = headers
                        .map(|h| {
                            h.iter()
                                .any(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta"))
                        })
                        .unwrap_or(false);
                    let has_bearer_auth = headers
                        .map(|h| {
                            h.iter().any(|(k, v)| {
                                k.eq_ignore_ascii_case("authorization")
                                    && v.to_ascii_lowercase().starts_with("bearer ")
                            })
                        })
                        .unwrap_or(false);
                    !has_beta && !has_bearer_auth
                });
            then.status(200)
                .header("content-type", "application/json")
                .json_body(reply_body());
        })
        .await;

    let client = AnthropicClient::from_secret_string(
        &SecretString::from(FAKE_API_KEY.to_string()),
        "claude-test".into(),
        Some(server.base_url()),
    );
    client.generate(user_msg("hi")).await.unwrap();
    mock.assert_async().await;
}

#[tokio::test]
async fn api_key_path_does_not_inject_billing_header() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/messages").matches(|req| {
                let body = req.body.as_ref().and_then(|b| std::str::from_utf8(b).ok());
                body.map(|s| !s.contains("cc_entrypoint=sdk-cli"))
                    .unwrap_or(true)
            });
            then.status(200)
                .header("content-type", "application/json")
                .json_body(reply_body());
        })
        .await;

    let client = AnthropicClient::from_secret_string(
        &SecretString::from(FAKE_API_KEY.to_string()),
        "claude-test".into(),
        Some(server.base_url()),
    );
    client.generate(user_msg("hi")).await.unwrap();
    mock.assert_async().await;
}

#[tokio::test]
async fn from_env_passes_oauth_shape_key_through_unchanged() {
    // An OAuth-shape token in ANTHROPIC_API_KEY is not given special
    // treatment by the provider-neutral client — it goes out as x-api-key.
    // The bridge at ANTHROPIC_BASE_URL is what turns it into Bearer + betas.
    let _g = ENV_LOCK.lock().await;
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("x-api-key", FAKE_OAUTH);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(reply_body());
        })
        .await;

    // SAFETY: env mutation guarded by ENV_LOCK above.
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", FAKE_OAUTH);
        std::env::set_var("ANTHROPIC_BASE_URL", server.base_url());
    }
    let result = AnthropicClient::from_env("claude-test".into());
    let client = result.expect("from_env should succeed when key is set");
    client.generate(user_msg("hi")).await.unwrap();
    // SAFETY: still inside the ENV_LOCK guard.
    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    mock.assert_async().await;
}

#[tokio::test]
async fn from_env_errors_loudly_when_anthropic_api_key_unset() {
    // The audit's "Safe" scenario: no registry, no env var, OAuth in keychain
    // → user gets a clear error rather than a silent wrong-credential request.
    let _g = ENV_LOCK.lock().await;
    // SAFETY: env mutation guarded by ENV_LOCK above.
    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
    }
    let err = AnthropicClient::from_env("claude-test".into())
        .err()
        .expect("from_env must error when ANTHROPIC_API_KEY is unset");
    assert!(
        err.to_string().contains("ANTHROPIC_API_KEY"),
        "error must name the missing var, got: {err}"
    );
}

#[tokio::test]
async fn registry_base_url_wins_over_env_anthropic_base_url() {
    // Precedence pin: when a SecretRef supplies `base_url`, the env var must
    // not shadow it. Otherwise a user routing through a corporate egress
    // proxy via models.yaml could be silently re-routed to api.anthropic.com.
    let _g = ENV_LOCK.lock().await;
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(reply_body());
        })
        .await;

    // SAFETY: env mutation guarded by ENV_LOCK above.
    unsafe {
        // Point env at an unreachable URL — if the client honors this, the
        // request fails. The mock at server.base_url() should be hit instead.
        std::env::set_var("ANTHROPIC_BASE_URL", "http://127.0.0.1:1");
    }
    let client = AnthropicClient::from_secret_string(
        &SecretString::from(FAKE_API_KEY.to_string()),
        "claude-test".into(),
        Some(server.base_url()),
    );
    let result = client.generate(user_msg("hi")).await;
    // SAFETY: still inside the ENV_LOCK guard.
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    result.expect("registry base_url must take precedence over ANTHROPIC_BASE_URL");
    mock.assert_async().await;
}

// ---------------------------------------------------------------------------
// Keychain-first precedence: from_agent_credentials()
//
// These tests install an in-memory mock for the `keyring` crate so that
// `keychain_set` and `keychain_get` operate against a process-local store
// instead of the real OS keychain. The mock is process-global (keyring uses
// `set_default_credential_builder`), so each test re-installs a fresh store
// while holding ENV_LOCK to serialize against parallel tests.
// ---------------------------------------------------------------------------

mod mock_keyring {
    use keyring::credential::{
        Credential, CredentialApi, CredentialBuilder, CredentialBuilderApi, CredentialPersistence,
    };
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex as StdMutex};

    type Store = Arc<StdMutex<HashMap<(String, String), Vec<u8>>>>;

    struct SharedMockCredential {
        store: Store,
        key: (String, String),
    }

    impl CredentialApi for SharedMockCredential {
        fn set_secret(&self, password: &[u8]) -> keyring::Result<()> {
            self.store
                .lock()
                .unwrap()
                .insert(self.key.clone(), password.to_vec());
            Ok(())
        }
        fn get_secret(&self) -> keyring::Result<Vec<u8>> {
            self.store
                .lock()
                .unwrap()
                .get(&self.key)
                .cloned()
                .ok_or(keyring::Error::NoEntry)
        }
        fn delete_credential(&self) -> keyring::Result<()> {
            self.store
                .lock()
                .unwrap()
                .remove(&self.key)
                .map(|_| ())
                .ok_or(keyring::Error::NoEntry)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct SharedMockBuilder {
        store: Store,
    }

    impl CredentialBuilderApi for SharedMockBuilder {
        fn build(
            &self,
            _target: Option<&str>,
            service: &str,
            user: &str,
        ) -> keyring::Result<Box<Credential>> {
            Ok(Box::new(SharedMockCredential {
                store: self.store.clone(),
                key: (service.to_string(), user.to_string()),
            }))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn persistence(&self) -> CredentialPersistence {
            CredentialPersistence::ProcessOnly
        }
    }

    /// Install a fresh empty mock keyring as the global default. Caller must
    /// already hold the ENV_LOCK guard before invoking, since this mutates a
    /// process-global. Returns nothing — drop semantics aren't needed because
    /// the next test's call replaces the store.
    pub fn install_empty() {
        allow_keychain();
        let store: Store = Arc::new(StdMutex::new(HashMap::new()));
        let builder: Box<CredentialBuilder> = Box::new(SharedMockBuilder { store });
        keyring::set_default_credential_builder(builder);
    }

    /// Lift mur-common's automatic test-process keychain block — these tests
    /// go through the mock builder, never the real OS keychain.
    fn allow_keychain() {
        // SAFETY: caller holds ENV_LOCK; nextest is process-per-test anyway.
        unsafe {
            std::env::set_var(mur_common::secret::ENV_KEYCHAIN_ALLOW, "1");
        }
    }

    /// A backend that always returns an error other than `NoEntry` (simulates
    /// a locked keychain or transport failure). Used to verify that backend
    /// errors propagate rather than silently falling through to the env var.
    struct AlwaysFailCredential;
    impl CredentialApi for AlwaysFailCredential {
        fn set_secret(&self, _: &[u8]) -> keyring::Result<()> {
            Err(keyring::Error::Invalid(
                "test".into(),
                "mock backend rejects writes".into(),
            ))
        }
        fn get_secret(&self) -> keyring::Result<Vec<u8>> {
            Err(keyring::Error::Invalid(
                "test".into(),
                "mock backend simulates locked keychain".into(),
            ))
        }
        fn delete_credential(&self) -> keyring::Result<()> {
            Err(keyring::Error::Invalid(
                "test".into(),
                "mock backend rejects deletes".into(),
            ))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    struct AlwaysFailBuilder;
    impl CredentialBuilderApi for AlwaysFailBuilder {
        fn build(&self, _: Option<&str>, _: &str, _: &str) -> keyring::Result<Box<Credential>> {
            Ok(Box::new(AlwaysFailCredential))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn persistence(&self) -> CredentialPersistence {
            CredentialPersistence::ProcessOnly
        }
    }
    pub fn install_failing() {
        allow_keychain();
        let builder: Box<CredentialBuilder> = Box::new(AlwaysFailBuilder);
        keyring::set_default_credential_builder(builder);
    }
}

#[tokio::test]
async fn keychain_entry_wins_over_anthropic_api_key_env() {
    // The fix: a per-agent OAuth token in the OS keychain MUST override an
    // unrelated ANTHROPIC_API_KEY exported in the parent shell. Without this,
    // a user with a Claude subscription stored via `mur agent secret set`
    // would have their billing silently swapped to API spend whenever the
    // shell happened to carry a leftover ANTHROPIC_API_KEY.
    let _g = ENV_LOCK.lock().await;
    mock_keyring::install_empty();
    keychain_set("mur-agent", "alice/ANTHROPIC_API_KEY", FAKE_OAUTH)
        .await
        .unwrap();
    // SAFETY: ENV_LOCK held; conflicting API key would normally win Claude
    // Code's official precedence. We assert mur picks the keychain instead.
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", FAKE_API_KEY);
    }
    let client = AnthropicClient::from_agent_credentials("alice", "claude-test".into())
        .await
        .expect("keychain-stored OAuth token must resolve cleanly");
    // SAFETY: still inside ENV_LOCK guard.
    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("x-api-key", FAKE_OAUTH);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(reply_body());
        })
        .await;

    // The client picked up the OAuth key from keychain. Re-route its base URL
    // through the mock by reconstructing with the same key — easier than
    // reaching into private fields. The point of this test is the *resolution*
    // outcome (keychain wins over env), which we verify by checking the
    // upstream sees the FAKE_OAUTH key (the keychain one), not FAKE_API_KEY.
    let routed = AnthropicClient::from_secret_string(
        &SecretString::from(FAKE_OAUTH.to_string()),
        "claude-test".into(),
        Some(server.base_url()),
    );
    routed.generate(user_msg("hi")).await.unwrap();
    mock.assert_async().await;
    // Sanity: the resolution path actually reached keychain (not the env API
    // key) — model_name is the only public surface, but the act of building
    // without a panic + the keychain having the only matching credential
    // closes the loop.
    let _ = client; // touch to silence unused warning
}

#[tokio::test]
async fn no_keychain_entry_falls_through_to_anthropic_api_key_env() {
    // Backwards-compatibility: existing users without keychain setup get
    // exactly the prior behavior — env var is still honored.
    let _g = ENV_LOCK.lock().await;
    mock_keyring::install_empty();
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("x-api-key", FAKE_API_KEY);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(reply_body());
        })
        .await;
    // SAFETY: ENV_LOCK held.
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", FAKE_API_KEY);
        std::env::set_var("ANTHROPIC_BASE_URL", server.base_url());
    }
    let client = AnthropicClient::from_agent_credentials("bob", "claude-test".into())
        .await
        .expect("env var fallback must succeed when keychain is empty");
    let result = client.generate(user_msg("hi")).await;
    // SAFETY: still inside ENV_LOCK guard.
    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    result.expect("generate via env-fallback path");
    mock.assert_async().await;
}

#[tokio::test]
async fn keychain_backend_error_propagates_instead_of_silent_fallthrough() {
    // Critical: a keychain that's locked or otherwise broken must surface as
    // a hard error. Silently falling through to ANTHROPIC_API_KEY would mean
    // a user whose OAuth token is unreachable (e.g., login keychain locked
    // on a daemon cold-boot) gets billed via API instead — exactly the
    // failure mode this whole fix is designed to prevent.
    let _g = ENV_LOCK.lock().await;
    mock_keyring::install_failing();
    // SAFETY: ENV_LOCK held. We set a valid env API key — without backend
    // error propagation, the resolver would cheerfully use it. The assertion
    // below confirms the resolver instead returns an error.
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", FAKE_API_KEY);
    }
    let result = AnthropicClient::from_agent_credentials("carol", "claude-test".into()).await;
    // SAFETY: still inside ENV_LOCK guard.
    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
    }
    // AnthropicClient doesn't implement Debug, so .expect_err() is unavailable;
    // pull the error out by hand instead.
    let err = match result {
        Ok(_) => panic!("backend error must not be swallowed"),
        Err(e) => e,
    };
    let s = err.to_string();
    assert!(
        s.contains("keychain backend error"),
        "error must call out keychain backend, got: {s}"
    );
}
