//! USD spot rate service — fetches live exchange rates from the Frankfurter API.
//!
//! No API key required. Rates are refreshed on every call; callers can cache
//! the result if they need a TTL-based cache.
//!
//! Example:
//! ```ignore
//! let rate = mur_core::store::spot_rate::fetch_usd_rate("EUR").await.unwrap();
//! println!("1 USD = {} EUR", rate.rate);
//! ```

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Response model ──────────────────────────────────────────────────────────

/// A single USD spot rate against one target currency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotRate {
    /// Base currency (always "USD").
    pub base: String,
    /// Target currency code, e.g. "EUR".
    pub target: String,
    /// Exchange rate: 1 USD = `rate` `target`.
    pub rate: f64,
    /// Date the rate was published by the upstream source.
    pub date: NaiveDate,
    /// When this struct was populated (wall-clock time of the fetch).
    pub fetched_at: DateTime<Utc>,
}

/// Full Frankfurter API response — includes all available rates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotRateSnapshot {
    /// Base currency (always "USD" when called via `fetch_usd_rates`).
    pub base: String,
    /// Date the rates were published.
    pub date: NaiveDate,
    /// All available rates keyed by currency code.
    pub rates: HashMap<String, f64>,
    /// When this snapshot was fetched.
    pub fetched_at: DateTime<Utc>,
}

// ─── Internal API types ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct FrankfurterResponse {
    base: String,
    date: NaiveDate,
    rates: HashMap<String, f64>,
}

// ─── Public API ───────────────────────────────────────────────────────────────

const FRANKFURTER_BASE: &str = "https://api.frankfurter.app";

/// Fetch the current USD spot rate for a single `target` currency.
///
/// Returns an error if the target is not supported by the upstream API or the
/// network request fails.
pub async fn fetch_usd_rate(target: &str) -> Result<SpotRate> {
    let url = format!("{}/latest?from=USD&to={}", FRANKFURTER_BASE, target);
    let resp = reqwest::get(&url)
        .await
        .with_context(|| format!("HTTP request to Frankfurter API failed: {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Frankfurter API error {status}: {body}");
    }

    let data: FrankfurterResponse = resp
        .json()
        .await
        .context("Failed to parse Frankfurter API response")?;

    let rate = *data
        .rates
        .get(target)
        .with_context(|| format!("Currency '{target}' not found in API response"))?;

    Ok(SpotRate {
        base: data.base,
        target: target.to_uppercase(),
        rate,
        date: data.date,
        fetched_at: Utc::now(),
    })
}

/// Fetch all current USD spot rates in a single request.
///
/// More efficient than calling `fetch_usd_rate` repeatedly when you need
/// multiple currencies.
pub async fn fetch_usd_rates() -> Result<SpotRateSnapshot> {
    let url = format!("{}/latest?from=USD", FRANKFURTER_BASE);
    let resp = reqwest::get(&url)
        .await
        .with_context(|| format!("HTTP request to Frankfurter API failed: {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Frankfurter API error {status}: {body}");
    }

    let data: FrankfurterResponse = resp
        .json()
        .await
        .context("Failed to parse Frankfurter API response")?;

    Ok(SpotRateSnapshot {
        base: data.base,
        date: data.date,
        rates: data.rates,
        fetched_at: Utc::now(),
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the response model deserializes correctly from a known-good payload.
    #[test]
    fn deserialize_frankfurter_response() {
        let json = r#"{
            "amount": 1.0,
            "base": "USD",
            "date": "2024-01-15",
            "rates": { "EUR": 0.9123, "GBP": 0.7891, "JPY": 148.5 }
        }"#;
        let resp: FrankfurterResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.base, "USD");
        assert!((resp.rates["EUR"] - 0.9123).abs() < 1e-6);
    }

    #[test]
    fn spot_rate_fields() {
        let rate = SpotRate {
            base: "USD".into(),
            target: "EUR".into(),
            rate: 0.92,
            date: "2024-01-15".parse().unwrap(),
            fetched_at: Utc::now(),
        };
        assert_eq!(rate.base, "USD");
        assert_eq!(rate.target, "EUR");
        assert!((rate.rate - 0.92).abs() < 1e-6);
    }

    #[test]
    fn deserialize_spot_rate_snapshot() {
        let json = r#"{
            "base": "USD",
            "date": "2024-01-15",
            "rates": { "EUR": 0.9123, "GBP": 0.7891 },
            "fetched_at": "2024-01-15T12:00:00Z"
        }"#;
        let snap: SpotRateSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.base, "USD");
        assert_eq!(snap.date, "2024-01-15".parse::<chrono::NaiveDate>().unwrap());
        assert!((snap.rates["EUR"] - 0.9123).abs() < 1e-6);
        assert!((snap.rates["GBP"] - 0.7891).abs() < 1e-6);
    }

    #[test]
    fn spot_rate_json_round_trip() {
        let original = SpotRate {
            base: "USD".into(),
            target: "JPY".into(),
            rate: 149.5,
            date: "2024-01-15".parse().unwrap(),
            fetched_at: chrono::DateTime::parse_from_rfc3339("2024-01-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let json = serde_json::to_string(&original).unwrap();
        let roundtripped: SpotRate = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtripped.base, original.base);
        assert_eq!(roundtripped.target, original.target);
        assert!((roundtripped.rate - original.rate).abs() < 1e-6);
        assert_eq!(roundtripped.date, original.date);
    }

    #[test]
    fn snapshot_json_round_trip() {
        let mut rates = HashMap::new();
        rates.insert("EUR".to_string(), 0.92_f64);
        rates.insert("GBP".to_string(), 0.79_f64);
        let original = SpotRateSnapshot {
            base: "USD".into(),
            date: "2024-01-15".parse().unwrap(),
            rates,
            fetched_at: chrono::DateTime::parse_from_rfc3339("2024-01-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let json = serde_json::to_string(&original).unwrap();
        let roundtripped: SpotRateSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtripped.base, original.base);
        assert_eq!(roundtripped.rates.len(), 2);
        assert!((roundtripped.rates["EUR"] - 0.92).abs() < 1e-6);
    }

    /// Live integration test — requires network. Run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn fetch_usd_rate_live_eur() {
        let rate = fetch_usd_rate("EUR").await.unwrap();
        assert_eq!(rate.base, "USD");
        assert_eq!(rate.target, "EUR");
        assert!(rate.rate > 0.0);
    }

    /// Live integration test — requires network. Run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn fetch_usd_rates_live_snapshot() {
        let snap = fetch_usd_rates().await.unwrap();
        assert_eq!(snap.base, "USD");
        assert!(!snap.rates.is_empty());
        assert!(snap.rates.contains_key("EUR"));
    }

    /// Live integration test — requires network. Run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn fetch_usd_rate_invalid_currency_errors() {
        let result = fetch_usd_rate("NOTACURRENCY").await;
        assert!(result.is_err());
    }
}
