//! Serialization + date-math tests for the remaining surfaces: window
//! end-dates, scenario schema round-trips, corpus loading, and article
//! serialization contracts.

use linkbot_core::fetcher::FetchedArticle;
use linkbot_core::scenario::{
    Scenario, ScenarioArticle, ScenarioExpectation, ScenarioOverrides, ScenarioSource,
};
use linkbot_core::searcher::{FreshnessWindow, SearchHit};

// ---------------------------------------------------------------------------
// FreshnessWindow date math
// ---------------------------------------------------------------------------

#[test]
fn end_date_is_today() {
    let w = FreshnessWindow {
        recency_minutes: Some(60),
        bucket: "fast",
    };
    // 2026-08-01T12:00:00Z → same day.
    let now = 1_785_585_600;
    assert_eq!(w.end_date(now), "2026-08-01");
}

#[test]
fn start_date_evergreen_is_none() {
    let w = FreshnessWindow {
        recency_minutes: None,
        bucket: "evergreen",
    };
    assert_eq!(w.start_date(1_785_542_400), None);
}

#[test]
fn start_date_hourly_window() {
    let w = FreshnessWindow {
        recency_minutes: Some(60),
        bucket: "fast",
    };
    // 2026-08-01T00:30Z minus 60m = 2026-07-31T23:30Z → previous day.
    let now = 1_785_544_200;
    assert_eq!(w.start_date(now).unwrap(), "2026-07-31");
}

#[test]
fn start_date_exact_day_boundary() {
    let w = FreshnessWindow {
        recency_minutes: Some(1440),
        bucket: "fast",
    };
    // Exactly 1 day back stays on the same date.
    let now = 1_785_542_400 + 86_400;
    assert_eq!(w.start_date(now).unwrap(), "2026-08-01");
}

#[test]
fn window_epoch_zero() {
    let w = FreshnessWindow {
        recency_minutes: Some(10_080),
        bucket: "fast",
    };
    // Unix epoch minus 7 days → 1969-12-25.
    assert_eq!(w.start_date(0).unwrap(), "1969-12-25");
    assert_eq!(w.end_date(0), "1970-01-01");
}

#[test]
fn window_bucket_roundtrip_labels() {
    for (bucket, minutes) in [
        ("breaking", 4_320_i64),
        ("fast", 10_080),
        ("standard", 43_200),
        ("slow", 129_600),
    ] {
        let w = FreshnessWindow {
            recency_minutes: Some(minutes),
            bucket,
        };
        assert!(!w.is_evergreen());
        assert!(w.start_date(1_785_542_400).is_some());
    }
}

// ---------------------------------------------------------------------------
// Scenario schema
// ---------------------------------------------------------------------------

fn sample_scenario(id: &str) -> Scenario {
    Scenario {
        id: id.into(),
        source: ScenarioSource {
            url: "https://example.com/s".into(),
            domain_bucket: "standard".into(),
            is_ai_topic: true,
            ground_truth_angles: 3,
            title: "AI story".into(),
        },
        corpus: vec![ScenarioArticle {
            url: "https://r.example/1".into(),
            angle: "mechanism".into(),
            relevance: 0.9,
            fetchable: true,
            published_date: Some("2026-07-30".into()),
            title: "R".into(),
            snippet: "s".into(),
        }],
        expected: ScenarioExpectation {
            min_angles_covered: 2,
            max_wasted_fetches: 1,
        },
        overrides: ScenarioOverrides {
            seed_queries: vec!["mechanism".into()],
            ..Default::default()
        },
    }
}

#[test]
fn scenario_json_roundtrip_preserves_all_fields() {
    let s = sample_scenario("rt1");
    let j = serde_json::to_string(&s).unwrap();
    let back: Scenario = serde_json::from_str(&j).unwrap();
    assert_eq!(back.id, "rt1");
    assert_eq!(back.source.domain_bucket, "standard");
    assert!(back.source.is_ai_topic);
    assert_eq!(back.source.ground_truth_angles, 3);
    assert_eq!(back.corpus[0].angle, "mechanism");
    assert!(back.corpus[0].fetchable);
    assert_eq!(back.expected.min_angles_covered, 2);
    assert_eq!(back.overrides.seed_queries, vec!["mechanism".to_string()]);
    // Missing overrides field must deserialize to Default.
    assert_eq!(back.overrides.rate_limit_after, None);
    assert!(back.overrides.angles.is_empty());
}

#[test]
fn scenario_missing_overrides_defaults() {
    let j = r#"{
        "id": "no_override",
        "source": {"url": "https://a/b", "domain_bucket": "fast", "is_ai_topic": false, "ground_truth_angles": 2, "title": "T"},
        "corpus": [],
        "expected": {"min_angles_covered": 1, "max_wasted_fetches": 0}
    }"#;
    let s: Scenario = serde_json::from_str(j).unwrap();
    assert!(s.overrides.angles.is_empty());
    assert!(s.overrides.seed_queries.is_empty());
    assert_eq!(s.overrides.rate_limit_after, None);
    assert!(s.overrides.coverage_per_round.is_empty());
    assert!(s.overrides.round_overrides.is_empty());
}

#[test]
fn scenario_corpus_missing_field_errors() {
    let j = r#"{"id":"x","source":{"url":"https://a/b","domain_bucket":"fast","is_ai_topic":false,"ground_truth_angles":1,"title":"T"},"expected":{"min_angles_covered":1,"max_wasted_fetches":0}}"#;
    assert!(
        serde_json::from_str::<Scenario>(j).is_err(),
        "corpus is required"
    );
}

#[test]
fn scenario_load_all_from_temp_dir() {
    let dir = std::env::temp_dir().join("linkbot_scn_load_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let a = sample_scenario("a");
    let b = sample_scenario("b");
    std::fs::write(dir.join("scn_a.json"), serde_json::to_string(&a).unwrap()).unwrap();
    std::fs::write(dir.join("scn_b.json"), serde_json::to_string(&b).unwrap()).unwrap();
    std::fs::write(dir.join("not_scenario.txt"), "hello").unwrap();

    let loaded = Scenario::load_all(dir.to_str().unwrap());
    assert_eq!(loaded.len(), 2, "only .json files load");
    let ids: Vec<String> = loaded.iter().map(|s| s.id.clone()).collect();
    assert!(ids.contains(&"a".to_string()));
    assert!(ids.contains(&"b".to_string()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scenario_load_all_missing_dir_is_empty() {
    let loaded = Scenario::load_all("/nonexistent/dir/xyz");
    assert!(loaded.is_empty());
}

#[test]
fn scenario_load_all_ignores_bad_json() {
    let dir = std::env::temp_dir().join("linkbot_scn_bad");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("broken.json"), "{not json").unwrap();
    std::fs::write(
        dir.join("good.json"),
        serde_json::to_string(&sample_scenario("g")).unwrap(),
    )
    .unwrap();
    let loaded = Scenario::load_all(dir.to_str().unwrap());
    assert_eq!(loaded.len(), 1, "broken file skipped");
    assert_eq!(loaded[0].id, "g");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// FetchedArticle / SearchHit serde
// ---------------------------------------------------------------------------

#[test]
fn fetched_article_roundtrip() {
    let a = FetchedArticle {
        url: "https://a.example/1".into(),
        title: "T".into(),
        published_date: Some("2026-07-01".into()),
        author: Some("Author".into()),
        language: Some("en".into()),
        text: "body text".into(),
    };
    let j = serde_json::to_string(&a).unwrap();
    let back: FetchedArticle = serde_json::from_str(&j).unwrap();
    assert_eq!(back, a);
}

#[test]
fn fetched_article_optionals_none() {
    let a = FetchedArticle {
        url: "https://a.example/2".into(),
        title: String::new(),
        published_date: None,
        author: None,
        language: None,
        text: String::new(),
    };
    let j = serde_json::to_string(&a).unwrap();
    let back: FetchedArticle = serde_json::from_str(&j).unwrap();
    assert_eq!(back.published_date, None);
    assert_eq!(back.author, None);
}

#[test]
fn search_hit_missing_dates_deserialize() {
    let j = r#"{"url":"https://x/1","title":"T","snippet":"S"}"#;
    let h: SearchHit = serde_json::from_str(j).unwrap();
    assert_eq!(h.published_date, None);
    assert_eq!(h.url, "https://x/1");
}

#[test]
fn search_hit_with_dates_deserialize() {
    let j = r#"{"url":"https://x/1","title":"T","snippet":"S","published_date":"2026-07-15"}"#;
    let h: SearchHit = serde_json::from_str(j).unwrap();
    assert_eq!(h.published_date.as_deref(), Some("2026-07-15"));
}

// ---------------------------------------------------------------------------
// ScenarioResult plumbing used by the optimizer
// ---------------------------------------------------------------------------

#[test]
fn scenario_result_accumulates_metrics() {
    let scn = sample_scenario("metrics");
    let r = linkbot_core::scenario::run_scenario(
        &scn,
        linkbot_core::optimizer_policy::Policy::default(),
    );
    // The run must produce a deterministic, inspectable result (usize fields).
    assert!(r.angles_expected >= 1);
    assert!(!r.detail.is_empty(), "detail must explain the outcome");
}

#[test]
fn scenario_result_passed_flag_consistent() {
    let scn = sample_scenario("consistency");
    let r = linkbot_core::scenario::run_scenario(
        &scn,
        linkbot_core::optimizer_policy::Policy::default(),
    );
    if r.passed {
        assert!(r.angles_covered >= r.angles_expected);
        assert!(r.wasted_fetches <= r.wasted_max);
    }
}
