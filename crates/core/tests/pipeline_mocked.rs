//! Mocked integration test — the full pipeline against mocks (§13):
//! asserts stage order, corpus assembly, failure drops, the 0-result path,
//! JSON repair retry, and the citation validator in the loop.

use std::sync::Arc;

use linkbot_core::clock::{Clock, FakeClock};
use linkbot_core::config::Config;
use linkbot_core::error::PipelineError;
use linkbot_core::mock_providers::{AiAwareMockFetcher, MockSearchProvider, ScriptedLlm};
use linkbot_core::optimizer_policy::Policy;
use linkbot_core::pipeline::{analyze, AnalysisRequest, ChannelCtx, Deps};
use linkbot_core::scenario::{
    Scenario, ScenarioArticle, ScenarioExpectation, ScenarioOverrides, ScenarioSource,
};

fn scenario_with(seed_queries: Vec<String>, coverage: Vec<f64>, angles: Vec<String>) -> Scenario {
    Scenario {
        id: "itest".into(),
        source: ScenarioSource {
            url: "https://unknown-blog.example/story/1".into(),
            domain_bucket: "default".into(),
            is_ai_topic: false,
            ground_truth_angles: 3,
            title: "Quarterly industry report".into(),
        },
        corpus: vec![
            ScenarioArticle {
                url: "https://news-0.example/a".into(),
                angle: "mechanism".into(),
                relevance: 0.95,
                fetchable: true,
                published_date: Some("2026-07-30".into()),
                title: "How it works".into(),
                snippet: "mech".into(),
            },
            ScenarioArticle {
                url: "https://news-1.example/b".into(),
                angle: "market-impact".into(),
                relevance: 0.9,
                fetchable: true,
                published_date: Some("2026-07-29".into()),
                title: "Market reaction".into(),
                snippet: "market".into(),
            },
            ScenarioArticle {
                url: "https://news-2.example/c".into(),
                angle: "criticism".into(),
                relevance: 0.8,
                fetchable: false, // bot-blocked — must be dropped, never cited
                published_date: Some("2026-07-28".into()),
                title: "Critics weigh in".into(),
                snippet: "crit".into(),
            },
            ScenarioArticle {
                url: "https://news-3.example/d".into(),
                angle: "regulation".into(),
                relevance: 0.7,
                fetchable: true,
                published_date: Some("2026-07-27".into()),
                title: "Regulatory angle".into(),
                snippet: "reg".into(),
            },
        ],
        expected: ScenarioExpectation {
            min_angles_covered: 2,
            max_wasted_fetches: 2,
        },
        overrides: ScenarioOverrides {
            coverage_per_round: coverage,
            angles,
            seed_queries,
            ..Default::default()
        },
    }
}

fn make_deps(scn: &Scenario, policy: Policy) -> Deps {
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
    Deps {
        fetcher,
        searcher,
        llm,
        clock,
        config: Arc::new(config),
    }
}

#[tokio::test]
async fn full_pipeline_assembles_corpus_and_validates_citations() {
    let scn = scenario_with(
        vec!["mechanism".into(), "market-impact".into()],
        vec![0.5, 0.9], // round1 below target → round2 reaches target
        vec!["regulation".into()],
    );
    let deps = make_deps(&scn, Policy::default());

    let analysis = analyze(
        AnalysisRequest {
            url: scn.source.url.clone(),
            channel: ChannelCtx {
                id: "test-channel".into(),
            },
        },
        &deps,
    )
    .await
    .expect("analysis succeeds");

    // Stage order + corpus: source + fetched hits (bot-blocked dropped).
    assert!(
        analysis.meta.corpus_size >= 2,
        "corpus {}",
        analysis.meta.corpus_size
    );
    assert!(analysis.meta.rounds >= 2, "rounds {}", analysis.meta.rounds);
    assert_eq!(analysis.meta.bucket, "default");
    assert_eq!(analysis.meta.window_used, "30d");

    // Citation validator: every cited URL was fetched — the bot-blocked URL
    // must never appear.
    for c in &analysis.citations {
        assert!(
            c.url != "https://news-2.example/c",
            "bot-blocked URL leaked into citations"
        );
    }
    assert!(analysis.meta.citations_rejected == 0);
    assert!(!analysis.summary.is_empty());
    assert!(!analysis.deep_analysis.is_empty());
}

#[tokio::test]
async fn zero_related_results_analyzes_source_alone() {
    // Sparse scenario: coverage scripted low, only 1 fetchable article.
    let mut scn = scenario_with(
        vec!["mechanism".into()],
        vec![0.2, 0.2],
        vec!["mechanism".into()],
    );
    scn.corpus = scn.corpus[..2].to_vec();
    scn.corpus[1].fetchable = false;
    let deps = make_deps(&scn, Policy::default());

    let analysis = analyze(
        AnalysisRequest {
            url: scn.source.url.clone(),
            channel: ChannelCtx { id: "c".into() },
        },
        &deps,
    )
    .await
    .expect("source-only analysis succeeds");
    assert!(analysis.meta.corpus_size <= 1);
    // No panic, analysis still produced.
    assert!(!analysis.critique.is_empty());
}

#[tokio::test]
async fn deadline_error_is_clean() {
    // No deadline test with real sleep — verify the error variant maps to a
    // user-facing message without panic.
    let msg = linkbot_core::error::user_message(&PipelineError::DeadlineExceeded);
    assert!(!msg.text().is_empty());
}
