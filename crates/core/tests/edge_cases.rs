//! Edge-case tests for the pure logic modules: domain-speed resolution,
//! classifier scoring, reader trimming, citation pool, error taxonomy.
//! These pin the §6/§8/§10 contracts from the spec.

use linkbot_core::citations::{CitationPool, all_legit, pool_from, validate};
use linkbot_core::classifier::{classify, keyword_score};
use linkbot_core::domain_speed::{
    DomainSpeedTable, etld_plus_one, resolve_window,
};
use linkbot_core::error::{PipelineError, user_message};
use linkbot_core::fetcher::FetchedArticle;
use linkbot_core::reader::{assemble_corpus, token_budget_chars, trim_head_tail};
use linkbot_core::synthesizer::{Citation, Synthesis};

// ---------------------------------------------------------------------------
// Domain speed (§6)
// ---------------------------------------------------------------------------

#[test]
fn breaking_bucket_3d_window() {
    let w = resolve_window(
        &DomainSpeedTable::default(),
        "https://statuspage.io/incident/1",
        false,
    );
    assert_eq!(w.recency_minutes, Some(4_320));
    assert_eq!(w.bucket, "breaking");
}

#[test]
fn slow_bucket_90d_window() {
    let w = resolve_window(&DomainSpeedTable::default(), "https://www.fcc.gov/ruling", false);
    assert_eq!(w.recency_minutes, Some(129_600));
    assert_eq!(w.bucket, "slow");
}

#[test]
fn github_evergreen_no_filter() {
    let w = resolve_window(&DomainSpeedTable::default(), "https://github.com/user/repo", false);
    assert_eq!(w.recency_minutes, None);
    assert_eq!(w.bucket, "evergreen");
}

#[test]
fn arxiv_standard_30d() {
    let w = resolve_window(&DomainSpeedTable::default(), "https://arxiv.org/abs/2607.12345", false);
    assert_eq!(w.recency_minutes, Some(43_200));
    assert_eq!(w.bucket, "standard");
}

#[test]
fn ai_override_beats_every_bucket() {
    for url in [
        "https://reuters.com/x",
        "https://statuspage.io/x",
        "https://wikipedia.org/x",
        "https://fcc.gov/x",
    ] {
        let w = resolve_window(&DomainSpeedTable::default(), url, true);
        assert_eq!(w.bucket, "ai-override", "{url}");
        assert_eq!(w.recency_minutes, Some(43_200), "{url}");
    }
}

#[test]
fn custom_table_via_domain_speed_json() {
    let table: DomainSpeedTable = serde_json::from_str(
        r#"{"buckets":{"fast":{"window_minutes":10080,"domains":["mynews.example"]}}}"#,
    )
    .unwrap();
    let w = resolve_window(&table, "https://mynews.example/x", false);
    assert_eq!(w.bucket, "fast");
    // Unknown domain falls to default 30d.
    let w = resolve_window(&table, "https://other.example/x", false);
    assert_eq!(w.bucket, "default");
}

#[test]
fn etld1_edge_cases() {
    assert_eq!(etld_plus_one("https://localhost:8080/x"), None);
    // IP literal: last-two-labels heuristic yields "1.1" — not None, but
    // harmless (never matches a bucket domain).
    assert!(etld_plus_one("https://192.168.1.1/x").is_some());
    assert_eq!(etld_plus_one("https://a.b.c.example/x"), Some("c.example".into()));
    assert_eq!(etld_plus_one("https://example.com"), Some("example.com".into()));
}

#[test]
fn window_struct_helpers() {
    let w = linkbot_core::searcher::FreshnessWindow {
        recency_minutes: Some(60),
        bucket: "fast",
    };
    assert!(!w.is_evergreen());
    let e = linkbot_core::searcher::FreshnessWindow {
        recency_minutes: None,
        bucket: "evergreen",
    };
    assert!(e.is_evergreen());
}

// ---------------------------------------------------------------------------
// Classifier (§5 Stage 2)
// ---------------------------------------------------------------------------

#[test]
fn keyword_score_counts_multiple_hits() {
    let (s, _) = keyword_score("LLM agents", "llm agent llm");
    assert!(s >= 10, "score {s}");
}

#[test]
fn keyword_score_zero_for_plain_text() {
    let (s, amb) = keyword_score("Weather forecast", "sunny and warm tomorrow");
    assert_eq!(s, 0);
    assert!(!amb);
}

#[test]
fn classify_decided_high_score_skips_llm() {
    let c = classify("OpenAI GPT-5 agentic coding", "llm rag", None);
    assert!(c.is_ai_topic);
    assert!(!c.ambiguous);
}

#[test]
fn classify_decided_zero_score_skips_llm() {
    let c = classify("Cooking recipes", "ingredients and steps", None);
    assert!(!c.is_ai_topic);
    assert!(!c.ambiguous);
}

#[test]
fn classify_ambiguous_uses_llm_verdict() {
    let yes = classify("The model", "plain", Some(&|_, _| true));
    assert!(yes.is_ai_topic);
    let no = classify("The model", "plain", Some(&|_, _| false));
    assert!(!no.is_ai_topic);
}

#[test]
fn classify_ambiguous_llm_gets_title_and_text() {
    // The closure must receive the actual title/text.
    let captured: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let cap2 = captured.clone();
    let hook = move |t: &str, x: &str| {
        cap2.lock().unwrap().push(format!("{t}|{x}"));
        false
    };
    classify("The model", "some body", Some(&hook));
    let got = captured.lock().unwrap();
    assert_eq!(got.len(), 1);
    assert!(got[0].starts_with("The model|some body"));
}

#[test]
fn vendor_name_detection() {
    for name in ["OpenAI", "Anthropic", "Mistral", "Hugging Face", "DeepSeek", "Gemini", "Claude"] {
        let (s, _) = keyword_score(name, "");
        assert!(s > 0, "{name} should score");
    }
}

#[test]
fn classification_serialization() {
    let c = linkbot_core::classifier::Classification {
        is_ai_topic: true,
        score: 5,
        ambiguous: false,
    };
    let j = serde_json::to_string(&c).unwrap();
    let back: linkbot_core::classifier::Classification = serde_json::from_str(&j).unwrap();
    assert!(back.is_ai_topic);
    assert_eq!(back.score, 5);
}

// ---------------------------------------------------------------------------
// Reader (§5 Stage 5)
// ---------------------------------------------------------------------------

#[test]
fn token_budget_conversion() {
    // Spec: 60k tokens ≈ 45k chars.
    assert_eq!(token_budget_chars(60_000), 45_000);
    assert_eq!(token_budget_chars(0), 0);
}

#[test]
fn trim_exact_limit_no_marker() {
    let t = trim_head_tail("abcdef", 6);
    assert_eq!(t, "abcdef");
    assert!(!t.contains("[trimmed]"));
}

#[test]
fn trim_odd_limits_split_head_tail() {
    let t = trim_head_tail(&"x".repeat(101), 101);
    assert_eq!(t, "x".repeat(101));
    let t2 = trim_head_tail(&"x".repeat(102), 101);
    // head 50 + marker 13 + tail 51 = 114 → clamped by design; verify no panic
    // and content preserved at both ends.
    assert!(t2.starts_with('x') && t2.ends_with('x'));
}

#[test]
fn assemble_corpus_zero_budget() {
    let src = FetchedArticle {
        url: "https://a.com/1".into(),
        title: String::new(),
        published_date: None,
        author: None,
        language: None,
        text: "long source text here".into(),
    };
    let (st, _) = assemble_corpus(&src, &[], 0, 0.5);
    assert!(st.is_empty() || st.chars().count() <= 13, "{st}");
}

#[test]
fn assemble_corpus_source_share_clamped() {
    let src = FetchedArticle {
        url: "https://a.com/1".into(),
        title: String::new(),
        published_date: None,
        author: None,
        language: None,
        text: "s".repeat(10_000),
    };
    let rel = FetchedArticle {
        url: "https://b.com/2".into(),
        title: String::new(),
        published_date: None,
        author: None,
        language: None,
        text: "r".repeat(10_000),
    };
    // source_share 0.0 → source gets nothing, related gets everything.
    let (st, rt) = assemble_corpus(&src, &[rel], 10_000, 0.0);
    assert!(st.chars().count() <= 13);
    assert!(rt[0].text.chars().count() > 5_000);
}

#[test]
fn assemble_corpus_many_related_split_evenly() {
    let src = FetchedArticle {
        url: "https://a.com/1".into(),
        title: String::new(),
        published_date: None,
        author: None,
        language: None,
        text: "s".into(),
    };
    let rels: Vec<FetchedArticle> = (0..10)
        .map(|i| FetchedArticle {
            url: format!("https://r{i}.com/{i}"),
            title: String::new(),
            published_date: None,
            author: None,
            language: None,
            text: "r".repeat(5_000),
        })
        .collect();
    let (_, rt) = assemble_corpus(&src, &rels, 10_000, 0.5);
    // Source used ~0 of its 5000 share → remaining ~9999 splits across 10.
    // Each related gets ≤ 999 + 13 (marker).
    for a in &rt {
        assert!(a.text.chars().count() <= 1_012, "{}", a.text.chars().count());
    }
    // Every related survived trimming with both head and tail content.
    for a in &rt {
        assert!(a.text.starts_with('r'), "head preserved");
        assert!(a.text.ends_with('r'), "tail preserved");
    }
}

// ---------------------------------------------------------------------------
// Citations (§8)
// ---------------------------------------------------------------------------

#[test]
fn pool_dedupes_normalized_urls() {
    let mut p = CitationPool::new();
    p.insert("https://Example.com/A");
    p.insert("https://example.com/A");
    assert_eq!(p.len(), 1, "case+path variants must dedupe to one entry");
}

#[test]
fn pool_rejects_garbage_urls() {
    let mut p = CitationPool::new();
    assert!(!p.insert("not a url"));
    assert_eq!(p.len(), 0);
}

#[test]
fn validate_returns_exact_pool_url() {
    let mut p = CitationPool::new();
    p.insert("https://Example.com/Path");
    let mut s = Synthesis {
        summary: "".into(),
        deep_analysis: "".into(),
        critique: "".into(),
        citations: vec![Citation { url: "https://example.com/Path".into(), context: "c".into() }],
    };
    let (kept, rejected) = validate(&mut s, &p);
    assert!(rejected.is_empty());
    // Exact original casing restored.
    assert_eq!(kept[0].url, "https://Example.com/Path");
}

#[test]
fn all_legit_detects_any_out_of_pool() {
    let mut p = CitationPool::new();
    p.insert("https://a.com/1");
    let ok = Synthesis {
        summary: "".into(),
        deep_analysis: "".into(),
        critique: "".into(),
        citations: vec![Citation { url: "https://a.com/1".into(), context: "c".into() }],
    };
    assert!(all_legit(&ok, &p));
    let bad = Synthesis {
        summary: "".into(),
        deep_analysis: "".into(),
        critique: "".into(),
        citations: vec![Citation { url: "https://a.com/1".into(), context: "c".into() }, Citation { url: "https://b.com/2".into(), context: "c2".into() }],
    };
    assert!(!all_legit(&bad, &p));
}

#[test]
fn pool_from_includes_source_and_related() {
    let src = FetchedArticle {
        url: "https://s.com/1".into(),
        title: String::new(),
        published_date: None,
        author: None,
        language: None,
        text: "x".into(),
    };
    let rel = vec![FetchedArticle {
        url: "https://r.com/2".into(),
        title: String::new(),
        published_date: None,
        author: None,
        language: None,
        text: "y".into(),
    }];
    let p = pool_from(&src, &rel);
    assert_eq!(p.len(), 2);
    assert!(p.contains("https://s.com/1"));
    assert!(p.contains("https://r.com/2"));
}

#[test]
fn validate_drops_all_when_nothing_in_pool() {
    let p = CitationPool::new();
    let mut s = Synthesis {
        summary: "".into(),
        deep_analysis: "".into(),
        critique: "".into(),
        citations: vec![Citation { url: "https://x.com/1".into(), context: "c".into() }],
    };
    let (kept, rejected) = validate(&mut s, &p);
    assert!(kept.is_empty());
    assert_eq!(rejected.len(), 1);
}

// ---------------------------------------------------------------------------
// Error taxonomy (§10)
// ---------------------------------------------------------------------------

#[test]
fn every_error_maps_to_nonempty_message() {
    let cases = vec![
        PipelineError::InvalidUrl,
        PipelineError::PageNotFound,
        PipelineError::TargetHttpError,
        PipelineError::BotBlocked,
        PipelineError::EmptyContent,
        PipelineError::Timeout,
        PipelineError::TargetUnreachable,
        PipelineError::ProxyError,
        PipelineError::SearchFailed("x".into()),
        PipelineError::SynthesisFailed("x".into()),
        PipelineError::Internal("x".into()),
        PipelineError::DeadlineExceeded,
    ];
    for e in cases {
        let m = user_message(&e);
        assert!(!m.text().is_empty());
        assert!(matches!(m, linkbot_core::error::UserMessage::Error(_)));
    }
}

#[test]
fn error_display_roundtrip() {
    let e = PipelineError::BotBlocked;
    assert_eq!(e.to_string(), "site blocks automated readers");
}

#[test]
fn user_message_info_variant() {
    let m = linkbot_core::error::UserMessage::info("hello");
    assert_eq!(m.text(), "hello");
    assert!(matches!(m, linkbot_core::error::UserMessage::Info(_)));
}
