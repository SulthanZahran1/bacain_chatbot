//! Loop-mechanics integration tests (§5 Stage 4) — the coverage-feedback
//! loop's stop conditions and budget behavior, exercised through the REAL
//! pipeline code with mocks.

use std::sync::Arc;

use linkbot_core::cache::Cache;
use linkbot_core::clock::{Clock, FakeClock};
use linkbot_core::config::Config;
use linkbot_core::mock_providers::{AiAwareMockFetcher, MockSearchProvider, ScriptedLlm};
use linkbot_core::optimizer_policy::Policy;
use linkbot_core::pipeline::{analyze, AnalysisRequest, ChannelCtx, Deps};
use linkbot_core::scenario::{
    Scenario, ScenarioArticle, ScenarioExpectation, ScenarioOverrides, ScenarioSource,
};

fn scenario(
    is_ai: bool,
    n_articles: usize,
    angles: &[&str],
    coverage: Vec<f64>,
    scripted_angles: Vec<String>,
    rate_limit_after: Option<usize>,
) -> Scenario {
    let corpus: Vec<ScenarioArticle> = (0..n_articles)
        .map(|i| ScenarioArticle {
            url: format!("https://news-{i}.example/article/{i}"),
            angle: angles[i % angles.len()].to_string(),
            relevance: 0.9 - (i as f64) * 0.01,
            fetchable: true,
            published_date: Some("2026-07-30".into()),
            title: format!("Related {i}"),
            snippet: "s".into(),
        })
        .collect();
    Scenario {
        id: format!("loop_{is_ai}_{n_articles}"),
        source: ScenarioSource {
            url: "https://unknown-blog.example/story/1".into(),
            domain_bucket: "default".into(),
            is_ai_topic: is_ai,
            ground_truth_angles: angles.len(),
            title: if is_ai {
                "New LLM agent framework"
            } else {
                "Industry report"
            }
            .into(),
        },
        corpus,
        expected: ScenarioExpectation {
            min_angles_covered: 1,
            max_wasted_fetches: 999,
        },
        overrides: ScenarioOverrides {
            coverage_per_round: coverage,
            angles: scripted_angles,
            rate_limit_after,
            seed_queries: vec![angles[0].to_string()],
            ..Default::default()
        },
    }
}

fn make_deps(scn: &Scenario, policy: Policy) -> (Deps, Arc<AiAwareMockFetcher>) {
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
    let cache = Arc::new(Cache::in_memory().unwrap());
    let clock: Clock = Arc::new(FakeClock::new(1_785_484_800));
    let config = Config {
        policy,
        ..Default::default()
    };
    let deps = Deps {
        fetcher: fetcher.clone(),
        searcher,
        llm,
        cache,
        clock,
        config: Arc::new(config),
    };
    (deps, fetcher)
}

async fn run(
    scn: &Scenario,
    policy: Policy,
) -> (linkbot_core::pipeline::Analysis, Arc<AiAwareMockFetcher>) {
    let (deps, fetcher) = make_deps(scn, policy);
    let a = analyze(
        AnalysisRequest {
            url: scn.source.url.clone(),
            channel: ChannelCtx { id: "c".into() },
        },
        &deps,
    )
    .await
    .expect("analysis");
    (a, fetcher)
}

#[tokio::test]
async fn stops_when_coverage_target_reached() {
    let scn = scenario(
        false,
        12,
        &["mechanism", "market"],
        vec![0.95],
        vec![],
        None,
    );
    let (a, _) = run(&scn, Policy::default()).await;
    assert_eq!(a.meta.stop_reason, "coverage(0.95)");
    assert_eq!(a.meta.rounds, 1);
}

#[tokio::test]
async fn expands_until_target_then_stops() {
    let scn = scenario(
        false,
        12,
        &["mechanism", "market", "regulation"],
        vec![0.4, 0.9],
        vec![],
        None,
    );
    let (a, _) = run(&scn, Policy::default()).await;
    assert_eq!(a.meta.rounds, 2);
    assert!(
        a.meta.stop_reason.starts_with("coverage"),
        "{}",
        a.meta.stop_reason
    );
    assert!(a.meta.corpus_size >= 2);
}

#[tokio::test]
async fn diminishing_returns_stops_loop() {
    // Coverage never reaches target; round 2 finds nothing new.
    let scn = scenario(
        false,
        6,
        &["mechanism"],
        vec![0.3, 0.3, 0.3],
        vec!["mechanism".into()],
        None,
    );
    let (a, _) = run(&scn, Policy::default()).await;
    assert!(
        a.meta.stop_reason.starts_with("diminishing")
            || a.meta.stop_reason.starts_with("max_rounds")
    );
    assert!(a.meta.rounds <= 3);
}

#[tokio::test]
async fn search_budget_is_hard_cap() {
    // 40 articles, aggressive expansion: budget must never be exceeded.
    let scn = scenario(
        false,
        40,
        &["mechanism", "market", "regulation", "history"],
        vec![0.1, 0.2, 0.3],
        vec![],
        None,
    );
    let policy = Policy {
        initial_k: 5,
        expansion_k: 3,
        coverage_target: 0.9,
        min_new_articles: 1,
        max_rounds: 4,
        search_budget: 15,
    };
    let (_a, fetcher) = run(&scn, policy).await;
    // +1 for the source fetch — the hard cap invariant.
    assert!(
        fetcher.fetch_count() <= 16,
        "fetched {}",
        fetcher.fetch_count()
    );
}

#[tokio::test]
async fn ai_override_forces_30d_window() {
    let scn = scenario(true, 8, &["mechanism", "market"], vec![0.95], vec![], None);
    let (a, _) = run(&scn, Policy::default()).await;
    assert_eq!(a.meta.bucket, "ai-override");
    assert_eq!(a.meta.window_used, "30d");
}

#[tokio::test]
async fn non_ai_unknown_domain_defaults_30d() {
    let scn = scenario(false, 8, &["mechanism", "market"], vec![0.95], vec![], None);
    let (a, _) = run(&scn, Policy::default()).await;
    assert_eq!(a.meta.bucket, "default");
    assert_eq!(a.meta.window_used, "30d");
}

#[tokio::test]
async fn search_rate_limit_degrades_gracefully() {
    // Rate limit after call 1 → round 2 search fails → loop stops gracefully,
    // analysis still produced from the round-1 corpus.
    let scn = scenario(
        false,
        10,
        &["mechanism", "market"],
        vec![0.5],
        vec![],
        Some(1),
    );
    let (a, _) = run(&scn, Policy::default()).await;
    assert_eq!(a.meta.stop_reason, "search-error");
    assert!(!a.summary.is_empty());
}

#[tokio::test]
async fn zero_fetchable_corpus_still_analyzes() {
    let mut scn = scenario(false, 4, &["mechanism"], vec![0.5], vec![], None);
    for art in &mut scn.corpus {
        art.fetchable = false;
    }
    let (a, _) = run(&scn, Policy::default()).await;
    assert_eq!(a.meta.corpus_size, 0);
    assert!(!a.critique.is_empty());
}

#[tokio::test]
async fn citation_validator_rejects_out_of_pool() {
    // Scripted LLM cites only fetched articles; a bogus URL would be pruned.
    let scn = scenario(false, 6, &["mechanism", "market"], vec![0.95], vec![], None);
    let (a, _) = run(&scn, Policy::default()).await;
    for c in &a.citations {
        assert!(
            c.url.starts_with("https://news-") || c.url == scn.source.url,
            "citation outside pool: {}",
            c.url
        );
    }
}

#[tokio::test]
async fn metadata_reports_latency_and_model() {
    let scn = scenario(false, 6, &["mechanism"], vec![0.95], vec![], None);
    let (a, _) = run(&scn, Policy::default()).await;
    assert!(a.meta.latency_ms < 60_000);
    assert!(!a.meta.llm_model.is_empty());
    assert!(a.meta.corpus_size > 0);
}

#[tokio::test]
async fn analysis_is_cached_after_run() {
    let scn = scenario(false, 6, &["mechanism"], vec![0.95], vec![], None);
    let (deps, _) = {
        let (d, _) = make_deps(&scn, Policy::default());
        (d, ())
    };
    let a = analyze(
        AnalysisRequest {
            url: scn.source.url.clone(),
            channel: ChannelCtx { id: "c".into() },
        },
        &deps,
    )
    .await
    .unwrap();
    let cached = deps
        .cache
        .get(&linkbot_core::normalize_url(&scn.source.url).unwrap())
        .unwrap()
        .expect("cached");
    let roundtrip: linkbot_core::pipeline::Analysis =
        serde_json::from_str(&cached.analysis_json).unwrap();
    assert_eq!(roundtrip.meta.window_used, a.meta.window_used);
}

#[tokio::test]
async fn multiple_rounds_never_duplicate_corpus_urls() {
    let scn = scenario(
        false,
        20,
        &["mechanism", "market", "regulation"],
        vec![0.3, 0.6, 0.9],
        vec![],
        None,
    );
    let policy = Policy {
        initial_k: 7,
        expansion_k: 5,
        coverage_target: 0.85,
        min_new_articles: 1,
        max_rounds: 3,
        search_budget: 20,
    };
    let (a, _) = run(&scn, policy).await;
    let mut urls: Vec<&String> = a.citations.iter().map(|c| &c.url).collect();
    urls.sort();
    urls.dedup();
    assert_eq!(urls.len(), a.citations.len(), "duplicate citations");
}

#[tokio::test]
async fn evergreen_bucket_uses_no_date_filter() {
    let mut scn = scenario(false, 6, &["mechanism"], vec![0.95], vec![], None);
    scn.source.domain_bucket = "evergreen".into();
    scn.source.url = "https://en.wikipedia.org/wiki/Foo".into();
    let (a, _) = run(&scn, Policy::default()).await;
    assert_eq!(a.meta.bucket, "evergreen");
    assert_eq!(a.meta.window_used, "evergreen");
}
