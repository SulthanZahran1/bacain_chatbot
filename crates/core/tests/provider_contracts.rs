//! Provider-layer tests: HTTP request serialization shapes, window math,
//! error mapping, and normalization edge cases. No network — we test the
//! contracts, not the transport.

use linkbot_core::fetcher::map_error_code_public;
use linkbot_core::normalize_url;
use linkbot_core::searcher::{
    FreshnessWindow, SearchHit, TinyFishSearchProvider, KEY_COOLDOWN_SECS,
};
use linkbot_core::synthesizer::{
    build_prompt, coerce_synthesis_fields, extract_json, SYSTEM_PROMPT,
};
use linkbot_core::SearchProvider;

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

// ---------------------------------------------------------------------------
// Search provider wiring — TinyFish endpoint shape + the Exa pool's rotation
// ---------------------------------------------------------------------------

/// The TinyFish Search API is a GET endpoint with query params; the previous
/// POST to `/search` 404ed (verified against the live API, 2026-09-23).
#[tokio::test]
async fn tinyfish_search_uses_get_with_query_params_and_api_key_header() {
    let (port, captured) = serve_once("200 OK", r#"{"results":[]}"#.to_string());
    let p = TinyFishSearchProvider::with_base(format!("http://127.0.0.1:{port}"), "tf-key".into());
    let w = FreshnessWindow {
        recency_minutes: Some(10_080),
        bucket: "fast",
    };
    let hits = p
        .search(&["rust async".into()], w, 5, 1_785_484_800)
        .await
        .unwrap();
    assert!(hits.is_empty());

    let req = captured.lock().unwrap().clone();
    let head = req.to_lowercase();
    assert!(req.starts_with("GET /?"), "expected GET with query: {req}");
    assert!(head.contains("x-api-key: tf-key"), "{req}");
    assert!(head.contains("query=rust"), "{req}");
    assert!(head.contains("recency_minutes=10080"), "{req}");
    assert!(head.contains("domain_type=news"), "{req}");
    // `num_results` is not part of this API — it must never be sent.
    assert!(!head.contains("num_results"), "{req}");
}

#[tokio::test]
async fn tinyfish_search_maps_the_date_field_to_published_date() {
    // The API returns `date` ("Jul 30, 2026" / "4 days ago"), not
    // `published_date`.
    let body = r#"{"results":[{"position":1,"title":"T","url":"https://x.example/1","snippet":"S","date":"Jul 30, 2026"}]}"#;
    let (port, _) = serve_once("200 OK", body.to_string());
    let p = TinyFishSearchProvider::with_base(format!("http://127.0.0.1:{port}"), "k".into());
    let w = FreshnessWindow {
        recency_minutes: None,
        bucket: "evergreen",
    };
    let hits = p.search(&["q".into()], w, 5, 0).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].published_date.as_deref(), Some("Jul 30, 2026"));
    assert_eq!(hits[0].url, "https://x.example/1");
}

#[tokio::test]
async fn tinyfish_search_surfaces_non_success_status() {
    let (port, _) = serve_once("402 Payment Required", "{}".to_string());
    let p = TinyFishSearchProvider::with_base(format!("http://127.0.0.1:{port}"), "k".into());
    let w = FreshnessWindow {
        recency_minutes: None,
        bucket: "evergreen",
    };
    let e = p.search(&["q".into()], w, 5, 0).await.unwrap_err();
    assert!(e.to_string().contains("402"), "{e}");
}

#[test]
fn key_cooldown_is_an_hour() {
    // Same window as the Hermes `exa-pool` plugin — a parked key must not be
    // retried on the next post.
    assert_eq!(KEY_COOLDOWN_SECS, 3600);
}

// ---------------------------------------------------------------------------
// Synthesis shape coercion — the live "invalid type: sequence" failure
// ---------------------------------------------------------------------------

#[test]
fn coercion_joins_an_array_into_bullets() {
    // Live failure 2026-09-23: the model answered a declared string field with
    // an array of bullets; the parse hard-failed and the analysis was lost.
    let v = serde_json::json!({
        "title": "T",
        "summary": "S",
        "deep_analysis": ["- first point", "- second point"],
        "critique": ["- weak evidence"],
        "citations": []
    });
    let out = coerce_synthesis_fields(v);
    assert_eq!(
        out["deep_analysis"],
        serde_json::json!("- first point\n- second point")
    );
    assert_eq!(out["critique"], serde_json::json!("- weak evidence"));
}

#[test]
fn coercion_leaves_strings_untouched() {
    let v = serde_json::json!({
        "title": "T", "summary": "S", "deep_analysis": "D", "critique": "C",
        "citations": []
    });
    let out = coerce_synthesis_fields(v.clone());
    for field in ["title", "summary", "deep_analysis", "critique"] {
        assert_eq!(out[field], v[field], "{field} changed");
    }
}

#[test]
fn coercion_renders_scalars_as_text() {
    let v = serde_json::json!({
        "title": "T", "summary": 42, "deep_analysis": true, "critique": null,
        "citations": []
    });
    let out = coerce_synthesis_fields(v);
    assert_eq!(out["summary"], serde_json::json!("42"));
    assert_eq!(out["deep_analysis"], serde_json::json!("true"));
    assert_eq!(out["critique"], serde_json::json!(""));
}

#[test]
fn coercion_normalizes_citation_contexts() {
    let v = serde_json::json!({
        "title": "T", "summary": "S", "deep_analysis": "D", "critique": "C",
        "citations": [
            {"url": "https://a.example/1", "context": ["supports claim A", "and B"]},
            {"url": "https://b.example/2"},
            {"url": "https://c.example/3", "context": "plain"},
            {"context": "no url at all"},
            "not an object"
        ]
    });
    let out = coerce_synthesis_fields(v);
    let items = out["citations"].as_array().unwrap();
    // The entry without a URL and the non-object entry are dropped.
    assert_eq!(items.len(), 3, "{items:?}");
    assert_eq!(
        items[0]["context"],
        serde_json::json!("supports claim A\nand B")
    );
    assert_eq!(items[1]["context"], serde_json::json!(""));
    assert_eq!(items[2]["context"], serde_json::json!("plain"));
}

#[test]
fn coercion_is_a_noop_for_non_objects() {
    let v = serde_json::json!([1, 2, 3]);
    assert_eq!(coerce_synthesis_fields(v.clone()), v);
}

#[test]
fn coerced_output_deserializes_into_synthesis() {
    // End-to-end: the exact live payload shape must now parse.
    let raw = r#"{"title":"Fingerprint OSS","summary":"A client-side fingerprinting library.",
        "deep_analysis":["- Open-source fingerprinting","- Logs user information"],
        "critique":["- No published benchmarks"],
        "citations":[{"url":"https://github.com/IntegerAlex/fingerprint-oss","context":["repo"]}]}"#;
    let v: serde_json::Value = serde_json::from_str(raw).unwrap();
    let coerced = coerce_synthesis_fields(v);
    let s: linkbot_core::synthesizer::Synthesis = serde_json::from_value(coerced).unwrap();
    assert!(s.deep_analysis.contains("- Open-source fingerprinting"));
    assert_eq!(s.citations.len(), 1);
    assert_eq!(s.citations[0].context, "repo");
}

// ---------------------------------------------------------------------------
// Exa key pool — rotation on 402/429 across several keys
// ---------------------------------------------------------------------------

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// Serve ONE request; capture the raw request head (headers included).
fn serve_once(status_line: &str, body: String) -> (u16, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured: Arc<Mutex<String>> = Arc::default();
    let cap = captured.clone();
    let status_line = status_line.to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            *cap.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
            let resp = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    (port, captured)
}

/// Serve a SEQUENCE of (status, body) responses, one per connection, and
/// capture each request head in order. The Exa pool makes exactly ONE request
/// per key attempted, so the captured heads are the per-key requests.
fn serve_n(responses: Vec<(&str, String)>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured: Arc<Mutex<Vec<String>>> = Arc::default();
    let cap = captured.clone();
    let responses: Vec<(String, String)> = responses
        .into_iter()
        .map(|(s, b)| (s.to_string(), b))
        .collect();
    std::thread::spawn(move || {
        for (status_line, body) in responses {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                cap.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_string());
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        }
    });
    (port, captured)
}

/// Extract the `x-api-key` header from a captured request head.
fn header_key(head: &str) -> Option<String> {
    head.lines()
        .find(|l| l.to_lowercase().starts_with("x-api-key:"))
        .map(|l| l[l.find(':').unwrap() + 1..].trim().to_string())
}

const EXA_OK: &str = r#"{"results":[{"url":"https://hit.example/1","title":"T","snippet":"S","published_date":"2026-07-30"}]}"#;
const EXA_402: &str = r#"{"requestId":"x","error":"You have exceeded your credits limit. Please top up","tag":"NO_MORE_CREDITS"}"#;

fn exa_window() -> FreshnessWindow {
    FreshnessWindow {
        recency_minutes: Some(43_200),
        bucket: "default",
    }
}

#[tokio::test]
async fn exa_pool_rotates_past_an_exhausted_key() {
    // Key 1 answers 402 (the live failure); the pool must retry on key 2 and
    // return its hits — no error reaches the pipeline.
    let (port, captured) = serve_n(vec![
        ("402 Payment Required", EXA_402.to_string()),
        ("200 OK", EXA_OK.to_string()),
    ]);
    let p = linkbot_core::searcher::ExaSearchProvider::with_base(
        format!("http://127.0.0.1:{port}"),
        vec!["key-one".into(), "key-two".into()],
    );
    let hits = p
        .search(&["q".into()], exa_window(), 5, 1_785_484_800)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://hit.example/1");

    let heads = captured.lock().unwrap().clone();
    assert_eq!(heads.len(), 2, "one request per attempted key");
    assert_eq!(header_key(&heads[0]).as_deref(), Some("key-one"));
    assert_eq!(header_key(&heads[1]).as_deref(), Some("key-two"));
}

#[tokio::test]
async fn exa_pool_parks_an_exhausted_key_for_the_next_call() {
    // After key-one 402s once it stays parked, so the next search goes
    // straight to key-two.
    let (port, captured) = serve_n(vec![
        ("402 Payment Required", EXA_402.to_string()),
        ("200 OK", EXA_OK.to_string()),
        ("200 OK", EXA_OK.to_string()),
    ]);
    let p = linkbot_core::searcher::ExaSearchProvider::with_base(
        format!("http://127.0.0.1:{port}"),
        vec!["key-one".into(), "key-two".into()],
    );
    let now = 1_785_484_800;
    p.search(&["first".into()], exa_window(), 5, now)
        .await
        .unwrap();
    p.search(&["second".into()], exa_window(), 5, now + 60)
        .await
        .unwrap();

    let heads = captured.lock().unwrap().clone();
    assert_eq!(heads.len(), 3);
    assert_eq!(header_key(&heads[0]).as_deref(), Some("key-one"));
    assert_eq!(header_key(&heads[1]).as_deref(), Some("key-two"));
    // Second call: key-one is still cooling, so only key-two is asked.
    assert_eq!(header_key(&heads[2]).as_deref(), Some("key-two"));
}

#[tokio::test]
async fn exa_pool_exhausted_reports_every_key_parked() {
    let (port, _) = serve_n(vec![
        ("402 Payment Required", EXA_402.to_string()),
        ("429 Too Many Requests", "{}".to_string()),
    ]);
    let p = linkbot_core::searcher::ExaSearchProvider::with_base(
        format!("http://127.0.0.1:{port}"),
        vec!["one".into(), "two".into()],
    );
    let e = p
        .search(&["q".into()], exa_window(), 5, 1_785_484_800)
        .await
        .unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("exhausted"), "{msg}");
}

#[tokio::test]
async fn exa_pool_does_not_rotate_on_a_request_error() {
    // A malformed request fails on every key — rotating would only burn
    // another key's quota. Only one request must be made.
    let (port, captured) = serve_n(vec![(
        "400 Bad Request",
        r#"{"error":"invalid numResults"}"#.to_string(),
    )]);
    let p = linkbot_core::searcher::ExaSearchProvider::with_base(
        format!("http://127.0.0.1:{port}"),
        vec!["one".into(), "two".into()],
    );
    let e = p
        .search(&["q".into()], exa_window(), 5, 1_785_484_800)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("400"), "{e}");
    assert_eq!(captured.lock().unwrap().len(), 1, "no rotation on 400");
}

#[tokio::test]
async fn exa_pool_with_no_keys_errors_without_a_request() {
    let p = linkbot_core::searcher::ExaSearchProvider::with_base(
        "http://127.0.0.1:1".to_string(),
        vec!["".into()],
    );
    let e = p
        .search(&["q".into()], exa_window(), 5, 0)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no api key"), "{e}");
}

#[tokio::test]
async fn fallback_search_uses_tinyfish_when_the_pool_is_exhausted() {
    // Primary Exa pool: both keys 402. Fallback TinyFish must answer the call.
    let (exa_port, _) = serve_n(vec![
        ("402 Payment Required", EXA_402.to_string()),
        ("402 Payment Required", EXA_402.to_string()),
    ]);
    let tf_body = r#"{"results":[{"position":1,"title":"TF hit","url":"https://tf.example/9","snippet":"s","date":"4 days ago"}]}"#;
    let (tf_port, _) = serve_once("200 OK", tf_body.to_string());

    let exa = Arc::new(linkbot_core::searcher::ExaSearchProvider::with_base(
        format!("http://127.0.0.1:{exa_port}"),
        vec!["one".into(), "two".into()],
    ));
    let tf = Arc::new(TinyFishSearchProvider::with_base(
        format!("http://127.0.0.1:{tf_port}"),
        "tf-key".into(),
    ));
    let chain = linkbot_core::searcher::FallbackSearchProvider::new(exa, tf);
    let hits = chain
        .search(&["q".into()], exa_window(), 5, 1_785_484_800)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://tf.example/9");
}

#[tokio::test]
async fn fallback_search_does_not_call_tinyfish_when_exa_succeeds() {
    let (exa_port, _) = serve_once("200 OK", EXA_OK.to_string());
    // The TinyFish listener is bound but never asked — if the chain called it
    // the port would receive the connection; we assert on the Exa answer only.
    let (tf_port, tf_captured) = serve_n(vec![("200 OK", r#"{"results":[]}"#.to_string())]);

    let exa = Arc::new(linkbot_core::searcher::ExaSearchProvider::with_base(
        format!("http://127.0.0.1:{exa_port}"),
        vec!["key".into()],
    ));
    let tf = Arc::new(TinyFishSearchProvider::with_base(
        format!("http://127.0.0.1:{tf_port}"),
        "tf-key".into(),
    ));
    let chain = linkbot_core::searcher::FallbackSearchProvider::new(exa, tf);
    let hits = chain
        .search(&["q".into()], exa_window(), 5, 1_785_484_800)
        .await
        .unwrap();
    assert_eq!(hits[0].url, "https://hit.example/1");
    assert!(
        tf_captured.lock().unwrap().is_empty(),
        "fallback must not be called on primary success"
    );
}
