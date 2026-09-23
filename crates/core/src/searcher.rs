//! Search providers behind one trait: Exa (default, multi-key pool) and
//! TinyFish Search (fallback). Mock implementation lives in `mock_providers.rs`.
//!
//! The Exa pool mirrors the local Hermes `exa-pool` convention: several keys
//! rotate on billing/quota (402) or rate-limit (429) errors, and a key that
//! answered with one is parked for `KEY_COOLDOWN_SECS` instead of being
//! retried on every post. `FallbackSearchProvider` then carries the call to
//! TinyFish Search when the pool is exhausted or Exa fails outright.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::PipelineError;

/// How long a key that answered with a billing/quota or rate-limit error is
/// parked before it may be tried again. Mirrors the Hermes `exa-pool` plugin's
/// one-hour cooldown.
pub const KEY_COOLDOWN_SECS: i64 = 3600;

/// Freshness window expressed for each backend:
/// - Exa / TinyFish: `recency_minutes` (None = no date filter)
/// - Exa: ISO start/end published dates (computed from now - window)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreshnessWindow {
    pub recency_minutes: Option<i64>,
    pub bucket: &'static str,
}

impl FreshnessWindow {
    pub fn is_evergreen(&self) -> bool {
        self.recency_minutes.is_none()
    }
    /// Start date as `YYYY-MM-DD` (for Exa startPublishedDate).
    pub fn start_date(&self, now_unix: i64) -> Option<String> {
        let minutes = self.recency_minutes?;
        let secs_ago = now_unix - minutes.saturating_mul(60);
        let days = secs_ago.div_euclid(86_400);
        Some(epoch_day_to_iso(days))
    }
    pub fn end_date(&self, now_unix: i64) -> String {
        epoch_day_to_iso(now_unix.div_euclid(86_400))
    }
}

fn epoch_day_to_iso(days: i64) -> String {
    // Days since 1970-01-01 → YYYY-MM-DD (civil-from-days algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchHit {
    pub url: String,
    pub title: String,
    pub snippet: String,
    pub published_date: Option<String>,
}

#[async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(
        &self,
        queries: &[String],
        window: FreshnessWindow,
        k: usize,
        now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError>;
    async fn find_similar(
        &self,
        url: &str,
        window: FreshnessWindow,
        k: usize,
        now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError>;
}

/// Whether a non-2xx Exa answer means "this key is unusable right now"
/// (billing/quota exhaustion or rate limiting) rather than "this request is
/// wrong". A wrong request fails identically on every key, so only the former
/// parks a key and rotates.
fn is_key_exhausted(status: reqwest::StatusCode, body: &str) -> bool {
    if status == reqwest::StatusCode::PAYMENT_REQUIRED
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return true;
    }
    let body = body.to_ascii_lowercase().replace(['_', '-'], " ");
    let resource = ["credit", "quota", "billing", "payment"]
        .iter()
        .any(|m| body.contains(m));
    let signal = [
        "exceed",
        "exhaust",
        "insufficient",
        "limit reached",
        "out of credits",
        "no credits",
        "zero balance",
        "payment required",
        "too many requests",
        "rate limit",
    ]
    .iter()
    .any(|m| body.contains(m));
    // A bare `rate limit` / `too many requests` phrase is enough on its own.
    resource && signal
        || body.contains("payment required")
        || body.contains("too many requests")
        || body.contains("rate limit")
}

// ---------------------------------------------------------------------------
// Exa — default implementation (POST {base}, multi-key pool)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ExaSearchProvider {
    client: reqwest::Client,
    base_url: String,
    /// Ordered key pool: first key is the preferred one. Empty = unconfigured.
    api_keys: Vec<String>,
    /// key → unix ts when it may be retried. Shared across clones so every
    /// concurrent query sees the same pool state.
    cooldowns: Arc<Mutex<HashMap<String, i64>>>,
}

#[derive(Serialize)]
struct ExaRequest {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_published_date: Option<String>,
    end_published_date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    num_results: usize,
    #[serde(rename = "type")]
    type_: String,
    /// Exa requires an ARRAY of phrases (max 5 words each), not a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    include_text: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_domains: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ExaResponse {
    results: Vec<ExaResult>,
}

#[derive(Deserialize)]
struct ExaResult {
    url: String,
    title: Option<String>,
    snippet: Option<String>,
    published_date: Option<String>,
}

/// Outcome of one attempt against one key.
enum KeyOutcome {
    /// Billing/quota or rate limit — park this key and try the next one.
    Park(String),
    /// Anything else — a malformed request fails on every key, so surface it.
    Fatal(PipelineError),
}

impl ExaSearchProvider {
    /// Single-key provider (back-compat).
    pub fn new(api_key: String) -> Self {
        Self::new_with_keys(vec![api_key])
    }

    /// Pooled provider; slot order is priority order.
    pub fn new_with_keys(api_keys: Vec<String>) -> Self {
        Self::with_base("https://api.exa.ai/search".to_string(), api_keys)
    }

    /// Pooled provider pointed at another base URL (tests, probes).
    pub fn with_base(base_url: String, api_keys: Vec<String>) -> Self {
        let api_keys: Vec<String> = api_keys
            .into_iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        ExaSearchProvider {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
            base_url,
            api_keys,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn key_count(&self) -> usize {
        self.api_keys.len()
    }

    /// Keys that are not parked at `now_unix`, in priority order.
    fn healthy_keys(&self, now_unix: i64) -> Vec<String> {
        let cooldowns = self.cooldowns.lock().expect("exa cooldown lock");
        self.api_keys
            .iter()
            .filter(|k| {
                cooldowns
                    .get(*k)
                    .map(|until| *until <= now_unix)
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }

    fn park_key(&self, key: &str, now_unix: i64) {
        let mut cooldowns = self.cooldowns.lock().expect("exa cooldown lock");
        cooldowns.insert(key.to_string(), now_unix + KEY_COOLDOWN_SECS);
    }

    #[cfg(test)]
    fn parked_key_count(&self) -> usize {
        self.cooldowns.lock().expect("exa cooldown lock").len()
    }

    /// One attempt against one key.
    async fn attempt(
        &self,
        key: &str,
        query: String,
        window: FreshnessWindow,
        k: usize,
        now_unix: i64,
        exclude_domains: Option<Vec<String>>,
    ) -> Result<Vec<SearchHit>, KeyOutcome> {
        let category = matches!(window.bucket, "fast" | "breaking").then(|| "news".to_string());
        let req = ExaRequest {
            query,
            start_published_date: window.start_date(now_unix),
            end_published_date: window.end_date(now_unix),
            category,
            num_results: k,
            type_: "auto".into(),
            include_text: Some(vec!["snippet".into()]),
            exclude_domains,
        };
        let resp = self
            .client
            .post(&self.base_url)
            .header("x-api-key", key)
            .json(&req)
            .send()
            .await
            .map_err(|e| {
                KeyOutcome::Fatal(PipelineError::SearchFailed(format!("exa transport: {e}")))
            })?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(KeyOutcome::Park("exa 429 (rate limited)".into()));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if is_key_exhausted(status, &body) {
                return Err(KeyOutcome::Park(format!("exa http {status}")));
            }
            return Err(KeyOutcome::Fatal(PipelineError::SearchFailed(format!(
                "exa http {status}"
            ))));
        }
        let body: ExaResponse = resp.json().await.map_err(|e| {
            KeyOutcome::Fatal(PipelineError::SearchFailed(format!("exa decode: {e}")))
        })?;
        Ok(body
            .results
            .into_iter()
            .map(|r| SearchHit {
                url: r.url,
                title: r.title.unwrap_or_default(),
                snippet: r.snippet.unwrap_or_default(),
                published_date: r.published_date,
            })
            .collect())
    }

    async fn run(
        &self,
        query: String,
        window: FreshnessWindow,
        k: usize,
        now_unix: i64,
        exclude_domains: Option<Vec<String>>,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        if self.api_keys.is_empty() {
            return Err(PipelineError::SearchFailed(
                "exa: no api key configured".into(),
            ));
        }
        let healthy = self.healthy_keys(now_unix);
        if healthy.is_empty() {
            return Err(PipelineError::SearchFailed(format!(
                "exa pool exhausted: all {} key(s) cooling after a credit/rate-limit error",
                self.api_keys.len()
            )));
        }
        let mut parked = 0usize;
        let mut last: Option<String> = None;
        for key in healthy {
            match self
                .attempt(
                    &key,
                    query.clone(),
                    window,
                    k,
                    now_unix,
                    exclude_domains.clone(),
                )
                .await
            {
                Ok(hits) => return Ok(hits),
                Err(KeyOutcome::Park(reason)) => {
                    tracing::warn!(reason = %reason, "exa key parked; rotating to the next key");
                    self.park_key(&key, now_unix);
                    parked += 1;
                    last = Some(reason);
                }
                Err(KeyOutcome::Fatal(e)) => return Err(e),
            }
        }
        Err(PipelineError::SearchFailed(format!(
            "exa pool exhausted: all {} healthy key(s) hit a credit/rate-limit error ({})",
            parked,
            last.unwrap_or_else(|| "unknown".into())
        )))
    }
}

#[async_trait]
impl SearchProvider for ExaSearchProvider {
    async fn search(
        &self,
        queries: &[String],
        window: FreshnessWindow,
        k: usize,
        now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        // Queries run CONCURRENTLY — sequential calls at ~10-15s each blew
        // the 60s pipeline deadline before fetching even started (measured
        // live: 49s in search+fetch with 3 sequential queries).
        let mut set = tokio::task::JoinSet::new();
        for q in queries {
            let prov = self.clone();
            let q = q.clone();
            set.spawn(async move { prov.run(q, window, k, now_unix, None).await });
        }
        let mut all = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut first_err: Option<PipelineError> = None;
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Ok(hits)) => {
                    for h in hits {
                        if seen.insert(h.url.clone()) {
                            all.push(h);
                        }
                    }
                }
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(PipelineError::Internal(format!("search task join: {e}")));
                    }
                }
            }
        }
        // Only surface an error if we got nothing at all — partial results
        // are better than an aborted search (one query 429ing shouldn't
        // discard the others' hits).
        if all.is_empty() {
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        Ok(all)
    }

    async fn find_similar(
        &self,
        url: &str,
        window: FreshnessWindow,
        k: usize,
        now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        self.run(format!("find similar to {url}"), window, k, now_unix, None)
            .await
    }
}

// ---------------------------------------------------------------------------
// TinyFish Search — fallback implementation
// (GET https://api.search.tinyfish.ai?query=… with an X-API-Key header)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TinyFishSearchProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

#[derive(Deserialize)]
struct TfSearchResponse {
    results: Vec<TfSearchResult>,
}

/// The API's result shape: `date` is free-form ("Jul 30, 2026" / "4 days ago").
#[derive(Deserialize)]
struct TfSearchResult {
    url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    snippet: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    site_name: Option<String>,
}

impl TinyFishSearchProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_base("https://api.search.tinyfish.ai".to_string(), api_key)
    }

    /// Provider pointed at another base URL (tests, probes).
    pub fn with_base(base_url: String, api_key: String) -> Self {
        TinyFishSearchProvider {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
            base_url,
            api_key,
        }
    }

    async fn run(
        &self,
        query: String,
        window: FreshnessWindow,
        k: usize,
        exclude_domains: Option<Vec<String>>,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        // The Search API is a GET endpoint with query parameters; there is no
        // `num_results` parameter, so `k` is applied to the returned page.
        let mut params: Vec<(&str, String)> = vec![("query", query)];
        if let Some(minutes) = window.recency_minutes {
            params.push(("recency_minutes", minutes.to_string()));
        }
        if matches!(window.bucket, "fast" | "breaking") {
            params.push(("domain_type", "news".to_string()));
        }
        if let Some(domains) = exclude_domains {
            if !domains.is_empty() {
                params.push(("exclude_domains", domains.join(",")));
            }
        }
        let resp = self
            .client
            .get(&self.base_url)
            .header("X-API-Key", &self.api_key)
            .query(&params)
            .send()
            .await
            .map_err(|e| PipelineError::SearchFailed(format!("tinyfish search transport: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(PipelineError::SearchFailed(format!(
                "tinyfish search http {status}"
            )));
        }
        let body: TfSearchResponse = resp
            .json()
            .await
            .map_err(|e| PipelineError::SearchFailed(format!("tinyfish search decode: {e}")))?;
        Ok(body
            .results
            .into_iter()
            .take(k)
            .map(|r| SearchHit {
                url: r.url,
                title: r.title.unwrap_or_default(),
                snippet: r.snippet.unwrap_or_default(),
                published_date: r.date,
            })
            .collect())
    }
}

#[async_trait]
impl SearchProvider for TinyFishSearchProvider {
    async fn search(
        &self,
        queries: &[String],
        window: FreshnessWindow,
        k: usize,
        _now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        // Queries run CONCURRENTLY — see ExaSearchProvider::search.
        let mut set = tokio::task::JoinSet::new();
        for q in queries {
            let prov = self.clone();
            let q = q.clone();
            set.spawn(async move { prov.run(q, window, k, None).await });
        }
        let mut all = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut first_err: Option<PipelineError> = None;
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Ok(hits)) => {
                    for h in hits {
                        if seen.insert(h.url.clone()) {
                            all.push(h);
                        }
                    }
                }
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(PipelineError::Internal(format!("search task join: {e}")));
                    }
                }
            }
        }
        if all.is_empty() {
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        Ok(all)
    }

    async fn find_similar(
        &self,
        url: &str,
        window: FreshnessWindow,
        k: usize,
        _now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        self.run(format!("find similar to {url}"), window, k, None)
            .await
    }
}

// ---------------------------------------------------------------------------
// Fallback chain — a second search backend behind the primary
// ---------------------------------------------------------------------------

/// Runs the primary search provider and, when it FAILS (pool exhausted, HTTP
/// error, transport error, decode error), retries the whole call on the
/// fallback. An empty-but-successful result is a real answer (sparse topic)
/// and is returned as-is — it is not an error to paper over.
#[derive(Clone)]
pub struct FallbackSearchProvider {
    primary: Arc<dyn SearchProvider>,
    fallback: Arc<dyn SearchProvider>,
}

impl FallbackSearchProvider {
    pub fn new(primary: Arc<dyn SearchProvider>, fallback: Arc<dyn SearchProvider>) -> Self {
        FallbackSearchProvider { primary, fallback }
    }
}

#[async_trait]
impl SearchProvider for FallbackSearchProvider {
    async fn search(
        &self,
        queries: &[String],
        window: FreshnessWindow,
        k: usize,
        now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        match self.primary.search(queries, window, k, now_unix).await {
            Ok(hits) => Ok(hits),
            Err(primary_err) => {
                tracing::warn!(?primary_err, "primary search failed; using fallback");
                self.fallback.search(queries, window, k, now_unix).await
            }
        }
    }

    async fn find_similar(
        &self,
        url: &str,
        window: FreshnessWindow,
        k: usize,
        now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        match self.primary.find_similar(url, window, k, now_unix).await {
            Ok(hits) => Ok(hits),
            Err(primary_err) => {
                tracing::warn!(?primary_err, "primary find_similar failed; using fallback");
                self.fallback.find_similar(url, window, k, now_unix).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_date_computes_iso() {
        let w = FreshnessWindow {
            recency_minutes: Some(10_080),
            bucket: "fast",
        };
        // 2026-08-01T00:00:00Z = epoch day 20666
        let now = 20_666_i64 * 86_400;
        assert_eq!(w.start_date(now).unwrap(), "2026-07-25");
        assert_eq!(w.end_date(now), "2026-08-01");
    }

    #[test]
    fn evergreen_has_no_start() {
        let w = FreshnessWindow {
            recency_minutes: None,
            bucket: "evergreen",
        };
        assert!(w.start_date(1_785_484_800).is_none());
        assert!(w.is_evergreen());
    }

    #[test]
    fn epoch_day_to_iso_known_dates() {
        assert_eq!(epoch_day_to_iso(0), "1970-01-01");
        assert_eq!(epoch_day_to_iso(19_723), "2024-01-01");
        assert_eq!(epoch_day_to_iso(20_662), "2026-07-28");
    }

    #[test]
    fn empty_key_slots_are_dropped() {
        let p = ExaSearchProvider::new_with_keys(vec!["".into(), "  ".into(), "k1".into()]);
        assert_eq!(p.key_count(), 1);
    }

    #[test]
    fn exhausted_status_recognition() {
        let mut body = String::new();
        assert!(is_key_exhausted(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            &body
        ));
        assert!(is_key_exhausted(
            reqwest::StatusCode::PAYMENT_REQUIRED,
            &body
        ));
        body = r#"{"error":"You have exceeded your credits limit. Please top up"}"#.to_string();
        assert!(is_key_exhausted(reqwest::StatusCode::BAD_REQUEST, &body));
        assert!(!is_key_exhausted(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"invalid numResults"}"#
        ));
    }

    #[test]
    fn all_keys_cooling_park_and_recover() {
        let p = ExaSearchProvider::new_with_keys(vec!["a".into(), "b".into()]);
        let now = 1_785_484_800;
        assert_eq!(p.healthy_keys(now).len(), 2);
        p.park_key("a", now);
        assert_eq!(p.healthy_keys(now), vec!["b".to_string()]);
        assert_eq!(p.parked_key_count(), 1);
        // After the cooldown the key returns to the pool.
        assert_eq!(p.healthy_keys(now + KEY_COOLDOWN_SECS).len(), 2);
    }
}
