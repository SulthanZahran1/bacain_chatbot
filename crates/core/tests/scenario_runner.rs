//! Mock-provider and scenario-runner tests (§5 Stage 9, §13) — pin the
//! behavior of the mocked search engine, scripted LLM, and the suite runner
//! that the optimizer and CI gate depend on.

use std::sync::Arc;

use linkbot_core::clock::{Clock, FakeClock};
use linkbot_core::config::Config;
use linkbot_core::error::PipelineError;
use linkbot_core::fetcher::Fetcher;
use linkbot_core::mock_providers::{
    AiAwareMockFetcher, MockFetcher, MockSearchProvider, ScriptedLlm,
};
use linkbot_core::optimizer_policy::Policy;
use linkbot_core::pipeline::{analyze, AnalysisRequest, ChannelCtx, Deps};
use linkbot_core::scenario::{
    run_scenario, run_suite, Scenario, ScenarioArticle, ScenarioExpectation, ScenarioOverrides,
    ScenarioSource,
};
use linkbot_core::searcher::{FreshnessWindow, SearchProvider};
use linkbot_core::synthesizer::Llm;

fn base_scenario() -> Scenario {
    Scenario {
        id: "t_base".into(),
        source: ScenarioSource {
            url: "https://medium.com/story/1".into(),
            domain_bucket: "standard".into(),
            is_ai_topic: false,
            ground_truth_angles: 3,
            title: "Industry report".into(),
        },
        corpus: (0..6)
            .map(|i| ScenarioArticle {
                url: format!("https://r{i}.example/a{i}"),
                angle: ["mechanism", "market", "history"][i % 3].to_string(),
                relevance: 0.9 - i as f64 * 0.05,
                fetchable: true,
                published_date: Some("2026-07-28".into()),
                title: format!("R{i}"),
                snippet: "s".into(),
            })
            .collect(),
        expected: ScenarioExpectation {
            min_angles_covered: 2,
            max_wasted_fetches: 1,
        },
        overrides: ScenarioOverrides {
            seed_queries: vec!["mechanism".into(), "market".into()],
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------------
// MockFetcher
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mock_fetcher_succeeds_only_on_fetchable() {
    let mut scn = base_scenario();
    scn.corpus[0].fetchable = false;
    let f = MockFetcher::new(&scn);
    assert!(
        f.fetch(&scn.source.url).await.is_ok(),
        "source always fetchable"
    );
    assert!(matches!(
        f.fetch(&scn.corpus[0].url).await,
        Err(PipelineError::BotBlocked)
    ));
    assert!(f.fetch(&scn.corpus[1].url).await.is_ok());
    assert!(matches!(
        f.fetch("https://unknown.example/x").await,
        Err(PipelineError::PageNotFound)
    ));
    assert_eq!(f.fetch_count(), 4);
}

#[tokio::test]
async fn ai_aware_fetcher_seeds_keywords_for_ai_source() {
    let mut scn = base_scenario();
    scn.source.is_ai_topic = true;
    let f = AiAwareMockFetcher::new(&scn);
    let a = f.fetch(&scn.source.url).await.unwrap();
    assert!(a.text.contains("LLM"), "AI source must contain LLM keyword");
    assert!(
        a.text.contains("agent"),
        "AI source must contain agent keyword"
    );
}

#[tokio::test]
async fn ai_aware_fetcher_plain_text_for_non_ai() {
    let scn = base_scenario();
    let f = AiAwareMockFetcher::new(&scn);
    let a = f.fetch(&scn.source.url).await.unwrap();
    assert!(
        !a.text.contains("LLM"),
        "non-AI source must not contain LLM keyword"
    );
}

// ---------------------------------------------------------------------------
// MockSearchProvider
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mock_search_filters_by_window() {
    let mut scn = base_scenario();
    scn.corpus[0].published_date = Some("2020-01-01".into()); // ancient
    let s = MockSearchProvider::new(&scn);

    // 7d window excludes the ancient article.
    let w = FreshnessWindow {
        recency_minutes: Some(10_080),
        bucket: "fast",
    };
    let hits = s.search(&["mechanism".into()], w, 10, 0).await.unwrap();
    assert!(
        !hits.iter().any(|h| h.url == scn.corpus[0].url),
        "ancient article leaked through"
    );

    // Evergreen (no filter) includes it.
    let w = FreshnessWindow {
        recency_minutes: None,
        bucket: "evergreen",
    };
    let hits = s.search(&["mechanism".into()], w, 10, 0).await.unwrap();
    assert!(
        hits.iter().any(|h| h.url == scn.corpus[0].url),
        "evergreen must not filter"
    );
}

#[tokio::test]
async fn mock_search_dedupes_across_queries() {
    let scn = base_scenario();
    let s = MockSearchProvider::new(&scn);
    let w = FreshnessWindow {
        recency_minutes: None,
        bucket: "evergreen",
    };
    let hits = s
        .search(&["mechanism".into(), "mechanism".into()], w, 10, 0)
        .await
        .unwrap();
    let mut urls: Vec<_> = hits.iter().map(|h| h.url.clone()).collect();
    urls.sort();
    urls.dedup();
    assert_eq!(urls.len(), hits.len(), "duplicate URLs across queries");
}

#[tokio::test]
async fn mock_search_rate_limits_after_n_calls() {
    let mut scn = base_scenario();
    scn.overrides.rate_limit_after = Some(1);
    let s = MockSearchProvider::new(&scn);
    let w = FreshnessWindow {
        recency_minutes: None,
        bucket: "evergreen",
    };
    assert!(s.search(&["mechanism".into()], w, 5, 0).await.is_ok());
    assert!(matches!(
        s.search(&["mechanism".into()], w, 5, 0).await,
        Err(PipelineError::SearchFailed(_))
    ));
}

#[tokio::test]
async fn mock_search_round_override_wins() {
    let mut scn = base_scenario();
    scn.overrides
        .round_overrides
        .insert(1, vec![scn.corpus[5].url.clone()]);
    let s = MockSearchProvider::new(&scn);
    let w = FreshnessWindow {
        recency_minutes: None,
        bucket: "evergreen",
    };
    let hits = s.search(&["mechanism".into()], w, 10, 0).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, scn.corpus[5].url);
}

#[tokio::test]
async fn mock_search_find_similar_returns_same_angle() {
    let scn = base_scenario();
    let s = MockSearchProvider::new(&scn);
    let w = FreshnessWindow {
        recency_minutes: None,
        bucket: "evergreen",
    };
    let similar = s.find_similar(&scn.corpus[0].url, w, 10, 0).await.unwrap();
    assert!(!similar.is_empty());
    let src_angle = scn.corpus[0].angle.clone();
    for h in &similar {
        let art = scn.corpus.iter().find(|a| a.url == h.url).unwrap();
        assert_eq!(
            art.angle, src_angle,
            "find_similar must return same-angle articles"
        );
    }
}

// ---------------------------------------------------------------------------
// ScriptedLlm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scripted_llm_routes_by_prompt() {
    let llm = ScriptedLlm::new(
        vec![],
        vec![],
        vec!["q1".into(), "q2".into()],
        true,
        3,
        &base_scenario().corpus,
    );

    // Classification call.
    let c = llm
        .chat_json("You classify articles. JSON only.", "x")
        .await
        .unwrap();
    assert!(c.contains("\"is_ai\": true"), "{c}");

    // Seed-query extraction call.
    let q = llm
        .chat_json("You extract search queries. JSON only.", "x")
        .await
        .unwrap();
    assert!(q.contains("q1"), "{q}");
    assert!(q.contains("q2"), "{q}");

    // Coverage call with no corpus in prompt → 0 coverage, dynamic angles.
    let cov = llm
        .chat_json("You assess coverage. JSON only.", "CORPUS:\n")
        .await
        .unwrap();
    assert!(
        cov.contains("\"coverage\": 0") && !cov.contains("\"coverage\": 0."),
        "coverage must be 0 with empty corpus: {cov}"
    );
    assert!(
        cov.contains("mechanism"),
        "uncovered angles must include corpus angles: {cov}"
    );
}

#[tokio::test]
async fn scripted_llm_coverage_from_corpus_lines() {
    let scn = base_scenario();
    let llm = ScriptedLlm::new(vec![], vec![], vec!["q".into()], false, 3, &scn.corpus);
    // Prompt listing 2 articles (1 distinct angle) → coverage 1/3.
    let prompt = format!(
        "CORPUS:\n- Title ({})\n- Title2 ({})",
        scn.corpus[0].url, scn.corpus[3].url
    );
    let cov = llm
        .chat_json("You assess coverage. JSON only.", &prompt)
        .await
        .unwrap();
    assert!(cov.contains("\"coverage\": 0.3333333333333333"), "{cov}");
}

#[tokio::test]
async fn scripted_llm_explicit_coverage_sequence() {
    let scn = base_scenario();
    let llm = ScriptedLlm::new(
        vec![0.4, 0.9],
        vec!["mechanism".into()],
        vec![],
        false,
        3,
        &scn.corpus,
    );
    let c1 = llm
        .chat_json("You assess coverage. JSON only.", "x")
        .await
        .unwrap();
    let c2 = llm
        .chat_json("You assess coverage. JSON only.", "x")
        .await
        .unwrap();
    assert!(c1.contains("0.4"), "{c1}");
    assert!(c2.contains("0.9"), "{c2}");
}

#[tokio::test]
async fn scripted_llm_synthesize_cites_all_related() {
    let scn = base_scenario();
    let llm = ScriptedLlm::new(vec![], vec![], vec![], false, 3, &scn.corpus);
    let src = linkbot_core::mock_providers::source_article(&scn.source);
    let related: Vec<_> = scn
        .corpus
        .iter()
        .map(|a| linkbot_core::fetcher::FetchedArticle {
            url: a.url.clone(),
            title: a.title.clone(),
            published_date: a.published_date.clone(),
            author: None,
            language: None,
            text: "x".into(),
        })
        .collect();
    let s = llm.synthesize(&src, &related).await.unwrap();
    assert_eq!(
        s.citations.len(),
        related.len() + 1,
        "source + all related cited"
    );
}

// ---------------------------------------------------------------------------
// Scenario runner
// ---------------------------------------------------------------------------

#[test]
fn run_scenario_passes_when_expectations_met() {
    let scn = base_scenario();
    let r = run_scenario(&scn, Policy::default());
    assert!(r.passed, "detail: {}", r.detail);
    assert!(r.angles_covered >= 2);
    assert!(r.wasted_fetches <= 1);
    assert!(r.corpus_size >= 2);
}

#[test]
fn run_scenario_fails_when_angles_missing() {
    let mut scn = base_scenario();
    scn.expected.min_angles_covered = 10; // impossible
    let r = run_scenario(&scn, Policy::default());
    assert!(!r.passed);
    assert!(r.angles_covered < 10);
}

#[test]
fn run_scenario_fails_when_wasted_exceeds() {
    let mut scn = base_scenario();
    scn.expected.max_wasted_fetches = 0;
    // Force dead articles so fetches fail → wasted > 0.
    scn.corpus[1].fetchable = false;
    scn.corpus[2].fetchable = false;
    let r = run_scenario(&scn, Policy::default());
    assert!(
        !r.passed,
        "wasted={} max={}",
        r.wasted_fetches, r.wasted_max
    );
}

#[test]
fn run_scenario_handles_empty_corpus() {
    let mut scn = base_scenario();
    scn.corpus = vec![];
    scn.expected.min_angles_covered = 0;
    let r = run_scenario(&scn, Policy::default());
    assert!(r.passed, "detail: {}", r.detail);
    assert_eq!(r.corpus_size, 0);
}

#[test]
fn run_suite_counts_pass_total() {
    let scns = vec![base_scenario(), base_scenario()];
    let (passed, total) = run_suite(&scns, Policy::default());
    assert_eq!(total, 2);
    assert_eq!(passed, 2);
}

#[test]
fn run_suite_reports_failures() {
    let mut bad = base_scenario();
    bad.expected.min_angles_covered = 99;
    let scns = vec![base_scenario(), bad];
    let (passed, total) = run_suite(&scns, Policy::default());
    assert_eq!(total, 2);
    assert_eq!(passed, 1);
}

// ---------------------------------------------------------------------------
// End-to-end through the pipeline with the scenario mocks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_loop_honors_policy_budget() {
    let scn = base_scenario();
    let fetcher = Arc::new(MockFetcher::new(&scn));
    let searcher = Arc::new(MockSearchProvider::new(&scn));
    let llm = Arc::new(ScriptedLlm::new(
        vec![0.5, 0.9],
        vec!["market".into()],
        vec!["mechanism".into()],
        false,
        3,
        &scn.corpus,
    ));
    let clock: Clock = Arc::new(FakeClock::new(1_785_484_800));
    let config = Config {
        policy: Policy {
            initial_k: 3,
            expansion_k: 2,
            coverage_target: 0.85,
            min_new_articles: 1,
            max_rounds: 3,
            search_budget: 10,
        },
        ..Default::default()
    };
    let deps = Deps {
        fetcher: fetcher.clone(),
        searcher,
        llm,
        clock,
        config: Arc::new(config),
    };
    let a = analyze(
        AnalysisRequest {
            url: scn.source.url.clone(),
            channel: ChannelCtx { id: "t".into() },
        },
        &deps,
    )
    .await
    .unwrap();
    assert_eq!(a.meta.stop_reason, "coverage(0.90)");
    assert!(a.meta.corpus_size >= 1);
    assert!(
        fetcher.fetch_count() <= 11,
        "budget exceeded: {}",
        fetcher.fetch_count()
    );
}

#[tokio::test]
async fn policy_load_with_env_override_works() {
    std::env::set_var("SEARCH_BUDGET", "42");
    let p = Policy::load_with_env_override(None);
    assert_eq!(p.search_budget, 42);
    assert_eq!(p.initial_k, 5, "untouched env var keeps default");
    std::env::remove_var("SEARCH_BUDGET");
}
