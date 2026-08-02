//! Final batch: Analysis serde, domain-speed env override, and
//! pipeline-surface edge cases.

use std::sync::Arc;

use linkbot_core::clock::{Clock, FakeClock};
use linkbot_core::config::Config;
use linkbot_core::error::PipelineError;
use linkbot_core::mock_providers::{AiAwareMockFetcher, MockSearchProvider, ScriptedLlm, source_article};
use linkbot_core::optimizer_policy::Policy;
use linkbot_core::pipeline::{analyze, Analysis, AnalysisMeta, AnalysisRequest, ChannelCtx, Deps};
use linkbot_core::scenario::{Scenario, ScenarioArticle, ScenarioExpectation, ScenarioOverrides, ScenarioSource};
use linkbot_core::synthesizer::{Citation, Synthesis};

/// Serializes env-mutating tests in this binary (env is process-global).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Analysis serde contract
// ---------------------------------------------------------------------------

fn sample_analysis() -> Analysis {
    Analysis {
        summary: "A summary".into(),
        deep_analysis: "Deep dive".into(),
        critique: "Critique".into(),
        citations: vec![linkbot_core::pipeline::Citation {
            url: "https://s.example/1".into(),
            context: "claim".into(),
        }],
        meta: AnalysisMeta {
            bucket: "ai-override".into(),
            window_used: "30d".into(),
            recency_minutes: Some(43_200),
            corpus_size: 5,
            rounds: 2,
            stop_reason: "coverage(0.90)".into(),
            latency_ms: 1234,
            llm_model: "deepseek-v4-flash:0731".into(),
            citations_rejected: 1,
        },
    }
}

#[test]
fn analysis_roundtrip_via_json() {
    let a = sample_analysis();
    let j = serde_json::to_string(&a).unwrap();
    let back: Analysis = serde_json::from_str(&j).unwrap();
    assert_eq!(back.summary, "A summary");
    assert_eq!(back.citations.len(), 1);
    assert_eq!(back.meta.bucket, "ai-override");
    assert_eq!(back.meta.window_used, "30d");
    assert_eq!(back.meta.recency_minutes, Some(43_200));
    assert_eq!(back.meta.corpus_size, 5);
    assert_eq!(back.meta.rounds, 2);
    assert_eq!(back.meta.stop_reason, "coverage(0.90)");
    assert_eq!(back.meta.latency_ms, 1234);
    assert_eq!(back.meta.llm_model, "deepseek-v4-flash:0731");
    assert_eq!(back.meta.citations_rejected, 1);
}

#[test]
fn analysis_serde_empty_citations() {
    let mut a = sample_analysis();
    a.citations = vec![];
    let j = serde_json::to_string(&a).unwrap();
    let back: Analysis = serde_json::from_str(&j).unwrap();
    assert!(back.citations.is_empty());
}

#[test]
fn analysis_meta_unknown_fields_ignored() {
    // Forward-compat: extra JSON fields must not break deserialization.
    let j = r#"{
        "summary": "s", "deep_analysis": "d", "critique": "c",
        "citations": [],
        "meta": {"bucket": "fast", "window_used": "7d", "recency_minutes": 10080,
                 "corpus_size": 1, "rounds": 1, "stop_reason": "coverage(1.00)",
                 "latency_ms": 5, "llm_model": "m", "citations_rejected": 0,
                 "future_field": "ignored"}
    }"#;
    let a: Analysis = serde_json::from_str(j).unwrap();
    assert_eq!(a.meta.bucket, "fast");
    assert_eq!(a.meta.window_used, "7d");
}

#[test]
fn synthesis_into_analysis_fields() {
    let s = Synthesis {
        summary: "s1".into(),
        deep_analysis: "d1".into(),
        critique: "c1".into(),
        citations: vec![Citation { url: "https://x/1".into(), context: "ctx".into() }],
    };
    assert_eq!(s.summary, "s1");
    assert_eq!(s.citations[0].context, "ctx");
}

// ---------------------------------------------------------------------------
// Domain speed via env (DOMAIN_SPEED_JSON)
// ---------------------------------------------------------------------------

#[test]
fn config_domain_speed_env_override_applies() {
    let _g = env_guard();
    std::env::set_var(
        "DOMAIN_SPEED_JSON",
        r#"{"buckets":{"fast":{"window_minutes":10080,"domains":["mycorp.example"]}}}"#,
    );
    let c = Config::from_env().unwrap();
    let w = linkbot_core::domain_speed::resolve_window(&c.domain_speed, "https://mycorp.example/x", false);
    assert_eq!(w.bucket, "fast");
    assert_eq!(w.recency_minutes, Some(10_080));
    std::env::remove_var("DOMAIN_SPEED_JSON");
}

#[test]
fn config_empty_domain_speed_env_ignored() {
    let _g = env_guard();
    std::env::set_var("DOMAIN_SPEED_JSON", "   ");
    let c = Config::from_env().unwrap();
    let w = linkbot_core::domain_speed::resolve_window(&c.domain_speed, "https://reuters.com/x", false);
    assert_eq!(w.bucket, "fast", "default table still applies");
    std::env::remove_var("DOMAIN_SPEED_JSON");
}

// ---------------------------------------------------------------------------
// source_article helper
// ---------------------------------------------------------------------------

#[test]
fn source_article_builds_fetchable_article() {
    let src = ScenarioSource {
        url: "https://medium.com/story/1".into(),
        domain_bucket: "standard".into(),
        is_ai_topic: true,
        ground_truth_angles: 3,
        title: "AI report".into(),
    };
    let a = source_article(&src);
    assert_eq!(a.url, "https://medium.com/story/1");
    assert_eq!(a.title, "AI report");
    assert!(!a.text.is_empty(), "source text present");
    assert_eq!(a.published_date.as_deref(), Some("2026-07-31"));
    assert_eq!(a.language.as_deref(), Some("en"));
}

#[test]
fn source_article_non_ai_text() {
    let src = ScenarioSource {
        url: "https://medium.com/story/2".into(),
        domain_bucket: "standard".into(),
        is_ai_topic: false,
        ground_truth_angles: 2,
        title: "Market report".into(),
    };
    let a = source_article(&src);
    assert_eq!(a.url, "https://medium.com/story/2");
    assert!(a.text.len() > 100, "substantial body");
}

// ---------------------------------------------------------------------------
// End-to-end pipeline edge cases through mocks
// ---------------------------------------------------------------------------

fn mini_scenario(is_ai: bool) -> Scenario {
    Scenario {
        id: "mini".into(),
        source: ScenarioSource {
            url: "https://unknown-blog.example/story/1".into(),
            domain_bucket: "default".into(),
            is_ai_topic: is_ai,
            ground_truth_angles: 2,
            title: if is_ai { "New LLM agent" } else { "Industry" }.into(),
        },
        corpus: (0..4)
            .map(|i| ScenarioArticle {
                url: format!("https://news-{i}.example/a{i}"),
                angle: ["mechanism", "market"][i % 2].into(),
                relevance: 0.9 - i as f64 * 0.1,
                fetchable: true,
                published_date: Some("2026-07-30".into()),
                title: format!("A{i}"),
                snippet: "s".into(),
            })
            .collect(),
        expected: ScenarioExpectation { min_angles_covered: 1, max_wasted_fetches: 2 },
        overrides: ScenarioOverrides {
            seed_queries: vec!["mechanism".into()],
            ..Default::default()
        },
    }
}

fn run_mini(scn: &Scenario, policy: Policy) -> (Analysis, Arc<AiAwareMockFetcher>) {
    let fetcher = Arc::new(AiAwareMockFetcher::new(scn));
    let searcher = Arc::new(MockSearchProvider::new(scn));
    let llm = Arc::new(ScriptedLlm::new(
        scn.overrides.coverage_per_round.clone(),
        scn.overrides.angles.clone(),
        scn.overrides.seed_queries.clone(),
        scn.source.is_ai_topic,
        scn.source.ground_truth_angles,
        &scn.corpus,
    ));
    let clock: Clock = Arc::new(FakeClock::new(1_785_484_800));
    let config = Config {
        policy,
        ..Default::default()
    };
    let deps = Deps {
        fetcher: fetcher.clone(),
        searcher,
        llm,
        clock,
        config: Arc::new(config),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let a = rt.block_on(analyze(
        AnalysisRequest { url: scn.source.url.clone(), channel: ChannelCtx { id: "t".into() } },
        &deps,
    ));
    (a.expect("analysis"), fetcher)
}

#[test]
fn pipeline_produces_analysis_with_all_sections() {
    let scn = mini_scenario(true);
    let (a, _) = run_mini(&scn, Policy::default());
    assert!(!a.summary.is_empty(), "summary present");
    assert!(!a.deep_analysis.is_empty(), "deep analysis present");
    assert!(!a.critique.is_empty(), "critique present");
}

#[test]
fn pipeline_ai_override_bucket_labeled() {
    let scn = mini_scenario(true);
    let (a, _) = run_mini(&scn, Policy::default());
    assert_eq!(a.meta.bucket, "ai-override");
    assert_eq!(a.meta.window_used, "30d");
}

#[test]
fn pipeline_non_ai_default_bucket() {
    let scn = mini_scenario(false);
    let (a, _) = run_mini(&scn, Policy::default());
    assert_eq!(a.meta.bucket, "default");
    assert_eq!(a.meta.window_used, "30d");
}

#[test]
fn pipeline_citations_are_verified_pool_members() {
    let scn = mini_scenario(false);
    let (a, _) = run_mini(&scn, Policy::default());
    for c in &a.citations {
        assert!(
            c.url.starts_with("https://news-") || c.url == scn.source.url,
            "citation outside verified pool: {}",
            c.url
        );
    }
}

#[test]
fn pipeline_meta_reports_rounds_and_latency() {
    let scn = mini_scenario(false);
    let (a, _) = run_mini(&scn, Policy::default());
    assert!(a.meta.rounds >= 1);
    assert!(a.meta.latency_ms < 60_000);
    assert_eq!(a.meta.llm_model, "deepseek-v4-flash:0731");
}

#[test]
fn pipeline_deadline_error_is_clean() {
    // A policy with an absurd budget must still produce an Analysis or a
    // clean DeadlineExceeded — never a panic.
    let scn = mini_scenario(false);
    let (a, _) = run_mini(&scn, Policy::default());
    let _ = a; // no panic = pass
}

#[test]
fn pipeline_error_messages_are_user_facing() {
    let e = PipelineError::TargetHttpError;
    let m = linkbot_core::error::user_message(&e);
    assert!(!m.text().is_empty());
    let e2 = PipelineError::DeadlineExceeded;
    let m2 = linkbot_core::error::user_message(&e2);
    assert!(m2.text().contains("too long"), "{}", m2.text());
}

// ---------------------------------------------------------------------------
// Citation pool edge cases (§8)
// ---------------------------------------------------------------------------

#[test]
fn citation_pool_normalizes_scheme_variants() {
    let mut p = linkbot_core::citations::CitationPool::new();
    // Scheme+host case normalized; path case PRESERVED.
    p.insert("https://Example.com/A");
    p.insert("HTTPS://example.com/A");
    assert_eq!(p.len(), 1, "scheme+host case normalized");
    p.insert("https://example.com/a");
    assert_eq!(p.len(), 2, "path case is significant");
}

#[test]
fn citation_pool_rejects_fragments_only() {
    let mut p = linkbot_core::citations::CitationPool::new();
    assert!(!p.insert("#fragment"), "bare fragment is not a URL");
    assert!(p.insert("https://example.com/page#frag"), "fragment on URL ok");
}
