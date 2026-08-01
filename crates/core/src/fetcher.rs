//! TinyFish Fetch client — the *only* way this bot retrieves URLs.
//!
//! Security property (documented in README): the bot never fetches a raw URL
//! itself. TinyFish rejects private IPs / localhost / metadata endpoints, so
//! SSRF is the provider's problem, not ours.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::PipelineError;
use crate::normalize_url;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FetchedArticle {
    pub url: String,
    pub title: String,
    pub published_date: Option<String>,
    pub author: Option<String>,
    pub language: Option<String>,
    pub text: String,
}

#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Fetch one URL → clean markdown + metadata. Errors map to the §10 taxonomy.
    async fn fetch(&self, url: &str) -> Result<FetchedArticle, PipelineError>;
}

// ---------------------------------------------------------------------------
// Real TinyFish implementation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TinyFishFetcher {
    client: reqwest::Client,
    api_key: String,
}

#[derive(Serialize)]
struct FetchRequest {
    urls: Vec<String>,
    format: String,
    ttl: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    per_url_timeout_ms: Option<u32>,
}

#[derive(Deserialize)]
struct FetchResponse {
    #[serde(default)]
    results: Vec<FetchResult>,
}

#[derive(Deserialize)]
struct FetchResult {
    url: String,
    #[serde(default)]
    error_code: Option<String>,
    title: Option<String>,
    published_date: Option<String>,
    author: Option<String>,
    language: Option<String>,
    text: Option<String>,
}

impl TinyFishFetcher {
    pub fn new(api_key: String) -> Self {
        TinyFishFetcher {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(50))
                .build()
                .expect("reqwest client"),
            api_key,
        }
    }

    async fn fetch_one(&self, url: &str, mut retry: bool) -> Result<FetchedArticle, PipelineError> {
        loop {
            let req = FetchRequest {
                urls: vec![url.to_string()],
                format: "markdown".into(),
                ttl: 3600,
                per_url_timeout_ms: Some(45_000),
            };
            let resp = self
                .client
                .post("https://api.fetch.tinyfish.ai")
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&req)
                .send()
                .await
                .map_err(|e| PipelineError::Internal(format!("tinyfish fetch transport: {e}")))?;
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(PipelineError::Internal("tinyfish fetch 429".into()));
            }
            if !status.is_success() {
                return Err(PipelineError::Internal(format!(
                    "tinyfish fetch http {status}"
                )));
            }
            let body: FetchResponse = resp
                .json()
                .await
                .map_err(|e| PipelineError::Internal(format!("tinyfish fetch decode: {e}")))?;
            let mut results = body.results.into_iter();
            let result = results.find(|r| r.url == url).or_else(|| results.next());

            let Some(result) = result else {
                return Err(PipelineError::Internal(
                    "tinyfish fetch: no result for url".into(),
                ));
            };

            if let Some(code) = result.error_code {
                let err = map_error_code_public(&code);
                // Transient → one retry with backoff.
                if matches!(
                    err,
                    PipelineError::Timeout | PipelineError::ProxyError | PipelineError::Internal(_)
                ) && retry
                {
                    retry = false;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    continue;
                }
                return Err(err);
            }

            let text = result.text.unwrap_or_default();
            if text.trim().is_empty() {
                return Err(PipelineError::EmptyContent);
            }
            return Ok(FetchedArticle {
                url: normalize_url(url).unwrap_or_else(|| url.to_string()),
                title: result.title.unwrap_or_default(),
                published_date: result.published_date,
                author: result.author,
                language: result.language,
                text,
            });
        }
    }
}

/// Map TinyFish per-URL error codes to the pipeline error taxonomy (§5
/// Stage 1 / §10). Public so integration tests can pin the contract.
pub fn map_error_code_public(code: &str) -> PipelineError {
    match code {
        "page_not_found" => PipelineError::PageNotFound,
        "target_unreachable" => PipelineError::TargetUnreachable,
        "bot_blocked" => PipelineError::BotBlocked,
        "empty_content" => PipelineError::EmptyContent,
        "timeout" => PipelineError::Timeout,
        "invalid_url" => PipelineError::InvalidUrl,
        "target_http_error" => PipelineError::TargetHttpError,
        "proxy_error" => PipelineError::ProxyError,
        _ => PipelineError::Internal(format!("tinyfish error_code {code}")),
    }
}

#[async_trait]
impl Fetcher for TinyFishFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedArticle, PipelineError> {
        self.fetch_one(url, true).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_mapping_covers_taxonomy() {
        assert_eq!(
            map_error_code_public("page_not_found"),
            PipelineError::PageNotFound
        );
        assert_eq!(map_error_code_public("bot_blocked"), PipelineError::BotBlocked);
        assert_eq!(map_error_code_public("timeout"), PipelineError::Timeout);
        assert_eq!(map_error_code_public("proxy_error"), PipelineError::ProxyError);
        assert_eq!(map_error_code_public("invalid_url"), PipelineError::InvalidUrl);
        assert_eq!(
            map_error_code_public("target_http_error"),
            PipelineError::TargetHttpError
        );
        assert_eq!(
            map_error_code_public("target_unreachable"),
            PipelineError::TargetUnreachable
        );
        assert_eq!(map_error_code_public("empty_content"), PipelineError::EmptyContent);
        assert!(matches!(
            map_error_code_public("weird"),
            PipelineError::Internal(_)
        ));
    }

    #[tokio::test]
    async fn fetch_requires_content() {
        // No network in unit tests — the mock lives in mock_providers.rs;
        // here we only verify the empty-content rule via the error mapper.
        assert!(matches!(
            map_error_code_public("empty_content"),
            PipelineError::EmptyContent
        ));
    }
}
