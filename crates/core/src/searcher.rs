//! Search providers behind one trait: Exa (default) and TinyFish Search
//! (fallback). Mock implementation lives in `mock_providers.rs`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::PipelineError;

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

// ---------------------------------------------------------------------------
// Exa — default implementation (POST https://api.exa.ai/search)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ExaSearchProvider {
    client: reqwest::Client,
    api_key: String,
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
    include_text: Option<String>, // "snippet" is default; keep minimal
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

impl ExaSearchProvider {
    pub fn new(api_key: String) -> Self {
        ExaSearchProvider {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
            api_key,
        }
    }

    async fn run(
        &self,
        query: String,
        window: FreshnessWindow,
        k: usize,
        now_unix: i64,
        exclude_domains: Option<Vec<String>>,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        let category = matches!(window.bucket, "fast" | "breaking").then(|| "news".to_string());
        let req = ExaRequest {
            query,
            start_published_date: window.start_date(now_unix),
            end_published_date: window.end_date(now_unix),
            category,
            num_results: k,
            type_: "auto".into(),
            include_text: Some("snippet".into()),
            exclude_domains,
        };
        let resp = self
            .client
            .post("https://api.exa.ai/search")
            .header("x-api-key", &self.api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| PipelineError::SearchFailed(format!("exa transport: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(PipelineError::SearchFailed("exa 429 (rate limited)".into()));
        }
        if !status.is_success() {
            return Err(PipelineError::SearchFailed(format!("exa http {status}")));
        }
        let body: ExaResponse = resp
            .json()
            .await
            .map_err(|e| PipelineError::SearchFailed(format!("exa decode: {e}")))?;
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
        let mut all = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for q in queries {
            let hits = self.run(q.clone(), window, k, now_unix, None).await?;
            for h in hits {
                if seen.insert(h.url.clone()) {
                    all.push(h);
                }
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
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TinyFishSearchProvider {
    client: reqwest::Client,
    api_key: String,
}

#[derive(Serialize)]
struct TfSearchRequest {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    recency_minutes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain_type: Option<String>,
    num_results: usize,
}

#[derive(Deserialize)]
struct TfSearchResponse {
    results: Vec<TfSearchResult>,
}

#[derive(Deserialize)]
struct TfSearchResult {
    url: String,
    title: Option<String>,
    snippet: Option<String>,
    published_date: Option<String>,
}

impl TinyFishSearchProvider {
    pub fn new(api_key: String) -> Self {
        TinyFishSearchProvider {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
            api_key,
        }
    }

    async fn run(
        &self,
        query: String,
        window: FreshnessWindow,
        k: usize,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        let req = TfSearchRequest {
            query,
            recency_minutes: window.recency_minutes,
            domain_type: matches!(window.bucket, "fast" | "breaking").then(|| "news".to_string()),
            num_results: k,
        };
        let resp = self
            .client
            .post("https://api.search.tinyfish.ai/search")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&req)
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
            .map(|r| SearchHit {
                url: r.url,
                title: r.title.unwrap_or_default(),
                snippet: r.snippet.unwrap_or_default(),
                published_date: r.published_date,
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
        let mut all = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for q in queries {
            let hits = self.run(q.clone(), window, k).await?;
            for h in hits {
                if seen.insert(h.url.clone()) {
                    all.push(h);
                }
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
        self.run(format!("find similar to {url}"), window, k).await
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
}
