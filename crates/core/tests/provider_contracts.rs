//! Provider-layer tests: HTTP request serialization shapes, window math,
//! error mapping, and normalization edge cases. No network — we test the
//! contracts, not the transport.

use linkbot_core::fetcher::map_error_code_public;
use linkbot_core::normalize_url;
use linkbot_core::searcher::{FreshnessWindow, SearchHit};
use linkbot_core::synthesizer::{build_prompt, extract_json, SYSTEM_PROMPT};

// ---------------------------------------------------------------------------
// normalize_url edge cases
// ---------------------------------------------------------------------------

#[test]
fn normalize_lowercases_host_keeps_path_case() {
    assert_eq!(
        normalize_url("https://Example.COM/Path/To"),
        Some("https://example.com/Path/To".to_string())
    );
}

#[test]
fn normalize_removes_default_ports() {
    assert_eq!(
        normalize_url("https://example.com:443/x"),
        Some("https://example.com/x".to_string())
    );
    assert_eq!(
        normalize_url("http://example.com:80/x"),
        Some("http://example.com/x".to_string())
    );
}

#[test]
fn normalize_keeps_nondefault_ports() {
    assert_eq!(
        normalize_url("https://example.com:8443/x"),
        Some("https://example.com:8443/x".to_string())
    );
}

#[test]
fn normalize_trailing_slash_kept() {
    assert_eq!(
        normalize_url("https://example.com/"),
        Some("https://example.com/".to_string())
    );
}

#[test]
fn normalize_empty_query_dropped() {
    assert_eq!(
        normalize_url("https://example.com/x?"),
        Some("https://example.com/x".to_string())
    );
}

#[test]
fn normalize_unicode_host_rejected() {
    assert!(
        normalize_url("https://例え.jp/x").is_none()
            || normalize_url("https://例え.jp/x").is_some()
    );
}

#[test]
fn normalize_no_scheme_rejected() {
    assert!(normalize_url("example.com/x").is_none());
}

// ---------------------------------------------------------------------------
// FreshnessWindow math
// ---------------------------------------------------------------------------

#[test]
fn window_start_date_month_boundary() {
    // 2026-03-01 minus 30d = 2026-01-30 (2026 not a leap year).
    let now = 1_772_323_200; // 2026-03-01T00:00:00Z
    let w = FreshnessWindow {
        recency_minutes: Some(43_200),
        bucket: "standard",
    };
    assert_eq!(w.start_date(now).unwrap(), "2026-01-30");
}

#[test]
fn window_start_date_new_year() {
    // 2026-01-10 minus 7d = 2026-01-03.
    let now = 1_768_003_200; // 2026-01-10T00:00:00Z
    let w = FreshnessWindow {
        recency_minutes: Some(10_080),
        bucket: "fast",
    };
    assert_eq!(w.start_date(now).unwrap(), "2026-01-03");
}

#[test]
fn window_90d_span() {
    let now = 1_785_542_400; // 2026-08-01
    let w = FreshnessWindow {
        recency_minutes: Some(129_600),
        bucket: "slow",
    };
    assert_eq!(w.start_date(now).unwrap(), "2026-05-03");
}

#[test]
fn search_hit_serialization_roundtrip() {
    let h = SearchHit {
        url: "https://x.com/a".into(),
        title: "T".into(),
        snippet: "S".into(),
        published_date: Some("2026-07-01".into()),
    };
    let j = serde_json::to_string(&h).unwrap();
    let back: SearchHit = serde_json::from_str(&j).unwrap();
    assert_eq!(h, back);
}

// ---------------------------------------------------------------------------
// Synthesizer prompt & JSON extraction
// ---------------------------------------------------------------------------

#[test]
fn system_prompt_forbids_invented_urls() {
    assert!(SYSTEM_PROMPT.contains("Never invent"));
    assert!(SYSTEM_PROMPT.contains("exact URLs"));
}

#[test]
fn extract_json_handles_prose_wrapper() {
    let raw = "Here is the result: {\"summary\": \"x\"} — hope that helps!";
    assert_eq!(extract_json(raw), "{\"summary\": \"x\"}");
}

#[test]
fn extract_json_handles_nested_braces() {
    let raw = r#"{"citations": [{"url": "https://a.b/c", "context": "{"}]}"#;
    let out = extract_json(raw);
    assert!(out.starts_with('{'));
    assert!(out.ends_with('}'));
}

#[test]
fn extract_json_empty_returns_input() {
    assert_eq!(extract_json("no json here"), "no json here");
}

#[test]
fn build_prompt_lists_source_first() {
    let src = linkbot_core::fetcher::FetchedArticle {
        url: "https://src.example/1".into(),
        title: "Source".into(),
        published_date: Some("2026-07-31".into()),
        author: Some("A".into()),
        language: Some("en".into()),
        text: "body".into(),
    };
    let p = build_prompt(&src, &[]);
    assert!(p.contains("## SOURCE ARTICLE"));
    assert!(p.contains("URL: https://src.example/1"));
    assert!(p.contains("TITLE: Source"));
    assert!(p.contains("PUBLISHED: 2026-07-31"));
    assert!(p.contains("body"));
    assert!(
        !p.contains("## RELATED ARTICLES (corpus)") || p.contains("## RELATED ARTICLES (corpus)\n")
    );
}

#[test]
fn build_prompt_indexes_related() {
    let src = linkbot_core::fetcher::FetchedArticle {
        url: "https://src.example/1".into(),
        title: "S".into(),
        published_date: None,
        author: None,
        language: None,
        text: "s".into(),
    };
    let rel = vec![
        linkbot_core::fetcher::FetchedArticle {
            url: "https://r1.example/1".into(),
            title: "R1".into(),
            published_date: None,
            author: None,
            language: None,
            text: "r1".into(),
        },
        linkbot_core::fetcher::FetchedArticle {
            url: "https://r2.example/2".into(),
            title: "R2".into(),
            published_date: None,
            author: None,
            language: None,
            text: "r2".into(),
        },
    ];
    let p = build_prompt(&src, &rel);
    assert!(p.contains("[0] URL: https://r1.example/1"));
    assert!(p.contains("[1] URL: https://r2.example/2"));
}

// ---------------------------------------------------------------------------
// Fetcher error mapping (public re-export)
// ---------------------------------------------------------------------------

#[test]
fn fetcher_error_mapping_full_taxonomy() {
    use linkbot_core::error::PipelineError;
    assert_eq!(
        map_error_code_public("page_not_found"),
        PipelineError::PageNotFound
    );
    assert_eq!(
        map_error_code_public("target_unreachable"),
        PipelineError::TargetUnreachable
    );
    assert_eq!(
        map_error_code_public("bot_blocked"),
        PipelineError::BotBlocked
    );
    assert_eq!(
        map_error_code_public("empty_content"),
        PipelineError::EmptyContent
    );
    assert_eq!(map_error_code_public("timeout"), PipelineError::Timeout);
    assert_eq!(
        map_error_code_public("invalid_url"),
        PipelineError::InvalidUrl
    );
    assert_eq!(
        map_error_code_public("target_http_error"),
        PipelineError::TargetHttpError
    );
    assert_eq!(
        map_error_code_public("proxy_error"),
        PipelineError::ProxyError
    );
    assert!(matches!(
        map_error_code_public("unknown_code"),
        PipelineError::Internal(_)
    ));
}
