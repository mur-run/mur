//! Daily revocations refresh service — spec §7.4.1.
//!
//! `RevocationRefresher::start()` spawns a background task that fetches the
//! remote revocations list once on startup and then every 24 hours. The list
//! is cached locally at `<mur_home>/trust/revocations.json`.
//!
//! `check_not_expired()` is called by Hub on startup to decide whether to show
//! the "Trust list expired — connect to refresh" dialog.
//!
//! V1 note: `REVOCATIONS_URL` is a placeholder endpoint. The mur root key and
//! issuer infrastructure are V2 work; signature verification in
//! `RevocationsList::parse_and_validate` is currently a stub.

use mur_common::trust::RevocationsList;
use std::path::{Path, PathBuf};
use tokio::time::{Duration, interval};

/// Endpoint for the signed revocations list. V2 will make this configurable.
pub const REVOCATIONS_URL: &str = "https://mur.run/revocations.json";

/// Background service that refreshes the revocations list daily.
pub struct RevocationRefresher {
    mur_home: PathBuf,
    http: reqwest::Client,
}

impl RevocationRefresher {
    /// Spawn the background refresh task.
    ///
    /// Fetches immediately on startup (so the first run doesn't wait 24 h),
    /// then ticks every 24 hours. Errors are logged as warnings — a failed
    /// refresh is non-fatal (Hub continues with the cached list until it
    /// expires).
    pub fn start(mur_home: PathBuf, http: reqwest::Client) {
        tokio::spawn(async move {
            let refresher = Self { mur_home, http };
            refresher.refresh().await;
            let mut ticker = interval(Duration::from_secs(86400));
            ticker.tick().await; // consume the immediate first tick
            loop {
                ticker.tick().await;
                refresher.refresh().await;
            }
        });
    }

    async fn refresh(&self) {
        let cached_crl = RevocationsList::load_cached(&self.mur_home).map(|r| r.crl_number);
        match self.fetch_and_parse(cached_crl).await {
            Ok(list) => {
                if let Err(e) = list.save_cached(&self.mur_home) {
                    tracing::warn!("revocations: failed to save cache: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("revocations: refresh failed: {e}");
            }
        }
    }

    async fn fetch_and_parse(
        &self,
        known_crl: Option<u64>,
    ) -> anyhow::Result<RevocationsList> {
        let bytes = self
            .http
            .get(REVOCATIONS_URL)
            .send()
            .await?
            .bytes()
            .await?;
        RevocationsList::parse_and_validate(&bytes, known_crl)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// Returns `true` if the locally-cached revocations list is present and not
/// yet expired, or if no cache exists (first-run bootstrap).
///
/// Returns `false` only when a cached list exists *and* its `expires_at` is
/// in the past — Hub should show the "Trust list expired" dialog in that case.
pub fn check_not_expired(mur_home: &Path) -> bool {
    match RevocationsList::load_cached(mur_home) {
        None => true, // first run: no cache, grant pass
        Some(list) => !list.is_expired(),
    }
}
