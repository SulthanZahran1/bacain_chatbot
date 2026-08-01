//! linkbot-core — the pure, Discord-free pipeline behind the bacain chatbot.
//!
//! The bot crate (frontend) talks to this crate through exactly one async entry
//! point: [`pipeline::analyze`]. Everything in here is unit-testable without a
//! network or a Discord connection (providers are trait-based and mocked in
//! tests, the clock is injectable).

pub mod cache;
pub mod citations;
pub mod classifier;
pub mod clock;
pub mod config;
pub mod domain_speed;
pub mod error;
pub mod fetcher;
pub mod mock_providers;
pub mod optimizer;
pub mod optimizer_policy;
pub mod pipeline;
pub mod reader;
pub mod scenario;
pub mod searcher;
pub mod synthesizer;

pub use config::Config;
pub use error::{PipelineError, UserMessage};
pub use pipeline::{analyze, Analysis, AnalysisMeta, AnalysisRequest, ChannelCtx, Deps};
pub use searcher::{FreshnessWindow, SearchHit, SearchProvider};

/// Normalized URL — canonical form used for dedupe, caching, and pool membership.
pub fn normalize_url(raw: &str) -> Option<String> {
    let s = raw.trim();
    // Strip Discord's angle-bracket wrappers: <https://…>
    let s = s
        .strip_prefix('<')
        .and_then(|r| r.strip_suffix('>'))
        .unwrap_or(s);
    let parsed = url::Url::parse(s).ok()?;
    let mut out = String::new();
    out.push_str(parsed.scheme());
    out.push_str("://");
    if let Some(host) = parsed.host_str() {
        out.push_str(&host.to_lowercase());
    }
    if let Some(port) = parsed.port() {
        out.push_str(&format!(":{port}"));
    }
    out.push_str(parsed.path());
    // Drop fragment; keep query only for non-trivial cases (e.g. ?q= matters on some sites).
    match parsed.query() {
        Some(q) if !q.is_empty() => {
            out.push('?');
            out.push_str(q);
        }
        _ => {}
    }
    Some(out)
}

pub fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_discord_wrappers_and_fragments() {
        let n = normalize_url("<https://Example.com/Path#frag>").unwrap();
        assert_eq!(n, "https://example.com/Path");
    }

    #[test]
    fn normalize_keeps_query() {
        let n = normalize_url("https://x.com/a?b=1").unwrap();
        assert_eq!(n, "https://x.com/a?b=1");
    }

    #[test]
    fn normalize_rejects_garbage() {
        assert!(normalize_url("not a url").is_none());
    }

    #[test]
    fn sha256_is_hex_and_stable() {
        assert_eq!(sha256_hex("x"), sha256_hex("x"));
        assert_eq!(sha256_hex("x").len(), 64);
    }
}
